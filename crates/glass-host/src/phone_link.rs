//! Keeps the link to each phone Glass manages (a `@udid` in its settings entry) up, so
//! nobody has to: forwards its WebDriverAgent ports to this computer's loopback (over USB
//! through usbmuxd, or over Wi-Fi through Apple's paired, encrypted device tunnel), and
//! starts WebDriverAgent on it whenever it isn't answering, again after it stops.
//!
//! What it finds is the phone's state (`state`), which the viewer shows: starting, locked,
//! waiting for the phone to be unlocked or for automation to be allowed on it, offline.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
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

fn set_state(name: &str, state: State) {
    let mut states = STATES.lock().unwrap();
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

fn forget(name: &str) {
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
                    stop.store(true, Ordering::Relaxed);
                    forget(name);
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

/// Whether the phone is locked, from Xcode (None: it can't tell).
fn passcode_required(udid: &str) -> Option<bool> {
    let out = std::env::temp_dir().join(format!("glass-phone-lock-{udid}.json"));
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
    set_state(&name, State::Starting);
    let route = Arc::new(Route::new(&phone));
    for port in [(phone.port, 8100u16), (phone.video, 9100u16)] {
        let (route, stop, name) = (route.clone(), stop.clone(), name.clone());
        let _ = std::thread::Builder::new()
            .name(format!("phone forward {}", port.0))
            .spawn(move || forward(&name, port, route, stop));
    }
    let mut runner: Option<Runner> = None;
    let mut retry = RETRY_FIRST;
    let mut next_start = Instant::now();
    // After the phone's automation prompt went unanswered or was cancelled: since when,
    // and whether the phone has been locked since. It isn't asked again until it's been
    // locked and unlocked (someone picked it up) or a few minutes have passed, so a phone
    // nobody's using isn't nagged.
    let mut asked: Option<(Instant, bool)> = None;
    let xcode = xctestrun(phone.udid.as_deref().unwrap_or_default()).is_some();
    while !stop.load(Ordering::Relaxed) {
        match probe(&phone) {
            Some(locked) => {
                retry = RETRY_FIRST;
                set_state(&name, if locked { State::Locked } else { State::Ready });
            }
            None => {
                let ended = runner.as_mut().is_some_and(|r| !r.running());
                if let Some(r) = runner.as_ref().filter(|_| ended) {
                    // What it said last, when that's something the person can fix.
                    let said = r.said();
                    tracing::info!("phone {name}: WebDriverAgent stopped; starting it again in {} s", retry.as_secs());
                    if said == Some(State::Approve) {
                        asked = Some((Instant::now(), false));
                    }
                    set_state(&name, said.unwrap_or_else(|| State::Offline("WebDriverAgent stopped; starting it again".into())));
                    runner = None;
                    next_start = Instant::now() + retry;
                    retry = (retry * 2).min(RETRY_MOST);
                } else if let Some(r) = runner.as_ref() {
                    if let Some(s) = r.said() {
                        set_state(&name, s);
                    }
                } else if Instant::now() >= next_start {
                    // Xcode can't start it on a locked phone (and asks to allow automation
                    // when it's unlocked): wait for that rather than trying.
                    let locked = if xcode { passcode_required(phone.udid.as_deref().unwrap_or_default()) } else { None };
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
                        set_state(&name, State::Unlock);
                        std::thread::sleep(Duration::from_secs(3));
                        continue;
                    }
                    match Runner::start(&phone) {
                        Ok(r) => {
                            set_state(&name, State::Starting);
                            runner = Some(r);
                        }
                        Err(why) => {
                            set_state(&name, State::Offline(why));
                            next_start = Instant::now() + RETRY_MOST;
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    drop(runner);
}

/// Whether WebDriverAgent answers through the forward, and then whether the phone is
/// locked (None: not answering).
fn probe(phone: &Phone) -> Option<bool> {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(2)))
            .timeout_global(Some(Duration::from_secs(6)))
            .build()
            .into()
    });
    let url = |path: &str| format!("http://127.0.0.1:{}{path}", phone.port);
    let ok = agent.get(url("/status")).call().ok()?.status().is_success();
    if !ok {
        return None;
    }
    let locked = agent
        .get(url("/wda/locked"))
        .call()
        .ok()
        .and_then(|r| r.into_body().read_json::<serde_json::Value>().ok())
        .and_then(|v| v["value"].as_bool())
        .unwrap_or(false);
    Some(locked)
}

// ---- Forwarding ----

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

/// A connection to the phone, readable and writable from two threads.
trait Pipe: Read + Write + Send {
    fn split(&self) -> std::io::Result<Box<dyn Pipe>>;
    fn close(&self);
}

impl Pipe for TcpStream {
    fn split(&self) -> std::io::Result<Box<dyn Pipe>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn close(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

#[cfg(unix)]
impl Pipe for std::os::unix::net::UnixStream {
    fn split(&self) -> std::io::Result<Box<dyn Pipe>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn close(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

/// Listens on loopback `local` and carries each connection to the phone's `remote` port.
fn forward(name: &str, (local, remote): (u16, u16), route: Arc<Route>, stop: Arc<AtomicBool>) {
    let mut said = false;
    let listener = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match TcpListener::bind(("127.0.0.1", local)) {
            Ok(l) => break l,
            Err(e) => {
                if !said {
                    tracing::warn!("phone {name}: can't listen on port {local} ({e}); is something else forwarding it? Trying again.");
                    said = true;
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    };
    let _ = listener.set_nonblocking(true);
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((client, _)) => {
                let _ = client.set_nonblocking(false);
                let route = route.clone();
                let _ = std::thread::Builder::new().name(format!("phone pipe {local}")).spawn(move || {
                    if let Ok(phone) = route.connect(remote) {
                        join(client, phone);
                    }
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Copies both ways until either side closes.
fn join(client: TcpStream, phone: Box<dyn Pipe>) {
    let _ = client.set_nodelay(true);
    let (Ok(mut client_read), Ok(mut phone_write)) = (client.try_clone(), phone.split()) else {
        return;
    };
    let mut phone_read = phone;
    let mut client_write = client;
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut client_read, &mut phone_write);
        phone_write.close();
    });
    let _ = std::io::copy(&mut phone_read, &mut client_write);
    let _ = client_write.shutdown(Shutdown::Both);
    let _ = up.join();
}

/// The device tunnel's address for a phone paired over the network (Xcode's CoreDevice).
fn tunnel_address(udid: &str) -> Result<String, String> {
    let out = std::env::temp_dir().join(format!("glass-phone-{udid}.json"));
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

    #[cfg(unix)]
    pub(super) fn connect(udid: &str, port: u16) -> Result<UnixStream, String> {
        let (_, list) = request("<key>MessageType</key><string>ListDevices</string>")?;
        let id = device_id(&list, udid).ok_or("it isn't plugged in by USB")?;
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
}

impl Runner {
    /// Starts WebDriverAgent: with Xcode when there's a build for this phone in
    /// `<config>/wda/<udid>/` (iOS 15 and later, USB or Wi-Fi), else with go-ios (`ios` on
    /// the PATH; older iOS, over USB).
    fn start(phone: &Phone) -> Result<Runner, String> {
        let udid = phone.udid.as_deref().unwrap_or_default();
        let mut cmd = if let Some(xctestrun) = xctestrun(udid) {
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
        Ok(Runner { child, said })
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
    }
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
    fn finds_webdriveragent_among_the_apps() {
        let apps = r#"[{"CFBundleIdentifier":"com.apple.Maps"},{"CFBundleIdentifier":"dev.glassdesk.WebDriverAgentRunner.xctrunner"}]"#;
        assert_eq!(find_wda_bundle(apps).as_deref(), Some("dev.glassdesk.WebDriverAgentRunner.xctrunner"));
        assert_eq!(find_wda_bundle("com.apple.Maps"), None);
    }
}
