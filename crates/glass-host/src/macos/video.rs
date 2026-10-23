//! Owns the active capture + encoder pairs (one per streamed display) and switches
//! between showing one display and all of them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use glass_proto::MAX_STREAMS;
use tokio::sync::broadcast;
use tracing::info;

use super::audio::{AudioPackets, Packetizer};
use super::capture::{self, Capture, Display, DisplayInfo, Source, Window};
use super::encode::{EncodedFrame, Encoder};
pub use crate::media::Layout;
use crate::media::VideoSource;

/// H.264 level 5.2 caps a frame at 36864 macroblocks (e.g. 4096x2304); HEVC (level 6.2)
/// takes 8192x4320, so a 5K display goes out at full size only in HEVC.
const MAX_MACROBLOCKS: f64 = 36864.0;
const MAX_HEVC_PIXELS: f64 = 8192.0 * 4320.0;

pub struct VideoConfig {
    pub fps: i32,
    pub bitrate: i32,
    pub scale: f64,
}

/// What to stream: one display (main when `None`) or every display.
struct Stream {
    capture: Capture,
    encoder: Arc<Encoder>,
    display: DisplayInfo,
    keyframe: Arc<AtomicBool>,
    /// Frame-rate ceiling the capture honours (from the sharpness policy).
    fps: Arc<std::sync::atomic::AtomicU32>,
    /// Recent output bytes (decaying), to share the bitrate by activity.
    activity: std::sync::atomic::AtomicU64,
    /// Encoded width (may be below native: dynamic resolution).
    width: usize,
    /// A single window rather than a display (`display` then describes the window).
    window: bool,
}

/// What a stream captures.
enum Target<'a> {
    Display(&'a Display),
    Window(&'a Window),
}

impl Target<'_> {
    fn info(&self) -> DisplayInfo {
        match self {
            Target::Display(d) => d.info.clone(),
            Target::Window(w) => w.stream_info(),
        }
    }

    fn source(&self) -> Source<'_> {
        match self {
            Target::Display(d) => Source::Display(&d.sc),
            Target::Window(w) => Source::Window(&w.sc),
        }
    }
}

pub struct Video {
    config: VideoConfig,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    /// Shared across layout changes so audio sequence numbers stay continuous.
    audio: Arc<Mutex<Packetizer>>,
    streams: Mutex<Vec<Stream>>,
    /// Current adaptive target; new streams start at it.
    bitrate: std::sync::atomic::AtomicU32,
    /// Per stream: the width viewers need (`None` = native), from dynamic resolution.
    target_widths: Mutex<Vec<Option<u32>>>,
    /// Encode in HEVC rather than H.264 (crate::codec decides).
    hevc: AtomicBool,
    /// HEVC 4:2:2 for sharp text (crate::sharp decides; only while HEVC is on).
    rext: AtomicBool,
    /// Capture in BGRA for lossless refreshes (sharp text).
    full_colour: AtomicBool,
}

impl Video {
    pub fn new(
        config: VideoConfig,
        frames: broadcast::Sender<Arc<EncodedFrame>>,
        audio: AudioPackets,
    ) -> Self {
        let audio = Arc::new(Mutex::new(Packetizer::new(audio)));
        let bitrate = std::sync::atomic::AtomicU32::new(config.bitrate as u32);
        Self {
            config,
            frames,
            audio,
            streams: Mutex::new(Vec::new()),
            bitrate,
            target_widths: Mutex::new(Vec::new()),
            hevc: AtomicBool::new(false),
            rext: AtomicBool::new(false),
            full_colour: AtomicBool::new(false),
        }
    }

    /// The displays being streamed; index = stream number.
    pub fn streams(&self) -> Vec<DisplayInfo> {
        self.streams
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.display.clone())
            .collect()
    }

    /// Requests a keyframe on one stream, or all of them.
    pub fn request_keyframe(&self, stream: Option<u8>) {
        for (i, s) in self.streams.lock().unwrap().iter().enumerate() {
            if stream.is_none_or(|n| n as usize == i) {
                s.keyframe.store(true, Ordering::Relaxed);
                s.capture.refresh();
            }
        }
    }

    /// Restarts capture with `layout`. Blocks on ScreenCaptureKit.
    pub fn set_layout(&self, layout: Layout) -> Result<Vec<DisplayInfo>> {
        let mut streams = self.streams.lock().unwrap();
        if let Layout::Window(id) = layout {
            let windows = capture::list_windows()?;
            let window = windows
                .iter()
                .find(|w| w.info.id == id)
                .context("that window is gone")?;
            streams.clear();
            streams.push(self.start(0, &Target::Window(window), Some(self.audio.clone()))?);
            return Ok(streams.iter().map(|s| s.display.clone()).collect());
        }
        let displays = capture::list_displays()?;
        let chosen: Vec<&Display> = match layout {
            Layout::Single(Some(id)) => {
                vec![
                    displays
                        .iter()
                        .find(|d| d.info.id == id)
                        .context("unknown display")?,
                ]
            }
            Layout::Single(None) => vec![&displays[0]],
            Layout::Window(_) => unreachable!("handled above"),
            Layout::All => {
                // Match System Settings › Displays › Arrange: left to right, then top to bottom.
                let mut all: Vec<&Display> = displays.iter().take(MAX_STREAMS).collect();
                all.sort_by(|a, b| {
                    a.info
                        .x
                        .total_cmp(&b.info.x)
                        .then(a.info.y.total_cmp(&b.info.y))
                });
                all
            }
        };
        // Stop the old streams first so a stream number never has two encoders feeding it.
        streams.clear();

        for (index, display) in chosen.into_iter().enumerate() {
            // Only the first stream carries audio, so it isn't heard once per display.
            let audio = (index == 0).then(|| self.audio.clone());
            streams.push(self.start(index as u8, &Target::Display(display), audio)?);
        }
        Ok(streams.iter().map(|s| s.display.clone()).collect())
    }

    fn start(
        &self,
        index: u8,
        target: &Target,
        audio: Option<Arc<Mutex<Packetizer>>>,
    ) -> Result<Stream> {
        let info = target.info();
        let (w, h) = (info.width as f64, info.height as f64);
        let hevc = self.hevc.load(Ordering::Relaxed);
        let rext = hevc && self.rext.load(Ordering::Relaxed);
        let max_pixels = if hevc {
            MAX_HEVC_PIXELS
        } else {
            MAX_MACROBLOCKS * 256.0
        };
        let wanted = self
            .target_widths
            .lock()
            .unwrap()
            .get(index as usize)
            .copied()
            .flatten();
        let scale = self
            .config
            .scale
            .min((max_pixels / (w * h)).sqrt())
            .min(wanted.map_or(1.0, |t| t as f64 / w));
        let width = ((w * scale) as usize) & !1;
        let height = ((h * scale) as usize) & !1;

        let keyframe = Arc::new(AtomicBool::new(true));
        let frames = self.frames.clone();
        let encoder = Encoder::new(
            width as i32,
            height as i32,
            self.config.fps,
            self.config.bitrate,
            hevc,
            rext,
            keyframe.clone(),
            Arc::new(move |frame| {
                let _ = frames.send(Arc::new(EncodedFrame {
                    stream: index,
                    ..frame
                }));
            }),
        )?;
        let encoder = Arc::new(encoder);
        let fps = Arc::new(std::sync::atomic::AtomicU32::new(
            encoder.apply_policy(self.bitrate.load(Ordering::Relaxed)),
        ));
        let capture = capture::start(
            target.source(),
            width,
            height,
            self.config.fps,
            encoder.clone(),
            fps.clone(),
            audio,
            self.full_colour.load(Ordering::Relaxed),
        )?;
        let id = info.id;
        let window = matches!(target, Target::Window(_));
        info!(
            "stream {index}: {} {id} at {width}x{height} @ {}fps ({})",
            if window { "window" } else { "display" },
            self.config.fps,
            if rext {
                "HEVC 4:2:2"
            } else if hevc {
                "HEVC"
            } else {
                "H.264"
            }
        );
        Ok(Stream {
            capture,
            encoder,
            display: info,
            keyframe,
            fps,
            activity: Default::default(),
            width,
            window,
        })
    }

    /// Restarts every stream with the current codec settings (each starts on a keyframe).
    fn restart_all(&self, why: &str) {
        let Ok(displays) = capture::list_displays() else {
            return;
        };
        let mut streams = self.streams.lock().unwrap();
        let windows = if streams.iter().any(|s| s.window) {
            capture::list_windows().unwrap_or_default()
        } else {
            Vec::new()
        };
        for index in 0..streams.len() {
            let id = streams[index].display.id;
            let target = if streams[index].window {
                match windows.iter().find(|w| w.info.id == id) {
                    Some(w) => Target::Window(w),
                    None => continue,
                }
            } else {
                match displays.iter().find(|d| d.info.id == id) {
                    Some(d) => Target::Display(d),
                    None => continue,
                }
            };
            let audio = (index == 0).then(|| self.audio.clone());
            // Stop the old one first so the stream never has two encoders feeding it.
            let old = std::mem::replace(
                &mut streams[index],
                match self.start(index as u8, &target, audio) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("restarting stream {index} ({why}): {e:#}");
                        continue;
                    }
                },
            );
            drop(old);
        }
        drop(streams);
        self.rebalance();
    }

    pub fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Ok(capture::list_displays()?
            .into_iter()
            .map(|d| d.info)
            .collect())
    }

    pub fn windows(&self) -> Result<Vec<crate::media::WindowInfo>> {
        Ok(capture::list_windows()?
            .into_iter()
            .map(|w| w.info)
            .collect())
    }
}

impl VideoSource for Video {
    fn streams(&self) -> Vec<DisplayInfo> {
        Video::streams(self)
    }
    fn request_keyframe(&self, stream: Option<u8>) {
        Video::request_keyframe(self, stream)
    }
    fn set_layout(&self, layout: Layout) -> Result<Vec<DisplayInfo>> {
        Video::set_layout(self, layout)
    }
    fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Video::displays(self)
    }
    fn supports_hevc(&self) -> bool {
        super::encode::supports_hevc()
    }
    fn supports_rext(&self) -> bool {
        super::encode::supports_rext()
    }
    fn supports_lossless(&self) -> bool {
        true
    }
    /// BGRA capture, so a still screen can go out losslessly (VideoToolbox converts).
    fn set_full_colour(&self, on: bool) {
        if self.full_colour.swap(on, Ordering::Relaxed) != on {
            self.restart_all("full colour");
            info!("capture colour: {}", if on { "BGRA (lossless refresh)" } else { "4:2:0" });
        }
    }
    fn thumbnail(&self, id: u32) -> Option<crate::lossless::RawFrame> {
        capture::screenshot(id, crate::thumbs::MAX)
            .inspect_err(|e| tracing::debug!("preview of display {id}: {e:#}"))
            .ok()
    }
    fn frame_generation(&self, stream: u8) -> Option<u64> {
        let streams = self.streams.lock().unwrap();
        streams.get(stream as usize).map(|s| s.capture.generation())
    }
    fn raw_frame(&self, stream: u8) -> Option<Arc<crate::lossless::RawFrame>> {
        let streams = self.streams.lock().unwrap();
        streams.get(stream as usize)?.capture.read_bgra().map(Arc::new)
    }
    /// Sharp text: HEVC 4:2:2 instead of 4:2:0 (applies while HEVC is on).
    fn set_rext(&self, on: bool) -> bool {
        let on = on && super::encode::supports_rext();
        if self.rext.swap(on, Ordering::Relaxed) != on && self.hevc.load(Ordering::Relaxed) {
            self.restart_all("chroma");
            info!("video chroma: {}", if on { "4:2:2" } else { "4:2:0" });
        }
        on && self.hevc.load(Ordering::Relaxed)
    }
    /// Restarts every stream in the new codec (each starts on a keyframe).
    fn set_hevc(&self, hevc: bool) -> bool {
        let hevc = hevc && super::encode::supports_hevc();
        if self.hevc.swap(hevc, Ordering::Relaxed) == hevc {
            return hevc;
        }
        self.restart_all("codec");
        info!("video codec: {}", if hevc { "HEVC" } else { "H.264" });
        hevc
    }
    fn supports_windows(&self) -> bool {
        true
    }
    fn windows(&self) -> Result<Vec<crate::media::WindowInfo>> {
        Video::windows(self)
    }
    /// `bps` is the total for all displays (not each): shared out by `rebalance`.
    fn set_bitrate(&self, bps: u32) {
        self.bitrate.store(bps, Ordering::Relaxed);
        self.rebalance();
    }

    /// Restarts any stream whose encoded size should change (dynamic resolution).
    fn set_target_widths(&self, widths: &[Option<u32>]) {
        *self.target_widths.lock().unwrap() = widths.to_vec();
        let Ok(displays) = capture::list_displays() else {
            return;
        };
        let mut streams = self.streams.lock().unwrap();
        let windows = if streams.iter().any(|s| s.window) {
            capture::list_windows().unwrap_or_default()
        } else {
            Vec::new()
        };
        for (index, want) in widths.iter().enumerate() {
            let Some(current) = streams.get(index) else {
                continue;
            };
            let native = current.display.width;
            let want = want.map_or(native, |w| (w as usize).min(native));
            // Within a few pixels is the same size (rounding to even, macroblock caps).
            if current.width.abs_diff(want) <= 16 {
                continue;
            }
            let id = current.display.id;
            let target = if current.window {
                match windows.iter().find(|w| w.info.id == id) {
                    Some(w) => Target::Window(w),
                    None => continue,
                }
            } else {
                match displays.iter().find(|d| d.info.id == id) {
                    Some(d) => Target::Display(d),
                    None => continue,
                }
            };
            let audio = (index == 0).then(|| self.audio.clone());
            // Stop the old one first so the stream never has two encoders feeding it.
            let old = std::mem::replace(
                &mut streams[index],
                match self.start(index as u8, &target, audio) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("resizing stream {index}: {e:#}");
                        continue;
                    }
                },
            );
            drop(old);
            info!(
                "stream {index}: now {}px wide for its viewers",
                streams[index].width
            );
        }
        drop(streams);
        self.rebalance();
    }

    /// Splits the total between displays by recent activity, and applies the sharpness
    /// policy (bitrate, frame-rate ceiling, quality cap) to each.
    fn rebalance(&self) {
        let streams = self.streams.lock().unwrap();
        let activity: Vec<u64> = streams
            .iter()
            .map(|s| {
                let recent = s.encoder.take_bytes() + s.activity.load(Ordering::Relaxed) / 2;
                s.activity.store(recent, Ordering::Relaxed);
                recent
            })
            .collect();
        let shares = crate::rate::split(self.bitrate.load(Ordering::Relaxed), &activity);
        for (s, bps) in streams.iter().zip(shares) {
            let fps = s.encoder.apply_policy(bps);
            s.fps.store(fps, Ordering::Relaxed);
        }
    }
}
