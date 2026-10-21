//! `glass --test`: deterministic fakes for end-to-end tests. A generated test pattern on
//! two fake displays replaces screen capture, input is recorded instead of injected, the
//! clipboard and file chooser are in memory, and HTTP hooks inject faults. Nothing here
//! touches the real screen, mouse, keyboard or clipboard.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::Json;
use axum::extract::Query;
use axum::routing::{get, post};
use glass_proto::{
    AUDIO_FRAMES_PER_PACKET, AUDIO_SAMPLE_RATE, InputEvent, MAX_STREAMS, audio_packet,
};
use tokio::sync::broadcast;

use crate::clock::now_us;
use crate::control::ServerMsg;
use crate::media::{
    AudioPackets, Clipboard, DisplayInfo, EncodedFrame, InputSink, Layout, VideoSource,
};
use crate::soft_encode::{Encoder, Frame, PixelFormat};

const WIDTH: usize = 640;
const HEIGHT: usize = 360;
const DISPLAYS: usize = 2;

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Frames to drop before sending (simulated loss).
static DROP_FRAMES: AtomicU32 = AtomicU32::new(0);
/// Corrupt the next frame after its checksum is computed.
static CORRUPT_NEXT: AtomicBool = AtomicBool::new(false);
/// Self-update requests, readable at /test/updates.
static UPDATES: AtomicU32 = AtomicU32::new(0);

pub fn record_update() {
    UPDATES.fetch_add(1, Ordering::Relaxed);
}

/// Current adaptive bitrate, readable at /test/bitrate.
static BITRATE: AtomicU32 = AtomicU32::new(0);

pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The OS test mode reports to clients: `GLASS_TEST_OS` (macos or linux), default macos,
/// so client behaviour under test doesn't depend on the machine running the tests.
pub fn host_os() -> &'static str {
    match std::env::var("GLASS_TEST_OS").as_deref() {
        Ok("linux") => "linux",
        _ => "macos",
    }
}

/// Server hook: true if this frame should be dropped (its id still advances).
pub fn take_drop() -> bool {
    if !enabled() {
        return false;
    }
    let mut n = DROP_FRAMES.load(Ordering::Relaxed);
    while n > 0 {
        match DROP_FRAMES.compare_exchange_weak(n, n - 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(current) => n = current,
        }
    }
    false
}

/// Server hook: true if this frame should be corrupted in flight.
pub fn take_corrupt() -> bool {
    enabled() && CORRUPT_NEXT.swap(false, Ordering::Relaxed)
}

// ---- Video: a moving test pattern per fake display ----

pub struct TestVideo {
    displays: Vec<DisplayInfo>,
    slots: Vec<Arc<AtomicI32>>,
    keyframes: Vec<Arc<AtomicBool>>,
    shown: Mutex<Vec<u32>>,
}

impl TestVideo {
    pub fn new(frames: broadcast::Sender<Arc<EncodedFrame>>, fps: u32, bitrate: u32) -> Self {
        BITRATE.store(bitrate, Ordering::Relaxed);
        let displays: Vec<DisplayInfo> = (0..DISPLAYS)
            .map(|i| DisplayInfo {
                id: i as u32,
                width: WIDTH,
                height: HEIGHT,
                x: (i * WIDTH) as f64,
                y: 0.0,
                points_width: WIDTH as f64,
                points_height: HEIGHT as f64,
                main: i == 0,
                builtin: false,
            })
            .collect();
        let slots: Vec<_> = (0..DISPLAYS)
            .map(|_| Arc::new(AtomicI32::new(-1)))
            .collect();
        let keyframes: Vec<_> = (0..DISPLAYS)
            .map(|_| Arc::new(AtomicBool::new(true)))
            .collect();
        for i in 0..DISPLAYS {
            let (slot, keyframe, frames) = (slots[i].clone(), keyframes[i].clone(), frames.clone());
            std::thread::spawn(move || pattern_loop(i, slot, keyframe, frames, fps));
        }
        Self {
            displays,
            slots,
            keyframes,
            shown: Mutex::new(Vec::new()),
        }
    }
}

impl VideoSource for TestVideo {
    fn streams(&self) -> Vec<DisplayInfo> {
        self.shown
            .lock()
            .unwrap()
            .iter()
            .map(|&i| self.displays[i as usize].clone())
            .collect()
    }

    fn request_keyframe(&self, stream: Option<u8>) {
        for (i, slot) in self.slots.iter().enumerate() {
            let s = slot.load(Ordering::Relaxed);
            if s >= 0 && stream.is_none_or(|n| n as i32 == s) {
                self.keyframes[i].store(true, Ordering::Relaxed);
            }
        }
    }

    fn set_layout(&self, layout: Layout) -> Result<Vec<DisplayInfo>> {
        let ids: Vec<u32> = match layout {
            Layout::Single(Some(id)) => {
                anyhow::ensure!((id as usize) < DISPLAYS, "unknown display");
                vec![id]
            }
            Layout::Single(None) => vec![0],
            Layout::All => (0..DISPLAYS.min(MAX_STREAMS) as u32).collect(),
        };
        for slot in &self.slots {
            slot.store(-1, Ordering::Relaxed);
        }
        for (n, &id) in ids.iter().enumerate() {
            self.slots[id as usize].store(n as i32, Ordering::Relaxed);
            self.keyframes[id as usize].store(true, Ordering::Relaxed);
        }
        *self.shown.lock().unwrap() = ids.clone();
        Ok(ids
            .iter()
            .map(|&i| self.displays[i as usize].clone())
            .collect())
    }

    fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Ok(self.displays.clone())
    }

    fn set_bitrate(&self, bps: u32) {
        BITRATE.store(bps, Ordering::Relaxed);
    }
}

fn pattern_loop(
    display: usize,
    slot: Arc<AtomicI32>,
    keyframe: Arc<AtomicBool>,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    fps: u32,
) {
    let mut applied = BITRATE.load(Ordering::Relaxed);
    let mut encoder = Encoder::new(WIDTH, HEIGHT, fps, applied, 1.0).expect("test encoder");
    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let mut next = Instant::now();
    for n in 0u64.. {
        next += interval;
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        let s = slot.load(Ordering::Relaxed);
        if s < 0 {
            continue;
        }
        let target = BITRATE.load(Ordering::Relaxed);
        if target != applied {
            encoder.set_bitrate(target);
            applied = target;
        }
        let frame = render(display, n);
        let force = keyframe.swap(false, Ordering::Relaxed);
        if let Ok(Some((data, key))) = encoder.encode(&frame, force) {
            let _ = frames.send(Arc::new(EncodedFrame {
                stream: s as u8,
                data,
                keyframe: key,
                capture_us: now_us(),
            }));
        }
    }
}

/// A distinct background per display with a bar sweeping across.
fn render(display: usize, n: u64) -> Frame {
    let mut data = vec![0u8; WIDTH * HEIGHT * 4];
    let bar = (n as usize * 8) % WIDTH;
    let bg = if display == 0 {
        [200, 80, 30]
    } else {
        [30, 160, 90]
    };
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let p = &mut data[(y * WIDTH + x) * 4..][..4];
            let on_bar = x >= bar && x < bar + 24;
            let [b, g, r] = if on_bar { [255, 255, 255] } else { bg };
            p.copy_from_slice(&[b, g, r, 255]);
        }
    }
    Frame {
        width: WIDTH,
        height: HEIGHT,
        stride: WIDTH * 4,
        format: PixelFormat::Bgrx,
        data,
    }
}

// ---- Audio: a 440 Hz tone ----

pub fn start_tone(packets: AudioPackets) {
    std::thread::spawn(move || {
        let per_packet = Duration::from_micros(
            AUDIO_FRAMES_PER_PACKET as u64 * 1_000_000 / AUDIO_SAMPLE_RATE as u64,
        );
        let mut next = Instant::now();
        let mut t = 0u64;
        for seq in 0u32.. {
            next += per_packet;
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
            let pcm: Vec<i16> = (0..AUDIO_FRAMES_PER_PACKET)
                .flat_map(|_| {
                    t += 1;
                    let v = ((t as f64 * 440.0 * std::f64::consts::TAU / AUDIO_SAMPLE_RATE as f64)
                        .sin()
                        * 8000.0) as i16;
                    [v, v]
                })
                .collect();
            let _ = packets.send(Arc::new(audio_packet(seq, now_us(), 2, &pcm)));
        }
    });
}

// ---- Input: recorded ----

#[derive(Default)]
struct Recorded {
    events: Vec<serde_json::Value>,
    pastes: u32,
}

static INPUT: Mutex<Recorded> = Mutex::new(Recorded {
    events: Vec::new(),
    pastes: 0,
});

pub struct FakeInput;

impl InputSink for FakeInput {
    fn handle(&mut self, ev: InputEvent) {
        let json = match ev {
            InputEvent::MouseMove { x, y, stream } => {
                serde_json::json!({"t": "move", "x": x, "y": y, "stream": stream})
            }
            InputEvent::MouseDown { button } => serde_json::json!({"t": "down", "button": button}),
            InputEvent::MouseUp { button } => serde_json::json!({"t": "up", "button": button}),
            InputEvent::Scroll { dx, dy } => serde_json::json!({"t": "scroll", "dx": dx, "dy": dy}),
            InputEvent::KeyDown { keycode } => {
                serde_json::json!({"t": "keydown", "keycode": keycode})
            }
            InputEvent::KeyUp { keycode } => serde_json::json!({"t": "keyup", "keycode": keycode}),
        };
        INPUT.lock().unwrap().events.push(json);
    }

    fn paste(&mut self) {
        INPUT.lock().unwrap().pastes += 1;
    }

    fn release_all(&mut self) {}

    fn set_displays(&mut self, _displays: &[u32]) {}
}

// ---- Clipboard: in memory ----

static CLIPBOARD: Mutex<String> = Mutex::new(String::new());

pub struct FakeClipboard;

impl Clipboard for FakeClipboard {
    fn set_text(&self, text: &str) {
        *CLIPBOARD.lock().unwrap() = text.to_owned();
    }
}

// ---- Files ----

/// What "Get files from Mac" returns in test mode: a generated file.
pub fn picked_files() -> Vec<PathBuf> {
    let path = std::env::temp_dir().join(format!("glass-test-pick-{}.txt", std::process::id()));
    let _ = std::fs::write(&path, b"hello from the glass test host\n");
    vec![path]
}

// ---- Test-only HTTP routes ----

#[derive(serde::Deserialize)]
struct DropQuery {
    frames: u32,
}

pub fn routes(events: broadcast::Sender<ServerMsg>, downloads: PathBuf) -> axum::Router {
    axum::Router::new()
        .route(
            "/test/input",
            get(|| async {
                let r = INPUT.lock().unwrap();
                Json(serde_json::json!({"events": r.events, "pastes": r.pastes}))
            })
            .delete(|| async {
                *INPUT.lock().unwrap() = Recorded::default();
            }),
        )
        .route(
            "/test/clipboard",
            get(|| async { CLIPBOARD.lock().unwrap().clone() }).post(move |text: String| {
                let events = events.clone();
                async move {
                    *CLIPBOARD.lock().unwrap() = text.clone();
                    let _ = events.send(ServerMsg::Clipboard { text });
                }
            }),
        )
        .route(
            "/test/drop",
            post(|Query(q): Query<DropQuery>| async move { DROP_FRAMES.store(q.frames, Ordering::Relaxed) }),
        )
        .route("/test/updates", get(|| async { Json(serde_json::json!({"count": UPDATES.load(Ordering::Relaxed)})) }))
        .route("/test/bitrate", get(|| async { Json(serde_json::json!({"bps": BITRATE.load(Ordering::Relaxed)})) }))
        .route("/test/corrupt", post(|| async { CORRUPT_NEXT.store(true, Ordering::Relaxed) }))
        .route(
            "/test/downloads",
            get(move || {
                let dir = downloads.clone();
                async move {
                    let files: Vec<serde_json::Value> = std::fs::read_dir(&dir)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter_map(|e| {
                            let meta = e.metadata().ok()?;
                            Some(serde_json::json!({"name": e.file_name().to_string_lossy(), "size": meta.len()}))
                        })
                        .collect();
                    Json(serde_json::json!({"files": files}))
                }
            }),
        )
}
