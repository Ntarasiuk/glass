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
    /// Where it is, for a computer found on this network (`nearby`): `dns` names it (and
    /// is what its certificate and this app's memory of it go by), this reaches it.
    pub at: Option<std::net::IpAddr>,
    /// The name it was typed by (`studio.lan`), kept to find it again when its address
    /// changes ([`Target::settle`]).
    pub typed: Option<String>,
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
            return Some(Self { name: "Remote computer".into(), dns: format!("remote:{}", &key[..key.len().min(12)]), is_self: false, token: param("t"), link: param("l"), remote: Some((origin, key)), at: None, typed: None });
        }
        Some(Self {
            // A name's first label ("mac"), or an address as it is.
            name: if host.parse::<std::net::IpAddr>().is_ok() || host.contains(':') { host.to_string() } else { host.split('.').next().unwrap_or(host).to_string() },
            dns,
            is_self: false,
            token: param("t"),
            link: param("l"),
            remote: None,
            at: None,
            typed: None,
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

/// Who a host says it is (`/config.json`'s `self`), once that checks out.
#[derive(Clone, Debug, PartialEq)]
pub struct Who {
    pub id: String,
    pub name: String,
    pub os: String,
    /// The key it proves itself with: it signed the certificate this viewer pins.
    pub key: String,
    /// The name its web certificate is for, when a certificate authority gave it one.
    pub tls: Option<String>,
    /// What it's called on its network's DNS, as far as it knows (`studio.lan`).
    pub host: Option<String>,
}

struct HostConfig {
    wt_port: u16,
    wt_host: Option<String>,
    cert_hash: Option<[u8; 32]>,
    who: Option<Who>,
    /// The web address that answered.
    base: String,
}

/// The host's web addresses to try and its name: for a tailnet name, HTTPS on 443, then on
/// 8080 (where Glass serves when something else, like `tailscale serve`, has 443); or a
/// whole URL (`http://127.0.0.1:8080`, for tests).
fn web_base(dns: &str) -> (Vec<String>, String) {
    match dns.split_once("://") {
        Some((_, rest)) => {
            let authority = rest.split('/').next().unwrap_or(rest);
            let host = authority.rsplit_once(':').filter(|(h, _)| !h.contains(':') || h.ends_with(']')).map_or(authority, |(h, _)| h);
            (vec![dns.trim_end_matches('/').to_string()], host.trim_matches(['[', ']']).to_string())
        }
        None => (vec![format!("https://{dns}"), format!("https://{dns}:8080")], dns.to_string()),
    }
}

/// Asks each web address in turn for `/config.json`; the first that's Glass, and which.
/// `pin`: reach that name at this address. `unverified`: read it whatever certificate is
/// there, only ever to learn the name to check it by ([`Target::settle`]).
fn fetch_config(bases: &[String], pin: Option<(String, std::net::IpAddr)>, timeout: Duration, unverified: bool) -> Result<(Value, String)> {
    let mut config = ureq::Agent::config_builder().timeout_global(Some(timeout));
    if unverified {
        config = config.tls_config(ureq::tls::TlsConfig::builder().disable_verification(true).build());
    }
    let config = config.build();
    let agent: ureq::Agent = match pin {
        Some((name, ip)) => ureq::Agent::with_parts(config, ureq::unversioned::transport::DefaultConnector::new(), Pinned(name, ip)),
        None => config.new_agent(),
    };
    let mut last = anyhow::anyhow!("no address to try");
    for base in bases {
        // Another app on that port answers too, with something that isn't Glass's config.
        match agent.get(format!("{base}/config.json")).call().map_err(anyhow::Error::from).and_then(|mut r| Ok(r.body_mut().read_json::<Value>()?)) {
            Ok(v) if v.get("wtPort").is_some() => return Ok((v, base.clone())),
            Ok(_) => last = anyhow::anyhow!("{base} isn't Glass"),
            Err(e) => last = e.context(base.clone()),
        }
    }
    Err(last)
}

/// What a host's `/config.json` says. Who it claims to be is only kept when it's proved:
/// its key signed the self-signed certificate named here (a certificate from an authority
/// was checked on the way in).
fn read_config(v: &Value, base: String) -> Result<HostConfig> {
    let cert_hash: Option<[u8; 32]> = match v.get("certHash") {
        Some(Value::Array(bytes)) => {
            let bytes: Vec<u8> = bytes.iter().filter_map(|b| b.as_u64().map(|b| b as u8)).collect();
            Some(bytes.try_into().map_err(|_| anyhow::anyhow!("bad certificate hash"))?)
        }
        _ => None,
    };
    let me = &v["self"];
    let text = |k: &str, max: usize| -> String { me[k].as_str().unwrap_or_default().chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_string() };
    // These go into an address and a file name's worth of memory: nothing odd in them.
    let plain = |s: &str, extra: &[char]| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(&c));
    let (id, key) = (text("id", 32), text("key", 128));
    let who = if id.len() >= 8 && plain(&id, &['-', '_']) && plain(&key, &['-', '_']) {
        if let Some(hash) = &cert_hash {
            anyhow::ensure!(
                crate::remote::verify(&key, &crate::remote::direct_payload(hash), me["sig"].as_str().unwrap_or_default()),
                "the computer there couldn't prove who it is"
            );
        }
        let name = text("name", 64);
        Some(Who {
            id,
            key,
            name: if name.is_empty() { "Glass computer".into() } else { name },
            os: text("os", 16),
            tls: Some(text("tls", 253)).filter(|t| plain(t, &['-', '.'])),
            host: Some(text("host", 253)).filter(|t| plain(t, &['-', '.'])),
        })
    } else {
        None
    };
    Ok(HostConfig {
        wt_port: v["wtPort"].as_u64().context("no wtPort")? as u16,
        wt_host: v["wtHost"].as_str().map(str::to_string),
        cert_hash,
        who,
        base,
    })
}

/// What the host's web server tells browsers: where WebTransport listens, and the hash of
/// its certificate when that's self-signed.
async fn host_config(dns: &str, at: Option<std::net::IpAddr>) -> Result<HostConfig> {
    let (bases, host) = web_base(dns);
    tokio::task::spawn_blocking(move || {
        // Generous: some hosts are slow to answer (older Windows hosts take seconds).
        let (v, base) = fetch_config(&bases, at.map(|ip| (host, ip)).or_else(pinned_address), Duration::from_secs(12), false)?;
        read_config(&v, base)
    })
    .await?
}

/// How long a remembered computer gets to answer when the list is only checking on it.
const REACH_TIMEOUT: Duration = Duration::from_millis(1500);

/// Where a computer's Glass opens in a browser: the first of its web addresses that
/// answers as Glass. Blocking, briefly.
pub fn web_address(dns: &str, at: Option<std::net::IpAddr>) -> String {
    let (bases, host) = web_base(dns);
    browser_address(&bases, at.map(|ip| (host, ip)).or_else(pinned_address), REACH_TIMEOUT)
}

/// When none answers in time, the last one: Glass always serves its web port, and the
/// standard one only when it could have it (another app there answers instead).
fn browser_address(bases: &[String], pin: Option<(String, std::net::IpAddr)>, timeout: Duration) -> String {
    let base = fetch_config(bases, pin, timeout, false).map_or_else(|_| bases.last().cloned().unwrap_or_default(), |(_, base)| base);
    format!("{base}/")
}

/// Whether the computer remembered as `saved` answers at `at` right now, and is still that
/// computer. Blocking.
fn is_at(saved: &super::saved::Computer, at: std::net::IpAddr, timeout: Duration) -> bool {
    let (bases, host) = web_base(&saved.url);
    let Ok(cfg) = fetch_config(&bases, Some((host, at)), timeout, false).and_then(|(v, base)| read_config(&v, base)) else { return false };
    still(saved, &cfg)
}

/// Over HTTPS its certificate's name was checked; otherwise it's the key that says so.
fn still(saved: &super::saved::Computer, cfg: &HostConfig) -> bool {
    cfg.base.starts_with("https://") && cfg.cert_hash.is_none() || cfg.cert_hash.is_some() && cfg.who.as_ref().is_some_and(|w| w.key == saved.key)
}

/// Where a remembered computer answers right now, if anywhere: its addresses, newest
/// first. Over a VPN into its network that's the address it had there; and when its router
/// has since given it another, the address its name has now. Blocking.
pub fn reach(saved: &super::saved::Computer) -> Option<std::net::IpAddr> {
    if let Some(at) = saved.addrs.iter().copied().find(|at| is_at(saved, *at, REACH_TIMEOUT)) {
        return Some(at);
    }
    // Only ever a private address: a name is a hint the computer gave, not somewhere on
    // the internet to go asking.
    let mut tried = saved.addrs.clone();
    saved.names.iter().flat_map(|name| lookup(name)).filter(|ip| crate::nearby::private(*ip)).find(|at| {
        let new = !tried.contains(at);
        tried.push(*at);
        new && is_at(saved, *at, REACH_TIMEOUT)
    })
}

/// A name's addresses, IPv4 first, if its network's DNS answers soon. Blocking, briefly.
fn lookup(name: &str) -> Vec<std::net::IpAddr> {
    use std::net::ToSocketAddrs;
    let (tx, rx) = std::sync::mpsc::channel();
    let name = name.to_string();
    // The system's lookup has no timeout of its own: left to finish by itself.
    std::thread::spawn(move || {
        let mut found: Vec<std::net::IpAddr> = (name.as_str(), 0).to_socket_addrs().map(|a| a.map(|a| a.ip()).collect()).unwrap_or_default();
        found.sort_by_key(|ip| !ip.is_ipv4());
        let _ = tx.send(found);
    });
    rx.recv_timeout(REACH_TIMEOUT).unwrap_or_default()
}

/// The names to look a computer up by later: the one it was typed by, and the one it says
/// it has (in full, and alone: a VPN's DNS often adds the network's own ending). Not
/// `.local` ones: those are only heard on its own network, where it's found anyway. Whatever
/// a name turns out to be, only a private address is ever asked ([`reach`]).
pub(super) fn names(typed: Option<&str>, host: Option<&str>) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    let short = host.and_then(|h| h.split('.').next());
    for name in typed.into_iter().chain(host).chain(short) {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if !name.is_empty() && name.parse::<std::net::IpAddr>().is_err() && !name.ends_with(".local") && name != "localhost" && !all.contains(&name) {
            all.push(name);
        }
    }
    all
}

/// A name or address on a private network rather than the internet: where a computer can
/// be without a certificate, or with one for another name (its tailnet's).
fn local_host(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok() || !host.contains('.') || [".local", ".lan", ".home", ".home.arpa", ".localdomain", ".internal"].iter().any(|d| host.ends_with(d))
}

/// The web addresses to try for a typed private address, best first. Without a certificate
/// Glass is plain HTTP there, and `from_url` reads a bare `host:port` as HTTPS, so each is
/// tried both ways; `http://…` typed out is taken at its word.
fn typed_bases(dns: &str, given: &[String], host: &str) -> Vec<String> {
    let authority = if host.contains(':') { format!("[{host}]") } else { host.to_string() };
    match dns.strip_prefix("https://") {
        Some(rest) if rest.starts_with(&format!("{authority}:")) => vec![dns.to_string(), format!("http://{rest}")],
        Some(_) => given.to_vec(),
        None if dns.contains("://") => given.to_vec(),
        // No port typed: where Glass serves by default, then 443 (it has a certificate).
        None => vec![format!("https://{authority}:8080"), format!("http://{authority}:8080"), format!("https://{authority}")],
    }
}

impl Target {
    /// An address someone typed (`192.168.1.20`, `studio.lan:8080`) turned into the
    /// computer that's there: what it calls itself, the name it's remembered by, and where
    /// it is. So it's the same computer as when it's heard on the network, with the same
    /// pass. Anything else (a link, a tailnet name) comes back as it was. Blocking.
    pub fn settle(mut self) -> Result<Self> {
        if self.remote.is_some() || self.at.is_some() || self.is_self {
            return Ok(self);
        }
        let (given, host) = web_base(&self.dns);
        use std::net::ToSocketAddrs;
        let found = (host.as_str(), 0).to_socket_addrs().map(|a| a.map(|a| a.ip()).collect::<Vec<_>>());
        let private = |ip: &&std::net::IpAddr| ip.is_ipv4() && crate::nearby::private(**ip);
        // Any other name is private too when it has a private address (a router's own
        // ending for its network: `studio.attlocal.net`), and that's the one asked.
        if !local_host(&host) && !found.as_ref().is_ok_and(|f| f.iter().any(|ip| private(&ip))) {
            return Ok(self);
        }
        let found = found.with_context(|| format!("Can't find {host}"))?;
        let at = *found.iter().find(private).or(found.iter().find(|ip| ip.is_ipv4())).or(found.first()).with_context(|| format!("Can't find {host}"))?;
        let mut bases = typed_bases(&self.dns, &given, &host);
        // A host only answers to names it knows are its own (a site could point any other
        // at it), and its router's name for it isn't one: asked by its address too.
        if host.parse::<std::net::IpAddr>().is_err() {
            let (name, ip) = (format!("://{host}"), format!("://{}", if at.is_ipv6() { format!("[{at}]") } else { at.to_string() }));
            let by_address: Vec<String> = bases.iter().filter(|b| b.starts_with("http://")).map(|b| b.replacen(&name, &ip, 1)).collect();
            bases.extend(by_address);
        }
        let port = |base: &str| base.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok());
        let pin = Some((host.clone(), at));
        let timeout = Duration::from_secs(6);
        let cfg = match fetch_config(&bases, pin.clone(), timeout, false).and_then(|(v, base)| read_config(&v, base)) {
            Ok(cfg) => cfg,
            // A certificate for another name (its tailnet's): learn the name from whatever
            // is there, then ask again by that name, which its certificate has to match.
            Err(first) => {
                let https: Vec<String> = bases.iter().filter(|b| b.starts_with("https://")).cloned().collect();
                let named = fetch_config(&https, pin, timeout, true).ok().and_then(|(v, base)| {
                    let name = v["self"]["tls"].as_str().filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'))?;
                    Some(format!("https://{name}{}", port(&base).map(|p| format!(":{p}")).unwrap_or_default()))
                });
                let Some(url) = named else { return Err(first.context("Glass isn't answering there")) };
                let (bases, name) = web_base(&url);
                fetch_config(&bases, Some((name, at)), timeout, false).and_then(|(v, base)| read_config(&v, base))?
            }
        };
        self.at = Some(at);
        self.typed = Some(host).filter(|h| h.parse::<std::net::IpAddr>().is_err());
        self.dns = cfg.base.clone();
        if let Some(who) = &cfg.who {
            self.name = who.name.clone();
            // Without a certificate, the name it announces itself by on its own network.
            if cfg.cert_hash.is_some() && cfg.base.starts_with("http://") {
                self.dns = format!("http://glass-{}.local:{}", who.id, port(&cfg.base).unwrap_or(8080));
            }
        }
        Ok(self)
    }
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
    let mut who = None;
    let conn = match &target.remote {
        // Remote access: through glassdesk.dev, then peer to peer.
        Some((base, key)) => Conn::Rtc(Arc::new(super::rtc::RtcConn::connect(base, key).await?)),
        None => {
            let cfg = host_config(&target.dns, target.at).await.context("Glass isn't answering there")?;
            step(&format!("config: wt port {}, pinned cert {}", cfg.wt_port, cfg.cert_hash.is_some()));
            // A computer opened before, at an address it had then: on another network that
            // address is someone else's. Nothing is sent unless it's still that computer.
            if let Some(saved) = super::saved::by_url(&target.dns) {
                anyhow::ensure!(still(&saved, &cfg), "something else answered at {}'s address", target.name);
            }
            who = cfg.who.clone();
            let builder = ClientConfig::builder().with_bind_default();
            let config = match cfg.cert_hash {
                Some(hash) => builder.with_server_certificate_hashes([Sha256Digest::new(hash)]),
                None => builder.with_native_certs(),
            }
            .keep_alive_interval(Some(Duration::from_secs(2)))
            .max_idle_timeout(Some(Duration::from_secs(10)))?
            .build();
            let mut config = config;
            let host = web_base(&target.dns).1;
            // A computer found on this network: by the name it gave, at the address it was heard from.
            let found = target.at.map(|ip| (cfg.wt_host.clone().unwrap_or_else(|| host.clone()), ip));
            if let Some((name, ip)) = found.or_else(pinned_address) {
                config.set_dns_resolver(Pinned(name, ip));
            }
            let endpoint = Endpoint::client(config)?;
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
    let token = target.token.clone().or_else(|| if target.is_self { own() } else { super::prefs::load(&target.dns).token });
    // A link works once: after that, the pass it was traded for.
    let pass = viewer.pass.lock().unwrap().clone().or_else(|| super::prefs::pass(&target.dns));
    let link = if pass.is_some() { None } else { target.link.clone() };
    let device = format!("Glass app on {}", crate::support::computer_name());
    // `cursor`: this Mac's own pointer is the pointer (no delay, none drawn into the video).
    let hello = json!({ "t": "hello", "token": token, "device": device, "codecs": codecs(), "cursor": cfg!(target_os = "macos"), "acks": true, "ua": if cfg!(windows) { "Glass app (Windows)" } else { "Glass app (macOS)" }, "link": link, "pass": pass,
        // Nothing to show: on the same network, the person at that computer can let this one in.
        "ask": token.is_none() && link.is_none() && pass.is_none() && target.remote.is_none(),
        // From anywhere, with nothing to show: the owner's passkey can say yes to this secret.
        "grant": (token.is_none() && link.is_none() && pass.is_none() && target.remote.is_some()).then(|| viewer.grant.clone()) });
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
                            // A token typed for this computer got in: remembered, as a browser does.
                            if let Some(t) = target.token.as_ref().filter(|_| !target.is_self) {
                                super::prefs::update(&target.dns, |m| m.token = Some(t.clone()));
                            }
                            // Let in, at an address rather than a tailnet name: remembered, to
                            // open again from wherever this computer is next (over a VPN, say).
                            if let (Some(at), Some(w), false) = (target.at, who.take(), target.is_self) {
                                let last = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                                let names = names(target.typed.as_deref(), w.host.as_deref());
                                super::saved::remember(super::saved::Computer { id: w.id, name: w.name, os: w.os, url: target.dns.clone(), key: w.key, addrs: Vec::new(), names, last }, at);
                            }
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
                            // On a fast screen (a ProMotion MacBook, a 144 Hz monitor), more
                            // frames, as the browser viewer asks; otherwise the host sends 60.
                            if let Some(hz) = screen_hz().filter(|&hz| hz > 68.0) {
                                write_json(&mut send, &json!({ "t": "refresh", "hz": hz })).await?;
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
                    "accountPasskey" => {
                        // The computer pinned its owner's passkey: the account's page asks for it.
                        if let (Some((base, _)), Some(host)) = (&target.remote, msg["host"].as_str()) {
                            let url = format!("{}/computers#grant={host}.{}", base.trim_end_matches('/'), crate::actions::grant_hash(&viewer.grant));
                            step(&format!("passkey: {url}"));
                            if !viewer.passkey_asked.swap(true, std::sync::atomic::Ordering::Relaxed) {
                                *viewer.passkey_url.lock().unwrap() = Some(url);
                            }
                            viewer.set_status(Status::Connecting("Confirm with your passkey in your browser to open this computer…".into()));
                        }
                    }
                    "waiting" => viewer.set_status(Status::Connecting(msg["msg"].as_str().unwrap_or("Waiting for someone at the computer to let you in…").into())),
                    "denied" => {
                        // No passkey came: the computer's access token still gets in.
                        if viewer.passkey_asked.load(std::sync::atomic::Ordering::Relaxed) {
                            viewer.needs_token.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        return Ok(End::Refused(msg["msg"].as_str().unwrap_or("Not let in.").into()));
                    }
                    "error" if msg["msg"] == "unauthorized" => {
                        // A pass or token that stopped working (revoked, expired, reset) isn't offered again.
                        super::prefs::update(&target.dns, |m| (m.pass, m.token) = (None, None));
                        viewer.needs_token.store(true, std::sync::atomic::Ordering::Relaxed);
                        return Ok(End::Refused(if token.is_some() {
                            format!("{} didn't take that access token.", target.name)
                        } else if target.remote.is_some() {
                            format!("{} needs its own sign-in the first time.", target.name)
                        } else {
                            format!("{} didn't let this computer in. Connect from the same network and allow it there, sign in to Tailscale as the same person on both, or open a link from it.", target.name)
                        }));
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
                                match super::audio::Player::new(viewer.volume.clone()) {
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
                        #[cfg(any(target_os = "macos", windows))]
                        let sending = on && mic.is_some();
                        #[cfg(not(any(target_os = "macos", windows)))]
                        let sending = false;
                        write_json(&mut send, &json!({ "t": "mic", "on": sending })).await?;
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
    // (`GLASS_VIEWER_DOWNLOADS`: elsewhere, for the self-test.)
    let dir = std::env::var_os("GLASS_VIEWER_DOWNLOADS")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default().join("Downloads"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};

    /// A host's key and what its `/config.json` would say with `hash` signed by `signer`.
    fn config(hash: Option<[u8; 32]>, signer: &EcdsaKeyPair, key: &EcdsaKeyPair) -> Value {
        let rng = ring::rand::SystemRandom::new();
        let sig = hash.map(|h| crate::remote::b64url(signer.sign(&rng, &crate::remote::direct_payload(&h)).unwrap().as_ref()));
        json!({
            "wtPort": 8443,
            "certHash": hash.map(|h| h.to_vec()),
            "self": { "id": "abcdef0123456789", "name": " Studio\u{7} ", "os": "macos", "key": crate::remote::b64url(key.public_key().as_ref()), "tls": null, "sig": sig },
        })
    }

    fn pair() -> EcdsaKeyPair {
        let rng = ring::rand::SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, doc.as_ref(), &rng).unwrap()
    }

    fn saved(key: &EcdsaKeyPair, url: &str) -> super::super::saved::Computer {
        super::super::saved::Computer { id: "abcdef0123456789".into(), name: "Studio".into(), os: "macos".into(), url: url.into(), key: crate::remote::b64url(key.public_key().as_ref()), addrs: Vec::new(), names: Vec::new(), last: 0 }
    }

    #[test]
    fn a_computer_is_who_it_says_only_when_its_key_signed_the_certificate() {
        let (mine, other) = (pair(), pair());
        let hash = [7u8; 32];
        let cfg = read_config(&config(Some(hash), &mine, &mine), "http://glass-abcdef0123456789.local:8080".into()).unwrap();
        let who = cfg.who.clone().unwrap();
        assert_eq!((who.id.as_str(), who.name.as_str(), who.os.as_str()), ("abcdef0123456789", "Studio", "macos"));
        assert!(still(&saved(&mine, "http://glass-abcdef0123456789.local:8080"), &cfg));
        // Another computer at that address, saying it's the one remembered: its own key
        // can't have signed for ours, and ours didn't sign its certificate.
        assert!(read_config(&config(Some(hash), &other, &mine), "http://x:8080".into()).is_err(), "claims our key without its signature");
        let theirs = read_config(&config(Some(hash), &other, &other), "http://x:8080".into()).unwrap();
        assert!(!still(&saved(&mine, "http://x:8080"), &theirs), "a different computer");
        // A signature for an older certificate doesn't cover a new one.
        let mut replayed = config(Some(hash), &mine, &mine);
        replayed["certHash"] = json!([9u8; 32].to_vec());
        assert!(read_config(&replayed, "http://x:8080".into()).is_err());
        // An older host says nothing about itself: it connects as before, and isn't remembered.
        let old = read_config(&json!({ "wtPort": 8443, "certHash": hash.to_vec() }), "http://x:8080".into()).unwrap();
        assert!(old.who.is_none() && !still(&saved(&mine, "http://x:8080"), &old));
    }

    /// Something on this computer answering every request with `body`; its address.
    fn serves(status: &'static str, body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let _ = s.read(&mut [0; 2048]);
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        base
    }

    #[test]
    fn a_browser_opens_where_glass_answers() {
        let limit = Duration::from_secs(5);
        let glass = serves("200 OK", r#"{"wtPort":4433}"#);
        let (missing, other) = (serves("404 Not Found", "{}"), serves("200 OK", r#"{"hello":1}"#));
        let nothing = format!("http://{}", std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap());
        assert_eq!(browser_address(&[glass.clone(), nothing.clone()], None, limit), format!("{glass}/"));
        // Another app has the standard port (`tailscale serve`): Glass is on its web port.
        assert_eq!(browser_address(&[missing.clone(), glass.clone()], None, limit), format!("{glass}/"));
        assert_eq!(browser_address(&[other, glass.clone()], None, limit), format!("{glass}/"));
        // Glass isn't answering: still its web port, not the other app.
        assert_eq!(browser_address(&[missing, nothing.clone()], None, limit), format!("{nothing}/"));
    }

    #[test]
    fn a_certificate_for_its_name_is_proof_enough() {
        let key = pair();
        let cfg = read_config(&config(None, &key, &key), "https://studio.tail1.ts.net:8080".into()).unwrap();
        assert!(still(&saved(&pair(), "https://studio.tail1.ts.net:8080"), &cfg), "the name was checked on the way in");
        // No certificate at all (plain HTTP, nothing to pin): nothing proves it.
        let bare = read_config(&config(None, &key, &key), "http://192.168.1.20:8080".into()).unwrap();
        assert!(!still(&saved(&key, "http://192.168.1.20:8080"), &bare));
    }

    #[test]
    fn a_typed_address_is_tried_where_glass_serves() {
        let bases = |typed: &str| {
            let t = Target::from_url(typed).unwrap();
            let (given, host) = web_base(&t.dns);
            typed_bases(&t.dns, &given, &host)
        };
        assert_eq!(bases("192.168.1.20"), ["https://192.168.1.20:8080", "http://192.168.1.20:8080", "https://192.168.1.20"]);
        assert_eq!(bases("studio.lan:9000"), ["https://studio.lan:9000", "http://studio.lan:9000"]);
        assert_eq!(bases("http://10.0.0.5:8080"), ["http://10.0.0.5:8080"]);
        assert_eq!(bases("[fd00::5]:8080"), ["https://[fd00::5]:8080", "http://[fd00::5]:8080"]);
        assert_eq!(web_base("https://[fd00::5]:8080").1, "fd00::5");
        assert_eq!(web_base("http://glass-abc.local:8080").1, "glass-abc.local");
    }

    #[test]
    fn a_computer_is_looked_up_later_by_its_private_names() {
        assert_eq!(names(Some("Studio.lan"), Some("Nathans-Mac.local")), ["studio.lan", "nathans-mac"]);
        assert_eq!(names(None, Some("studio.home.arpa")), ["studio.home.arpa", "studio"]);
        assert_eq!(names(Some("studio"), Some("studio")), ["studio"]);
        assert_eq!(names(None, Some("studio.attlocal.net")), ["studio.attlocal.net", "studio"], "a router's own ending");
        assert!(names(Some("192.168.1.20"), None).is_empty());
        assert!(names(None, Some("localhost")).is_empty());
    }

    #[test]
    fn only_private_names_are_looked_up_as_addresses() {
        for host in ["192.168.1.20", "10.8.0.6", "fd00::5", "studio", "studio.local", "nas.lan", "mini.home.arpa"] {
            assert!(local_host(host), "{host}");
        }
        for host in ["studio.tail1234.ts.net", "glassdesk.dev", "example.com"] {
            assert!(!local_host(host), "{host}");
        }
        // A link, or a tailnet name, comes back untouched (and nothing is looked up).
        let link = Target::from_url("https://studio.tail1234.ts.net/#l=code").unwrap();
        assert_eq!(link.clone().settle().unwrap().dns, link.dns);
    }
}

/// The fastest active screen's refresh rate, if it can be told. On a Mac the display link's:
/// a ProMotion panel's display mode says 0 Hz, its link 120.
#[cfg(target_os = "macos")]
fn screen_hz() -> Option<f64> {
    use objc2_core_video::CVDisplayLink;
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    let ok = unsafe { objc2_core_graphics::CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if ok != objc2_core_graphics::CGError::Success {
        return None;
    }
    ids[..count as usize]
        .iter()
        .filter_map(|&id| {
            let mut link: *mut CVDisplayLink = std::ptr::null_mut();
            #[allow(deprecated)]
            let made = unsafe { CVDisplayLink::create_with_cg_display(id, std::ptr::NonNull::from(&mut link)) };
            let link = std::ptr::NonNull::new(link)?;
            // Created here: released when this drops.
            let link = unsafe { objc2_core_foundation::CFRetained::from_raw(link) };
            if made != objc2_core_video::kCVReturnSuccess {
                return None;
            }
            #[allow(deprecated)]
            let period = link.nominal_output_video_refresh_period();
            (period.timeValue > 0 && period.timeScale > 0).then(|| period.timeScale as f64 / period.timeValue as f64)
        })
        .reduce(f64::max)
}

/// The primary screen's refresh rate (Windows reports whole hertz).
#[cfg(windows)]
fn screen_hz() -> Option<f64> {
    use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
    let mut mode = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    let ok = unsafe { EnumDisplaySettingsW(windows::core::PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut mode) };
    (ok.as_bool() && mode.dmDisplayFrequency > 1).then_some(mode.dmDisplayFrequency as f64)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn screen_hz() -> Option<f64> {
    None
}

#[cfg(all(test, any(target_os = "macos", windows)))]
mod screen_hz_tests {
    #[test]
    fn a_screen_refreshes_at_a_plausible_rate() {
        // No screen at all (a CI runner) says nothing; one that's there says something sane.
        if let Some(hz) = super::screen_hz() {
            eprintln!("screen refresh: {hz:.1} Hz");
            assert!((20.0..=500.0).contains(&hz), "{hz}");
        }
    }
}
