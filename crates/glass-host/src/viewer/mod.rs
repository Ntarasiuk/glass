//! The native viewer: another computer's screen inside the Glass app, without a browser.
//!
//! It speaks the same protocol as the web viewer (QUIC datagrams over WebTransport, see
//! `glass-proto`), so any host works unchanged, but from Rust end to end: datagrams are
//! reassembled and handed to VideoToolbox on the network task, and the decoded IOSurface
//! goes to GPUI's Metal renderer as is. Keys come from a session event tap, so system
//! shortcuts (⌘Tab, ⌘Space, ⌘Q, Mission Control) reach the remote computer.

pub mod annexb;
pub mod assemble;
#[cfg(any(target_os = "macos", windows))]
mod audio;
#[cfg(target_os = "macos")]
pub mod capture;
#[cfg(target_os = "macos")]
pub mod decode;
#[cfg(any(target_os = "macos", windows))]
pub mod gamepad;
#[cfg(windows)]
pub mod win;
#[cfg(windows)]
mod winmf;
pub mod keys;
#[cfg(target_os = "macos")]
pub mod mac;
mod net;

/// What the window needs from this OS, the same on each: pictures, the pointer, the
/// clipboard, keyboard and mouse capture, controllers.
#[cfg(target_os = "macos")]
pub mod sys {
    pub use super::capture::{KeyCapture, RELEASE_HINT, allowed, ask, lock_pointer};
    pub use super::decode::Picture;
    pub use super::gamepad::Pads;
    pub use super::keys::swapped;
    pub use super::mac::{Cursors, clipboard, clipboard_count};
}
#[cfg(windows)]
pub mod sys {
    pub use super::gamepad::Pads;
    pub use super::win::*;
}
pub mod passkey;
pub mod prefs;
mod rtc;
#[cfg(any(target_os = "macos", windows))]
pub mod selftest;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use glass_proto::{CursorPos, InputEvent, MAX_STREAMS};
use serde_json::Value;
use tokio::sync::{Notify, mpsc};

pub use net::{Target, codecs};
#[cfg(windows)]
pub fn winmf_report() -> String {
    winmf::report()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// On its way (or back): what's happening, for the person waiting.
    Connecting(String),
    Live,
    /// Turned away; it won't retry by itself.
    Refused(String),
}

/// The live session's numbers, for the toolbar.
#[derive(Default, Clone, Debug)]
pub struct Live {
    /// The first stream's display size in pixels.
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub rtt_ms: f64,
    /// Last second's.
    pub fps: f64,
    pub mbps: f64,
    /// Capture on the host to decoded here, averaged over the last second.
    pub latency_ms: f64,
    /// The host's adaptive bitrate.
    pub target_mbps: f64,
    /// Every frame decoded this session.
    pub decoded: u64,
    pub dropped: u64,
    /// HEVC frames decoded this session (sharp text's 4:2:2 among them).
    pub hevc_decoded: u64,
    /// Sound packets played this session.
    pub audio_packets: u64,
    frames: u32,
    pub(crate) bytes: u64,
    latency_sum: f64,
    latency_n: u32,
}

impl Live {
    fn note_latency(&mut self, ms: f64) {
        if (0.0..5000.0).contains(&ms) {
            self.latency_sum += ms;
            self.latency_n += 1;
        }
    }
}

/// One display being streamed: which, and its size in pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stream {
    pub display: u32,
    pub width: u32,
    pub height: u32,
    /// Where it sits in the host's desktop (points), for showing several as arranged.
    pub x: f64,
    pub y: f64,
    pub points_width: f64,
    pub points_height: f64,
}

impl Stream {
    fn from_json(v: &Value) -> Self {
        Self {
            display: v["id"].as_u64().unwrap_or(0) as u32,
            width: v["width"].as_u64().unwrap_or(0) as u32,
            height: v["height"].as_u64().unwrap_or(0) as u32,
            x: v["x"].as_f64().unwrap_or(0.0),
            y: v["y"].as_f64().unwrap_or(0.0),
            points_width: v["points_width"].as_f64().unwrap_or(0.0),
            points_height: v["points_height"].as_f64().unwrap_or(0.0),
        }
    }
}

/// A lossless (PNG) picture of part of a stream: sharp text on a still screen. It stays on
/// top until the screen really changes.
#[derive(Clone)]
pub struct Lossless {
    pub stream: usize,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub fw: u32,
    pub fh: u32,
    pub capture_us: u64,
    pub png: Arc<Vec<u8>>,
}

/// What a session shares with the window drawing it.
pub struct Viewer {
    status: Mutex<Status>,
    pub live: Mutex<Live>,
    #[cfg(any(target_os = "macos", windows))]
    pictures: Mutex<[Option<sys::Picture>; MAX_STREAMS]>,
    streams: Mutex<Vec<Stream>>,
    /// Every display the host has (from welcome and layout), as the host describes them.
    displays: Mutex<Vec<Value>>,
    welcome: Mutex<Option<Value>>,
    /// Host messages for the window (chat, clipboard, cursor images, notices…) and the
    /// session's own (`_file`, `_upload`, `_clipboardImage`).
    inbox: Mutex<VecDeque<Value>>,
    /// Where the host says the pointer is, and whether it's showing.
    cursor: Mutex<Option<CursorPos>>,
    lossless: Mutex<Vec<Lossless>>,
    /// A clipboard image from the host, waiting to go on this Mac's clipboard.
    clip_image: Mutex<Option<(String, Vec<u8>)>>,
    /// Play the computer's sound here.
    pub audio_on: AtomicBool,
    /// Video is wanted (the session is on screen).
    video_on: AtomicBool,
    /// The last viewport sent, sent again to each new session.
    viewport: Mutex<Option<Value>>,
    /// A guest pass (from a link), for reconnecting.
    pass: Mutex<Option<String>>,
    changed: Notify,
    /// A change is waiting to be drawn (so a burst of frames wakes the window once).
    dirty: AtomicBool,
    last_tick: Mutex<Option<Instant>>,
}

impl Viewer {
    fn new() -> Self {
        Self {
            status: Mutex::new(Status::Connecting("Connecting…".into())),
            live: Mutex::new(Live::default()),
            #[cfg(any(target_os = "macos", windows))]
            pictures: Mutex::new(Default::default()),
            streams: Mutex::new(Vec::new()),
            displays: Mutex::new(Vec::new()),
            welcome: Mutex::new(None),
            inbox: Mutex::new(VecDeque::new()),
            cursor: Mutex::new(None),
            lossless: Mutex::new(Vec::new()),
            clip_image: Mutex::new(None),
            audio_on: AtomicBool::new(true),
            video_on: AtomicBool::new(true),
            viewport: Mutex::new(None),
            pass: Mutex::new(None),
            changed: Notify::new(),
            dirty: AtomicBool::new(false),
            last_tick: Mutex::new(None),
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn set_status(&self, status: Status) {
        let mut s = self.status.lock().unwrap();
        if *s != status {
            *s = status;
            drop(s);
            self.wake();
        }
    }

    fn wake(&self) {
        if !self.dirty.swap(true, Ordering::AcqRel) {
            self.changed.notify_one();
        }
    }

    /// Waits until there's something new to draw.
    pub async fn changed(&self) {
        self.changed.notified().await;
        self.dirty.store(false, Ordering::Release);
    }

    #[cfg(any(target_os = "macos", windows))]
    fn show(&self, stream: usize, picture: sys::Picture) {
        if let Some(slot) = self.pictures.lock().unwrap().get_mut(stream) {
            *slot = Some(picture);
        }
        let mut live = self.live.lock().unwrap();
        live.frames += 1;
        live.decoded += 1;
        drop(live);
        self.wake();
    }

    /// The newest picture of a stream.
    #[cfg(any(target_os = "macos", windows))]
    pub fn picture(&self, stream: usize) -> Option<sys::Picture> {
        self.pictures.lock().unwrap().get(stream).cloned().flatten()
    }

    pub fn streams(&self) -> Vec<Stream> {
        self.streams.lock().unwrap().clone()
    }

    pub fn displays(&self) -> Vec<Value> {
        self.displays.lock().unwrap().clone()
    }

    /// The host's welcome: its features, permissions and OS.
    pub fn welcome(&self) -> Option<Value> {
        self.welcome.lock().unwrap().clone()
    }

    pub fn cursor(&self) -> Option<CursorPos> {
        *self.cursor.lock().unwrap()
    }

    pub fn lossless(&self) -> Vec<Lossless> {
        self.lossless.lock().unwrap().clone()
    }

    pub fn take_clip_image(&self) -> Option<(String, Vec<u8>)> {
        self.clip_image.lock().unwrap().take()
    }

    /// Messages since last asked.
    pub fn take_inbox(&self) -> Vec<Value> {
        self.inbox.lock().unwrap().drain(..).collect()
    }

    fn post(&self, msg: Value) {
        let mut inbox = self.inbox.lock().unwrap();
        // A window that isn't reading (closed mid-session) doesn't hold everything forever.
        if inbox.len() > 1000 {
            inbox.pop_front();
        }
        inbox.push_back(msg);
        drop(inbox);
        self.wake();
    }

    /// Rolls the per-second numbers over (called about once a second).
    fn tick_stats(&self) {
        let now = Instant::now();
        let mut last = self.last_tick.lock().unwrap();
        let secs = last.map_or(1.0, |t| (now - t).as_secs_f64()).max(0.001);
        *last = Some(now);
        let mut l = self.live.lock().unwrap();
        l.fps = l.frames as f64 / secs;
        l.mbps = l.bytes as f64 * 8.0 / secs / 1e6;
        if l.latency_n > 0 {
            l.latency_ms = l.latency_sum / l.latency_n as f64;
        }
        (l.frames, l.bytes, l.latency_sum, l.latency_n) = (0, 0, 0.0, 0);
        drop(l);
        self.wake();
    }
}

/// A connection to another computer, for as long as this lives.
pub struct Session {
    pub target: Target,
    pub viewer: Arc<Viewer>,
    tx: mpsc::UnboundedSender<net::Out>,
}

impl Session {
    pub fn start(rt: &tokio::runtime::Handle, target: Target) -> Self {
        let viewer = Arc::new(Viewer::new());
        let (tx, rx) = mpsc::unbounded_channel();
        rt.spawn(net::run(target.clone(), viewer.clone(), rx));
        Self { target, viewer, tx }
    }

    pub fn input(&self, ev: InputEvent) {
        let _ = self.tx.send(net::Out::Input(ev));
    }

    /// Sends input from elsewhere (the keyboard tap).
    pub fn input_sender(&self) -> impl Fn(InputEvent) + Send + 'static {
        let tx = self.tx.clone();
        move |ev| {
            let _ = tx.send(net::Out::Input(ev));
        }
    }

    /// Types text as it is (IME, emoji, accents), in order with keys.
    pub fn text(&self, text: &str) {
        for ev in glass_proto::text_events(text) {
            self.input(ev);
        }
    }

    /// Sends control messages from elsewhere (menus).
    pub fn control_sender(&self) -> impl Fn(Value) + Send + 'static {
        let tx = self.tx.clone();
        move |v| {
            let _ = tx.send(net::Out::Control(v));
        }
    }

    pub fn control(&self, msg: Value) {
        let _ = self.tx.send(net::Out::Control(msg));
    }

    /// The screen area each stream gets, in device pixels: the host encodes at this size,
    /// so nothing is sent that would be scaled away.
    pub fn viewport(&self, streams: &[[u32; 2]]) {
        self.control(serde_json::json!({ "t": "viewport", "streams": streams }));
    }

    /// Sends a file (or a folder's files) to the other computer's Downloads, or `dir`.
    pub fn send_file(&self, path: PathBuf, dir: Option<String>) {
        let _ = self.tx.send(net::Out::File { path, dir });
    }

    /// Puts an image (and its text, if any) on the other computer's clipboard.
    pub fn clipboard_image(&self, mime: String, bytes: Vec<u8>, text: String, paste: bool) {
        let _ = self.tx.send(net::Out::ClipImage { mime, bytes, text, paste });
    }

    /// Sends this Mac's microphone to the other computer, or stops.
    pub fn mic(&self, on: bool) {
        let _ = self.tx.send(net::Out::Mic(on));
    }

    /// Turns the computer's sound on or off here.
    pub fn audio(&self, on: bool) {
        self.viewer.audio_on.store(on, Ordering::Relaxed);
        self.control(serde_json::json!({ "t": "audio", "on": on }));
    }
}

/// `glass viewer-probe`: connects like the app's viewer, without a window, and reports what
/// arrived: the native pipeline end to end (QUIC, reassembly, hardware decode, input).
pub async fn probe(target: &str, token: Option<String>, secs: u64, send_input: bool, save: Option<std::path::PathBuf>, sharp: bool, click: Option<(f32, f32)>, typed: Option<String>, controls: Vec<String>) -> anyhow::Result<()> {
    let session = Session::start(&tokio::runtime::Handle::current(), {
        let mut t = Target::from_url(target).unwrap_or(Target { name: target.into(), dns: target.into(), is_self: false, token: None, link: None, remote: None });
        t.token = token.or(t.token);
        t
    });
    session.viewer.audio_on.store(false, Ordering::Relaxed);
    session.viewport(&[[1280, 720]]);
    let start = Instant::now();
    let mut moved = !send_input;
    let mut clicked = click.is_none();
    let mut typing = typed;
    let mut controls = Some(controls);
    while start.elapsed().as_secs() < secs {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if let Status::Refused(m) = session.viewer.status() {
            anyhow::bail!("refused: {m}");
        }
        if !clicked && session.viewer.status() == Status::Live {
            clicked = true;
            let (x, y) = click.unwrap();
            session.input(InputEvent::MouseMove { x, y, stream: 0 });
            session.input(InputEvent::MouseDown { button: 0 });
            session.input(InputEvent::MouseUp { button: 0 });
        }
        // `--control`: messages sent as they are, once connected.
        if session.viewer.status() == Status::Live
            && let Some(list) = controls.take()
        {
            for c in list {
                session.control(serde_json::from_str(&c)?);
            }
        }
        // `--keys`: typed key by key (US layout), `{wait:N}` pausing N seconds.
        if let Some(text) = if session.viewer.status() == Status::Live { typing.take() } else { None } {
            let mut rest = text.as_str();
            while !rest.is_empty() {
                if let Some(after) = rest.strip_prefix("{wait:") {
                    let (n, tail) = after.split_once('}').unwrap_or((after, ""));
                    tokio::time::sleep(std::time::Duration::from_secs(n.parse().unwrap_or(1))).await;
                    rest = tail;
                    continue;
                }
                let c = rest.chars().next().unwrap();
                rest = &rest[c.len_utf8()..];
                let Some((keycode, shift)) = keys::for_char(c) else { continue };
                if shift {
                    session.input(InputEvent::KeyDown { keycode: keys::SHIFT });
                }
                session.input(InputEvent::KeyDown { keycode });
                session.input(InputEvent::KeyUp { keycode });
                if shift {
                    session.input(InputEvent::KeyUp { keycode: keys::SHIFT });
                }
                tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            }
        }
        if !moved && session.viewer.status() == Status::Live {
            moved = true;
            session.input(InputEvent::MouseMove { x: 0.5, y: 0.25, stream: 0 });
            session.input(InputEvent::KeyDown { keycode: keys::COMMAND });
            session.input(InputEvent::KeyDown { keycode: 0x30 });
            session.input(InputEvent::KeyUp { keycode: 0x30 });
            session.input(InputEvent::KeyUp { keycode: keys::COMMAND });
            for keycode in [0x04u16, 0x22] {
                session.input(InputEvent::KeyDown { keycode });
                session.input(InputEvent::KeyUp { keycode });
            }
            session.text("héllo 👋");
            session.control(serde_json::json!({ "t": "showAll" }));
            session.control(serde_json::json!({ "t": "clipboard", "text": "from the native viewer" }));
            let f = std::env::temp_dir().join("glass-probe.txt");
            let _ = std::fs::write(&f, b"probe file");
            session.send_file(f, None);
        }
    }
    // Sharp text: the host switches to HEVC 4:2:2 for viewers that can decode it.
    let mut inbox = Vec::new();
    if sharp {
        let before = session.viewer.live.lock().unwrap().decoded;
        session.control(serde_json::json!({ "t": "sharp", "on": true }));
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        inbox.extend(session.viewer.take_inbox());
        let mode = inbox.iter().rev().find(|m| m["t"] == "sharp").map(|m| m["mode"].to_string()).unwrap_or_default();
        let codec = session.viewer.live.lock().unwrap().codec.clone();
        let during = session.viewer.live.lock().unwrap().decoded - before;
        session.control(serde_json::json!({ "t": "sharp", "on": false }));
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let hevc = session.viewer.live.lock().unwrap().hevc_decoded;
        println!("sharp text: mode {mode}, codec {codec}, {during} frames decoded while on, {hevc} of them HEVC");
    }
    let l = session.viewer.live.lock().unwrap().clone();
    inbox.extend(session.viewer.take_inbox());
    let kinds: Vec<String> = inbox.iter().filter_map(|m| m["t"].as_str().map(str::to_string)).collect();
    println!(
        "status {:?} · {}x{} {} · {} streams · {} frames decoded · {:.0} fps · {:.1} ms capture-to-decode · rtt {:.1} ms · {:.2} Mb/s · messages {kinds:?}",
        session.viewer.status(),
        l.width,
        l.height,
        l.codec,
        session.viewer.streams().len(),
        l.decoded,
        l.fps,
        l.latency_ms,
        l.rtt_ms,
        l.mbps
    );
    if l.decoded == 0 {
        anyhow::bail!("no frames decoded");
    }
    #[cfg(target_os = "macos")]
    if let Some(path) = save {
        let png = session.viewer.picture(0).and_then(|p| p.png()).ok_or_else(|| anyhow::anyhow!("no picture to save"))?;
        std::fs::write(&path, png)?;
        println!("saved {}", path.display());
    }
    #[cfg(not(target_os = "macos"))]
    let _ = save;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Target;

    #[test]
    fn links_become_targets() {
        let t = Target::from_url("https://mac.tail1234.ts.net/#l=abc123").unwrap();
        assert_eq!((t.name.as_str(), t.dns.as_str(), t.link.as_deref(), t.token.as_deref()), ("mac", "mac.tail1234.ts.net", Some("abc123"), None));
        let t = Target::from_url("https://mac.tail1234.ts.net:8080/#t=tok").unwrap();
        assert_eq!((t.dns.as_str(), t.token.as_deref()), ("https://mac.tail1234.ts.net:8080", Some("tok")));
        let t = Target::from_url("http://127.0.0.1:18080").unwrap();
        assert_eq!(t.dns, "http://127.0.0.1:18080");
        assert_eq!(Target::from_url("web+glass://mac.local").unwrap().dns, "mac.local");
        assert!(Target::from_url("  ").is_none());
        // Remote access: through the introducer the link names, with its code.
        let t = Target::from_url("https://glassdesk.dev/app/#r=BKEY123&l=code9").unwrap();
        assert_eq!(t.remote, Some(("https://glassdesk.dev".to_string(), "BKEY123".to_string())));
        assert_eq!(t.link.as_deref(), Some("code9"));
    }
}
