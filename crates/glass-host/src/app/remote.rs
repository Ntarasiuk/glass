//! Another computer's screen, right in the window: the native viewer (`crate::viewer`).
//! Its own entity, so a new frame redraws just this view.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent as FieldEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use glass_proto::InputEvent;
use serde_json::{Value, json};

use super::ui::{self, I};
use crate::viewer::{Session, Status, Stream, Target, keys};

/// Asks the window to close the viewer (the Disconnect button).
pub struct Disconnect;
impl EventEmitter<Disconnect> for RemoteView {}

/// A chat line.
struct Line {
    from: String,
    text: String,
    mine: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Panel {
    None,
    Chat,
    Files,
}

/// The file browser: a folder on the other computer (relative to its home folder).
#[derive(Default)]
struct Files {
    path: String,
    entries: Vec<Value>,
    error: Option<String>,
    req: u32,
}

/// A line drawn on the screen, by anyone viewing.
struct Stroke {
    stream: usize,
    color: Hsla,
    points: Vec<[f32; 2]>,
    updated: std::time::Instant,
}

const PENS: [(&str, u32); 5] = [
    ("#ff453a", 0xff453a),
    ("#ffd60a", 0xffd60a),
    ("#32d74b", 0x32d74b),
    ("#0a84ff", 0x0a84ff),
    ("#ffffff", 0xffffff),
];
const FADE_AFTER: Duration = Duration::from_secs(6);
const FADE: Duration = Duration::from_secs(2);

fn hex_color(c: &str) -> Hsla {
    u32::from_str_radix(c.trim_start_matches('#'), 16)
        .map(|v| gpui_kit::rgb(v).into())
        .unwrap_or_else(|_| gpui_kit::rgb(0xff453a).into())
}

/// A file on its way, for the toolbar.
struct Transfer {
    name: String,
    done: u64,
    total: Option<u64>,
    up: bool,
}

pub struct RemoteView {
    session: Session,
    focus: FocusHandle,
    /// Where the picture area is in the window (from the last paint).
    bounds: Bounds<Pixels>,
    /// The viewport last sent, in device pixels.
    sent_viewport: Vec<[u32; 2]>,
    scale: f32,
    /// Keys go to the other computer through the event tap (system shortcuts included).
    capture: Option<crate::viewer::sys::KeyCapture>,
    /// Take the keyboard whenever the picture has focus.
    capture_on: bool,
    /// ⌘ is Control there (a Windows or Linux computer).
    swap: Arc<AtomicBool>,
    /// The mouse is captured (relative motion, pointer locked and hidden).
    relative: Arc<AtomicBool>,
    /// Modifiers last sent without the tap.
    mods: Modifiers,
    full_screen: bool,
    cursors: crate::viewer::sys::Cursors,
    over_picture: bool,
    /// This Mac's clipboard as last synced (its change count), so neither side echoes.
    clip_seen: isize,
    /// Images decoded for sharp text, by their bytes' address.
    lossless_images: HashMap<usize, Arc<Image>>,
    chat: Vec<Line>,
    chat_open: bool,
    unread: usize,
    chat_input: Entity<InputState>,
    now_playing: Option<Value>,
    sharp: bool,
    privacy: bool,
    recording: bool,
    extended: bool,
    fitted: bool,
    mic: bool,
    update: Option<String>,
    transfer: Option<Transfer>,
    dragging_files: bool,
    /// The side panel: chat or the file browser.
    panel: Panel,
    terminal: Option<Entity<super::remote_term::TermView>>,
    files: Files,
    /// Windows that can be streamed on their own.
    windows: Vec<Value>,
    /// What agents are asking the owner, and whether they're paused or stopped.
    asks: Vec<Value>,
    agent: Value,
    /// Drawing on the screen instead of using it.
    drawing: bool,
    pen: usize,
    strokes: HashMap<String, Stroke>,
    /// The stroke being drawn: (id, stream, points not sent yet, when last sent).
    current: Option<(String, u8, Vec<[f32; 2]>, std::time::Instant)>,
    stroke_seq: u32,
    /// On screen (not behind a page or another session).
    visible: bool,
    /// Live previews of each display along the bottom, to switch with.
    previews: bool,
    thumbs: HashMap<u32, Arc<Image>>,
    /// The frame each stream showed last (Windows), freed when the next replaces it.
    #[cfg(windows)]
    shown: HashMap<usize, Arc<RenderImage>>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl RemoteView {
    pub fn new(
        target: Target,
        rt: &tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prefs = crate::viewer::prefs::load(&target.dns);
        let session = Session::start(rt, target);
        session
            .viewer
            .audio_on
            .store(prefs.audio.unwrap_or(true), Ordering::Relaxed);
        let viewer = session.viewer.clone();
        // Redraw when there's a new frame, message or status, at most once per change.
        let frames = cx.spawn_in(window, async move |this, cx| {
            loop {
                viewer.changed().await;
                if this
                    .update_in(cx, |v, window, cx| v.on_change(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        // This Mac's clipboard, to the other computer when it changes while this is in front.
        let clip = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(400))
                    .await;
                if this
                    .update_in(cx, |v, window, cx| v.sync_clipboard(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        // Strokes fade out even on a still screen.
        let fade = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let Ok(()) = this.update(cx, |v, cx| {
                    if !v.strokes.is_empty() {
                        v.strokes
                            .retain(|_, s| s.updated.elapsed() < FADE_AFTER + FADE);
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        });
        // Display previews (only what changed comes back), while they're showing.
        let previews = cx.spawn(async move |this, cx| {
            loop {
                let Ok(()) = this.update(cx, |v, _| {
                    if v.visible
                        && v.previews
                        && v.session.viewer.displays().len() > 1
                        && v.session.viewer.status() == Status::Live
                    {
                        v.session.control(json!({ "t": "thumbs" }));
                    }
                }) else {
                    break;
                };
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        });
        // Game controllers, while this is on screen and the app is in front.
        let pads = cx.spawn_in(window, async move |this, cx| {
            let mut pads = crate::viewer::sys::Pads::default();
            loop {
                let any = crate::viewer::sys::Pads::any();
                cx.background_executor()
                    .timer(Duration::from_millis(if any { 8 } else { 1000 }))
                    .await;
                let Ok(()) = this.update_in(cx, |v, window, _| {
                    if v.visible && window.is_window_active() && v.feature("gamepad") {
                        let send = v.session.input_sender();
                        pads.poll(&send);
                    }
                }) else {
                    break;
                };
            }
        });
        let focus = cx.focus_handle();
        let chat_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Message everyone on this computer…")
        });
        let subs = vec![
            cx.on_focus(&focus, window, |this, window, cx| {
                this.sync_capture(window, cx)
            }),
            cx.on_blur(&focus, window, |this, window, cx| {
                this.sync_capture(window, cx)
            }),
            cx.observe_window_activation(window, |this, window, cx| this.sync_capture(window, cx)),
            cx.subscribe_in(
                &chat_input,
                window,
                |this, input, ev: &FieldEvent, window, cx| {
                    if let FieldEvent::PressEnter { .. } = ev {
                        let text = input.read(cx).value().to_string();
                        if !text.trim().is_empty() {
                            this.session.control(json!({ "t": "chat", "text": text }));
                            input.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                    }
                },
            ),
        ];
        window.set_window_title(&format!("{} · Glass", session.target.name));
        window.focus(&focus, cx);
        Self {
            session,
            focus,
            bounds: Bounds::default(),
            sent_viewport: Vec::new(),
            scale: window.scale_factor(),
            capture: None,
            capture_on: true,
            swap: Arc::new(AtomicBool::new(false)),
            relative: Arc::new(AtomicBool::new(false)),
            mods: Modifiers::default(),
            full_screen: false,
            cursors: Default::default(),
            over_picture: false,
            clip_seen: crate::viewer::sys::clipboard_count(),
            lossless_images: HashMap::new(),
            chat: Vec::new(),
            chat_open: false,
            unread: 0,
            chat_input,
            now_playing: None,
            sharp: false,
            privacy: false,
            recording: false,
            extended: false,
            fitted: false,
            mic: false,
            update: None,
            transfer: None,
            dragging_files: false,
            panel: Panel::None,
            terminal: None,
            files: Files::default(),
            windows: Vec::new(),
            asks: Vec::new(),
            agent: Value::Null,
            drawing: false,
            pen: 0,
            strokes: HashMap::new(),
            current: None,
            stroke_seq: 0,
            visible: true,
            previews: true,
            thumbs: HashMap::new(),
            #[cfg(windows)]
            shown: HashMap::new(),
            _tasks: vec![frames, clip, fade, pads, previews],
            _subs: subs,
        }
    }

    pub fn full_screen(&self) -> bool {
        self.full_screen
    }

    /// The computer's own name once it's said, else what it was opened as.
    fn name(&self) -> String {
        self.welcome()["computer"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map_or_else(|| self.session.target.name.clone(), str::to_string)
    }

    pub fn dns(&self) -> &str {
        &self.session.target.dns
    }

    /// Its name and whether it's connected, for the sidebar.
    pub fn summary(&self) -> (String, bool) {
        (self.name(), self.session.viewer.status() == Status::Live)
    }

    /// On screen, or not: video pauses (and the keyboard goes back) while it isn't.
    pub fn set_visible(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = on;
        self.session.control(json!({ "t": "video", "on": on }));
        if on {
            window.set_window_title(&format!("{} · Glass", self.session.target.name));
            window.focus(&self.focus, cx);
        } else {
            self.set_relative(false);
            self.capture = None;
            self.cursors.arrow();
        }
        cx.notify();
    }

    /// Off screen (a page is showing).
    pub fn pause(&mut self, cx: &mut Context<Self>) {
        self.visible = false;
        self.session.control(json!({ "t": "video", "on": false }));
        self.set_relative(false);
        self.capture = None;
        self.cursors.arrow();
        cx.notify();
    }

    fn welcome(&self) -> Value {
        self.session.viewer.welcome().unwrap_or(Value::Null)
    }

    fn owner(&self) -> bool {
        self.welcome()["perms"]["owner"].as_bool().unwrap_or(false)
    }

    fn feature(&self, name: &str) -> bool {
        self.welcome()["features"][name].as_bool().unwrap_or(false)
    }

    fn host_os(&self) -> String {
        self.welcome()["host_os"]
            .as_str()
            .unwrap_or("macos")
            .to_string()
    }

    /// Something changed in the session: messages to act on, then a redraw.
    fn on_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for msg in self.session.viewer.take_inbox() {
            self.on_message(msg, window, cx);
        }
        if self.over_picture {
            self.cursors.apply(self.session.viewer.cursor());
        }
        cx.notify();
    }

    fn on_message(&mut self, msg: Value, window: &mut Window, cx: &mut Context<Self>) {
        let text = |k: &str| msg[k].as_str().unwrap_or_default().to_string();
        match msg["t"].as_str().unwrap_or_default() {
            "welcome" => {
                // A PC's Ctrl does what ⌘ does here.
                let prefs = crate::viewer::prefs::load(&self.session.target.dns);
                self.swap.store(
                    prefs.swap.unwrap_or(msg["host_os"] != "macos"),
                    Ordering::Relaxed,
                );
                if msg["features"]["windows"].as_bool().unwrap_or(false) {
                    self.session.control(json!({ "t": "listWindows" }));
                }
                // For screenshots and tests: open panels (`files`, `chat`, `terminal`, `draw`).
                if let Ok(open) = std::env::var("GLASS_APP_PANELS") {
                    if open.contains("files") {
                        self.panel = Panel::Files;
                    } else if open.contains("chat") {
                        self.panel = Panel::Chat;
                    }
                    if open.contains("terminal") && self.terminal.is_none() {
                        self.toggle_terminal(window, cx);
                    }
                    self.drawing |= open.contains("draw");
                    if open.contains("keys-off") {
                        self.capture_on = false;
                        self.capture = None;
                    }
                    if open.contains("mouse") {
                        self.set_relative(true);
                    }
                }
                if self.panel == Panel::Files {
                    self.list_files(self.files.path.clone());
                }
                self.cursors.forget();
                self.sent_viewport.clear();
                if let Some(name) = msg["computer"].as_str().filter(|n| !n.is_empty()) {
                    window.set_window_title(&format!("{name} · Glass"));
                }
            }
            "cursor" => self.cursors.add(&msg),
            "clipboard" => {
                let clip = crate::clip::Clip {
                    text: text("text"),
                    html: msg["html"].as_str().map(str::to_string),
                    image: None,
                };
                if msg["image"].as_bool().unwrap_or(false) {
                    // Images come on their own stream: ask for it.
                    self.session
                        .control(json!({ "t": "clipboardImage", "id": 1 }));
                }
                if !clip.text.is_empty() || clip.html.is_some() {
                    crate::clipboard::set(&clip);
                    self.clip_seen = crate::viewer::sys::clipboard_count();
                }
            }
            "_clipboardImage" => {
                if let Some((_, bytes)) = self.session.viewer.take_clip_image() {
                    let (text, html, _) = crate::viewer::sys::clipboard();
                    crate::clipboard::set(&crate::clip::Clip {
                        text,
                        html,
                        image: Some(bytes),
                    });
                    self.clip_seen = crate::viewer::sys::clipboard_count();
                }
            }
            "chat" => {
                let mine = msg["sender"] == self.welcome()["session"];
                self.chat.push(Line {
                    from: text("from"),
                    text: text("text"),
                    mine,
                });
                if !mine && !self.chat_open {
                    self.unread += 1;
                    super::toast(
                        window,
                        cx,
                        Ok(format!("{}: {}", text("from"), text("text"))),
                    );
                }
                // In the background: a notification, as the web viewer's push does.
                if !mine && !window.is_window_active() {
                    crate::notify::notify(
                        &format!("{} · {}", text("from"), self.session.target.name),
                        &text("text"),
                    );
                }
            }
            "notice" => super::toast(window, cx, Ok(text("msg"))),
            "error" if msg["msg"] != "unauthorized" => {
                super::toast(window, cx, Err(anyhow::anyhow!(text("msg"))))
            }
            "_error" => super::toast(window, cx, Err(anyhow::anyhow!(text("msg")))),
            "fileSaved" => super::toast(window, cx, Ok(format!("Saved there: {}", text("path")))),
            "_file" => {
                self.transfer = None;
                super::toast(
                    window,
                    cx,
                    Ok(format!("{} is in your Downloads", text("name"))),
                );
            }
            "_progress" => {
                let (done, total) = (msg["done"].as_u64().unwrap_or(0), msg["total"].as_u64());
                self.transfer = (Some(done) != total).then(|| Transfer {
                    name: text("name"),
                    done,
                    total,
                    up: msg["dir"] == "up",
                });
            }
            "_uploadFailed" => {
                self.transfer = None;
                super::toast(
                    window,
                    cx,
                    Err(anyhow::anyhow!(
                        "Couldn't send {}: {}",
                        text("name"),
                        text("error")
                    )),
                );
            }
            "nowPlaying" => {
                self.now_playing = msg["info"]["title"]
                    .is_string()
                    .then(|| msg["info"].clone())
            }
            "sharp" => self.sharp = msg["mode"] != "off",
            "privacy" => self.privacy = msg["on"].as_bool().unwrap_or(false),
            "recording" => {
                self.recording = msg["on"].as_bool().unwrap_or(false);
                if let Some(p) = msg["path"].as_str().filter(|_| !self.recording) {
                    super::toast(window, cx, Ok(format!("Recording saved there: {p}")));
                }
            }
            "extended" => self.extended = msg["on"].as_bool().unwrap_or(false),
            "micReady" => super::toast(window, cx, Ok("The microphone is ready there.".into())),
            "updateAvailable" => self.update = msg["version"].as_str().map(str::to_string),
            "agentAsk" => {
                if !window.is_window_active() {
                    crate::notify::notify(
                        &format!("An agent on {}", self.session.target.name),
                        &text("text"),
                    );
                }
                self.asks.push(msg.clone());
            }
            "agentAnswered" => self.asks.retain(|a| a["id"] != msg["id"]),
            "agentStatus" => self.agent = msg["status"].clone(),
            "thumbs" => {
                use base64::Engine;
                for t in msg["thumbs"].as_array().into_iter().flatten() {
                    if let (Some(id), Some(png)) = (
                        t["id"].as_u64(),
                        t["png"]
                            .as_str()
                            .and_then(|p| base64::engine::general_purpose::STANDARD.decode(p).ok()),
                    ) {
                        self.thumbs.insert(
                            id as u32,
                            Arc::new(Image::from_bytes(ImageFormat::Png, png)),
                        );
                    }
                }
            }
            "windows" => self.windows = msg["windows"].as_array().cloned().unwrap_or_default(),
            "files" => {
                if msg["req"].is_null() || msg["req"] == self.files.req {
                    self.files.path = text("path");
                    self.files.entries = msg["entries"].as_array().cloned().unwrap_or_default();
                    self.files.error = msg["error"].as_str().map(str::to_string);
                }
            }
            "term" => {
                if let Some(t) = &self.terminal {
                    t.update(cx, |t, cx| t.on_message(&msg, cx));
                }
            }
            "annotate" => {
                let st = &msg["stroke"];
                let key = format!("{}:{}", msg["sender"], st["id"]);
                let stroke = self.strokes.entry(key).or_insert_with(|| Stroke {
                    stream: st["stream"].as_u64().unwrap_or(0) as usize,
                    color: hex_color(st["color"].as_str().unwrap_or("#ff453a")),
                    points: Vec::new(),
                    updated: std::time::Instant::now(),
                });
                stroke.updated = std::time::Instant::now();
                for p in st["points"].as_array().into_iter().flatten() {
                    if let (Some(x), Some(y)) = (p[0].as_f64(), p[1].as_f64()) {
                        stroke.points.push([x as f32, y as f32]);
                    }
                }
            }
            "annotateClear" => {
                let prefix = format!("{}:", msg["sender"]);
                self.strokes.retain(|k, _| !k.starts_with(&prefix));
            }
            _ => {}
        }
    }

    /// This Mac's clipboard changed: the other computer gets it too.
    fn sync_clipboard(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        if !window.is_window_active()
            || self.session.viewer.status() != Status::Live
            || !self.welcome()["perms"]["clipboard"]
                .as_bool()
                .unwrap_or(false)
        {
            return;
        }
        let count = crate::viewer::sys::clipboard_count();
        if count == self.clip_seen {
            return;
        }
        self.clip_seen = count;
        let (text, html, image) = crate::viewer::sys::clipboard();
        match image.then(crate::clipboard::image_png).flatten() {
            Some(png) if png.len() <= crate::clip::MAX_IMAGE => {
                self.session
                    .clipboard_image("image/png".into(), png, text, false)
            }
            _ if !text.is_empty() || html.is_some() => self
                .session
                .control(json!({ "t": "clipboard", "text": text, "html": html })),
            _ => {}
        }
    }

    /// The keyboard is the other computer's while the picture has focus in the active window.
    fn sync_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let want = self.capture_on && window.is_window_active() && self.focus.is_focused(window);
        if want && self.capture.is_none() {
            let send = self.session.input_sender();
            let this = cx.entity().downgrade();
            let async_cx = cx.to_async();
            self.capture = crate::viewer::sys::KeyCapture::start(
                send,
                move || {
                    // From inside the tap: let go on the next turn of the loop.
                    let this = this.clone();
                    async_cx.update(|cx| {
                        cx.defer(move |cx| {
                            let _ = this.update(cx, |v, cx| {
                                v.capture_on = false;
                                v.set_relative(false);
                                v.capture = None;
                                cx.notify();
                            });
                        })
                    });
                },
                self.swap.clone(),
                self.relative.clone(),
            );
        } else if !want {
            self.set_relative(false);
            self.capture = None;
        }
        if !window.is_window_active() {
            self.over_picture = false;
        }
        cx.notify();
    }

    /// Captures the mouse too (for games), or lets it go.
    fn set_relative(&mut self, on: bool) {
        let on = on && self.capture.is_some();
        if self.relative.swap(on, Ordering::Relaxed) != on {
            crate::viewer::sys::lock_pointer(on);
        }
    }

    fn captured(&self) -> bool {
        self.capture.is_some()
    }

    /// Where each stream is drawn: arranged as the displays are on the desk, fitted to the area.
    fn layout(&self) -> Vec<(usize, Bounds<Pixels>)> {
        let streams = self.session.viewer.streams();
        let b = self.bounds;
        if streams.is_empty() {
            return Vec::new();
        }
        let arranged = streams.len() > 1 && streams.iter().all(|s| s.points_width > 0.0);
        // Each stream's rectangle in the host's desktop points (side by side if unknown).
        let mut x = 0.0;
        let rects: Vec<(f64, f64, f64, f64)> = streams
            .iter()
            .map(|s: &Stream| {
                let (w, h) = if s.points_width > 0.0 {
                    (s.points_width, s.points_height)
                } else {
                    (s.width.max(1) as f64, s.height.max(1) as f64)
                };
                let r = if arranged {
                    (s.x, s.y, w, h)
                } else {
                    (x, 0.0, w, h)
                };
                x += w;
                r
            })
            .collect();
        let (x0, y0) = rects
            .iter()
            .fold((f64::MAX, f64::MAX), |(a, b), r| (a.min(r.0), b.min(r.1)));
        let (x1, y1) = rects.iter().fold((f64::MIN, f64::MIN), |(a, b), r| {
            (a.max(r.0 + r.2), b.max(r.1 + r.3))
        });
        let (bw, bh) = (
            f32::from(b.size.width) as f64,
            f32::from(b.size.height) as f64,
        );
        let gap = if streams.len() > 1 { 6.0 } else { 0.0 };
        let scale = ((bw - gap) / (x1 - x0))
            .min((bh - gap) / (y1 - y0))
            .max(0.0);
        let (ox, oy) = (
            (bw - (x1 - x0) * scale) / 2.0,
            (bh - (y1 - y0) * scale) / 2.0,
        );
        rects
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let (rx, ry) = (ox + (r.0 - x0) * scale, oy + (r.1 - y0) * scale);
                let inset = if streams.len() > 1 { 2.0 } else { 0.0 };
                (
                    i,
                    Bounds::new(
                        point(
                            b.origin.x + px((rx + inset) as f32),
                            b.origin.y + px((ry + inset) as f32),
                        ),
                        size(
                            px((r.2 * scale - inset * 2.0) as f32),
                            px((r.3 * scale - inset * 2.0) as f32),
                        ),
                    ),
                )
            })
            .collect()
    }

    /// The stream under a point, and where in it (0..1).
    fn locate(&self, p: Point<Pixels>) -> Option<(u8, f32, f32)> {
        let layout = self.layout();
        let (i, r) = layout.iter().find(|(_, r)| r.contains(&p)).or_else(|| {
            // Off every picture (a drag past the edge): the nearest one.
            layout
                .iter()
                .min_by(|a, b| dist(a.1, p).total_cmp(&dist(b.1, p)))
        })?;
        let x = (f32::from(p.x - r.origin.x) / f32::from(r.size.width).max(1.)).clamp(0., 1.);
        let y = (f32::from(p.y - r.origin.y) / f32::from(r.size.height).max(1.)).clamp(0., 1.);
        Some((*i as u8, x, y))
    }

    fn list_files(&mut self, path: String) {
        self.files.req += 1;
        self.session
            .control(json!({ "t": "filesList", "path": path, "req": self.files.req }));
    }

    fn pen_down(&mut self, p: Point<Pixels>) {
        let Some((stream, x, y)) = self.locate(p) else {
            return;
        };
        self.stroke_seq += 1;
        let id = format!("n{}", self.stroke_seq);
        self.add_points(&id, stream, &[[x, y]]);
        self.current = Some((
            id,
            stream,
            vec![[x, y]],
            std::time::Instant::now() - Duration::from_secs(1),
        ));
        self.flush_stroke(false);
    }

    fn pen_move(&mut self, p: Point<Pixels>) {
        let Some((id, stream, ..)) = self.current.clone() else {
            return;
        };
        let layout = self.layout();
        let Some((_, r)) = layout.iter().find(|(i, _)| *i == stream as usize) else {
            return;
        };
        let x = (f32::from(p.x - r.origin.x) / f32::from(r.size.width).max(1.)).clamp(0., 1.);
        let y = (f32::from(p.y - r.origin.y) / f32::from(r.size.height).max(1.)).clamp(0., 1.);
        self.add_points(&id, stream, &[[x, y]]);
        if let Some(c) = &mut self.current {
            c.2.push([x, y]);
        }
        self.flush_stroke(false);
    }

    /// Sends the stroke's new points (at most every 40 ms while drawing).
    fn flush_stroke(&mut self, done: bool) {
        let Some((id, stream, pending, last)) = &mut self.current else {
            return;
        };
        if pending.is_empty() || (!done && last.elapsed() < Duration::from_millis(40)) {
            if done {
                self.current = None;
            }
            return;
        }
        let points: Vec<[f32; 2]> = std::mem::take(pending)
            .iter()
            .map(|p| {
                [
                    (p[0] * 10000.).round() / 10000.,
                    (p[1] * 10000.).round() / 10000.,
                ]
            })
            .collect();
        self.session.control(json!({ "t": "annotate", "stroke": { "id": id, "stream": stream, "color": PENS[self.pen].0, "points": points } }));
        *last = std::time::Instant::now();
        if done {
            self.current = None;
        }
    }

    fn add_points(&mut self, id: &str, stream: u8, points: &[[f32; 2]]) {
        let me = self.welcome()["session"].clone();
        let s = self
            .strokes
            .entry(format!("{me}:{id}"))
            .or_insert_with(|| Stroke {
                stream: stream as usize,
                color: gpui_kit::rgb(PENS[self.pen].1).into(),
                points: Vec::new(),
                updated: std::time::Instant::now(),
            });
        s.updated = std::time::Instant::now();
        s.points.extend_from_slice(points);
    }

    fn move_to(&mut self, p: Point<Pixels>) {
        let inside = self.layout().iter().any(|(_, r)| r.contains(&p));
        if inside != self.over_picture {
            self.over_picture = inside;
            if !inside {
                self.cursors.arrow();
            }
        }
        if inside {
            self.cursors.apply(self.session.viewer.cursor());
        }
        if let Some((stream, x, y)) = self.locate(p) {
            self.session.input(InputEvent::MouseMove { x, y, stream });
        }
    }

    fn button(b: MouseButton) -> Option<u8> {
        match b {
            MouseButton::Left => Some(0),
            MouseButton::Middle => Some(1),
            MouseButton::Right => Some(2),
            _ => None,
        }
    }

    /// Without the tap: GPUI's keys. Characters are typed as text (any layout, accents,
    /// emoji); the rest as the Mac's keycodes.
    fn key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.captured() {
            return;
        }
        let m = &ev.keystroke.modifiers;
        // ⌘Q always quits Glass (here, not there).
        if cfg!(target_os = "macos")
            && m.platform
            && !m.control
            && !m.alt
            && ev.keystroke.key == "q"
        {
            return;
        }
        let shortcut = m.platform || m.control || m.function;
        // Characters come through the input handler below (any layout, accents, IME).
        let text = ev
            .keystroke
            .key_char
            .as_deref()
            .is_some_and(|c| !shortcut && !c.chars().any(char::is_control));
        if text {
            return;
        }
        if let Some(keycode) = keys::keycode(&ev.keystroke.key) {
            self.session.input(InputEvent::KeyDown { keycode });
            self.session.input(InputEvent::KeyUp { keycode });
            cx.stop_propagation();
        }
    }

    fn modifiers(&mut self, m: &Modifiers) {
        if self.captured() {
            return;
        }
        let swap = self.swap.load(Ordering::Relaxed);
        let map = |k| {
            if swap {
                crate::viewer::sys::swapped(k)
            } else {
                k
            }
        };
        for (was, now, keycode) in [
            (self.mods.platform, m.platform, keys::COMMAND),
            (self.mods.shift, m.shift, keys::SHIFT),
            (self.mods.alt, m.alt, keys::OPTION),
            (self.mods.control, m.control, keys::CONTROL),
        ] {
            if was != now {
                let keycode = map(keycode);
                self.session.input(if now {
                    InputEvent::KeyDown { keycode }
                } else {
                    InputEvent::KeyUp { keycode }
                });
            }
        }
        self.mods = *m;
    }

    /// The area's size changed: the host encodes each stream for the space it gets here.
    fn laid_out(&mut self, bounds: Bounds<Pixels>, scale: f32) {
        self.bounds = bounds;
        self.scale = scale;
        let sizes: Vec<[u32; 2]> = self
            .layout()
            .iter()
            .map(|(_, r)| {
                [
                    (f32::from(r.size.width) * scale).round() as u32,
                    (f32::from(r.size.height) * scale).round() as u32,
                ]
            })
            .collect();
        if !sizes.is_empty()
            && sizes.iter().all(|s| s[0] > 16 && s[1] > 16)
            && sizes != self.sent_viewport
        {
            self.sent_viewport = sizes.clone();
            self.session.viewport(&sizes);
        }
    }

    /// The area in device pixels (for fitting the computer's desktop to this window).
    fn area_pixels(&self) -> [u32; 2] {
        [
            (f32::from(self.bounds.size.width) * self.scale) as u32,
            (f32::from(self.bounds.size.height) * self.scale) as u32,
        ]
    }

    fn send_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Send".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update(cx, |v, _| {
                    paths.into_iter().for_each(|p| v.session.send_file(p, None))
                });
            }
        })
        .detach();
    }

    fn displays_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let previews = self.previews;
        let displays = self.session.viewer.displays();
        let streams = self.session.viewer.streams();
        let showing: Vec<u32> = streams.iter().map(|s| s.display).collect();
        let windows = self.windows.clone();
        let ctl = self.session_control();
        Button::new("remote-displays")
            .small()
            .ghost()
            .icon(I::Monitor)
            .label(if streams.len() > 1 {
                format!("{} displays", streams.len())
            } else {
                "Display".into()
            })
            .dropdown_menu(move |mut menu, _, _| {
                for (i, d) in displays.iter().enumerate() {
                    let id = d["id"].as_u64().unwrap_or(0) as u32;
                    let label = format!(
                        "{}{} · {}×{}",
                        if d["builtin"].as_bool().unwrap_or(false) {
                            "Built-in"
                        } else {
                            "Display"
                        },
                        if d["main"].as_bool().unwrap_or(false) {
                            " (main)".to_string()
                        } else {
                            format!(" {}", i + 1)
                        },
                        d["width"],
                        d["height"]
                    );
                    let ctl = ctl.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(showing.len() == 1 && showing[0] == id)
                            .on_click(move |_, _, _| {
                                ctl(json!({ "t": "selectDisplay", "id": id }))
                            }),
                    );
                }
                if !windows.is_empty() {
                    menu = menu.separator().label("Just one window");
                    for w in windows.iter().take(20) {
                        let id = w["id"].as_u64().unwrap_or(0) as u32;
                        let label = format!(
                            "{} — {}",
                            w["app"].as_str().unwrap_or_default(),
                            w["title"].as_str().unwrap_or_default()
                        );
                        let ctl = ctl.clone();
                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, _| {
                            ctl(json!({ "t": "selectWindow", "id": id }))
                        }));
                    }
                }
                if displays.len() > 1 {
                    let this = this.clone();
                    menu = menu.separator().item(
                        PopupMenuItem::new("Previews along the bottom")
                            .checked(previews)
                            .on_click(move |_, _, cx| {
                                let _ = this.update(cx, |v, cx| {
                                    v.previews = !v.previews;
                                    cx.notify();
                                });
                            }),
                    );
                    let ctl = ctl.clone();
                    menu = menu.item(
                        PopupMenuItem::new("All displays, side by side")
                            .checked(showing.len() > 1)
                            .on_click(move |_, _, _| ctl(json!({ "t": "showAll" }))),
                    );
                }
                menu
            })
    }

    fn session_control(&self) -> Rc<dyn Fn(Value)> {
        let send = self.session.control_sender();
        Rc::new(move |v| send(v))
    }

    fn more_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let owner = self.owner();
        let (sharp, privacy, recording, extended, fitted, mic) = (
            self.sharp,
            self.privacy,
            self.recording,
            self.extended,
            self.fitted,
            self.mic,
        );
        let (f_privacy, f_fit, f_extend, f_record, f_mic, f_restart) = (
            self.feature("privacy"),
            self.feature("fit"),
            self.feature("extend"),
            self.feature("record"),
            self.feature("mic") || self.feature("mic_install"),
            self.feature("restart"),
        );
        let pc = self.host_os() == "windows";
        let swap = self.swap.load(Ordering::Relaxed);
        let relative = self.relative.load(Ordering::Relaxed);
        let update = self.update.clone();
        let web = format!("https://{}/", self.session.target.dns);
        let ctl = self.session_control();
        let act = move |f: fn(&mut RemoteView, &mut Window, &mut Context<RemoteView>)| {
            let this = this.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |v, cx| f(v, window, cx));
            }
        };
        Button::new("remote-more")
            .small()
            .ghost()
            .icon(I::Settings)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let c = |v: Value| {
                    let ctl = ctl.clone();
                    move |_: &ClickEvent, _: &mut Window, _: &mut App| ctl(v.clone())
                };
                let mut menu = menu
                    .item(
                        PopupMenuItem::new("Sharp text")
                            .checked(sharp)
                            .on_click(c(json!({ "t": "sharp", "on": !sharp }))),
                    )
                    .item(
                        PopupMenuItem::new("Send files…")
                            .icon(I::Folder)
                            .on_click(act(|v, w, cx| v.send_files(w, cx))),
                    )
                    .item(
                        PopupMenuItem::new("Get files from there…")
                            .icon(I::Download)
                            .on_click(c(json!({ "t": "pickFiles" }))),
                    )
                    .item(
                        PopupMenuItem::new("Microphone")
                            .icon(I::Mic)
                            .checked(mic)
                            .disabled(!f_mic)
                            .on_click(act(|v, _, cx| {
                                v.mic = !v.mic;
                                v.session.mic(v.mic);
                                cx.notify();
                            })),
                    )
                    .item(
                        PopupMenuItem::new(format!(
                            "Capture the mouse (games) · {} releases",
                            crate::viewer::sys::RELEASE_HINT
                        ))
                        .checked(relative)
                        .on_click(act(|v, window, cx| {
                            if !v.captured() {
                                v.toggle_capture(window, cx);
                            }
                            let on = !v.relative.load(Ordering::Relaxed);
                            v.set_relative(on);
                            cx.notify();
                        })),
                    )
                    .item(PopupMenuItem::new("⌘ as Ctrl").checked(swap).on_click(act(
                        |v, _, cx| {
                            let on = !v.swap.fetch_xor(true, Ordering::Relaxed);
                            crate::viewer::prefs::update(&v.session.target.dns, |p| {
                                p.swap = Some(on)
                            });
                            cx.notify();
                        },
                    )));
                if pc {
                    menu = menu.item(
                        PopupMenuItem::new("Send Ctrl+Alt+Delete")
                            .on_click(c(json!({ "t": "sas" }))),
                    );
                }
                menu = menu.separator();
                if f_fit {
                    menu = menu.item(
                        PopupMenuItem::new("Fit its desktop to this window")
                            .checked(fitted)
                            .on_click(act(|v, _, cx| {
                                v.fitted = !v.fitted;
                                let size = v.fitted.then(|| v.area_pixels());
                                v.session.control(json!({ "t": "fit", "size": size }));
                                cx.notify();
                            })),
                    );
                }
                if f_extend && owner {
                    menu = menu.item(
                        PopupMenuItem::new("Use this window as another display")
                            .checked(extended)
                            .on_click(act(|v, _, cx| {
                                let size = (!v.extended).then(|| v.area_pixels());
                                v.session.control(
                                    json!({ "t": "extend", "size": size, "hidpi": v.scale > 1.5 }),
                                );
                                cx.notify();
                            })),
                    );
                }
                if f_privacy && owner {
                    menu = menu.item(
                        PopupMenuItem::new("Privacy mode (blank its screen)")
                            .icon(I::EyeOff)
                            .checked(privacy)
                            .on_click(c(json!({ "t": "privacy", "on": !privacy }))),
                    );
                }
                if f_record && owner {
                    menu = menu.item(
                        PopupMenuItem::new(if recording {
                            "Stop recording"
                        } else {
                            "Record this session"
                        })
                        .icon(I::Video)
                        .on_click(c(json!({ "t": "record", "on": !recording }))),
                    );
                }
                menu = menu.separator().item(
                    PopupMenuItem::new("Open in the browser")
                        .icon(I::ExternalLink)
                        .on_click({
                            let web = web.clone();
                            move |_, _, _| super::actions::open(&web)
                        }),
                );
                if owner {
                    if let Some(v) = &update {
                        menu = menu.item(
                            PopupMenuItem::new(format!("Update Glass there to {v}"))
                                .icon(I::Download)
                                .on_click(c(json!({ "t": "update" }))),
                        );
                    }
                    if f_restart {
                        menu = menu
                            .item(
                                PopupMenuItem::new("Restart Glass there")
                                    .icon(I::RotateCw)
                                    .on_click(c(json!({ "t": "restart", "glass": true }))),
                            )
                            .item(
                                PopupMenuItem::new("Restart the computer")
                                    .icon(I::Power)
                                    .on_click(c(json!({ "t": "restart", "glass": false }))),
                            );
                    }
                }
                menu
            })
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let viewer = &self.session.viewer;
        let status = viewer.status();
        let live = viewer.live.lock().unwrap().clone();
        let (color, detail) = match &status {
            Status::Live => (
                theme.success,
                format!(
                    "{} · {:.0} fps · {:.0} ms · {:.1} Mb/s{}",
                    live.codec.to_uppercase(),
                    live.fps,
                    live.latency_ms.max(live.rtt_ms / 2.),
                    live.mbps,
                    if self.privacy { " · privacy on" } else { "" }
                ),
            ),
            Status::Connecting(m) => (theme.warning, m.clone()),
            Status::Refused(m) => (theme.danger, m.clone()),
        };
        let captured = self.captured();
        let audio = viewer.audio_on.load(Ordering::Relaxed);
        let has_audio = self.welcome()["audio"].as_bool().unwrap_or(false);
        h_flex()
            .flex_none()
            .h(px(40.))
            .px_3()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(ui::dot(color))
            .child(
                div()
                    .pl_1()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.name()),
            )
            .child(
                div()
                    .pl_1()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(detail),
            )
            .children(self.transfer.as_ref().map(|t| {
                let pct = t
                    .total
                    .filter(|&n| n > 0)
                    .map_or(String::new(), |n| format!(" {}%", t.done * 100 / n));
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} {}{pct}", if t.up { "↑" } else { "↓" }, t.name))
            }))
            .children(self.now_playing.as_ref().map(|np| self.media(np, cx)))
            .when(
                self.session.viewer.displays().len() > 1 || !self.windows.is_empty(),
                |d| d.child(self.displays_menu(cx)),
            )
            .when(has_audio, |d| {
                d.child(
                    Button::new("remote-sound")
                        .small()
                        .ghost()
                        .icon(if audio { I::Volume2 } else { I::VolumeX })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.session.audio(!audio);
                            crate::viewer::prefs::update(&this.session.target.dns, |p| {
                                p.audio = Some(!audio)
                            });
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("remote-chat")
                    .small()
                    .ghost()
                    .icon(I::MessageSquare)
                    .when(self.unread > 0, |b| b.label(self.unread.to_string()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.panel = if this.panel == Panel::Chat {
                            Panel::None
                        } else {
                            Panel::Chat
                        };
                        this.chat_open = this.panel == Panel::Chat;
                        this.unread = 0;
                        if this.chat_open {
                            this.chat_input.update(cx, |s, cx| s.focus(window, cx));
                        }
                        cx.notify();
                    })),
            )
            .when(self.owner() && self.feature("browse"), |d| {
                d.child(
                    Button::new("remote-files")
                        .small()
                        .ghost()
                        .icon(I::Folder)
                        .map(|b| {
                            if self.panel == Panel::Files {
                                b.primary()
                            } else {
                                b
                            }
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.panel = if this.panel == Panel::Files {
                                Panel::None
                            } else {
                                Panel::Files
                            };
                            this.chat_open = false;
                            if this.panel == Panel::Files {
                                this.list_files(this.files.path.clone());
                            }
                            cx.notify();
                        })),
                )
            })
            .when(self.owner() && self.feature("terminal"), |d| {
                d.child(
                    Button::new("remote-term")
                        .small()
                        .ghost()
                        .icon(I::SquareTerminal)
                        .map(|b| {
                            if self.terminal.is_some() {
                                b.primary()
                            } else {
                                b
                            }
                        })
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_terminal(window, cx)),
                        ),
                )
            })
            .child(
                Button::new("remote-draw")
                    .small()
                    .ghost()
                    .icon(I::Pencil)
                    .map(|b| if self.drawing { b.primary() } else { b })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.drawing = !this.drawing;
                        cx.notify();
                    })),
            )
            .child(self.more_menu(cx))
            .child(
                Button::new("remote-keys")
                    .small()
                    .map(|b| if captured { b.primary() } else { b.ghost() })
                    .icon(I::Keyboard)
                    .label(if captured {
                        format!("{} to release", crate::viewer::sys::RELEASE_HINT)
                    } else {
                        "Capture keys".into()
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_capture(window, cx))),
            )
            .child(
                Button::new("remote-full")
                    .small()
                    .ghost()
                    .icon(if self.full_screen {
                        I::Minimize
                    } else {
                        I::Maximize
                    })
                    .on_click(
                        cx.listener(|this, _, window, cx| this.toggle_full_screen(window, cx)),
                    ),
            )
            .child(
                Button::new("remote-close")
                    .small()
                    .danger()
                    .icon(I::Unplug)
                    .label("End")
                    .tooltip("End the session")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Disconnect))),
            )
    }

    /// What the computer is playing, with its controls.
    fn media(&self, np: &Value, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let title = np["title"].as_str().unwrap_or_default();
        let artist = np["artist"].as_str().unwrap_or_default();
        let playing = np["playing"].as_bool().unwrap_or(false);
        let ctl = |id: &'static str, icon: I, action: &'static str| {
            Button::new(id)
                .xsmall()
                .ghost()
                .icon(icon)
                .on_click(cx.listener(move |this, _, _, _| {
                    this.session
                        .control(json!({ "t": "media", "action": action }))
                }))
        };
        h_flex()
            .gap_0p5()
            .max_w(px(260.))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if artist.is_empty() {
                        title.to_string()
                    } else {
                        format!("{title} — {artist}")
                    }),
            )
            .child(ctl("media-prev", I::SkipBack, "previous"))
            .child(ctl(
                "media-play",
                if playing { I::Pause } else { I::Play },
                "toggle",
            ))
            .child(ctl("media-next", I::SkipForward, "next"))
    }

    fn toggle_full_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.full_screen = !self.full_screen;
        window.toggle_fullscreen();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn toggle_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::viewer::sys::allowed() {
            crate::viewer::sys::ask();
            super::toast(
                window,
                cx,
                Err(anyhow::anyhow!(
                    "Allow Glass in Accessibility so ⌘Tab, ⌘Space and other shortcuts reach the other computer."
                )),
            );
            return;
        }
        self.capture_on = !self.captured();
        if self.capture_on {
            window.focus(&self.focus, cx);
        }
        self.sync_capture(window, cx);
    }

    fn chat_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .w(px(280.))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(h_flex().h(px(36.)).px_3().text_sm().font_weight(FontWeight::MEDIUM).child("Chat"))
            .child(
                v_flex()
                    .id("remote-chat-lines")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .gap_2()
                    .children(self.chat.iter().map(|l| {
                        v_flex()
                            .when(l.mine, |d| d.items_end())
                            .child(div().text_xs().text_color(theme.muted_foreground).child(if l.mine { "You".to_string() } else { l.from.clone() }))
                            .child(div().px_2().py_1().rounded(px(8.)).bg(if l.mine { theme.primary.opacity(0.18) } else { theme.secondary }).text_sm().child(l.text.clone()))
                    }))
                    .when(self.chat.is_empty(), |d| d.child(div().text_xs().text_color(theme.muted_foreground).child("Messages go to everyone viewing this computer, and to the person at it."))),
            )
            .child(div().p_2().child(Input::new(&self.chat_input).small()))
    }
}

impl RemoteView {
    /// Every display as a live preview; the ones showing are marked, a click shows that one.
    fn preview_strip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let displays = self.session.viewer.displays();
        if !self.previews || displays.len() < 2 || self.full_screen {
            return None;
        }
        let theme = cx.theme().clone();
        let showing: Vec<u32> = self
            .session
            .viewer
            .streams()
            .iter()
            .map(|s| s.display)
            .collect();
        Some(
            h_flex()
                .flex_none()
                .h(px(92.))
                .px_3()
                .gap_3()
                .justify_center()
                .bg(theme.background)
                .border_t_1()
                .border_color(theme.border)
                .children(displays.iter().map(|d| {
                    let id = d["id"].as_u64().unwrap_or(0) as u32;
                    let (w, h) = (
                        d["width"].as_f64().unwrap_or(16.),
                        d["height"].as_f64().unwrap_or(9.),
                    );
                    let tw = (72. * w / h.max(1.)) as f32;
                    let on = showing.contains(&id);
                    div()
                        .id(SharedString::from(format!("thumb-{id}")))
                        .w(px(tw))
                        .h(px(72.))
                        .rounded(px(6.))
                        .overflow_hidden()
                        .bg(gpui_kit::black())
                        .border_2()
                        .border_color(if on { theme.primary } else { theme.border })
                        .cursor_pointer()
                        .children(self.thumbs.get(&id).cloned().map(|img_| {
                            img(ImageSource::Image(img_))
                                .size_full()
                                .object_fit(ObjectFit::Contain)
                        }))
                        .on_click(cx.listener(move |this, _, _, _| {
                            this.session
                                .control(json!({ "t": "selectDisplay", "id": id }))
                        }))
                })),
        )
    }

    fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.terminal.take() {
            t.update(cx, |t, _| t.close());
            window.focus(&self.focus, cx);
        } else {
            let send = self.session.control_sender();
            let t = cx.new(|cx| super::remote_term::TermView::new(send, cx));
            let f = t.read(cx).focus.clone();
            window.focus(&f, cx);
            self.terminal = Some(t);
        }
        self.sync_capture(window, cx);
    }

    fn files_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let path = self.files.path.clone();
        let join = |name: &str| {
            if path.is_empty() {
                name.to_string()
            } else {
                format!("{path}/{name}")
            }
        };
        let up = path
            .rsplit_once('/')
            .map_or(String::new(), |(p, _)| p.to_string());
        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, e) in self.files.entries.iter().enumerate() {
            let name = e["name"].as_str().unwrap_or_default().to_string();
            let dir = e["dir"].as_bool().unwrap_or(false);
            let full = join(&name);
            let size = e["size"].as_u64().unwrap_or(0);
            let detail = if dir { String::new() } else { human(size) };
            let (open, trash) = (full.clone(), full.clone());
            rows.push(
                h_flex()
                    .id(SharedString::from(format!("file-{i}")))
                    .px_2()
                    .h(px(28.))
                    .gap_2()
                    .rounded(px(6.))
                    .hover(|d| d.bg(theme.secondary_hover))
                    .cursor_pointer()
                    .child(
                        gpui_kit::component::Icon::new(if dir { I::Folder } else { I::FileText })
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    )
                    .child(
                        Button::new(SharedString::from(format!("trash-{i}")))
                            .xsmall()
                            .ghost()
                            .icon(I::Trash)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                let name = trash.rsplit('/').next().unwrap_or(&trash).to_string();
                                let answer = window.prompt(
                                    PromptLevel::Warning,
                                    &format!(
                                        "Move “{name}” to the Trash on {}?",
                                        this.session.target.name
                                    ),
                                    None,
                                    &["Move to Trash", "Cancel"],
                                    cx,
                                );
                                let (path, send) = (trash.clone(), this.session.control_sender());
                                cx.spawn(async move |_, _| {
                                    if answer.await == Ok(0) {
                                        send(json!({ "t": "filesTrash", "path": path }));
                                    }
                                })
                                .detach();
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, _| {
                        if dir {
                            this.list_files(open.clone());
                        } else {
                            // Comes back as a file stream, into Downloads.
                            this.session
                                .control(json!({ "t": "filesFetch", "path": open }));
                        }
                    }))
                    .into_any_element(),
            );
        }
        let here = path.clone();
        v_flex()
            .w(px(320.))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .h(px(36.))
                    .px_2()
                    .gap_1()
                    .child(
                        Button::new("files-up")
                            .xsmall()
                            .ghost()
                            .icon(I::ChevronLeft)
                            .disabled(path.is_empty())
                            .on_click(
                                cx.listener(move |this, _, _, _| this.list_files(up.clone())),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(if path.is_empty() {
                                "Home".to_string()
                            } else {
                                format!("~/{path}")
                            }),
                    )
                    .child(
                        Button::new("files-refresh")
                            .xsmall()
                            .ghost()
                            .icon(I::RefreshCw)
                            .on_click(cx.listener(|this, _, _, _| {
                                this.list_files(this.files.path.clone())
                            })),
                    )
                    .child(
                        Button::new("files-send")
                            .xsmall()
                            .ghost()
                            .icon(I::Plus)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let dir = here.clone();
                                let paths = cx.prompt_for_paths(PathPromptOptions {
                                    files: true,
                                    directories: true,
                                    multiple: true,
                                    prompt: Some("Send here".into()),
                                });
                                cx.spawn_in(window, async move |this, cx| {
                                    if let Ok(Ok(Some(paths))) = paths.await {
                                        let _ = this.update(cx, |v, _| {
                                            paths.into_iter().for_each(|p| {
                                                v.session.send_file(p, Some(dir.clone()))
                                            })
                                        });
                                    }
                                })
                                .detach();
                            })),
                    ),
            )
            .children(
                self.files
                    .error
                    .clone()
                    .map(|e| div().px_3().text_xs().text_color(theme.danger).child(e)),
            )
            .child(
                v_flex()
                    .id("files-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1()
                    .children(rows),
            )
    }

    fn agent_asks(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let owner = self.owner();
        v_flex()
            .absolute()
            .top_3()
            .left_3()
            .w(px(340.))
            .gap_2()
            .children(self.asks.iter().map(|a| {
                let id = a["id"].as_u64().unwrap_or(0);
                let kind = a["kind"].as_str().unwrap_or("confirm").to_string();
                let text = a["text"].as_str().unwrap_or_default();
                let text = if kind == "consent" {
                    text.rsplit_once(" [").map_or(text, |(t, _)| t).to_string()
                } else {
                    text.to_string()
                };
                let answer = |label: String, ok: bool, note: Option<String>, primary: bool| {
                    Button::new(SharedString::from(format!("ask-{id}-{label}")))
                        .xsmall()
                        .label(label)
                        .map(|b| if primary { b.primary() } else { b.ghost() })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.session.control(
                                json!({ "t": "agentAnswer", "id": id, "ok": ok, "note": note }),
                            );
                            this.asks.retain(|a| a["id"] != id);
                            cx.notify();
                        }))
                };
                let mut row = h_flex().gap_1().flex_wrap();
                if owner {
                    if kind == "consent" {
                        for c in a["choices"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c.as_str())
                        {
                            row = row.child(answer(c.to_string(), true, Some(c.to_string()), true));
                        }
                        row = row.child(answer("Deny".into(), false, None, false));
                    } else {
                        row = row.child(answer(
                            if kind == "help" {
                                "Done".into()
                            } else {
                                "Allow".into()
                            },
                            true,
                            None,
                            true,
                        ));
                        row = row.child(answer(
                            if kind == "help" {
                                "Can't".into()
                            } else {
                                "Don't".into()
                            },
                            false,
                            None,
                            false,
                        ));
                    }
                }
                v_flex()
                    .p_3()
                    .gap_2()
                    .rounded(px(10.))
                    .bg(theme.popover)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_lg()
                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(
                        match kind.as_str() {
                            "help" => "An agent needs a hand",
                            "consent" => "An agent wants to sign in",
                            _ => "An agent asks",
                        },
                    ))
                    .child(div().text_sm().child(text))
                    .child(row)
            }))
    }

    /// The pen colours, and a way to clear what you drew.
    fn pens(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        h_flex()
            .absolute()
            .bottom_3()
            .left_3()
            .p_1()
            .gap_1()
            .rounded(px(10.))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .children(PENS.iter().enumerate().map(|(i, (_, c))| {
                div()
                    .id(SharedString::from(format!("pen-{i}")))
                    .size(px(20.))
                    .rounded_full()
                    .bg(gpui_kit::rgb(*c))
                    .border_2()
                    .border_color(if i == self.pen {
                        theme.foreground
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pen = i;
                        cx.notify();
                    }))
            }))
            .child(
                Button::new("pen-clear")
                    .xsmall()
                    .ghost()
                    .label("Clear")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.session.control(json!({ "t": "annotateClear" }));
                        let me = format!("{}:", this.welcome()["session"]);
                        this.strokes.retain(|k, _| !k.starts_with(&me));
                        cx.notify();
                    })),
            )
            .child(
                Button::new("pen-done")
                    .xsmall()
                    .ghost()
                    .label("Done")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.drawing = false;
                        cx.notify();
                    })),
            )
    }
}

fn human(n: u64) -> String {
    match n {
        0..1_000 => format!("{n} B"),
        1_000..1_000_000 => format!("{:.0} KB", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", n as f64 / 1e6),
        _ => format!("{:.1} GB", n as f64 / 1e9),
    }
}

fn dist(r: Bounds<Pixels>, p: Point<Pixels>) -> f32 {
    let c = r.center();
    let (dx, dy) = (f32::from(c.x - p.x), f32::from(c.y - p.y));
    dx * dx + dy * dy
}

use std::rc::Rc;

impl Render for RemoteView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = &window;
        let viewer = self.session.viewer.clone();
        let status = viewer.status();
        let this = cx.entity().downgrade();
        let theme = cx.theme().clone();

        let mut area = div()
            .id("remote-screen")
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .h_full()
            .bg(gpui_kit::black())
            .track_focus(&self.focus)
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.drawing {
                    if ev.pressed_button == Some(MouseButton::Left) {
                        this.pen_move(ev.position);
                        cx.notify();
                    }
                    return;
                }
                if this.relative.load(Ordering::Relaxed) {
                    return;
                }
                this.move_to(ev.position)
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, _| {
                if !hovered {
                    this.over_picture = false;
                    this.cursors.arrow();
                }
            }))
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, _| {
                let (dx, dy) = match ev.delta {
                    ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
                    ScrollDelta::Lines(l) => (l.x * 16., l.y * 16.),
                };
                // The wire's sign is the web's: positive moves content up/left.
                this.session.input(InputEvent::Scroll { dx: -dx, dy: -dy });
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| this.key(ev, cx)))
            .on_modifiers_changed(
                cx.listener(|this, ev: &ModifiersChangedEvent, _, _| this.modifiers(&ev.modifiers)),
            )
            // Files dropped on the picture go to the other computer.
            .on_drag_move(
                cx.listener(|this, _: &DragMoveEvent<ExternalPaths>, _, cx| {
                    if !this.dragging_files {
                        this.dragging_files = true;
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.dragging_files = false;
                for p in paths.paths() {
                    this.session.send_file(p.clone(), None);
                }
                cx.notify();
            }));
        for b in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            area = area
                .on_mouse_down(
                    b,
                    cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                        if !this.focus.is_focused(window) {
                            window.focus(&this.focus, cx);
                            this.sync_capture(window, cx);
                        }
                        if this.drawing {
                            if b == MouseButton::Left {
                                this.pen_down(ev.position);
                                cx.notify();
                            }
                            return;
                        }
                        this.move_to(ev.position);
                        if let Some(button) = Self::button(b) {
                            this.session.input(InputEvent::MouseDown { button });
                        }
                    }),
                )
                .on_mouse_up(
                    b,
                    cx.listener(move |this, ev: &MouseUpEvent, _, _| {
                        if this.drawing {
                            this.flush_stroke(true);
                            return;
                        }
                        this.move_to(ev.position);
                        if let Some(button) = Self::button(b) {
                            this.session.input(InputEvent::MouseUp { button });
                        }
                    }),
                )
                .on_mouse_up_out(
                    b,
                    cx.listener(move |this, _: &MouseUpEvent, _, _| {
                        if this.drawing {
                            this.flush_stroke(true);
                            return;
                        }
                        if let Some(button) = Self::button(b) {
                            this.session.input(InputEvent::MouseUp { button });
                        }
                    }),
                );
        }

        // Each display's picture where the layout puts it, sharp-text pictures on top.
        let origin = self.bounds.origin;
        let lossless = viewer.lossless();
        let mut keep = HashMap::new();
        for (i, r) in self.layout() {
            let local = Bounds::new(point(r.origin.x - origin.x, r.origin.y - origin.y), r.size);
            if let Some(pic) = viewer.picture(i) {
                // A Mac draws the decoder's IOSurface as it is; Windows, the decoded image.
                #[cfg(target_os = "macos")]
                let picture = surface(pic.0).object_fit(ObjectFit::Fill);
                #[cfg(windows)]
                let picture = {
                    // The image before this one leaves the GPU's atlas.
                    if let Some(old) = self
                        .shown
                        .insert(i, pic.0.clone())
                        .filter(|o| !Arc::ptr_eq(o, &pic.0))
                    {
                        let _ = window.drop_image(old);
                    }
                    img(ImageSource::Render(pic.0)).object_fit(ObjectFit::Fill)
                };
                area = area.child(
                    picture
                        .absolute()
                        .left(local.origin.x)
                        .top(local.origin.y)
                        .w(local.size.width)
                        .h(local.size.height),
                );
            }
            for l in lossless
                .iter()
                .filter(|l| l.stream == i && l.fw > 0 && l.fh > 0)
            {
                let key = Arc::as_ptr(&l.png) as usize;
                let image = self.lossless_images.get(&key).cloned().unwrap_or_else(|| {
                    Arc::new(Image::from_bytes(ImageFormat::Png, l.png.to_vec()))
                });
                keep.insert(key, image.clone());
                let (sx, sy) = (
                    f32::from(local.size.width) / l.fw as f32,
                    f32::from(local.size.height) / l.fh as f32,
                );
                area = area.child(
                    img(ImageSource::Image(image))
                        .absolute()
                        .left(local.origin.x + px(l.x as f32 * sx))
                        .top(local.origin.y + px(l.y as f32 * sy))
                        .w(px(l.w as f32 * sx))
                        .h(px(l.h as f32 * sy)),
                );
            }
        }
        self.lossless_images = keep;

        // Strokes, over the pictures they were drawn on.
        if !self.strokes.is_empty() {
            let rects: HashMap<usize, Bounds<Pixels>> = self.layout().into_iter().collect();
            let lines: Vec<(Hsla, Vec<Point<Pixels>>)> = self
                .strokes
                .values()
                .filter_map(|s| {
                    let r = rects.get(&s.stream)?;
                    let age = s.updated.elapsed();
                    let alpha = if age < FADE_AFTER {
                        1.
                    } else {
                        1. - (age - FADE_AFTER).as_secs_f32() / FADE.as_secs_f32()
                    };
                    let pts = s
                        .points
                        .iter()
                        .map(|p| {
                            point(
                                r.origin.x + r.size.width * p[0],
                                r.origin.y + r.size.height * p[1],
                            )
                        })
                        .collect();
                    Some((s.color.opacity(alpha.clamp(0., 1.)), pts))
                })
                .collect();
            area = area.child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        for (color, pts) in lines {
                            let mut path = PathBuilder::stroke(px(4.));
                            let Some(first) = pts.first() else { continue };
                            path.move_to(*first);
                            if pts.len() == 1 {
                                path.line_to(*first + point(px(0.5), px(0.)));
                            }
                            for p in &pts[1..] {
                                path.line_to(*p);
                            }
                            if let Ok(path) = path.build() {
                                window.paint_path(path, color);
                            }
                        }
                    },
                )
                .absolute()
                .inset_0()
                .size_full(),
            );
        }

        // An agent's questions, for the owner.
        if !self.asks.is_empty() {
            area = area.child(self.agent_asks(cx));
        }
        if self.drawing {
            area = area.child(self.pens(cx));
        }

        // Where the area is, for mapping the pointer and sizing the stream; and the text
        // that isn't keys (IME, emoji, accents) goes in here.
        let (focus, entity) = (self.focus.clone(), cx.entity());
        area = area.child(
            canvas(
                move |bounds, window, cx| {
                    let scale = window.scale_factor();
                    let _ = this.update(cx, |v, _| v.laid_out(bounds, scale));
                },
                move |bounds, _, window, cx| {
                    window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx)
                },
            )
            .absolute()
            .inset_0()
            .size_full(),
        );

        if self.dragging_files {
            area = area.child(
                div()
                    .absolute()
                    .inset_4()
                    .rounded(px(12.))
                    .border_2()
                    .border_color(theme.primary)
                    .bg(theme.primary.opacity(0.12))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(gpui_kit::white())
                    .child(format!("Drop to send to {}", self.session.target.name)),
            );
        }

        if status != Status::Live {
            let (text, color) = match &status {
                Status::Refused(m) => (m.clone(), theme.danger),
                Status::Connecting(m) => (m.clone(), gpui_kit::white().opacity(0.8)),
                Status::Live => unreachable!(),
            };
            area = area.child(
                v_flex()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .p_8()
                    .child(
                        div()
                            .max_w(px(460.))
                            .text_center()
                            .text_sm()
                            .text_color(color)
                            .child(text),
                    ),
            );
        }

        let full = self.full_screen;
        v_flex()
            .relative()
            .size_full()
            .when(!full, |d| d.child(self.toolbar(cx)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(area)
                            .children(self.preview_strip(cx))
                            .children(self.terminal.clone().map(|t| {
                                div()
                                    .h(px(300.))
                                    .flex_none()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .child(t)
                            })),
                    )
                    .when(self.panel == Panel::Chat, |d| d.child(self.chat_panel(cx)))
                    .when(self.panel == Panel::Files, |d| {
                        d.child(self.files_panel(cx))
                    }),
            )
            .when(full, |d| {
                d.child(
                    div().absolute().top_2().right_2().child(
                        Button::new("remote-exit-full")
                            .small()
                            .ghost()
                            .icon(I::Minimize)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_full_screen(window, cx)
                            })),
                    ),
                )
            })
    }
}

impl Focusable for RemoteView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for RemoteView {
    fn drop(&mut self) {
        // The pointer never stays locked or hidden after the viewer goes.
        if self.relative.load(Ordering::Relaxed) {
            crate::viewer::sys::lock_pointer(false);
        }
    }
}

/// Text typed into the picture: whatever this computer's keyboard and input methods make
/// of the keys, typed there as it is.
impl EntityInputHandler for RemoteView {
    fn text_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        Some(String::new())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        if !text.is_empty() && !text.chars().any(char::is_control) {
            self.session.text(text);
        }
    }

    // A composition in progress (IME) is the input method's to show; only what's committed goes.
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        _: &str,
        _: Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    fn bounds_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        element: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds::new(element.origin, size(px(1.), px(20.))))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
