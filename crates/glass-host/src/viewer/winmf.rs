//! HEVC on Windows: Media Foundation's decoder (the HEVC Video Extensions, or the GPU
//! vendor's). The viewer offers HEVC, and HEVC 4:2:2 for sharp text, only where a sample
//! frame of each (made by a Mac's encoder, checked in) really decodes here: the same rule
//! a browser follows, which offers HEVC where the system can decode it.

use std::mem::ManuallyDrop;
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaType, IMFSample, IMFTransform, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_VERSION, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFSTARTUP_LITE, MFStartup,
    MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_HEVC, MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};

use super::win::Picture;

static MAIN: &[u8] = include_bytes!("testdata/hevc-main.bin");
static REXT: &[u8] = include_bytes!("testdata/hevc-422.bin");

fn startup() -> Result<()> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| {
            // SAFETY: once per process; COM may already be set up on this thread (fine).
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.map_err(|e| e.to_string())
        })
        .clone()
        .map_err(|e| anyhow!("MFStartup: {e}"))
}

/// The HEVC decoders installed, best first.
fn decoders() -> Result<Vec<IMFActivate>> {
    startup()?;
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_HEVC };
    let (mut list, mut count): (*mut Option<IMFActivate>, u32) = (std::ptr::null_mut(), 0);
    // SAFETY: MFTEnumEx fills `list` with `count` activation objects, freed here once moved out.
    unsafe {
        MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER, Some(&input), None, &mut list, &mut count)
            .context("MFTEnumEx")?;
        if list.is_null() {
            return Ok(Vec::new());
        }
        let found = std::slice::from_raw_parts_mut(list, count as usize).iter_mut().filter_map(Option::take).collect();
        CoTaskMemFree(Some(list as *const _));
        Ok(found)
    }
}

/// What decoding each sample frame here comes to (for `glass viewer-codecs`).
pub fn report() -> String {
    let one = |name: &str, sample: &[u8]| match Decoder::new().and_then(|mut d| {
        let mut got = d.decode(sample, 0)?;
        if got.is_none() {
            got = d.drain()?;
        }
        Ok(got)
    }) {
        Ok(Some(p)) => {
            if let Some(dir) = std::env::var_os("GLASS_VIEWER_SAVE") {
                let bgra = p.0.as_bytes(0).unwrap_or_default();
                let rgba: Vec<u8> = bgra.chunks_exact(4).flat_map(|c| [c[2], c[1], c[0], 255]).collect();
                let sz = p.0.size(0);
                if let Some(img) = image::RgbaImage::from_raw(sz.width.0 as u32, sz.height.0 as u32, rgba) {
                    let path = std::path::Path::new(&dir).join(format!("{}.png", name.replace([' ', ':'], "-")));
                    let mut out = Vec::new();
                    let mut enc = png::Encoder::new(&mut out, img.width(), img.height());
                    enc.set_color(png::ColorType::Rgba);
                    enc.set_depth(png::BitDepth::Eight);
                    if let Ok(mut w) = enc.write_header() {
                        let _ = w.write_image_data(img.as_raw());
                    }
                    let _ = std::fs::write(path, out);
                }
            }
            format!("{name}: decoded {:?}", p.0.size(0))
        }
        Ok(None) => format!("{name}: no picture"),
        Err(e) => format!("{name}: {e:#}"),
    };
    let names: Vec<String> = decoders().map(|l| l.iter().map(|a| friendly_name(a)).collect()).unwrap_or_default();
    let gpu = match Decoder::new() {
        Ok(d) if d.gpu.is_some() => "the GPU (DXVA)".to_string(),
        Ok(_) => format!("software ({})", gpu_manager().err().map_or("the decoder declined the GPU".into(), |e| e.to_string())),
        Err(e) => format!("no decoder ({e:#})"),
    };
    format!("decodes on {gpu}\ndecoders {names:?}\n{}\n{}", one("HEVC Main", MAIN), one("HEVC 4:2:2", REXT))
}

fn friendly_name(activate: &IMFActivate) -> String {
    use windows::Win32::Media::MediaFoundation::MFT_FRIENDLY_NAME_Attribute;
    let (mut name, mut len) = (windows::core::PWSTR::null(), 0u32);
    // SAFETY: GetAllocatedString hands over a CoTaskMemAlloc'd string, freed here.
    unsafe {
        if activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name, &mut len).is_err() {
            return "?".into();
        }
        let s = name.to_string().unwrap_or_default();
        CoTaskMemFree(Some(name.0 as *const _));
        s
    }
}

/// Whether a sample frame decodes here.
fn decodes(sample: &[u8]) -> bool {
    match Decoder::new().and_then(|mut d| {
        let mut got = d.decode(sample, 0)?;
        if got.is_none() {
            got = d.drain()?;
        }
        Ok(got)
    }) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(e) => {
            tracing::info!("viewer: HEVC decoding here: {e:#}");
            false
        }
    }
}

/// HEVC Main decodes on this PC.
pub fn hevc_supported() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let ok = decodes(MAIN);
        tracing::info!("viewer: HEVC decoding {}", if ok { "works" } else { "isn't available" });
        ok
    })
}

/// HEVC Main 4:2:2 10 (sharp text) decodes on this PC, into a picture this viewer can draw.
pub fn rext_supported() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let ok = hevc_supported() && decodes(REXT);
        tracing::info!("viewer: HEVC 4:2:2 decoding {}", if ok { "works" } else { "isn't available" });
        ok
    })
}

pub struct Decoder {
    transform: IMFTransform,
    provides_samples: bool,
    output_size: u32,
    /// As laid out (rows padded to the decoder's alignment)…
    size: (u32, u32),
    /// …and the part that's the picture.
    shown: (u32, u32),
    stride: usize,
    time: i64,
    /// The Direct3D device manager the decoder uses, when it decodes on the GPU.
    gpu: Option<windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager>,
}

/// A Direct3D 11 device (video-capable, safe across threads) wrapped for Media Foundation.
fn gpu_manager() -> windows::core::Result<windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Multithread};
    use windows::Win32::Media::MediaFoundation::MFCreateDXGIDeviceManager;
    use windows::core::Interface;
    // SAFETY: COM calls on objects made here.
    unsafe {
        let mut device = None;
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_VIDEO_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut device), None, None)?;
        let device = device.ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(-1)))?;
        // Media Foundation uses the device from its own threads.
        let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
        let (mut token, mut manager) = (0u32, None);
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(-1)))?;
        manager.ResetDevice(&device, token)?;
        Ok(manager)
    }
}

// Used from the session task only.
unsafe impl Send for Decoder {}

fn video_type(subtype: windows::core::GUID) -> Result<IMFMediaType> {
    // SAFETY: COM calls on a type we own.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
        Ok(t)
    }
}

impl Decoder {
    pub fn new() -> Result<Self> {
        let activate = decoders()?.into_iter().next().context("no HEVC decoder (the HEVC Video Extensions aren't installed)")?;
        // SAFETY: COM calls on the decoder we just made.
        unsafe {
            let transform: IMFTransform = activate.ActivateObject().context("ActivateObject")?;
            if let Ok(attrs) = transform.GetAttributes() {
                // A frame out for every frame in, nothing held back for reordering.
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            // The GPU decodes, where it can (DXVA through a Direct3D 11 device); otherwise
            // the decoder does it in software. Pictures come back the same way either way.
            let gpu = gpu_manager().and_then(|m| {
                transform.ProcessMessage(windows::Win32::Media::MediaFoundation::MFT_MESSAGE_SET_D3D_MANAGER, windows::core::Interface::as_raw(&m) as usize)?;
                Ok(m)
            });
            match &gpu {
                Ok(_) => tracing::info!("viewer: HEVC decoding on the GPU"),
                Err(e) => tracing::info!("viewer: HEVC decoding in software ({e})"),
            }
            // A provisional size: decoders offer no output type without one, and say the
            // stream's real size (a stream change) once they've read its first frame.
            let input = video_type(MFVideoFormat_HEVC)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (1920u64 << 32) | 1080)?;
            transform.SetInputType(0, &input, 0).context("SetInputType")?;
            let mut d = Self { transform, provides_samples: false, output_size: 0, size: (0, 0), shown: (0, 0), stride: 0, time: 0, gpu: gpu.ok() };
            if let Err(e) = d.choose_output() {
                tracing::debug!("viewer: HEVC output type before the stream: {e:#}");
            }
            d.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            d.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(d)
        }
    }

    /// NV12 out (what this viewer converts to BGRA); its size once the stream says.
    fn choose_output(&mut self) -> Result<()> {
        // SAFETY: COM calls.
        unsafe {
            for i in 0.. {
                let Ok(t) = self.transform.GetOutputAvailableType(0, i) else { break };
                if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                    self.transform.SetOutputType(0, &t, 0).context("SetOutputType")?;
                    if let Ok(packed) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                        self.size = ((packed >> 32) as u32, packed as u32);
                    }
                    self.shown = self.size;
                    let mut area = windows::Win32::Media::MediaFoundation::MFVideoArea::default();
                    let bytes = std::slice::from_raw_parts_mut(&mut area as *mut _ as *mut u8, std::mem::size_of_val(&area));
                    if t.GetBlob(&windows::Win32::Media::MediaFoundation::MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None).is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
                        self.shown = ((area.Area.cx as u32).min(self.size.0), (area.Area.cy as u32).min(self.size.1));
                    }
                    self.stride = t.GetUINT32(&windows::Win32::Media::MediaFoundation::MF_MT_DEFAULT_STRIDE).map_or(self.size.0 as usize, |s| (s as i32).unsigned_abs() as usize);
                    let info = self.transform.GetOutputStreamInfo(0)?;
                    let flags = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
                    self.provides_samples = info.dwFlags & flags != 0;
                    self.output_size = info.cbSize.max(self.size.0 * self.size.1 * 3 / 2);
                    return Ok(());
                }
            }
        }
        bail!("this decoder has no NV12 output (4:2:2 10-bit only?)")
    }

    /// One Annex B frame; the picture it makes, when it makes one.
    pub fn decode(&mut self, frame: &[u8], _id: u32) -> Result<Option<Picture>> {
        self.time += 166_666;
        // SAFETY: the buffer is locked for exactly its length while written.
        unsafe {
            let buffer = MFCreateMemoryBuffer(frame.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(frame.as_ptr(), ptr, frame.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(frame.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.time)?;
            self.transform.ProcessInput(0, &sample, 0).context("ProcessInput")?;
        }
        self.output()
    }

    /// Whatever's still inside (the probe's single frame).
    fn drain(&mut self) -> Result<Option<Picture>> {
        // SAFETY: COM call.
        unsafe { self.transform.ProcessMessage(windows::Win32::Media::MediaFoundation::MFT_MESSAGE_COMMAND_DRAIN, 0)? };
        self.output()
    }

    fn output(&mut self) -> Result<Option<Picture>> {
        let mut last = None;
        for _ in 0..8 {
            // SAFETY: COM calls; the sample and events go back out of their ManuallyDrop.
            unsafe {
                let mine = if self.provides_samples {
                    None
                } else {
                    let s = MFCreateSample()?;
                    s.AddBuffer(&MFCreateMemoryBuffer(self.output_size.max(1))?)?;
                    Some(s)
                };
                let mut buffers = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(mine), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
                let mut status = 0;
                let result = self.transform.ProcessOutput(0, &mut buffers, &mut status);
                let sample = ManuallyDrop::take(&mut buffers[0].pSample);
                drop(ManuallyDrop::take(&mut buffers[0].pEvents));
                match result {
                    Ok(()) => {
                        if let Some(s) = sample {
                            last = Some(self.picture(&s)?);
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(last),
                    // The stream's real format (size) is known now.
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.choose_output()?,
                    Err(e) => return Err(e).context("ProcessOutput"),
                }
            }
        }
        Ok(last)
    }

    /// NV12 (BT.709, video range) to the BGRA image GPUI draws.
    fn picture(&self, sample: &IMFSample) -> Result<Picture> {
        let (w, h) = (self.shown.0 as usize, self.shown.1 as usize);
        let rows = self.size.1 as usize;
        if w == 0 || h == 0 {
            bail!("no frame size");
        }
        // SAFETY: the contiguous buffer is read only while locked, within its length.
        let bgra = unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize);
            // The planes as laid out: the stride, and the padded rows before the colour plane.
            let stride = if self.stride * rows * 3 / 2 <= len as usize { self.stride.max(w) } else { (len as usize * 2 / 3 / rows.max(1)).max(w) };
            let out = nv12_to_bgra(data, w, h, stride, rows.max(h));
            buffer.Unlock()?;
            out
        };
        let img = image::RgbaImage::from_raw(w as u32, h as u32, bgra).context("bad frame size")?;
        Ok(Picture(std::sync::Arc::new(gpui_kit::RenderImage::new(vec![image::Frame::new(img)]))))
    }
}

/// `luma_rows`: rows in the Y plane as laid out (at least the height; padding below).
fn nv12_to_bgra(data: &[u8], w: usize, h: usize, stride: usize, luma_rows: usize) -> Vec<u8> {
    let mut out = vec![255u8; w * h * 4];
    let uv0 = stride * luma_rows;
    for y in 0..h {
        for x in 0..w {
            let yy = (*data.get(y * stride + x).unwrap_or(&16) as f32 - 16.0) * 1.164;
            let i = uv0 + (y / 2) * stride + (x & !1);
            let u = *data.get(i).unwrap_or(&128) as f32 - 128.0;
            let v = *data.get(i + 1).unwrap_or(&128) as f32 - 128.0;
            let px = &mut out[(y * w + x) * 4..][..4];
            px[2] = (yy + 1.793 * v).clamp(0.0, 255.0) as u8;
            px[1] = (yy - 0.213 * u - 0.533 * v).clamp(0.0, 255.0) as u8;
            px[0] = (yy + 2.112 * u).clamp(0.0, 255.0) as u8;
        }
    }
    out
}
