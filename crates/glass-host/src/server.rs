//! WebTransport session handling plus the HTTP(S) server for the web client.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::Json;
use axum::routing::{get, post};
use glass_proto::{INPUT_EVENT_LEN, InputEvent, KIND_KEYFRAME_REQUEST, KIND_PING, MAX_STREAMS};
use tokio::sync::{broadcast, mpsc};
use axum::http::{HeaderValue, header};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tracing::{info, warn};
use wtransport::endpoint::IncomingSession;
use wtransport::tls::Sha256DigestFmt;
use wtransport::{Endpoint, Identity, RecvStream, SendStream, ServerConfig};

use crate::audio::AudioPackets;
use crate::clock::now_us;
use crate::control::{self, ClientMsg, STREAM_FILE, STREAM_INPUT, ServerMsg};
use crate::files;
use crate::tailscale;
use crate::media::{Clipboard, InputSink, Layout, VideoSource};
use crate::media::EncodedFrame;

pub struct Shared {
    pub frames: broadcast::Sender<Arc<EncodedFrame>>,
    pub injector: Mutex<Box<dyn InputSink>>,
    pub video: Arc<dyn VideoSource>,
    pub clipboard: Arc<dyn Clipboard>,
    /// Host-wide events fanned out to every authenticated session's control stream.
    pub events: broadcast::Sender<ServerMsg>,
    pub token: String,
    /// Tailnet login of this Mac's owner; their devices skip the token.
    pub tailnet_owner: Option<String>,
    pub audio: AudioPackets,
}

pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

pub async fn run(
    shared: Arc<Shared>,
    web_dir: PathBuf,
    http_port: u16,
    wt_port: u16,
    wt_host: Option<String>,
    tls: Option<TlsFiles>,
    client_log: PathBuf,
    extra_routes: Option<axum::Router>,
) -> Result<()> {
    let identity = match &tls {
        Some(t) => Identity::load_pemfiles(&t.cert, &t.key).await?,
        None => self_signed_identity(wt_host.as_deref()).await?,
    };
    // Only meaningful for the self-signed identity; a real cert validates normally.
    let cert_hash = tls.is_none().then(|| {
        identity.certificate_chain().as_slice()[0].hash().fmt(Sha256DigestFmt::BytesArray)
    });

    let config = ServerConfig::builder()
        .with_bind_default(wt_port)
        .with_identity(identity)
        .keep_alive_interval(Some(Duration::from_secs(2)))
        .max_idle_timeout(Some(Duration::from_secs(10)))?
        .build();
    let endpoint = Endpoint::server(config)?;
    info!("WebTransport listening on udp/{wt_port}");

    let cert_hash = cert_hash.map(|h| serde_json::from_str::<Vec<u8>>(&h).unwrap());
    let version_dir = web_dir.clone();
    let client_config = move || {
        let (wt_host, cert_hash) = (wt_host.clone(), cert_hash.clone());
        let version = web_version(&version_dir);
        async move {
            Json(serde_json::json!({
                "wtPort": wt_port,
                "wtHost": wt_host,
                "certHash": cert_hash,
                "version": version,
            }))
        }
    };
    let machines_cache = Arc::new(tokio::sync::Mutex::new(None));
    let app = axum::Router::new()
        .merge(extra_routes.unwrap_or_default())
        .route("/config.json", get(client_config))
        .route("/machines.json", get(move || machines(machines_cache.clone(), http_port, wt_port)))
        .route("/log", post(move |body: String| append_client_log(client_log.clone(), body)))
        .fallback_service(ServeDir::new(web_dir))
        // The client is tiny and changes often; never let a stale worker linger.
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));
    let addr = SocketAddr::from(([0, 0, 0, 0], http_port));
    tokio::spawn(async move {
        let result = match tls {
            Some(t) => {
                let rustls =
                    axum_server::tls_rustls::RustlsConfig::from_pem_file(&t.cert, &t.key).await?;
                info!("web client at https://<host>:{http_port}");
                axum_server::bind_rustls(addr, rustls).serve(app.into_make_service()).await
            }
            None => {
                info!("web client at http://localhost:{http_port}");
                axum_server::bind(addr).serve(app.into_make_service()).await
            }
        };
        anyhow::Ok(result?)
    });

    loop {
        let incoming = endpoint.accept().await;
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(e) = session(incoming, shared).await {
                info!("session ended: {e:#}");
            }
        });
    }
}

async fn session(incoming: IncomingSession, shared: Arc<Shared>) -> Result<()> {
    let request = incoming.await?;
    let conn = request.accept().await?;
    let peer = conn.remote_address();
    info!("client connected: {peer}");

    let authed = Arc::new(AtomicBool::new(false));
    let audio_on = Arc::new(AtomicBool::new(false));
    let video_on = Arc::new(AtomicBool::new(true));
    let mut audio = shared.audio.subscribe();
    let (ctl_tx, ctl_rx) = mpsc::unbounded_channel::<ServerMsg>();
    let mut ctl_rx = Some(ctl_rx);
    let mut frames = shared.frames.subscribe();
    // Per stream, so a client sees a gap only when that stream lost a frame.
    let mut next_frame_id = [0u32; MAX_STREAMS];

    let result: Result<()> = async {
        loop {
            tokio::select! {
                frame = frames.recv() => {
                    let frame = match frame {
                        Ok(f) => f,
                        // Fell behind: skip the ids too, so the client sees the gap and
                        // requests a keyframe instead of decoding a delta against a frame it
                        // never got.
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("{peer}: dropped {n} frames");
                            for id in &mut next_frame_id {
                                *id = id.wrapping_add(n as u32);
                            }
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    };
                    let Some(next_id) = next_frame_id.get_mut(frame.stream as usize) else { continue };
                    if authed.load(Ordering::Relaxed) && video_on.load(Ordering::Relaxed) {
                        send_frame(&conn, &frame, next_id)?;
                    } else {
                        // Keep ids moving so a resumed client sees a gap and waits for a keyframe.
                        *next_id = next_id.wrapping_add(1);
                    }
                }
                packet = audio.recv() => {
                    match packet {
                        Ok(p) if audio_on.load(Ordering::Relaxed) => {
                            conn.send_datagram(p.as_slice())?;
                        }
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                }
                dgram = conn.receive_datagram() => {
                    let dgram = dgram?;
                    match dgram.first() {
                        Some(&KIND_PING) => {
                            if let Some(pong) = glass_proto::pong(&dgram, now_us()) {
                                conn.send_datagram(pong)?;
                            }
                        }
                        Some(&KIND_KEYFRAME_REQUEST) if authed.load(Ordering::Relaxed) => {
                            shared.video.request_keyframe(dgram.get(1).copied());
                        }
                        _ => {}
                    }
                }
                stream = conn.accept_bi() => {
                    let (send, recv) = stream?;
                    // One control stream per session; extra ones are ignored.
                    let Some(rx) = ctl_rx.take() else { continue };
                    let ctx = SessionCtx {
                        conn: conn.clone(),
                        shared: shared.clone(),
                        authed: authed.clone(),
                        audio_on: audio_on.clone(),
                        video_on: video_on.clone(),
                        ctl_tx: ctl_tx.clone(),
                    };
                    tokio::spawn(async move {
                        if let Err(e) = control(ctx, send, recv, rx).await {
                            info!("{peer}: control ended: {e:#}");
                        }
                    });
                }
                stream = conn.accept_uni() => {
                    let stream = stream?;
                    let ctx = SessionCtx {
                        conn: conn.clone(),
                        shared: shared.clone(),
                        authed: authed.clone(),
                        audio_on: audio_on.clone(),
                        video_on: video_on.clone(),
                        ctl_tx: ctl_tx.clone(),
                    };
                    tokio::spawn(async move {
                        if let Err(e) = uni_stream(ctx, stream).await {
                            warn!("{peer}: stream failed: {e:#}");
                        }
                    });
                }
            }
        }
    }
    .await;

    if authed.load(Ordering::Relaxed) {
        shared.injector.lock().unwrap().release_all();
    }
    info!("client disconnected: {peer}");
    result
}

#[derive(Clone)]
struct SessionCtx {
    conn: wtransport::Connection,
    shared: Arc<Shared>,
    authed: Arc<AtomicBool>,
    audio_on: Arc<AtomicBool>,
    video_on: Arc<AtomicBool>,
    ctl_tx: mpsc::UnboundedSender<ServerMsg>,
}

async fn control(
    ctx: SessionCtx,
    mut send: SendStream,
    mut recv: RecvStream,
    mut ctl_rx: mpsc::UnboundedReceiver<ServerMsg>,
) -> Result<()> {
    let shared = &ctx.shared;
    let hello = control::read_json::<ClientMsg>(&mut recv).await;
    let token_ok = matches!(
        &hello,
        Ok(Some(ClientMsg::Hello { token: Some(t) })) if control::token_matches(t, &shared.token)
    );
    let peer = ctx.conn.remote_address().ip();
    let tailnet_ok = !token_ok
        && matches!(hello, Ok(Some(ClientMsg::Hello { .. })))
        && match &shared.tailnet_owner {
            Some(owner) => tailscale::is_trusted(peer, owner).await,
            None => false,
        };
    if !(token_ok || tailnet_ok) {
        warn!("{}: rejected (bad or missing token)", ctx.conn.remote_address());
        control::write_json(&mut send, &ServerMsg::Error { msg: "unauthorized".into() }).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        ctx.conn.close(1u32.into(), b"unauthorized");
        return Ok(());
    }
    ctx.authed.store(true, Ordering::Relaxed);
    shared.video.request_keyframe(None);
    info!(
        "{}: authenticated via {}",
        ctx.conn.remote_address(),
        if token_ok { "token" } else { "tailnet identity" }
    );

    let displays = {
        let shared = shared.clone();
        tokio::task::spawn_blocking(move || shared.video.displays()).await?.unwrap_or_default()
    };
    let welcome = ServerMsg::Welcome {
        displays,
        streams: shared.video.streams(),
        audio: crate::audio::SUPPORTED,
        host_os: if crate::testmode::enabled() { crate::testmode::host_os() } else { crate::platform::OS },
    };
    control::write_json(&mut send, &welcome).await?;

    let mut events = shared.events.subscribe();
    tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                msg = ctl_rx.recv() => match msg { Some(m) => m, None => break },
                msg = events.recv() => match msg {
                    Ok(m) => m,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            };
            if control::write_json(&mut send, &msg).await.is_err() {
                break;
            }
        }
    });

    while let Some(msg) = control::read_json::<ClientMsg>(&mut recv).await? {
        match msg {
            ClientMsg::Hello { .. } => {}
            ClientMsg::SelectDisplay { id } => set_layout(&ctx, Layout::Single(Some(id))).await?,
            ClientMsg::ShowAll => set_layout(&ctx, Layout::All).await?,
            ClientMsg::Clipboard { text, paste } => {
                shared.clipboard.set_text(&text);
                if paste {
                    shared.injector.lock().unwrap().paste();
                }
            }
            ClientMsg::Video { on } => {
                ctx.video_on.store(on, Ordering::Relaxed);
                if on {
                    shared.video.request_keyframe(None);
                }
                info!("{}: video {}", ctx.conn.remote_address(), if on { "resumed" } else { "paused" });
            }
            ClientMsg::Audio { on } => {
                ctx.audio_on.store(on, Ordering::Relaxed);
                info!("{}: audio {}", ctx.conn.remote_address(), if on { "on" } else { "off" });
            }
            ClientMsg::PickFiles => {
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    match files::pick().await {
                        Ok(paths) => {
                            for path in paths {
                                if let Err(e) = files::send(&ctx.conn, &path).await {
                                    let _ = ctx.ctl_tx.send(ServerMsg::Error {
                                        msg: format!("send {}: {e:#}", path.display()),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            let _ = ctx.ctl_tx.send(ServerMsg::Error { msg: format!("pick files: {e:#}") });
                        }
                    }
                });
            }
        }
    }
    Ok(())
}

async fn set_layout(ctx: &SessionCtx, layout: Layout) -> Result<()> {
    let worker = ctx.shared.clone();
    match tokio::task::spawn_blocking(move || worker.video.set_layout(layout)).await? {
        Ok(streams) => {
            let ids: Vec<u32> = streams.iter().map(|d| d.id).collect();
            ctx.shared.injector.lock().unwrap().set_displays(&ids);
            let _ = ctx.shared.events.send(ServerMsg::Layout { streams });
        }
        Err(e) => {
            let _ = ctx.ctl_tx.send(ServerMsg::Error { msg: format!("switch display: {e:#}") });
        }
    }
    Ok(())
}

const AUTH_WAIT: Duration = Duration::from_secs(10);


async fn uni_stream(ctx: SessionCtx, mut stream: RecvStream) -> Result<()> {
    let mut tag = [0u8; 1];
    stream.read_exact(&mut tag).await?;
    // Streams opened right after hello can arrive while auth is still in progress
    // (a tailnet whois takes tens of ms); wait for it rather than dropping them.
    let deadline = tokio::time::Instant::now() + AUTH_WAIT;
    while !ctx.authed.load(Ordering::Relaxed) {
        if tokio::time::Instant::now() > deadline {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    match tag[0] {
        STREAM_INPUT => {
            info!("{}: input stream open", ctx.conn.remote_address());
            let mut buf = [0u8; INPUT_EVENT_LEN];
            let mut events = 0u64;
            while stream.read_exact(&mut buf).await.is_ok() {
                if let Some(ev) = InputEvent::read(&buf) {
                    ctx.shared.injector.lock().unwrap().handle(ev);
                    events += 1;
                }
            }
            info!("{}: input stream closed after {events} events", ctx.conn.remote_address());
        }
        STREAM_FILE => {
            let (name, path) = files::receive(&mut stream).await?;
            let _ = ctx.ctl_tx.send(ServerMsg::FileSaved { name, path: path.display().to_string() });
        }
        other => warn!("unknown stream type {other:#x}"),
    }
    Ok(())
}

/// Frame ids are per session so the client sees a gap only when this session lost a frame.
fn send_frame(conn: &wtransport::Connection, frame: &EncodedFrame, next_id: &mut u32) -> Result<()> {
    let max = conn.max_datagram_size().unwrap_or(1200);
    let id = *next_id;
    *next_id = next_id.wrapping_add(1);
    // Test-mode fault injection: a dropped frame leaves a gap in the ids.
    if crate::testmode::take_drop() {
        return Ok(());
    }
    let mut dgrams = glass_proto::fragment(&frame.data, frame.stream, id, frame.keyframe, frame.capture_us, max);
    if crate::testmode::take_corrupt()
        && let Some(last) = dgrams.last_mut()
        && let Some(byte) = last.last_mut()
    {
        *byte ^= 0xff;
    }
    for dgram in dgrams {
        conn.send_datagram(dgram)?;
    }
    Ok(())
}

#[derive(serde::Serialize, Clone)]
struct MachineEntry {
    #[serde(flatten)]
    machine: tailscale::Machine,
    /// Glass answered on this machine (so the client can connect to it).
    glass: bool,
    /// Its WebTransport port, from its /config.json.
    wt_port: Option<u16>,
}

type MachinesCache = Arc<tokio::sync::Mutex<Option<(std::time::Instant, Vec<MachineEntry>)>>>;
const MACHINES_TTL: Duration = Duration::from_secs(15);

/// Your tailnet devices and which of them run Glass, for the client's machine switcher.
async fn machines(cache: MachinesCache, port: u16, wt_port: u16) -> Json<serde_json::Value> {
    let mut cache = cache.lock().await;
    if let Some((at, list)) = cache.as_ref()
        && at.elapsed() < MACHINES_TTL
    {
        return Json(serde_json::json!({ "machines": list }));
    }
    let list = match tailscale::machines().await {
        Ok(found) => {
            // Probe every machine at once; offline ones are skipped.
            let probes: Vec<_> = found
                .into_iter()
                .map(|m| {
                    tokio::spawn(async move {
                        let wt_port = if m.is_self {
                            Some(wt_port)
                        } else if m.online {
                            probe_glass(&m.dns, port).await
                        } else {
                            None
                        };
                        MachineEntry { glass: wt_port.is_some(), machine: m, wt_port }
                    })
                })
                .collect();
            let mut list = Vec::with_capacity(probes.len());
            for p in probes {
                if let Ok(entry) = p.await {
                    list.push(entry);
                }
            }
            list
        }
        Err(e) => {
            warn!("machines: {e:#}");
            Vec::new()
        }
    };
    *cache = Some((std::time::Instant::now(), list.clone()));
    Json(serde_json::json!({ "machines": list }))
}

/// If a Glass host answers at `https://<dns>:<port>/config.json`, its WebTransport port.
async fn probe_glass(dns: &str, port: u16) -> Option<u16> {
    let url = format!("https://{dns}:{port}/config.json");
    let out = tokio::process::Command::new("curl").args(["-fsS", "--max-time", "1.5", &url]).output().await.ok()?;
    let config: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    config["wtPort"].as_u64().map(|p| p as u16)
}

/// Browsers accept a self-signed cert via `serverCertificateHashes` only if it is ECDSA
/// and valid for at most 14 days. It's saved and reused (and replaced before it expires)
/// so its hash stays stable across restarts, and hosts sharing a config dir share it.
async fn self_signed_identity(wt_host: Option<&str>) -> Result<Identity> {
    const MAX_AGE: Duration = Duration::from_secs(10 * 86400);
    let dir = crate::config_dir().join("certs");
    let (cert, key) = (dir.join("self-signed.crt"), dir.join("self-signed.key"));
    let fresh = std::fs::metadata(&cert)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or(MAX_AGE) < MAX_AGE);
    if fresh && let Ok(identity) = Identity::load_pemfiles(&cert, &key).await {
        return Ok(identity);
    }
    let names = ["localhost", "127.0.0.1", "::1"].map(String::from).into_iter().chain(wt_host.map(String::from));
    let identity = Identity::self_signed(names)?;
    tokio::fs::create_dir_all(&dir).await?;
    identity.certificate_chain().store_pemfile(&cert).await?;
    identity.private_key().store_secret_pemfile(&key).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600));
    }
    Ok(identity)
}

/// Changes whenever a web client file changes, so open tabs know to reload.
fn web_version(dir: &std::path::Path) -> String {
    let newest = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .unwrap_or_default();
    format!("{}", newest.as_millis())
}

/// Client log batches arrive as JSON lines; appended verbatim with a receive timestamp.
async fn append_client_log(path: PathBuf, body: String) {
    use std::io::Write;
    let now = unix_now();
    let lines: String = body.lines().map(|l| format!("{now} {l}\n")).collect();
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(lines.as_bytes()));
    if let Err(e) = result {
        warn!("writing client log: {e}");
    }
    for line in body.lines().filter(|l| l.contains("\"level\":\"error\"")) {
        warn!(target: "client", "{line}");
    }
}

fn unix_now() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:03}", d.as_secs(), d.subsec_millis())
}
