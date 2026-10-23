//! Types shared by the platform capture/encode code and the server.

use std::sync::Arc;

use tokio::sync::broadcast;

/// A capturable display. `id` is platform-specific (CGDirectDisplayID on macOS, the
/// portal stream index on Linux).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DisplayInfo {
    pub id: u32,
    pub width: usize,
    pub height: usize,
    /// Position and size in the desktop's display arrangement (global points).
    pub x: f64,
    pub y: f64,
    pub points_width: f64,
    pub points_height: f64,
    pub main: bool,
    pub builtin: bool,
}

pub struct EncodedFrame {
    /// Display stream number, set by the stream that owns the encoder.
    pub stream: u8,
    /// Annex B H.264 with SPS/PPS before every keyframe.
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub capture_us: u64,
    /// HEVC (Annex B, VPS/SPS/PPS before keyframes) instead of H.264.
    pub hevc: bool,
}

/// A window that can be streamed on its own (single-app streaming).
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowInfo {
    pub id: u32,
    pub app: String,
    pub title: String,
    /// Pixel size.
    pub width: usize,
    pub height: usize,
    /// Position and size on the desktop (global points), for input mapping.
    pub x: f64,
    pub y: f64,
    pub points_width: f64,
    pub points_height: f64,
}

/// Fixed-size PCM packets (see `glass_proto::audio_packet`), fanned out to sessions.
pub type AudioPackets = broadcast::Sender<Arc<Vec<u8>>>;

/// What to stream: one display (the main one when `None`) or every display.
#[derive(Debug, Clone, Copy)]
pub enum Layout {
    Single(Option<u32>),
    All,
    /// One window, by `WindowInfo::id`.
    Window(u32),
}

/// A source of encoded display streams (the platform capturer, or the test pattern).
pub trait VideoSource: Send + Sync {
    /// The displays being streamed; index = stream number.
    fn streams(&self) -> Vec<DisplayInfo>;
    /// Requests a keyframe on one stream, or all of them.
    fn request_keyframe(&self, stream: Option<u8>);
    /// Restarts capture with `layout`. May block.
    fn set_layout(&self, layout: Layout) -> anyhow::Result<Vec<DisplayInfo>>;
    /// Every capturable display. May block.
    fn displays(&self) -> anyhow::Result<Vec<DisplayInfo>>;
    /// Target bitrate for each stream, in bits per second (adaptive rate control).
    fn set_bitrate(&self, bps: u32);
    /// Re-shares the bitrate between displays (called every second).
    fn rebalance(&self) {}
    /// Encodes each stream at this width (`None` = native), from what viewers show.
    fn set_target_widths(&self, _widths: &[Option<u32>]) {}
    /// Can this source stream single windows?
    fn supports_windows(&self) -> bool {
        false
    }
    /// Windows that can be streamed on their own. May block.
    fn windows(&self) -> anyhow::Result<Vec<WindowInfo>> {
        Ok(Vec::new())
    }
    /// Encode in HEVC (`true`) or H.264; returns the codec now in use.
    fn set_hevc(&self, _hevc: bool) -> bool {
        false
    }
    /// Can this source encode HEVC at all?
    fn supports_hevc(&self) -> bool {
        false
    }
}

/// Where client input goes (the platform injector, or a recorder in tests).
pub trait InputSink: Send {
    fn handle(&mut self, ev: glass_proto::InputEvent);
    /// Pastes the host clipboard (the platform's paste shortcut).
    fn paste(&mut self);
    /// Releases anything still held, e.g. when a client disconnects mid-drag.
    fn release_all(&mut self);
    /// Retargets absolute mouse coordinates to the streamed displays.
    fn set_displays(&mut self, displays: &[u32]);
    /// Maps absolute coordinates on stream 0 to this rectangle of the desktop (global
    /// points) instead of a display: a streamed window. `None` goes back to displays.
    fn set_window_rect(&mut self, _rect: Option<[f64; 4]>) {}
}

/// The host clipboard (text).
pub trait Clipboard: Send + Sync {
    fn set_text(&self, text: &str);
}
