//! Hardware H.264 / HEVC decoding with VideoToolbox, into IOSurface-backed pixel buffers
//! that GPUI's Metal renderer draws as they are: no copy from decode to screen.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use anyhow::{Result, bail};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag, kCMVideoCodecType_HEVC,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession,
    VTIsHardwareDecodeSupported, VTSessionSetProperty, kVTDecompressionPropertyKey_RealTime,
    kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder,
};

use super::annexb;

/// A decoded picture. Pixel buffers are reference counted and safe to hand between threads.
#[derive(Clone)]
pub struct Picture(pub CFRetained<CVPixelBuffer>);
unsafe impl Send for Picture {}
unsafe impl Sync for Picture {}

/// Where decoded frames go: (picture, frame id), or `None` when one couldn't be decoded.
pub type Output = Arc<dyn Fn(Option<(Picture, u32)>) + Send + Sync>;

/// Whether this Mac decodes HEVC in hardware (so the host may send it).
pub fn hevc_supported() -> bool {
    unsafe { VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC) }
}

/// Whether this Mac decodes HEVC Main 4:2:2 10 (the host's sharp-text profile) into a
/// picture the window can draw: proved once, by encoding a frame and decoding it.
pub fn rext_supported() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        use std::sync::Mutex;
        use std::sync::atomic::AtomicBool;
        if !hevc_supported() || !crate::platform::encode::supports_rext() {
            return false;
        }
        let frame = Arc::new(Mutex::new(None::<Vec<u8>>));
        let f = frame.clone();
        let Ok(enc) = crate::platform::encode::Encoder::new(
            320,
            180,
            30,
            1_000_000,
            true,
            true,
            false,
            Arc::new(AtomicBool::new(true)),
            Arc::new(move |e: crate::media::EncodedFrame| {
                f.lock().unwrap().get_or_insert(e.data);
            }),
        ) else {
            return false;
        };
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        let format = objc2_core_video::kCVPixelFormatType_32BGRA;
        if unsafe { objc2_core_video::CVPixelBufferCreate(None, 320, 180, format, None, NonNull::from(&mut raw)) } != 0 {
            return false;
        }
        let Some(raw) = NonNull::new(raw) else { return false };
        let image = unsafe { CFRetained::from_raw(raw) };
        enc.encode(&image, unsafe { CMTime::new(1, 1_000_000) }, 1, false);
        enc.flush();
        let Some(key) = frame.lock().unwrap().take() else { return false };
        let decoded = Arc::new(AtomicBool::new(false));
        let d = decoded.clone();
        let ok = Decoder::new(&key, true, Arc::new(move |p| d.store(p.is_some(), std::sync::atomic::Ordering::Relaxed))).and_then(|dec| dec.decode(&key, 0)).is_ok()
            && decoded.load(std::sync::atomic::Ordering::Relaxed);
        tracing::info!("viewer: HEVC 4:2:2 decoding {}", if ok { "works" } else { "doesn't work" });
        ok
    })
}

impl Picture {
    /// The picture as a PNG (BT.709, full range: what the decoder was asked for).
    pub fn png(&self) -> Option<Vec<u8>> {
        use objc2_core_video::*;
        let pb = &self.0;
        unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) };
        let (w, h) = (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb));
        let (y, ys) = (CVPixelBufferGetBaseAddressOfPlane(pb, 0) as *const u8, CVPixelBufferGetBytesPerRowOfPlane(pb, 0));
        let (uv, uvs) = (CVPixelBufferGetBaseAddressOfPlane(pb, 1) as *const u8, CVPixelBufferGetBytesPerRowOfPlane(pb, 1));
        let mut rgb = vec![0u8; w * h * 3];
        for r in 0..h {
            for c in 0..w {
                let yv = unsafe { *y.add(r * ys + c) } as f32;
                let u = unsafe { *uv.add((r / 2) * uvs + (c / 2) * 2) } as f32 - 128.0;
                let v = unsafe { *uv.add((r / 2) * uvs + (c / 2) * 2 + 1) } as f32 - 128.0;
                let px = &mut rgb[(r * w + c) * 3..][..3];
                px[0] = (yv + 1.5748 * v).clamp(0.0, 255.0) as u8;
                px[1] = (yv - 0.1873 * u - 0.4681 * v).clamp(0.0, 255.0) as u8;
                px[2] = (yv + 1.8556 * u).clamp(0.0, 255.0) as u8;
            }
        }
        unsafe { CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) };
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().ok()?.write_image_data(&rgb).ok()?;
        Some(out)
    }
}

pub struct Decoder {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    /// The parameter sets it was made for: new ones (a new size, codec) need a new decoder.
    params: Vec<Vec<u8>>,
    hevc: bool,
    /// The callback's context; lives as long as the session.
    _output: Box<Output>,
}

// VTDecompressionSession is thread-safe; one stream's frames are decoded in order.
unsafe impl Send for Decoder {}

unsafe extern "C-unwind" fn on_decoded(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _pts: CMTime,
    _duration: CMTime,
) {
    let output = unsafe { &*(refcon as *const Output) };
    let id = frame_refcon as usize as u32;
    match NonNull::new(image) {
        Some(image) if status == 0 => output(Some((Picture(unsafe { CFRetained::retain(image) }), id))),
        _ => {
            tracing::warn!("viewer: decode failed ({status})");
            output(None)
        }
    }
}

impl Decoder {
    /// A decoder for the stream a keyframe starts (its parameter sets).
    pub fn new(keyframe: &[u8], hevc: bool, output: Output) -> Result<Self> {
        let Some(sets) = annexb::parameter_sets(keyframe, hevc) else { bail!("keyframe without parameter sets") };
        let pointers: Vec<NonNull<u8>> = sets.iter().map(|s| NonNull::new(s.as_ptr() as *mut u8).unwrap()).collect();
        let sizes: Vec<usize> = sets.iter().map(|s| s.len()).collect();
        let mut format: *const CMFormatDescription = ptr::null();
        let status = unsafe {
            let (p, s, out) = (NonNull::new(pointers.as_ptr() as *mut _).unwrap(), NonNull::new(sizes.as_ptr() as *mut _).unwrap(), NonNull::from(&mut format));
            if hevc {
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(None, sets.len(), p, s, 4, None, out)
            } else {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(None, sets.len(), p, s, 4, out)
            }
        };
        let Some(format) = NonNull::new(format as *mut CMFormatDescription).filter(|_| status == 0) else {
            bail!("CMVideoFormatDescriptionCreate failed: {status}")
        };
        let format = unsafe { CFRetained::from_raw(format) };

        let yes = CFBoolean::new(true);
        let spec = unsafe {
            CFDictionary::<CFString, CFType>::from_slices(&[kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder], &[yes.as_ref()])
        };
        // What GPUI's renderer draws: bi-planar 4:2:0, full range, in an IOSurface Metal can use.
        let pixel_format = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange as i32);
        let no_props = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let attrs = unsafe {
            CFDictionary::<CFString, CFType>::from_slices(
                &[kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferIOSurfacePropertiesKey],
                &[pixel_format.as_ref(), yes.as_ref(), no_props.as_ref()],
            )
        };
        let output = Box::new(output);
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_decoded),
            decompressionOutputRefCon: &*output as *const Output as *mut c_void,
        };
        let mut raw: *mut VTDecompressionSession = ptr::null_mut();
        let status = unsafe {
            VTDecompressionSession::create(None, &format, Some(spec.as_opaque()), Some(attrs.as_opaque()), &record, NonNull::from(&mut raw))
        };
        let Some(raw) = NonNull::new(raw).filter(|_| status == 0) else { bail!("VTDecompressionSessionCreate failed: {status}") };
        let session = unsafe { CFRetained::from_raw(raw) };
        unsafe { VTSessionSetProperty(&session, kVTDecompressionPropertyKey_RealTime, Some(yes.as_ref())) };
        Ok(Self { session, format, params: sets.iter().map(|s| s.to_vec()).collect(), hevc, _output: output })
    }

    /// Whether this keyframe continues the stream this decoder was made for.
    pub fn fits(&self, keyframe: &[u8], hevc: bool) -> bool {
        hevc == self.hevc && annexb::parameter_sets(keyframe, hevc).is_some_and(|s| s.iter().copied().eq(self.params.iter().map(Vec::as_slice)))
    }

    /// Decodes one Annex B frame; the picture goes to the output (before this returns).
    pub fn decode(&self, frame: &[u8], id: u32) -> Result<()> {
        let sample = annexb::to_length_prefixed(frame, self.hevc);
        if sample.is_empty() {
            bail!("frame has no picture");
        }
        unsafe {
            let mut block: *mut CMBlockBuffer = ptr::null_mut();
            let status = CMBlockBuffer::create_with_memory_block(
                None,
                ptr::null_mut(),
                sample.len(),
                None,
                ptr::null(),
                0,
                sample.len(),
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut block),
            );
            let Some(block) = NonNull::new(block).filter(|_| status == 0) else { bail!("CMBlockBufferCreate failed: {status}") };
            let block = CFRetained::from_raw(block);
            let status = CMBlockBuffer::replace_data_bytes(NonNull::new_unchecked(sample.as_ptr() as *mut c_void), &block, 0, sample.len());
            if status != 0 {
                bail!("CMBlockBufferReplaceDataBytes failed: {status}");
            }
            let mut buf: *mut CMSampleBuffer = ptr::null_mut();
            let size = sample.len();
            let status = CMSampleBuffer::create_ready(None, Some(&block), Some(&self.format), 1, 0, ptr::null(), 1, &size, NonNull::from(&mut buf));
            let Some(buf) = NonNull::new(buf).filter(|_| status == 0) else { bail!("CMSampleBufferCreateReady failed: {status}") };
            let buf = CFRetained::from_raw(buf);
            // Synchronous: the picture is out before this returns, so frames stay in order and
            // nothing waits in a queue.
            let status = self.session.decode_frame(&buf, VTDecodeFrameFlags(0), id as usize as *mut c_void, ptr::null_mut());
            if status != 0 {
                bail!("VTDecompressionSessionDecodeFrame failed: {status}");
            }
        }
        Ok(())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { self.session.invalidate() };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use objc2_core_media::CMTime;
    use objc2_core_video::*;

    use super::*;
    use crate::platform::encode::{EncodedFrame, Encoder};

    /// A picture the encoder takes: BGRA (4:2:2 sharp text) or 420v, with moving bars.
    pub(super) fn picture(w: usize, h: usize, bgra: bool, t: usize) -> CFRetained<CVImageBuffer> {
        let format = if bgra { kCVPixelFormatType_32BGRA } else { u32::from_be_bytes(*b"420v") };
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        assert_eq!(unsafe { CVPixelBufferCreate(None, w, h, format, None, NonNull::from(&mut raw)) }, 0);
        let image = unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) };
        unsafe { CVPixelBufferLockBaseAddress(&image, CVPixelBufferLockFlags(0)) };
        if bgra {
            let (base, stride) = (CVPixelBufferGetBaseAddress(&image) as *mut u8, CVPixelBufferGetBytesPerRow(&image));
            for y in 0..h {
                for x in 0..w {
                    let on = (x + t * 8) / 40 % 2 == 0;
                    let p = unsafe { base.add(y * stride + x * 4) };
                    let c = [if on { 255 } else { 20 }, (y * 255 / h) as u8, (x * 255 / w) as u8, 255];
                    unsafe { std::ptr::copy_nonoverlapping(c.as_ptr(), p, 4) };
                }
            }
        } else {
            let (yp, ys) = (CVPixelBufferGetBaseAddressOfPlane(&image, 0) as *mut u8, CVPixelBufferGetBytesPerRowOfPlane(&image, 0));
            let (uv, uvs) = (CVPixelBufferGetBaseAddressOfPlane(&image, 1) as *mut u8, CVPixelBufferGetBytesPerRowOfPlane(&image, 1));
            for y in 0..h {
                for x in 0..w {
                    unsafe { *yp.add(y * ys + x) = if (x + t * 8) / 40 % 2 == 0 { 220 } else { 30 } };
                }
            }
            for y in 0..h / 2 {
                for x in 0..w {
                    unsafe { *uv.add(y * uvs + x) = 128u8.wrapping_add((x / 50) as u8) };
                }
            }
        }
        unsafe { CVPixelBufferUnlockBaseAddress(&image, CVPixelBufferLockFlags(0)) };
        image
    }

    /// Real VideoToolbox both ways: the host's encoder, this decoder, in every codec the
    /// viewer offers (HEVC 4:2:2 where this Mac encodes it). Every frame comes out, as what
    /// GPUI draws (bi-planar 4:2:0 full range).
    /// `cargo test -p glass-host decodes_every_codec -- --ignored --nocapture`
    #[test]
    #[ignore = "real VideoToolbox: run by hand"]
    fn decodes_every_codec() {
        let mut modes = vec![("h264", false, false), ("hevc", true, false)];
        if crate::platform::encode::supports_rext() {
            modes.push(("hevc 4:2:2", true, true));
        }
        for (name, hevc, rext) in modes {
            let (w, h, n) = (1280usize, 720usize, 30usize);
            let frames = Arc::new(Mutex::new(Vec::<EncodedFrame>::new()));
            let f = frames.clone();
            let enc = Encoder::new(w as i32, h as i32, 60, 8_000_000, hevc, rext, false, Arc::new(AtomicBool::new(true)), Arc::new(move |e| f.lock().unwrap().push(e))).unwrap();
            for t in 0..n {
                let us = 1_000 + t as u64 * 16_666;
                enc.encode(&picture(w, h, enc.wants_bgra(), t), unsafe { CMTime::new(us as i64, 1_000_000) }, us, false);
            }
            enc.flush();
            let frames = std::mem::take(&mut *frames.lock().unwrap());
            assert!(frames.first().is_some_and(|f| f.keyframe), "{name}: starts on a keyframe");
            let out = Arc::new(Mutex::new(Vec::<(u32, usize, usize, u32)>::new()));
            let o = out.clone();
            let dec = Decoder::new(
                &frames[0].data,
                hevc,
                Arc::new(move |p| {
                    let (pic, id) = p.expect("decoded");
                    o.lock().unwrap().push((id, CVPixelBufferGetWidth(&pic.0), CVPixelBufferGetHeight(&pic.0), CVPixelBufferGetPixelFormatType(&pic.0)));
                }),
            )
            .unwrap();
            for (i, f) in frames.iter().enumerate() {
                dec.decode(&f.data, i as u32).unwrap();
            }
            let out = out.lock().unwrap();
            println!("{name}: {} encoded, {} decoded, {}x{}", frames.len(), out.len(), out[0].1, out[0].2);
            assert_eq!(out.len(), frames.len(), "{name}: every frame decoded");
            assert!(out.iter().all(|o| (o.1, o.2, o.3) == (w, h, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange)), "{name}: {out:?}");
        }
    }
}

#[cfg(test)]
mod vectors {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use objc2_core_media::CMTime;
    use objc2_core_video::*;

    use super::*;
    use crate::platform::encode::{EncodedFrame, Encoder};

    /// Writes the HEVC keyframes the Windows viewer decodes to find out what it can take
    /// (`src/viewer/testdata`): Main, and Main 4:2:2 10 (sharp text).
    /// `cargo test -p glass-host write_hevc_vectors -- --ignored`
    #[test]
    #[ignore = "regenerates checked-in files"]
    fn write_hevc_vectors() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/viewer/testdata");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, rext) in [("hevc-main.bin", false), ("hevc-422.bin", true)] {
            let out = Arc::new(Mutex::new(None::<Vec<u8>>));
            let o = out.clone();
            let enc = Encoder::new(128, 72, 30, 500_000, true, rext, false, Arc::new(AtomicBool::new(true)), Arc::new(move |e: EncodedFrame| {
                o.lock().unwrap().get_or_insert(e.data);
            }))
            .unwrap();
            let image = super::tests::picture(128, 72, enc.wants_bgra(), 0);
            enc.encode(&image, unsafe { CMTime::new(1, 1_000_000) }, 1, false);
            enc.flush();
            let key = out.lock().unwrap().take().unwrap();
            assert!(annexb::parameter_sets(&key, true).is_some());
            std::fs::write(dir.join(name), &key).unwrap();
            println!("{name}: {} bytes", key.len());
        }
    }
}
