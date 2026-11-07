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
    /// The phone itself (its UDID), when Glass keeps the link to it up (phone_link): it
    /// forwards the two ports and starts WebDriverAgent. Without one, they're the user's.
    pub udid: Option<String>,
}

impl Phone {
    /// How it's written in settings: `name=port`, or `name=port:video` when the video port
    /// isn't the usual one, with `/wifi` after it for a phone reached over Wi-Fi and
    /// `@udid` for one Glass keeps connected.
    pub fn entry(&self) -> String {
        let ports = if Some(self.video) == default_video(self.port) {
            format!("{}", self.port)
        } else {
            format!("{}:{}", self.port, self.video)
        };
        let wifi = if self.wifi { "/wifi" } else { "" };
        let udid = self.udid.as_ref().map(|u| format!("@{u}")).unwrap_or_default();
        format!("{}={ports}{wifi}{udid}", self.name)
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

/// `name=port[:video][/wifi][@udid]` entries, as `glass config phones` takes them.
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
            let (ports, udid) = match ports.trim().rsplit_once('@') {
                Some((ports, udid)) => {
                    let udid = udid.trim();
                    anyhow::ensure!(
                        (20..=64).contains(&udid.len()) && udid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'),
                        "{name}: {udid:?} isn't a phone's UDID"
                    );
                    (ports, Some(udid.to_string()))
                }
                None => (ports, None),
            };
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
                udid,
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
    if !due {
        return;
    }
    // Not while something reads the app deeply (`deep`): it would make that read shallow,
    // and it puts the quick settings back itself when it's done. Then again next time.
    let Ok(_reading) = DEPTH.try_lock() else {
        QUICK_AT.lock().unwrap().get_or_insert_with(HashMap::new).remove(&p.port);
        return;
    };
    let quick = json!({ "settings": { "waitForIdleTimeout": 0, "animationCoolOffTimeout": 0, "snapshotMaxDepth": SHALLOW } });
    let _ = in_session(p, "POST", "/appium/settings", Some(&quick));
}

/// How deep WebDriverAgent looks into the app before a gesture, and when reading it.
const SHALLOW: u32 = 0;
const DEEP: u32 = 50;

/// Held while WebDriverAgent looks deep into the app, so nothing makes it shallow meanwhile
/// (`keep_quick`, another deep read finishing first).
static DEPTH: Mutex<()> = Mutex::new(());

fn set_depth(p: &Phone, d: u32) -> Result<Value, Refused> {
    in_session(p, "POST", "/appium/settings", Some(&json!({ "settings": { "snapshotMaxDepth": d } })))
}

/// Runs `f` with WebDriverAgent looking all the way into the app (reading the screen as
/// text), then makes gestures quick again.
fn deep<T>(p: &Phone, f: impl FnOnce() -> T) -> T {
    let _reading = DEPTH.lock().unwrap();
    let _ = set_depth(p, DEEP);
    let out = f();
    let _ = set_depth(p, SHALLOW);
    out
}

/// What has the phone's keyboard, looked up once for an action by an agent under a grant
/// (one deep look: four or six requests). Its rules keep a password the person typed from
/// the agent without stopping it from carrying on: it can't type there or touch the
/// keyboard (a key, or the suggested password above it), but it can tap Sign in, scroll,
/// or press return.
struct Focus {
    /// A password field has the keyboard (an error: that can't be told).
    secure: Result<bool, Refused>,
    /// Where the keyboard is (points: x, y, width, height); `None` when none is up; an
    /// error when it can't be told. Only looked up when a password field may have it.
    keyboard: Result<Option<[f64; 4]>, Refused>,
}

/// The suggestions bar above the keyboard (where a saved password is offered), in points:
/// counted as the keyboard.
const ABOVE_KEYBOARD: f64 = 56.0;
/// Where the keyboard may be when it can't be told: the bottom of the screen, this much of it
/// (a landscape keyboard with its suggestions takes about this).
const KEYBOARD_GUESS: f64 = 0.6;

impl Focus {
    /// Nothing to keep from the owner's own tools.
    fn clear() -> Focus {
        Focus { secure: Ok(false), keyboard: Ok(None) }
    }

    fn of(p: &Phone) -> Focus {
        if !matches!(crate::grants::current(), crate::grants::Actor::Agent(_)) {
            return Focus::clear();
        }
        let _reading = DEPTH.lock().unwrap();
        // A shallow look finds no field, so it must be the deep one, and its answer must come.
        let secure = set_depth(p, DEEP).and_then(|_| focused_is_secure(p));
        let keyboard = match secure {
            Ok(false) => Ok(None),
            _ => keyboard_frame(p),
        };
        let _ = set_depth(p, SHALLOW);
        Focus { secure, keyboard }
    }

    /// Typing (and keys that type) into what has the keyboard.
    fn may_type(&self, p: &Phone) -> Result<(), Refused> {
        match &self.secure {
            Ok(false) => Ok(()),
            Ok(true) => Err(Refused(format!(
                "A password field has the keyboard on {}, so agents don't type there. Ask the person to fill it in with request_help; then you can tap Sign in or press return.",
                p.name
            ))),
            Err(e) => Err(cant_tell(p, e)),
        }
    }

    /// A touch at these points (the phone's points); `screen` is its size, for when where
    /// the keyboard is can't be told.
    fn may_touch(&self, p: &Phone, points: &[(f64, f64)], screen: impl FnOnce() -> Result<(f64, f64), Refused>) -> Result<(), Refused> {
        if matches!(self.secure, Ok(false)) {
            return Ok(());
        }
        let (area, guessed) = match &self.keyboard {
            Ok(None) => return Ok(()),
            Ok(Some([x, y, w, h])) => ([*x, y - ABOVE_KEYBOARD, *w, h + ABOVE_KEYBOARD], false),
            Err(_) => {
                let (w, h) = screen()?;
                ([0.0, h * (1.0 - KEYBOARD_GUESS), w, h * KEYBOARD_GUESS], true)
            }
        };
        let [x, y, w, h] = area;
        if !points.iter().any(|&(px, py)| px >= x && px < x + w && py >= y && py < y + h) {
            return Ok(());
        }
        Err(Refused(if guessed {
            format!(
                "A password field may have the keyboard on {}, and Glass can't tell where the keyboard is, so agents don't touch the bottom {:.0}% of the screen. Tap above it, or ask the person with request_help.",
                p.name,
                KEYBOARD_GUESS * 100.0
            )
        } else {
            format!(
                "A password field has the keyboard on {}, so agents don't touch the keyboard (or the suggestions above it). Tap elsewhere (Sign in, say), press return, or ask the person with request_help.",
                p.name
            )
        }))
    }
}

fn cant_tell(p: &Phone, e: &Refused) -> Refused {
    Refused(format!(
        "Can't tell whether a password field has the keyboard on {} ({}), so agents don't type there now. Try again in a moment.",
        p.name, e.0
    ))
}

/// The id in an element WebDriverAgent returned.
fn element_id(v: &Value) -> Option<&str> {
    v["ELEMENT"].as_str().or(v["element-6066-11e4-a52e-4f735466cecf"].as_str())
}

fn focused_is_secure(p: &Phone) -> Result<bool, Refused> {
    let v = match in_session(p, "GET", "/element/active", None) {
        Ok(v) => v,
        // Nothing has the keyboard.
        Err(Refused(e)) if e.contains("said no such element") => return Ok(false),
        Err(e) => return Err(e),
    };
    let id = element_id(&v["value"]).ok_or_else(|| Refused("it named no focused element".into()))?;
    let v = in_session(p, "GET", &format!("/element/{id}/attribute/type"), None)?;
    let kind = v["value"]
        .as_str()
        .ok_or_else(|| Refused("it didn't say what the focused element is".into()))?;
    Ok(kind.contains("SecureTextField"))
}

/// Where the on-screen keyboard is (points), if one is up.
fn keyboard_frame(p: &Phone) -> Result<Option<[f64; 4]>, Refused> {
    let found = in_session(p, "POST", "/element", Some(&json!({ "using": "class name", "value": "XCUIElementTypeKeyboard" })));
    let v = match found {
        Ok(v) => v,
        Err(Refused(e)) if e.contains("said no such element") => return Ok(None),
        Err(e) => return Err(e),
    };
    let id = element_id(&v["value"]).ok_or_else(|| Refused("it named no keyboard".into()))?;
    let r = in_session(p, "GET", &format!("/element/{id}/rect"), None)?;
    let n = |k: &str| r["value"][k].as_f64();
    match (n("x"), n("y"), n("width"), n("height")) {
        (Some(x), Some(y), Some(w), Some(h)) if w > 0.0 && h > 0.0 => Ok(Some([x, y, w, h])),
        (Some(_), Some(_), Some(_), Some(_)) => Ok(None),
        _ => Err(Refused("it didn't say where the keyboard is".into())),
    }
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

/// Which way the phone's screen is turned, as WebDriverAgent says it: `PORTRAIT`,
/// `LANDSCAPE` (turned left, the bottom on the right), `UIA_DEVICE_ORIENTATION_LANDSCAPERIGHT`,
/// `UIA_DEVICE_ORIENTATION_PORTRAIT_UPSIDEDOWN`.
pub(crate) fn orientation(p: &Phone) -> Result<String, Refused> {
    let v = in_session(p, "GET", "/orientation", None)?;
    Ok(v["value"].as_str().unwrap_or("PORTRAIT").to_string())
}

/// Two fingers on either side of `at` (points), from `from` apart to `to` apart over `ms`.
pub(crate) fn pinch(p: &Phone, at: (f64, f64), from: f64, to: f64, ms: u64) -> Result<(), Refused> {
    let finger = |id: &str, side: f64| {
        let x = |gap: f64| (at.0 + side * gap / 2.0).round();
        json!({ "type": "pointer", "id": id, "parameters": { "pointerType": "touch" }, "actions": [
            { "type": "pointerMove", "duration": 0, "x": x(from), "y": at.1.round() },
            { "type": "pointerDown", "button": 0 },
            { "type": "pointerMove", "duration": ms, "x": x(to), "y": at.1.round() },
            { "type": "pointerUp", "button": 0 },
        ] })
    };
    let body = json!({ "actions": [finger("left", -1.0), finger("right", 1.0)] });
    in_session(p, "POST", "/actions", Some(&body)).map(|_| ())
}

pub(crate) fn type_keys(p: &Phone, text: &str) -> Result<(), Refused> {
    let keys: Vec<String> = text.chars().map(String::from).collect();
    in_session(p, "POST", "/wda/keys", Some(&json!({ "value": keys }))).map(|_| ())
}

/// What `press_keys` can do on a phone.
const KEYS: &str = "home, return, backspace, tab, space, volume_up, volume_down";

/// A key's name as `press_key` reads it.
fn key_name(keys: &str) -> String {
    keys.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

/// Return (or enter): submits a field rather than typing into it.
fn is_return(keys: &str) -> bool {
    matches!(key_name(keys).as_str(), "return" | "enter")
}

/// One of the phone's buttons (home, volume), not a key of its keyboard.
fn is_button(keys: &str) -> bool {
    matches!(key_name(keys).as_str(), "home" | "volume_up" | "volumeup" | "volume_down" | "volumedown")
}

pub(crate) fn press_key(p: &Phone, keys: &str) -> Result<(), Refused> {
    match key_name(keys).as_str() {
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

/// A snapshot an agent took: who took it, of which phone, and what its refs point at.
struct Snap {
    asker: String,
    phone: String,
    items: Vec<Item>,
    at: Instant,
}

/// Recent snapshots by id.
static SNAPSHOTS: Mutex<Option<HashMap<String, Snap>>> = Mutex::new(None);
/// How many are kept (the oldest goes first).
const KEEP_SNAPSHOTS: usize = 64;

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

/// A snapshot's id: random and long enough that two never meet, so a ref always means the
/// element of the snapshot it came from.
fn new_snap_id() -> String {
    let mut b = [0u8; 6];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).expect("random");
    format!("{PHONE_SNAP}{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
}

/// Keeps a snapshot's refs for the agent that took it.
fn keep_snapshot(id: &str, snap: Snap) {
    let mut all = SNAPSHOTS.lock().unwrap();
    let all = all.get_or_insert_with(HashMap::new);
    while all.len() >= KEEP_SNAPSHOTS {
        let Some(oldest) = all.iter().min_by_key(|(_, s)| s.at).map(|(k, _)| k.clone()) else { break };
        all.remove(&oldest);
    }
    all.insert(id.to_string(), snap);
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

/// The phone and element a ref points at: from a snapshot `asker` took (any, for the audit
/// log, which runs before the action).
fn lookup(target: &str, asker: Option<&str>) -> Result<(Phone, Item), Refused> {
    let (phone, item) = ref_item(target, asker)?;
    Ok((find(Some(&phone))?, item))
}

/// The phone's name and the element a ref points at.
fn ref_item(target: &str, asker: Option<&str>) -> Result<(String, Item), Refused> {
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
        .filter(|s| asker.is_none_or(|a| s.asker == a))
        .map(|s| (s.phone.clone(), s.items.clone()))
        .ok_or_else(|| Refused(format!("{target} is from an old snapshot; take a new one.")))?;
    let item = items
        .get(n.wrapping_sub(1))
        .cloned()
        .ok_or_else(|| Refused(format!("{target} isn't in that snapshot.")))?;
    Ok((phone, item))
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
            lookup(target, None).ok()?.0
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
    // Touching or typing waits while the person is using the computer or a phone from a
    // viewer, as on the computer; looking doesn't.
    crate::computer::may_act(look).await?;
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
        let (p, item) = lookup(target, Some(asker))?;
        guard(&p)?;
        let (x, y) = item.center;
        // A ref may be a key of the keyboard.
        Focus::of(&p).may_touch(&p, &[(x, y)], || window(&p))?;
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
                // The tap may have given a password field the keyboard.
                Focus::of(&p).may_type(&p)?;
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
            Focus::of(&p).may_touch(&p, &[(px, py)], || window(&p))?;
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
            Focus::of(&p).may_touch(&p, &[a, b], || window(&p))?;
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
            Focus::of(&p).may_touch(&p, &[(sx, sy), (ex, ey)], || window(&p))?;
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
            Focus::of(&p).may_type(&p)?;
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
            // The buttons don't type (home leaves a password field), nor does return (it
            // submits what the person typed); the rest are keys that type.
            if !is_button(&keys) && !is_return(&keys) {
                Focus::of(&p).may_type(&p)?;
            }
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
            keep_snapshot(&snap, Snap { asker: asker.to_string(), phone: p.name.clone(), items, at: Instant::now() });
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
                    wifi: false,
                    udid: None
                },
                Phone {
                    name: "se_2".into(),
                    port: 8101,
                    video: 9300,
                    wifi: false,
                    udid: None
                },
                Phone {
                    name: "pro".into(),
                    port: 8200,
                    video: 9200,
                    wifi: true,
                    udid: None
                }
            ]
        );
        // Written back the way it was given: the usual video port is left out.
        assert_eq!(ok[0].entry(), "xr=8100");
        assert_eq!(ok[1].entry(), "se_2=8101:9300");
        assert_eq!(ok[2].entry(), "pro=8200/wifi");
        assert_eq!(parse(&["pro=8200:9300/wifi".into()]).unwrap()[0].entry(), "pro=8200:9300/wifi");
        assert!(parse(&["xr=8100/fast".into()]).is_err(), "only /wifi");
        let kept = parse(&["xr=8100@00008020-000649323451002E".into(), "pro=8200:9200/wifi@00008120-00040D520C38C01E".into()]).unwrap();
        assert_eq!(kept[0].udid.as_deref(), Some("00008020-000649323451002E"));
        assert!(!kept[0].wifi);
        assert!(kept[1].wifi && kept[1].video == 9200);
        assert_eq!(kept[1].entry(), "pro=8200/wifi@00008120-00040D520C38C01E", "9200 is 8200's usual video port");
        assert!(parse(&["xr=8100@nope".into()]).is_err());
        assert!(parse(&["xr=8100@../../etc".into()]).is_err());
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
    fn password_fields_keep_the_agent_off_the_keyboard_but_not_the_screen() {
        let p = Phone { name: "xr".into(), port: 1, video: 2, wifi: false, udid: None };
        let screen = || Ok((390.0, 844.0));
        let no_screen = || -> Result<(f64, f64), Refused> { panic!("not needed") };
        let focus = |secure: Result<bool, Refused>, keyboard: Result<Option<[f64; 4]>, Refused>| Focus { secure, keyboard };
        let (above, key, bar) = ((200.0, 100.0), (200.0, 700.0), (200.0, 520.0));
        // Nothing secure: anything goes.
        let clear = Focus::clear();
        assert!(clear.may_type(&p).is_ok() && clear.may_touch(&p, &[key], no_screen).is_ok());
        // A password field has the keyboard (at 0, 550 + 294): no typing, nor touching the keyboard
        // or the suggestions bar just above it; the rest of the screen is fine.
        let secure = focus(Ok(true), Ok(Some([0.0, 550.0, 390.0, 294.0])));
        assert!(secure.may_type(&p).unwrap_err().0.contains("return"));
        assert!(secure.may_touch(&p, &[above], no_screen).is_ok(), "Sign in, above it");
        assert!(secure.may_touch(&p, &[key], no_screen).unwrap_err().0.contains("keyboard"));
        assert!(secure.may_touch(&p, &[bar], no_screen).is_err(), "the suggestions bar");
        assert!(secure.may_touch(&p, &[above, key], no_screen).is_err(), "a swipe onto it");
        // Focused but no keyboard up (a hardware keyboard): touches are fine.
        assert!(focus(Ok(true), Ok(None)).may_touch(&p, &[key], no_screen).is_ok());
        // Where the keyboard is can't be told: the bottom of the screen is off limits.
        let unsure = focus(Ok(true), Err(Refused("no answer".into())));
        assert!(unsure.may_touch(&p, &[above], screen).is_ok());
        assert!(unsure.may_touch(&p, &[key], screen).unwrap_err().0.contains("can't tell where"));
        // Whether a password field has it can't be told: no typing (fail closed); touches as
        // for one that does.
        let unknown = focus(Err(Refused("no answer".into())), Ok(Some([0.0, 550.0, 390.0, 294.0])));
        assert!(unknown.may_type(&p).unwrap_err().0.contains("Can't tell"));
        assert!(unknown.may_touch(&p, &[above], no_screen).is_ok() && unknown.may_touch(&p, &[key], no_screen).is_err());
        // An agent's look at a phone that doesn't answer can't tell (nothing listens on port 1).
        let g = crate::grants::Grant::for_test(None);
        let looked = crate::grants::ACTOR.sync_scope(crate::grants::Actor::Agent(g), || Focus::of(&p));
        assert!(looked.secure.is_err() && looked.may_type(&p).is_err());
        assert!(matches!(Focus::of(&p).secure, Ok(false)), "the owner's own tools aren't limited");
        // Its buttons and return aren't keys that type.
        assert!(is_button("Home") && is_button("volume-up") && is_button("volume_down"));
        assert!(!is_button("return") && !is_button("space") && !is_button("backspace"));
        assert!(is_return("Return") && is_return("enter") && !is_return("space"));
    }

    #[test]
    fn snapshot_refs_point_into_their_own_snapshot() {
        let (a, b) = (new_snap_id(), new_snap_id());
        assert_ne!(a, b);
        assert_eq!(a.len(), PHONE_SNAP.len() + 12, "48 random bits: {a}");
        let item = |x: f64| Item { center: (x, x), value: String::new() };
        keep_snapshot(&a, Snap { asker: "g1".into(), phone: "xr".into(), items: vec![item(1.0), item(2.0)], at: Instant::now() });
        keep_snapshot(&b, Snap { asker: "g2".into(), phone: "se".into(), items: vec![item(3.0)], at: Instant::now() });
        let (phone, it) = ref_item(&format!("@{a}:e2"), Some("g1")).unwrap();
        assert_eq!((phone.as_str(), it.center), ("xr", (2.0, 2.0)));
        assert!(ref_item(&format!("@{a}:e2"), Some("g2")).is_err(), "another agent's snapshot");
        assert!(ref_item(&format!("@{b}:e2"), Some("g2")).is_err(), "not in it");
        assert_eq!(ref_item(&format!("@{b}:e1"), None).unwrap().0, "se", "the audit log reads any");
        // Many more: the oldest go, a few at a time, not everything at once.
        for _ in 0..KEEP_SNAPSHOTS {
            keep_snapshot(&new_snap_id(), Snap { asker: "g3".into(), phone: "xr".into(), items: vec![], at: Instant::now() });
        }
        assert!(SNAPSHOTS.lock().unwrap().as_ref().unwrap().len() <= KEEP_SNAPSHOTS);
        assert!(ref_item(&format!("@{a}:e1"), Some("g1")).is_err());
    }

    #[test]
    fn agents_stay_out_of_protected_phone_apps() {
        let p = Phone {
            name: "xr".into(),
            port: 1,
            video: 2,
            wifi: false,
            udid: None,
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
