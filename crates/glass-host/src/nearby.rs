//! Glass computers on the same network, with no Tailscale and nothing to type. A host
//! announces itself over mDNS (`_glass._tcp`, like a printer); the Glass app lists what it
//! hears under Machines.
//!
//! Being found isn't being let in. A device on the same network that has nothing to show
//! (no token, link or pass) may *ask*: the person at the computer sees who, and allowing it
//! gives that device a pass like a link's (`server::control`). `glass config nearby off`
//! stops both the announcing and the asking.
//!
//! A VPN into this network is the same thing
//! from further away: the device arrives from another private address, routed here rather
//! than on the same subnet, and nothing it announces crosses the tunnel. So it may ask too
//! ([`From::Routed`]), and the app remembers the computers it has opened and where they
//! were (`viewer::saved`), checking it's still them by the key in [`proof`].

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

const SERVICE: &str = "_glass._tcp.local.";
/// How often a running host looks at the `nearby` setting.
const SETTING_CHECK: Duration = Duration::from_secs(5);
/// How long a pass given by allowing a nearby device lasts.
pub const GRANT: Duration = crate::access::MAX_GRANT;
/// After a "no", how long before that address may ask again.
const DENIED_FOR: Duration = Duration::from_secs(10 * 60);

/// One mDNS responder for the process (the host announcing and the app listening share it).
fn daemon() -> Option<&'static ServiceDaemon> {
    static DAEMON: OnceLock<Option<ServiceDaemon>> = OnceLock::new();
    DAEMON
        .get_or_init(|| match ServiceDaemon::new() {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::warn!("nearby: mDNS unavailable: {e}");
                None
            }
        })
        .as_ref()
}

/// This computer's id on the network: from its host key, so it stays the same, but not
/// the id glassdesk.dev knows it by (that one isn't for the whole network to hear).
fn own_id() -> Option<&'static str> {
    static ID: OnceLock<Option<String>> = OnceLock::new();
    ID.get_or_init(|| Some(id_for(&crate::remote::HostKey::load_or_create().ok()?.public()))).as_deref()
}

/// The id a computer with this public key announces itself by.
pub fn id_for(key: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, format!("glass-nearby-v1\n{key}").as_bytes());
    digest.as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Who this computer is to a viewer that reaches it directly, for /config.json: its id, the
/// key it proves itself with, and that key's signature over the certificate the viewer is
/// about to pin (`cert_hash`; none when a certificate authority vouches for its name, `tls`).
pub fn proof(cert_hash: Option<&[u8]>, tls: Option<&str>) -> serde_json::Value {
    static KEY: OnceLock<Option<crate::remote::HostKey>> = OnceLock::new();
    // Not a computer to remember: the sign-in screen's host, a quick-support session.
    if crate::signin::active() || crate::support::is_host() {
        return serde_json::Value::Null;
    }
    let Some(key) = KEY.get_or_init(|| crate::remote::HostKey::direct().map_err(|e| tracing::warn!("nearby: no key to prove this computer by: {e:#}")).ok()) else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "id": own_id(),
        "name": crate::support::computer_name(),
        "os": std::env::consts::OS,
        "key": key.public(),
        "tls": tls,
        // To find it again by, when its router gives it another address.
        "host": dns_name(),
        "sig": cert_hash.and_then(|h| key.sign(&crate::remote::direct_payload(h)).ok()),
    })
}

/// What this computer calls itself to its network (`studio`, `studio.lan`): the name its
/// router's DNS usually has for it.
fn dns_name() -> Option<String> {
    #[cfg(unix)]
    let name = {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer outlives the call, and its last byte stays 0.
        let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) } == 0;
        ok.then(|| std::ffi::CStr::from_bytes_until_nul(&buf).ok().map(|s| s.to_string_lossy().into_owned())).flatten()
    };
    #[cfg(not(unix))]
    let name = std::env::var("COMPUTERNAME").ok();
    name.map(|n| n.trim().to_ascii_lowercase()).filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'))
}

/// Whether this computer announces itself and lets nearby devices ask.
pub fn enabled() -> bool {
    !crate::settings::load().nearby_off
}

/// Announces this host for as long as it runs, following the `nearby` setting.
/// `tls_name`: the name on the web port's certificate, when it serves HTTPS.
pub fn announce(web_port: u16, tls_name: Option<String>) {
    // Not a host to find: tests, the sign-in screen's (SYSTEM) host, a quick-support session.
    if crate::testmode::enabled() || crate::signin::active() || crate::support::is_host() {
        return;
    }
    tokio::spawn(async move {
        let mut announced: Option<String> = None;
        loop {
            let on = enabled();
            if on && announced.is_none() {
                let tls = tls_name.clone();
                announced = tokio::task::spawn_blocking(move || register(web_port, tls.as_deref())).await.ok().flatten();
            } else if !on
                && let Some(fullname) = announced.take()
                && let Some(d) = daemon()
            {
                let _ = d.unregister(&fullname);
                tracing::info!("nearby: no longer announcing this computer");
            }
            tokio::time::sleep(SETTING_CHECK).await;
        }
    });
}

fn register(web_port: u16, tls_name: Option<&str>) -> Option<String> {
    let id = own_id()?;
    let name = crate::support::computer_name();
    let mut txt = vec![("id", id.to_string()), ("name", name.clone()), ("os", std::env::consts::OS.to_string()), ("v", env!("CARGO_PKG_VERSION").to_string())];
    if let Some(tls) = tls_name {
        txt.push(("tls", tls.to_string()));
    }
    // The instance is the id (unique, so no renaming on a clash); the name people see is in `name`.
    let info = ServiceInfo::new(SERVICE, id, &host_name(id), (), web_port, &txt[..])
        .map_err(|e| tracing::warn!("nearby: {e}"))
        .ok()?
        .enable_addr_auto();
    let fullname = info.get_fullname().to_string();
    match daemon()?.register(info) {
        Ok(()) => {
            tracing::info!("nearby: announcing \"{name}\" on the local network (glass config nearby off to stop)");
            Some(fullname)
        }
        Err(e) => {
            tracing::warn!("nearby: couldn't announce: {e}");
            None
        }
    }
}

/// `glass-<id>.local.`: this host's mDNS name, also what a viewer calls it by (see [`Found::url`]).
fn host_name(id: &str) -> String {
    format!("glass-{id}.local.")
}

/// A Glass computer heard on this network.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub id: String,
    pub name: String,
    pub os: String,
    /// Its web address, by a name that stays the same when its IP address changes: what it's
    /// remembered by (`viewer::prefs`). Reached at `at`.
    pub url: String,
    pub at: IpAddr,
}

static FOUND: Mutex<Option<HashMap<String, Found>>> = Mutex::new(None);

/// Starts listening for Glass computers (once); [`found`] has what's been heard.
pub fn listen() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let Some(events) = daemon().and_then(|d| d.browse(SERVICE).map_err(|e| tracing::warn!("nearby: {e}")).ok()) else { return };
        *FOUND.lock().unwrap() = Some(HashMap::new());
        std::thread::spawn(move || {
            while let Ok(event) = events.recv() {
                match event {
                    ServiceEvent::ServiceResolved(s) => {
                        let txt = |k: &str| s.txt_properties.get_property_val_str(k).unwrap_or_default().to_string();
                        let addrs: Vec<IpAddr> = s.addresses.iter().map(|a| a.to_ip_addr()).collect();
                        let found = parse(&txt("id"), &txt("name"), &txt("os"), &txt("tls"), s.port, &addrs, &local_nets());
                        let mut all = FOUND.lock().unwrap();
                        let all = all.get_or_insert_default();
                        match found {
                            Some(f) => _ = all.insert(s.fullname.clone(), f),
                            None => _ = all.remove(&s.fullname),
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        if let Some(all) = FOUND.lock().unwrap().as_mut() {
                            all.remove(&fullname);
                        }
                    }
                    _ => {}
                }
            }
        });
    });
}

/// The other Glass computers on this network right now, by name.
pub fn found() -> Vec<Found> {
    let own = own_id();
    let mut out: Vec<Found> = FOUND.lock().unwrap().iter().flat_map(|all| all.values()).filter(|f| Some(f.id.as_str()) != own).cloned().collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// What an announcement says, as somewhere to connect: `None` when it's malformed or none
/// of its addresses is on a network this computer is on.
fn parse(id: &str, name: &str, os: &str, tls: &str, port: u16, addrs: &[IpAddr], nets: &[Net]) -> Option<Found> {
    // These go into a URL and onto the screen.
    let id_ok = (8..=32).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let tls_ok = tls.len() <= 253 && tls.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if !id_ok || !tls_ok || port == 0 {
        return None;
    }
    // IPv4 first: an IPv6 link-local address needs its interface to be dialled.
    let at = addrs.iter().find(|a| a.is_ipv4() && on_link_in(**a, nets)).or_else(|| addrs.iter().find(|a| on_link_in(**a, nets) && !link_local(**a)))?;
    let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    let name = name.trim();
    Some(Found {
        id: id.to_string(),
        name: if name.is_empty() { "Glass computer".into() } else { name.to_string() },
        os: os.chars().filter(char::is_ascii_alphanumeric).take(16).collect(),
        // HTTPS by the certificate's name, or plain HTTP (the picture's own connection is
        // then pinned to the certificate hash /config.json gives).
        url: if tls.is_empty() { format!("http://{}:{port}", host_name(id).trim_end_matches('.')) } else { format!("https://{tls}:{port}") },
        at: *at,
    })
}

fn link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 == 0xfe80,
    }
}

/// A network this computer is directly on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Net {
    ip: IpAddr,
    prefix: u8,
}

/// The local networks, read at most every few seconds.
fn local_nets() -> Vec<Net> {
    static CACHE: Mutex<Option<(Instant, Vec<Net>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    if let Some((at, nets)) = cache.as_ref()
        && at.elapsed() < Duration::from_secs(5)
    {
        return nets.clone();
    }
    let nets: Vec<Net> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        // Not a VPN's point-to-point link: that isn't "the same network".
        .filter(|i| !i.is_loopback() && !i.is_p2p())
        .map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) => Net { ip: IpAddr::V4(a.ip), prefix: a.prefixlen },
            if_addrs::IfAddr::V6(a) => Net { ip: IpAddr::V6(a.ip), prefix: a.prefixlen },
        })
        .collect();
    *cache = Some((Instant::now(), nets.clone()));
    nets
}

/// This computer's address on its local network: what another computer there, or on a VPN
/// into it, types to reach it. The one traffic leaves by, when that's a private network;
/// else any private one it has.
pub fn own_address() -> Option<IpAddr> {
    let lan = |ip: &IpAddr| ip.is_ipv4() && private(*ip) && !link_local(*ip) && !crate::tailscale::is_tailscale_ip(*ip);
    // No packet is sent: connecting a UDP socket only picks the route.
    let routed = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|s| s.connect("192.0.2.1:9").and_then(|()| s.local_addr())).ok().map(|a| a.ip());
    routed.filter(lan).or_else(|| local_nets().into_iter().map(|n| n.ip).find(lan))
}

fn on_link_in(peer: IpAddr, nets: &[Net]) -> bool {
    if peer.is_loopback() || peer.is_unspecified() || crate::tailscale::is_tailscale_ip(peer) || !private(peer) {
        return false;
    }
    nets.iter().any(|n| same_net(peer, n.ip, n.prefix))
}

/// Home and office address ranges: an ISP that hands every customer a public address on
/// one big subnet doesn't make its other customers "nearby".
pub(crate) fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        // Unique local (fc00::/7) and link-local.
        IpAddr::V6(v6) => v6.segments()[0] & 0xfe00 == 0xfc00 || link_local(ip),
    }
}

fn same_net(a: IpAddr, b: IpAddr, prefix: u8) -> bool {
    fn bits(ip: IpAddr) -> Option<(u128, u8)> {
        Some(match ip {
            IpAddr::V4(v4) => (u32::from(v4) as u128, 32),
            IpAddr::V6(v6) => (u128::from(v6), 128),
        })
    }
    let (Some((a, wa)), Some((b, wb))) = (bits(a), bits(b)) else { return false };
    // A /0 (or a nonsense prefix) would make everything local.
    if wa != wb || prefix == 0 || prefix > wa {
        return false;
    }
    let shift = (wa - prefix) as u32;
    a >> shift == b >> shift
}

// ---- Asking to be let in ----

static DENIED: Mutex<Vec<(IpAddr, Instant)>> = Mutex::new(Vec::new());
static ASKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Holds the one "let them in?" a host shows for nearby devices at a time, so a network
/// full of strangers can't stack prompts up.
pub struct Asking(());

impl Drop for Asking {
    fn drop(&mut self) {
        ASKING.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Where a device that may ask is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum From {
    /// On a network this computer is on.
    Link,
    /// On another private network, routed here: a VPN into this one, or another of the
    /// router's networks.
    Routed,
}

impl From {
    /// For the question the person here is asked, and the log.
    pub fn describe(self) -> &'static str {
        match self {
            From::Link => "on this network",
            From::Routed => "from another private network, like your VPN",
        }
    }
}

/// A private address that isn't on any of `nets`: it can only have been routed here, by a
/// VPN or between a router's networks. (The internet can't send from one, and the tailnet
/// has its own sign-in.)
fn routed_in(peer: IpAddr, nets: &[Net]) -> bool {
    !peer.is_loopback() && !link_local(peer) && !crate::tailscale::is_tailscale_ip(peer) && private(peer) && !on_link_in(peer, nets)
}

fn from_in(peer: IpAddr, nets: &[Net]) -> Option<From> {
    if on_link_in(peer, nets) {
        Some(From::Link)
    } else {
        routed_in(peer, nets).then_some(From::Routed)
    }
}

/// Whether a device at `peer` with nothing to show may ask the person here to let it in,
/// and from where: `Err` says why not, for the log.
pub fn may_ask(peer: IpAddr) -> Result<(Asking, From), &'static str> {
    if !enabled() {
        return Err("nearby is off");
    }
    let Some(from) = from_in(peer.to_canonical(), &local_nets()) else {
        return Err("not on this network or another private one");
    };
    {
        let mut denied = DENIED.lock().unwrap();
        denied.retain(|(_, at)| at.elapsed() < DENIED_FOR);
        if denied.iter().any(|(ip, _)| *ip == peer) {
            return Err("told no a moment ago");
        }
    }
    if ASKING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return Err("another device is asking");
    }
    Ok((Asking(()), from))
}

/// The person said no: that address doesn't get to ask again for a while.
pub fn denied(peer: IpAddr) {
    DENIED.lock().unwrap().push((peer, Instant::now()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn nets() -> Vec<Net> {
        vec![Net { ip: ip("192.168.1.20"), prefix: 24 }, Net { ip: ip("fd00:1::5"), prefix: 64 }, Net { ip: ip("203.0.113.9"), prefix: 20 }, Net { ip: ip("100.101.102.103"), prefix: 10 }]
    }

    #[test]
    fn only_the_same_private_subnet_is_nearby() {
        let n = nets();
        assert!(on_link_in(ip("192.168.1.77"), &n));
        assert!(on_link_in(ip("fd00:1::99"), &n));
        assert!(!on_link_in(ip("192.168.2.77"), &n), "another subnet");
        assert!(!on_link_in(ip("10.0.0.5"), &n), "private, but routed from elsewhere");
        assert!(!on_link_in(ip("203.0.113.50"), &n), "a public address, even on our subnet");
        assert!(!on_link_in(ip("100.101.1.1"), &n), "the tailnet has its own sign-in");
        assert!(!on_link_in(ip("127.0.0.1"), &n));
        assert!(!on_link_in(ip("192.168.1.77"), &[]));
        assert!(!on_link_in(ip("192.168.1.77"), &[Net { ip: ip("192.168.1.20"), prefix: 0 }]), "a /0 isn't a network");
    }

    #[test]
    fn a_vpn_into_this_network_may_ask_and_the_internet_may_not() {
        let n = nets();
        assert_eq!(from_in(ip("192.168.1.77"), &n), Some(From::Link));
        // A VPN usually hands its clients a subnet of their own, routed to this one.
        assert_eq!(from_in(ip("192.168.3.2"), &n), Some(From::Routed));
        assert_eq!(from_in(ip("10.8.0.6"), &n), Some(From::Routed));
        assert_eq!(from_in(ip("fd12:3456::7"), &n), Some(From::Routed));
        assert_eq!(from_in(ip("203.0.113.50"), &n), None, "a public address");
        assert_eq!(from_in(ip("8.8.8.8"), &n), None);
        assert_eq!(from_in(ip("100.101.1.1"), &n), None, "the tailnet has its own sign-in");
        assert_eq!(from_in(ip("169.254.9.9"), &n), None, "link-local, but on no link of ours");
        assert_eq!(from_in(ip("127.0.0.1"), &n), None);
    }

    #[test]
    fn an_announcement_becomes_an_address() {
        let n = nets();
        let addrs = [ip("100.90.1.1"), ip("fe80::1"), ip("192.168.1.31")];
        let f = parse("abcdef0123456789", " Studio\u{7} ", "macos", "", 8080, &addrs, &n).unwrap();
        assert_eq!((f.name.as_str(), f.url.as_str(), f.at), ("Studio", "http://glass-abcdef0123456789.local:8080", ip("192.168.1.31")));
        let f = parse("abcdef0123456789", "", "linux", "box.tail1234.ts.net", 8443, &addrs, &n).unwrap();
        assert_eq!((f.name.as_str(), f.url.as_str()), ("Glass computer", "https://box.tail1234.ts.net:8443"));
        // Nothing of it is on a network we're on; or it isn't ours to trust in a URL.
        assert!(parse("abcdef0123456789", "x", "", "", 8080, &[ip("10.9.9.9")], &n).is_none());
        assert!(parse("abc/../def012345", "x", "", "", 8080, &addrs, &n).is_none());
        assert!(parse("abcdef0123456789", "x", "", "evil.example/#t=", 8080, &addrs, &n).is_none());
        assert!(parse("abcdef0123456789", "x", "", "", 0, &addrs, &n).is_none());
    }
}
