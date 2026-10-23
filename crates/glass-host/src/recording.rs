//! Session recording: the encoded video, as sent, saved to an MP4 on the host.
//!
//! TODO(recording agent): muxing. Interface is fixed.

use std::path::PathBuf;

pub fn supported() -> bool {
    false
}

/// A recording of the host's streams that ends (and is finalized) when dropped.
pub struct Recording {
    pub path: PathBuf,
}

/// Starts recording what's streamed, for `who`.
pub fn start(who: &str) -> anyhow::Result<Recording> {
    let _ = who;
    anyhow::bail!("recording isn't available yet")
}
