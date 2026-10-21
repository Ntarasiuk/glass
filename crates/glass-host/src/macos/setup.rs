//! macOS-specific setup steps: Screen Recording and Accessibility (TCC) grants.

use std::time::{Duration, Instant};

use anyhow::Result;
use cliclack::{log, note, spinner};

use super::{agent, permissions};
use crate::status::{self, Status};

/// Screen Recording grants often only apply after a restart; nudge the agent this often.
const RESTART_EVERY: Duration = Duration::from_secs(8);
/// Each restart is a fresh launch, and every launch shows the system prompt again, so
/// stop nudging after a few tries instead of re-prompting forever (e.g. after "Deny").
const MAX_RESTARTS: u32 = 3;

pub async fn prepare() -> Result<()> {
    Ok(())
}

pub fn prepare_unattended() -> Vec<crate::setup::Need> {
    Vec::new()
}

/// No sudo involved on macOS.
pub fn try_tailscale_operator() -> bool {
    false
}

/// Opens the panes on the Mac's screen, so whoever is there (or controlling it) can click.
pub fn permission_needs(status: &Status) -> Vec<crate::setup::Need> {
    let mut needs = Vec::new();
    if !status.capturing {
        permissions::open_pane(permissions::SCREEN_RECORDING_PANE);
        needs.push(crate::setup::Need {
            step: "screen-recording",
            action: "Allow \"glass\" in System Settings › Privacy & Security › Screen & System Audio Recording (opened on the Mac)".into(),
            on_screen: true,
        });
    }
    if !status.accessibility {
        permissions::open_pane(permissions::ACCESSIBILITY_PANE);
        needs.push(crate::setup::Need {
            step: "accessibility",
            action: "Allow \"glass\" in System Settings › Privacy & Security › Accessibility (opened on the Mac)".into(),
            on_screen: true,
        });
    }
    needs
}

pub fn permissions(status: Status) -> Result<()> {
    let status = screen_recording_step(status)?;
    accessibility_step(status)?;
    Ok(())
}

fn screen_recording_step(status: Status) -> Result<Status> {
    log::step("Screen Recording permission")?;
    if status.capturing {
        log::success("Already allowed")?;
        return Ok(status);
    }
    note(
        "Allow Glass to see your screen",
        "macOS will ask to let \"glass\" record your screen and audio.\n\
         Click Allow, or switch Glass on in the System Settings pane that opens.\n\
         If it asks to \"Quit & Reopen\", go ahead. Glass restarts itself.",
    )?;
    permissions::open_pane(permissions::SCREEN_RECORDING_PANE);
    loop {
        let sp = spinner();
        sp.start("Waiting for Screen Recording to be allowed");
        if let Some(s) = wait_for_capturing() {
            sp.stop("Screen Recording allowed, capturing your display");
            return Ok(s);
        }
        sp.error("Screen Recording still isn't allowed");
        log::warning(
            "If you clicked Deny, switch Glass on in System Settings › Privacy & Security › \
             Screen & System Audio Recording.\nIf it isn't listed, click + and choose ~/.glass/bin/glass.",
        )?;
        if !crate::setup::ask_retry("Allowed it?", "Quit setup (run `glass setup` again later)")? {
            // Leave the agent stopped so it doesn't keep prompting in the background.
            agent::stop();
            anyhow::bail!("Screen Recording isn't allowed yet; run `glass setup` again once it is");
        }
        // The grant may only apply to a fresh process.
        let _ = agent::restart();
    }
}

/// Waits for the agent to report it is capturing, restarting it at most `MAX_RESTARTS`
/// times. Returns `None` if it never does.
fn wait_for_capturing() -> Option<Status> {
    let mut restarts = 0;
    let mut last_restart = Instant::now();
    loop {
        if let Some(s) = status::read().filter(|s| s.is_fresh() && s.capturing) {
            return Some(s);
        }
        if last_restart.elapsed() > RESTART_EVERY {
            if restarts == MAX_RESTARTS {
                return None;
            }
            let _ = agent::restart();
            restarts += 1;
            last_restart = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn accessibility_step(mut status: Status) -> Result<Status> {
    log::step("Accessibility permission (mouse and keyboard)")?;
    if status.accessibility {
        log::success("Already allowed")?;
        return Ok(status);
    }
    note(
        "Allow Glass to control the mouse and keyboard",
        "In the Accessibility pane that opens, switch Glass on.\n\
         If it isn't listed, click + and choose ~/.glass/bin/glass.",
    )?;
    permissions::open_pane(permissions::ACCESSIBILITY_PANE);
    let sp = spinner();
    sp.start("Waiting for Accessibility to be allowed");
    loop {
        if let Some(s) = status::read().filter(|s| s.is_fresh() && s.accessibility) {
            status = s;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    sp.stop("Accessibility allowed, remote mouse and keyboard enabled");
    Ok(status)
}
