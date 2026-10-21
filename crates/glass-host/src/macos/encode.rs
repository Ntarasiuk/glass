//! Hardware H.264 encoding with VideoToolbox in low-latency rate-control mode.
//!
//! Output is Annex B (start-code delimited) with SPS/PPS prepended to every
//! keyframe, which WebCodecs decodes without an out-of-band `description`.

use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use block2::RcBlock;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    kCMTimeInvalid, kCMVideoCodecType_H264,
};
use objc2_core_video::CVImageBuffer;
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_High_AutoLevel,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};

const START_CODE: [u8; 4] = [0, 0, 0, 1];
const MAX_GOP_FRAMES: i32 = 900;

pub use crate::media::EncodedFrame;

pub struct Encoder {
    session: CFRetained<VTCompressionSession>,
    force_keyframe: Arc<AtomicBool>,
    on_frame: Arc<dyn Fn(EncodedFrame) + Send + Sync>,
}

// VTCompressionSession is thread-safe; frames are submitted from the capture queue only.
unsafe impl Send for Encoder {}
unsafe impl Sync for Encoder {}

impl Encoder {
    pub fn new(
        width: i32,
        height: i32,
        fps: i32,
        bitrate: i32,
        force_keyframe: Arc<AtomicBool>,
        on_frame: Arc<dyn Fn(EncodedFrame) + Send + Sync>,
    ) -> Result<Self> {
        let yes = CFBoolean::new(true);
        let spec_keys = unsafe {
            [
                kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
            ]
        };
        let spec = CFDictionary::<CFString, CFType>::from_slices(&spec_keys, &[yes, yes]);

        let mut raw: *mut VTCompressionSession = ptr::null_mut();
        let status = unsafe {
            VTCompressionSession::create(
                None,
                width,
                height,
                kCMVideoCodecType_H264,
                Some(spec.as_opaque()),
                None,
                None,
                None,
                ptr::null_mut(),
                NonNull::from(&mut raw),
            )
        };
        if status != 0 || raw.is_null() {
            bail!("VTCompressionSessionCreate failed: {status}");
        }
        let session = unsafe { CFRetained::from_raw(NonNull::new_unchecked(raw)) };

        let set = |key: &CFString, value: &CFType| {
            let status = unsafe { VTSessionSetProperty(&session, key, Some(value)) };
            if status != 0 {
                tracing::warn!("VTSessionSetProperty({key}) failed: {status}");
            }
        };
        unsafe {
            set(kVTCompressionPropertyKey_RealTime, yes);
            set(kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::new(false));
            set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_High_AutoLevel);
            set(kVTCompressionPropertyKey_AverageBitRate, &CFNumber::new_i32(bitrate));
            set(kVTCompressionPropertyKey_ExpectedFrameRate, &CFNumber::new_i32(fps));
            // Keyframes on demand (new client or reported loss), plus one before H.264's
            // frame_num wraps at 1024: Safari's hardware decoder fails on the wrap.
            set(kVTCompressionPropertyKey_MaxKeyFrameInterval, &CFNumber::new_i32(MAX_GOP_FRAMES));
            session.prepare_to_encode_frames();
        }

        Ok(Self { session, force_keyframe, on_frame })
    }

    /// Changes the target bitrate on the fly; low-latency rate control adapts within frames.
    pub fn set_bitrate(&self, bps: u32) {
        let value = CFNumber::new_i32(bps as i32);
        let status = unsafe { VTSessionSetProperty(&self.session, kVTCompressionPropertyKey_AverageBitRate, Some(&value)) };
        if status != 0 {
            tracing::warn!("set bitrate {bps}: {status}");
        }
    }

    pub fn encode(&self, image: &CVImageBuffer, pts: CMTime, capture_us: u64) {
        let props = self.force_keyframe.swap(false, Ordering::Relaxed).then(|| {
            let keys = unsafe { [kVTEncodeFrameOptionKey_ForceKeyFrame] };
            CFDictionary::<CFString, CFType>::from_slices(&keys, &[CFBoolean::new(true)])
        });
        let on_frame = self.on_frame.clone();
        let handler = RcBlock::new(
            move |status: i32, _flags: VTEncodeInfoFlags, sample: *mut CMSampleBuffer| {
                if status != 0 || sample.is_null() {
                    tracing::warn!("encode failed: {status}");
                    return;
                }
                if let Some(frame) = unsafe { annex_b(&*sample) } {
                    on_frame(EncodedFrame { capture_us, ..frame });
                }
            },
        );
        let status = unsafe {
            self.session.encode_frame_with_output_handler(
                image,
                pts,
                CMTime::new(0, 0),
                props.as_deref().map(|d| d.as_opaque()),
                ptr::null_mut(),
                RcBlock::as_ptr(&handler),
            )
        };
        if status != 0 {
            tracing::warn!("VTCompressionSessionEncodeFrame failed: {status}");
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // Flush and stop so no late frames from this session follow the next encoder's
        // keyframe (the client would decode them against the wrong reference).
        unsafe {
            self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
        }
    }
}

/// Converts VideoToolbox's AVCC (4-byte length-prefixed) output to Annex B.
unsafe fn annex_b(sample: &CMSampleBuffer) -> Option<EncodedFrame> {
    let block = unsafe { sample.data_buffer() }?;
    let len = unsafe { block.data_length() };
    let mut avcc = vec![0u8; len];
    let status =
        unsafe { block.copy_data_bytes(0, len, NonNull::new_unchecked(avcc.as_mut_ptr().cast())) };
    if status != 0 {
        return None;
    }

    let mut out = Vec::with_capacity(len + 64);
    let mut keyframe = false;
    let mut i = 0;
    while i + 4 <= avcc.len() {
        let n = u32::from_be_bytes(avcc[i..i + 4].try_into().unwrap()) as usize;
        let nal = avcc.get(i + 4..i + 4 + n)?;
        if nal.first().is_some_and(|h| h & 0x1f == 5) {
            keyframe = true;
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
        i += 4 + n;
    }

    if keyframe {
        let format = unsafe { sample.format_description() }?;
        let mut params = Vec::new();
        for idx in 0..2 {
            params.extend_from_slice(&START_CODE);
            params.extend_from_slice(unsafe { parameter_set(&format, idx) }?);
        }
        out.splice(0..0, params);
    }

    Some(EncodedFrame { stream: 0, data: out, keyframe, capture_us: 0 })
}

/// Index 0 is the SPS, 1 the PPS.
unsafe fn parameter_set(format: &CMFormatDescription, idx: usize) -> Option<&[u8]> {
    let mut ptr: *const u8 = ptr::null();
    let mut size = 0usize;
    let status = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format,
            idx,
            &mut ptr,
            &mut size,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    (status == 0 && !ptr.is_null()).then(|| unsafe { std::slice::from_raw_parts(ptr, size) })
}
