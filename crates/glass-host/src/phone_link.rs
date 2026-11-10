//! Keeps the link to each phone Glass manages (a `@udid` in its settings entry) up, so
//! nobody has to: reaches WebDriverAgent on it (over USB through usbmuxd, or over Wi-Fi
//! through Apple's paired, encrypted device tunnel), and starts WebDriverAgent on it
//! whenever it isn't answering, again after it stops.
//!
//! Glass opens each connection to the phone itself (`open`, `request`) rather than
//! forwarding its ports to this computer's loopback: WebDriverAgent has no sign-in, so a
//! port listening here would let any program on this computer, or a web page in one of its
//! browsers, drive the phone and see its screen without Glass's grants, checks and audit.
//!
//! What it finds is the phone's state (`state`), which the viewer shows: starting, locked,
//! waiting for the phone to be unlocked or for automation to be allowed on it, offline.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::phone::Phone;

/// Where a managed phone is.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Starting WebDriverAgent (or waiting for it to answer).
    Starting,
    /// Working, but the phone is locked: what Glass sends waits for it to be unlocked.
    Locked,
    /// WebDriverAgent can't start until the phone is unlocked.
    Unlock,
    /// The phone is asking to allow UI automation (its passcode or Face ID).
    Approve,
    /// Can't be reached, and why.
    Offline(String),
    Ready,
}

impl State {
    /// For the viewer: a word for the state and, when the person can do something about it,
    /// what.
    pub fn describe(&self) -> (&'static str, Option<String>) {
        match self {
            State::Starting => ("starting", None),
            State::Locked => ("locked", Some("Unlock the iPhone to use it.".into())),
            State::Unlock => ("unlock", Some("Unlock the iPhone so Glass can start on it.".into())),
            State::Approve => ("approve", Some("Unlock the iPhone, then allow UI automation when it asks (passcode or Face ID).".into())),
            State::Offline(why) => ("offline", Some(why.clone())),
            State::Ready => ("ready", None),
        }
    }
}

static STATES: Mutex<Option<HashMap<String, State>>> = Mutex::new(None);
/// Bumped whenever a state changes, so the viewer's list can be sent again.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The state of a phone Glass manages (None for one it doesn't).
pub fn state(name: &str) -> Option<State> {
    STATES.lock().unwrap().as_ref()?.get(name).cloned()
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Sets a phone's state, unless its supervisor was told to stop (the phone was taken out of
/// the settings): a check still running then mustn't bring back a state that was forgotten.
/// `stop` is read under the lock `forget` takes, after it's set, so one or the other wins.
fn set_state(name: &str, state: State, stop: &AtomicBool) {
    let mut states = STATES.lock().unwrap();
    if stop.load(Ordering::SeqCst) {
        return;
    }
    let states = states.get_or_insert_with(HashMap::new);
    if states.get(name) != Some(&state) {
        tracing::info!("phone {name}: {}", match &state {
            State::Offline(why) => format!("offline: {why}"),
            s => s.describe().0.to_string(),
        });
        states.insert(name.to_string(), state);
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Stops a phone's supervisor and forgets its state, for good.
fn retire(name: &str, stop: &AtomicBool) {
    stop.store(true, Ordering::SeqCst);
    if let Some(states) = STATES.lock().unwrap().as_mut()
        && states.remove(name).is_some()
    {
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Starts looking after the managed phones in the settings, now and as they change.
pub fn start() {
    let _ = std::thread::Builder::new().name("phone links".into()).spawn(|| {
        let mut running: HashMap<String, (Phone, Arc<AtomicBool>)> = HashMap::new();
        loop {
            let wanted: Vec<Phone> = crate::phone::configured().into_iter().filter(|p| p.udid.is_some()).collect();
            running.retain(|name, (phone, stop)| {
                let keep = wanted.contains(phone);
                if !keep {
                    retire(name, stop);
                }
                keep
            });
            for phone in wanted {
                if running.contains_key(&phone.name) {
                    continue;
                }
                let stop = Arc::new(AtomicBool::new(false));
                running.insert(phone.name.clone(), (phone.clone(), stop.clone()));
                let _ = std::thread::Builder::new()
                    .name(format!("phone link {}", phone.name))
                    .spawn(move || supervise(phone, stop));
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

// ---- Looking after one phone ----

/// How long to wait before starting WebDriverAgent again after it stops: doubling from
/// 5 s to 60 s while it keeps failing, back to 5 s once it has worked.
const RETRY_FIRST: Duration = Duration::from_secs(5);
const RETRY_MOST: Duration = Duration::from_secs(60);
/// How long an unanswered automation prompt keeps Glass from asking again, unless the phone
/// is picked up (locked, then unlocked) first.
const ASK_AGAIN: Duration = Duration::from_secs(300);

/// How long WebDriverAgent may take to answer after it's started before it's started again
/// (xcodebuild can hang, and would say "starting" for ever).
const START_LIMIT: Duration = Duration::from_secs(180);

/// A file of its own for devicectl to write into: two threads may ask about one phone at once.
fn temp_json(what: &str, udid: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("glass-phone-{what}-{udid}-{}-{n}.json", std::process::id()))
}

/// Whether the phone is locked, from Xcode (None: it can't tell).
fn passcode_required(udid: &str) -> Option<bool> {
    let out = temp_json("lock", udid);
    let ok = Command::new("xcrun")
        .args(["devicectl", "device", "info", "lockState", "--quiet", "--device", udid, "--json-output"])
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?
        .success();
    let json = std::fs::read_to_string(&out).ok();
    let _ = std::fs::remove_file(&out);
    let v: serde_json::Value = serde_json::from_str(&json?).ok()?;
    ok.then(|| v["result"]["passcodeRequired"].as_bool()).flatten()
}

fn supervise(phone: Phone, stop: Arc<AtomicBool>) {
    let name = phone.name.clone();
    let set = |state: State| set_state(&name, state, &stop);
    set(State::Starting);
    let udid = phone.udid.clone().unwrap_or_default();
    let route = route(&phone);
    let mut runner: Option<Runner> = None;
    let mut retry = RETRY_FIRST;
    let mut next_start = Instant::now();
    // After the phone's automation prompt went unanswered or was cancelled: since when,
    // and whether the phone has been locked since. It isn't asked again until it's been
    // locked and unlocked (someone picked it up) or a few minutes have passed, so a phone
    // nobody's using isn't nagged.
    let mut asked: Option<(Instant, bool)> = None;
    while !stop.load(Ordering::Relaxed) {
        match probe(&phone) {
            Some(locked) => {
                retry = RETRY_FIRST;
                if let Some(r) = runner.as_mut() {
                    r.answered = true;
                }
                set(if locked { State::Locked } else { State::Ready });
            }
            None => {
                let ended = runner.as_mut().is_some_and(|r| !r.running());
                let stuck = runner.as_ref().is_some_and(|r| !r.answered && r.started.elapsed() >= START_LIMIT);
                if let Some(r) = runner.as_ref().filter(|_| ended || stuck) {
                    // What it said last, when that's something the person can fix; or
                    // that the phone's gone.
                    let said = r.said();
                    if said == Some(State::Approve) {
                        asked = Some((Instant::now(), false));
                    }
                    let state = match route.present() {
                        Err(gone) => State::Offline(gone),
                        Ok(()) if stuck => State::Offline(format!("WebDriverAgent didn't answer within {} minutes of starting; starting it again", START_LIMIT.as_secs() / 60)),
                        Ok(()) => said.unwrap_or_else(|| State::Offline("WebDriverAgent stopped; starting it again".into())),
                    };
                    tracing::info!("phone {name}: WebDriverAgent {}; starting it again in {} s", if stuck { "didn't answer in time" } else { "stopped" }, retry.as_secs());
                    set(state);
                    runner = None;
                    next_start = Instant::now() + retry;
                    retry = (retry * 2).min(RETRY_MOST);
                } else if let Some(r) = runner.as_ref() {
                    if let Some(s) = r.said() {
                        set(s);
                    }
                } else if Instant::now() >= next_start {
                    match route.present() {
                        // Unplugged or out of range: nothing to start until it's back.
                        Err(gone) => {
                            set(State::Offline(gone));
                            next_start = Instant::now() + RETRY_FIRST;
                        }
                        Ok(()) => {
                            // The build is looked for each time (one may be added later),
                            // and the same one is checked and started.
                            let build = xctestrun(&udid);
                            // Xcode can't start it on a locked phone (and asks to allow
                            // automation when it's unlocked): wait for that rather than trying.
                            let locked = if build.is_some() { passcode_required(&udid) } else { None };
                            if let Some((at, seen_locked)) = asked.as_mut() {
                                *seen_locked |= locked == Some(true);
                                let picked_up = *seen_locked && locked == Some(false);
                                if !picked_up && at.elapsed() < ASK_AGAIN {
                                    std::thread::sleep(Duration::from_secs(3));
                                    continue;
                                }
                                asked = None;
                            }
                            if locked == Some(true) {
                                set(State::Unlock);
                                std::thread::sleep(Duration::from_secs(3));
                                continue;
                            }
                            match Runner::start(&phone, build) {
                                Ok(r) => {
                                    set(State::Starting);
                                    runner = Some(r);
                                }
                                Err(why) => {
                                    set(State::Offline(why));
                                    next_start = Instant::now() + RETRY_MOST;
                                }
                            }
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    drop(runner);
}

/// Whether WebDriverAgent answers, and then whether the phone is locked (None: not
/// answering).
fn probe(phone: &Phone) -> Option<bool> {
    let get = |path: &str| request(phone, "GET", path, None, Duration::from_secs(6)).ok().filter(|r| (200..300).contains(&r.status));
    get("/status")?;
    let locked = get("/wda/locked")
        .and_then(|r| serde_json::from_slice::<serde_json::Value>(&r.body).ok())
        .and_then(|v| v["value"].as_bool())
        .unwrap_or(false);
    Some(locked)
}

// ---- Reaching the phone ----

/// WebDriverAgent's ports on the phone itself: commands, and its live picture (MJPEG).
pub(crate) const COMMANDS: u16 = 8100;
pub(crate) const PICTURE: u16 = 9100;

/// The way to each managed phone, kept for the tunnel address it remembers.
static ROUTES: Mutex<Option<HashMap<(String, bool), Arc<Route>>>> = Mutex::new(None);

fn route(phone: &Phone) -> Arc<Route> {
    let key = (phone.udid.clone().unwrap_or_default(), phone.wifi);
    ROUTES.lock().unwrap().get_or_insert_with(HashMap::new).entry(key).or_insert_with(|| Arc::new(Route::new(phone))).clone()
}

/// A connection straight to one of a managed phone's ports (`COMMANDS`, `PICTURE`), timing
/// out reads and writes after `timeout`.
pub(crate) fn open(phone: &Phone, port: u16, timeout: Duration) -> Result<Box<dyn Pipe>, String> {
    let pipe = route(phone).connect(port)?;
    pipe.set_timeout(Some(timeout));
    Ok(pipe)
}

/// What WebDriverAgent answered.
#[derive(Debug)]
pub(crate) struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

/// The most a reply may be (a screenshot of a big screen as base64, a deep tree).
const REPLY_MOST: usize = 64 << 20;

/// One HTTP request to WebDriverAgent on a managed phone, on a connection of its own.
pub(crate) fn request(phone: &Phone, method: &str, path: &str, body: Option<&[u8]>, timeout: Duration) -> Result<Reply, String> {
    let mut pipe = open(phone, COMMANDS, timeout)?;
    let body = body.unwrap_or_default();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{COMMANDS}\r\nConnection: close\r\n");
    if method != "GET" {
        req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    pipe.write_all(req.as_bytes()).and_then(|_| pipe.write_all(body)).map_err(|e| format!("couldn't send to it ({e})"))?;
    read_reply(&mut BufReader::new(pipe))
}

/// The longest line of a reply's head (its status, a header, a chunk's size), and the most
/// headers: a reply past them is refused rather than read without end.
const LINE_MOST: u64 = 8 << 10;
const HEADERS_MOST: usize = 100;

/// One line, at most LINE_MOST long.
fn read_line(r: &mut impl BufRead, line: &mut String) -> Result<usize, String> {
    line.clear();
    let n = r.take(LINE_MOST).read_line(line).map_err(|e| format!("its reply broke off ({e})"))?;
    if n as u64 == LINE_MOST && !line.ends_with('\n') {
        return Err("a line of its reply was too long".into());
    }
    Ok(n)
}

/// An HTTP/1.1 response: its status and body (by its length, in chunks, or to the end).
fn read_reply(r: &mut impl BufRead) -> Result<Reply, String> {
    let bad = |e: std::io::Error| format!("its reply broke off ({e})");
    let too_big = || "its reply was too big".to_string();
    let mut line = String::new();
    read_line(r, &mut line)?;
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("an odd reply: {:?}", line.trim()))?;
    let (mut length, mut chunked) = (None, false);
    for n in 0.. {
        if read_line(r, &mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if n >= HEADERS_MOST {
            return Err("its reply had too many headers".into());
        }
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            if k.eq_ignore_ascii_case("content-length") {
                length = v.parse::<usize>().ok();
            } else if k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            read_line(r, &mut line)?;
            let n = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16).map_err(|_| "an odd chunk".to_string())?;
            if n == 0 {
                break;
            }
            let end = body.len().checked_add(n).filter(|&e| e <= REPLY_MOST).ok_or_else(too_big)?;
            let at = body.len();
            body.resize(end, 0);
            r.read_exact(&mut body[at..]).map_err(bad)?;
            read_line(r, &mut line)?;
        }
    } else if let Some(n) = length {
        if n > REPLY_MOST {
            return Err(too_big());
        }
        body.resize(n, 0);
        r.read_exact(&mut body).map_err(bad)?;
    } else {
        r.take(REPLY_MOST as u64 + 1).read_to_end(&mut body).map_err(bad)?;
        if body.len() > REPLY_MOST {
            return Err(too_big());
        }
    }
    Ok(Reply { status, body })
}

/// How to reach a port on the phone: usbmuxd over USB, or the device tunnel's address over
/// Wi-Fi (looked up again when it stops working: it changes when the phone reconnects).
struct Route {
    udid: String,
    wifi: bool,
    tunnel: Mutex<Option<String>>,
}

impl Route {
    fn new(phone: &Phone) -> Self {
        Route { udid: phone.udid.clone().unwrap_or_default(), wifi: phone.wifi, tunnel: Mutex::new(None) }
    }

    /// Whether the phone is there to be reached: plugged in (usbmuxd lists it), or on the
    /// network with its tunnel up. Why not, when it isn't.
    fn present(&self) -> Result<(), String> {
        if !self.wifi {
            return usbmux::device(&self.udid).map(|_| ());
        }
        let address = tunnel_address(&self.udid).map_err(|_| "It isn't on this network (or isn't paired with this Mac).".to_string())?;
        *self.tunnel.lock().unwrap() = Some(address);
        Ok(())
    }

    fn connect(&self, port: u16) -> Result<Box<dyn Pipe>, String> {
        if !self.wifi {
            return usbmux::connect(&self.udid, port).map(|s| Box::new(s) as Box<dyn Pipe>);
        }
        for fresh in [false, true] {
            let known = self.tunnel.lock().unwrap().clone();
            let address = match known {
                Some(a) if !fresh => a,
                _ => {
                    let a = tunnel_address(&self.udid)?;
                    *self.tunnel.lock().unwrap() = Some(a.clone());
                    a
                }
            };
            let target = format!("[{address}]:{port}");
            if let Ok(addr) = target.parse()
                && let Ok(s) = TcpStream::connect_timeout(&addr, Duration::from_secs(4))
            {
                return Ok(Box::new(s));
            }
        }
        Err("can't reach it over Wi-Fi".into())
    }
}

/// A connection to the phone (or, for one Glass doesn't manage, to the port it's forwarded
/// to).
pub(crate) trait Pipe: Read + Write + Send {
    fn set_timeout(&self, t: Option<Duration>);
}

impl Pipe for TcpStream {
    fn set_timeout(&self, t: Option<Duration>) {
        let _ = self.set_read_timeout(t);
        let _ = self.set_write_timeout(t);
    }
}

#[cfg(unix)]
impl Pipe for std::os::unix::net::UnixStream {
    fn set_timeout(&self, t: Option<Duration>) {
        let _ = self.set_read_timeout(t);
        let _ = self.set_write_timeout(t);
    }
}

/// The device tunnel's address for a phone paired over the network (Xcode's CoreDevice).
fn tunnel_address(udid: &str) -> Result<String, String> {
    let out = temp_json("details", udid);
    let ran = Command::new("xcrun")
        .args(["devicectl", "device", "info", "details", "--quiet", "--device", udid, "--json-output"])
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("can't run Xcode's devicectl ({e})"))?;
    let json = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
    match v["result"]["connectionProperties"]["tunnelIPAddress"].as_str() {
        Some(a) if ran.success() => Ok(a.to_string()),
        _ => Err("it isn't on this network (or isn't paired with this Mac)".into()),
    }
}

/// usbmuxd: the Mac's daemon for talking to iPhones over USB. One request per connection
/// (plist messages); after a Connect is accepted, the connection is the phone's port.
mod usbmux {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;

    #[cfg(unix)]
    fn request(body: &str) -> Result<(UnixStream, String), String> {
        let mut s = UnixStream::connect("/var/run/usbmuxd").map_err(|e| format!("usbmuxd isn't answering ({e})"))?;
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>{body}<key>ClientVersionString</key><string>glass</string><key>ProgName</key><string>glass</string></dict></plist>\n"
        );
        let mut msg = Vec::with_capacity(16 + plist.len());
        for n in [16 + plist.len() as u32, 1, 8, 1] {
            msg.extend_from_slice(&n.to_le_bytes());
        }
        msg.extend_from_slice(plist.as_bytes());
        s.write_all(&msg).map_err(|e| e.to_string())?;
        let mut head = [0u8; 16];
        s.read_exact(&mut head).map_err(|e| format!("usbmuxd: {e}"))?;
        let len = u32::from_le_bytes(head[0..4].try_into().unwrap()) as usize;
        if !(16..=4 << 20).contains(&len) {
            return Err("usbmuxd: odd reply".into());
        }
        let mut reply = vec![0u8; len - 16];
        s.read_exact(&mut reply).map_err(|e| format!("usbmuxd: {e}"))?;
        let _ = s.set_read_timeout(None);
        Ok((s, String::from_utf8_lossy(&reply).into_owned()))
    }

    /// The usbmuxd id of the phone with this UDID plugged in by USB.
    pub(super) fn device_id(list: &str, udid: &str) -> Option<u64> {
        let want = udid.replace('-', "").to_ascii_lowercase();
        // Each attached device: ...<key>DeviceID</key><integer>N</integer>...<key>SerialNumber</key><string>UDID</string>
        let mut rest = list;
        while let Some(at) = rest.find("<key>SerialNumber</key>") {
            let (before, after) = rest.split_at(at);
            let serial = after
                .split("<string>")
                .nth(1)
                .and_then(|s| s.split("</string>").next())
                .unwrap_or("");
            let entry = before.rsplit("<key>MessageType</key>").next().unwrap_or(before);
            let usb = !entry.contains("<string>Network</string>");
            if usb && serial.replace('-', "").to_ascii_lowercase() == want {
                let id = entry
                    .rsplit("<key>DeviceID</key>")
                    .next()
                    .and_then(|s| s.split("<integer>").nth(1))
                    .and_then(|s| s.split("</integer>").next())
                    .and_then(|s| s.trim().parse().ok());
                if id.is_some() {
                    return id;
                }
            }
            rest = &after["<key>SerialNumber</key>".len()..];
        }
        None
    }

    /// The usbmuxd id of the phone, when it's plugged in.
    #[cfg(unix)]
    pub(super) fn device(udid: &str) -> Result<u64, String> {
        let (_, list) = request("<key>MessageType</key><string>ListDevices</string>")?;
        device_id(&list, udid).ok_or_else(|| "It isn't plugged in by USB (or isn't trusted on this Mac).".into())
    }

    #[cfg(not(unix))]
    pub(super) fn device(_udid: &str) -> Result<u64, String> {
        Err("phones over USB are only looked after on a Mac".into())
    }

    #[cfg(unix)]
    pub(super) fn connect(udid: &str, port: u16) -> Result<UnixStream, String> {
        let id = device(udid)?;
        let (s, reply) = request(&format!(
            "<key>MessageType</key><string>Connect</string><key>DeviceID</key><integer>{id}</integer><key>PortNumber</key><integer>{}</integer>",
            port.to_be()
        ))?;
        if reply.contains("<key>Number</key>") && !reply.contains("<integer>0</integer>") {
            return Err(format!("nothing is listening on its port {port}"));
        }
        Ok(s)
    }

    #[cfg(not(unix))]
    pub(super) fn connect(_udid: &str, _port: u16) -> Result<TcpStream, String> {
        Err("phones over USB are only looked after on a Mac".into())
    }
}

// ---- Starting WebDriverAgent ----

/// WebDriverAgent running on the phone, through the tool that started it (and stopped
/// with it).
struct Runner {
    child: Child,
    /// The last thing it said that the person can fix.
    said: Arc<Mutex<Option<State>>>,
    started: Instant,
    /// WebDriverAgent has answered since.
    answered: bool,
    /// Where its process id is kept, so one left running by an earlier Glass (one that
    /// crashed or was killed) is stopped before another starts.
    pid_file: std::path::PathBuf,
}

impl Runner {
    /// Starts WebDriverAgent: with Xcode when there's a build for this phone (`build`, from
    /// `<config>/wda/<udid>/`; iOS 15 and later, USB or Wi-Fi), else with go-ios (`ios` on
    /// the PATH; older iOS, over USB).
    fn start(phone: &Phone, build: Option<std::path::PathBuf>) -> Result<Runner, String> {
        let udid = phone.udid.as_deref().unwrap_or_default();
        let pid_file = wda_dir(udid).join("runner.pid");
        stop_left_over(&pid_file, udid);
        let mut cmd = if let Some(xctestrun) = build {
            let mut c = Command::new("xcrun");
            c.args(["xcodebuild", "test-without-building", "-xctestrun"]).arg(xctestrun).args(["-destination", &format!("id={udid}")]);
            c
        } else if let Some(ios) = go_ios() {
            if phone.wifi {
                return Err(format!("no WebDriverAgent build for it in {}", wda_dir(udid).display()));
            }
            let bundle = wda_bundle(&ios, udid).ok_or("WebDriverAgent isn't installed on it")?;
            let mut c = Command::new(ios);
            c.args(["runwda", "--bundleid", &bundle, "--testrunnerbundleid", &bundle, "--xctestconfig", "WebDriverAgentRunner.xctest", "--udid", udid]);
            c
        } else {
            return Err(format!("no WebDriverAgent build for it in {}, and go-ios isn't installed", wda_dir(udid).display()));
        };
        tracing::info!("phone {}: starting WebDriverAgent", phone.name);
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("couldn't start WebDriverAgent ({e})"))?;
        if std::fs::create_dir_all(wda_dir(udid)).is_ok() {
            let _ = std::fs::write(&pid_file, child.id().to_string());
        }
        let said = Arc::new(Mutex::new(None));
        for out in [child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>)].into_iter().flatten() {
            let (said, name) = (said.clone(), phone.name.clone());
            let _ = std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if let Some(s) = heard(&line) {
                        tracing::debug!("phone {name}: WebDriverAgent: {line}");
                        *said.lock().unwrap() = Some(s);
                    }
                }
            });
        }
        Ok(Runner { child, said, started: Instant::now(), answered: false, pid_file })
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn said(&self) -> Option<State> {
        self.said.lock().unwrap().clone()
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Only its own: another runner's may be there now.
        if std::fs::read_to_string(&self.pid_file).is_ok_and(|s| s.trim() == self.child.id().to_string()) {
            let _ = std::fs::remove_file(&self.pid_file);
        }
    }
}

/// Stops the xcodebuild or go-ios an earlier Glass started for this phone and left running
/// (it ended without stopping it), so two don't fight over the phone.
fn stop_left_over(pid_file: &std::path::Path, udid: &str) {
    let Some(pid) = std::fs::read_to_string(pid_file).ok().and_then(|s| s.trim().parse::<u32>().ok()) else { return };
    let _ = std::fs::remove_file(pid_file);
    // Only if that process is still one running WebDriverAgent on this phone: its id may
    // have been reused since, by anything (a developer's own xcodebuild, say).
    let args = Command::new("ps").args(["-ww", "-o", "args=", "-p", &pid.to_string()]).output().ok();
    let args = args.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if is_runner_for(&args, udid) {
        tracing::info!("stopping WebDriverAgent's runner left over from before (process {pid})");
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

/// Whether a process's command line is the one Glass starts WebDriverAgent on this phone with.
fn is_runner_for(args: &str, udid: &str) -> bool {
    let program = args.split_whitespace().next().unwrap_or("");
    let program = program.rsplit('/').next().unwrap_or(program);
    let xcode = matches!(program, "xcodebuild" | "xcrun") && args.contains("test-without-building") && args.contains(".xctestrun");
    let go_ios = program == "ios" && args.contains(" runwda ") && args.contains("WebDriverAgentRunner.xctest");
    !udid.is_empty() && args.contains(udid) && (xcode || go_ios)
}

/// What a line from xcodebuild or go-ios means for the person, if anything.
fn heard(line: &str) -> Option<State> {
    let l = line.to_ascii_lowercase();
    if l.contains("is locked") || (l.contains("unlock") && l.contains("to continue")) {
        Some(State::Unlock)
    } else if l.contains("enabling automation mode") || l.contains("authentication canceled") || l.contains("ui automation") {
        Some(State::Approve)
    } else if l.contains("serverurlhere") {
        Some(State::Starting)
    } else if l.contains("developer mode") && l.contains("disabled") {
        Some(State::Offline("Turn on Developer Mode on the iPhone (Settings → Privacy & Security).".into()))
    } else {
        None
    }
}

fn wda_dir(udid: &str) -> std::path::PathBuf {
    crate::config_dir().join("wda").join(udid)
}

fn xctestrun(udid: &str) -> Option<std::path::PathBuf> {
    std::fs::read_dir(wda_dir(udid))
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "xctestrun"))
}

fn go_ios() -> Option<std::path::PathBuf> {
    let mut places: Vec<std::path::PathBuf> = std::env::var_os("GLASS_GO_IOS").map(Into::into).into_iter().collect();
    places.push(crate::config_dir().join("tools").join("ios"));
    places.extend(std::env::var_os("PATH").iter().flat_map(std::env::split_paths).map(|d| d.join("ios")));
    places.extend(["/opt/homebrew/bin/ios", "/usr/local/bin/ios"].map(Into::into));
    places.into_iter().find(|p| p.is_file())
}

/// The bundle id of WebDriverAgent's runner installed on the phone.
fn wda_bundle(ios: &std::path::Path, udid: &str) -> Option<String> {
    let out = Command::new(ios).args(["apps", "--udid", udid]).stderr(Stdio::null()).output().ok()?;
    find_wda_bundle(&String::from_utf8_lossy(&out.stdout))
}

fn find_wda_bundle(apps: &str) -> Option<String> {
    apps.split(|c: char| c == '"' || c.is_whitespace() || c == ',' || c == '[' || c == ']')
        .find(|s| s.ends_with(".xctrunner") && s.to_ascii_lowercase().contains("webdriveragent"))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_usb_phone_in_usbmuxds_list() {
        let list = "<plist><dict><key>DeviceList</key><array>\
            <dict><key>DeviceID</key><integer>3</integer><key>MessageType</key><string>Attached</string><key>Properties</key><dict><key>ConnectionType</key><string>Network</string><key>DeviceID</key><integer>3</integer><key>SerialNumber</key><string>00008020-000649323451002E</string></dict></dict>\
            <dict><key>DeviceID</key><integer>7</integer><key>MessageType</key><string>Attached</string><key>Properties</key><dict><key>ConnectionSpeed</key><integer>480000000</integer><key>ConnectionType</key><string>USB</string><key>DeviceID</key><integer>7</integer><key>SerialNumber</key><string>00008020000649323451002E</string></dict></dict>\
            </array></dict></plist>";
        assert_eq!(usbmux::device_id(list, "00008020-000649323451002E"), Some(7), "the USB one, dashes or not");
        assert_eq!(usbmux::device_id(list, "00008120-00040D520C38C01E"), None);
    }

    #[test]
    fn what_the_runner_says_becomes_a_state() {
        assert_eq!(heard("Error Domain=com.apple.dt.deviceprep Code=-3 \"Unlock Nathan’s iPhone to Continue\""), Some(State::Unlock));
        assert_eq!(heard("The test runner failed to initialize for UI testing. (Underlying Error: Timed out while enabling automation mode.)"), Some(State::Approve));
        assert_eq!(heard("(Underlying Error: Authentication canceled.)"), Some(State::Approve));
        assert_eq!(heard("Testing started"), None);
    }

    #[test]
    fn a_phone_taken_out_of_the_settings_stays_forgotten() {
        let stop = AtomicBool::new(false);
        set_state("gone-xr", State::Ready, &stop);
        assert_eq!(state("gone-xr"), Some(State::Ready));
        retire("gone-xr", &stop);
        assert_eq!(state("gone-xr"), None);
        // A check that was still running finishes after: it doesn't bring the phone back.
        set_state("gone-xr", State::Offline("WebDriverAgent stopped".into()), &stop);
        set_state("gone-xr", State::Starting, &stop);
        assert_eq!(state("gone-xr"), None);
        // Put back in the settings, its new supervisor's state is shown.
        let again = AtomicBool::new(false);
        set_state("gone-xr", State::Starting, &again);
        assert_eq!(state("gone-xr"), Some(State::Starting));
        retire("gone-xr", &again);
    }

    #[test]
    fn replies_are_read_by_length_in_chunks_or_to_the_end() {
        let read = |raw: &str| read_reply(&mut BufReader::new(raw.as_bytes()));
        let r = read("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nServer: WDA\r\n\r\nhello and more").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"hello"[..]));
        let r = read("HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (404, &b"abcde"[..]));
        let r = read("HTTP/1.0 200 OK\r\n\r\n{\"value\":1}").unwrap();
        assert_eq!(r.body, b"{\"value\":1}");
        assert!(read("garbage").is_err());
        assert!(read("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort").is_err(), "cut short");
        assert!(read(&format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", REPLY_MOST + 1)).is_err());
    }

    #[test]
    fn temp_files_differ_and_left_over_runners_are_told_apart() {
        assert_ne!(temp_json("details", "u"), temp_json("details", "u"));
        let u = "00008020-000649323451002E";
        let xcode = format!("/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild test-without-building -xctestrun /x/wda/{u}/WDA.xctestrun -destination id={u}");
        assert!(is_runner_for(&xcode, u));
        let ios = format!("/opt/homebrew/bin/ios runwda --bundleid a.WebDriverAgentRunner.xctrunner --testrunnerbundleid a.WebDriverAgentRunner.xctrunner --xctestconfig WebDriverAgentRunner.xctest --udid {u}");
        assert!(is_runner_for(&ios, u));
        // Another phone's, or a developer's own build: left alone.
        assert!(!is_runner_for(&xcode, "00008120-00040D520C38C01E"));
        assert!(!is_runner_for(&format!("xcodebuild build -scheme App -destination id={u}"), u));
        assert!(!is_runner_for("/usr/bin/login", u) && !is_runner_for("", u) && !is_runner_for(&xcode, ""));
    }

    #[test]
    fn odd_replies_are_refused_not_read_without_end() {
        let read = |raw: String| read_reply(&mut BufReader::new(std::io::Cursor::new(raw.into_bytes())));
        // A chunk's size that would wrap round: refused, not a panic.
        assert!(read("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\nffffffffffffffff\r\n".into()).unwrap_err().contains("too big"));
        assert!(read(format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "a".repeat(LINE_MOST as usize))).unwrap_err().contains("too long"));
        assert!(read(format!("HTTP/1.1 200 OK\r\n{}\r\n", "X: y\r\n".repeat(HEADERS_MOST + 1))).unwrap_err().contains("too many"));
        assert!(read(format!("HTTP/1.1 200 OK\r\n{}Content-Length: 2\r\n\r\nok", "X: y\r\n".repeat(HEADERS_MOST - 1))).is_ok());
    }

    #[test]
    fn finds_webdriveragent_among_the_apps() {
        let apps = r#"[{"CFBundleIdentifier":"com.apple.Maps"},{"CFBundleIdentifier":"dev.glassdesk.WebDriverAgentRunner.xctrunner"}]"#;
        assert_eq!(find_wda_bundle(apps).as_deref(), Some("dev.glassdesk.WebDriverAgentRunner.xctrunner"));
        assert_eq!(find_wda_bundle("com.apple.Maps"), None);
    }
}
