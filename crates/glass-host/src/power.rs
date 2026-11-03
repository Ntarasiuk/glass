//! Restarting the computer, or just Glass, from the viewer. The viewer keeps retrying, so
//! it reconnects once Glass is back (after login, for a computer restart).

use anyhow::Result;

pub fn restart_glass() -> Result<()> {
    if crate::testmode::enabled() {
        crate::testmode::record_restart("glass");
        return Ok(());
    }
    tracing::info!("restarting Glass (asked by a viewer)");
    crate::viewers::disconnect_all();
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(300));
        // A non-zero exit: the login item / systemd unit starts it again.
        exit_now(75);
    });
    Ok(())
}

/// Exits at once. On Linux without running exit handlers: with NVENC loaded, CUDA's
/// handlers deadlocked against the encoder's threads, so a restart left Glass hanging
/// (still running, not restarted). Logs are flushed first.
fn exit_now(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(target_os = "linux")]
    // SAFETY: _exit ends the process; nothing runs after it.
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(target_os = "linux"))]
    std::process::exit(code)
}

pub fn restart_computer() -> Result<()> {
    if crate::testmode::enabled() {
        crate::testmode::record_restart("computer");
        return Ok(());
    }
    tracing::info!("restarting the computer (asked by a viewer)");
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("osascript")
        .args(["-e", "tell application \"loginwindow\" to «event aevtrrst»"])
        .status()?;
    #[cfg(target_os = "linux")]
    let status = std::process::Command::new("systemctl").arg("reboot").status()?;
    #[cfg(windows)]
    let status = std::process::Command::new("shutdown").args(["/r", "/t", "0"]).status()?;
    anyhow::ensure!(status.success(), "the system didn't accept the restart");
    Ok(())
}
