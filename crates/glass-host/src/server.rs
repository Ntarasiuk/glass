//! WebTransport session handling plus the HTTP(S) server for the web client.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::Json;
use axum::http::{HeaderValue, header};
use axum::routing::{get, post};
use glass_proto::{INPUT_EVENT_LEN, InputEvent, KIND_KEYFRAME_REQUEST, KIND_PING, MAX_STREAMS};
use tokio::sync::{broadcast, mpsc};
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
use crate::media::EncodedFrame;
use crate::media::{Clipboard, InputSink, Layout, VideoSource};
use crate::tailscale;

pub struct Shared {
    pub frames: broadcast::Sender<Arc<EncodedFrame>>,
    pub injector: Mutex<Box<dyn InputSink>>,
    pub video: Arc<dyn VideoSource>,
    pub clipboard: Arc<dyn Clipboard>,
    /// Host-wide events fanned out to every authenticated session's control stream.
    pub events: broadcast::Sender<ServerMsg>,
    /// Read on every sign-in, so `glass token --reset` takes effect without a restart.
    pub token_file: PathBuf,
    /// Failed sign-ins per address, for brute-force protection.
    pub auth_failures:
        Mutex<std::collections::HashMap<std::net::IpAddr, (u32, std::time::Instant)>>,
    /// Tailnet login of this Mac's owner; their devices skip the token.
    pub tailnet_owner: Option<String>,
    pub audio: AudioPackets,
    pub rate: Mutex<crate::rate::RateControl>,
}

pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

pub async fn run(
    shared: Arc<Shared>,
    web_dir: PathBuf,
    // The usual web port, where other machines' hosts are probed.
    http_port: u16,
    http_listener: std::net::TcpListener,
    https_listener: Option<std::net::TcpListener>,
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
        identity.certificate_chain().as_slice()[0]
            .hash()
            .fmt(Sha256DigestFmt::BytesArray)
    });

    // The usual port, or the next free one: clients learn it from /config.json.
    let mut bound = None;
    for port in wt_port..wt_port.saturating_add(crate::PORT_TRIES) {
        let config = ServerConfig::builder()
            .with_bind_default(port)
            .with_identity(identity.clone_identity())
            .keep_alive_interval(Some(Duration::from_secs(2)))
            .max_idle_timeout(Some(Duration::from_secs(10)))?
            .build();
        match Endpoint::server(config) {
            Ok(endpoint) => {
                bound = Some((endpoint, port));
                break;
            }
            Err(e) => warn!("udp/{port} unavailable ({e}); trying the next port"),
        }
    }
    let Some((endpoint, wt_port)) = bound else {
        anyhow::bail!(
            "UDP ports {wt_port}–{} are all in use; free one or pass --wt-port",
            wt_port.saturating_add(crate::PORT_TRIES - 1)
        );
    };
    info!("WebTransport listening on udp/{wt_port}");
    crate::remote::watch(wt_port);

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
    let _ = SHARED.set(shared.clone());
    let _ = CLIENT_LOG.set(client_log.clone());
    let machines_cache = Arc::new(tokio::sync::Mutex::new(None));
    let app = axum::Router::new()
        .merge(extra_routes.unwrap_or_default())
        .route("/config.json", get(client_config))
        .route(
            "/machines.json",
            get(move || machines(machines_cache.clone(), http_port, wt_port)),
        )
        .route(
            "/log",
            post(move |body: String| append_client_log(client_log.clone(), body)),
        )
        .fallback_service(ServeDir::new(web_dir))
        // The client is tiny and changes often; never let a stale worker linger.
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));
    let web_port = http_listener.local_addr()?.port();
    http_listener.set_nonblocking(true)?;
    tokio::spawn(async move {
        let result = match tls {
            Some(t) => {
                let rustls =
                    axum_server::tls_rustls::RustlsConfig::from_pem_file(&t.cert, &t.key).await?;
                if let Some(listener) = https_listener {
                    let port = listener.local_addr()?.port();
                    listener.set_nonblocking(true)?;
                    let server = axum_server::from_tcp_rustls(listener, rustls.clone())?;
                    let app = app.clone();
                    info!("web client at https://<host>/ (port {port})");
                    tokio::spawn(async move {
                        if let Err(e) = server.serve(app.into_make_service()).await {
                            warn!("port {port}: {e}");
                        }
                    });
                }
                info!("web client at https://<host>:{web_port}");
                axum_server::from_tcp_rustls(http_listener, rustls)?
                    .serve(app.into_make_service())
                    .await
            }
            None => {
                info!("web client at http://localhost:{web_port}");
                axum_server::from_tcp(http_listener)?
                    .serve(app.into_make_service())
                    .await
            }
        };
        if let Err(e) = &result {
            tracing::error!("web server stopped: {e}");
        }
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
    // A WebRTC viewer arrives through the local bridge, which vouches for its real address.
    let relayed = crate::remote::relayed_peer(request.path());
    let conn = request.accept().await?;
    let peer = conn.remote_address();
    match &relayed {
        Some(r) => info!(
            "client connected: {} (remote, over WebRTC{})",
            r.peer,
            if r.rtp_video { ", video as RTP" } else { "" }
        ),
        None => info!("client connected: {peer}"),
    }
    // Its video goes out as RTP tracks (rtp.rs), not as datagrams on this session.
    let rtp_video = relayed.as_ref().is_some_and(|r| r.rtp_video);

    let authed = Arc::new(AtomicBool::new(false));
    let audio_on = Arc::new(AtomicBool::new(false));
    let video_on = Arc::new(AtomicBool::new(true));
    let mut audio = shared.audio.subscribe();
    let (ctl_tx, ctl_rx) = mpsc::unbounded_channel::<ServerMsg>();
    let mut ctl_rx = Some(ctl_rx);
    let mut frames = shared.frames.subscribe();
    // Per stream, so a client sees a gap only when that stream lost a frame.
    let mut next_frame_id = [0u32; MAX_STREAMS];
    // When this client last had a keyframe request honoured: [all streams, stream 0, …].
    let mut last_keyframe_request = [None::<std::time::Instant>; MAX_STREAMS + 1];

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
                    if rtp_video {
                        continue;
                    }
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
                            // Keyframes are big: on a lossy link, honouring every request
                            // makes the loss worse. One per stream per interval is enough.
                            let stream = dgram.get(1).copied();
                            let slot = stream.map_or(0, |s| (s as usize + 1).min(MAX_STREAMS));
                            let now = std::time::Instant::now();
                            if last_keyframe_request[slot].is_none_or(|t| now.duration_since(t) >= KEYFRAME_REQUEST_SPACING) {
                                last_keyframe_request[slot] = Some(now);
                                shared.video.request_keyframe(stream);
                            }
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
                        relayed: relayed.clone(),
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
                        relayed: relayed.clone(),
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
    /// The real address of a WebRTC viewer (the connection itself is loopback).
    relayed: Option<crate::remote::Relay>,
}

async fn control(
    ctx: SessionCtx,
    mut send: SendStream,
    mut recv: RecvStream,
    mut ctl_rx: mpsc::UnboundedReceiver<ServerMsg>,
) -> Result<()> {
    let shared = &ctx.shared;
    let hello = control::read_json::<ClientMsg>(&mut recv).await;
    let peer = ctx
        .relayed
        .as_ref()
        .map(|r| r.peer)
        .unwrap_or_else(|| ctx.conn.remote_address().ip().to_canonical());
    if auth_blocked(shared, peer) {
        warn!("{peer}: rejected (too many failed sign-ins)");
        control::write_json(
            &mut send,
            &ServerMsg::Error {
                msg: "unauthorized".into(),
            },
        )
        .await?;
        // Let the message arrive before closing, so the client shows the token prompt.
        tokio::time::sleep(Duration::from_millis(200)).await;
        ctx.conn.close(1u32.into(), b"too many attempts");
        return Ok(());
    }
    let hello_ok = matches!(hello, Ok(Some(ClientMsg::Hello { .. })));
    let (token, link, pass) = match hello {
        Ok(Some(ClientMsg::Hello { token, link, pass })) => (token, link, pass),
        _ => (None, None, None),
    };
    let expected = control::load_or_create_token(&shared.token_file).unwrap_or_default();
    let access_file = crate::access::file_for(&shared.token_file);
    // Owner credentials first, so a tailnet device opening a link doesn't use it up.
    let mut auth = None;
    if !expected.is_empty()
        && token
            .as_deref()
            .is_some_and(|t| control::token_matches(t, &expected))
    {
        auth = Some(Auth::Token);
    }
    if auth.is_none()
        && hello_ok
        // Over WebRTC the address is public, never a tailnet device's.
        && ctx.relayed.is_none()
        && let Some(owner) = &shared.tailnet_owner
        && tailscale::is_trusted(peer, owner).await
    {
        auth = Some(Auth::Tailnet);
    }
    if auth.is_none()
        && let Some(p) = &pass
        && let Some(expires) = crate::access::pass_expiry(&access_file, p)
    {
        auth = Some(Auth::Guest {
            pass: p.clone(),
            expires,
            new: false,
        });
    }
    let mut link_failed = false;
    if auth.is_none()
        && let Some(code) = &link
    {
        match crate::access::redeem_link(&access_file, code) {
            Ok(Some(g)) => {
                auth = Some(Auth::Guest {
                    pass: g.pass,
                    expires: g.expires,
                    new: true,
                })
            }
            Ok(None) => link_failed = true,
            Err(e) => warn!("{peer}: magic link check failed: {e:#}"),
        }
    }
    let Some(auth) = auth else {
        record_auth_failure(shared, peer);
        warn!("{peer}: rejected (bad or missing token)");
        if link_failed {
            let msg = "This link has expired or was already used. Ask for a new one.".into();
            control::write_json(&mut send, &ServerMsg::Error { msg }).await?;
        }
        control::write_json(
            &mut send,
            &ServerMsg::Error {
                msg: "unauthorized".into(),
            },
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        ctx.conn.close(1u32.into(), b"unauthorized");
        return Ok(());
    };
    ctx.authed.store(true, Ordering::Relaxed);
    if let Some(r) = &ctx.relayed {
        crate::remote::bridge_authed(r.bridge);
    }
    shared.video.request_keyframe(None);
    let guest = matches!(auth, Auth::Guest { .. });
    match &auth {
        Auth::Token => info!("{peer}: authenticated via token"),
        Auth::Tailnet => info!("{peer}: authenticated via tailnet identity"),
        Auth::Guest { expires, new, .. } => {
            let left = crate::access::describe(Duration::from_secs(
                expires.saturating_sub(crate::access::now()),
            ));
            let how = if *new { "magic link" } else { "guest pass" };
            info!("{peer}: authenticated via {how} ({left} left)");
        }
    }
    if let Auth::Guest {
        pass,
        expires,
        new: true,
    } = &auth
    {
        control::write_json(
            &mut send,
            &ServerMsg::Pass {
                pass: pass.clone(),
                expires: *expires,
            },
        )
        .await?;
    }
    if let Auth::Guest { pass, .. } = &auth {
        // A guest is cut off when its pass expires or is revoked (`glass link --revoke`).
        let (conn, pass, file) = (ctx.conn.clone(), pass.clone(), access_file.clone());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = conn.closed() => return,
                    _ = tokio::time::sleep(GUEST_RECHECK) => {}
                }
                if crate::access::pass_expiry(&file, &pass).is_none() {
                    info!("{}: guest access ended", conn.remote_address());
                    conn.close(1u32.into(), b"access ended");
                    return;
                }
            }
        });
    }

    let displays = {
        let shared = shared.clone();
        tokio::task::spawn_blocking(move || shared.video.displays())
            .await?
            .unwrap_or_default()
    };
    let welcome = ServerMsg::Welcome {
        displays,
        streams: shared.video.streams(),
        audio: crate::audio::SUPPORTED,
        host_os: if crate::testmode::enabled() {
            crate::testmode::host_os()
        } else {
            crate::platform::OS
        },
        guest,
    };
    control::write_json(&mut send, &welcome).await?;
    let bps = shared.rate.lock().unwrap().target();
    control::write_json(&mut send, &ServerMsg::Bitrate { bps }).await?;
    if let Some(version) = crate::update::available() {
        control::write_json(
            &mut send,
            &ServerMsg::UpdateAvailable {
                current: crate::update::CURRENT,
                version,
            },
        )
        .await?;
    }

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

    while let Some(buf) = control::read_frame(&mut recv).await? {
        // Skip what we don't understand (say, a newer client) rather than dropping the
        // whole control stream, which would silently break display switching and more.
        let msg = match serde_json::from_slice::<ClientMsg>(&buf) {
            Ok(msg) => msg,
            Err(e) => {
                warn!("{peer}: ignoring control message: {e}");
                continue;
            }
        };
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
                if let Some(r) = &ctx.relayed {
                    crate::remote::bridge_watching(r.bridge, on);
                }
                if on {
                    shared.video.request_keyframe(None);
                }
                info!("{peer}: video {}", if on { "resumed" } else { "paused" });
            }
            ClientMsg::Update if guest => warn!("{peer}: guest asked to update the host; ignored"),
            ClientMsg::Update => {
                if crate::testmode::enabled() {
                    crate::testmode::record_update();
                } else if let Err(e) = crate::update::start_self_update() {
                    let _ = ctx.ctl_tx.send(ServerMsg::Error {
                        msg: format!("update: {e:#}"),
                    });
                }
            }
            ClientMsg::Log { lines } => {
                if let Some(path) = CLIENT_LOG.get() {
                    let body = lines
                        .iter()
                        .take(LOG_LINES_MAX)
                        .map(|l| {
                            l.chars()
                                .take(LOG_LINE_MAX)
                                .collect::<String>()
                                .replace('\n', " ")
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    append_client_log(path.clone(), body).await;
                }
            }
            ClientMsg::NetStats(report) => {
                apply_rate_report(shared, report);
            }
            ClientMsg::Audio { on } => {
                ctx.audio_on.store(on, Ordering::Relaxed);
                info!("{peer}: audio {}", if on { "on" } else { "off" });
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
                            let _ = ctx.ctl_tx.send(ServerMsg::Error {
                                msg: format!("pick files: {e:#}"),
                            });
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
    let peer = ctx.conn.remote_address();
    info!("{peer}: layout {layout:?}");
    match tokio::task::spawn_blocking(move || worker.video.set_layout(layout)).await? {
        Ok(streams) => {
            let ids: Vec<u32> = streams.iter().map(|d| d.id).collect();
            info!("{peer}: now streaming displays {ids:?}");
            ctx.shared.injector.lock().unwrap().set_displays(&ids);
            let _ = ctx.shared.events.send(ServerMsg::Layout { streams });
        }
        Err(e) => {
            warn!("{peer}: switch display failed: {e:#}");
            let _ = ctx.ctl_tx.send(ServerMsg::Error {
                msg: format!("switch display: {e:#}"),
            });
        }
    }
    Ok(())
}

/// Failed sign-ins allowed per address within `AUTH_WINDOW` before it's locked out.
/// How a session signed in.
enum Auth {
    Token,
    Tailnet,
    /// Via a magic link: `new` when the link was opened just now (the pass is sent back).
    Guest {
        pass: String,
        expires: u64,
        new: bool,
    },
}

/// How often a guest session re-checks that its pass is still valid.
const GUEST_RECHECK: Duration = Duration::from_secs(10);

/// A client's keyframe requests are honoured at most this often (per stream).
const KEYFRAME_REQUEST_SPACING: Duration = Duration::from_millis(500);

const AUTH_MAX_FAILURES: u32 = 10;
const AUTH_WINDOW: Duration = Duration::from_secs(600);

fn auth_blocked(shared: &Shared, peer: std::net::IpAddr) -> bool {
    let mut failures = shared.auth_failures.lock().unwrap();
    failures.retain(|_, (_, since)| since.elapsed() < AUTH_WINDOW);
    failures
        .get(&peer)
        .is_some_and(|(n, _)| *n >= AUTH_MAX_FAILURES)
}

fn record_auth_failure(shared: &Shared, peer: std::net::IpAddr) {
    let mut failures = shared.auth_failures.lock().unwrap();
    let entry = failures
        .entry(peer)
        .or_insert((0, std::time::Instant::now()));
    entry.0 += 1;
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
            info!(
                "{}: input stream closed after {events} events",
                ctx.conn.remote_address()
            );
        }
        STREAM_FILE => {
            let (name, path) = files::receive(&mut stream).await?;
            let _ = ctx.ctl_tx.send(ServerMsg::FileSaved {
                name,
                path: path.display().to_string(),
            });
        }
        other => warn!("unknown stream type {other:#x}"),
    }
    Ok(())
}

/// Frame ids are per session so the client sees a gap only when this session lost a frame.
fn send_frame(
    conn: &wtransport::Connection,
    frame: &EncodedFrame,
    next_id: &mut u32,
) -> Result<()> {
    let max = conn.max_datagram_size().unwrap_or(1200);
    let id = *next_id;
    *next_id = next_id.wrapping_add(1);
    // Test-mode fault injection: a dropped frame leaves a gap in the ids.
    if crate::testmode::take_drop() {
        return Ok(());
    }
    let mut dgrams = glass_proto::fragment(
        &frame.data,
        frame.stream,
        id,
        frame.keyframe,
        frame.capture_us,
        max,
    );
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
                        MachineEntry {
                            glass: wt_port.is_some(),
                            machine: m,
                            wt_port,
                        }
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
/// Tries the port-less address first (it's there even when a host's web port had to move),
/// then the usual port.
async fn probe_glass(dns: &str, port: u16) -> Option<u16> {
    for url in [
        format!("https://{dns}/config.json"),
        format!("https://{dns}:{port}/config.json"),
    ] {
        let Ok(out) = tokio::process::Command::new("curl")
            .args(["-fsS", "--max-time", "1.5", &url])
            .output()
            .await
        else {
            continue;
        };
        let config: Option<serde_json::Value> = serde_json::from_slice(&out.stdout).ok();
        if let Some(port) = config.and_then(|c| c["wtPort"].as_u64()) {
            return Some(port as u16);
        }
    }
    None
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
    let names = ["localhost", "127.0.0.1", "::1"]
        .map(String::from)
        .into_iter()
        .chain(wt_host.map(String::from));
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
/// Feeds one congestion report (from a client, or the WebRTC bridge) to the adaptive bitrate.
fn apply_rate_report(shared: &Shared, report: crate::rate::Report) {
    let changed = shared
        .rate
        .lock()
        .unwrap()
        .report(report, std::time::Instant::now());
    if changed.is_some() {
        apply_bitrate(shared);
    }
}

/// Each remote (RTP) viewer's congestion-control estimate, as a cap on the shared encoders.
static REMOTE_CAPS: Mutex<Option<std::collections::HashMap<u64, u32>>> = Mutex::new(None);
static APPLIED_BPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Sets (or with `None` removes) a remote viewer's bitrate cap.
pub fn set_remote_cap(viewer: u64, bps: Option<u32>) {
    {
        let mut caps = REMOTE_CAPS.lock().unwrap();
        let caps = caps.get_or_insert_with(Default::default);
        match bps {
            Some(b) => caps.insert(viewer, b),
            None => caps.remove(&viewer),
        };
    }
    if let Some(shared) = SHARED.get() {
        apply_bitrate(shared);
    }
}

/// The encoders' bitrate: the tailnet clients' adaptive target, capped by every remote
/// viewer's estimate (they share the encoders, so the slowest link sets the rate).
fn apply_bitrate(shared: &Shared) {
    let aimd = shared.rate.lock().unwrap().target();
    let cap = REMOTE_CAPS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|c| c.values().min().copied());
    let bps = cap.map_or(aimd, |c| c.min(aimd));
    let last = APPLIED_BPS.load(Ordering::Relaxed);
    // Ignore wiggles under 3% so encoders aren't poked twice a second.
    if last != 0 && (bps as f64 - last as f64).abs() < last as f64 * 0.03 {
        return;
    }
    APPLIED_BPS.store(bps, Ordering::Relaxed);
    // Log real moves only: congestion control nudges the rate every half second.
    static LOGGED_BPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let logged = LOGGED_BPS.load(Ordering::Relaxed);
    if logged == 0 || (bps as f64 - logged as f64).abs() >= logged as f64 * 0.15 {
        LOGGED_BPS.store(bps, Ordering::Relaxed);
        let why = if cap.is_some_and(|c| c < aimd) {
            " (remote link)"
        } else {
            ""
        };
        info!("bitrate → {:.1} Mbps{why}", bps as f64 / 1e6);
    }
    shared.video.set_bitrate(bps);
    let _ = shared.events.send(ServerMsg::Bitrate { bps });
}

pub fn shared() -> Option<Arc<Shared>> {
    SHARED.get().cloned()
}

static SHARED: std::sync::OnceLock<Arc<Shared>> = std::sync::OnceLock::new();

/// The WebRTC bridge drops whole frames when its link backs up; the browser only sees
/// skipped frames, never damaged ones, so it reports no loss. The bridge reports it here.
pub fn bridge_congestion(dropped_frames: u32) {
    if let Some(shared) = SHARED.get() {
        let report = crate::rate::Report {
            dropped: dropped_frames,
            frames: 1,
            latency_ms: None,
            received_bps: None,
        };
        apply_rate_report(shared, report);
    }
}

/// A viewer arrived over the internet (WebRTC): start the stream at a rate such links
/// usually take, instead of the LAN maximum, so the first keyframe isn't lost.
pub fn remote_viewer_joined() {
    if let Some(shared) = SHARED.get() {
        let max = shared.rate.lock().unwrap().max();
        let start = (max / 5).max(2_000_000);
        let changed = shared
            .rate
            .lock()
            .unwrap()
            .start_cautiously(start, std::time::Instant::now());
        if changed.is_some() {
            apply_bitrate(shared);
        }
    }
}

/// Where client logs go; set once the server starts.
static CLIENT_LOG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
/// Caps on log lines arriving over the control stream (remote clients).
const LOG_LINES_MAX: usize = 200;
const LOG_LINE_MAX: usize = 4096;

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
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", d.as_secs(), d.subsec_millis())
}
