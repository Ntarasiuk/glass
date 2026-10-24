//! Agents driving this computer: screenshots, pointer and keyboard, waiting for the screen,
//! and asking the person for help or a yes/no. Owner-only HTTP at `/agent/v1/*` (an owner
//! tailnet device, or the token as a bearer header); `glass mcp` wraps it for MCP clients.
//!
//! Everything an agent does is shown to the people viewing: an "agent working" banner with
//! Pause and Stop, a marker where it points, and its questions as cards to answer.
//! Paused, actions wait; stopped, they're refused until the owner allows it again.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use base64::Engine;
use glass_proto::InputEvent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tracing::info;

use crate::control::ServerMsg;
use crate::media::DisplayInfo;

/// Screenshot size when the agent doesn't ask for one (what computer-use models expect).
pub const DEFAULT_MAX: [u32; 2] = [1280, 800];
/// The largest screenshot an agent can ask for.
const LARGEST: u32 = 2560;
/// How long a paused agent's action waits before giving up.
const PAUSE_WAIT: Duration = Duration::from_secs(600);
/// How long a question waits for the person.
const ASK_WAIT: Duration = Duration::from_secs(1800);

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Default)]
pub struct Status {
    pub paused: bool,
    pub stopped: bool,
}

struct Ask {
    kind: &'static str,
    text: String,
    reply: oneshot::Sender<Answer>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Answer {
    pub ok: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Default)]
struct State {
    status: Status,
    /// Size of the last screenshot of each display: actions' x, y are in its pixels.
    sizes: HashMap<u32, (u32, u32)>,
    asks: HashMap<u64, Ask>,
    next_ask: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().unwrap().get_or_insert_with(State::default))
}

fn broadcast(msg: ServerMsg) {
    if let Some(shared) = crate::server::shared() {
        let _ = shared.events.send(msg);
    }
}

/// What a viewer who just joined needs: the status, and questions waiting for an answer.
pub fn snapshot() -> Vec<ServerMsg> {
    with(|s| {
        let mut out = Vec::new();
        if s.status != Status::default() {
            out.push(ServerMsg::AgentStatus { status: s.status });
        }
        let mut asks: Vec<_> = s.asks.iter().collect();
        asks.sort_by_key(|(id, _)| **id);
        for (id, a) in asks {
            out.push(ServerMsg::AgentAsk { id: *id, kind: a.kind, text: a.text.clone() });
        }
        out
    })
}

/// The owner, from a viewer: `pause`, `resume`, `stop` or `allow` (after a stop).
pub fn control(op: &str) {
    let status = with(|s| {
        match op {
            "pause" => s.status.paused = true,
            "resume" => s.status.paused = false,
            "stop" => {
                s.status = Status { paused: false, stopped: true };
                // Nobody's waiting on answers from an agent that's been stopped.
                for (id, ask) in s.asks.drain() {
                    let _ = ask.reply.send(Answer { ok: false, note: Some("stopped by the user".into()) });
                    broadcast(ServerMsg::AgentAnswered { id });
                }
            }
            "allow" => s.status = Status::default(),
            _ => {}
        }
        s.status
    });
    info!("agent: {op} (by the owner)");
    crate::testmode::record("agent", json!({ "op": op }));
    broadcast(ServerMsg::AgentStatus { status });
}

pub fn status() -> Status {
    with(|s| s.status)
}

/// Questions waiting for the owner, oldest first.
pub fn asks() -> Vec<Value> {
    with(|s| {
        let mut asks: Vec<_> = s.asks.iter().map(|(id, a)| json!({ "id": id, "kind": a.kind, "text": a.text })).collect();
        asks.sort_by_key(|a| a["id"].as_u64());
        asks
    })
}

/// The owner answered a question.
pub fn answer(id: u64, answer: Answer) {
    if let Some(ask) = with(|s| s.asks.remove(&id)) {
        info!("agent: question {id} answered ({})", if answer.ok { "yes/done" } else { "no" });
        let _ = ask.reply.send(answer);
        broadcast(ServerMsg::AgentAnswered { id });
    }
}

#[derive(Debug)]
pub struct Refused(pub String);

/// Waits while paused; refuses when stopped.
async fn may_act() -> Result<(), Refused> {
    let start = Instant::now();
    loop {
        let status = with(|s| s.status);
        if status.stopped {
            return Err(Refused(
                "The user stopped the agent. Don't continue; tell them what you were doing.".into(),
            ));
        }
        if !status.paused {
            return Ok(());
        }
        if start.elapsed() > PAUSE_WAIT {
            return Err(Refused("The user paused the agent and hasn't resumed it.".into()));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn shared() -> Result<std::sync::Arc<crate::server::Shared>, Refused> {
    crate::server::shared().ok_or_else(|| Refused("Glass isn't ready".into()))
}

async fn displays() -> Result<Vec<DisplayInfo>, Refused> {
    let shared = shared()?;
    tokio::task::spawn_blocking(move || shared.video.displays())
        .await
        .map_err(|e| Refused(e.to_string()))?
        .map_err(|e| Refused(format!("{e:#}")))
}

/// The display meant: `id`, or the main one.
async fn display(id: Option<u32>) -> Result<DisplayInfo, Refused> {
    let all = displays().await?;
    match id {
        Some(id) => all.into_iter().find(|d| d.id == id).ok_or_else(|| Refused(format!("no display {id}"))),
        None => all.iter().find(|d| d.main).or(all.first()).cloned().ok_or_else(|| Refused("no displays".into())),
    }
}

/// `x`, `y` in the pixels of the last screenshot of `d` (or of a default-sized one), as 0..1.
fn normalize(d: &DisplayInfo, x: f64, y: f64) -> (f32, f32) {
    let (w, h) = with(|s| s.sizes.get(&d.id).copied()).unwrap_or_else(|| {
        let (w, h) = crate::thumbs::fit(d.width as usize, d.height as usize, DEFAULT_MAX);
        (w as u32, h as u32)
    });
    ((x / w.max(1) as f64).clamp(0.0, 1.0) as f32, (y / h.max(1) as f64).clamp(0.0, 1.0) as f32)
}

fn announce(kind: &str, detail: String, at: Option<(u32, f32, f32)>) {
    info!("agent: {kind} {detail}");
    broadcast(ServerMsg::AgentAction {
        kind: kind.into(),
        detail,
        display: at.map(|a| a.0),
        x: at.map(|a| a.1),
        y: at.map(|a| a.2),
    });
}

fn inject(events: impl IntoIterator<Item = InputEvent>) -> Result<(), Refused> {
    let shared = shared()?;
    let mut sink = shared.injector.lock().unwrap();
    for ev in events {
        sink.handle(ev);
    }
    Ok(())
}

fn move_to(d: &DisplayInfo, x: f32, y: f32) -> Result<(), Refused> {
    shared()?.injector.lock().unwrap().move_on_display(d.id, x, y);
    Ok(())
}

pub async fn screenshot(id: Option<u32>, max: Option<[u32; 2]>) -> Result<Value, Refused> {
    may_act().await?;
    let d = display(id).await?;
    let max = max.unwrap_or(DEFAULT_MAX).map(|v| v.clamp(16, LARGEST));
    let shared = shared()?;
    let id = d.id;
    // A source may need a moment for its first picture (Linux keeps them only on demand).
    let frame = tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        loop {
            if let Some(f) = shared.video.picture(id, max) {
                return Some(f);
            }
            if start.elapsed() > Duration::from_secs(2) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })
    .await
    .map_err(|e| Refused(e.to_string()))?
    .ok_or_else(|| Refused("couldn't take a screenshot of that display".into()))?;
    let png = crate::lossless::encode_png(&frame, [0, 0, frame.width, frame.height])
        .map_err(|e| Refused(format!("{e:#}")))?;
    with(|s| s.sizes.insert(id, (frame.width as u32, frame.height as u32)));
    announce("screenshot", format!("of display {id}"), None);
    Ok(json!({
        "display": id,
        "width": frame.width,
        "height": frame.height,
        "png": base64::engine::general_purpose::STANDARD.encode(png),
    }))
}

fn button_code(name: Option<&str>) -> Result<u8, Refused> {
    Ok(match name.unwrap_or("left") {
        "left" => 0,
        "middle" => 1,
        "right" => 2,
        other => return Err(Refused(format!("unknown button {other:?}"))),
    })
}

pub async fn click(id: Option<u32>, x: f64, y: f64, button: Option<&str>, count: u8) -> Result<Value, Refused> {
    may_act().await?;
    let d = display(id).await?;
    let b = button_code(button)?;
    let (nx, ny) = normalize(&d, x, y);
    move_to(&d, nx, ny)?;
    for _ in 0..count.clamp(1, 3) {
        inject([InputEvent::MouseDown { button: b }, InputEvent::MouseUp { button: b }])?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let what = match (count, button.unwrap_or("left")) {
        (2, _) => "double-click",
        (3, _) => "triple-click",
        (_, "right") => "right-click",
        _ => "click",
    };
    announce(what, format!("at {x:.0}, {y:.0}"), Some((d.id, nx, ny)));
    Ok(json!({ "ok": true }))
}

pub async fn pointer(id: Option<u32>, x: f64, y: f64) -> Result<Value, Refused> {
    may_act().await?;
    let d = display(id).await?;
    let (nx, ny) = normalize(&d, x, y);
    move_to(&d, nx, ny)?;
    announce("move", format!("to {x:.0}, {y:.0}"), Some((d.id, nx, ny)));
    Ok(json!({ "ok": true }))
}

pub async fn drag(id: Option<u32>, from: [f64; 2], to: [f64; 2]) -> Result<Value, Refused> {
    may_act().await?;
    let d = display(id).await?;
    let (fx, fy) = normalize(&d, from[0], from[1]);
    let (tx, ty) = normalize(&d, to[0], to[1]);
    move_to(&d, fx, fy)?;
    inject([InputEvent::MouseDown { button: 0 }])?;
    // In steps, so apps see a drag rather than a jump.
    const STEPS: u32 = 12;
    for i in 1..=STEPS {
        let t = i as f32 / STEPS as f32;
        tokio::time::sleep(Duration::from_millis(16)).await;
        move_to(&d, fx + (tx - fx) * t, fy + (ty - fy) * t)?;
    }
    inject([InputEvent::MouseUp { button: 0 }])?;
    announce("drag", format!("from {:.0}, {:.0} to {:.0}, {:.0}", from[0], from[1], to[0], to[1]), Some((d.id, tx, ty)));
    Ok(json!({ "ok": true }))
}

pub async fn scroll(id: Option<u32>, x: f64, y: f64, dx: f64, dy: f64) -> Result<Value, Refused> {
    may_act().await?;
    let d = display(id).await?;
    let (nx, ny) = normalize(&d, x, y);
    move_to(&d, nx, ny)?;
    inject([InputEvent::Scroll { dx: dx as f32, dy: dy as f32 }])?;
    announce("scroll", format!("by {dx:.0}, {dy:.0}"), Some((d.id, nx, ny)));
    Ok(json!({ "ok": true }))
}

pub async fn type_text(text: &str) -> Result<Value, Refused> {
    may_act().await?;
    if text.chars().count() > 10_000 {
        return Err(Refused("too much text at once (10,000 characters at most)".into()));
    }
    let typed = shared()?.injector.lock().unwrap().type_text(text);
    if !typed {
        // Key by key on a US layout.
        let mut events = Vec::new();
        for c in text.chars() {
            let (key, shift) = crate::keys::char_key(c)
                .ok_or_else(|| Refused(format!("can't type {c:?} on this computer; try pasting it")))?;
            if shift {
                events.push(InputEvent::KeyDown { keycode: crate::keys::SHIFT });
            }
            events.push(InputEvent::KeyDown { keycode: key });
            events.push(InputEvent::KeyUp { keycode: key });
            if shift {
                events.push(InputEvent::KeyUp { keycode: crate::keys::SHIFT });
            }
        }
        inject(events)?;
    }
    let shown: String = text.chars().take(60).collect();
    let more = if text.chars().count() > 60 { "…" } else { "" };
    announce("type", format!("{shown:?}{more}"), None);
    Ok(json!({ "ok": true }))
}

pub async fn key(keys: &str) -> Result<Value, Refused> {
    may_act().await?;
    let codes = crate::keys::combo(keys).map_err(Refused)?;
    let mut events: Vec<InputEvent> = codes.iter().map(|&keycode| InputEvent::KeyDown { keycode }).collect();
    events.extend(codes.iter().rev().map(|&keycode| InputEvent::KeyUp { keycode }));
    inject(events)?;
    announce("key", keys.to_string(), None);
    Ok(json!({ "ok": true }))
}

/// Waits for the display to change and then settle (or for `timeout`).
pub async fn wait_for_change(id: Option<u32>, timeout: Duration) -> Result<Value, Refused> {
    may_act().await?;
    let d = display(id).await?;
    let shared = shared()?;
    let id = d.id;
    let changed = tokio::task::spawn_blocking(move || {
        const SMALL: [u32; 2] = [256, 160];
        let start = Instant::now();
        let first = shared.video.picture(id, SMALL).map(|f| f.bgra);
        let mut last = first.clone();
        let mut changed_at: Option<Instant> = None;
        while start.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(200));
            let now = shared.video.picture(id, SMALL).map(|f| f.bgra);
            if now != last {
                changed_at = Some(Instant::now());
                last = now;
            } else if changed_at.is_some_and(|t| t.elapsed() >= Duration::from_millis(500)) {
                return (true, start.elapsed());
            }
        }
        (changed_at.is_some(), start.elapsed())
    })
    .await
    .map_err(|e| Refused(e.to_string()))?;
    Ok(json!({ "changed": changed.0, "waited_ms": changed.1.as_millis() as u64 }))
}

/// Asks the person something and waits for their answer: `help` (they do something, then
/// say done) or `confirm` (yes or no).
pub async fn ask(kind: &'static str, text: &str) -> Result<Value, Refused> {
    may_act().await?;
    let text: String = text.chars().take(500).collect();
    let (tx, rx) = oneshot::channel();
    let id = with(|s| {
        s.next_ask += 1;
        let id = s.next_ask;
        s.asks.insert(id, Ask { kind, text: text.clone(), reply: tx });
        id
    });
    info!("agent: asks ({kind}) {text:?}");
    crate::testmode::record("agent", json!({ "ask": kind, "text": text }));
    broadcast(ServerMsg::AgentAsk { id, kind, text: text.clone() });
    crate::notify::notify(
        if kind == "help" { "An agent needs your help" } else { "An agent asks before it acts" },
        &text,
    );
    crate::push::agent_ask(id, kind == "help", &text);
    let answer = match tokio::time::timeout(ASK_WAIT, rx).await {
        Ok(Ok(a)) => a,
        _ => {
            with(|s| s.asks.remove(&id));
            broadcast(ServerMsg::AgentAnswered { id });
            return Err(Refused("Nobody answered.".into()));
        }
    };
    Ok(match kind {
        "confirm" => json!({ "approved": answer.ok, "note": answer.note }),
        _ => json!({ "done": answer.ok, "note": answer.note }),
    })
}

// ---- HTTP ----

pub fn routes() -> axum::Router {
    axum::Router::new()
        .route("/agent/v1/displays", get(http_displays))
        .route("/agent/v1/status", get(http_status))
        .route("/agent/v1/act", post(http_act))
}

type Peer = ConnectInfo<SocketAddr>;

fn reply(result: Result<Value, Refused>) -> Response<Body> {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(Refused(msg)) => (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response(),
    }
}

async fn owner(peer: &Peer, headers: &HeaderMap) -> Option<Response<Body>> {
    (!crate::forward::is_owner(peer.0.ip(), headers).await)
        .then(|| (StatusCode::FORBIDDEN, Json(json!({ "error": "owner only" }))).into_response())
}

async fn http_displays(peer: Peer, headers: HeaderMap) -> Response<Body> {
    if let Some(r) = owner(&peer, &headers).await {
        return r;
    }
    reply(displays().await.map(|all| {
        json!({ "displays": all.iter().map(|d| json!({
            "id": d.id, "width": d.width, "height": d.height, "main": d.main,
            "x": d.x, "y": d.y,
        })).collect::<Vec<_>>() })
    }))
}

async fn http_status(peer: Peer, headers: HeaderMap) -> Response<Body> {
    if let Some(r) = owner(&peer, &headers).await {
        return r;
    }
    Json(json!({ "status": with(|s| s.status) })).into_response()
}

/// One action: `{"action": "click", "x": …, "y": …, "display": …}` and so on.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Screenshot { display: Option<u32>, max_width: Option<u32>, max_height: Option<u32> },
    Click { display: Option<u32>, x: f64, y: f64, button: Option<String>, #[serde(default)] count: Option<u8> },
    Move { display: Option<u32>, x: f64, y: f64 },
    Drag { display: Option<u32>, from_x: f64, from_y: f64, to_x: f64, to_y: f64 },
    Scroll { display: Option<u32>, x: f64, y: f64, #[serde(default)] dx: f64, #[serde(default)] dy: f64 },
    Type { text: String },
    Key { keys: String },
    WaitForChange { display: Option<u32>, timeout_ms: Option<u64> },
    RequestHelp { text: String },
    Confirm { text: String },
}

pub async fn act(action: Action) -> Result<Value, Refused> {
    match action {
        Action::Screenshot { display, max_width, max_height } => {
            let max = (max_width.is_some() || max_height.is_some())
                .then(|| [max_width.unwrap_or(LARGEST), max_height.unwrap_or(LARGEST)]);
            screenshot(display, max).await
        }
        Action::Click { display, x, y, button, count } => {
            click(display, x, y, button.as_deref(), count.unwrap_or(1)).await
        }
        Action::Move { display, x, y } => pointer(display, x, y).await,
        Action::Drag { display, from_x, from_y, to_x, to_y } => drag(display, [from_x, from_y], [to_x, to_y]).await,
        Action::Scroll { display, x, y, dx, dy } => scroll(display, x, y, dx, dy).await,
        Action::Type { text } => type_text(&text).await,
        Action::Key { keys } => key(&keys).await,
        Action::WaitForChange { display, timeout_ms } => {
            wait_for_change(display, Duration::from_millis(timeout_ms.unwrap_or(10_000).min(60_000))).await
        }
        Action::RequestHelp { text } => ask("help", &text).await,
        Action::Confirm { text } => ask("confirm", &text).await,
    }
}

async fn http_act(peer: Peer, headers: HeaderMap, body: axum::body::Bytes) -> Response<Body> {
    if let Some(r) = owner(&peer, &headers).await {
        return r;
    }
    match serde_json::from_slice::<Action>(&body) {
        Ok(action) => reply(act(action).await),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(w: usize, h: usize) -> DisplayInfo {
        DisplayInfo { id: 99, width: w, height: h, x: 0.0, y: 0.0, points_width: w as f64, points_height: h as f64, main: true, builtin: false }
    }

    #[test]
    fn coordinates_are_in_the_default_screenshot_until_one_is_taken() {
        let d = display(2560, 1600);
        // Default screenshot: 1280×800.
        assert_eq!(normalize(&d, 640.0, 400.0), (0.5, 0.5));
        with(|s| s.sizes.insert(99, (100, 50)));
        assert_eq!(normalize(&d, 25.0, 50.0), (0.25, 1.0));
        with(|s| s.sizes.remove(&99));
    }
}
