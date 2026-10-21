//! macOS: ScreenCaptureKit capture, VideoToolbox encode, CGEvent input, NSPasteboard,
//! TCC permissions, and a launchd LaunchAgent.

pub mod agent;
pub mod audio;
pub mod capture;
pub mod clipboard;
pub mod clock;
pub mod encode;
pub mod input;
pub mod menubar;
pub mod permissions;
pub mod setup;
pub mod video;

pub const OS: &str = "macos";

pub fn os_version() -> String {
    std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .map(|o| format!("macOS {}", String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_default()
}

pub fn log_dir() -> std::path::PathBuf {
    crate::home().join("Library/Logs/Glass")
}

/// Sleeping displays vanish from ScreenCaptureKit, so keep them awake while the host runs.
/// caffeinate exits when this process does.
pub fn keep_awake() -> Option<std::process::Child> {
    std::process::Command::new("caffeinate")
        .args(["-d", "-i", "-w", &std::process::id().to_string()])
        .spawn()
        .inspect_err(|e| tracing::warn!("caffeinate: {e}"))
        .ok()
}

/// Asleep displays aren't listed; declaring user activity wakes them.
pub fn wake_displays() {
    let _ = std::process::Command::new("caffeinate")
        .args(["-u", "-t", "1"])
        .status();
}
