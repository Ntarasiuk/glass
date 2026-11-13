//! Captures the monitors shared through the portal (one PipeWire stream each), encodes
//! each on its own thread, and maps them to stream numbers according to the layout.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use glass_proto::MAX_STREAMS;
use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;
use tokio::sync::broadcast;
use tracing::{info, warn};

use super::{audio, clock::now_us, permissions, portal};
use crate::mailbox::{FrameSender, Mailbox};
use crate::media::VideoSource;
pub use crate::media::{AudioPackets, DisplayInfo, EncodedFrame, Layout};
use crate::soft_encode::{Encoder, Frame, PixelFormat};

/// How long an encoder waits for a new frame before checking for a keyframe request it
/// can satisfy by re-encoding the last one (static screens produce no new frames).
const IDLE_POLL: Duration = Duration::from_millis(100);

pub struct VideoConfig {
    pub fps: i32,
    pub bitrate: i32,
    pub scale: f64,
}

/// A rectangle in the compositor's logical (scaled) coordinates.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

struct Monitor {
    info: Mutex<DisplayInfo>,
    /// Stream number this monitor is sent as, or -1 when not shown.
    slot: AtomicI32,
    keyframe: AtomicBool,
    /// Adaptive target bitrate; the encoder thread applies changes.
    bitrate: std::sync::atomic::AtomicU32,
    /// The latest picture in BGRA, kept only while sharp text wants lossless refreshes.
    latest: Mutex<Option<Arc<crate::lossless::RawFrame>>>,
    /// Bumped with each new captured picture.
    generation: std::sync::atomic::AtomicU64,
    /// A small picture for display previews, refreshed while viewers want them.
    thumb: Mutex<Option<(std::time::Instant, Arc<crate::lossless::RawFrame>)>>,
}

/// Pictures (display previews, agents' screenshots) are wanted until then; each request
/// extends it. Kept at up to PICTURE_MAX and scaled down further on request.
static THUMBS_UNTIL: Mutex<Option<std::time::Instant>> = Mutex::new(None);
const THUMB_EVERY: Duration = Duration::from_millis(300);
const PICTURE_MAX: [u32; 2] = [1600, 1600];

/// Lossless refreshes wanted (sharp text): keep a BGRA copy of each new picture.
static FULL_COLOUR: AtomicBool = AtomicBool::new(false);

/// Monitor rects for input mapping, by display id.
static RECTS: Mutex<Vec<Rect>> = Mutex::new(Vec::new());

pub fn rect_of(id: u32) -> Option<Rect> {
    RECTS.lock().unwrap().get(id as usize).copied()
}

/// Bounding box of every monitor (what an absolute pointer device spans).
pub fn layout_rect() -> Rect {
    let monitors = hyprland_monitors();
    let rects = if monitors.is_empty() {
        RECTS.lock().unwrap().clone()
    } else {
        monitors
    };
    let x0 = rects.iter().map(|r| r.x).fold(f64::INFINITY, f64::min);
    let y0 = rects.iter().map(|r| r.y).fold(f64::INFINITY, f64::min);
    let x1 = rects
        .iter()
        .map(|r| r.x + r.w)
        .fold(f64::NEG_INFINITY, f64::max);
    let y1 = rects
        .iter()
        .map(|r| r.y + r.h)
        .fold(f64::NEG_INFINITY, f64::max);
    if rects.is_empty() {
        return Rect {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
    }
    Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// Logical monitor rects from `hyprctl monitors -j`, if running under Hyprland.
fn hyprland_monitors() -> Vec<Rect> {
    let Ok(out) = std::process::Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
    else {
        return Vec::new();
    };
    let Ok(list) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|m| {
            let scale = m["scale"].as_f64().unwrap_or(1.0).max(0.1);
            let (mut w, mut h) = (m["width"].as_f64()? / scale, m["height"].as_f64()? / scale);
            if m["transform"].as_i64().unwrap_or(0) % 2 == 1 {
                std::mem::swap(&mut w, &mut h);
            }
            Some(Rect {
                x: m["x"].as_f64()?,
                y: m["y"].as_f64()?,
                w,
                h,
            })
        })
        .collect()
}

pub struct Video {
    config: VideoConfig,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    /// `None` until the portal approves a share, and again once that share has ended.
    monitors: Arc<Mutex<Option<Vec<Arc<Monitor>>>>>,
    shown: Mutex<Vec<u32>>,
    /// What's asked to be streamed, to go back to when a share that ended is replaced.
    layout: Mutex<Option<Layout>>,
    /// One portal session being started at a time. Apart from `monitors`, as it can wait as long
    /// as the share dialog stays open, and viewers' bitrates and keyframes shouldn't.
    starting: Mutex<()>,
    /// A share that ended, and how it's going getting a new one.
    ended: Arc<Mutex<Option<Rejoin>>>,
}

/// After a share ends (PipeWire restarted, the compositor closed it, the person at the computer
/// stopped it), a new one is asked for when it's time. That's silent while the restore token
/// holds; otherwise it shows the share dialog, so once that's turned down Glass doesn't ask
/// again by itself. A viewer choosing what to show still does.
struct Rejoin {
    at: std::time::Instant,
    tries: u32,
    declined: bool,
}

impl Rejoin {
    fn new() -> Self {
        Self { at: std::time::Instant::now() + REJOIN_FIRST, tries: 0, declined: false }
    }

    fn failed(&mut self, declined: bool) {
        self.declined = declined;
        self.tries += 1;
        self.at = std::time::Instant::now() + REJOIN_FIRST.saturating_mul(1 << self.tries.min(6)).min(REJOIN_MAX);
    }

    fn due(&self) -> bool {
        !self.declined && std::time::Instant::now() >= self.at
    }
}

const REJOIN_FIRST: Duration = Duration::from_secs(5);
const REJOIN_MAX: Duration = Duration::from_secs(300);

impl Video {
    pub fn new(
        config: VideoConfig,
        frames: broadcast::Sender<Arc<EncodedFrame>>,
        audio: AudioPackets,
    ) -> Self {
        audio::start(audio);
        gpu_report();
        Self {
            config,
            frames,
            monitors: Arc::new(Mutex::new(None)),
            shown: Mutex::new(Vec::new()),
            layout: Mutex::new(None),
            starting: Mutex::new(()),
            ended: Arc::new(Mutex::new(None)),
        }
    }

    pub fn streams(&self) -> Vec<DisplayInfo> {
        let monitors = self.monitors.lock().unwrap();
        let Some(monitors) = monitors.as_ref() else {
            return Vec::new();
        };
        self.shown
            .lock()
            .unwrap()
            .iter()
            .filter_map(|&id| monitors.get(id as usize))
            .map(|m| m.info.lock().unwrap().clone())
            .collect()
    }

    pub fn request_keyframe(&self, stream: Option<u8>) {
        if let Some(monitors) = self.monitors.lock().unwrap().as_ref() {
            for m in monitors {
                let slot = m.slot.load(Ordering::Relaxed);
                if slot >= 0 && stream.is_none_or(|s| s as i32 == slot) {
                    m.keyframe.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    /// Chooses which shared monitors are streamed. The first call starts the portal
    /// session (the share dialog) and blocks until it's approved.
    pub fn set_layout(&self, layout: Layout) -> Result<Vec<DisplayInfo>> {
        let monitors = self.ensure_started()?;
        let shown = self.show(&monitors, layout)?;
        *self.layout.lock().unwrap() = Some(layout);
        Ok(shown)
    }

    /// Maps `monitors` to stream numbers.
    fn show(&self, monitors: &[Arc<Monitor>], layout: Layout) -> Result<Vec<DisplayInfo>> {
        let ids: Vec<u32> = match layout {
            Layout::Single(Some(id)) => {
                anyhow::ensure!((id as usize) < monitors.len(), "unknown display");
                vec![id]
            }
            Layout::Single(None) => vec![0],
            Layout::Window(_) => anyhow::bail!("streaming one window isn't available on Linux yet"),
            Layout::All => {
                // Left to right, then top to bottom, like the compositor arrangement.
                let mut all: Vec<(u32, f64, f64)> = monitors
                    .iter()
                    .enumerate()
                    .take(MAX_STREAMS)
                    .map(|(i, m)| {
                        let info = m.info.lock().unwrap();
                        (i as u32, info.x, info.y)
                    })
                    .collect();
                all.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.2.total_cmp(&b.2)));
                all.into_iter().map(|(i, ..)| i).collect()
            }
        };
        for m in monitors {
            m.slot.store(-1, Ordering::Release);
        }
        for (slot, &id) in ids.iter().enumerate() {
            // The keyframe first: whoever sees the slot sees that it's wanted.
            monitors[id as usize]
                .keyframe
                .store(true, Ordering::Relaxed);
            monitors[id as usize]
                .slot
                .store(slot as i32, Ordering::Release);
        }
        *self.shown.lock().unwrap() = ids.clone();
        Ok(ids
            .iter()
            .map(|&id| monitors[id as usize].info.lock().unwrap().clone())
            .collect())
    }

    pub fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Ok(self
            .ensure_started()?
            .iter()
            .map(|m| m.info.lock().unwrap().clone())
            .collect())
    }

    fn ensure_started(&self) -> Result<Vec<Arc<Monitor>>> {
        let _one = self.starting.lock().unwrap();
        if let Some(m) = self.monitors.lock().unwrap().as_ref() {
            return Ok(m.clone());
        }
        let cast = portal::start()?;
        permissions::set_screen_cast_granted(true);
        info!("screen cast approved: {} monitor(s)", cast.streams.len());

        let hypr = hyprland_monitors();
        let mut rects = Vec::new();
        let mut monitors = Vec::new();
        let mut captures = Vec::new();
        for (i, s) in cast.streams.iter().enumerate() {
            let (w, h) = s
                .size
                .map(|(w, h)| (w as f64, h as f64))
                .unwrap_or((1920.0, 1080.0));
            // Prefer the portal's position; otherwise match a Hyprland monitor by size.
            let (x, y) = s
                .position
                .map(|(x, y)| (x as f64, y as f64))
                .or_else(|| {
                    hypr.iter()
                        .find(|r| (r.w - w).abs() < 1.0 && (r.h - h).abs() < 1.0)
                        .map(|r| (r.x, r.y))
                })
                .unwrap_or((0.0, 0.0));
            rects.push(Rect { x, y, w, h });
            let monitor = Arc::new(Monitor {
                info: Mutex::new(DisplayInfo {
                    id: i as u32,
                    width: w as usize,
                    height: h as usize,
                    x,
                    y,
                    points_width: w,
                    points_height: h,
                    main: i == 0,
                    builtin: false,
                }),
                slot: AtomicI32::new(-1),
                latest: Mutex::new(None),
                thumb: Mutex::new(None),
                generation: std::sync::atomic::AtomicU64::new(0),
                keyframe: AtomicBool::new(true),
                bitrate: std::sync::atomic::AtomicU32::new(self.config.bitrate as u32),
            });
            let rx = Arc::new(Mailbox::default());
            let tx = FrameSender(rx.clone());
            let (fps, bitrate, scale) = (
                self.config.fps as u32,
                self.config.bitrate as u32,
                self.config.scale,
            );
            let (frames, m) = (self.frames.clone(), monitor.clone());
            std::thread::Builder::new()
                .name(format!("glass-encode-{i}"))
                .spawn(move || encode_loop(m, rx, frames, fps, bitrate, scale))?;
            captures.push((s.node_id, monitor.clone(), tx));
            monitors.push(monitor);
        }
        *RECTS.lock().unwrap() = rects;

        let (fd, session) = (cast.fd, cast.session);
        let (all, ended) = (self.monitors.clone(), self.ended.clone());
        std::thread::Builder::new()
            .name("glass-pipewire".into())
            .spawn(move || {
                match run_pipewire(fd, captures) {
                    Ok(()) => warn!("the screen share ended"),
                    Err(e) => warn!("pipewire capture stopped: {e:#}"),
                }
                // Its encoders have ended with it (their mailboxes closed). What's left is to
                // say so, rather than go on looking like a frozen screen.
                drop(session);
                permissions::set_screen_cast_granted(false);
                *all.lock().unwrap() = None;
                *ended.lock().unwrap() = Some(Rejoin::new());
            })
            .context("starting capture thread")?;

        *self.monitors.lock().unwrap() = Some(monitors.clone());
        *self.ended.lock().unwrap() = None;
        // A share that ended is being replaced: back to what was streaming.
        let layout = *self.layout.lock().unwrap();
        if let Some(layout) = layout
            && self.show(&monitors, layout).is_err()
        {
            let _ = self.show(&monitors, Layout::Single(None));
        }
        Ok(monitors)
    }

    /// Gets a new share when one has ended, when it's time.
    fn rejoin(&self) {
        if !self.ended.lock().unwrap().as_ref().is_some_and(Rejoin::due) {
            return;
        }
        match self.ensure_started() {
            Ok(_) => info!("the screen share is back"),
            Err(e) => {
                warn!("getting the screen share back: {e:#}");
                let declined = e.is::<portal::NotApproved>();
                if let Some(rejoin) = self.ended.lock().unwrap().as_mut() {
                    rejoin.failed(declined);
                }
            }
        }
    }
}

fn encode_loop(
    monitor: Arc<Monitor>,
    rx: Arc<Mailbox>,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    fps: u32,
    bitrate: u32,
    scale: f64,
) {
    let mut encoder: Option<VideoEncoder> = None;
    let mut applied_bitrate = bitrate;
    let mut last: Option<(Frame, u64)> = None;
    // The latest picture hasn't been encoded yet (held while a viewer's link is full).
    let mut unsent = false;
    // No encoder could be made: not again until then.
    let mut retry_at = std::time::Instant::now();
    loop {
        let wait = if unsent { crate::flow::POLL } else { IDLE_POLL };
        let fresh = match rx.recv_timeout(wait) {
            Ok(f) => Some(f),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let has_fresh = fresh.is_some();
        if let Some(f) = fresh
            && let Some((old, _)) = last.replace(f)
        {
            rx.recycle(old.data);
        }
        // Previews cover every monitor, shown or not.
        if has_fresh
            && THUMBS_UNTIL.lock().unwrap().is_some_and(|t| t > std::time::Instant::now())
            && monitor.thumb.lock().unwrap().as_ref().is_none_or(|(at, _)| at.elapsed() >= THUMB_EVERY)
            && let Some((frame, _)) = last.as_ref()
            && let Some(small) = crate::thumbs::downscale(
                &frame.data,
                frame.width,
                frame.height,
                frame.stride,
                matches!(frame.format, PixelFormat::Rgbx),
                PICTURE_MAX,
            )
        {
            *monitor.thumb.lock().unwrap() = Some((std::time::Instant::now(), Arc::new(small)));
        }
        let slot = monitor.slot.load(Ordering::Acquire);
        if slot < 0 {
            continue;
        }
        unsent |= has_fresh;
        // Nothing new: only re-encode the last frame to answer a keyframe request.
        if (!unsent && !monitor.keyframe.load(Ordering::Relaxed)) || std::time::Instant::now() < retry_at {
            continue;
        }
        let Some((frame, captured_us)) = last.as_ref() else {
            continue;
        };
        // Taken, not just cleared: one asked for since the look above is answered by this frame.
        let wants_key = monitor.keyframe.swap(false, Ordering::Relaxed);

        let size = (frame.width, frame.height);
        let mut force = wants_key;
        if encoder.as_ref().is_none_or(|e| !e.accepts(frame)) {
            match VideoEncoder::new(
                frame.width,
                frame.height,
                frame.format,
                fps,
                monitor.bitrate.load(Ordering::Relaxed),
                scale,
            ) {
                Ok(e) => encoder = Some(e),
                Err(e) => {
                    warn!("encoder: {e:#}");
                    retry_at = std::time::Instant::now() + Duration::from_secs(1);
                    monitor.keyframe.store(wants_key, Ordering::Relaxed);
                    continue;
                }
            }
            force = true;
            let mut info = monitor.info.lock().unwrap();
            (info.width, info.height) = size;
        }
        let target = monitor.bitrate.load(Ordering::Relaxed);
        if target != applied_bitrate {
            encoder.as_mut().unwrap().set_bitrate(target);
            applied_bitrate = target;
        }
        if has_fresh {
            monitor.generation.fetch_add(1, Ordering::Relaxed);
            // PipeWire hands over BGRx/RGBx memory as is: full colour, nothing to convert
            // back from YUV. Copied only while someone wants it.
            if FULL_COLOUR.load(Ordering::Relaxed)
                && let Some(bgra) = crate::lossless::to_bgra(
                    &frame.data,
                    frame.width,
                    frame.height,
                    frame.stride,
                    matches!(frame.format, PixelFormat::Rgbx),
                )
            {
                *monitor.latest.lock().unwrap() = Some(Arc::new(crate::lossless::RawFrame {
                    width: frame.width,
                    height: frame.height,
                    bgra,
                    capture_us: *captured_us,
                }));
            }
        }
        // A viewer's link is still busy (crate::flow): keep the picture and look again
        // shortly; the next frame carries every change since.
        if !wants_key && crate::flow::held(slot as u8) {
            continue;
        }
        let capture_us = if unsent { *captured_us } else { now_us() };
        unsent = false;
        match encoder.as_mut().unwrap().encode(frame, force) {
            Ok(Some((data, keyframe))) => {
                let _ = frames.send(Arc::new(EncodedFrame {
                    stream: slot as u8,
                    data,
                    keyframe,
                    capture_us,
                    hevc: false,
                    refresh: false,
                }));
            }
            Ok(None) => {}
            Err(e) => {
                warn!("encode: {e:#}");
                if encoder.as_ref().is_some_and(VideoEncoder::is_hardware) {
                    // Drop it; the picture goes again through OpenH264, as a keyframe, without
                    // waiting for the screen to change.
                    warn!("hardware encoding failed; using OpenH264 from now on");
                    HARDWARE_FAILED.store(true, Ordering::Relaxed);
                    encoder = None;
                    monitor.keyframe.store(true, Ordering::Relaxed);
                }
            }
        }
    }
}

/// Set once the GPU encoder has failed: the host stays on OpenH264 after that.
static HARDWARE_FAILED: AtomicBool = AtomicBool::new(false);

/// What `GLASS_ENCODER` asks for: `vaapi`, `nvenc`, or `gpu` (NVENC, else VA-API).
/// Anything else (or nothing) is OpenH264.
fn wanted_backends() -> &'static [&'static str] {
    match std::env::var("GLASS_ENCODER").unwrap_or_default().to_ascii_lowercase().as_str() {
        "vaapi" | "va" => &["vaapi"],
        "nvenc" | "nvidia" => &["nvenc"],
        "gpu" | "hardware" | "auto" => &["nvenc", "vaapi"],
        _ => &[],
    }
}

/// At startup, when `GLASS_ENCODER` asks for the GPU: which encoder will be used, or a
/// warning with what's missing and how to install it (the host then uses OpenH264).
fn gpu_report() {
    use super::va_encode::{Backend, available, missing};
    let wanted = wanted_backends();
    if wanted.is_empty() {
        return;
    }
    let backends: Vec<Backend> = wanted
        .iter()
        .map(|n| if *n == "nvenc" { Backend::Nvenc } else { Backend::Va })
        .collect();
    if let Some((backend, name)) =
        backends.iter().find_map(|&b| available(b).map(|name| (b, name)))
    {
        let version = super::gst_lib::lib().map(|l| l.version.clone()).unwrap_or_default();
        let path = super::gst_lib::lib().map(|l| l.path.display().to_string()).unwrap_or_default();
        info!("GPU encoding: {} ({name}, {version}, {path})", backend.label());
        return;
    }
    for backend in backends {
        if let Some(why) = missing(backend) {
            warn!("GPU encoding ({}) unavailable: {why}", backend.label());
        }
    }
    warn!("GLASS_ENCODER asks for the GPU, but no GPU encoder can be used: encoding with OpenH264 (software)");
}

/// OpenH264, or with `GLASS_ENCODER` the GPU through VA-API or NVENC. Opt-in for now:
/// with frames captured into system memory, copying them to a discrete GPU costs about
/// what software encoding does (the GPU path uses less CPU). Capturing DMA-BUFs would
/// remove the copy.
enum VideoEncoder {
    Soft(Encoder),
    Va(super::va_encode::VaEncoder),
}

impl VideoEncoder {
    fn new(
        width: usize,
        height: usize,
        format: PixelFormat,
        fps: u32,
        bitrate: u32,
        scale: f64,
    ) -> Result<Self> {
        let wanted = wanted_backends();
        if !HARDWARE_FAILED.load(Ordering::Relaxed) {
            use super::va_encode::{Backend, VaEncoder, fits};
            let mut tried = false;
            for name in wanted {
                let backend = if *name == "nvenc" { Backend::Nvenc } else { Backend::Va };
                if !fits(backend, width, height, scale) {
                    continue;
                }
                tried = true;
                match VaEncoder::new(backend, width, height, format, fps, bitrate, scale) {
                    Ok(e) => {
                        info!("encoder: {} ({})", e.name, backend.label());
                        return Ok(Self::Va(e));
                    }
                    Err(e) => match super::va_encode::missing(backend) {
                        Some(why) => warn!("{} unavailable: {why}", backend.label()),
                        None => warn!("{} encoder unavailable: {e:#}", backend.label()),
                    },
                }
            }
            if tried {
                warn!("no GPU encoder; using OpenH264");
                HARDWARE_FAILED.store(true, Ordering::Relaxed);
            }
        }
        Ok(Self::Soft(Encoder::new(width, height, fps, bitrate, scale)?))
    }

    fn is_hardware(&self) -> bool {
        !matches!(self, Self::Soft(_))
    }

    /// Whether this encoder takes the frame (same size; NVENC also the same byte order).
    fn accepts(&self, frame: &Frame) -> bool {
        match self {
            Self::Soft(e) => e.source_size() == (frame.width, frame.height),
            Self::Va(e) => e.accepts(frame),
        }
    }

    fn set_bitrate(&mut self, bps: u32) {
        match self {
            Self::Soft(e) => e.set_bitrate(bps),
            Self::Va(e) => e.set_bitrate(bps),
        }
    }

    fn encode(&mut self, frame: &Frame, force: bool) -> Result<Option<(Vec<u8>, bool)>> {
        match self {
            Self::Soft(e) => e.encode(frame, force),
            Self::Va(e) => e.encode(frame, force),
        }
    }
}

struct StreamState {
    format: spa::param::video::VideoInfoRaw,
    pixel: Option<PixelFormat>,
    tx: FrameSender,
    first: bool,
    id: usize,
    /// Compares frames for crate::damage when the compositor doesn't say what changed.
    differ: crate::damage::Differ,
}

fn run_pipewire(
    fd: std::os::fd::OwnedFd,
    captures: Vec<(u32, Arc<Monitor>, FrameSender)>,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;
    // A shared screen's node going away (the portal crashed or closed it) ends the share: its
    // stream only goes Paused, waiting for a source that won't come back. One already gone is
    // never announced, so a node not listed by the end of a round trip is gone too.
    let nodes: Vec<u32> = captures.iter().map(|(node, ..)| *node).collect();
    let listed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let registry = core.get_registry()?;
    let (quit, watched) = (mainloop.clone(), nodes.clone());
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let listed = listed.clone();
            move |global| listed.borrow_mut().push(global.id)
        })
        .global_remove(move |id| {
            if watched.contains(&id) {
                info!("screen node {id} is gone");
                quit.quit();
            }
        })
        .register();
    let listing = core.sync(0)?;
    // PipeWire going away ends the share.
    let (quit, done) = (mainloop.clone(), mainloop.clone());
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _seq, _res, message| {
            if id == pw::core::PW_ID_CORE {
                warn!("pipewire: {message}");
                quit.quit();
            }
        })
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE
                && seq == listing
                && let Some(node) = nodes.iter().find(|n| !listed.borrow().contains(n))
            {
                info!("screen node {node} was gone before capture started");
                done.quit();
            }
        })
        .register();

    let mut keep = Vec::new();
    for (i, (node, _monitor, tx)) in captures.into_iter().enumerate() {
        let stream = pw::stream::StreamBox::new(
            &core,
            "glass-screen",
            pw::properties::properties! {
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Screen",
            },
        )?;
        let state = StreamState {
            format: Default::default(),
            pixel: None,
            tx,
            first: true,
            id: i,
            differ: Default::default(),
        };
        let quit = mainloop.clone();
        let listener = stream
            .add_local_listener_with_user_data(state)
            // Its node gone (the share closed): it won't come back by itself.
            .state_changed(move |_stream, st, old, new| {
                if share_ended(&old, &new) {
                    info!("screen {}: {old:?} -> {new:?}", st.id);
                    quit.quit();
                }
            })
            .param_changed(|stream, st, id, param| {
                let Some(param) = param else { return };
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                if st.format.parse(param).is_err() {
                    return;
                }
                use spa::param::video::VideoFormat as F;
                st.pixel = match st.format.format() {
                    F::BGRx | F::BGRA => Some(PixelFormat::Bgrx),
                    F::RGBx | F::RGBA => Some(PixelFormat::Rgbx),
                    other => {
                        warn!("screen {}: unsupported pixel format {other:?}", st.id);
                        None
                    }
                };
                let size = st.format.size();
                info!(
                    "screen {}: {}x{} {:?}",
                    st.id,
                    size.width,
                    size.height,
                    st.format.format()
                );
                // Ask for what changed in each frame, for agents waiting on the screen.
                let meta = damage_params();
                if let Some(pod) = Pod::from_bytes(&meta)
                    && let Err(e) = stream.update_params(&mut [pod])
                {
                    warn!("screen {}: can't ask for damage: {e}", st.id);
                }
            })
            .process(|stream, st| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(pixel) = st.pixel else { return };
                let size = st.format.size();
                let (width, height) = (size.width as usize, size.height as usize);
                // Where it changed, when the compositor says (read before the pixels).
                let damage: Option<Vec<(u32, u32, u32, u32)>> = buffer
                    .find_meta::<spa::buffer::meta::MetaVideoDamage>()
                    .map(|d| {
                        d.iter()
                            .map(|r| {
                                let (p, s) = (r.position(), r.size());
                                let (x, y) = (p.x.max(0) as u32, p.y.max(0) as u32);
                                (x, y, x.saturating_add(s.width), y.saturating_add(s.height))
                            })
                            .collect()
                    })
                    .filter(|rects: &Vec<_>| !rects.is_empty());
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else {
                    return;
                };
                let chunk = data.chunk();
                let (offset, stride) = (chunk.offset() as usize, chunk.stride().max(0) as usize);
                let stride = if stride == 0 { width * 4 } else { stride };
                let Some(bytes) = data.data() else { return };
                let needed = offset + stride * height;
                if bytes.len() < needed || width == 0 || height == 0 {
                    return;
                }
                let first = st.first;
                if first {
                    st.first = false;
                    info!("screen {}: first frame", st.id);
                }
                let display = st.id as u32;
                match damage {
                    // How the display looks, not a change.
                    _ if first => crate::damage::note(display, (width, height), &[]),
                    Some(rects) => crate::damage::note(display, (width, height), &rects),
                    // Compared only while an agent is looking: it reads the whole frame.
                    None if crate::damage::wanted(display) => {
                        if let Some(rects) = st.differ.diff(width, height, stride, &bytes[offset..needed]) {
                            crate::damage::note(display, (width, height), &rects);
                        }
                    }
                    None => {
                        st.differ = Default::default();
                        crate::damage::note_all(display, (width, height));
                    }
                }
                let captured_us = now_us();
                let mut pixels = st.tx.0.buffer();
                pixels.clear();
                pixels.extend_from_slice(&bytes[offset..needed]);
                let frame = Frame {
                    width,
                    height,
                    stride,
                    format: pixel,
                    data: pixels,
                };
                // Replaces a frame the encoder hasn't started on: it always gets the newest.
                st.tx.0.put(frame, captured_us);
            })
            .register()?;

        let values = format_params();
        let mut params = [Pod::from_bytes(&values).context("format pod")?];
        stream.connect(
            spa::utils::Direction::Input,
            Some(node),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )?;
        // The listener first: they drop in order, and destroying the stream frees the list
        // its hook is on, which removing the hook afterwards would write to.
        keep.push((listener, stream, values));
    }
    mainloop.run();
    Ok(())
}

/// Whether a stream's change of state means it won't deliver again.
fn share_ended(old: &pw::stream::StreamState, new: &pw::stream::StreamState) -> bool {
    use pw::stream::StreamState::{Error, Unconnected};
    matches!(new, Error(_)) || (matches!(new, Unconnected) && !matches!(old, Unconnected))
}

/// Damage regions with each buffer (SPA_META_VideoDamage), up to 16 a frame.
fn damage_params() -> Vec<u8> {
    use spa::pod::{ChoiceValue, Object, Property, Value};
    use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Id, SpaTypes};
    // SPA_PARAM_META_type and SPA_PARAM_META_size (spa/param/buffers.h).
    const META_TYPE: u32 = 1;
    const META_SIZE: u32 = 2;
    let region = std::mem::size_of::<spa::sys::spa_meta_region>() as i32;
    let obj = Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: spa::param::ParamType::Meta.as_raw(),
        properties: vec![
            Property::new(META_TYPE, Value::Id(Id(spa::sys::SPA_META_VideoDamage))),
            Property::new(
                META_SIZE,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range { default: region * 16, min: region, max: region * 16 },
                ))),
            ),
        ],
    };
    spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
        .expect("serialize meta pod")
        .0
        .into_inner()
}

/// Accept 32-bit RGB formats in shared memory (no DMA-BUF modifiers offered, so the
/// compositor sends mappable buffers).
fn format_params() -> Vec<u8> {
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::param::video::VideoFormat;
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            spa::utils::Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 240, denom: 1 }
        ),
    );
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .expect("serialize format pod")
    .0
    .into_inner()
}

impl VideoSource for Video {
    fn streams(&self) -> Vec<DisplayInfo> {
        Video::streams(self)
    }
    fn request_keyframe(&self, stream: Option<u8>) {
        Video::request_keyframe(self, stream)
    }
    fn set_layout(&self, layout: Layout) -> Result<Vec<DisplayInfo>> {
        Video::set_layout(self, layout)
    }
    fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Video::displays(self)
    }
    fn supports_lossless(&self) -> bool {
        true
    }
    fn set_full_colour(&self, on: bool) {
        FULL_COLOUR.store(on, Ordering::Relaxed);
        if !on && let Some(monitors) = self.monitors.lock().unwrap().as_ref() {
            for m in monitors {
                *m.latest.lock().unwrap() = None;
            }
        }
    }
    /// Every shared monitor is captured all the time; frames say (or show) what changed.
    fn track_damage(&self, id: u32) -> bool {
        self.monitors.lock().unwrap().as_ref().is_some_and(|m| (id as usize) < m.len())
    }
    fn picture(&self, id: u32, max: [u32; 2]) -> Option<crate::lossless::RawFrame> {
        *THUMBS_UNTIL.lock().unwrap() = Some(std::time::Instant::now() + Duration::from_secs(10));
        let frame = {
            let monitors = self.monitors.lock().unwrap();
            let m = monitors.as_ref()?.get(id as usize)?;
            let thumb = m.thumb.lock().unwrap();
            thumb.as_ref()?.1.clone()
        };
        crate::thumbs::downscale(&frame.bgra, frame.width, frame.height, frame.width * 4, false, max)
    }
    fn frame_generation(&self, stream: u8) -> Option<u64> {
        let monitors = self.monitors.lock().unwrap();
        let m = monitors.as_ref()?.iter().find(|m| m.slot.load(Ordering::Relaxed) == stream as i32)?;
        Some(m.generation.load(Ordering::Relaxed))
    }
    fn raw_frame(&self, stream: u8) -> Option<Arc<crate::lossless::RawFrame>> {
        let monitors = self.monitors.lock().unwrap();
        let m = monitors.as_ref()?.iter().find(|m| m.slot.load(Ordering::Relaxed) == stream as i32)?;
        m.latest.lock().unwrap().clone()
    }
    fn set_bitrate(&self, bps: u32) {
        if let Some(monitors) = self.monitors.lock().unwrap().as_ref() {
            for m in monitors {
                m.bitrate.store(bps, Ordering::Relaxed);
            }
        }
    }
    fn heal(&self) {
        self.rejoin();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pw::stream::StreamState::{Connecting, Error, Paused, Streaming, Unconnected};

    #[test]
    fn a_share_ends_when_its_stream_can_no_longer_deliver() {
        assert!(share_ended(&Streaming, &Unconnected));
        assert!(share_ended(&Paused, &Error("node gone".into())));
        assert!(!share_ended(&Unconnected, &Unconnected), "not started yet");
        assert!(!share_ended(&Unconnected, &Connecting));
        assert!(!share_ended(&Streaming, &Paused), "nothing to send isn't ended");
    }

    #[test]
    fn a_share_turned_down_isnt_asked_for_again_unprompted() {
        let mut rejoin = Rejoin::new();
        assert!(!rejoin.due(), "a moment first");
        rejoin.at = std::time::Instant::now();
        assert!(rejoin.due());
        rejoin.failed(false);
        assert!(!rejoin.due(), "a wait after a failure");
        rejoin.at = std::time::Instant::now();
        assert!(rejoin.due(), "tried again");
        rejoin.failed(true);
        rejoin.at = std::time::Instant::now();
        assert!(!rejoin.due());
    }
}
