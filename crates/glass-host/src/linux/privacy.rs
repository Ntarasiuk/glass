//! Privacy screen on Linux: not available (Wayland gives a client no way to blank the
//! physical outputs while still capturing them).

pub const SUPPORTED: bool = false;

pub fn set(on: bool) -> anyhow::Result<()> {
    let _ = on;
    anyhow::bail!("privacy mode isn't available on Linux yet")
}
