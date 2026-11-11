//! The connection to another computer's Glass: the same QUIC session (WebTransport over
//! HTTP/3) and wire format a browser uses, from Rust. Video arrives as unreliable datagrams
//! and goes straight to the hardware decoder on this thread: no JavaScript, no worker hop,
//! no canvas copy.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use std::path::PathBuf;

use glass_proto::{CursorPos, InputEvent, KIND_AUDIO, KIND_AUDIO_OPUS, KIND_CURSOR, KIND_KEYFRAME_REQUEST, KIND_NACK, KIND_PING, KIND_PONG, KIND_VIDEO, MAX_STREAMS, VideoHeader};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use wtransport::tls::Sha256Digest;
use wtransport::{ClientConfig, Endpoint};

use super::assemble::{Action, Assembler};
use super::{Status, Viewer};

/// A computer to connect to.
#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub dns: String,
    /// This computer: its token is right here, so no tailnet lookup is needed.
    pub is_self: bool,
    /// The host's access token, when there's no tailnet identity to go on.
    pub token: Option<String>,
    /// A guest link's code (`#l=`), traded for a pass on first use.
    pub link: Option<String>,
    /// Remote access (`…/app/#r=key`): the introducer's address and the host's public key.
    pub remote: Option<(String, String)>,
}

impl Target {
    /// A Glass link: `https://mac.tailnet.ts.net/#l=code`, `…#t=token`, or just an address.
    pub fn from_url(url: &str) -> Option<Self> {
        let url = url.trim();
        let url = url.strip_prefix("web+glass://").or_else(|| url.strip_prefix("web+glass:")).map_or(url.to_string(), |rest| format!("https://{rest}"));
        let (base, hash) = url.split_once('#').unwrap_or((&url, ""));
        let rest = base.split_once("://").map_or(base, |(_, r)| r);
        let authority = rest.split('/').next()?.trim();
        if authority.is_empty() || authority.contains(' ') {
            return None;
        }
        let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
        let param = |k: &str| hash.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('='))).map(str::to_string);
        // With a port, the address as written; without, the tailnet name (tried on 443, then 8080).
        let dns = if authority.contains(':') || base.starts_with("http://") { format!("{}://{authority}", if base.starts_with("http://") { "http" } else { "https" }) } else { authority.to_string() };
        if let Some(key) = param("r") {
            let origin = base.split_once("://").map_or(format!("https://{authority}"), |(scheme, _)| format!("{scheme}://{authority}"));
            return Some(Self { name: "Remote computer".into(), dns: format!("remote:{}", &key[..key.len().min(12)]), is_self: false, token: param("t"), link: param("l"), remote: Some((origin, key)) });
        }
        Some(Self {
            // A name's first label ("mac"), or an address as it is.
            name: if host.parse::<std::net::IpAddr>().is_ok() || host.contains(':') { host.to_string() } else { host.split('.').next().unwrap_or(host).to_string() },
            dns,
            is_self: false,
            token: param("t"),
            link: param("l"),
            remote: None,
        })
    }
}

/// To the session task.
pub enum Out {
    Input(InputEvent),
    Control(Value),
    File { path: PathBuf, dir: Option<String> },
    /// Send this Mac's microphone (and tell the host), or stop.
    Mic(bool),
    ClipImage { mime: String, bytes: Vec<u8>, text: String, paste: bool },
}

/// Why a session ended.
enum End {
    /// Turned away: asking again the same way won't help.
    Refused(String),
    Closed(String),
}

/// Runs a viewer's connection until the handle goes, reconnecting when it drops.
pub async fn run(target: Target, viewer: Arc<Viewer>, mut rx: mpsc::UnboundedReceiver<Out>) {
    let mut backoff = Duration::from_millis(300);
    loop {
        viewer.set_status(Status::Connecting(format!("Connecting to {}…", target.name)));
        let started = Instant::now();
        let end = once(&target, &viewer, &mut rx).await;
        match end {
            Ok(End::Refused(msg)) => {
                viewer.set_status(Status::Refused(msg));
                return;
            }
            Ok(End::Closed(why)) => {
                step(&format!("ended: {why}"));
                tracing::info!("viewer: {}: closed ({why})", target.dns)
            }
            Err(e) => {
                step(&format!("failed: {e:#}"));
                tracing::warn!("viewer: {}: {e:#}", target.dns);
                viewer.set_status(Status::Connecting(format!("Can't reach {} yet: {e}. Retrying…", target.name)));
            }
        }
        if rx.is_closed() {
            return;
        }
        // A session that ran a while gets a fresh start; one that keeps failing backs off.
        if started.elapsed() > Duration::from_secs(10) {
            backoff = Duration::from_millis(300);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(5));
        // Input typed while disconnected is stale.
        while rx.try_recv().is_ok() {}
    }
}

/// `GLASS_VIEWER_RESOLVE=name=ip`: reach a host by its name (so its certificate still
/// matches) at this address. For tests: a computer off the tailnet reaching one on it over
/// the local network, or this Mac reaching itself as a guest rather than its owner.
fn pinned_address() -> Option<(String, std::net::IpAddr)> {
    let v = std::env::var("GLASS_VIEWER_RESOLVE").ok()?;
    let (name, ip) = v.split_once('=')?;
    Some((name.to_string(), ip.parse().ok()?))
}

#[derive(Debug)]
struct Pinned(String, std::net::IpAddr);

impl ureq::unversioned::resolver::Resolver for Pinned {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        if uri.host() != Some(self.0.as_str()) {
            return ureq::unversioned::resolver::DefaultResolver::default().resolve(uri, config, timeout);
        }
        let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("http") { 80 } else { 443 });
        let mut out = self.empty();
        out.push(std::net::SocketAddr::new(self.1, port));
        Ok(out)
    }
}

impl wtransport::config::DnsResolver for Pinned {
    fn resolve(&self, host: &str) -> std::pin::Pin<Box<dyn wtransport::config::DnsLookupFuture>> {
        let (name, ip, host) = (self.0.clone(), self.1, host.to_string());
        Box::pin(async move {
            match host.rsplit_once(':') {
                Some((h, port)) if h == name => Ok(Some(std::net::SocketAddr::new(ip, port.parse().unwrap_or(443)))),
                _ => Ok(tokio::net::lookup_host(host).await?.next()),
            }
        })
    }
}

struct HostConfig {
    wt_port: u16,
    wt_host: Option<String>,
    cert_hash: Option<[u8; 32]>,
}

/// The host's web addresses to try and its name: for a tailnet name, HTTPS on 443, then on
/// 8080 (where Glass serves when something else, like `tailscale serve`, has 443); or a
/// whole URL (`http://127.0.0.1:8080`, for tests).
fn web_base(dns: &str) -> (Vec<String>, String) {
    match dns.split_once("://") {
        Some((_, rest)) => {
            let authority = rest.split('/').next().unwrap_or(rest);
            let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
            (vec![dns.trim_end_matches('/').to_string()], host.trim_matches(['[', ']']).to_string())
        }
        None => (vec![format!("https://{dns}"), format!("https://{dns}:8080")], dns.to_string()),
    }
}

/// What the host's web server tells browsers: where WebTransport listens, and the hash of
/// its certificate when that's self-signed.
async fn host_config(dns: &str) -> Result<HostConfig> {
    let (bases, _) = web_base(dns);
    let v: Value = tokio::task::spawn_blocking(move || -> Result<Value> {
        // Generous: some hosts are slow to answer (older Windows hosts take seconds).
        let config = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(12))).build();
        let agent: ureq::Agent = match pinned_address() {
            Some((name, ip)) => ureq::Agent::with_parts(config, ureq::unversioned::transport::DefaultConnector::new(), Pinned(name, ip)),
            None => config.new_agent(),
        };
        let mut last = anyhow::anyhow!("no address to try");
        for base in bases {
            // Another app on that port answers too, with something that isn't Glass's config.
            match agent.get(format!("{base}/config.json")).call().map_err(anyhow::Error::from).and_then(|mut r| Ok(r.body_mut().read_json::<Value>()?)) {
                Ok(v) if v.get("wtPort").is_some() => return Ok(v),
                Ok(_) => last = anyhow::anyhow!("{base} isn't Glass"),
                Err(e) => last = e.context(base),
            }
        }
        Err(last)
    })
    .await??;
    let cert_hash = match v.get("certHash") {
        Some(Value::Array(bytes)) => {
            let bytes: Vec<u8> = bytes.iter().filter_map(|b| b.as_u64().map(|b| b as u8)).collect();
            Some(bytes.try_into().map_err(|_| anyhow::anyhow!("bad certificate hash"))?)
        }
        _ => None,
    };
    Ok(HostConfig {
        wt_port: v["wtPort"].as_u64().context("no wtPort")? as u16,
        wt_host: v["wtHost"].as_str().map(str::to_string),
        cert_hash,
    })
}

pub fn codecs() -> Vec<&'static str> {
    let mut codecs = vec!["h264", "opus"];
    #[cfg(target_os = "macos")]
    if super::decode::hevc_supported() {
        codecs.push("hevc");
        // Sharp text in 4:2:2, where this Mac can decode it.
        if super::decode::rext_supported() {
            codecs.push("hevc-rext");
        }
    }
    #[cfg(windows)]
    if super::winmf::hevc_supported() {
        codecs.push("hevc");
        if super::winmf::rext_supported() {
            codecs.push("hevc-rext");
        }
    }
    codecs
}

/// The control stream's messages, read on their own task (a read cut off halfway would lose
/// its place in the stream).
fn read_control(mut recv: Rx) -> mpsc::UnboundedReceiver<Value> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(Some(msg)) = read_json(&mut recv).await {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    rx
}

/// `GLASS_VIEWER_DEBUG`: each step of connecting, on stderr.
/// (A path instead of `1`: appended to that file, for a viewer with no terminal.)
fn step(what: &str) {
    let Some(to) = std::env::var_os("GLASS_VIEWER_DEBUG") else { return };
    let line = format!("{:?} viewer: {what}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default());
    if to == "1" {
        eprintln!("{line}");
    } else {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&to) {
            let _ = writeln!(f, "{line}");
        }
    }
}

async fn once(target: &Target, viewer: &Arc<Viewer>, rx: &mut mpsc::UnboundedReceiver<Out>) -> Result<End> {
    step("start");
    let conn = match &target.remote {
        // Remote access: through glassdesk.dev, then peer to peer.
        Some((base, key)) => Conn::Rtc(Arc::new(super::rtc::RtcConn::connect(base, key).await?)),
        None => {
            let cfg = host_config(&target.dns).await.context("Glass isn't answering there")?;
            step(&format!("config: wt port {}, pinned cert {}", cfg.wt_port, cfg.cert_hash.is_some()));
            let builder = ClientConfig::builder().with_bind_default();
            let config = match cfg.cert_hash {
                Some(hash) => builder.with_server_certificate_hashes([Sha256Digest::new(hash)]),
                None => builder.with_native_certs(),
            }
            .keep_alive_interval(Some(Duration::from_secs(2)))
            .max_idle_timeout(Some(Duration::from_secs(10)))?
            .build();
            let mut config = config;
            if let Some((name, ip)) = pinned_address() {
                config.set_dns_resolver(Pinned(name, ip));
            }
            let endpoint = Endpoint::client(config)?;
            let host = web_base(&target.dns).1;
            let url = format!("https://{}:{}/", cfg.wt_host.as_deref().unwrap_or(&host), cfg.wt_port);
            step(&format!("connecting to {url}"));
            let conn = tokio::time::timeout(Duration::from_secs(8), endpoint.connect(&url)).await.context("timed out")??;
            step("connected");
            tracing::info!("viewer: connected to {url} (datagrams up to {} bytes)", conn.max_datagram_size().unwrap_or(1200));
            Conn::Wt(Arc::new(conn))
        }
    };

    let (mut send, recv) = conn.open_bi().await?;
    step("control stream open");
    let own = || std::fs::read_to_string(crate::default_token_file()).ok().map(|t| t.trim().to_string());
    let token = target.token.clone().or_else(|| if target.is_self { own() } else { None });
    // A link works once: after that, the pass it was traded for.
    let pass = viewer.pass.lock().unwrap().clone().or_else(|| super::prefs::pass(&target.dns));
    let link = if pass.is_some() { None } else { target.link.clone() };
    let device = format!("Glass app on {}", crate::support::computer_name());
    // `cursor`: this Mac's own pointer is the pointer (no delay, none drawn into the video).
    let hello = json!({ "t": "hello", "token": token, "device": device, "codecs": codecs(), "cursor": cfg!(target_os = "macos"), "acks": true, "ua": if cfg!(windows) { "Glass app (Windows)" } else { "Glass app (macOS)" }, "link": link, "pass": pass });
    write_json(&mut send, &hello).await?;
    step("hello sent");
    let mut control = read_control(recv);
    let mut input = conn.open_uni().await?;
    input.write_all(&[crate::control::STREAM_INPUT]).await?;
    // Files, clipboard images and lossless pictures come on streams of their own.
    let incoming = Abort(tokio::spawn(accept_streams(conn.clone(), viewer.clone())));

    let clock = Instant::now();
    let now_ms = || clock.elapsed().as_secs_f64() * 1000.0;
    let ping = |conn: &Conn| {
        let mut p = [0u8; 16];
        p[0] = KIND_PING;
        p[8..16].copy_from_slice(&now_ms().to_le_bytes());
        let _ = conn.send_datagram(p);
    };
    ping(&conn);

    let mut pipes: Vec<Pipe> = (0..MAX_STREAMS).map(|_| Pipe { asm: Assembler::new(Instant::now()), decoder: None }).collect();
    let mut sync = ClockSync::default();
    let mut repair = tokio::time::interval(Duration::from_millis(10));
    let mut pinger = tokio::time::interval(Duration::from_secs(1));
    #[cfg(any(target_os = "macos", windows))]
    let mut player: Option<super::audio::Player> = None;
    let mut sound_failed = false;
    #[cfg(any(target_os = "macos", windows))]
    let mut mic: Option<super::audio::Mic> = None;

    let mut passkey: Option<tokio::task::JoinHandle<Value>> = None;
    loop {
        tokio::select! {
            reply = async { passkey.as_mut().unwrap().await }, if passkey.is_some() => {
                passkey = None;
                let reply = reply.unwrap_or_else(|_| json!({ "t": "passkey", "mode": "error" }));
                step(&format!("passkey: answered {}", reply["mode"]));
                write_json(&mut send, &reply).await?;
            }
            msg = control.recv() => {
                let Some(msg) = msg else { return Ok(End::Closed("control stream ended".into())) };
                match msg["t"].as_str().unwrap_or_default() {
                    "welcome" | "layout" => {
                        if msg["t"] == "welcome" {
                            let repair = msg["features"]["nack"].as_bool().unwrap_or(false);
                            for p in &mut pipes {
                                p.asm.reset(Instant::now());
                                p.asm.repair = repair;
                            }
                            *viewer.welcome.lock().unwrap() = Some(msg.clone());
                            // Its size, if the window has one yet: frames wait for it.
                            let viewport = viewer.viewport.lock().unwrap().clone();
                            if let Some(v) = viewport {
                                write_json(&mut send, &v).await?;
                            }
                            if !viewer.video_on.load(Ordering::Relaxed) {
                                write_json(&mut send, &json!({ "t": "video", "on": false })).await?;
                            }
                            if viewer.audio_on.load(Ordering::Relaxed) && msg["audio"].as_bool().unwrap_or(false) {
                                write_json(&mut send, &json!({ "t": "audio", "on": true })).await?;
                            }
                        }
                        if let Some(d) = msg["displays"].as_array() {
                            *viewer.displays.lock().unwrap() = d.clone();
                        }
                        let streams: Vec<super::Stream> = msg["streams"].as_array().map(|a| a.iter().map(super::Stream::from_json).collect()).unwrap_or_default();
                        let mut live = viewer.live.lock().unwrap();
                        if let Some(s) = streams.first() {
                            (live.width, live.height) = (s.width, s.height);
                        }
                        if let Some(c) = msg["codec"].as_str() {
                            live.codec = c.to_string();
                        }
                        drop(live);
                        *viewer.streams.lock().unwrap() = streams;
                        viewer.set_status(Status::Live);
                    }
                    "passkey" => {
                        // A link made with --passkey: this app answers as its own authenticator.
                        let origin = match &target.remote {
                            Some((base, _)) => base.clone(),
                            None => web_base(&target.dns).0[0].clone(),
                        };
                        let how = if cfg!(windows) { "Windows Hello" } else { "Touch ID or your password" };
                        viewer.set_status(Status::Connecting(format!("Confirm with {how} to use this link's passkey…")));
                        step(&format!("passkey {}: asking", msg["mode"]));
                        // Answered on the side: the session keeps going (and alive) for as
                        // long as the person takes.
                        let (msg, name) = (msg.clone(), target.name.clone());
                        passkey = Some(tokio::spawn(async move { super::passkey::answer(&msg, &origin, &name).await }));
                    }
                    "pass" => {
                        if let Some(p) = msg["pass"].as_str() {
                            *viewer.pass.lock().unwrap() = Some(p.to_string());
                            let expires = msg["expires"].as_u64().unwrap_or(0);
                            super::prefs::update(&target.dns, |m| m.pass = Some((p.to_string(), expires)));
                        }
                    }
                    "codec" => viewer.live.lock().unwrap().codec = msg["codec"].as_str().unwrap_or_default().to_string(),
                    "bitrate" => viewer.live.lock().unwrap().target_mbps = msg["bps"].as_f64().unwrap_or(0.0) / 1e6,
                    "waiting" => viewer.set_status(Status::Connecting(msg["msg"].as_str().unwrap_or("Waiting for someone at the computer to let you in…").into())),
                    "denied" => return Ok(End::Refused(msg["msg"].as_str().unwrap_or("Not let in.").into())),
                    "error" if msg["msg"] == "unauthorized" => {
                        // A pass that stopped working (revoked, expired) isn't offered again.
                        super::prefs::update(&target.dns, |m| m.pass = None);
                        return Ok(End::Refused(format!("{} didn't let this Mac in. Sign in to Tailscale as the same person, or open it in the browser with a link.", target.name)));
                    }
                    "error" => tracing::warn!("viewer: host error: {}", msg["msg"]),
                    _ => {}
                }
                viewer.post(msg);
            }
            dgram = conn.receive_datagram() => {
                let dgram = match dgram {
                    Ok(d) => d,
                    Err(e) => return Ok(End::Closed(format!("{e:#}"))),
                };
                let now = Instant::now();
                match dgram.first().copied() {
                    Some(KIND_VIDEO) => {
                        let Some(h) = VideoHeader::read(&dgram) else { continue };
                        let Some(pipe) = pipes.get_mut(h.stream as usize) else { continue };
                        viewer.live.lock().unwrap().bytes += dgram.len() as u64;
                        pipe.asm.on_fragment(&h, &dgram, now);
                    }
                    Some(KIND_PONG) if dgram.len() >= 24 => {
                        let sent = f64::from_le_bytes(dgram[8..16].try_into().unwrap());
                        let host_us = u64::from_le_bytes(dgram[16..24].try_into().unwrap());
                        sync.pong(sent, now_ms(), host_us);
                        viewer.live.lock().unwrap().rtt_ms = sync.rtt_ms;
                    }
                    Some(KIND_NACK) if dgram.len() >= 8 => {
                        if let Some(p) = pipes.get_mut(dgram[1] as usize) {
                            p.asm.frame_gone(u32::from_le_bytes(dgram[4..8].try_into().unwrap()), now);
                        }
                    }
                    Some(KIND_CURSOR) => {
                        if let Some(c) = CursorPos::read(&dgram) {
                            let mut cur = viewer.cursor.lock().unwrap();
                            // Only what changes how it looks wakes the window.
                            let wake = cur.is_none_or(|p| p.flags != c.flags || p.shape != c.shape || p.stream != c.stream);
                            *cur = Some(c);
                            drop(cur);
                            if wake { viewer.wake() }
                        }
                    }
                    Some(kind @ (KIND_AUDIO | KIND_AUDIO_OPUS)) if !sound_failed && viewer.audio_on.load(Ordering::Relaxed) => {
                        #[cfg(any(target_os = "macos", windows))]
                        {
                            if player.is_none() {
                                match super::audio::Player::new() {
                                    Ok(p) => player = Some(p),
                                    Err(e) => {
                                        tracing::warn!("viewer: no sound: {e:#}");
                                        sound_failed = true;
                                    }
                                }
                            }
                            if let (Some(p), Some(h)) = (player.as_mut(), glass_proto::AudioHeader::read(&dgram)) {
                                viewer.live.lock().unwrap().audio_packets += 1;
                                let body = &dgram[glass_proto::AUDIO_HEADER_LEN..];
                                if kind == KIND_AUDIO_OPUS {
                                    p.opus(body);
                                } else {
                                    let pcm: Vec<i16> = body.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                                    p.push(&pcm, h.channels as usize);
                                }
                            }
                        }
                        let _ = kind;
                    }
                    _ => {}
                }
            }
            out = rx.recv() => {
                match out {
                    Some(Out::Input(ev)) => {
                        // A view-only link: nothing to send (the host would drop it anyway).
                        let allowed = viewer.welcome.lock().unwrap().as_ref().is_none_or(|w| w["perms"]["input"] != false);
                        if allowed {
                            input.write_all(&ev.write()).await?;
                        }
                    }
                    Some(Out::Control(v)) => {
                        // Remembered for the next session too.
                        if v["t"] == "viewport" {
                            *viewer.viewport.lock().unwrap() = Some(v.clone());
                        }
                        if v["t"] == "video" {
                            viewer.video_on.store(v["on"] != false, Ordering::Relaxed);
                        }
                        if v["t"] == "audio" && v["on"] == false {
                            #[cfg(any(target_os = "macos", windows))]
                            { player = None; }
                        }
                        write_json(&mut send, &v).await?;
                    }
                    Some(Out::File { path, dir }) => {
                        tokio::spawn(upload(conn.clone(), viewer.clone(), path, dir));
                    }
                    Some(Out::Mic(on)) => {
                        #[cfg(any(target_os = "macos", windows))]
                        {
                            mic = None;
                            if on {
                                let conn = conn.clone();
                                match super::audio::Mic::start(move |p| conn.send_datagram(p)) {
                                    Ok(m) => mic = Some(m),
                                    Err(e) => viewer.post(json!({ "t": "_error", "msg": format!("Couldn't use the microphone: {e:#}") })),
                                }
                            }
                        }
                        write_json(&mut send, &json!({ "t": "mic", "on": on && mic.is_some() })).await?;
                    }
                    Some(Out::ClipImage { mime, bytes, text, paste }) => {
                        tokio::spawn(send_clip_image(conn.clone(), mime, bytes, text, paste));
                    }
                    None => {
                        conn.close().await;
                        return Ok(End::Closed("viewer closed".into()));
                    }
                }
            }
            _ = repair.tick() => {
                let rtt = Duration::from_secs_f64(sync.rtt_ms / 1000.0);
                for p in &mut pipes { p.asm.repair_tick(rtt, Instant::now()) }
            }
            _ = pinger.tick() => {
                step("tick");
                ping(&conn);
                viewer.live.lock().unwrap().dropped = pipes.iter().map(|p| p.asm.dropped).sum();
                viewer.tick_stats();
            }
        }

        // What reassembly asked for, and the frames it finished.
        for (s, pipe) in pipes.iter_mut().enumerate() {
            let s8 = s as u8;
            let (actions, frames) = pipe.asm.take();
            for a in actions {
                match a {
                    Action::Decode => {}
                    Action::Ack(id) => conn.send_datagram(glass_proto::frame_ack(s8, id)),
                    Action::Keyframe => conn.send_datagram([KIND_KEYFRAME_REQUEST, s8]),
                    Action::Nack(id, frags) => conn.send_datagram(glass_proto::nack(s8, id, &frags)),
                }
            }
            for f in frames {
                // A picture that really changed takes sharp-text pictures off.
                if !f.refresh {
                    let mut ll = viewer.lossless.lock().unwrap();
                    if ll.iter().any(|l| l.stream == s && l.capture_us < f.capture_us) {
                        ll.retain(|l| l.stream != s || l.capture_us >= f.capture_us);
                    }
                }
                let ok = decode(&mut pipe.decoder, s, &f, &conn, viewer, &sync, clock);
                if !ok {
                    pipe.decoder.take_if(|d| d.failed.load(Ordering::Relaxed));
                    pipe.asm.request_keyframe(Instant::now());
                    let _ = conn.send_datagram(glass_proto::frame_ack(s8, f.id));
                }
            }
        }
        let _ = &incoming;
    }
}

struct Pipe {
    asm: Assembler,
    decoder: Option<Decoding>,
}

/// Ends a task when dropped.
struct Abort(tokio::task::JoinHandle<()>);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn read_exact(r: &mut Rx, n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

/// A JSON header: u32 LE length, then the JSON.
async fn read_header(r: &mut Rx) -> Result<Value> {
    let len = u32::from_le_bytes(read_exact(r, 4).await?.try_into().unwrap()) as usize;
    if len > 1 << 20 {
        anyhow::bail!("header too long");
    }
    Ok(serde_json::from_slice(&read_exact(r, len).await?)?)
}

async fn read_rest(r: &mut Rx, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 << 10];
    while let Some(n) = r.read(&mut buf).await? {
        out.extend_from_slice(&buf[..n]);
        if out.len() > limit {
            anyhow::bail!("too big");
        }
    }
    Ok(out)
}

/// Streams the host opens: files it sends (to Downloads), clipboard images, and lossless
/// pictures for sharp text.
async fn accept_streams(conn: Conn, viewer: Arc<Viewer>) {
    while let Ok(mut r) = conn.accept_uni().await {
        let viewer = viewer.clone();
        tokio::spawn(async move {
            let result: Result<()> = async {
                let tag = read_exact(&mut r, 1).await?[0];
                let header = read_header(&mut r).await?;
                match tag {
                    crate::control::STREAM_FILE => receive_file(&mut r, &header, &viewer).await,
                    crate::control::STREAM_CLIPBOARD => {
                        let bytes = read_rest(&mut r, crate::clip::MAX_IMAGE * 2).await?;
                        let mime = header["mime"].as_str().unwrap_or("image/png").to_string();
                        *viewer.clip_image.lock().unwrap() = Some((mime, bytes));
                        viewer.post(json!({ "t": "_clipboardImage", "id": header["id"] }));
                        Ok(())
                    }
                    crate::control::STREAM_LOSSLESS => {
                        let png = read_rest(&mut r, 64 << 20).await?;
                        let n = |k: &str| header[k].as_u64().unwrap_or(0) as u32;
                        let l = super::Lossless {
                            stream: n("stream") as usize,
                            x: n("x"),
                            y: n("y"),
                            w: n("w"),
                            h: n("h"),
                            fw: n("fw"),
                            fh: n("fh"),
                            capture_us: header["capture_us"].as_u64().unwrap_or(0),
                            png: Arc::new(png),
                        };
                        let mut ll = viewer.lossless.lock().unwrap();
                        // A whole picture replaces what was there; parts stack on top.
                        if l.x == 0 && l.y == 0 && l.w == l.fw && l.h == l.fh {
                            ll.retain(|o| o.stream != l.stream);
                        }
                        ll.push(l);
                        if ll.len() > 32 {
                            ll.remove(0);
                        }
                        drop(ll);
                        viewer.wake();
                        Ok(())
                    }
                    _ => Ok(()),
                }
            }
            .await;
            if let Err(e) = result {
                tracing::warn!("viewer: incoming stream: {e:#}");
            }
        });
    }
}

/// Where a file from the other computer goes: this Mac's Downloads, under a name that's free.
fn download_path(name: &str) -> PathBuf {
    let dir = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join("Downloads");
    let _ = std::fs::create_dir_all(&dir);
    let clean: String = name.chars().map(|c| if c == '/' || c == '\\' || c == '\0' { '_' } else { c }).collect();
    let clean = clean.trim_start_matches('.').to_string();
    let clean = if clean.is_empty() { "file".to_string() } else { clean };
    let first = dir.join(&clean);
    if !first.exists() {
        return first;
    }
    let p = std::path::Path::new(&clean);
    let (stem, ext) = (p.file_stem().and_then(|s| s.to_str()).unwrap_or("file"), p.extension().and_then(|s| s.to_str()));
    for i in 2.. {
        let name = match ext {
            Some(e) => format!("{stem} {i}.{e}"),
            None => format!("{stem} {i}"),
        };
        if !dir.join(&name).exists() {
            return dir.join(name);
        }
    }
    unreachable!()
}

async fn receive_file(r: &mut Rx, header: &Value, viewer: &Arc<Viewer>) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let name = header["name"].as_str().unwrap_or("file").to_string();
    let size = header["size"].as_u64().unwrap_or(0);
    let streamed = header["streamed"].as_bool().unwrap_or(false);
    let path = download_path(&name);
    let part = path.with_extension(format!("{}glassdownload", path.extension().map_or(String::new(), |e| format!("{}.", e.to_string_lossy()))));
    let mut file = tokio::fs::File::create(&part).await?;
    let mut buf = vec![0u8; 256 << 10];
    let mut got = 0u64;
    let mut last = Instant::now();
    while let Some(n) = r.read(&mut buf).await? {
        file.write_all(&buf[..n]).await?;
        got += n as u64;
        if last.elapsed() > Duration::from_millis(250) {
            last = Instant::now();
            viewer.post(json!({ "t": "_progress", "name": name, "done": got, "total": if streamed { Value::Null } else { size.into() }, "dir": "down" }));
        }
    }
    file.flush().await?;
    drop(file);
    if !streamed && got != size {
        let _ = tokio::fs::remove_file(&part).await;
        anyhow::bail!("{name}: cut short after {got} of {size} bytes");
    }
    tokio::fs::rename(&part, &path).await?;
    viewer.post(json!({ "t": "_file", "name": name, "path": path.to_string_lossy(), "size": got }));
    Ok(())
}

/// Sends a file to the other computer (a folder: each file in it).
async fn upload(conn: Conn, viewer: Arc<Viewer>, path: PathBuf, dir: Option<String>) {
    let mut files = Vec::new();
    collect(&path, &mut files);
    for f in files {
        if let Err(e) = upload_one(&conn, &viewer, &f, dir.clone()).await {
            viewer.post(json!({ "t": "_uploadFailed", "name": f.file_name().map(|n| n.to_string_lossy().to_string()), "error": format!("{e:#}") }));
        }
    }
}

fn collect(path: &std::path::Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for e in entries.flatten() {
                collect(&e.path(), out);
            }
        }
    } else if path.is_file() {
        out.push(path.to_path_buf());
    }
}

async fn upload_one(conn: &Conn, viewer: &Viewer, path: &std::path::Path, dir: Option<String>) -> Result<()> {
    use tokio::io::AsyncReadExt;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    let size = tokio::fs::metadata(path).await?.len();
    let mut header = json!({ "name": name, "size": size });
    if let Some(d) = dir {
        header["dir"] = d.into();
    }
    let header = serde_json::to_vec(&header)?;
    let mut s = conn.open_uni().await?;
    let mut prefix = vec![crate::control::STREAM_FILE];
    prefix.extend_from_slice(&(header.len() as u32).to_le_bytes());
    prefix.extend_from_slice(&header);
    s.write_all(&prefix).await?;
    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 256 << 10];
    let mut sent = 0u64;
    let mut last = Instant::now();
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        s.write_all(&buf[..n]).await?;
        sent += n as u64;
        if last.elapsed() > Duration::from_millis(250) {
            last = Instant::now();
            viewer.post(json!({ "t": "_progress", "name": name, "done": sent, "total": size, "dir": "up" }));
        }
    }
    s.finish().await?;
    viewer.post(json!({ "t": "_progress", "name": name, "done": size, "total": size, "dir": "up" }));
    Ok(())
}

async fn send_clip_image(conn: Conn, mime: String, bytes: Vec<u8>, text: String, paste: bool) {
    let result: Result<()> = async {
        let header = serde_json::to_vec(&json!({ "mime": mime, "size": bytes.len(), "text": text, "paste": paste }))?;
        let mut s = conn.open_uni().await?;
        let mut prefix = vec![crate::control::STREAM_CLIPBOARD];
        prefix.extend_from_slice(&(header.len() as u32).to_le_bytes());
        prefix.extend_from_slice(&header);
        s.write_all(&prefix).await?;
        s.write_all(&bytes).await?;
        s.finish().await?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::warn!("viewer: clipboard image: {e:#}");
    }
}

/// The host's clock against ours, from pings: frames carry when they were captured (host
/// clock), which makes the capture-to-screen delay measurable here.
#[derive(Default)]
struct ClockSync {
    /// host_us - our_us, from the ping with the shortest round trip.
    offset_us: Option<f64>,
    best_rtt: f64,
    rtt_ms: f64,
}

impl ClockSync {
    fn pong(&mut self, sent_ms: f64, now_ms: f64, host_us: u64) {
        let rtt = (now_ms - sent_ms).max(0.0);
        self.rtt_ms = if self.rtt_ms == 0.0 { rtt } else { self.rtt_ms * 0.8 + rtt * 0.2 };
        // A fast round trip says the most about the offset; slowly forget old ones.
        if self.offset_us.is_none() || rtt <= self.best_rtt * 1.2 {
            self.best_rtt = rtt;
            self.offset_us = Some(host_us as f64 - (sent_ms + rtt / 2.0) * 1000.0);
        } else {
            self.best_rtt *= 1.01;
        }
    }

    /// How long ago (ms) the host captured a frame, if the clocks are synced.
    fn age_ms(&self, capture_us: u64, now_ms: f64) -> Option<f64> {
        Some(((now_ms * 1000.0 + self.offset_us?) - capture_us as f64) / 1000.0)
    }
}

struct Decoding {
    #[cfg(target_os = "macos")]
    decoder: super::decode::Decoder,
    #[cfg(windows)]
    decoder: WinDecoder,
    /// The decoder said it couldn't decode a frame.
    failed: Arc<AtomicBool>,
}

/// Decodes one frame into the viewer's picture; false if it couldn't be.
#[cfg(target_os = "macos")]
fn decode(slot: &mut Option<Decoding>, stream: usize, f: &super::assemble::Frame, conn: &Conn, viewer: &Arc<Viewer>, sync: &ClockSync, clock: Instant) -> bool {
    use super::decode::Decoder;
    if f.keyframe && !slot.as_ref().is_some_and(|d| d.decoder.fits(&f.data, f.hevc)) {
        *slot = None;
        let failed = Arc::new(AtomicBool::new(false));
        let (conn, viewer, flag) = (conn.clone(), viewer.clone(), failed.clone());
        let output: super::decode::Output = Arc::new(move |pic| match pic {
            Some((pic, id)) => {
                viewer.show(stream, pic);
                // Acked once decoded and handed to the screen: the host paces video by these.
                conn.send_datagram(glass_proto::frame_ack(stream as u8, id));
            }
            None => flag.store(true, Ordering::Relaxed),
        });
        match Decoder::new(&f.data, f.hevc, output) {
            Ok(decoder) => *slot = Some(Decoding { decoder, failed }),
            Err(e) => {
                tracing::warn!("viewer: no decoder: {e:#}");
                return false;
            }
        }
    }
    let Some(d) = slot.as_ref() else { return false };
    if let Err(e) = d.decoder.decode(&f.data, f.id) {
        tracing::warn!("viewer: {e:#}");
        d.failed.store(true, Ordering::Relaxed);
        return false;
    }
    if d.failed.swap(false, Ordering::Relaxed) {
        return false;
    }
    if f.hevc {
        viewer.live.lock().unwrap().hevc_decoded += 1;
    }
    let now_ms = clock.elapsed().as_secs_f64() * 1000.0;
    if let Some(age) = sync.age_ms(f.capture_us, now_ms) {
        viewer.live.lock().unwrap().note_latency(age);
    }
    true
}

/// H.264 through OpenH264; HEVC (and 4:2:2) through Media Foundation.
#[cfg(windows)]
enum WinDecoder {
    H264(super::win::Decoder),
    Hevc(super::winmf::Decoder),
}

#[cfg(windows)]
impl WinDecoder {
    fn new(hevc: bool) -> Result<Self> {
        Ok(if hevc { Self::Hevc(super::winmf::Decoder::new()?) } else { Self::H264(super::win::Decoder::new()?) })
    }

    fn hevc(&self) -> bool {
        matches!(self, Self::Hevc(_))
    }

    fn decode(&mut self, frame: &[u8], id: u32) -> Result<Option<super::win::Picture>> {
        match self {
            Self::H264(d) => d.decode(frame),
            Self::Hevc(d) => d.decode(frame, id),
        }
    }
}

/// Windows: decoded into an image the window draws.
#[cfg(windows)]
fn decode(slot: &mut Option<Decoding>, stream: usize, f: &super::assemble::Frame, conn: &Conn, viewer: &Arc<Viewer>, sync: &ClockSync, clock: Instant) -> bool {
    // A codec change (sharp text turning 4:2:2 on) starts a new decoder at its keyframe.
    if f.keyframe && slot.as_ref().is_none_or(|d| d.decoder.hevc() != f.hevc) {
        *slot = None;
        match WinDecoder::new(f.hevc) {
            Ok(decoder) => *slot = Some(Decoding { decoder, failed: Arc::new(AtomicBool::new(false)) }),
            Err(e) => {
                tracing::warn!("viewer: no decoder: {e:#}");
                return false;
            }
        }
    }
    let Some(d) = slot.as_mut() else { return false };
    match d.decoder.decode(&f.data, f.id) {
        Ok(Some(pic)) => {
            if f.hevc {
                viewer.live.lock().unwrap().hevc_decoded += 1;
            }
            viewer.show(stream, pic);
            conn.send_datagram(glass_proto::frame_ack(stream as u8, f.id));
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("viewer: {e:#}");
            d.failed.store(true, Ordering::Relaxed);
            return false;
        }
    }
    let now_ms = clock.elapsed().as_secs_f64() * 1000.0;
    if let Some(age) = sync.age_ms(f.capture_us, now_ms) {
        viewer.live.lock().unwrap().note_latency(age);
    }
    true
}

#[cfg(not(any(target_os = "macos", windows)))]
fn decode(_: &mut Option<Decoding>, _: usize, _: &super::assemble::Frame, _: &Conn, _: &Arc<Viewer>, _: &ClockSync, _: Instant) -> bool {
    false
}

/// The session's transport: WebTransport (tailnet, LAN) or WebRTC (remote access).
#[derive(Clone)]
pub enum Conn {
    Wt(Arc<wtransport::Connection>),
    Rtc(Arc<super::rtc::RtcConn>),
}

pub enum Tx {
    Wt(wtransport::SendStream),
    Rtc(super::rtc::RtcTx),
}

pub enum Rx {
    Wt(wtransport::RecvStream),
    Rtc(super::rtc::RtcRx),
}

impl Conn {
    pub fn send_datagram(&self, bytes: impl AsRef<[u8]>) {
        match self {
            Conn::Wt(c) => {
                let _ = c.send_datagram(bytes.as_ref());
            }
            Conn::Rtc(c) => c.send_datagram(bytes.as_ref()),
        }
    }

    async fn receive_datagram(&self) -> Result<bytes::Bytes> {
        match self {
            Conn::Wt(c) => Ok(c.receive_datagram().await?.payload()),
            Conn::Rtc(c) => c.receive_datagram().await,
        }
    }

    async fn open_bi(&self) -> Result<(Tx, Rx)> {
        Ok(match self {
            Conn::Wt(c) => {
                let (s, r) = c.open_bi().await?.await?;
                (Tx::Wt(s), Rx::Wt(r))
            }
            Conn::Rtc(c) => {
                let (s, r) = c.open_bi().await?;
                (Tx::Rtc(s), Rx::Rtc(r))
            }
        })
    }

    async fn open_uni(&self) -> Result<Tx> {
        Ok(match self {
            Conn::Wt(c) => Tx::Wt(c.open_uni().await?.await?),
            Conn::Rtc(c) => Tx::Rtc(c.open_uni().await?),
        })
    }

    async fn accept_uni(&self) -> Result<Rx> {
        Ok(match self {
            Conn::Wt(c) => Rx::Wt(c.accept_uni().await?),
            Conn::Rtc(c) => Rx::Rtc(c.accept_uni().await?),
        })
    }

    async fn close(&self) {
        match self {
            Conn::Wt(c) => c.close(0u32.into(), b"closed"),
            Conn::Rtc(c) => c.close().await,
        }
    }
}

impl Tx {
    async fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            Tx::Wt(s) => Ok(s.write_all(bytes).await?),
            Tx::Rtc(s) => s.write_all(bytes).await,
        }
    }

    async fn finish(&mut self) -> Result<()> {
        match self {
            Tx::Wt(s) => Ok(s.finish().await?),
            Tx::Rtc(s) => s.finish().await,
        }
    }
}

impl Rx {
    async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>> {
        match self {
            Rx::Wt(r) => Ok(r.read(buf).await?),
            Rx::Rtc(r) => r.read(buf).await,
        }
    }

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut got = 0;
        while got < buf.len() {
            match self.read(&mut buf[got..]).await? {
                Some(n) => got += n,
                None => anyhow::bail!("stream ended early"),
            }
        }
        Ok(())
    }
}

/// A control message: u32 LE length, then JSON (see `crate::control`).
async fn write_json(s: &mut Tx, v: &Value) -> Result<()> {
    let body = serde_json::to_vec(v)?;
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&body);
    s.write_all(&frame).await
}

async fn read_json(r: &mut Rx) -> Result<Option<Value>> {
    let mut len = [0u8; 4];
    match r.read(&mut len[..1]).await? {
        None => return Ok(None),
        Some(_) => r.read_exact(&mut len[1..]).await?,
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > 16 << 20 {
        anyhow::bail!("control message too long");
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}
