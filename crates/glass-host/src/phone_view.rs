//! Phones in the viewer: each phone set up for agents (`glass config phones`, see phone.rs)
//! is also a display. Pick it in the viewer and its live picture streams like any other
//! display, and the viewer's pointer, touches and keyboard work it.
//!
//! The picture is WebDriverAgent's MJPEG stream (about 30 frames a second), decoded and
//! encoded to H.264 in software (crate::soft_encode). The computer's own capture keeps
//! running underneath; its frames are held back while a phone is shown.
//!
//! Input: WebDriverAgent plays a gesture as one request, so a press is held here until it's
//! released and then played back, with its own timing, as a tap, a long press or a swipe.
//! Scrolling is a swipe; keys type; Escape is the Home button.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use glass_proto::InputEvent;
use tokio::sync::broadcast;

use crate::lossless::RawFrame;
use crate::media::{DisplayInfo, EncodedFrame, InputSink, Layout, PhoneTag, VideoSource, WindowInfo};
use crate::phone::Phone;

/// Phones' display ids: far from any real display's (small numbers on every platform), and
/// how the viewer knows a display is a phone.
const ID_BASE: u32 = glass_proto::PHONE_DISPLAY_BASE;

pub fn is_phone_display(id: u32) -> bool {
    glass_proto::is_phone_display(id)
}

/// How often a phone that isn't shown decodes a picture, for the display previews.
const PREVIEW_EVERY: Duration = Duration::from_millis(1000);
/// A phone that's shown but still sends one picture this often (the viewer's watchdog, a
/// refresh), even when nothing changed.
const RESEND_EVERY: Duration = Duration::from_millis(1000);

/// One phone's picture.
struct Feed {
    phone: Phone,
    id: u32,
    /// Pixels, and points (what touches are in), once its picture arrives.
    size: Mutex<Option<((usize, usize), (f64, f64))>>,
    /// Pictures are arriving.
    live: AtomicBool,
    /// Its stream number while shown, else -1.
    slot: AtomicI32,
    keyframe: AtomicBool,
    /// The width to encode at (0: as it comes).
    width: AtomicUsize,
    /// A recent picture, for previews.
    latest: Mutex<Option<(Instant, Arc<RawFrame>)>>,
    /// Gone from the settings: its thread ends.
    stop: AtomicBool,
}

impl Feed {
    fn info(&self, x: f64) -> Option<DisplayInfo> {
        let ((w, h), (pw, ph)) = (*self.size.lock().unwrap())?;
        Some(DisplayInfo {
            id: self.id,
            width: w,
            height: h,
            x,
            y: 0.0,
            points_width: pw,
            points_height: ph,
            main: false,
            builtin: false,
            phone: Some(PhoneTag { name: self.phone.name.clone(), wifi: self.phone.wifi }),
        })
    }
}

/// The phones, and which one is shown; shared by the video and input sides.
pub struct Phones {
    feeds: Mutex<Vec<Arc<Feed>>>,
    shown: Mutex<Option<Arc<Feed>>>,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    bitrate: AtomicU32,
    fps: AtomicU32,
    /// When the viewer's last input left for the phone, until its picture changes (for
    /// the input-to-picture time in the debug log).
    input_at: Mutex<Option<Instant>>,
}

impl Phones {
    fn shown(&self) -> Option<Arc<Feed>> {
        self.shown.lock().unwrap().clone()
    }

    /// Starts a feed for each phone newly in the settings, and ends those taken out.
    fn sync(self: &Arc<Self>) {
        let wanted = crate::phone::configured();
        let mut feeds = self.feeds.lock().unwrap();
        feeds.retain(|f| {
            let keep = wanted.contains(&f.phone);
            if !keep {
                f.stop.store(true, Ordering::Relaxed);
            }
            keep
        });
        for phone in wanted {
            if feeds.iter().any(|f| f.phone == phone) {
                continue;
            }
            // Ids follow the name, so a phone keeps its id across restarts.
            let id = ID_BASE | (phone.name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) & 0x00FF_FFFF);
            let feed = Arc::new(Feed {
                phone,
                id,
                size: Mutex::new(None),
                live: AtomicBool::new(false),
                slot: AtomicI32::new(-1),
                keyframe: AtomicBool::new(true),
                width: AtomicUsize::new(0),
                latest: Mutex::new(None),
                stop: AtomicBool::new(false),
            });
            feeds.push(feed.clone());
            let phones = self.clone();
            let _ = std::thread::Builder::new()
                .name(format!("phone {}", feed.phone.name))
                .spawn(move || feed_loop(feed, phones));
        }
    }
}

/// Wraps the computer's video source (which sends to `inner_frames`) so phones show up as
/// displays too, everything reaching viewers through `frames`.
pub fn wrap(
    inner: Arc<dyn VideoSource>,
    inner_frames: broadcast::Sender<Arc<EncodedFrame>>,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    bitrate: u32,
    fps: u32,
) -> (Arc<PhoneVideo>, Arc<Phones>) {
    let phones = Arc::new(Phones {
        feeds: Mutex::new(Vec::new()),
        shown: Mutex::new(None),
        frames: frames.clone(),
        bitrate: AtomicU32::new(bitrate),
        fps: AtomicU32::new(fps.clamp(1, 30)),
        input_at: Mutex::new(None),
    });
    let inner_on = Arc::new(AtomicBool::new(true));
    {
        let (mut rx, on) = (inner_frames.subscribe(), inner_on.clone());
        let _ = std::thread::Builder::new().name("computer frames".into()).spawn(move || {
            loop {
                match rx.blocking_recv() {
                    Ok(f) if on.load(Ordering::Relaxed) => {
                        let _ = frames.send(f);
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
    phones.sync();
    (Arc::new(PhoneVideo { inner, phones: phones.clone(), inner_on }), phones)
}

pub struct PhoneVideo {
    inner: Arc<dyn VideoSource>,
    phones: Arc<Phones>,
    /// The computer's frames reach viewers (false while a phone is shown).
    inner_on: Arc<AtomicBool>,
}

impl PhoneVideo {
    /// The live phones as displays, placed to the right of the computer's.
    fn phone_displays(&self, beside: &[DisplayInfo]) -> Vec<DisplayInfo> {
        let mut x = beside.iter().map(|d| d.x + d.points_width).fold(0.0, f64::max) + 40.0;
        let mut out = Vec::new();
        for f in self.phones.feeds.lock().unwrap().iter() {
            if f.live.load(Ordering::Relaxed)
                && let Some(d) = f.info(x)
            {
                x += d.points_width + 40.0;
                out.push(d);
            }
        }
        out
    }

    fn shown_info(&self) -> Option<DisplayInfo> {
        let feed = self.phones.shown()?;
        let beside = self.inner.displays().unwrap_or_default();
        self.phone_displays(&beside).into_iter().find(|d| d.id == feed.id)
    }

    fn feed(&self, id: u32) -> Option<Arc<Feed>> {
        self.phones.feeds.lock().unwrap().iter().find(|f| f.id == id).cloned()
    }

    fn phone_shown(&self) -> bool {
        self.phones.shown().is_some()
    }
}

impl VideoSource for PhoneVideo {
    fn streams(&self) -> Vec<DisplayInfo> {
        match self.shown_info() {
            Some(d) => vec![d],
            None => self.inner.streams(),
        }
    }

    fn request_keyframe(&self, stream: Option<u8>) {
        match self.phones.shown() {
            Some(f) => f.keyframe.store(true, Ordering::Relaxed),
            None => self.inner.request_keyframe(stream),
        }
    }

    fn set_layout(&self, layout: Layout) -> Result<Vec<DisplayInfo>> {
        if let Layout::Single(Some(id)) = layout
            && is_phone_display(id)
        {
            let feed = self.feed(id).context("that phone is gone")?;
            anyhow::ensure!(feed.live.load(Ordering::Relaxed), "{} isn't sending its picture yet", feed.phone.name);
            let mut shown = self.phones.shown.lock().unwrap();
            if let Some(old) = shown.take() {
                old.slot.store(-1, Ordering::Relaxed);
            }
            self.inner_on.store(false, Ordering::Relaxed);
            feed.width.store(0, Ordering::Relaxed);
            feed.keyframe.store(true, Ordering::Relaxed);
            feed.slot.store(0, Ordering::Relaxed);
            *shown = Some(feed);
            drop(shown);
            return self.shown_info().map(|d| vec![d]).context("that phone stopped sending its picture");
        }
        let streams = self.inner.set_layout(layout)?;
        if let Some(old) = self.phones.shown.lock().unwrap().take() {
            old.slot.store(-1, Ordering::Relaxed);
        }
        self.inner_on.store(true, Ordering::Relaxed);
        self.inner.request_keyframe(None);
        Ok(streams)
    }

    fn displays(&self) -> Result<Vec<DisplayInfo>> {
        let mut all = self.inner.displays()?;
        let phones = self.phone_displays(&all);
        all.extend(phones);
        Ok(all)
    }

    fn set_bitrate(&self, bps: u32) {
        self.phones.bitrate.store(bps, Ordering::Relaxed);
        self.inner.set_bitrate(bps);
    }

    fn rebalance(&self) {
        self.inner.rebalance();
    }

    fn heal(&self) {
        self.inner.heal();
        self.phones.sync();
    }

    fn set_target_widths(&self, widths: &[Option<u32>]) {
        match self.phones.shown() {
            Some(f) => f.width.store(widths.first().copied().flatten().unwrap_or(0) as usize, Ordering::Relaxed),
            None => self.inner.set_target_widths(widths),
        }
    }

    fn set_max_fps(&self, fps: u32) {
        self.phones.fps.store(fps.clamp(1, 30), Ordering::Relaxed);
        self.inner.set_max_fps(fps);
    }

    fn icc(&self, stream: u8) -> Option<Arc<Vec<u8>>> {
        if self.phone_shown() { None } else { self.inner.icc(stream) }
    }

    fn supports_windows(&self) -> bool {
        self.inner.supports_windows()
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        self.inner.windows()
    }

    fn set_hevc(&self, hevc: bool) -> bool {
        // A phone's picture is always H.264 (each frame says which it is).
        self.inner.set_hevc(hevc)
    }

    fn raw_frame(&self, stream: u8) -> Option<Arc<RawFrame>> {
        if self.phone_shown() { None } else { self.inner.raw_frame(stream) }
    }

    fn frame_generation(&self, stream: u8) -> Option<u64> {
        if self.phone_shown() { None } else { self.inner.frame_generation(stream) }
    }

    fn picture(&self, id: u32, max: [u32; 2]) -> Option<RawFrame> {
        if !is_phone_display(id) {
            return self.inner.picture(id, max);
        }
        let feed = self.feed(id)?;
        let latest = feed.latest.lock().unwrap().as_ref().map(|(_, f)| f.clone())?;
        let (w, h) = crate::thumbs::fit(latest.width, latest.height, max);
        Some(RawFrame { width: w, height: h, bgra: scale_bgra(&latest.bgra, latest.width, latest.height, w, h), capture_us: latest.capture_us })
    }

    fn track_damage(&self, id: u32) -> bool {
        !is_phone_display(id) && self.inner.track_damage(id)
    }

    fn set_full_colour(&self, on: bool) {
        self.inner.set_full_colour(on);
    }

    fn set_cursor_baked(&self, on: bool) {
        self.inner.set_cursor_baked(on);
    }

    fn supports_lossless(&self) -> bool {
        self.inner.supports_lossless()
    }

    fn set_rext(&self, on: bool) -> bool {
        self.inner.set_rext(on)
    }

    fn supports_rext(&self) -> bool {
        self.inner.supports_rext()
    }

    fn supports_hevc(&self) -> bool {
        self.inner.supports_hevc()
    }
}

// ---- The picture ----

fn feed_loop(feed: Arc<Feed>, phones: Arc<Phones>) {
    let mut said = false;
    while !feed.stop.load(Ordering::Relaxed) {
        match stream(&feed, &phones) {
            Ok(()) => {}
            Err(e) if !said => {
                tracing::info!("phone {}: no picture yet ({e:#}); trying again every few seconds", feed.phone.name);
                said = true;
            }
            Err(_) => {}
        }
        feed.live.store(false, Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// The input-to-picture time waiting to be logged once the next picture is sent.
type Probe = Arc<Mutex<Option<Instant>>>;

/// Sends an encoded picture to viewers while the phone is shown.
fn deliver(feed: &Feed, frames: &broadcast::Sender<Arc<EncodedFrame>>, probe: &Probe, frame: EncodedFrame) {
    let slot = feed.slot.load(Ordering::Relaxed);
    if slot < 0 {
        return;
    }
    if let Some(at) = probe.lock().unwrap().take() {
        tracing::debug!("phone {}: input to picture sent {} ms", feed.phone.name, at.elapsed().as_millis());
    }
    let _ = frames.send(Arc::new(EncodedFrame { stream: slot as u8, hevc: false, ..frame }));
}

/// An encoder for one size and frame rate: the Mac's hardware encoder where there is one
/// (a picture in about 1 ms, against about 10 in software), else OpenH264.
enum Enc {
    #[cfg(target_os = "macos")]
    Hardware {
        encoder: crate::platform::encode::Encoder,
        keyframe: Arc<AtomicBool>,
        /// Pixel buffers to fill, reused once the encoder lets go of them: a new 6 MB one each
        /// picture cost more than the encoding.
        ring: Vec<objc2_core_foundation::CFRetained<objc2_core_video::CVImageBuffer>>,
    },
    Software { encoder: crate::soft_encode::Encoder },
}

struct Encoding {
    enc: Enc,
    size: (usize, usize),
    width: usize,
    fps: u32,
    bitrate: u32,
}

impl Encoding {
    fn new(feed: &Arc<Feed>, frames: &broadcast::Sender<Arc<EncodedFrame>>, probe: &Probe, (w, h): (usize, usize), width: usize, fps: u32, bitrate: u32) -> Result<Self> {
        // The hardware encoder takes the picture as it comes, whatever width the viewer
        // shows it at: scaling would cost more than it saves.
        #[cfg(target_os = "macos")]
        {
            let keyframe = Arc::new(AtomicBool::new(true));
            let (to, frames, probe) = (feed.clone(), frames.clone(), probe.clone());
            let on_frame: Arc<dyn Fn(EncodedFrame) + Send + Sync> = Arc::new(move |f| deliver(&to, &frames, &probe, f));
            match crate::platform::encode::Encoder::new(w as i32, h as i32, fps as i32, bitrate as i32, false, false, false, keyframe.clone(), on_frame) {
                Ok(encoder) => {
                    encoder.apply_policy(bitrate, fps);
                    return Ok(Encoding { enc: Enc::Hardware { encoder, keyframe, ring: Vec::new() }, size: (w, h), width, fps, bitrate });
                }
                Err(e) => tracing::debug!("phone {}: no hardware encoder ({e:#}); encoding in software", feed.phone.name),
            }
        }
        let encoder = crate::soft_encode::Encoder::new(w, h, fps, bitrate, width as f64 / w as f64)?;
        Ok(Encoding { enc: Enc::Software { encoder }, size: (w, h), width, fps, bitrate })
    }

    /// Encodes at the picture's own size whatever width is asked for (the hardware one).
    fn native(&self) -> bool {
        match self.enc {
            #[cfg(target_os = "macos")]
            Enc::Hardware { .. } => true,
            Enc::Software { .. } => false,
        }
    }

    fn set_bitrate(&mut self, bitrate: u32) {
        if self.bitrate == bitrate {
            return;
        }
        self.bitrate = bitrate;
        match &mut self.enc {
            #[cfg(target_os = "macos")]
            Enc::Hardware { encoder, .. } => {
                encoder.apply_policy(bitrate, self.fps);
            }
            Enc::Software { encoder } => encoder.set_bitrate(bitrate),
        }
    }

    /// Encodes a BGRA picture; it reaches viewers through `deliver`. True when one went.
    fn encode(&mut self, feed: &Feed, frames: &broadcast::Sender<Arc<EncodedFrame>>, probe: &Probe, bgra: Vec<u8>, force: bool, capture_us: u64) -> Result<bool> {
        let (w, h) = self.size;
        match &mut self.enc {
            #[cfg(target_os = "macos")]
            Enc::Hardware { encoder, keyframe, ring } => {
                if force {
                    keyframe.store(true, Ordering::Relaxed);
                }
                // A buffer only we hold (the encoder is done with it), else a new one.
                let image = match ring.iter().find(|b| b.retain_count() == 1) {
                    Some(b) => b.clone(),
                    None => {
                        let b = bgra_buffer(w, h).context("couldn't make a picture for the encoder")?;
                        if ring.len() < 6 {
                            ring.push(b.clone());
                        }
                        b
                    }
                };
                anyhow::ensure!(fill_bgra(&image, &bgra, w, h), "couldn't fill a picture for the encoder");
                encoder.encode(&image, unsafe { objc2_core_media::CMTime::new(capture_us as i64, 1_000_000) }, capture_us, false);
                Ok(true)
            }
            Enc::Software { encoder } => {
                let frame = crate::soft_encode::Frame { width: w, height: h, stride: w * 4, format: crate::soft_encode::PixelFormat::Bgrx, data: bgra };
                Ok(match encoder.encode(&frame, force)? {
                    Some((data, keyframe)) => {
                        deliver(feed, frames, probe, EncodedFrame { stream: 0, data, keyframe, capture_us, hevc: false, refresh: false });
                        true
                    }
                    None => false,
                })
            }
        }
    }
}

/// An empty BGRA pixel buffer for VideoToolbox.
#[cfg(target_os = "macos")]
fn bgra_buffer(w: usize, h: usize) -> Option<objc2_core_foundation::CFRetained<objc2_core_video::CVImageBuffer>> {
    use objc2_core_video::*;
    let mut raw: *mut CVPixelBuffer = std::ptr::null_mut();
    // SAFETY: a fresh buffer of this size and format; `raw` receives it retained.
    let status = unsafe { CVPixelBufferCreate(None, w, h, kCVPixelFormatType_32BGRA, None, std::ptr::NonNull::from(&mut raw)) };
    let image = (status == 0).then(|| std::ptr::NonNull::new(raw)).flatten()?;
    // SAFETY: we own the +1 reference CVPixelBufferCreate returned.
    Some(unsafe { objc2_core_foundation::CFRetained::from_raw(image) })
}

/// Copies a BGRA picture into a pixel buffer of its size.
#[cfg(target_os = "macos")]
fn fill_bgra(image: &objc2_core_video::CVImageBuffer, bgra: &[u8], w: usize, h: usize) -> bool {
    use objc2_core_video::*;
    // SAFETY: locked while its rows are written, each within the buffer's bytes per row and
    // its height (checked against the picture's).
    unsafe {
        if CVPixelBufferLockBaseAddress(image, CVPixelBufferLockFlags(0)) != 0 {
            return false;
        }
        let (base, stride) = (CVPixelBufferGetBaseAddress(image) as *mut u8, CVPixelBufferGetBytesPerRow(image));
        let ok = !base.is_null() && stride >= w * 4 && CVPixelBufferGetHeight(image) >= h && bgra.len() >= w * h * 4;
        if ok {
            for row in 0..h {
                std::ptr::copy_nonoverlapping(bgra.as_ptr().add(row * w * 4), base.add(row * stride), w * 4);
            }
        }
        CVPixelBufferUnlockBaseAddress(image, CVPixelBufferLockFlags(0));
        ok
    }
}

/// Reads the phone's MJPEG stream until it ends.
fn stream(feed: &Arc<Feed>, phones: &Phones) -> Result<()> {
    let p = &feed.phone;
    let points = crate::phone::window(p).map_err(|e| anyhow::anyhow!(e.0))?;
    if let Err(e) = crate::phone::start_video(p) {
        tracing::debug!("phone {}: couldn't raise its frame rate: {}", p.name, e.0);
    }
    let mut socket = TcpStream::connect_timeout(&([127, 0, 0, 1], p.video).into(), Duration::from_secs(2))
        .with_context(|| format!("nothing on port {} (forward WebDriverAgent's video there)", p.video))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")?;
    let mut r = BufReader::new(socket);
    skip_headers(&mut r)?;
    tracing::info!("phone {}: streaming its picture", p.name);
    let mut enc: Option<Encoding> = None;
    let probe: Probe = Arc::new(Mutex::new(None));
    let mut last_jpeg = 0u64;
    let mut last_sent: Option<Instant> = None;
    let mut stats = Stats::new();
    loop {
        if feed.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        stats.report(&p.name);
        stats.frames += 1;
        let jpeg = next_part(&mut r)?;
        let capture_us = crate::clock::now_us();
        let slot = feed.slot.load(Ordering::Relaxed);
        let preview_due = feed.latest.lock().unwrap().as_ref().is_none_or(|(at, _)| at.elapsed() >= PREVIEW_EVERY);
        if slot < 0 && !preview_due {
            continue;
        }
        let same = std::mem::replace(&mut last_jpeg, hash(&jpeg)) == last_jpeg;
        // The viewer's last input, when this is the first new picture since: timed until
        // it's sent (debug log).
        if !same && slot >= 0 && let Some(at) = phones.input_at.lock().unwrap().take() {
            tracing::debug!("phone {}: input to new picture arrived {} ms", p.name, at.elapsed().as_millis());
            *probe.lock().unwrap() = Some(at);
        }
        let keyframe = feed.keyframe.load(Ordering::Relaxed);
        let resend = last_sent.is_none_or(|t| t.elapsed() >= RESEND_EVERY);
        if slot >= 0 && same && !keyframe && !resend && !preview_due {
            continue;
        }
        let started = Instant::now();
        let (bgra, w, h) = decode(&jpeg)?;
        stats.decoded += 1;
        stats.decode += started.elapsed();
        *feed.size.lock().unwrap() = Some(((w, h), points));
        feed.live.store(true, Ordering::Relaxed);
        if preview_due {
            let raw = Arc::new(RawFrame { width: w, height: h, bgra: bgra.clone(), capture_us });
            *feed.latest.lock().unwrap() = Some((Instant::now(), raw));
        }
        if slot < 0 || (same && !keyframe && !resend) {
            continue;
        }
        // A new encoder for a new size, width or frame rate, starting on a keyframe.
        let want_w = match feed.width.load(Ordering::Relaxed) {
            0 => w,
            n => n.min(w),
        };
        let fps = phones.fps.load(Ordering::Relaxed);
        let bitrate = phones.bitrate.load(Ordering::Relaxed);
        if enc.as_ref().is_none_or(|e| e.size != (w, h) || (e.width != want_w && !e.native()) || e.fps != fps) {
            enc = Some(Encoding::new(feed, &phones.frames, &probe, (w, h), want_w, fps, bitrate)?);
            feed.keyframe.store(true, Ordering::Relaxed);
        }
        let e = enc.as_mut().unwrap();
        e.set_bitrate(bitrate);
        let force = feed.keyframe.swap(false, Ordering::Relaxed);
        let started = Instant::now();
        let sent = e.encode(feed, &phones.frames, &probe, bgra, force, capture_us)?;
        stats.encode += started.elapsed();
        if sent {
            stats.sent += 1;
            last_sent = Some(Instant::now());
        }
    }
}

/// What the picture costs, said every few seconds while it's working (debug log).
struct Stats {
    since: Instant,
    frames: u32,
    decoded: u32,
    sent: u32,
    decode: Duration,
    encode: Duration,
}

impl Stats {
    fn new() -> Self {
        Stats { since: Instant::now(), frames: 0, decoded: 0, sent: 0, decode: Duration::ZERO, encode: Duration::ZERO }
    }

    fn report(&mut self, name: &str) {
        let secs = self.since.elapsed().as_secs_f64();
        if secs < 5.0 {
            return;
        }
        if self.sent > 0 {
            let per = |d: Duration, n: u32| d.as_secs_f64() * 1000.0 / n.max(1) as f64;
            tracing::debug!(
                "phone {name}: {:.1} pictures/s in, {:.1} sent; decode {:.1} ms, encode {:.1} ms each",
                self.frames as f64 / secs,
                self.sent as f64 / secs,
                per(self.decode, self.decoded),
                per(self.encode, self.sent)
            );
        }
        *self = Stats::new();
    }
}

/// Reads past the HTTP response's headers.
fn skip_headers(r: &mut impl BufRead) -> Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        anyhow::ensure!(r.read_line(&mut line)? > 0, "the picture stream ended");
        if line.trim().is_empty() {
            return Ok(());
        }
    }
}

/// The next JPEG of a multipart (MJPEG) stream: past its boundary and headers.
fn next_part(r: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut line = String::new();
    let mut len = None;
    let mut in_headers = false;
    loop {
        line.clear();
        anyhow::ensure!(r.read_line(&mut line)? > 0, "the picture stream ended");
        let l = line.trim();
        if l.starts_with("--") {
            in_headers = true;
            len = None;
            continue;
        }
        if !in_headers {
            continue;
        }
        if l.is_empty() {
            if let Some(n) = len {
                anyhow::ensure!(n > 0 && n < 32 << 20, "a picture of {n} bytes");
                let mut jpeg = vec![0; n];
                r.read_exact(&mut jpeg)?;
                return Ok(jpeg);
            }
            continue;
        }
        if let Some((k, v)) = l.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
}

fn decode(jpeg: &[u8]) -> Result<(Vec<u8>, usize, usize)> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    let bgra = d.decode().map_err(|e| anyhow::anyhow!("bad picture: {e:?}"))?;
    let (w, h) = d.dimensions().context("a picture without a size")?;
    anyhow::ensure!(bgra.len() == w * h * 4, "a picture of the wrong size");
    Ok((bgra, w, h))
}

fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// A BGRA picture shrunk to `dw` × `dh` (box filter), for previews.
fn scale_bgra(src: &[u8], w: usize, h: usize, dw: usize, dh: usize) -> Vec<u8> {
    if (dw, dh) == (w, h) {
        return src.to_vec();
    }
    let mut out = Vec::with_capacity(dw * dh * 4);
    for oy in 0..dh {
        let (y0, y1) = (oy * h / dh, ((oy + 1) * h / dh).max(oy * h / dh + 1).min(h));
        for ox in 0..dw {
            let (x0, x1) = (ox * w / dw, ((ox + 1) * w / dw).max(ox * w / dw + 1).min(w));
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    for c in 0..4 {
                        sum[c] += src[i + c] as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1) as u32;
            out.extend(sum.iter().map(|s| (s / n) as u8));
        }
    }
    out
}

// ---- Input ----

/// What the phone does, in order, off the viewer's input path (each is a request to it).
enum Cmd {
    Touch { phone: Phone, path: Vec<(f64, f64, u64)>, hold: u64 },
    Keys { phone: Phone, text: String },
    Home { phone: Phone },
    /// One of the phone's buttons or system gestures (glass_proto::phone_keys).
    System { phone: Phone, key: u16, size: (f64, f64) },
    /// Scrolling at a point (points), by `dx`, `dy` (points, content moving up/left).
    Scroll { phone: Phone, at: (f64, f64), size: (f64, f64), dx: f64, dy: f64 },
}

/// A press being held: where it went (0..1 of the picture) and when (ms since it began).
struct Press {
    button: u8,
    start: Instant,
    path: Vec<(f32, f32, u64)>,
}

/// Wraps the computer's input so a shown phone gets the viewer's pointer and keyboard.
pub struct PhoneInput {
    inner: Box<dyn InputSink>,
    phones: Arc<Phones>,
    at: (f32, f32),
    press: Option<Press>,
    text: glass_proto::TextReader,
    shift: bool,
    command: bool,
    tx: mpsc::Sender<Cmd>,
}

pub fn wrap_input(inner: Box<dyn InputSink>, phones: Arc<Phones>) -> Box<dyn InputSink> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new().name("phone input".into()).spawn(move || input_worker(rx));
    Box::new(PhoneInput { inner, phones, at: (0.5, 0.5), press: None, text: Default::default(), shift: false, command: false, tx })
}

impl PhoneInput {
    fn send(&self, cmd: Cmd) {
        *self.phones.input_at.lock().unwrap() = Some(Instant::now());
        let _ = self.tx.send(cmd);
    }

    fn key(&mut self, phone: &Phone, keycode: u16, size: (f64, f64)) {
        use crate::keys::{COMMAND, CONTROL, OPTION, SHIFT};
        let cmd = |text: &str| Cmd::Keys { phone: phone.clone(), text: text.into() };
        let send = match keycode {
            k if glass_proto::phone_keys::is_phone_key(k) => Cmd::System { phone: phone.clone(), key: k, size },
            SHIFT | 0x3C => {
                self.shift = true;
                return;
            }
            COMMAND | 0x36 => {
                self.command = true;
                return;
            }
            OPTION | 0x3D | CONTROL | 0x3E | 0x39 | 0x3F => return,
            // Shortcuts are the computer's; a phone has none.
            _ if self.command => return,
            0x35 => Cmd::Home { phone: phone.clone() },
            0x24 | 0x4C => cmd("\n"),
            0x33 => cmd("\u{8}"),
            0x30 => cmd("\t"),
            k => match typed(k, self.shift) {
                Some(c) => cmd(&c.to_string()),
                None => return,
            },
        };
        self.send(send);
    }
}

/// The character a key types on a US keyboard, by macOS keycode.
fn typed(keycode: u16, shift: bool) -> Option<char> {
    static KEYS: OnceLock<HashMap<(u16, bool), char>> = OnceLock::new();
    KEYS.get_or_init(|| {
        (' '..='~')
            .filter_map(|c| crate::keys::char_key(c).map(|(k, s)| ((k, s), c)))
            .collect()
    })
    .get(&(keycode, shift))
    .copied()
}

impl InputSink for PhoneInput {
    fn handle(&mut self, ev: InputEvent) {
        let Some(feed) = self.phones.shown() else {
            if let InputEvent::KeyDown { keycode } | InputEvent::KeyUp { keycode } = ev
                && glass_proto::phone_keys::is_phone_key(keycode)
            {
                return;
            }
            return self.inner.handle(ev);
        };
        let Some((_, points)) = *feed.size.lock().unwrap() else {
            tracing::debug!("phone {}: input before its picture, dropped", feed.phone.name);
            return;
        };
        if !matches!(ev, InputEvent::MouseMove { .. }) {
            tracing::debug!("phone {}: {ev:?}", feed.phone.name);
        }
        let phone = feed.phone.clone();
        match ev {
            InputEvent::MouseMove { x, y, .. } => {
                self.at = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
                if let Some(p) = &mut self.press {
                    p.path.push((self.at.0, self.at.1, p.start.elapsed().as_millis() as u64));
                }
            }
            InputEvent::MouseDown { button } => {
                if self.press.is_none() {
                    self.press = Some(Press { button, start: Instant::now(), path: vec![(self.at.0, self.at.1, 0)] });
                }
            }
            InputEvent::MouseUp { .. } => {
                if let Some(p) = self.press.take() {
                    let (path, hold) = plan_touch(p.button, &p.path, p.start.elapsed().as_millis() as u64, points);
                    tracing::debug!("phone {}: touch of {} points, held {hold} ms", phone.name, path.len());
                    self.send(Cmd::Touch { phone, path, hold });
                }
            }
            InputEvent::Scroll { dx, dy } => {
                let at = (self.at.0 as f64 * points.0, self.at.1 as f64 * points.1);
                self.send(Cmd::Scroll { phone, at, size: points, dx: dx as f64, dy: dy as f64 });
            }
            InputEvent::KeyDown { keycode } => self.key(&phone, keycode, points),
            InputEvent::KeyUp { keycode } => match keycode {
                crate::keys::SHIFT | 0x3C => self.shift = false,
                crate::keys::COMMAND | 0x36 => self.command = false,
                _ => {}
            },
            ev @ InputEvent::Text { .. } => {
                if let Some(text) = self.text.push(&ev) {
                    self.send(Cmd::Keys { phone, text });
                }
            }
            // A pen is a finger here; controllers and relative motion mean nothing to a phone.
            InputEvent::Pen { .. } | InputEvent::Gamepad { .. } | InputEvent::MouseMoveBy { .. } => {}
        }
    }

    fn paste(&mut self) {
        if self.phones.shown().is_none() {
            self.inner.paste();
        }
    }

    fn release_all(&mut self) {
        self.press = None;
        self.shift = false;
        self.command = false;
        self.inner.release_all();
    }

    fn set_displays(&mut self, displays: &[u32]) {
        let computer: Vec<u32> = displays.iter().copied().filter(|&d| !is_phone_display(d)).collect();
        if !computer.is_empty() {
            self.inner.set_displays(&computer);
        }
    }

    fn set_window_rect(&mut self, rect: Option<[f64; 4]>) {
        self.inner.set_window_rect(rect);
    }

    fn move_on_display(&mut self, display: u32, x: f32, y: f32) {
        if !is_phone_display(display) {
            self.inner.move_on_display(display, x, y);
        }
    }

    fn type_text(&mut self, text: &str) -> bool {
        self.inner.type_text(text)
    }
}

/// Distance (points) a press may wander and still be a tap.
const SLOP: f64 = 8.0;

/// A press as a touch to play back: its path (points, with each move's duration in ms) and
/// how long the finger stays down before moving. Still: a tap, or a long press when held
/// (or with the right button). Moved: one move from where it was pressed to where it was
/// released, over the time it took (its speed is what scrolls and flicks), after a hold
/// when it waited before moving (dragging an icon). One move, because WebDriverAgent spends
/// about 70 ms setting up each one before the finger even touches down.
fn plan_touch(button: u8, samples: &[(f32, f32, u64)], up_ms: u64, (pw, ph): (f64, f64)) -> (Vec<(f64, f64, u64)>, u64) {
    let pts: Vec<(f64, f64, u64)> = samples.iter().map(|&(x, y, t)| (x as f64 * pw, y as f64 * ph, t)).collect();
    let Some(&(x0, y0, _)) = pts.first() else { return (Vec::new(), 0) };
    let far = |&(x, y, _): &(f64, f64, u64)| ((x - x0).powi(2) + (y - y0).powi(2)).sqrt() > SLOP;
    let Some(first_move) = pts.iter().position(far) else {
        let hold = if button == 2 { 900 } else if up_ms >= 500 { up_ms.min(3000) } else { 60 };
        return (vec![(x0, y0, 0)], hold);
    };
    let started = pts[first_move.saturating_sub(1)].2;
    let hold = if started >= 300 { started.min(3000) } else { 0 };
    let &(x1, y1, t1) = pts.last().unwrap();
    let took = t1.max(up_ms.min(t1 + 100)).saturating_sub(started).clamp(10, 3000);
    (vec![(x0, y0, 0), (x1.clamp(0.0, pw - 1.0), y1.clamp(0.0, ph - 1.0), took)], hold)
}

/// One of the phone's buttons or system gestures, as an iPhone without a Home button does it.
fn system(phone: &Phone, key: u16, (w, h): (f64, f64)) -> Result<(), crate::computer::Refused> {
    use glass_proto::phone_keys::*;
    use crate::phone::{press_key, touch, touch_then_rest};
    match key {
        HOME => press_key(phone, "home"),
        VOLUME_UP => press_key(phone, "volume_up"),
        VOLUME_DOWN => press_key(phone, "volume_down"),
        // Slowly up from the bottom edge, then resting: quicker is a flick home (measured on
        // an iPhone XR: 300 and 600 ms went home, 900 ms opened the switcher every time).
        APP_SWITCHER => touch_then_rest(phone, &[(w / 2.0, h - 1.0, 0), (w / 2.0, h * 0.6, 900)], 0, 900),
        // Down from the top right corner, and from the top left.
        CONTROL_CENTER => touch(phone, &[(w - 25.0, 2.0, 0), (w - 25.0, h * 0.55, 250)], 0),
        NOTIFICATIONS => touch(phone, &[(w * 0.3, 2.0, 0), (w * 0.3, h * 0.6, 250)], 0),
        // Home, then down from the middle of the home screen.
        SPOTLIGHT => {
            press_key(phone, "home")?;
            std::thread::sleep(Duration::from_millis(500));
            touch(phone, &[(w / 2.0, h * 0.4, 0), (w / 2.0, h * 0.65, 250)], 0)
        }
        _ => Ok(()),
    }
}

/// A scroll as a swipe: the finger moves against the content, from `at` by the scroll's
/// length (at least a short flick), kept on the screen.
fn plan_scroll(at: (f64, f64), (pw, ph): (f64, f64), dx: f64, dy: f64) -> Vec<(f64, f64, u64)> {
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let reach = len.max(40.0) / len;
    let (ex, ey) = ((at.0 - dx * reach).clamp(1.0, pw - 1.0), (at.1 - dy * reach).clamp(1.0, ph - 1.0));
    vec![(at.0, at.1, 0), (ex, ey, 200)]
}

fn input_worker(rx: mpsc::Receiver<Cmd>) {
    let mut next: Option<Cmd> = None;
    loop {
        let Some(cmd) = next.take().or_else(|| rx.recv().ok()) else { return };
        let started = Instant::now();
        let what = match &cmd {
            Cmd::Touch { .. } => "touch",
            Cmd::Keys { .. } => "keys",
            Cmd::Home { .. } => "home",
            Cmd::System { .. } => "system",
            Cmd::Scroll { .. } => "scroll",
        };
        let phone = match &cmd {
            Cmd::Touch { phone, .. } | Cmd::Keys { phone, .. } | Cmd::Home { phone } | Cmd::System { phone, .. } | Cmd::Scroll { phone, .. } => phone.clone(),
        };
        let done = match cmd {
            Cmd::Touch { phone, path, hold } => crate::phone::touch(&phone, &path, hold),
            Cmd::Home { phone } => crate::phone::press_key(&phone, "home"),
            Cmd::System { phone, key, size } => system(&phone, key, size),
            Cmd::Keys { phone, mut text } => {
                // Typing outruns the phone: send what's queued together.
                while let Ok(more) = rx.try_recv() {
                    match more {
                        Cmd::Keys { phone: p, text: t } if p == phone => text.push_str(&t),
                        other => {
                            next = Some(other);
                            break;
                        }
                    }
                }
                crate::phone::type_keys(&phone, &text)
            }
            Cmd::Scroll { phone, at, size, mut dx, mut dy } => {
                // A wheel or trackpad sends many small scrolls: one swipe once they pause.
                loop {
                    match rx.recv_timeout(Duration::from_millis(120)) {
                        Ok(Cmd::Scroll { phone: p, dx: x, dy: y, .. }) if p == phone => {
                            dx += x;
                            dy += y;
                        }
                        Ok(other) => {
                            next = Some(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                crate::phone::touch(&phone, &plan_scroll(at, size, dx, dy), 0)
            }
        };
        tracing::debug!("phone input: {what} took {} ms", started.elapsed().as_millis());
        crate::phone::keep_quick(&phone);
        if let Err(e) = done {
            tracing::debug!("phone input: {}", e.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_ids_stay_clear_of_displays() {
        assert!(is_phone_display(ID_BASE | 42));
        assert!(!is_phone_display(1) && !is_phone_display(69_734_662) && !is_phone_display(0));
    }

    #[test]
    fn presses_become_taps_long_presses_and_swipes() {
        let size = (400.0, 800.0);
        // Still and quick: a tap where it was.
        let (path, hold) = plan_touch(0, &[(0.5, 0.5, 0), (0.501, 0.5, 40)], 90, size);
        assert_eq!((path, hold), (vec![(200.0, 400.0, 0)], 60));
        // Still and held, or the right button: a long press.
        assert_eq!(plan_touch(0, &[(0.5, 0.5, 0)], 800, size).1, 800);
        assert_eq!(plan_touch(2, &[(0.5, 0.5, 0)], 100, size).1, 900);
        // Moved: a swipe with its own timing, no hold.
        let samples: Vec<(f32, f32, u64)> = (0..=10).map(|i| (0.5, 0.8 - i as f32 * 0.05, i * 20)).collect();
        let (path, hold) = plan_touch(0, &samples, 210, size);
        assert_eq!(hold, 0);
        let first = path[0];
        assert!((first.0 - 200.0).abs() < 0.01 && (first.1 - 640.0).abs() < 0.01 && first.2 == 0, "starts where it was pressed: {path:?}");
        assert_eq!(path.len(), 2, "one move: {path:?}");
        let last = path[1];
        assert!((last.1 - 240.0).abs() < 0.01, "ends where it was released: {path:?}");
        assert!((190..=210).contains(&last.2), "over the time it took: {path:?}");
        // Held, then moved: dragging (an icon) after a hold.
        let (_, hold) = plan_touch(0, &[(0.5, 0.5, 0), (0.5, 0.5, 600), (0.7, 0.5, 700)], 800, size);
        assert_eq!(hold, 600);
    }

    #[test]
    fn scrolling_swipes_against_the_content_and_stays_on_screen() {
        // Content moving up (scrolling down): the finger goes up.
        let p = plan_scroll((200.0, 400.0), (400.0, 800.0), 0.0, 100.0);
        assert_eq!(p[1].0, 200.0);
        assert!(p[1].1 < 400.0);
        // A nudge still flicks a little; a huge one stops at the edge.
        let p = plan_scroll((200.0, 400.0), (400.0, 800.0), 0.0, 2.0);
        assert!((400.0 - p[1].1 - 40.0).abs() < 0.01);
        let p = plan_scroll((200.0, 400.0), (400.0, 800.0), 0.0, 5000.0);
        assert_eq!(p[1].1, 1.0);
    }

    /// A computer's input sink that records what reaches it.
    struct Recorder(Arc<Mutex<Vec<InputEvent>>>);

    impl InputSink for Recorder {
        fn handle(&mut self, ev: InputEvent) {
            self.0.lock().unwrap().push(ev);
        }
        fn paste(&mut self) {}
        fn release_all(&mut self) {}
        fn set_displays(&mut self, _: &[u32]) {}
        fn move_on_display(&mut self, _: u32, _: f32, _: f32) {}
    }

    #[test]
    fn phone_keys_never_reach_the_computer() {
        let (tx, _) = broadcast::channel(8);
        let phones = Arc::new(Phones { feeds: Mutex::new(Vec::new()), shown: Mutex::new(None), frames: tx, bitrate: AtomicU32::new(0), fps: AtomicU32::new(30), input_at: Mutex::new(None) });
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut input = wrap_input(Box::new(Recorder(seen.clone())), phones);
        // No phone shown: a phone's Home goes nowhere, an ordinary key goes to the computer.
        input.handle(InputEvent::KeyDown { keycode: glass_proto::phone_keys::HOME });
        input.handle(InputEvent::KeyUp { keycode: glass_proto::phone_keys::HOME });
        input.handle(InputEvent::KeyDown { keycode: 0x00 });
        assert_eq!(*seen.lock().unwrap(), vec![InputEvent::KeyDown { keycode: 0x00 }]);
        assert!(glass_proto::phone_keys::is_phone_key(glass_proto::phone_keys::VOLUME_DOWN));
        assert!(!glass_proto::phone_keys::is_phone_key(0x7F), "no macOS keycode is one");
        assert_eq!(ID_BASE, glass_proto::PHONE_DISPLAY_BASE);
    }

    #[test]
    fn keys_type_what_they_say() {
        assert_eq!(typed(crate::keys::char_key('a').unwrap().0, false), Some('a'));
        assert_eq!(typed(crate::keys::char_key('A').unwrap().0, true), Some('A'));
        assert_eq!(typed(crate::keys::char_key('1').unwrap().0, true), Some('!'));
        assert_eq!(typed(0x7B, false), None, "arrows type nothing");
    }

    #[test]
    fn mjpeg_parts_come_apart() {
        let stream = b"HTTP/1.0 200 OK\r\nServer: WDA MJPEG Server\r\n\r\n--BoundaryString\r\nContent-type: image/jpg\r\nContent-Length: 3\r\n\r\nabc\r\n\r\n--BoundaryString\r\nContent-type: image/jpg\r\nContent-Length: 2\r\n\r\nxy\r\n";
        let mut r = BufReader::new(&stream[..]);
        skip_headers(&mut r).unwrap();
        assert_eq!(next_part(&mut r).unwrap(), b"abc");
        assert_eq!(next_part(&mut r).unwrap(), b"xy");
        assert!(next_part(&mut r).is_err(), "the end of the stream ends it");
    }

    #[test]
    fn previews_shrink_by_averaging() {
        let src = [10u8, 20, 30, 255, 30, 40, 50, 255, 10, 20, 30, 255, 30, 40, 50, 255];
        assert_eq!(scale_bgra(&src, 2, 2, 1, 1), vec![20, 30, 40, 255]);
        assert_eq!(scale_bgra(&src, 2, 2, 2, 2), src.to_vec());
    }

    /// A computer with one display, recording what it was asked.
    struct Fake {
        layouts: Mutex<Vec<String>>,
    }

    impl VideoSource for Fake {
        fn streams(&self) -> Vec<DisplayInfo> {
            self.displays().unwrap()
        }
        fn request_keyframe(&self, _: Option<u8>) {}
        fn set_layout(&self, layout: Layout) -> Result<Vec<DisplayInfo>> {
            self.layouts.lock().unwrap().push(format!("{layout:?}"));
            self.displays()
        }
        fn displays(&self) -> Result<Vec<DisplayInfo>> {
            Ok(vec![DisplayInfo { id: 1, width: 2560, height: 1600, x: 0.0, y: 0.0, points_width: 1280.0, points_height: 800.0, main: true, builtin: true, phone: None }])
        }
        fn set_bitrate(&self, _: u32) {}
    }

    #[test]
    fn showing_a_phone_holds_the_computer_back_and_switching_back_resumes_it() {
        let (inner_tx, _) = broadcast::channel(8);
        let (tx, _) = broadcast::channel(8);
        let fake = Arc::new(Fake { layouts: Mutex::new(Vec::new()) });
        let (video, phones) = wrap(fake.clone(), inner_tx, tx, 1_000_000, 30);
        let feed = Arc::new(Feed {
            phone: Phone { name: "xr".into(), port: 8100, video: 9100, wifi: false },
            id: ID_BASE | 7,
            size: Mutex::new(Some(((828, 1792), (414.0, 896.0)))),
            live: AtomicBool::new(true),
            slot: AtomicI32::new(-1),
            keyframe: AtomicBool::new(false),
            width: AtomicUsize::new(0),
            latest: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        phones.feeds.lock().unwrap().push(feed.clone());
        let all = video.displays().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!((all[1].id, all[1].x, all[1].points_width), (ID_BASE | 7, 1320.0, 414.0), "beside the computer's");

        let streams = video.set_layout(Layout::Single(Some(ID_BASE | 7))).unwrap();
        assert_eq!((streams[0].id, streams[0].width), (ID_BASE | 7, 828));
        assert!(!video.inner_on.load(Ordering::Relaxed));
        assert_eq!(feed.slot.load(Ordering::Relaxed), 0);
        assert!(feed.keyframe.load(Ordering::Relaxed));
        assert_eq!(video.streams()[0].id, ID_BASE | 7);
        assert!(fake.layouts.lock().unwrap().is_empty(), "the computer's capture is left alone");

        video.set_layout(Layout::Single(None)).unwrap();
        assert!(video.inner_on.load(Ordering::Relaxed));
        assert_eq!(feed.slot.load(Ordering::Relaxed), -1);
        assert_eq!(video.streams()[0].id, 1);

        // A phone not sending its picture can't be shown.
        feed.live.store(false, Ordering::Relaxed);
        assert!(video.set_layout(Layout::Single(Some(ID_BASE | 7))).is_err());
        assert!(video.displays().unwrap().iter().all(|d| !is_phone_display(d.id)));
    }
}
