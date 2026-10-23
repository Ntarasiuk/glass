//! The viewer's microphone, as a microphone on the host (KIND_MIC datagrams).
//!
//! TODO(mic agent): playback/virtual source. Interface is fixed.

pub fn supported() -> bool {
    crate::testmode::enabled()
}

/// One KIND_MIC datagram from a session that turned its mic on.
pub fn packet(data: &[u8]) {
    if crate::testmode::enabled() {
        crate::testmode::record_mic(data.len());
    }
}

/// A session started (true) or stopped (false) sending.
pub fn active(on: bool) {
    let _ = on;
}
