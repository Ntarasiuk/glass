//! Which codec the shared encoders use. Every viewer gets the same stream, so HEVC is on
//! only when `glass config hevc on`, the host can encode it, and every connected viewer
//! said it can decode it (WebRTC viewers get RTP H.264, so they keep it H.264).
//!
//! TODO(hevc agent): switching. Interface is fixed.

/// Held for a session's lifetime.
pub struct Guard(pub u64);

impl Drop for Guard {
    fn drop(&mut self) {}
}

pub fn joined(session: u64, can_hevc: bool, rtp: bool) -> Guard {
    let _ = (can_hevc, rtp);
    Guard(session)
}

pub fn current() -> &'static str {
    "h264"
}
