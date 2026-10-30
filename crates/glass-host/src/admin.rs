//! The console at `/admin/`: one page for the computer's owner to see who's connected,
//! answer requests, hand out and revoke links, read the history, change settings and keep
//! an address book of their machines in groups. Every `/admin/api/*` route is owner-only
//! (`forward::is_owner`: an owner tailnet device, the token as a bearer header, or the
//! cookie `/admin/auth?t=…` sets).

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, Query};
use axum::http::{HeaderMap, Response, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub fn routes() -> axum::Router {
    axum::Router::new()
        .route("/admin/auth", get(auth))
        .route("/admin", get(|| async { redirect("/admin/", None) }))
        .route("/admin/api/state", get(state))
        .route("/admin/api/machines", get(machines))
        .route("/admin/api/groups", get(get_groups).put(put_groups))
        .route("/admin/api/settings", axum::routing::put(put_setting))
        .route("/admin/api/link", post(link))
        .route("/admin/api/revoke", post(revoke))
        .route("/admin/api/disconnect", post(disconnect))
        .route("/admin/api/decide", post(decide))
        .route("/admin/api/wake", post(wake))
}

type Peer = ConnectInfo<SocketAddr>;

async fn owner(peer: &Peer, headers: &HeaderMap) -> Result<(), Response<Body>> {
    if crate::forward::is_owner(peer.0.ip(), headers).await {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, Json(json!({ "error": "owner only" }))).into_response())
    }
}

fn bad(msg: impl std::fmt::Display) -> Response<Body> {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg.to_string() }))).into_response()
}

fn redirect(to: &str, cookie: Option<String>) -> Response<Body> {
    let mut r = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, to);
    if let Some(c) = cookie {
        r = r.header(header::SET_COOKIE, c);
    }
    r.body(Body::empty()).unwrap()
}

#[derive(Deserialize)]
struct AuthQuery {
    t: String,
}

/// `/admin/auth?t=<token>`: proves the token once, then the cookie does (scoped to /admin).
async fn auth(Query(q): Query<AuthQuery>) -> Response<Body> {
    let token = crate::server::shared()
        .and_then(|s| crate::control::load_or_create_token(&s.token_file).ok())
        .filter(|t| !t.is_empty());
    let Some(token) = token else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !crate::control::token_matches(&q.t, &token) {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        return (StatusCode::FORBIDDEN, "wrong token").into_response();
    }
    let cookie = format!(
        "{}={}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age=2592000",
        crate::forward::COOKIE,
        crate::forward::cookie_value(&token)
    );
    redirect("/admin/", Some(cookie))
}

/// Everything the console shows, in one request.
async fn state(peer: Peer, headers: HeaderMap) -> Response<Body> {
    if let Err(r) = owner(&peer, &headers).await {
        return r;
    }
    let settings = crate::settings::load();
    let settings: Vec<Value> = crate::settings::KEYS
        .iter()
        .map(|(k, help)| json!({ "key": k, "value": settings.get(k).unwrap_or_default(), "help": help }))
        .collect();
    let viewers: Vec<Value> = crate::viewers::subscribe()
        .borrow()
        .iter()
        .map(|v| json!({ "id": v.id, "describe": v.describe(), "device": v.device }))
        .collect();
    let access = crate::server::shared()
        .map(|s| crate::access::file_for(&s.token_file))
        .unwrap_or_else(|| crate::access::file_for(&crate::default_token_file()));
    let log: Vec<Value> = crate::connlog::recent(200)
        .into_iter()
        .rev()
        .map(|e| json!({ "entry": e, "describe": crate::connlog::describe(&e) }))
        .collect();
    Json(json!({
        "viewers": viewers,
        "pending": crate::approval::pending(),
        "passes": crate::access::list(&access),
        "log": log,
        "settings": settings,
        "groups": load_groups().groups,
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

// ---- Address book ----

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Groups {
    #[serde(default)]
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Group {
    pub name: String,
    /// Machines by MagicDNS name.
    #[serde(default)]
    pub machines: Vec<String>,
}

fn groups_file() -> PathBuf {
    crate::config_dir().join("groups.json")
}

pub fn load_groups() -> Groups {
    std::fs::read(groups_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Cleans up what the console sent: names trimmed, unique and non-empty, a machine in at
/// most one group (the first), sizes capped.
pub fn normalize(groups: Groups) -> Result<Groups, String> {
    let mut seen_names = std::collections::HashSet::new();
    let mut seen_machines = std::collections::HashSet::new();
    let mut out = Vec::new();
    for g in groups.groups.into_iter().take(100) {
        let name: String = g.name.trim().chars().filter(|c| !c.is_control()).take(40).collect();
        if name.is_empty() {
            return Err("every group needs a name".into());
        }
        if !seen_names.insert(name.to_lowercase()) {
            return Err(format!("there are two groups called {name}"));
        }
        let machines = g
            .machines
            .into_iter()
            .map(|m| m.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|m| !m.is_empty() && seen_machines.insert(m.clone()))
            .take(500)
            .collect();
        out.push(Group { name, machines });
    }
    Ok(Groups { groups: out })
}

pub fn save_groups(groups: &Groups) -> anyhow::Result<()> {
    let p = groups_file();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(groups)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

async fn get_groups(peer: Peer, headers: HeaderMap) -> Response<Body> {
    if let Err(r) = owner(&peer, &headers).await {
        return r;
    }
    Json(load_groups()).into_response()
}

async fn put_groups(peer: Peer, headers: HeaderMap, Json(groups): Json<Groups>) -> Response<Body> {
    if let Err(r) = owner(&peer, &headers).await {
        return r;
    }
    match normalize(groups) {
        Ok(g) => match save_groups(&g) {
            Ok(()) => Json(g).into_response(),
            Err(e) => bad(format!("{e:#}")),
        },
        Err(e) => bad(e),
    }
}

/// Tailnet machines with their group and wake addresses (test mode: two fake ones).
async fn machines(peer: Peer, headers: HeaderMap) -> Response<Body> {
    if let Err(r) = owner(&peer, &headers).await {
        return r;
    }
    let list: Vec<crate::tailscale::Machine> = if crate::testmode::enabled() {
        vec![
            crate::tailscale::Machine {
                name: "studio-mac".into(),
                dns: "studio-mac.example.ts.net".into(),
                os: "macOS".into(),
                online: true,
                is_self: true,
            },
            crate::tailscale::Machine {
                name: "nas".into(),
                dns: "nas.example.ts.net".into(),
                os: "linux".into(),
                online: false,
                is_self: false,
            },
        ]
    } else {
        crate::tailscale::machines().await.unwrap_or_default()
    };
    let known = crate::wol::known();
    let groups = load_groups();
    let out: Vec<Value> = list
        .into_iter()
        .map(|m| {
            let group = groups
                .groups
                .iter()
                .find(|g| g.machines.iter().any(|d| d == &m.dns.to_ascii_lowercase()))
                .map(|g| g.name.clone());
            let mut macs = known.get(&m.dns).cloned().unwrap_or_default();
            if crate::testmode::enabled() && m.name == "nas" {
                macs = vec!["02:00:00:00:00:02".into()];
            }
            json!({ "name": m.name, "dns": m.dns, "os": m.os, "online": m.online, "self": m.is_self, "group": group, "macs": macs })
        })
        .collect();
    Json(json!({ "machines": out, "groups": groups.groups })).into_response()
}

#[derive(Deserialize)]
struct WakeReq {
    target: String,
}

async fn wake(peer: Peer, headers: HeaderMap, Json(r): Json<WakeReq>) -> Response<Body> {
    if let Err(resp) = owner(&peer, &headers).await {
        return resp;
    }
    let target = if crate::testmode::enabled() && r.target == "nas.example.ts.net" {
        "02:00:00:00:00:02".to_string()
    } else {
        r.target
    };
    match crate::wol::wake_target(&target) {
        Ok(macs) => Json(json!({ "sent": macs })).into_response(),
        Err(e) => bad(format!("{e:#}")),
    }
}

// ---- People and links ----

#[derive(Deserialize)]
struct SettingReq {
    key: String,
    value: String,
}

async fn put_setting(peer: Peer, headers: HeaderMap, Json(r): Json<SettingReq>) -> Response<Body> {
    if let Err(resp) = owner(&peer, &headers).await {
        return resp;
    }
    let mut s = crate::settings::load();
    if let Err(e) = s.set(&r.key, &r.value) {
        return bad(format!("{e:#}"));
    }
    if let Err(e) = crate::settings::save(&s) {
        return bad(format!("{e:#}"));
    }
    Json(json!({ "key": r.key, "value": s.get(&r.key).unwrap_or_default() })).into_response()
}

#[derive(Deserialize)]
struct LinkReq {
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    anywhere: bool,
    #[serde(default)]
    scope: crate::access::Scope,
}

async fn link(peer: Peer, headers: HeaderMap, Json(r): Json<LinkReq>) -> Response<Body> {
    if let Err(resp) = owner(&peer, &headers).await {
        return resp;
    }
    let grant = match crate::access::parse_duration(r.duration.as_deref().unwrap_or("24h")) {
        Ok(d) => d,
        Err(e) => return bad(e),
    };
    let Some(base) = crate::status::current().and_then(|s| s.url) else {
        return bad("the host isn't ready yet");
    };
    match crate::make_link(&base, grant, r.anywhere, r.scope) {
        Ok((url, link)) => Json(json!({
            "url": url,
            "qr": qr_svg(&url),
            "scope": link.scope,
            "summary": format!(
                "Works on one device, within {}. Gives that device {} of access ({}).",
                crate::access::describe(crate::access::LINK_TTL),
                crate::access::describe(link.grant),
                link.scope.describe()
            ),
        }))
        .into_response(),
        Err(e) => bad(format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct IdReq {
    id: String,
}

async fn revoke(peer: Peer, headers: HeaderMap, Json(r): Json<IdReq>) -> Response<Body> {
    if let Err(resp) = owner(&peer, &headers).await {
        return resp;
    }
    let access = crate::server::shared()
        .map(|s| crate::access::file_for(&s.token_file))
        .unwrap_or_else(|| crate::access::file_for(&crate::default_token_file()));
    match crate::access::revoke(&access, &r.id) {
        Ok(true) => Json(json!({ "revoked": 1 })).into_response(),
        Ok(false) => bad("no single device matches that id"),
        Err(e) => bad(format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct DisconnectReq {
    id: Option<u64>,
}

async fn disconnect(peer: Peer, headers: HeaderMap, Json(r): Json<DisconnectReq>) -> Response<Body> {
    if let Err(resp) = owner(&peer, &headers).await {
        return resp;
    }
    let n = match r.id {
        Some(id) => crate::viewers::disconnect(id) as usize,
        None => {
            let n = crate::viewers::subscribe().borrow().len();
            crate::viewers::disconnect_all();
            n
        }
    };
    Json(json!({ "disconnected": n })).into_response()
}

#[derive(Deserialize)]
struct DecideReq {
    id: u64,
    allow: bool,
}

async fn decide(peer: Peer, headers: HeaderMap, Json(r): Json<DecideReq>) -> Response<Body> {
    if let Err(resp) = owner(&peer, &headers).await {
        return resp;
    }
    Json(json!({ "ok": crate::approval::decide(r.id, r.allow) })).into_response()
}

/// A QR code as a small SVG (one path, a module per unit), for the console.
pub fn qr_svg(text: &str) -> String {
    let Ok(code) = qrcode::QrCode::new(text) else {
        return String::new();
    };
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 2;
    let size = width + quiet * 2;
    let mut path = String::new();
    for (i, c) in colors.iter().enumerate() {
        if *c == qrcode::Color::Dark {
            let (x, y) = (i % width + quiet, i / width + quiet);
            path.push_str(&format!("M{x} {y}h1v1h-1z"));
        }
    }
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" shape-rendering="crispEdges"><rect width="{size}" height="{size}" fill="#fff"/><path d="{path}" fill="#000"/></svg>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_are_cleaned_up() {
        let g = normalize(Groups {
            groups: vec![
                Group { name: " Home ".into(), machines: vec!["Mac.ts.net.".into(), "mac.ts.net".into()] },
                Group { name: "Work".into(), machines: vec!["mac.ts.net".into(), "pc.ts.net".into()] },
            ],
        })
        .unwrap();
        assert_eq!(g.groups[0], Group { name: "Home".into(), machines: vec!["mac.ts.net".into()] });
        assert_eq!(g.groups[1].machines, vec!["pc.ts.net"], "a machine sits in one group");
        assert!(normalize(Groups { groups: vec![Group { name: "".into(), machines: vec![] }] }).is_err());
        assert!(
            normalize(Groups {
                groups: vec![
                    Group { name: "a".into(), machines: vec![] },
                    Group { name: "A".into(), machines: vec![] }
                ]
            })
            .is_err()
        );
    }

    #[test]
    fn groups_round_trip_on_disk() {
        let dir = std::env::temp_dir().join(format!("glass-groups-{}", std::process::id()));
        let path = dir.join("groups.json");
        std::fs::create_dir_all(&dir).unwrap();
        let g = Groups { groups: vec![Group { name: "Home".into(), machines: vec!["mac.ts.net".into()] }] };
        std::fs::write(&path, serde_json::to_vec(&g).unwrap()).unwrap();
        let back: Groups = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back, g);
        let old: Groups = serde_json::from_str("{}").unwrap();
        assert!(old.groups.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn qr_is_an_svg() {
        let svg = qr_svg("http://localhost/#l=abc");
        assert!(svg.starts_with("<svg") && svg.contains("<path d=\"M"));
    }
}
