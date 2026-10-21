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
