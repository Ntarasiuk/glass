//! Hardware H.264 through GStreamer: VA-API (Intel, AMD; the `va` plugin) or NVENC
//! (NVIDIA; the `nvcodec` plugin).
//!
//! VA-API: the CPU converts the captured RGB to NV12 (the parallel converter OpenH264
//! uses, about 3 ms at 4K) and the GPU encodes it: handing the GPU 33 MB of RGB per 4K
//! frame to convert itself (vapostproc) was far slower, as frames are in system memory.
//! Screens above 2560x1600 are halved, as for OpenH264: encoding 4K whole took longer
//! than software at half size.
//!
//! NVENC: the RGB goes to the GPU as is (cudaupload), CUDA converts it to NV12 and NVENC
//! encodes it, so the CPU only copies. It's fast enough to encode screens whole, up to
//! the encoder's 4096 pixels a side.
//!
//! Each frame goes in and comes out before the next one (no B-frames, no lookahead), so
//! nothing is buffered. Output is Annex B with SPS/PPS before every IDR, like the OpenH264
//! path. Used when the plugin and a device are there; otherwise the host falls back to
//! OpenH264.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};

use crate::soft_encode::{Frame, MAX_PIXELS, PixelFormat, to_i420};

/// Matches the macOS and OpenH264 encoders: a periodic IDR.
const MAX_GOP_FRAMES: u32 = 900;
/// Largest picture the H.264 encoders on current GPUs take; bigger screens are halved.
const MAX_SIDE: usize = 4096;
/// How long to wait for a frame's output before giving up on the encoder.
const OUTPUT_TIMEOUT: Duration = Duration::from_millis(500);
/// Low-power (fixed-function) encoders first: they're the fastest where present.
const VA_ENCODERS: [&str; 2] = ["vah264lpenc", "vah264enc"];
/// NVENC through CUDA memory, with the CUDA elements that feed it.
const NV_ENCODERS: [&str; 1] = ["nvcudah264enc"];
const NV_HELPERS: [&str; 2] = ["cudaupload", "cudaconvertscale"];

/// Which GPU API encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Va,
    Nvenc,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Va => "VA-API",
            Self::Nvenc => "NVENC",
        }
    }
}
/// A new bitrate makes the VA encoders reconfigure, which starts with a keyframe, so not
/// every target is applied (see `should_apply`): changes of at least BITRATE_STEP, at
/// once for cuts and at most every BITRATE_EVERY for raises; anything smaller once the
/// target has gone BITRATE_SETTLE without a change.
const BITRATE_STEP: f64 = 1.1;
const BITRATE_EVERY: Duration = Duration::from_secs(2);
const BITRATE_SETTLE: Duration = Duration::from_secs(10);

/// Whether to reconfigure from `applied` to `wanted` bps, `since` the last change.
fn should_apply(applied: u32, wanted: u32, since: Duration) -> bool {
    let (a, w) = (applied as f64, wanted as f64);
    if wanted == applied {
        false
    } else if w * BITRATE_STEP <= a {
        // A real cut (congestion: AIMD cuts 30%, GCC about 15%): don't wait.
        true
    } else if w >= a * BITRATE_STEP {
        since >= BITRATE_EVERY
    } else {
        // Small steps from probing or a slowly moving estimate aren't worth a keyframe
        // each; whatever is left is applied once things settle.
        since >= BITRATE_SETTLE
    }
}

pub struct VaEncoder {
    backend: Backend,
    pipeline: gst::Pipeline,
    src: AppSrc,
    sink: AppSink,
    encoder: gst::Element,
    source: (usize, usize),
    /// 1, or 2 to halve the picture (`--scale 0.5`, or a screen too big to encode whole).
    div: usize,
    out: (usize, usize),
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    /// GStreamer's layout for what goes in: NV12 at the output size (rows padded to 4
    /// bytes) for VA-API, the captured RGB for NVENC.
    layout: gstreamer_video::VideoInfo,
    format: PixelFormat,
    next_pts: Option<gst::ClockTime>,
    frame_duration: gst::ClockTime,
    /// Bitrate (bps) the encoder runs at, the latest target, and when it last changed.
    bitrate: (u32, u32, Instant),
    pub name: &'static str,
}

/// Whether an H.264 encoder for `backend` is installed (and its device usable), and
/// which one. GStreamer registers the VA and NVENC elements only for devices it could
/// open, so a missing element means no driver or no GPU.
pub fn available(backend: Backend) -> Option<&'static str> {
    if init().is_err() {
        return None;
    }
    let found = |name: &&str| gst::ElementFactory::find(name).is_some();
    match backend {
        Backend::Va => VA_ENCODERS.into_iter().find(found),
        Backend::Nvenc => {
            NV_HELPERS.iter().all(found).then(|| NV_ENCODERS.into_iter().find(found))?
        }
    }
}

fn init() -> Result<()> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| gst::init().map_err(|e| e.to_string()))
        .clone()
        .map_err(|e| anyhow!("gstreamer: {e}"))
}

/// Whether to halve the picture: asked to (`scale` of 0.5 or less), bigger than what the
/// encoders take, or (VA-API) bigger than OpenH264's limit.
fn divisor(backend: Backend, (w, h): (usize, usize), scale: f64) -> usize {
    let big = match backend {
        Backend::Va => w * h > MAX_PIXELS,
        Backend::Nvenc => false,
    };
    if scale <= 0.5 || big || w > MAX_SIDE || h > MAX_SIDE { 2 } else { 1 }
}

fn output_size(backend: Backend, (w, h): (usize, usize), scale: f64) -> (usize, usize) {
    let div = divisor(backend, (w, h), scale);
    ((w / div) & !1, (h / div) & !1)
}

/// Whether a screen this size can be encoded (otherwise it's OpenH264's, without
/// giving up on the GPU for the other screens). NVENC takes at least 160x64.
pub fn fits(backend: Backend, width: usize, height: usize, scale: f64) -> bool {
    let (w, h) = output_size(backend, (width, height), scale);
    let min_w = if backend == Backend::Nvenc { 160 } else { 64 };
    (min_w..=MAX_SIDE).contains(&w) && (64..=MAX_SIDE).contains(&h)
}

impl VaEncoder {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backend: Backend,
        width: usize,
        height: usize,
        format: PixelFormat,
        fps: u32,
        bitrate: u32,
        scale: f64,
    ) -> Result<Self> {
        let name = available(backend).with_context(|| {
            format!("no {} H.264 encoder (GStreamer va / nvcodec plugin)", backend.label())
        })?;
        anyhow::ensure!(fits(backend, width, height, scale), "{width}x{height} can't be encoded");
        let div = divisor(backend, (width, height), scale);
        let (ow, oh) = output_size(backend, (width, height), scale);
        let kbps = (bitrate / 1000).max(100);
        let (layout, description) = match backend {
            Backend::Va => {
                let layout = gstreamer_video::VideoInfo::builder(
                    gstreamer_video::VideoFormat::Nv12,
                    ow as u32,
                    oh as u32,
                )
                .build()?;
                // to_i420's output is BT.601 limited range; like the OpenH264 path, the
                // stream carries no colour description, which decoders read as BT.601 for
                // SD and usually BT.709 for HD.
                let description = format!(
                    "appsrc name=src is-live=true format=time block=false \
                        caps=video/x-raw,format=NV12,width={ow},height={oh},framerate={fps}/1,colorimetry=bt601 \
                     ! {name} name=enc rate-control=cbr bitrate={kbps} key-int-max={MAX_GOP_FRAMES} \
                        b-frames=0 ref-frames=1 target-usage=7 \
                     ! h264parse config-interval=-1 \
                     ! video/x-h264,stream-format=byte-stream,alignment=au \
                     ! appsink name=sink sync=false max-buffers=8"
                );
                (layout, description)
            }
            Backend::Nvenc => {
                let (gst_format, caps_format) = match format {
                    PixelFormat::Bgrx => (gstreamer_video::VideoFormat::Bgrx, "BGRx"),
                    PixelFormat::Rgbx => (gstreamer_video::VideoFormat::Rgbx, "RGBx"),
                };
                let layout =
                    gstreamer_video::VideoInfo::builder(gst_format, width as u32, height as u32)
                        .build()?;
                // The converter writes BT.709 limited range for HD and the encoder puts the
                // colour description in the stream, so decoders don't have to guess.
                let description = format!(
                    "appsrc name=src is-live=true format=time block=false \
                        caps=video/x-raw,format={caps_format},width={width},height={height},framerate={fps}/1 \
                     ! cudaupload ! cudaconvertscale \
                     ! video/x-raw(memory:CUDAMemory),format=NV12,width={ow},height={oh} \
                     ! {name} name=enc preset=p1 tune=ultra-low-latency rate-control=cbr \
                        bitrate={kbps} gop-size={MAX_GOP_FRAMES} b-frames=0 zero-reorder-delay=true \
                        rc-lookahead=0 \
                     ! h264parse config-interval=-1 \
                     ! video/x-h264,stream-format=byte-stream,alignment=au \
                     ! appsink name=sink sync=false max-buffers=8"
                );
                (layout, description)
            }
        };
        let pipeline = gst::parse::launch(&description)
            .context("building the VA-API pipeline")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let by_name = |n: &str| pipeline.by_name(n).with_context(|| format!("no {n}"));
        let src = by_name("src")?
            .downcast::<AppSrc>()
            .map_err(|_| anyhow!("src isn't an appsrc"))?;
        let sink = by_name("sink")?
            .downcast::<AppSink>()
            .map_err(|_| anyhow!("sink isn't an appsink"))?;
        let encoder = by_name("enc")?;
        let this = Self {
            backend,
            pipeline,
            src,
            sink,
            encoder,
            source: (width, height),
            div,
            out: (ow, oh),
            // The CPU converts only for VA-API.
            y: if backend == Backend::Va { vec![0; ow * oh] } else { Vec::new() },
            u: if backend == Backend::Va { vec![0; ow * oh / 4] } else { Vec::new() },
            v: if backend == Backend::Va { vec![0; ow * oh / 4] } else { Vec::new() },
            layout,
            format,
            next_pts: None,
            frame_duration: gst::ClockTime::from_nseconds(1_000_000_000 / fps.max(1) as u64),
            bitrate: (bitrate, bitrate, Instant::now()),
            name,
        };
        this.pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| this.error(&format!("starting the {} pipeline", backend.label())))?;
        Ok(this)
    }

    /// Whether frames of this size and pixel format can go to this encoder (NVENC takes
    /// the captured RGB as is, so its caps name the byte order).
    pub fn accepts(&self, frame: &Frame) -> bool {
        (frame.width, frame.height) == self.source
            && (self.backend == Backend::Va || frame.format == self.format)
    }


    /// Sets the target bitrate; `should_apply` decides when the encoder follows it.
    pub fn set_bitrate(&mut self, bps: u32) {
        self.bitrate.1 = bps;
    }

    fn apply_bitrate(&mut self) {
        let (applied, wanted, at) = self.bitrate;
        if should_apply(applied, wanted, at.elapsed()) {
            self.encoder.set_property("bitrate", (wanted / 1000).max(100));
            self.bitrate = (wanted, wanted, Instant::now());
        }
    }

    /// Returns Annex B data and whether it's a keyframe, or None if nothing was produced.
    pub fn encode(&mut self, frame: &Frame, force_keyframe: bool) -> Result<Option<(Vec<u8>, bool)>> {
        anyhow::ensure!(self.accepts(frame), "frame doesn't match the encoder");
        self.apply_bitrate();
        let mut buffer = gst::Buffer::with_size(self.layout.size())?;
        {
            let buffer = buffer.get_mut().unwrap();
            // Strictly increasing timestamps, also when the same picture is sent again.
            let pts = self.next_pts.unwrap_or(gst::ClockTime::ZERO);
            self.next_pts = Some(pts + self.frame_duration);
            buffer.set_pts(pts);
            buffer.set_duration(self.frame_duration);
            if self.backend == Backend::Nvenc {
                // The RGB as captured, row by row (capture rows are often padded).
                let stride = self.layout.stride()[0] as usize;
                let row = frame.width * 4;
                let mut map = buffer.map_writable()?;
                if stride == frame.stride {
                    let n = stride * frame.height;
                    map[..n].copy_from_slice(&frame.data[..n]);
                } else {
                    for (dst, src) in map
                        .chunks_mut(stride)
                        .zip(frame.data.chunks(frame.stride))
                        .take(frame.height)
                    {
                        dst[..row].copy_from_slice(&src[..row]);
                    }
                }
            }
        }
        if self.backend == Backend::Va {
            to_i420(frame, self.div, self.out, &mut self.y, &mut self.u, &mut self.v);
            let (w, h) = self.out;
            let buffer = buffer.get_mut().unwrap();
            // NV12: the Y plane, then U and V interleaved, in GStreamer's row layout.
            let (offsets, strides) = (self.layout.offset(), self.layout.stride());
            let mut map = buffer.map_writable()?;
            let (luma, chroma) = map.split_at_mut(offsets[1]);
            for (dst, src) in luma[offsets[0]..]
                .chunks_mut(strides[0] as usize)
                .zip(self.y.chunks_exact(w))
            {
                dst[..w].copy_from_slice(src);
            }
            let cw = w / 2;
            for ((dst, u), v) in chroma
                .chunks_mut(strides[1] as usize)
                .zip(self.u.chunks_exact(cw))
                .zip(self.v.chunks_exact(cw))
                .take(h / 2)
            {
                for ((uv, u), v) in dst[..w].chunks_exact_mut(2).zip(u).zip(v) {
                    uv[0] = *u;
                    uv[1] = *v;
                }
            }
        }
        if force_keyframe && self.backend == Backend::Va {
            // A force-key-unit event makes vah264enc (GStreamer 1.24, Intel iHD) send an
            // I-frame that isn't an IDR and has no SPS, which a decoder joining the stream
            // can't start from. Reconfiguring starts a new sequence with an IDR, so nudge
            // the bitrate by 1 kbps instead.
            let kbps = (self.bitrate.0 / 1000).max(100);
            let now: u32 = self.encoder.property("bitrate");
            self.encoder.set_property("bitrate", if now == kbps { kbps + 1 } else { kbps });
        } else if force_keyframe {
            let event = gstreamer_video::DownstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            self.src.send_event(event);
        }
        self.src
            .push_buffer(buffer)
            .map_err(|e| self.error(&format!("{} input: {e:?}", self.backend.label())))?;
        let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::from_nseconds(
            OUTPUT_TIMEOUT.as_nanos() as u64,
        )) else {
            return Err(self.error(&format!("{} encoder gave no output", self.backend.label())));
        };
        self.check_bus()?;
        let out = sample.buffer().context("sample without a buffer")?;
        let keyframe = !out.flags().contains(gst::BufferFlags::DELTA_UNIT);
        let data = out.map_readable()?.to_vec();
        Ok((!data.is_empty()).then_some((data, keyframe)))
    }

    /// Empties the bus (nothing else reads it, so tags and other messages would pile up)
    /// and returns the first error on it.
    fn check_bus(&self) -> Result<()> {
        let Some(bus) = self.pipeline.bus() else {
            return Ok(());
        };
        while let Some(message) = bus.pop() {
            if let gst::MessageView::Error(e) = message.view() {
                return Err(anyhow!(
                    "{} pipeline: {} ({:?})",
                    self.backend.label(),
                    e.error(),
                    e.debug()
                ));
            }
        }
        Ok(())
    }

    /// An error with whatever the pipeline posted on its bus.
    fn error(&self, what: &str) -> anyhow::Error {
        let detail = self.pipeline.bus().and_then(|bus| {
            bus.pop_filtered(&[gst::MessageType::Error]).and_then(|m| match m.view() {
                gst::MessageView::Error(e) => Some(format!("{} ({:?})", e.error(), e.debug())),
                _ => None,
            })
        });
        match detail {
            Some(d) => anyhow!("{what}: {d}"),
            None => anyhow!("{what}"),
        }
    }
}

impl Drop for VaEncoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_changes_worth_a_keyframe() {
        let (now, soon, later, settled) = (Duration::ZERO, Duration::from_millis(500), BITRATE_EVERY, BITRATE_SETTLE);
        assert!(!should_apply(10_000_000, 10_000_000, settled));
        // Real cuts at once; small ones (500 ms reports, slow estimates) wait to settle.
        assert!(should_apply(10_000_000, 7_000_000, now));
        assert!(should_apply(10_000_000, 9_000_000, now));
        assert!(!should_apply(10_000_000, 9_700_000, soon));
        assert!(!should_apply(10_000_000, 9_700_000, later));
        assert!(should_apply(10_000_000, 9_700_000, settled));
        // Raises wait BITRATE_EVERY; small ones until settled (e.g. the last step to max).
        assert!(!should_apply(10_000_000, 14_000_000, soon));
        assert!(should_apply(10_000_000, 14_000_000, later));
        assert!(!should_apply(29_700_000, 30_000_000, later));
        assert!(should_apply(29_700_000, 30_000_000, settled));
    }

    #[test]
    fn halves_when_asked_or_big() {
        use Backend::{Nvenc, Va};
        assert_eq!(output_size(Va, (3840, 2160), 1.0), (1920, 1080));
        assert_eq!(output_size(Va, (3440, 1440), 1.0), (1720, 720));
        assert_eq!(output_size(Va, (2560, 1440), 1.0), (2560, 1440));
        assert_eq!(output_size(Va, (2560, 1440), 0.5), (1280, 720));
        assert_eq!(output_size(Va, (1366, 768), 1.0), (1366, 768));
        assert_eq!(output_size(Va, (1365, 767), 1.0), (1364, 766));
        assert!(fits(Va, 1366, 768, 1.0) && fits(Va, 7680, 4320, 1.0));
        assert!(!fits(Va, 100, 100, 0.5) && !fits(Va, 16384, 4096, 1.0));
        // NVENC encodes whole up to 4096 a side.
        assert_eq!(output_size(Nvenc, (3840, 2160), 1.0), (3840, 2160));
        assert_eq!(output_size(Nvenc, (3440, 1440), 1.0), (3440, 1440));
        assert_eq!(output_size(Nvenc, (3440, 1440), 0.5), (1720, 720));
        assert_eq!(output_size(Nvenc, (5120, 2880), 1.0), (2560, 1440));
        assert!(!fits(Nvenc, 150, 100, 1.0) && fits(Nvenc, 160, 100, 1.0));
    }

    fn desktop(width: usize, height: usize, stride: usize) -> Frame {
        let mut data = vec![0u8; stride * height];
        for (y, row) in data.chunks_exact_mut(stride).enumerate() {
            for (x, p) in row[..width * 4].chunks_exact_mut(4).enumerate() {
                p[0] = (x ^ y) as u8;
                p[1] = (x * 3 / 7) as u8;
                p[2] = ((x / 40) * 37 + (y / 20) * 11) as u8;
            }
        }
        Frame { width, height, stride, format: crate::soft_encode::PixelFormat::Bgrx, data }
    }

    /// Needs a GPU with VA-API H.264 encode and GStreamer's va plugin; skipped without:
    /// `cargo test --release -p glass-host --features vaapi va_encode -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn encodes_4k_on_the_gpu() {
        encodes_on(Backend::Va);
    }

    /// The same with NVENC (an NVIDIA GPU and GStreamer's nvcodec plugin).
    #[test]
    #[ignore]
    fn encodes_4k_with_nvenc() {
        encodes_on(Backend::Nvenc);
    }

    fn encodes_on(backend: Backend) {
        if available(backend).is_none() {
            eprintln!("no {} H.264 encoder here; skipping", backend.label());
            return;
        }
        encode_run(backend, 3840, 2160);
        encode_run(backend, 3440, 1440);
        encode_run(backend, 2560, 1440);
        if backend == Backend::Nvenc {
            // NVENC's converter writes BT.709, so the planes aren't compared to to_i420's.
            for (width, height) in [(1366, 768), (1922, 1082), (1050, 1680), (1365, 767)] {
                let frame = desktop(width, height, width * 4 + 64);
                let mut enc =
                    VaEncoder::new(backend, width, height, frame.format, 60, 8_000_000, 1.0).unwrap();
                let (data, keyframe) = enc.encode(&frame, true).unwrap().expect("output");
                assert!(keyframe);
                let mut dec = openh264::decoder::Decoder::new().unwrap();
                let picture = dec.decode(&data).unwrap().expect("decodes");
                use openh264::formats::YUVSource;
                assert_eq!(picture.dimensions(), enc.out, "{width}x{height}");
            }
            return;
        }
        // Widths that aren't a multiple of 4 have padded NV12 rows: the decoded planes must
        // match what went in (a wrong stride shears the picture).
        for (width, height) in [(1366, 768), (1922, 1082), (1050, 1680), (1365, 767)] {
            let frame = desktop(width, height, width * 4);
            let mut enc =
                VaEncoder::new(backend, width, height, frame.format, 60, 8_000_000, 1.0).unwrap();
            let (data, keyframe) = enc.encode(&frame, true).unwrap().expect("output");
            assert!(keyframe);
            let mut dec = openh264::decoder::Decoder::new().unwrap();
            let picture = dec.decode(&data).unwrap().expect("decodes");
            use openh264::formats::YUVSource;
            assert_eq!(picture.dimensions(), enc.out, "{width}x{height}");
            let (w, h) = enc.out;
            let (ys, us, vs) = picture.strides();
            let error = |plane: &[u8], stride: usize, ours: &[u8], pw: usize, ph: usize| {
                let total: u64 = (0..ph)
                    .flat_map(|y| (0..pw).map(move |x| (y, x)))
                    .map(|(y, x)| plane[y * stride + x].abs_diff(ours[y * pw + x]) as u64)
                    .sum();
                total as f64 / (pw * ph) as f64
            };
            for (name, e) in [
                ("Y", error(picture.y(), ys, &enc.y, w, h)),
                ("U", error(picture.u(), us, &enc.u, w / 2, h / 2)),
                ("V", error(picture.v(), vs, &enc.v, w / 2, h / 2)),
            ] {
                assert!(e < 3.0, "{width}x{height} {name}: mean error {e:.1}");
            }
        }
    }

    /// CPU time the whole process has used (every thread), in seconds.
    fn cpu_seconds() -> f64 {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 * 1e-6;
        t(usage.ru_utime) + t(usage.ru_stime)
    }

    /// OpenH264 against the GPU encoders on the same moving desktop, at 60 fps pacing:
    /// time per frame (what a frame waits for its encoding) and CPU per frame.
    /// `GLASS_BENCH_SIZE=3440x1440 cargo test --release -p glass-host --features vaapi \
    ///  compare_encoders -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn compare_encoders() {
        let size = std::env::var("GLASS_BENCH_SIZE").unwrap_or_else(|_| "3440x1440".into());
        let (width, height) = size
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .expect("GLASS_BENCH_SIZE=WxH");
        const WARM_UP: usize = 10;
        let frames = 300;
        let run = |label: &str, encode: &mut dyn FnMut(&Frame, bool) -> usize| {
            let mut frame = desktop(width, height, width * 4);
            let (mut wall, mut worst, mut bytes) = (0.0f64, 0.0f64, 0usize);
            let cpu = cpu_seconds();
            let start = Instant::now();
            for i in 0..frames {
                // A window being dragged: a band of changes moving across the screen.
                for row in (height / 4)..(height / 4 + 400).min(height) {
                    let x = (i * 9) % (width - 700);
                    let start = row * frame.stride + x * 4;
                    frame.data[start..start + 2400].iter_mut().for_each(|b| *b = b.wrapping_add(37));
                }
                let t = Instant::now();
                bytes += encode(&frame, i == 0);
                let took = t.elapsed().as_secs_f64();
                // The first frames include starting the pipeline.
                if i >= WARM_UP {
                    wall += took;
                    worst = worst.max(took);
                }
                // Pace at 60 fps, as the capture would.
                let due = Duration::from_secs_f64((i + 1) as f64 / 60.0);
                if let Some(rest) = due.checked_sub(start.elapsed()) {
                    std::thread::sleep(rest);
                }
            }
            let cpu = cpu_seconds() - cpu;
            let elapsed = start.elapsed().as_secs_f64();
            println!(
                "{label:<34} {:>5.1} ms/frame (worst {:>5.1}), CPU {:>5.1} ms/frame = {:>4.0}% of a core at 60 fps, {:.1} Mbps",
                wall / (frames - WARM_UP) as f64 * 1e3,
                worst * 1e3,
                cpu / frames as f64 * 1e3,
                cpu / elapsed * 100.0,
                bytes as f64 * 8.0 / elapsed / 1e6,
            );
        };
        let bitrate = 20_000_000;
        let mut soft = crate::soft_encode::Encoder::new(width, height, 60, bitrate, 1.0).unwrap();
        run(&format!("OpenH264 {width}x{height}"), &mut |f, k| {
            soft.encode(f, k).unwrap().map_or(0, |(d, _)| d.len())
        });
        drop(soft);
        for backend in [Backend::Va, Backend::Nvenc] {
            if available(backend).is_none() {
                println!("{}: not available", backend.label());
                continue;
            }
            let frame = desktop(width, height, width * 4);
            let mut enc =
                VaEncoder::new(backend, width, height, frame.format, 60, bitrate, 1.0).unwrap();
            let label = format!("{} {}x{}", enc.name, enc.out.0, enc.out.1);
            run(&label, &mut |f, k| enc.encode(f, k).unwrap().map_or(0, |(d, _)| d.len()));
        }
    }

    fn encode_run(backend: Backend, width: usize, height: usize) {
        // Padded rows, as compositors often send.
        let mut frame = desktop(width, height, width * 4 + 256);
        let mut enc =
            VaEncoder::new(backend, width, height, frame.format, 60, 30_000_000, 1.0).unwrap();
        let mut dec = openh264::decoder::Decoder::new().unwrap();
        let (frames, mut total, mut worst) = (120, 0.0f64, 0.0f64);
        for i in 0..frames {
            for row in 300..700 {
                let start = row * frame.stride + i * 120;
                frame.data[start..start + 2400].iter_mut().for_each(|b| *b = b.wrapping_add(90));
            }
            let force = i == 0 || i == 60;
            let changed = enc.bitrate.2;
            let t = std::time::Instant::now();
            let (data, keyframe) = enc.encode(&frame, force).unwrap().expect("output");
            let reconfigured = enc.bitrate.2 != changed;
            let took = t.elapsed().as_secs_f64();
            if i >= 5 {
                total += took;
                worst = worst.max(took);
            }
            // A bitrate change costs a keyframe when it's applied (should_apply decides).
            // A bitrate change costs VA-API a keyframe; NVENC may change it without one.
            assert!(!keyframe || force || reconfigured, "frame {i}: keyframes only when asked");
            assert!(keyframe || !force, "frame {i}: a keyframe when asked");
            if backend == Backend::Va {
                assert_eq!(keyframe, force || reconfigured, "frame {i}");
            }
            match i {
                31 => assert!(reconfigured, "a real cut applies at once"),
                33 => assert!(!reconfigured, "a small cut waits"),
                35 => assert!(reconfigured, "a raise applies after BITRATE_EVERY"),
                _ => {}
            }
            if keyframe {
                let nals: Vec<u8> = data
                    .windows(4)
                    .filter(|w| w[..3] == [0, 0, 1])
                    .map(|w| w[3] & 0x1f)
                    .collect();
                assert!(nals.contains(&7), "frame {i}: keyframe without SPS (NAL types {nals:?})");
            }
            // OpenH264's decoder trips over the VA encoder's later IDRs (ffmpeg decodes
            // them cleanly), so only the start is decoded here.
            if i < 3 {
                assert!(dec.decode(&data).unwrap().is_some(), "frame {i} decodes");
            }
            match i {
                30 => enc.set_bitrate(10_000_000),
                32 => enc.set_bitrate(9_700_000),
                34 => {
                    enc.bitrate.2 -= BITRATE_EVERY;
                    enc.set_bitrate(15_000_000);
                }
                _ => {}
            }
        }
        println!(
            "{}: {width}x{height} frame in, {}x{} H.264 out: {:.1} ms average, {:.1} ms worst",
            enc.name,
            enc.out.0,
            enc.out.1,
            total / (frames - 5) as f64 * 1e3,
            worst * 1e3
        );
    }
}
