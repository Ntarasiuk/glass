//! macOS-specific setup steps: Screen Recording and Accessibility (TCC) grants.

use std::time::{Duration, Instant};

use anyhow::Result;
use cliclack::{log, note, spinner};

use super::{agent, permissions};
use crate::status::{self, Status};

/// Screen Recording grants often only apply after a restart; nudge the agent this often.
const RESTART_EVERY: Duration = Duration::from_secs(8);

pub async fn prepare() -> Result<()> {
    Ok(())
}

pub fn permissions(status: Status) -> Result<()> {
    let status = screen_recording_step(status)?;
    accessibility_step(status)?;
    Ok(())
}

fn screen_recording_step(mut status: Status) -> Result<Status> {
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
    let sp = spinner();
    sp.start("Waiting for Screen Recording to be allowed");
    let mut last_restart = Instant::now();
    loop {
        if let Some(s) = status::read().filter(|s| s.is_fresh() && s.capturing) {
            status = s;
            break;
        }
        if last_restart.elapsed() > RESTART_EVERY {
            let _ = agent::restart();
            last_restart = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    sp.stop("Screen Recording allowed, capturing your display");
    Ok(status)
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

