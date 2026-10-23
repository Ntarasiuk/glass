//! Privacy screen on macOS. TODO(privacy agent).

pub const SUPPORTED: bool = false;

pub fn set(on: bool) -> anyhow::Result<()> {
    let _ = on;
    anyhow::bail!("privacy mode isn't available yet")
}
