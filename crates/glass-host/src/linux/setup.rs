//! Linux-specific setup: desktop dependencies, input access (uinput), and approving
//! screen sharing in the compositor's picker.

use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use cliclack::{confirm, log, note, spinner};
use console::style;

use super::{has_command, input};
use crate::setup::ask_retry;
use crate::status::{self, Status};

const UDEV_RULE: &str = "/etc/udev/rules.d/60-glass-uinput.rules";
const MODULES_LOAD: &str = "/etc/modules-load.d/glass-uinput.conf";

pub async fn prepare() -> Result<()> {
    dependencies_step()?;
    input_access_step()
}

fn dependencies_step() -> Result<()> {
    log::step("Desktop integration")?;
    loop {
        let missing = missing_dependencies();
        if missing.is_empty() {
            log::success("PipeWire, screen-sharing portal and wl-clipboard found")?;
            return Ok(());
        }
        log::warning(format!(
            "Missing: {}. On Arch / Omarchy:\n  {}",
            missing.join(", "),
            style(format!("sudo pacman -S --needed {}", missing.join(" "))).bold()
        ))?;
        if !ask_retry("Installed them?", "Continue anyway")? {
            return Ok(());
        }
    }
}

fn input_access_step() -> Result<()> {
    log::step("Mouse and keyboard access")?;
    if input::has_accessibility() {
        log::success("Glass can create its virtual mouse and keyboard")?;
        return Ok(());
    }
    note(
        "Allow Glass to control the mouse and keyboard",
        format!(
            "Glass injects input through /dev/uinput. A one-line udev rule gives your\n\
             logged-in session access ({UDEV_RULE}). This needs your password once."
        ),
    )?;
    if !confirm("Install the rule now?")
        .initial_value(true)
        .interact()?
    {
        log::warning(
            "Skipped: viewing works, but remote mouse and keyboard won't until this is set up.",
        )?;
        return Ok(());
    }
    let script = format!(
        "echo 'KERNEL==\"uinput\", TAG+=\"uaccess\", OPTIONS+=\"static_node=uinput\"' > {UDEV_RULE} && \
         echo uinput > {MODULES_LOAD} && modprobe uinput && \
         udevadm control --reload-rules && udevadm trigger --name-match=uinput"
    );
    let ok = Command::new("sudo")
        .args(["sh", "-c", &script])
        .status()
        .is_ok_and(|s| s.success());
    std::thread::sleep(Duration::from_millis(500));
    if ok && input::has_accessibility() {
        log::success("Input access granted")?;
    } else {
        log::warning(
            "Still no access to /dev/uinput. Log out and back in, then run `glass setup` again.",
        )?;
    }
    Ok(())
}

fn sudo_n(script: &str) -> bool {
    Command::new("sudo")
        .args(["-n", "sh", "-c", script])
        .status()
        .is_ok_and(|s| s.success())
}

fn missing_dependencies() -> Vec<&'static str> {
    let portal = std::path::Path::new("/usr/share/xdg-desktop-portal/portals")
        .read_dir()
        .is_ok_and(|mut d| {
            d.any(|e| {
                e.is_ok_and(|e| {
                    std::fs::read_to_string(e.path()).is_ok_and(|s| s.contains("ScreenCast"))
                })
            })
        });
    [
        (!has_command("pipewire")).then_some("pipewire"),
        (!portal).then_some("xdg-desktop-portal-hyprland"),
        (!has_command("wl-copy")).then_some("wl-clipboard"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Installs what it can with passwordless sudo; reports the rest.
pub fn prepare_unattended() -> Vec<crate::setup::Need> {
    let mut needs = Vec::new();
    let missing = missing_dependencies();
    if !missing.is_empty()
        && !sudo_n(&format!(
            "pacman -S --needed --noconfirm {}",
            missing.join(" ")
        ))
    {
        needs.push(crate::setup::Need {
            step: "dependencies",
            action: format!("Install: sudo pacman -S --needed {}", missing.join(" ")),
            on_screen: false,
        });
    }
    if !input::has_accessibility() {
        let script = format!(
            "echo 'KERNEL==\"uinput\", TAG+=\"uaccess\", OPTIONS+=\"static_node=uinput\"' > {UDEV_RULE} && \
             echo uinput > {MODULES_LOAD} && modprobe uinput && \
             udevadm control --reload-rules && udevadm trigger --name-match=uinput"
        );
        if !(sudo_n(&script) && input::has_accessibility()) {
            needs.push(crate::setup::Need {
                step: "input-access",
                action: "Grant /dev/uinput access: run `glass setup` on the machine (asks for sudo once)".into(),
                on_screen: false,
            });
        }
    }
    needs
}

pub fn try_tailscale_operator() -> bool {
    let user = std::env::var("USER").unwrap_or_default();
    !user.is_empty() && sudo_n(&format!("tailscale set --operator={user}"))
}

pub fn permission_needs(status: &Status) -> Vec<crate::setup::Need> {
    let mut needs = Vec::new();
    if !status.capturing {
        needs.push(crate::setup::Need {
            step: "screen-share",
            action: "Approve screen sharing in the desktop's share picker (shown on the machine's screen)".into(),
            on_screen: true,
        });
    }
    if !status.accessibility {
        needs.push(crate::setup::Need {
            step: "input-access",
            action: "Log out and back in so the uinput rule applies, or run `glass setup` there"
                .into(),
            on_screen: false,
        });
    }
    needs
}

pub fn permissions(status: Status) -> Result<()> {
    log::step("Screen sharing")?;
    if status.capturing {
        log::success("Already sharing")?;
    } else {
        note(
            "Choose what to share",
            "Your desktop will ask which screen Glass may share. Pick the screen(s)\n\
             you want to control. Glass remembers the choice, so this is a one-time step.",
        )?;
        let sp = spinner();
        sp.start("Waiting for screen sharing to be approved");
        loop {
            if status::read().is_some_and(|s| s.is_fresh() && s.capturing) {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        sp.stop("Screen sharing approved");
    }
    if !status::read().is_some_and(|s| s.accessibility) {
        log::warning(
            "The service can't open /dev/uinput yet, so remote input is off. Re-run `glass setup` after logging out and in.",
        )?;
    }
    Ok(())
}
