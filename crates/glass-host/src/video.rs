//! Owns the active capture + encoder pairs (one per streamed display) and switches
//! between showing one display and all of them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use glass_proto::MAX_STREAMS;
use tokio::sync::broadcast;
use tracing::info;

use crate::audio::{AudioPackets, Packetizer};
use crate::capture::{self, Capture, Display, DisplayInfo};
use crate::encode::{EncodedFrame, Encoder};

const MAX_MACROBLOCKS: f64 = 36864.0;

pub struct VideoConfig {
    pub fps: i32,
    pub bitrate: i32,
    pub scale: f64,
}

/// What to stream: one display (main when `None`) or every display.
#[derive(Debug, Clone, Copy)]
pub enum Layout {
    Single(Option<u32>),
    All,
}

struct Stream {
    capture: Capture,
    display: DisplayInfo,
    keyframe: Arc<AtomicBool>,
}

pub struct Video {
    config: VideoConfig,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    /// Shared across layout changes so audio sequence numbers stay continuous.
    audio: Arc<Mutex<Packetizer>>,
    streams: Mutex<Vec<Stream>>,
}

impl Video {
    pub fn new(
        config: VideoConfig,
        frames: broadcast::Sender<Arc<EncodedFrame>>,
        audio: AudioPackets,
    ) -> Self {
        let audio = Arc::new(Mutex::new(Packetizer::new(audio)));
        Self { config, frames, audio, streams: Mutex::new(Vec::new()) }
    }

    /// The displays being streamed; index = stream number.
    pub fn streams(&self) -> Vec<DisplayInfo> {
        self.streams.lock().unwrap().iter().map(|s| s.display.clone()).collect()
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
                vec![displays.iter().find(|d| d.info.id == id).context("unknown display")?]
            }
            Layout::Single(None) => vec![&displays[0]],
            Layout::All => {
                // Match System Settings › Displays › Arrange: left to right, then top to bottom.
                let mut all: Vec<&Display> = displays.iter().take(MAX_STREAMS).collect();
                all.sort_by(|a, b| a.info.x.total_cmp(&b.info.x).then(a.info.y.total_cmp(&b.info.y)));
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

    fn start(&self, index: u8, display: &Display, audio: Option<Arc<Mutex<Packetizer>>>) -> Result<Stream> {
        let (w, h) = (display.info.width as f64, display.info.height as f64);
        // H.264 level 5.2 caps a frame at 36864 macroblocks (e.g. 4096x2304), so a 5K
        // display is downscaled until HEVC lands.
        let scale = self.config.scale.min((MAX_MACROBLOCKS * 256.0 / (w * h)).sqrt());
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
                let _ = frames.send(Arc::new(EncodedFrame { stream: index, ..frame }));
            }),
        )?;
        let capture = capture::start(&display.sc, width, height, self.config.fps, Arc::new(encoder), audio)?;
        let id = display.info.id;
        info!("stream {index}: display {id} at {width}x{height} @ {}fps", self.config.fps);
        Ok(Stream { capture, display: display.info.clone(), keyframe })
    }

    pub fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Ok(capture::list_displays()?.into_iter().map(|d| d.info).collect())
    }
}
