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
}

/// Fixed-size PCM packets (see `glass_proto::audio_packet`), fanned out to sessions.
pub type AudioPackets = broadcast::Sender<Arc<Vec<u8>>>;

/// What to stream: one display (the main one when `None`) or every display.
#[derive(Debug, Clone, Copy)]
pub enum Layout {
    Single(Option<u32>),
    All,
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
}

/// The host clipboard (text).
pub trait Clipboard: Send + Sync {
    fn set_text(&self, text: &str);
}
