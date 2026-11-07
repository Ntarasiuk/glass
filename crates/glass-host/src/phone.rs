//! Phones an agent can use: iPhones reached through WebDriverAgent (Appium's XCTest server)
//! on this computer, usually forwarded from the phone over USB (`ios forward 8100 8100`).
//!
//! The owner names each one with `glass config phones xr=8100`. Glass only talks to them on
//! loopback: WebDriverAgent has no sign-in of its own, so it must never be reached over a
//! network. Every action goes through the same path as one on this computer (stop, pause,
//! grants, the hourly budget, the audit log, the viewer's activity line), and for agents
//! under a grant the app in front on the phone is checked just as it is here.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};

use crate::computer::{Action, Refused};

/// Apps on a phone an agent never sees or uses, whatever its grant: settings, wallet and
/// passwords. The person does those (request_help).
pub const PROTECTED_APPS: &[&str] = &[
    "com.apple.Preferences",
    "com.apple.Passbook",
    "com.apple.Passwords",
];

/// The phone's home screen, lock screen, Control Center and notifications: an agent limited
/// to some apps can't reach them (they open anything).
const SYSTEM_UI: &[&str] = &["com.apple.springboard"];

/// One phone: its name, and the loopback ports its WebDriverAgent answers on: commands, and
/// its live picture (MJPEG).
#[derive(Clone, Debug, PartialEq)]
pub struct Phone {
    pub name: String,
    pub port: u16,
    pub video: u16,
    /// Reached over Wi-Fi rather than USB: its picture is sent smaller and less often, which
    /// Wi-Fi (and Apple's device tunnel over it, about 6 Mbit/s) can carry without holding
    /// up the touches queued behind it.
    pub wifi: bool,
}

impl Phone {
    /// How it's written in settings: `name=port`, or `name=port:video` when the video port
    /// isn't the usual one, with `/wifi` after it for a phone reached over Wi-Fi.
    pub fn entry(&self) -> String {
        let ports = if Some(self.video) == default_video(self.port) {
            format!("{}", self.port)
        } else {
            format!("{}:{}", self.port, self.video)
        };
        format!("{}={ports}{}", self.name, if self.wifi { "/wifi" } else { "" })
    }
}

/// WebDriverAgent's usual pairing: commands on 8100, its picture on 9100.
fn default_video(port: u16) -> Option<u16> {
    port.checked_add(1000)
}

fn port_number(s: &str) -> anyhow::Result<u16> {
    s.trim()
        .parse::<u16>()
        .ok()
        .filter(|&p| p != 0)
        .ok_or_else(|| anyhow::anyhow!("{s} isn't a port"))
}

/// `name=port[:video][/wifi]` entries, as `glass config phones` takes them.
pub fn parse(entries: &[String]) -> anyhow::Result<Vec<Phone>> {
    entries
        .iter()
        .map(|e| {
            let (name, ports) = e
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("{e}: expected name=port, e.g. xr=8100"))?;
            let name = name.trim();
            anyhow::ensure!(
                !name.is_empty()
                    && name.len() <= 40
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
                "{name:?} isn't a phone name (letters, digits, - and _)"
            );
            let (ports, wifi) = match ports.trim().split_once('/') {
                Some((ports, "wifi")) => (ports, true),
                Some((_, other)) => anyhow::bail!("{name}: unknown option /{other} (only /wifi)"),
                None => (ports, false),
            };
            let (port, video) = match ports.split_once(':') {
                Some((p, v)) => (port_number(p)?, port_number(v)?),
                None => {
                    let port = port_number(ports)?;
                    let video = default_video(port)
                        .ok_or_else(|| anyhow::anyhow!("give its video port too, e.g. {name}={port}:9100"))?;
                    (port, video)
                }
            };
            anyhow::ensure!(port != video, "{name}: its command and video ports must differ");
            Ok(Phone {
                name: name.to_string(),
                port,
                video,
                wifi,
            })
        })
        .collect()
}

pub(crate) fn configured() -> Vec<Phone> {
    parse(&crate::settings::load().phones).unwrap_or_default()
}

fn find(name: Option<&str>) -> Result<Phone, Refused> {
    let phones = configured();
    if phones.is_empty() {
        return Err(Refused(
            "No phones are set up on this computer (glass config phones name=port).".into(),
        ));
    }
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        None if phones.len() == 1 => Ok(phones[0].clone()),
        None => Err(Refused(format!("Say which phone: {}.", names(&phones)))),
        Some(n) => phones
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(n))
            .cloned()
            .ok_or_else(|| {
                Refused(format!(
                    "No phone called {n:?} (there's {}).",
                    names(&phones)
                ))
            }),
    }
}

fn names(phones: &[Phone]) -> String {
    phones
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---- WebDriverAgent ----

fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(2)))
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into()
    })
}

/// Sessions by port: WebDriverAgent wants one for most commands, and keeps it until it
/// restarts.
static SESSIONS: Mutex<Option<HashMap<u16, String>>> = Mutex::new(None);

fn call(p: &Phone, method: &str, path: &str, body: Option<&Value>) -> Result<Value, Refused> {
    let url = format!("http://127.0.0.1:{}{path}", p.port);
    let sent = if method == "GET" {
        agent().get(&url).call()
    } else {
        agent()
            .post(&url)
            .send_json(body.cloned().unwrap_or(json!({})))
    };
    let resp = sent.map_err(|e| {
        Refused(format!(
            "Can't reach {}: {e}. Is WebDriverAgent running and forwarded to port {}?",
            p.name, p.port
        ))
    })?;
    let status = resp.status();
    let text = resp
        .into_body()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| Refused(format!("bad reply from {}: {e}", p.name)))?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let error = v["value"]["error"].as_str();
    if status.is_success() && error.is_none() {
        return Ok(v);
    }
    Err(Refused(match error {
        Some(e) => format!(
            "{} said {e}: {}",
            p.name,
            v["value"]["message"].as_str().unwrap_or("")
        ),
        None => format!("{} answered {status}", p.name),
    }))
}

fn session(p: &Phone) -> Result<String, Refused> {
    if let Some(s) = SESSIONS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .get(&p.port)
    {
        return Ok(s.clone());
    }
    // Gestures don't wait for the app to settle afterwards (the viewer shows it settling).
    let v = call(
        p,
        "POST",
        "/session",
        Some(&json!({ "capabilities": { "alwaysMatch": { "shouldWaitForQuiescence": false } } })),
    )?;
    let id = v["sessionId"]
        .as_str()
        .or(v["value"]["sessionId"].as_str())
        .ok_or_else(|| Refused(format!("{} gave no session", p.name)))?;
    // Before each gesture WebDriverAgent snapshots the app in front, as deep as this: a
    // shallow one starts a touch about 100 ms sooner. What reads the tree goes deep (`deep`).
    let quick = json!({ "settings": { "waitForIdleTimeout": 0, "animationCoolOffTimeout": 0, "snapshotMaxDepth": SHALLOW } });
    if let Err(e) = call(p, "POST", &format!("/session/{id}/appium/settings"), Some(&quick)) {
        tracing::debug!("phone {}: couldn't speed up its gestures: {}", p.name, e.0);
    }
    SESSIONS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(p.port, id.to_string());
    Ok(id.to_string())
}

/// A call in the phone's session, starting a new session once if WebDriverAgent restarted.
pub(crate) fn in_session(p: &Phone, method: &str, path: &str, body: Option<&Value>) -> Result<Value, Refused> {
    let s = session(p)?;
    match call(p, method, &format!("/session/{s}{path}"), body) {
        Err(Refused(e))
            if e.contains("invalid session id") || e.contains("Session does not exist") =>
        {
            SESSIONS
                .lock()
                .unwrap()
                .get_or_insert_with(HashMap::new)
                .remove(&p.port);
            let s = session(p)?;
            call(p, method, &format!("/session/{s}{path}"), body)
        }
        other => other,
    }
}

/// The screen's size in points (what taps are in).
pub(crate) fn window(p: &Phone) -> Result<(f64, f64), Refused> {
    let v = in_session(p, "GET", "/window/size", None)?;
    let (w, h) = (
        v["value"]["width"].as_f64().unwrap_or(0.0),
        v["value"]["height"].as_f64().unwrap_or(0.0),
    );
    if w <= 0.0 || h <= 0.0 {
        return Err(Refused(format!("{} didn't say its screen size", p.name)));
    }
    Ok((w, h))
}

/// Asks WebDriverAgent for a smooth live picture: 30 frames a second (its most, on the
/// phones tried), at a quality that keeps text readable.
pub(crate) fn start_video(p: &Phone) -> Result<(), Refused> {
    // Over Wi-Fi a full-size picture (about 200 KB a frame on an iPhone 14 Pro) fills the
    // link and touches wait seconds behind it; measured, 30% at 10 a second keeps them
    // under about 100 ms. (WebDriverAgent ignores the quality when it scales.)
    let (fps, scale) = if p.wifi { (10, 30) } else { (30, 100) };
    let settings = json!({ "settings": { "mjpegServerFramerate": fps, "mjpegServerScreenshotQuality": 60, "mjpegScalingFactor": scale } });
    in_session(p, "POST", "/appium/settings", Some(&settings)).map(|_| ())
}

/// The app in front on the phone (bundle id).
fn front_app_of(p: &Phone) -> Option<String> {
    call(p, "GET", "/wda/activeAppInfo", None).ok()?["value"]["bundleId"]
        .as_str()
        .map(str::to_string)
}

/// Whether a password field has the phone's keyboard.
/// When each phone's quick settings were last made sure of (by port).
static QUICK_AT: Mutex<Option<HashMap<u16, Instant>>> = Mutex::new(None);

/// Makes sure the settings that keep gestures quick are still in force, at most every 10 s:
/// WebDriverAgent puts its settings back to their defaults whenever anything starts a new
/// session. Called after input, off its path.
pub(crate) fn keep_quick(p: &Phone) {
    let due = {
        let mut at = QUICK_AT.lock().unwrap();
        let at = at.get_or_insert_with(HashMap::new);
        let due = at.get(&p.port).is_none_or(|t| t.elapsed() >= Duration::from_secs(10));
        if due {
            at.insert(p.port, Instant::now());
        }
        due
    };
    if due {
        let quick = json!({ "settings": { "waitForIdleTimeout": 0, "animationCoolOffTimeout": 0, "snapshotMaxDepth": SHALLOW } });
        let _ = in_session(p, "POST", "/appium/settings", Some(&quick));
    }
}

/// How deep WebDriverAgent looks into the app before a gesture, and when reading it.
const SHALLOW: u32 = 0;
const DEEP: u32 = 50;

/// Runs `f` with WebDriverAgent looking all the way into the app (reading the screen as
/// text, finding the focused field), then makes gestures quick again.
fn deep<T>(p: &Phone, f: impl FnOnce() -> T) -> T {
    let depth = |d: u32| in_session(p, "POST", "/appium/settings", Some(&json!({ "settings": { "snapshotMaxDepth": d } })));
    let _ = depth(DEEP);
    let out = f();
    let _ = depth(SHALLOW);
    out
}

fn secure_field_focused(p: &Phone) -> bool {
    deep(p, || focused_is_secure(p))
}

fn focused_is_secure(p: &Phone) -> bool {
    let Ok(v) = in_session(p, "GET", "/element/active", None) else {
        return false;
    };
    let Some(id) = v["value"]["ELEMENT"]
        .as_str()
        .or(v["value"]["element-6066-11e4-a52e-4f735466cecf"].as_str())
    else {
        return false;
    };
    in_session(p, "GET", &format!("/element/{id}/attribute/type"), None)
        .ok()
        .and_then(|v| v["value"].as_str().map(|t| t.contains("SecureTextField")))
        .unwrap_or(false)
}

// ---- Rules (agents under a grant) ----

/// Refuses when an agent under a grant may not see or use what's in front on the phone.
fn check_front(p: &Phone, front: Option<&str>) -> Result<(), Refused> {
    let crate::grants::Actor::Agent(g) = crate::grants::current() else {
        return Ok(());
    };
    let Some(app) = front else {
        return Err(Refused(format!(
            "Can't tell which app is in front on {}, so agents wait. Try again in a moment.",
            p.name
        )));
    };
    if PROTECTED_APPS.contains(&app) {
        return Err(Refused(format!(
            "{app} is in front on {}, and agents don't use it. Ask the person with request_help.",
            p.name
        )));
    }
    if let Some(apps) = &g.apps
        && !apps.iter().any(|a| a == app)
    {
        let why = if SYSTEM_UI.contains(&app) {
            " (the home screen can open anything)"
        } else {
            ""
        };
        return Err(Refused(format!(
            "This agent may only use {}; {app} is in front on {}{why}.",
            apps.join(", "),
            p.name
        )));
    }
    Ok(())
}

fn guard(p: &Phone) -> Result<(), Refused> {
    if !matches!(crate::grants::current(), crate::grants::Actor::Agent(_)) {
        return Ok(());
    }
    check_front(p, front_app_of(p).as_deref())
}

// ---- Screenshots and coordinates ----

/// The size of each agent's latest screenshot of each phone, so its coordinates mean what
/// it saw.
type Sizes = HashMap<(String, String), (u32, u32)>;
static SIZES: Mutex<Option<Sizes>> = Mutex::new(None);

fn shot_size(p: &Phone) -> Option<(u32, u32)> {
    SIZES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .get(&(crate::computer::asker(), p.name.clone()))
        .copied()
}

/// Screenshot pixels to the phone's points.
fn to_points(p: &Phone, x: f64, y: f64) -> Result<(f64, f64), Refused> {
    let (w, h) = window(p)?;
    let (sw, sh) = shot_size(p)
        .map(|(a, b)| (a as f64, b as f64))
        .unwrap_or((w, h));
    Ok((
        (x * w / sw.max(1.0)).clamp(0.0, w - 1.0),
        (y * h / sh.max(1.0)).clamp(0.0, h - 1.0),
    ))
}

/// The phone's screen as PNG, scaled to fit `max`.
fn grab(p: &Phone, max: [u32; 2]) -> Result<(Vec<u8>, u32, u32), Refused> {
    let v = call(p, "GET", "/screenshot", None)?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(v["value"].as_str().unwrap_or("").replace(['\n', '\r'], ""))
        .map_err(|e| Refused(format!("bad screenshot from {}: {e}", p.name)))?;
    fit_png(&png, max).map_err(|e| Refused(format!("bad screenshot from {}: {e:#}", p.name)))
}

/// A PNG scaled down (box filter) to fit within `max`; as it is when it already fits.
pub fn fit_png(png: &[u8], max: [u32; 2]) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(Cursor::new(png));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width, info.height);
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => anyhow::bail!("indexed PNG left after expanding"),
    };
    let scale = (max[0] as f64 / w as f64)
        .min(max[1] as f64 / h as f64)
        .min(1.0);
    if scale >= 1.0 {
        return Ok((png.to_vec(), w, h));
    }
    let (nw, nh) = (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    );
    let mut rgb = Vec::with_capacity((nw * nh * 3) as usize);
    for oy in 0..nh {
        let (y0, y1) = (oy * h / nh, ((oy + 1) * h / nh).max(oy * h / nh + 1).min(h));
        for ox in 0..nw {
            let (x0, x1) = (ox * w / nw, ((ox + 1) * w / nw).max(ox * w / nw + 1).min(w));
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * w + x) * channels) as usize;
                    let px: [u32; 3] = if channels >= 3 {
                        [buf[i] as u32, buf[i + 1] as u32, buf[i + 2] as u32]
                    } else {
                        [buf[i] as u32; 3]
                    };
                    for c in 0..3 {
                        sum[c] += px[c];
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            rgb.extend(sum.iter().map(|s| (s / n.max(1)) as u8));
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, nw, nh);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&rgb)?;
    }
    Ok((out, nw, nh))
}

// ---- Touches ----

pub(crate) fn touch(p: &Phone, path: &[(f64, f64, u64)], hold_ms: u64) -> Result<(), Refused> {
    touch_then_rest(p, path, hold_ms, 0)
}

/// A touch that also rests where it ends for `rest_ms` before lifting (the app switcher is
/// a swipe up from the bottom edge that pauses).
pub(crate) fn touch_then_rest(p: &Phone, path: &[(f64, f64, u64)], hold_ms: u64, rest_ms: u64) -> Result<(), Refused> {
    let Some(&(x0, y0, _)) = path.first() else {
        return Ok(());
    };
    let mut steps = vec![
        json!({ "type": "pointerMove", "duration": 0, "x": x0.round(), "y": y0.round() }),
        json!({ "type": "pointerDown", "button": 0 }),
    ];
    if hold_ms > 0 {
        steps.push(json!({ "type": "pause", "duration": hold_ms }));
    }
    for &(x, y, ms) in &path[1..] {
        steps
            .push(json!({ "type": "pointerMove", "duration": ms, "x": x.round(), "y": y.round() }));
    }
    if rest_ms > 0 {
        steps.push(json!({ "type": "pause", "duration": rest_ms }));
    }
    steps.push(json!({ "type": "pointerUp", "button": 0 }));
    let body = json!({ "actions": [{ "type": "pointer", "id": "finger", "parameters": { "pointerType": "touch" }, "actions": steps }] });
    in_session(p, "POST", "/actions", Some(&body)).map(|_| ())
}

pub(crate) fn type_keys(p: &Phone, text: &str) -> Result<(), Refused> {
    let keys: Vec<String> = text.chars().map(String::from).collect();
    in_session(p, "POST", "/wda/keys", Some(&json!({ "value": keys }))).map(|_| ())
}

/// What `press_keys` can do on a phone.
const KEYS: &str = "home, return, backspace, tab, space, volume_up, volume_down";

pub(crate) fn press_key(p: &Phone, keys: &str) -> Result<(), Refused> {
    match keys
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-'], "_")
        .as_str()
    {
        // The button itself: `/wda/homescreen` also waits to see the home screen, and times
        // out over Spotlight (its own app on older iOS).
        "home" => button(p, "home").or_else(|_| call(p, "POST", "/wda/homescreen", None).map(|_| ())),
        "return" | "enter" => type_keys(p, "\n"),
        "backspace" | "delete" => type_keys(p, "\u{8}"),
        "tab" => type_keys(p, "\t"),
        "space" => type_keys(p, " "),
        "volume_up" | "volumeup" => button(p, "volumeUp"),
        "volume_down" | "volumedown" => button(p, "volumeDown"),
        other => Err(Refused(format!(
            "A phone has no {other:?} key. It has: {KEYS}."
        ))),
    }
}

/// Presses one of the phone's buttons (`home`, `volumeUp`, `volumeDown`). Older
/// WebDriverAgents only take this in a session.
fn button(p: &Phone, name: &str) -> Result<(), Refused> {
    let body = json!({ "name": name });
    in_session(p, "POST", "/wda/pressButton", Some(&body))
        .or_else(|_| call(p, "POST", "/wda/pressButton", Some(&body)))
        .map(|_| ())
}

// ---- Snapshots ----

/// An element a snapshot gave a ref to: where it is (points) and its text value.
#[derive(Clone, Debug)]
struct Item {
    center: (f64, f64),
    value: String,
}

/// Recent snapshots by id, with the phone they came from.
type Snapshots = HashMap<String, (String, Vec<Item>)>;
static SNAPSHOTS: Mutex<Option<Snapshots>> = Mutex::new(None);

/// Element types worth a ref: the ones a person taps or types into.
const ACTIONABLE: &[&str] = &[
    "Button",
    "Cell",
    "TextField",
    "SecureTextField",
    "SearchField",
    "TextView",
    "Switch",
    "Toggle",
    "Link",
    "Icon",
    "Key",
    "Slider",
    "SegmentedControl",
    "Tab",
    "MenuItem",
    "PickerWheel",
    "Stepper",
    "Image",
];

/// A WebDriverAgent tree (`/source?format=json`) as text, giving refs to what can be acted on.
fn render(root: &Value, snap: &str, max_lines: usize) -> (String, Vec<Item>, usize) {
    let mut lines = Vec::new();
    let mut items = Vec::new();
    let mut hidden = 0;
    fn walk(
        e: &Value,
        depth: usize,
        snap: &str,
        lines: &mut Vec<String>,
        items: &mut Vec<Item>,
        hidden: &mut usize,
        max: usize,
    ) {
        let kind = e["type"]
            .as_str()
            .unwrap_or("Other")
            .trim_start_matches("XCUIElementType");
        let label = e["label"]
            .as_str()
            .or(e["name"].as_str())
            .unwrap_or("")
            .trim();
        let value = match &e["value"] {
            Value::String(s) => s.trim().to_string(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let r = &e["rect"];
        let (x, y, w, h) = (
            r["x"].as_f64().unwrap_or(0.0),
            r["y"].as_f64().unwrap_or(0.0),
            r["width"].as_f64().unwrap_or(0.0),
            r["height"].as_f64().unwrap_or(0.0),
        );
        // Off-screen pages (the home screen's others, a list's far rows) are in the tree too.
        let on_screen = w > 0.0 && h > 0.0 && flag(&e["isVisible"]) != Some(false);
        let actionable =
            on_screen && flag(&e["isEnabled"]) != Some(false) && ACTIONABLE.contains(&kind);
        let says = !label.is_empty() || !value.is_empty();
        let shown = actionable
            || (says && on_screen)
            || matches!(kind, "Application" | "Alert" | "Keyboard" | "NavigationBar");
        let child_depth = if shown {
            if lines.len() >= max {
                *hidden += 1;
            } else {
                let mut line = format!("{}{}", "  ".repeat(depth), kind.to_ascii_lowercase());
                if !label.is_empty() {
                    line.push_str(&format!(" {label:?}"));
                }
                if !value.is_empty() && value != label {
                    line.push_str(&format!(" = {value:?}"));
                }
                if actionable {
                    items.push(Item {
                        center: (x + w / 2.0, y + h / 2.0),
                        value: value.clone(),
                    });
                    line.push_str(&format!(" @{snap}:e{}", items.len()));
                }
                lines.push(line);
            }
            depth + 1
        } else {
            depth
        };
        for c in e["children"].as_array().into_iter().flatten() {
            walk(c, child_depth, snap, lines, items, hidden, max);
        }
    }
    walk(
        root,
        0,
        snap,
        &mut lines,
        &mut items,
        &mut hidden,
        max_lines,
    );
    (lines.join("\n"), items, hidden)
}

/// A yes/no from WebDriverAgent, which sends `"1"` and `"0"` as often as booleans.
fn flag(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.as_str() {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        },
        Value::Number(n) => n.as_i64().map(|n| n != 0),
        _ => None,
    }
}

fn new_snap_id() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("{PHONE_SNAP}{:03x}", n % 0xfff)
}

/// Starts a phone snapshot's id. Computer snapshot ids (`elements::new_id`) can start with
/// `p` but never hold a `-`.
const PHONE_SNAP: &str = "p-";

/// Whether a ref came from a phone snapshot.
pub fn is_phone_ref(target: &str) -> bool {
    target
        .trim()
        .strip_prefix('@')
        .is_some_and(|r| r.starts_with(PHONE_SNAP) && r.contains(":e"))
}

fn lookup(target: &str) -> Result<(Phone, Item), Refused> {
    let r = target.trim().trim_start_matches('@');
    let (snap, n) = r
        .split_once(":e")
        .ok_or_else(|| Refused(format!("{target} isn't a ref")))?;
    let n: usize = n
        .parse()
        .map_err(|_| Refused(format!("{target} isn't a ref")))?;
    let (phone, items) = SNAPSHOTS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .get(snap)
        .cloned()
        .ok_or_else(|| Refused(format!("{target} is from an old snapshot; take a new one.")))?;
    let item = items
        .get(n.wrapping_sub(1))
        .cloned()
        .ok_or_else(|| Refused(format!("{target} isn't in that snapshot.")))?;
    Ok((find(Some(&phone))?, item))
}

// ---- Actions ----

/// Whether an action goes to a phone: it names one, or acts on a ref from a phone snapshot.
pub fn routes(phone: Option<&str>, action: &Action) -> bool {
    phone.is_some_and(|p| !p.trim().is_empty())
        || matches!(action, Action::Phones)
        || matches!(action, Action::Press { target } | Action::SetValue { target, .. } if is_phone_ref(target))
}

/// The app in front on the phone an action goes to, for the audit log.
pub async fn front_app(phone: Option<String>, action: &Action) -> Option<String> {
    let p = match action {
        Action::Press { target } | Action::SetValue { target, .. } if is_phone_ref(target) => {
            lookup(target).ok()?.0
        }
        _ => find(phone.as_deref()).ok()?,
    };
    tokio::task::spawn_blocking(move || front_app_of(&p))
        .await
        .ok()
        .flatten()
}

/// One action on a phone.
pub async fn act(phone: Option<String>, action: Action) -> Result<Value, Refused> {
    let look = action.describe().1 || matches!(action, Action::Phones);
    crate::computer::may_act(true).await?;
    if matches!(action, Action::Phones) {
        return tokio::task::spawn_blocking(list)
            .await
            .map_err(|e| Refused(e.to_string()))?;
    }
    let asker = crate::computer::asker();
    let actor = crate::grants::current();
    let run = move || crate::grants::ACTOR.sync_scope(actor, || run(phone, action, &asker));
    let (v, (kind, detail)) = tokio::task::spawn_blocking(run)
        .await
        .map_err(|e| Refused(e.to_string()))??;
    // The viewer's activity line: every input, and screenshots (as on a computer).
    if !kind.is_empty() && (!look || kind == "screenshot") {
        crate::computer::announce(&kind, detail, None);
    }
    Ok(v)
}

type Said = (String, String);

fn run(phone: Option<String>, action: Action, asker: &str) -> Result<(Value, Said), Refused> {
    let phone_ref = match &action {
        Action::Press { target } | Action::SetValue { target, .. } if is_phone_ref(target) => {
            Some(target.clone())
        }
        _ => None,
    };
    if let Some(target) = phone_ref {
        let target = target.as_str();
        let (p, item) = lookup(target)?;
        guard(&p)?;
        let (x, y) = item.center;
        return match action {
            Action::Press { .. } => {
                touch(&p, &[(x, y, 0)], 60)?;
                // `phone` tells the caller where a follow-up screenshot belongs.
                Ok((
                    json!({ "text": format!("Tapped {target} on {}.", p.name), "phone": p.name }),
                    ("press".into(), format!("{target} on {}", p.name)),
                ))
            }
            Action::SetValue { text, .. } => {
                touch(&p, &[(x, y, 0)], 60)?;
                if secure_field_focused(&p)
                    && matches!(crate::grants::current(), crate::grants::Actor::Agent(_))
                {
                    return Err(Refused(
                        "That's a password field. Ask the person to type it with request_help."
                            .into(),
                    ));
                }
                let erase: String = "\u{8}".repeat(item.value.chars().count());
                type_keys(&p, &format!("{erase}{text}"))?;
                let n = text.chars().count();
                Ok((
                    json!({ "text": format!("Typed {n} characters into {target} on {}.", p.name), "phone": p.name }),
                    ("type".into(), format!("{n} characters on {}", p.name)),
                ))
            }
            _ => unreachable!(),
        };
    }
    let p = find(phone.as_deref())?;
    let on = format!("on {}", p.name);
    match action {
        Action::Screenshot {
            max_width,
            max_height,
            ..
        } => {
            guard(&p)?;
            let max = [
                max_width
                    .unwrap_or(crate::computer::DEFAULT_MAX[0])
                    .clamp(16, 2560),
                max_height
                    .unwrap_or(crate::computer::DEFAULT_MAX[1])
                    .clamp(16, 2560),
            ];
            let (png, w, h) = grab(&p, max)?;
            SIZES
                .lock()
                .unwrap()
                .get_or_insert_with(HashMap::new)
                .insert((asker.to_string(), p.name.clone()), (w, h));
            Ok((
                json!({ "display": p.name, "phone": p.name, "width": w, "height": h, "png": base64::engine::general_purpose::STANDARD.encode(png) }),
                ("screenshot".into(), on),
            ))
        }
        Action::Click {
            x,
            y,
            button,
            count,
            ..
        } => {
            guard(&p)?;
            let (px, py) = to_points(&p, x, y)?;
            let long = button.as_deref() == Some("right");
            if button
                .as_deref()
                .is_some_and(|b| b != "left" && b != "right")
            {
                return Err(Refused(
                    "A phone takes taps (left) and long presses (right).".into(),
                ));
            }
            for _ in 0..count.unwrap_or(1).clamp(1, 3) {
                touch(&p, &[(px, py, 0)], if long { 900 } else { 60 })?;
            }
            Ok((
                json!({ "ok": true }),
                (
                    if long { "long press" } else { "tap" }.into(),
                    format!("at {x:.0}, {y:.0} {on}"),
                ),
            ))
        }
        Action::Drag {
            from_x,
            from_y,
            to_x,
            to_y,
            ..
        } => {
            guard(&p)?;
            let (a, b) = (to_points(&p, from_x, from_y)?, to_points(&p, to_x, to_y)?);
            touch(&p, &[(a.0, a.1, 0), (b.0, b.1, 400)], 150)?;
            Ok((
                json!({ "ok": true }),
                (
                    "drag".into(),
                    format!("{from_x:.0}, {from_y:.0} → {to_x:.0}, {to_y:.0} {on}"),
                ),
            ))
        }
        Action::Scroll { x, y, dx, dy, .. } => {
            guard(&p)?;
            // Scrolling down moves the finger up.
            let (sx, sy) = to_points(&p, x, y)?;
            let (ex, ey) = to_points(&p, x - dx, y - dy)?;
            touch(&p, &[(sx, sy, 0), (ex, ey, 250)], 0)?;
            Ok((
                json!({ "ok": true }),
                ("scroll".into(), format!("at {x:.0}, {y:.0} {on}")),
            ))
        }
        Action::Move { .. } => Err(Refused(
            "A phone has no pointer to move; use click (a tap) or drag.".into(),
        )),
        Action::Type { text } => {
            guard(&p)?;
            if matches!(crate::grants::current(), crate::grants::Actor::Agent(_))
                && secure_field_focused(&p)
            {
                return Err(Refused("A password field has the keyboard. Ask the person to type it with request_help.".into()));
            }
            type_keys(&p, &text)?;
            Ok((
                json!({ "ok": true }),
                (
                    "type".into(),
                    format!("{} characters {on}", text.chars().count()),
                ),
            ))
        }
        Action::Key { keys } => {
            guard(&p)?;
            press_key(&p, &keys)?;
            Ok((
                json!({ "ok": true }),
                ("key".into(), format!("{keys} {on}")),
            ))
        }
        Action::Snapshot { max_lines, .. } => {
            guard(&p)?;
            let v = deep(&p, || in_session(&p, "GET", "/source?format=json", None))?;
            let snap = new_snap_id();
            let (text, items, hidden) =
                render(&v["value"], &snap, max_lines.unwrap_or(400).clamp(20, 2000));
            let refs = items.len();
            let mut all = SNAPSHOTS.lock().unwrap();
            let all = all.get_or_insert_with(HashMap::new);
            if all.len() >= 16 {
                all.clear();
            }
            all.insert(snap.clone(), (p.name.clone(), items));
            let more = if hidden > 0 {
                format!("\n… {hidden} more (raise max_lines)")
            } else {
                String::new()
            };
            let said = format!(
                "{}, snapshot {snap}: {refs} elements to act on with press or set_value, by ref. Coordinates are the phone's points; press taps the element.\n\n{text}{more}",
                p.name
            );
            Ok((json!({ "text": said }), (String::new(), String::new())))
        }
        Action::WaitForChange {
            change,
            idle_ms,
            timeout_ms,
            ..
        } => {
            let w = wait(
                &p,
                change,
                Duration::from_millis(idle_ms.unwrap_or(if change { 0 } else { 300 }).min(60_000)),
                Duration::from_millis(timeout_ms.unwrap_or(10_000).min(60_000)),
            )?;
            Ok((w, (String::new(), String::new())))
        }
        Action::Record { .. } => Err(Refused(
            "Recording a phone isn't built yet; take screenshots instead.".into(),
        )),
        Action::RequestHelp { .. } | Action::Confirm { .. } | Action::CheckAnswer { .. } => {
            Err(Refused(
                "Ask the person without naming a phone: questions go to the person, not a device."
                    .into(),
            ))
        }
        Action::Press { .. } | Action::SetValue { .. } => {
            Err(Refused("That ref isn't from a phone snapshot.".into()))
        }
        Action::Phones => unreachable!(),
    }
}

/// Waits for the phone's screen by comparing small screenshots.
fn wait(p: &Phone, change: bool, idle: Duration, timeout: Duration) -> Result<Value, Refused> {
    let start = Instant::now();
    let look = || grab(p, [160, 160]).map(|(png, _, _)| png);
    let first = look()?;
    let (mut last, mut since) = (first.clone(), Instant::now());
    let mut changed = false;
    loop {
        if start.elapsed() >= timeout {
            return Ok(
                json!({ "changed": changed, "timed_out": true, "waited_ms": start.elapsed().as_millis() as u64 }),
            );
        }
        std::thread::sleep(Duration::from_millis(250));
        let now = look()?;
        if now != last {
            changed = true;
            last = now;
            since = Instant::now();
            continue;
        }
        let settled = since.elapsed() >= idle;
        if (change && changed && settled) || (!change && settled) {
            return Ok(
                json!({ "changed": changed, "timed_out": false, "waited_ms": start.elapsed().as_millis() as u64 }),
            );
        }
    }
}

/// The phones set up here, and whether each is ready.
fn list() -> Result<Value, Refused> {
    let phones: Vec<Value> = configured()
        .into_iter()
        .map(|p| {
            let status = call(&p, "GET", "/status", None);
            let ready = status
                .as_ref()
                .is_ok_and(|s| s["value"]["ready"].as_bool().unwrap_or(false));
            let os = status.as_ref().ok().and_then(|s| {
                s["value"]["os"]["version"]
                    .as_str()
                    .map(|v| format!("iOS {v}"))
            });
            let size = ready
                .then(|| window(&p).ok())
                .flatten()
                .map(|(w, h)| format!("{w:.0}×{h:.0} points"));
            json!({ "name": p.name, "ready": ready, "os": os, "screen": size,
                "front_app": ready.then(|| front_app_of(&p)).flatten(),
                "problem": status.err().map(|e| e.0) })
        })
        .collect();
    let text = if phones.is_empty() {
        "No phones are set up on this computer (glass config phones name=port).".to_string()
    } else {
        serde_json::to_string_pretty(&phones).unwrap_or_default()
    };
    Ok(json!({ "text": text, "phones": phones }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phones_are_named_ports_on_loopback() {
        let ok = parse(&["xr=8100".into(), "se_2=8101:9300".into(), "pro=8200/wifi".into()]).unwrap();
        assert_eq!(
            ok,
            vec![
                Phone {
                    name: "xr".into(),
                    port: 8100,
                    video: 9100,
                    wifi: false
                },
                Phone {
                    name: "se_2".into(),
                    port: 8101,
                    video: 9300,
                    wifi: false
                },
                Phone {
                    name: "pro".into(),
                    port: 8200,
                    video: 9200,
                    wifi: true
                }
            ]
        );
        // Written back the way it was given: the usual video port is left out.
        assert_eq!(ok[0].entry(), "xr=8100");
        assert_eq!(ok[1].entry(), "se_2=8101:9300");
        assert_eq!(ok[2].entry(), "pro=8200/wifi");
        assert_eq!(parse(&["pro=8200:9300/wifi".into()]).unwrap()[0].entry(), "pro=8200:9300/wifi");
        assert!(parse(&["xr=8100/fast".into()]).is_err(), "only /wifi");
        assert!(parse(&["xr=8100:8100".into()]).is_err(), "command and video ports differ");
        assert!(parse(&["xr=65000".into()]).is_err(), "no room for the usual video port");
        assert!(parse(&["xr".into()]).is_err());
        assert!(
            parse(&["xr=http://10.0.0.5:8100".into()]).is_err(),
            "a phone is a local port, never a network address"
        );
        assert!(parse(&["x r=8100".into()]).is_err());
        assert!(parse(&["xr=0".into()]).is_err());
    }

    #[test]
    fn phone_refs_are_told_apart_from_computer_refs() {
        assert!(is_phone_ref(&format!("@{}:e3", new_snap_id())));
        assert!(!is_phone_ref("@k3f9:e12"));
        // A computer snapshot's id can start with a p too.
        assert!(!is_phone_ref("@p3f9:e12"));
        assert!(!is_phone_ref("p-1a2:e3"));
    }

    #[test]
    fn snapshots_ref_what_can_be_tapped_and_skip_empty_containers() {
        let tree = json!({ "type": "XCUIElementTypeApplication", "label": "Settings", "rect": { "x": 0, "y": 0, "width": 414, "height": 896 }, "children": [
            { "type": "XCUIElementTypeOther", "rect": { "x": 0, "y": 0, "width": 414, "height": 896 }, "children": [
                { "type": "XCUIElementTypeButton", "label": "General", "isEnabled": true, "rect": { "x": 10, "y": 100, "width": 100, "height": 40 } },
                { "type": "XCUIElementTypeTextField", "label": "Search", "value": "wifi", "isEnabled": true, "rect": { "x": 10, "y": 50, "width": 300, "height": 30 } },
                { "type": "XCUIElementTypeButton", "label": "Hidden", "isEnabled": true, "rect": { "x": 0, "y": 0, "width": 0, "height": 0 } },
                { "type": "XCUIElementTypeIcon", "label": "Off page", "isEnabled": "1", "isVisible": "0", "rect": { "x": 500, "y": 0, "width": 60, "height": 60 } },
                { "type": "XCUIElementTypeButton", "label": "Greyed", "isEnabled": "0", "isVisible": "1", "rect": { "x": 10, "y": 300, "width": 60, "height": 30 } },
                { "type": "XCUIElementTypeStaticText", "label": "Version 14.8", "rect": { "x": 0, "y": 200, "width": 100, "height": 20 } },
            ] },
        ] });
        let (text, items, hidden) = render(&tree, "p1", 400);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].center, (60.0, 120.0));
        assert_eq!(items[1].value, "wifi");
        assert!(text.contains("button \"General\" @p1:e1"), "{text}");
        assert!(
            text.contains("textfield \"Search\" = \"wifi\" @p1:e2"),
            "{text}"
        );
        assert!(text.contains("statictext \"Version 14.8\""));
        assert!(
            !text.contains("Hidden") && !text.contains("Off page") && !text.contains("other"),
            "{text}"
        );
        assert!(
            text.contains("button \"Greyed\"") && !text.contains("Greyed\" @"),
            "shown, but disabled: no ref\n{text}"
        );
        assert_eq!(hidden, 0);
        let (_, _, hidden) = render(&tree, "p1", 2);
        assert_eq!(hidden, 3);
    }

    #[test]
    fn screenshots_shrink_to_fit() {
        let (w, h) = (40u32, 80u32);
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()
                .unwrap()
                .write_image_data(&vec![200u8; (w * h * 4) as usize])
                .unwrap();
        }
        let (_, sw, sh) = fit_png(&png_bytes, [1280, 800]).unwrap();
        assert_eq!((sw, sh), (40, 80), "already fits: as it is");
        let (out, sw, sh) = fit_png(&png_bytes, [100, 40]).unwrap();
        assert_eq!((sw, sh), (20, 40));
        let (back, _, _) = fit_png(&out, [1000, 1000]).unwrap();
        assert_eq!(back, out);
    }

    #[test]
    fn agents_stay_out_of_protected_phone_apps() {
        let p = Phone {
            name: "xr".into(),
            port: 1,
            video: 2,
            wifi: false,
        };
        // The owner isn't limited.
        assert!(check_front(&p, Some("com.apple.Preferences")).is_ok());
        let g = crate::grants::Grant::for_test(None);
        let refused = crate::grants::ACTOR
            .sync_scope(crate::grants::Actor::Agent(g.clone()), || {
                check_front(&p, Some("com.apple.Preferences"))
            });
        assert!(refused.unwrap_err().0.contains("don't use it"));
        assert!(
            crate::grants::ACTOR
                .sync_scope(crate::grants::Actor::Agent(g), || check_front(
                    &p,
                    Some("com.apple.mobilesafari")
                ))
                .is_ok()
        );
        let limited = crate::grants::Grant::for_test(Some(vec!["com.apple.mobilesafari".into()]));
        let home = crate::grants::ACTOR
            .sync_scope(crate::grants::Actor::Agent(limited.clone()), || {
                check_front(&p, Some("com.apple.springboard"))
            });
        assert!(home.unwrap_err().0.contains("home screen"));
        assert!(
            crate::grants::ACTOR
                .sync_scope(
                    crate::grants::Actor::Agent(limited.clone()),
                    || check_front(&p, Some("com.apple.mobilesafari"))
                )
                .is_ok()
        );
        assert!(
            crate::grants::ACTOR
                .sync_scope(crate::grants::Actor::Agent(limited), || check_front(
                    &p, None
                ))
                .is_err(),
            "unknown front app: wait"
        );
    }
}
