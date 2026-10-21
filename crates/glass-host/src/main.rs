mod control;
mod files;
mod media;
mod rate;
mod server;
mod setup;
mod soft_encode;
mod status;
mod testmode;
mod tailscale;

#[cfg(target_os = "macos")]
#[path = "macos/mod.rs"]
mod platform;
#[cfg(target_os = "linux")]
#[path = "linux/mod.rs"]
mod platform;

// Platform modules keep the same names everywhere, so shared code says `crate::input` etc.
use platform::{agent, audio, clipboard, clock, input, permissions, video};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::Parser;
use tokio::sync::broadcast;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::media::VideoSource;

#[derive(Parser)]
#[command(name = "glass", about = "Low-latency remote desktop for your Mac", version)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Port for the web client (TCP).
    #[arg(long, default_value_t = 8080)]
    http_port: u16,
    /// Port for WebTransport (UDP).
    #[arg(long, default_value_t = 4433)]
    wt_port: u16,
    /// Capture frame rate cap.
    #[arg(long, default_value_t = 60)]
    fps: i32,
    /// Target bitrate in Mbps.
    #[arg(long, default_value_t = 30)]
    mbps: i32,
    /// Output scale relative to the display's native pixel size.
    #[arg(long, default_value_t = 1.0)]
    scale: f64,
    /// TLS certificate (PEM). Defaults to an automatic `tailscale cert` for this Mac when
    /// Tailscale HTTPS is enabled; without any, the web client is served over plain HTTP,
    /// which browsers only treat as secure on localhost.
    #[arg(long, requires = "key")]
    cert: Option<PathBuf>,
    #[arg(long, requires = "cert")]
    key: Option<PathBuf>,
    /// Host the browser should connect WebTransport to, when it differs from the page's
    /// host (e.g. page served through a tunnel, video direct over the LAN).
    #[arg(long)]
    wt_host: Option<String>,
    /// Test mode: a generated test pattern instead of screen capture, recorded input, an
    /// in-memory clipboard, no Tailscale, and /test/* hooks. Never touches the real desktop.
    #[arg(long)]
    test: bool,
    /// Where received files are saved (default ~/Downloads).
    #[arg(long)]
    downloads_dir: Option<PathBuf>,
    /// Web client files. Defaults to the installed copy next to the binary, or the source tree.
    #[arg(long)]
    web_dir: Option<PathBuf>,
    /// Access token file; created with a random token on first run.
    #[arg(long, default_value_os_t = default_token_file())]
    token_file: PathBuf,
    /// Where host.log and client.log (forwarded from browsers) are written. Defaults to
    /// ~/Library/Logs/Glass (macOS) or ~/.local/state/glass (Linux) when installed, or logs/
    /// in the source tree.
    #[arg(long)]
    log_dir: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Guided setup: Tailscale, background service, permissions, then how to connect.
    Setup {
        /// Non-interactive (for scripts and AI agents): do every step that needs no
        /// human, then report what's left. Uses sudo only if it needs no password.
        #[arg(long)]
        yes: bool,
        /// With --yes: print the report as JSON.
        #[arg(long, requires = "yes")]
        json: bool,
    },
    /// Show whether the background service is running and what it can do.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Send feedback to the Glass developers.
    Feedback { message: Vec<String> },
    /// Stop the background service and remove it from login items.
    Uninstall,
}

const FEEDBACK_URL: &str = "https://glassdesk.dev/api/feedback";

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match &args.command {
        None => run_host(args).await,
        Some(Command::Setup { yes: false, .. }) => setup::run().await,
        Some(Command::Setup { yes: true, json }) => setup::run_unattended(*json).await,
        Some(Command::Status { json: false }) => print_status(),
        Some(Command::Status { json: true }) => {
            let status = status::read().filter(|s| s.is_fresh());
            println!("{}", serde_json::json!({ "running": status.is_some(), "status": status }));
            Ok(())
        }
        Some(Command::Feedback { message }) => send_feedback(&message.join(" ")),
        Some(Command::Uninstall) => {
            agent::uninstall()?;
            println!("Glass service removed. Delete ~/.glass and ~/.config/glass to remove everything.");
            Ok(())
        }
    }
}

fn print_status() -> Result<()> {
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    match status::read().filter(|s| s.is_fresh()) {
        Some(s) => {
            println!("Glass {} running (pid {})", s.version, s.pid);
            println!("  capturing:        {}", yes_no(s.capturing));
            println!("  screen recording: {}", yes_no(s.screen_recording));
            println!("  accessibility:    {}", yes_no(s.accessibility));
            if let Some(url) = s.url {
                println!("  open:             {url}");
            }
        }
        None => println!(
            "Glass isn't running.{}",
            if agent::is_installed() { agent::LOG_HINT } else { " Run `glass setup`." }
        ),
    }
    Ok(())
}

fn send_feedback(message: &str) -> Result<()> {
    anyhow::ensure!(!message.trim().is_empty(), "usage: glass feedback \"what happened or what you'd like\"");
    let body = serde_json::json!({
        "message": message,
        "source": "cli",
        "version": env!("CARGO_PKG_VERSION"),
        "os": platform::os_version(),
    });
    let status = std::process::Command::new("curl")
        .args(["-fsS", "-X", "POST", "-H", "content-type: application/json", "--data-binary"])
        .arg(body.to_string())
        .arg(FEEDBACK_URL)
        .stdout(std::process::Stdio::null())
        .status()?;
    anyhow::ensure!(status.success(), "couldn't reach {FEEDBACK_URL}");
    println!("Thanks! Feedback sent.");
    Ok(())
}

async fn run_host(args: Args) -> Result<()> {
    let paths = Paths::resolve(args.web_dir.clone(), args.log_dir.clone());
    std::fs::create_dir_all(&paths.log_dir)?;
    let host_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_dir.join("host.log"))?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_ansi(false)
        .with_writer(std::io::stdout.and(Mutex::new(host_log)))
        .init();
    // wtransport and axum-server pull in different rustls crypto backends; pick one.
    let _ = rustls::crypto::ring::default_provider().install_default();

    if args.test {
        testmode::enable();
        info!("TEST MODE: test pattern, recorded input, in-memory clipboard");
    }
    if let Some(dir) = &args.downloads_dir {
        files::set_downloads_dir(dir.clone());
    }

    let (frames, _) = broadcast::channel(8);
    // ~80 ms of 5 ms packets; a session that falls further behind just skips ahead.
    let (audio, _) = broadcast::channel(16);
    let (events, _) = broadcast::channel(32);

    let token = control::load_or_create_token(&args.token_file)?;
    let tls = match args.cert.clone().zip(args.key.clone()) {
        Some((cert, key)) => Some(server::TlsFiles { cert, key }),
        None if args.test => None,
        None => tailscale::ensure_cert(&config_dir().join("certs"))
            .await
            .map(|(cert, key)| server::TlsFiles { cert, key }),
    };
    let host = tls
        .as_ref()
        .and_then(|t| t.cert.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "localhost".into());
    let scheme = if tls.is_some() { "https" } else { "http" };
    status::start(Some(format!("{scheme}://{host}:{}/", args.http_port)));

    let (video, injector, clipboard, tailnet_owner, _awake): (
        Arc<dyn media::VideoSource>,
        Box<dyn media::InputSink>,
        Arc<dyn media::Clipboard>,
        _,
        _,
    ) = if args.test {
        let video = Arc::new(testmode::TestVideo::new(frames.clone(), args.fps as u32, (args.mbps * 1_000_000) as u32));
        video.set_layout(media::Layout::Single(None))?;
        testmode::start_tone(audio.clone());
        (video, Box::new(testmode::FakeInput), Arc::new(testmode::FakeClipboard), None, None)
    } else {
        // Keep displays (and the system) awake for as long as the host runs.
        let awake = platform::keep_awake();
        let video = Arc::new(video::Video::new(
            video::VideoConfig { fps: args.fps, bitrate: args.mbps * 1_000_000, scale: args.scale },
            frames.clone(),
            audio.clone(),
        ));
        if input::has_accessibility() {
            info!("accessibility: granted (input injection enabled)");
        } else {
            tracing::warn!(
                "accessibility: NOT granted; remote input will be ignored. Run `glass setup`, or allow \
                 Glass in System Settings → Privacy & Security → Accessibility"
            );
        }
        permissions::request_missing();
        let display = loop {
            match video.set_layout(media::Layout::Single(None)) {
                Ok(streams) => break streams[0].clone(),
                Err(e) => {
                    tracing::warn!("{e:#}; waking displays and retrying");
                    platform::wake_displays();
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        };
        clipboard::watch(events.clone());
        let tailnet_owner = match tailscale::self_login().await {
            Ok(login) => {
                info!("tailnet devices owned by {login} connect without a token");
                Some(login)
            }
            Err(e) => {
                info!("tailscale unavailable ({e:#}); token required for all clients");
                None
            }
        };
        (video, Box::new(input::Injector::new(display.id)), Arc::new(clipboard::SystemClipboard), tailnet_owner, awake)
    };
    status::set_capturing(true);

    let test_routes = args.test.then(|| {
        testmode::routes(events.clone(), files::downloads_dir())
    });
    let shared = Arc::new(server::Shared {
        frames,
        injector: Mutex::new(injector),
        video,
        clipboard,
        events,
        token: token.clone(),
        tailnet_owner,
        audio,
        rate: Mutex::new(rate::RateControl::new((args.mbps * 1_000_000) as u32)),
    });
    info!("open {scheme}://{host}:{}/#t={token}", args.http_port);
    server::run(
        shared,
        paths.web_dir,
        args.http_port,
        args.wt_port,
        args.wt_host,
        tls,
        paths.log_dir.join("client.log"),
        test_routes,
    )
    .await
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// ~/.config/glass, or $GLASS_CONFIG_DIR (tests use it to stay out of the real config).
pub fn config_dir() -> PathBuf {
    std::env::var_os("GLASS_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home().join(".config/glass"))
}

pub fn default_token_file() -> PathBuf {
    config_dir().join("token")
}

struct Paths {
    web_dir: PathBuf,
    log_dir: PathBuf,
}

impl Paths {
    /// Installed layout is `<prefix>/bin/glass` + `<prefix>/share/glass/web`; otherwise
    /// fall back to the source tree this binary was built from.
    fn resolve(web_dir: Option<PathBuf>, log_dir: Option<PathBuf>) -> Self {
        let installed = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.canonicalize().ok()?.parent()?.parent()?.join("share/glass/web")))
            .filter(|dir| dir.is_dir());
        let source = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let web_dir = web_dir.or(installed.clone()).unwrap_or_else(|| source.join("web"));
        let log_dir = log_dir.unwrap_or_else(|| match installed {
            Some(_) => platform::log_dir(),
            None => source.join("logs"),
        });
        Self { web_dir, log_dir }
    }
}
