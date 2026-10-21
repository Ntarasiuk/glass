mod agent;
mod audio;
mod capture;
mod clipboard;
mod clock;
mod control;
mod encode;
mod files;
mod input;
mod permissions;
mod server;
mod setup;
mod status;
mod tailscale;
mod video;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::Parser;
use tokio::sync::broadcast;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

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
    /// Web client files. Defaults to the installed copy next to the binary, or the source tree.
    #[arg(long)]
    web_dir: Option<PathBuf>,
    /// Access token file; created with a random token on first run.
    #[arg(long, default_value_os_t = default_token_file())]
    token_file: PathBuf,
    /// Where host.log and client.log (forwarded from browsers) are written. Defaults to
    /// ~/Library/Logs/Glass when installed, or logs/ in the source tree.
    #[arg(long)]
    log_dir: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Guided setup: Tailscale, background service, permissions, then how to connect.
    Setup,
    /// Show whether the background service is running and what it can do.
    Status,
    /// Send feedback to the Glass developers.
    Feedback { message: Vec<String> },
    /// Stop the background service and remove it from login items.
    Uninstall,
}

const FEEDBACK_URL: &str = "https://getglass.lol/api/feedback";

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match &args.command {
        None => run_host(args).await,
        Some(Command::Setup) => setup::run().await,
        Some(Command::Status) => print_status(),
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
            if agent::is_installed() { " Check ~/Library/Logs/Glass/agent.log." } else { " Run `glass setup`." }
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
        "os": std::process::Command::new("sw_vers").arg("-productVersion").output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default(),
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

    // Displays that sleep vanish from ScreenCaptureKit, so keep them (and the system)
    // awake for as long as the host runs. caffeinate exits when this process does.
    let _caffeinate = std::process::Command::new("caffeinate")
        .args(["-d", "-i", "-w", &std::process::id().to_string()])
        .spawn()
        .inspect_err(|e| tracing::warn!("caffeinate: {e}"));
    // wtransport and axum-server pull in different rustls crypto backends; pick one.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (frames, _) = broadcast::channel(8);
    // ~80 ms of 5 ms packets; a session that falls further behind just skips ahead.
    let (audio, _) = broadcast::channel(16);
    let video = video::Video::new(
        video::VideoConfig { fps: args.fps, bitrate: args.mbps * 1_000_000, scale: args.scale },
        frames.clone(),
        audio.clone(),
    );
    if input::has_accessibility() {
        info!("accessibility: granted (input injection enabled)");
    } else {
        tracing::warn!(
            "accessibility: NOT granted; remote input will be ignored. Run `glass setup`, or allow \
             Glass in System Settings → Privacy & Security → Accessibility"
        );
    }

    let token = control::load_or_create_token(&args.token_file)?;
    let tls = match args.cert.clone().zip(args.key.clone()) {
        Some((cert, key)) => Some(server::TlsFiles { cert, key }),
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
    permissions::request_missing();

    let display = loop {
        match video.set_layout(video::Layout::Single(None)) {
            Ok(streams) => break streams[0].clone(),
            Err(e) => {
                // Asleep displays aren't listed; declaring user activity wakes them.
                tracing::warn!("{e:#}; waking displays and retrying");
                let _ = std::process::Command::new("caffeinate").args(["-u", "-t", "1"]).status();
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    };

    status::set_capturing(true);

    let (events, _) = broadcast::channel(32);
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

    let shared = Arc::new(server::Shared {
        frames,
        injector: Mutex::new(input::Injector::new(display.id)),
        video,
        events,
        token: token.clone(),
        tailnet_owner,
        audio,
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
    )
    .await
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

pub fn config_dir() -> PathBuf {
    home().join(".config/glass")
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
            Some(_) => home().join("Library/Logs/Glass"),
            None => source.join("logs"),
        });
        Self { web_dir, log_dir }
    }
}
