//! `glass setup`: an interactive walkthrough from fresh install to a working connection.

use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use cliclack::{confirm, intro, log, note, outro, spinner};
use console::style;

use crate::status::{self, Status};
use crate::{agent, platform, tailscale};

#[cfg(target_os = "macos")]
const TAILSCALE_DOWNLOAD: &str = "https://tailscale.com/download/mac";
#[cfg(target_os = "linux")]
const TAILSCALE_DOWNLOAD: &str = "https://tailscale.com/download/linux (on Arch/Omarchy: sudo pacman -S tailscale)";
const TAILSCALE_DNS_ADMIN: &str = "https://login.tailscale.com/admin/dns";
const START_TIMEOUT: Duration = Duration::from_secs(20);

pub async fn run() -> Result<()> {
    intro(style(" glass ").black().on_cyan())?;
    log::remark("Remote control for this computer from any browser, with very low latency.")?;

    let tailnet = tailscale_step().await?;
    platform::setup::prepare().await?;
    service_step()?;
    let status = wait_for_host()?;
    platform::setup::permissions(status)?;
    finish(tailnet)
}

/// Returns this Mac's tailnet name when remote access is set up.
async fn tailscale_step() -> Result<Option<String>> {
    log::step("Remote access with Tailscale")?;
    loop {
        match tailscale::probe().await {
            tailscale::Probe::Missing => {
                log::warning(format!(
                    "Tailscale isn't installed. It's free and gives your devices a private, secure link.\n\
                     Install it here and on your other devices: {TAILSCALE_DOWNLOAD}"
                ))?;
                if !ask_retry("Installed it?", "Skip (use Glass only on this computer)")? {
                    return Ok(None);
                }
            }
            tailscale::Probe::LoggedOut => {
                log::warning("Tailscale is installed but not signed in. Open Tailscale from the menu bar and log in.")?;
                if !ask_retry("Signed in?", "Skip remote access for now")? {
                    return Ok(None);
                }
            }
            tailscale::Probe::NoHttps { name } => {
                log::warning(format!(
                    "Browsers need HTTPS for Glass. Turn on {} for your tailnet\n\
                     (one switch at the bottom of {TAILSCALE_DNS_ADMIN}), so {name} can get a certificate.",
                    style("HTTPS Certificates").bold()
                ))?;
                let _ = std::process::Command::new("open").arg(TAILSCALE_DNS_ADMIN).status();
                if !ask_retry("Turned it on?", "Skip remote access for now")? {
                    return Ok(None);
                }
            }
            tailscale::Probe::Ready { name } => {
                let sp = spinner();
                sp.start(format!("Getting a certificate for {name}"));
                match tailscale::ensure_cert(&crate::config_dir().join("certs")).await {
                    Some(_) => {
                        sp.stop(format!("Tailscale ready: {}", style(&name).cyan()));
                        return Ok(Some(name));
                    }
                    None => {
                        sp.error("Couldn't get a certificate from Tailscale");
                        // On Linux, non-root users need to be Tailscale "operators" to fetch certs.
                        #[cfg(target_os = "linux")]
                        if confirm("Allow your user to manage Tailscale certificates? (runs `sudo tailscale set --operator=$USER`)")
                            .initial_value(true)
                            .interact()?
                        {
                            let user = std::env::var("USER").unwrap_or_default();
                            let _ = std::process::Command::new("sudo")
                                .args(["tailscale", "set", &format!("--operator={user}")])
                                .status();
                            continue;
                        }
                        if !ask_retry("Try again?", "Skip remote access for now")? {
                            return Ok(None);
                        }
                    }
                }
            }
        }
    }
}

fn service_step() -> Result<()> {
    log::step("Background service")?;
    let at_login = confirm("Start Glass automatically when you log in?").initial_value(true).interact()?;
    let sp = spinner();
    sp.start("Installing the Glass service");
    agent::install(at_login)?;
    sp.stop(if at_login { "Glass runs in the background and starts at login" } else { "Glass is running in the background" });
    Ok(())
}

fn wait_for_host() -> Result<Status> {
    let sp = spinner();
    sp.start("Starting Glass");
    let started = Instant::now();
    loop {
        if let Some(s) = status::read().filter(|s| s.is_fresh()) {
            sp.stop(format!("Glass {} is running", s.version));
            return Ok(s);
        }
        if started.elapsed() > START_TIMEOUT {
            sp.error("Glass didn't start");
            anyhow::bail!("see ~/Library/Logs/Glass/agent.log for details");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn finish(tailnet: Option<String>) -> Result<()> {
    let token = std::fs::read_to_string(crate::default_token_file()).unwrap_or_default();
    let token = token.trim();
    match tailnet {
        Some(name) => {
            let url = format!("https://{name}:8080/");
            note(
                "Connect from your iPad",
                format!(
                    "{qr}\n\
                     Scan this, or open {url} on any device signed in to your Tailscale.\n\
                     Tip: Share › Add to Home Screen for a true fullscreen app.\n\n\
                     Devices outside your tailnet need this token: {token}",
                    qr = qr(&url),
                    url = style(&url).cyan().underlined(),
                ),
            )?;
        }
        None => {
            note(
                "Use Glass on this computer",
                format!(
                    "Open {} to try it here.\n\
                     Run {} again once Tailscale is set up to connect from other devices.",
                    style("http://localhost:8080/").cyan(),
                    style("glass setup").bold(),
                ),
            )?;
        }
    }
    outro(format!(
        "All set. Problems or ideas? {}",
        style("glass feedback \"…\"").bold()
    ))?;
    Ok(())
}

/// Confirm with a retry/skip choice. Returns true to retry.
pub(crate) fn ask_retry(retry: &str, skip: &str) -> io::Result<bool> {
    cliclack::select(retry).item(true, "Yes, check again", "").item(false, skip, "").interact()
}

fn qr(text: &str) -> String {
    use qrcode::render::unicode::Dense1x2;
    match qrcode::QrCode::new(text) {
        Ok(code) => code.render::<Dense1x2>().quiet_zone(true).build(),
        Err(_) => String::new(),
    }
}
