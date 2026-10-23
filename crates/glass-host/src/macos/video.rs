//! Owns the active capture + encoder pairs (one per streamed display) and switches
//! between showing one display and all of them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use glass_proto::MAX_STREAMS;
use tokio::sync::broadcast;
use tracing::info;

use super::audio::{AudioPackets, Packetizer};
use super::capture::{self, Capture, Display, DisplayInfo};
use super::encode::{EncodedFrame, Encoder};
pub use crate::media::Layout;
use crate::media::VideoSource;

const MAX_MACROBLOCKS: f64 = 36864.0;

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
            Layout::Window(_) => anyhow::bail!("streaming one window isn't available yet"),
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
            streams.push(self.start(index as u8, display, audio)?);
        }
        Ok(streams.iter().map(|s| s.display.clone()).collect())
    }

    fn start(
        &self,
        index: u8,
        display: &Display,
        audio: Option<Arc<Mutex<Packetizer>>>,
    ) -> Result<Stream> {
        let (w, h) = (display.info.width as f64, display.info.height as f64);
        // H.264 level 5.2 caps a frame at 36864 macroblocks (e.g. 4096x2304), so a 5K
        // display is downscaled until HEVC lands.
        let target = self
            .target_widths
            .lock()
            .unwrap()
            .get(index as usize)
            .copied()
            .flatten();
        let scale = self
            .config
            .scale
            .min((MAX_MACROBLOCKS * 256.0 / (w * h)).sqrt())
            .min(target.map_or(1.0, |t| t as f64 / w));
        let width = ((w * scale) as usize) & !1;
        let height = ((h * scale) as usize) & !1;

        let keyframe = Arc::new(AtomicBool::new(true));
        let frames = self.frames.clone();
        let encoder = Encoder::new(
            width as i32,
            height as i32,
            self.config.fps,
            self.config.bitrate,
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
            &display.sc,
            width,
            height,
            self.config.fps,
            encoder.clone(),
            fps.clone(),
            audio,
        )?;
        let id = display.info.id;
        info!(
            "stream {index}: display {id} at {width}x{height} @ {}fps",
            self.config.fps
        );
        Ok(Stream {
            capture,
            encoder,
            display: display.info.clone(),
            keyframe,
            fps,
            activity: Default::default(),
            width,
        })
    }

    pub fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Ok(capture::list_displays()?
            .into_iter()
            .map(|d| d.info)
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
            let Some(display) = displays.iter().find(|d| d.info.id == current.display.id) else {
                continue;
            };
            let audio = (index == 0).then(|| self.audio.clone());
            // Stop the old one first so the stream never has two encoders feeding it.
            let old = std::mem::replace(
                &mut streams[index],
                match self.start(index as u8, display, audio) {
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
