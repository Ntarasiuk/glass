//! Hardware H.264 through GStreamer's VA-API plugin (Intel, AMD). The CPU converts the
//! captured RGB to NV12 (the parallel converter OpenH264 uses, about 3 ms at 4K) and the
//! GPU encodes it: handing the GPU 33 MB of RGB per 4K frame to convert itself (vapostproc)
//! was far slower, as frames are in system memory. Each frame goes in and comes out before
//! the next one (no B-frames, no lookahead), so nothing is buffered. Output is Annex B
//! with SPS/PPS before every IDR, like the OpenH264 path. Used when the plugin and a VA
//! device are there; otherwise the host falls back to OpenH264.

use std::sync::Once;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};

use crate::soft_encode::{Frame, to_i420};

/// Matches the macOS and OpenH264 encoders: a periodic IDR.
const MAX_GOP_FRAMES: u32 = 900;
/// Largest picture the H.264 encoders on current GPUs take; bigger screens are halved.
const MAX_SIDE: usize = 4096;
/// How long to wait for a frame's output before giving up on the encoder.
const OUTPUT_TIMEOUT: Duration = Duration::from_millis(500);
/// Low-power (fixed-function) encoders first: they're the fastest where present.
const ENCODERS: [&str; 2] = ["vah264lpenc", "vah264enc"];
/// A new bitrate makes the VA encoders reconfigure, which starts with a keyframe, so
/// changes are applied at most this often (the latest target wins).
const BITRATE_EVERY: Duration = Duration::from_secs(2);

pub struct VaEncoder {
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
    next_pts: Option<gst::ClockTime>,
    frame_duration: gst::ClockTime,
    /// Bitrate (bps) the encoder runs at, the latest target, and when it last changed.
    bitrate: (u32, u32, Instant),
    pub name: &'static str,
}

/// Whether a VA-API H.264 encoder is installed, and which one.
pub fn available() -> Option<&'static str> {
    if init().is_err() {
        return None;
    }
    ENCODERS
        .into_iter()
        .find(|name| gst::ElementFactory::find(name).is_some())
}

fn init() -> Result<()> {
    static ONCE: Once = Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| result = gst::init().map_err(|e| anyhow!("gstreamer: {e}")));
    result
}

/// Whether to halve the picture: asked to (`scale` of 0.5 or less), or too big to encode.
fn divisor((w, h): (usize, usize), scale: f64) -> usize {
    if scale <= 0.5 || w > MAX_SIDE || h > MAX_SIDE { 2 } else { 1 }
}

impl VaEncoder {
    pub fn new(
        width: usize,
        height: usize,
        fps: u32,
        bitrate: u32,
        scale: f64,
    ) -> Result<Self> {
        let name = available().context("no VA-API H.264 encoder (GStreamer va plugin)")?;
        let div = divisor((width, height), scale);
        let (ow, oh) = ((width / div) & !1, (height / div) & !1);
        anyhow::ensure!(ow <= MAX_SIDE && oh <= MAX_SIDE && ow >= 64 && oh >= 64, "{ow}x{oh}");
        // BT.601 limited range: what to_i420 produces, written into the stream's VUI.
        let description = format!(
            "appsrc name=src is-live=true format=time block=false \
                caps=video/x-raw,format=NV12,width={ow},height={oh},framerate={fps}/1,colorimetry=bt601 \
             ! {name} name=enc rate-control=cbr bitrate={kbps} key-int-max={MAX_GOP_FRAMES} \
                b-frames=0 ref-frames=1 target-usage=7 \
             ! h264parse config-interval=-1 \
             ! video/x-h264,stream-format=byte-stream,alignment=au \
             ! appsink name=sink sync=false max-buffers=8",
            kbps = (bitrate / 1000).max(100),
        );
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
            pipeline,
            src,
            sink,
            encoder,
            source: (width, height),
            div,
            out: (ow, oh),
            y: vec![0; ow * oh],
            u: vec![0; ow * oh / 4],
            v: vec![0; ow * oh / 4],
            next_pts: None,
            frame_duration: gst::ClockTime::from_nseconds(1_000_000_000 / fps.max(1) as u64),
            bitrate: (bitrate, bitrate, Instant::now()),
            name,
        };
        this.pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| this.error("starting the VA-API pipeline"))?;
        Ok(this)
    }

    /// Size the encoder was built for; a different frame size needs a new encoder.
    pub fn source_size(&self) -> (usize, usize) {
        self.source
    }

    /// Changes the target bitrate, from the next frame or once BITRATE_EVERY has passed
    /// since the last change (each change costs a keyframe).
    pub fn set_bitrate(&mut self, bps: u32) {
        self.bitrate.1 = bps;
    }

    fn apply_bitrate(&mut self) {
        let (applied, wanted, at) = self.bitrate;
        if wanted != applied && at.elapsed() >= BITRATE_EVERY {
            self.encoder.set_property("bitrate", (wanted / 1000).max(100));
            self.bitrate = (wanted, wanted, Instant::now());
        }
    }

    /// Returns Annex B data and whether it's a keyframe, or None if nothing was produced.
    pub fn encode(&mut self, frame: &Frame, force_keyframe: bool) -> Result<Option<(Vec<u8>, bool)>> {
        anyhow::ensure!(
            (frame.width, frame.height) == self.source,
            "frame doesn't match the encoder"
        );
        self.apply_bitrate();
        to_i420(frame, self.div, self.out, &mut self.y, &mut self.u, &mut self.v);
        let (w, h) = self.out;
        let mut buffer = gst::Buffer::with_size(w * h * 3 / 2)?;
        {
            let buffer = buffer.get_mut().unwrap();
            // Strictly increasing timestamps, also when the same picture is sent again.
            let pts = self.next_pts.unwrap_or(gst::ClockTime::ZERO);
            self.next_pts = Some(pts + self.frame_duration);
            buffer.set_pts(pts);
            buffer.set_duration(self.frame_duration);
            // NV12: the Y plane, then U and V interleaved.
            let mut map = buffer.map_writable()?;
            let (luma, chroma) = map.split_at_mut(w * h);
            luma.copy_from_slice(&self.y);
            for ((uv, u), v) in chroma.chunks_exact_mut(2).zip(&self.u).zip(&self.v) {
                uv[0] = *u;
                uv[1] = *v;
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
            .map_err(|e| self.error(&format!("VA-API input: {e:?}")))?;
        let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::from_nseconds(
            OUTPUT_TIMEOUT.as_nanos() as u64,
        )) else {
            return Err(self.error("VA-API encoder gave no output"));
        };
        let out = sample.buffer().context("sample without a buffer")?;
        let keyframe = !out.flags().contains(gst::BufferFlags::DELTA_UNIT);
        let data = out.map_readable()?.to_vec();
        Ok((!data.is_empty()).then_some((data, keyframe)))
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
    fn halves_when_asked_or_too_big() {
        assert_eq!(divisor((3840, 2160), 1.0), 1);
        assert_eq!(divisor((3840, 2160), 0.5), 2);
        assert_eq!(divisor((5120, 2880), 1.0), 2);
        assert_eq!(divisor((1366, 768), 1.0), 1);
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
    /// `cargo test --release -p glass-host va_encode -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn encodes_4k_on_the_gpu() {
        if available().is_none() {
            eprintln!("no VA-API H.264 encoder here; skipping");
            return;
        }
        encode_4k(1.0);
        encode_4k(0.5);
    }

    fn encode_4k(scale: f64) {
        let (width, height) = (3840, 2160);
        // Padded rows, as compositors often send.
        let mut frame = desktop(width, height, width * 4 + 256);
        let mut enc = VaEncoder::new(width, height, 60, 30_000_000, scale).unwrap();
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
            // A bitrate change costs a keyframe when it's applied.
            assert_eq!(keyframe, force || reconfigured, "frame {i}: keyframes only when asked");
            assert_eq!(reconfigured, i == 31 || (i > 32 && keyframe && !force), "frame {i}");
            if keyframe {
                assert!(data.windows(5).any(|w| w[..4] == [0, 0, 0, 1] && w[4] & 0x1f == 7), "SPS");
            }
            if i < 3 {
                assert!(dec.decode(&data).unwrap().is_some(), "frame {i} decodes");
            }
            if i == 30 {
                enc.bitrate.2 -= BITRATE_EVERY;
                enc.set_bitrate(10_000_000);
            }
            if i == 32 {
                // Too soon after the last change: waits until BITRATE_EVERY has passed.
                enc.set_bitrate(5_000_000);
            }
        }
        println!(
            "{}: 4K frame in, {}x{} H.264 out: {:.1} ms average, {:.1} ms worst",
            enc.name,
            enc.out.0,
            enc.out.1,
            total / (frames - 5) as f64 * 1e3,
            worst * 1e3
        );
    }
}
