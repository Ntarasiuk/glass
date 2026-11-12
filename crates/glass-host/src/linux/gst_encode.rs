//! Shared system-memory NV12 pipeline for optional VA-API and NVENC encoding.
//! Conversion/scaling is identical to OpenH264. Each input is drained before the
//! next is submitted: no B-frames, lookahead, or additional raw-frame queue.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};

use super::encode_policy::Backend;
use crate::soft_encode::{Frame, MAX_PIXELS, to_i420};

/// Matches the macOS and OpenH264 encoders: a periodic IDR.
const MAX_GOP_FRAMES: u32 = 900;
/// Conservative size budget; the selected encoder's caps are checked as well.
const MAX_SIDE: usize = 4096;
/// How long to wait for a frame's output before giving up on the encoder.
const OUTPUT_TIMEOUT: Duration = Duration::from_millis(500);
/// Low-power (fixed-function) encoders first: they're the fastest where present.
const ENCODERS: [&str; 2] = ["vah264lpenc", "vah264enc"];
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

pub struct GstEncoder {
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
    /// GStreamer's NV12 layout for the output size (rows padded to 4 bytes).
    layout: gstreamer_video::VideoInfo,
    next_pts: Option<gst::ClockTime>,
    frame_duration: gst::ClockTime,
    /// Frames a second it was made for (sizes the rate controller's buffer).
    fps: u32,
    /// Bitrate (bps) the encoder runs at, the latest target, and when it last changed.
    bitrate: (u32, u32, Instant),
    pub backend: Backend,
    pub name: &'static str,
}

/// Factory discovery only; session creation and the first frame may still fail.
pub fn available(backend: Backend) -> Option<&'static str> {
    if init().is_err() {
        return None;
    }
    let names: &[&'static str] = match backend {
        Backend::Vaapi => &ENCODERS,
        Backend::Nvenc => &["nvh264enc"],
    };
    names
        .iter()
        .copied()
        .find(|name| gst::ElementFactory::find(name).is_some())
}

// GStreamer's NVENC implementation multiplies bitrate/VBV properties by 1024.
fn nv_kbits(bps: u32) -> u32 {
    (bps / 1024).max(100)
}

fn init() -> Result<()> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| gst::init().map_err(|e| e.to_string()))
        .clone()
        .map_err(|e| anyhow!("gstreamer: {e}"))
}

/// Whether to halve the picture: asked to (`scale` of 0.5 or less), or bigger than
/// OpenH264's limit (which also keeps it within what the encoders take).
fn divisor((w, h): (usize, usize), scale: f64) -> usize {
    if scale <= 0.5 || w * h > MAX_PIXELS || w > MAX_SIDE || h > MAX_SIDE {
        2
    } else {
        1
    }
}

fn output_size((w, h): (usize, usize), scale: f64) -> (usize, usize) {
    let div = divisor((w, h), scale);
    ((w / div) & !1, (h / div) & !1)
}

/// Whether a screen fits our size budget (the backend may have tighter limits).
pub fn fits(width: usize, height: usize, scale: f64) -> bool {
    let (w, h) = output_size((width, height), scale);
    (64..=MAX_SIDE).contains(&w) && (64..=MAX_SIDE).contains(&h)
}

impl GstEncoder {
    pub fn new(
        backend: Backend,
        width: usize,
        height: usize,
        fps: u32,
        bitrate: u32,
        scale: f64,
    ) -> Result<Self> {
        let fps = fps.max(1);
        let name = available(backend)
            .with_context(|| format!("no {} H.264 encoder plugin", backend.label()))?;
        anyhow::ensure!(
            fits(width, height, scale),
            "{width}x{height} can't be encoded"
        );
        let div = divisor((width, height), scale);
        let (ow, oh) = output_size((width, height), scale);
        let layout = gstreamer_video::VideoInfo::builder(
            gstreamer_video::VideoFormat::Nv12,
            ow as u32,
            oh as u32,
        )
        .build()?;
        // The converter produces BT.601 limited-range samples for both backends.
        let settings = match backend {
            Backend::Vaapi => format!(
                "rate-control=cbr bitrate={} key-int-max={MAX_GOP_FRAMES} b-frames=0 ref-frames=1 target-usage=7",
                (bitrate / 1000).max(100)
            ),
            Backend::Nvenc => format!(
                "preset=p1 tune=ultra-low-latency rc-mode=cbr bitrate={} gop-size={MAX_GOP_FRAMES} bframes=0 rc-lookahead=0 zerolatency=true repeat-sequence-header=true",
                nv_kbits(bitrate)
            ),
        };
        let description = format!(
            "appsrc name=src is-live=true format=time block=false max-buffers=1 \
                caps=video/x-raw,format=NV12,width={ow},height={oh},framerate={fps}/1,colorimetry=bt601 \
             ! {name} name=enc {settings} \
             ! h264parse config-interval=-1 \
             ! video/x-h264,stream-format=byte-stream,alignment=au \
             ! appsink name=sink sync=false max-buffers=1",
        );
        let pipeline = gst::parse::launch(&description)
            .context("building the hardware pipeline")?
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
        // Reject unsupported geometry before allocating frame buffers or starting.
        let caps = layout.to_caps()?;
        anyhow::ensure!(
            encoder
                .static_pad("sink")
                .context("no encoder sink pad")?
                .query_caps(Some(&caps))
                .can_intersect(&caps),
            "{name} does not accept {ow}x{oh} NV12"
        );
        let this = Self {
            pipeline,
            backend,
            src,
            sink,
            encoder,
            source: (width, height),
            div,
            out: (ow, oh),
            y: vec![0; ow * oh],
            u: vec![0; ow * oh / 4],
            v: vec![0; ow * oh / 4],
            layout,
            next_pts: None,
            frame_duration: gst::ClockTime::from_nseconds(1_000_000_000 / fps.max(1) as u64),
            bitrate: (bitrate, bitrate, Instant::now()),
            fps: fps.max(1),
            name,
        };
        this.set_cpb(bitrate);
        this.pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| this.error("starting the hardware pipeline"))?;
        if backend == Backend::Nvenc {
            tracing::info!(
                "NVENC CUDA device {}, {}x{} output, {}",
                this.encoder.property::<u32>("cuda-device-id"),
                ow,
                oh,
                gst::version_string()
            );
        }
        Ok(this)
    }

    /// Size the encoder was built for; a different frame size needs a new encoder.
    pub fn source_size(&self) -> (usize, usize) {
        self.source
    }

    /// The rate controller's buffer: about two frames at the target (rate::short_window),
    /// so one frame can't take a second's worth of bits on a slow link. Older va plugins
    /// have no such property; they keep their default.
    fn set_cpb(&self, bps: u32) {
        let (property, unit) = match self.backend {
            Backend::Vaapi => ("cpb-size", 1000.0),
            Backend::Nvenc => ("vbv-buffer-size", 1024.0),
        };
        if self.encoder.find_property(property).is_some() {
            let kbits = (crate::rate::short_window(bps, self.fps).0 * 8.0 / unit) as u32;
            self.encoder.set_property(property, kbits.max(1));
        }
    }

    /// Sets the target bitrate; `should_apply` decides when the encoder follows it.
    pub fn set_bitrate(&mut self, bps: u32) {
        self.bitrate.1 = bps;
    }

    fn apply_bitrate(&mut self) {
        let (applied, wanted, at) = self.bitrate;
        if should_apply(applied, wanted, at.elapsed()) {
            let kbps = match self.backend {
                Backend::Vaapi => (wanted / 1000).max(100),
                Backend::Nvenc => nv_kbits(wanted),
            };
            self.encoder.set_property("bitrate", kbps);
            self.set_cpb(wanted);
            self.bitrate = (wanted, wanted, Instant::now());
        }
    }

    /// Returns Annex B data and whether it's a keyframe, or None if nothing was produced.
    pub fn encode(
        &mut self,
        frame: &Frame,
        force_keyframe: bool,
    ) -> Result<Option<(Vec<u8>, bool)>> {
        anyhow::ensure!(
            (frame.width, frame.height) == self.source,
            "frame doesn't match the encoder"
        );
        self.apply_bitrate();
        to_i420(
            frame,
            self.div,
            self.out,
            &mut self.y,
            &mut self.u,
            &mut self.v,
        );
        let (w, h) = self.out;
        let mut buffer = gst::Buffer::with_size(self.layout.size())?;
        {
            let buffer = buffer.get_mut().unwrap();
            // Strictly increasing timestamps, also when the same picture is sent again.
            let pts = self.next_pts.unwrap_or(gst::ClockTime::ZERO);
            self.next_pts = Some(pts + self.frame_duration);
            buffer.set_pts(pts);
            buffer.set_duration(self.frame_duration);
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
        if force_keyframe {
            let event = gstreamer_video::DownstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            self.src.send_event(event);
        }
        self.src
            .push_buffer(buffer)
            .map_err(|e| self.error(&format!("hardware input: {e:?}")))?;
        let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::from_nseconds(
            OUTPUT_TIMEOUT.as_nanos() as u64,
        )) else {
            return Err(self.error("hardware encoder gave no output"));
        };
        self.check_bus()?;
        let out = sample.buffer().context("sample without a buffer")?;
        let keyframe = !out.flags().contains(gst::BufferFlags::DELTA_UNIT);
        let data = out.map_readable()?.to_vec();
        anyhow::ensure!(
            !data.is_empty(),
            "hardware encoder returned an empty access unit"
        );
        anyhow::ensure!(
            !force_keyframe || keyframe,
            "hardware encoder ignored the keyframe request"
        );
        Ok(Some((data, keyframe)))
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
                    "hardware pipeline: {} ({:?})",
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
            bus.pop_filtered(&[gst::MessageType::Error])
                .and_then(|m| match m.view() {
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

impl Drop for GstEncoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "nvenc")]
    fn has_nal(data: &[u8], kind: u8) -> bool {
        data.windows(4)
            .any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == kind)
    }

    /// Explicit GPU tests must fail if the plugin/driver/session is unavailable.
    /// `cargo test --profile ci -p glass-host --features nvenc nvenc_roundtrip -- --ignored --nocapture`
    #[test]
    #[ignore]
    #[cfg(feature = "nvenc")]
    fn nvenc_roundtrip() {
        for (width, height, scale) in [
            (1366, 768, 1.0),
            (1365, 767, 1.0),
            (1050, 1680, 1.0),
            (1922, 1082, 1.0),
            (2560, 1440, 0.5),
            (3840, 2160, 1.0),
        ] {
            for rgb in [false, true] {
                let mut frame = desktop(width, height, width * 4 + 256);
                if rgb {
                    for row in frame.data.chunks_exact_mut(frame.stride) {
                        for p in row[..width * 4].chunks_exact_mut(4) {
                            p.swap(0, 2);
                        }
                    }
                    frame.format = crate::soft_encode::PixelFormat::Rgbx;
                }
                let mut enc =
                    GstEncoder::new(Backend::Nvenc, width, height, 60, 8_000_000, scale).unwrap();
                let mut stream_decoder = openh264::decoder::Decoder::new().unwrap();
                for i in 0..6 {
                    if i == 2 {
                        enc.set_bitrate(4_000_000);
                    }
                    if i == 4 {
                        enc.bitrate.2 -= BITRATE_EVERY;
                        enc.set_bitrate(8_000_000);
                    }
                    let force = i == 0 || i == 3;
                    // No following input and no EOS: a static screen must make progress.
                    let (data, keyframe) =
                        enc.encode(&frame, force).unwrap().expect("one access unit");
                    assert!(
                        stream_decoder.decode(&data).unwrap().is_some(),
                        "frame {i} decodes"
                    );
                    let expected = if i == 2 || i == 3 {
                        4_000_000
                    } else {
                        8_000_000
                    };
                    assert_eq!(enc.bitrate.0, expected);
                    assert_eq!(enc.encoder.property::<u32>("bitrate"), nv_kbits(expected));
                    assert_eq!(keyframe, has_nal(&data, 5));
                    if force {
                        assert!(keyframe, "forced IDR at {i}");
                    }
                    if keyframe {
                        assert!(has_nal(&data, 7) && has_nal(&data, 8), "IDR needs SPS/PPS");
                        let mut dec = openh264::decoder::Decoder::new().unwrap();
                        let picture = dec.decode(&data).unwrap().expect("standalone IDR decodes");
                        use openh264::formats::YUVSource;
                        assert_eq!(picture.dimensions(), enc.out);
                        let (ys, us, vs) = picture.strides();
                        let (w, h) = enc.out;
                        for (plane, stride, expected, pw, ph) in [
                            (picture.y(), ys, &enc.y, w, h),
                            (picture.u(), us, &enc.u, w / 2, h / 2),
                            (picture.v(), vs, &enc.v, w / 2, h / 2),
                        ] {
                            let error: u64 = (0..ph)
                                .map(|y| {
                                    (0..pw)
                                        .map(|x| {
                                            plane[y * stride + x].abs_diff(expected[y * pw + x])
                                                as u64
                                        })
                                        .sum::<u64>()
                                })
                                .sum();
                            assert!(
                                (error as f64 / (pw * ph) as f64) < 8.0,
                                "conversion/stride mismatch at {width}x{height}, RGB={rgb}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Same raw workload, output dimensions, rate target and fps for both encoders.
    /// This measures conversion + upload + encode + output copy, not display latency.
    #[test]
    #[ignore]
    #[cfg(feature = "nvenc")]
    fn nvenc_compare_software() {
        for (width, height) in [(1920, 1080), (2560, 1440), (3840, 2160)] {
            for hardware in [false, true] {
                let mut frame = desktop(width, height, width * 4 + 256);
                let mut nv = hardware.then(|| {
                    GstEncoder::new(Backend::Nvenc, width, height, 60, 8_000_000, 1.0).unwrap()
                });
                let mut soft = (!hardware).then(|| {
                    crate::soft_encode::Encoder::new(width, height, 60, 8_000_000, 1.0).unwrap()
                });
                let mut timings = Vec::new();
                let mut bytes = 0;
                for i in 0..125 {
                    for row in frame
                        .data
                        .chunks_exact_mut(frame.stride)
                        .skip(100)
                        .take(100)
                    {
                        for b in &mut row[400..1200] {
                            *b = b.wrapping_add(13);
                        }
                    }
                    let at = Instant::now();
                    let (data, _) = if let Some(e) = nv.as_mut() {
                        e.encode(&frame, i == 0 || i == 65)
                    } else {
                        soft.as_mut().unwrap().encode(&frame, i == 0 || i == 65)
                    }
                    .unwrap()
                    .expect("one access unit");
                    if i >= 5 {
                        timings.push(at.elapsed().as_secs_f64() * 1000.0);
                        bytes += data.len();
                    }
                }
                timings.sort_by(f64::total_cmp);
                let mean = timings.iter().sum::<f64>() / timings.len() as f64;
                println!(
                    "{} source={width}x{height} out={:?} mean={mean:.2}ms p50={:.2}ms p95={:.2}ms p99={:.2}ms stream={:.2}Mbps",
                    if hardware { "NVENC" } else { "OpenH264" },
                    output_size((width, height), 1.0),
                    timings[60],
                    timings[114],
                    timings[118],
                    bytes as f64 * 8.0 / 2_000_000.0
                );
            }
        }
    }

    #[test]
    fn bitrate_changes_worth_a_keyframe() {
        let (now, soon, later, settled) = (
            Duration::ZERO,
            Duration::from_millis(500),
            BITRATE_EVERY,
            BITRATE_SETTLE,
        );
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
        assert_eq!(output_size((3840, 2160), 1.0), (1920, 1080));
        assert_eq!(output_size((2560, 1440), 1.0), (2560, 1440));
        assert_eq!(output_size((2560, 1440), 0.5), (1280, 720));
        assert_eq!(output_size((1366, 768), 1.0), (1366, 768));
        assert_eq!(output_size((1365, 767), 1.0), (1364, 766));
        assert!(fits(1366, 768, 1.0) && fits(7680, 4320, 1.0));
        assert!(!fits(100, 100, 0.5) && !fits(16384, 4096, 1.0));
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
        Frame {
            width,
            height,
            stride,
            format: crate::soft_encode::PixelFormat::Bgrx,
            data,
        }
    }

    /// Needs a GPU with VA-API H.264 encode and GStreamer's va plugin; skipped without:
    /// `cargo test --release -p glass-host --features vaapi gst_encode -- --ignored --nocapture`
    #[test]
    #[ignore]
    #[cfg(feature = "vaapi")]
    fn encodes_4k_on_the_gpu() {
        if available(Backend::Vaapi).is_none() {
            eprintln!("no VA-API H.264 encoder here; skipping");
            return;
        }
        encode_run(3840, 2160);
        encode_run(2560, 1440);
        // Widths that aren't a multiple of 4 have padded NV12 rows: the decoded planes must
        // match what went in (a wrong stride shears the picture).
        for (width, height) in [(1366, 768), (1922, 1082), (1050, 1680), (1365, 767)] {
            let frame = desktop(width, height, width * 4);
            let mut enc =
                GstEncoder::new(Backend::Vaapi, width, height, 60, 8_000_000, 1.0).unwrap();
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

    #[cfg(feature = "vaapi")]
    fn encode_run(width: usize, height: usize) {
        // Padded rows, as compositors often send.
        let mut frame = desktop(width, height, width * 4 + 256);
        let mut enc = GstEncoder::new(Backend::Vaapi, width, height, 60, 30_000_000, 1.0).unwrap();
        let mut dec = openh264::decoder::Decoder::new().unwrap();
        let (frames, mut total, mut worst) = (120, 0.0f64, 0.0f64);
        for i in 0..frames {
            for row in 300..700 {
                let start = row * frame.stride + i * 120;
                frame.data[start..start + 2400]
                    .iter_mut()
                    .for_each(|b| *b = b.wrapping_add(90));
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
            assert_eq!(
                keyframe,
                force || reconfigured,
                "frame {i}: keyframes only when asked"
            );
            match i {
                31 => assert!(reconfigured, "a real cut applies at once"),
                33 => assert!(!reconfigured, "a small cut waits"),
                35 => assert!(reconfigured, "a raise applies after BITRATE_EVERY"),
                _ => {}
            }
            if keyframe {
                assert!(
                    data.windows(5)
                        .any(|w| w[..4] == [0, 0, 0, 1] && w[4] & 0x1f == 7),
                    "SPS"
                );
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
