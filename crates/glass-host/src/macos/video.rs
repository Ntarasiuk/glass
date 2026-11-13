//! Owns the active capture + encoder pairs (one per streamed display) and switches
//! between showing one display and all of them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use glass_proto::MAX_STREAMS;
use objc2_core_graphics::CGDisplayIsAsleep;
use tokio::sync::broadcast;
use tracing::info;

use super::audio::{AudioPackets, Packetizer};
use super::capture::{self, Capture, Display, DisplayInfo, Gate, Source, Window};
use super::encode::{EncodedFrame, Encoder};
pub use crate::media::Layout;
use crate::media::VideoSource;

/// H.264 level 5.2 caps a frame at 36864 macroblocks (e.g. 4096x2304); HEVC (level 6.2)
/// takes 8192x4320, so a 5K display goes out at full size only in HEVC.
const MAX_MACROBLOCKS: f64 = 36864.0;
const MAX_HEVC_PIXELS: f64 = 8192.0 * 4320.0;

pub struct VideoConfig {
    pub fps: i32,
    pub bitrate: i32,
    pub scale: f64,
}

/// What a stream's encoder and capture run with; a change needs a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pipeline {
    hevc: bool,
    /// HEVC 4:2:2 (only with HEVC).
    rext: bool,
    full_colour: bool,
}

/// What to stream: one display (main when `None`) or every display.
struct Stream {
    capture: Capture,
    encoder: Arc<Encoder>,
    display: DisplayInfo,
    keyframe: Arc<AtomicBool>,
    /// Frame-rate ceiling the capture honours (from the sharpness policy).
    fps: Arc<std::sync::atomic::AtomicU32>,
    /// Recent output bytes (decaying), to share the bitrate by activity.
    activity: std::sync::atomic::AtomicU64,
    /// Encoded width (may be below native: dynamic resolution).
    width: usize,
    /// A single window rather than a display (`display` then describes the window).
    window: bool,
    /// The most frames its display can show (its refresh rate, capped by `--fps`).
    refresh: u32,
    /// Captured in Display P3 (a wide-colour display).
    p3: bool,
    /// Its display was asleep, which ends the capture: restart it once the display wakes.
    slept: AtomicBool,
    /// What it was started with.
    pipeline: Pipeline,
}

impl Stream {
    fn go_live(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
        self.capture.go_live();
    }

    /// Puts `new` in this one's place: it stops reaching viewers before `new` starts to, so a
    /// stream number never has two encoders feeding it.
    fn replace_with(&mut self, new: Stream) {
        self.capture.retire();
        let old = std::mem::replace(self, new);
        self.go_live();
        drop(old);
    }
}

/// The size a `w`×`h` stream is encoded at. `wanted`: the width viewers need.
fn encoded_size((w, h): (f64, f64), config_scale: f64, hevc: bool, wanted: Option<u32>) -> (usize, usize) {
    let max_pixels = if hevc { MAX_HEVC_PIXELS } else { MAX_MACROBLOCKS * 256.0 };
    let scale = config_scale.min((max_pixels / (w * h)).sqrt()).min(wanted.map_or(1.0, |t| t as f64 / w));
    (((w * scale) as usize) & !1, ((h * scale) as usize) & !1)
}

/// Within a few pixels is the same size (rounding to even).
fn same_width(running: usize, wanted: usize) -> bool {
    running.abs_diff(wanted) <= 16
}

/// What a stream captures.
enum Target<'a> {
    Display(&'a Display),
    Window(&'a Window),
}

impl Target<'_> {
    fn info(&self) -> DisplayInfo {
        match self {
            Target::Display(d) => d.info.clone(),
            Target::Window(w) => w.stream_info(),
        }
    }

    fn source(&self) -> Source<'_> {
        match self {
            Target::Display(d) => Source::Display(&d.sc),
            Target::Window(w) => Source::Window(&w.sc),
        }
    }
}

pub struct Video {
    config: VideoConfig,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    /// Shared across layout changes so audio sequence numbers stay continuous.
    audio: Arc<Mutex<Packetizer>>,
    streams: Mutex<Vec<Stream>>,
    /// Current adaptive target; new streams start at it.
    bitrate: std::sync::atomic::AtomicU32,
    /// Per stream: the width viewers need (`None` = native), from dynamic resolution.
    target_widths: Mutex<Vec<Option<u32>>>,
    /// Encode in HEVC rather than H.264 (crate::codec decides).
    hevc: AtomicBool,
    /// HEVC 4:2:2 for sharp text (crate::sharp decides; only while HEVC is on).
    rext: AtomicBool,
    /// Capture in BGRA for lossless refreshes (sharp text).
    full_colour: AtomicBool,
    /// The frame rate viewers want (crate::refresh): 60, or up to 120 for fast screens.
    max_fps: std::sync::atomic::AtomicU32,
    /// Draw the pointer into the picture (crate::cursor decides).
    cursor: AtomicBool,
    /// Displays an agent is waiting on that no stream shows, watched for changes alone.
    watches: Mutex<std::collections::HashMap<u32, capture::Watch>>,
    /// When `heal` last looked for streams not running what's wanted.
    reconciled: Mutex<Option<std::time::Instant>>,
}

/// How often a stream that couldn't take a new setting is tried again.
const RECONCILE_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

impl Video {
    pub fn new(
        config: VideoConfig,
        frames: broadcast::Sender<Arc<EncodedFrame>>,
        audio: AudioPackets,
    ) -> Self {
        let audio = Arc::new(Mutex::new(Packetizer::new(audio)));
        let bitrate = std::sync::atomic::AtomicU32::new(config.bitrate as u32);
        Self {
            config,
            frames,
            audio,
            streams: Mutex::new(Vec::new()),
            bitrate,
            target_widths: Mutex::new(Vec::new()),
            hevc: AtomicBool::new(false),
            rext: AtomicBool::new(false),
            full_colour: AtomicBool::new(false),
            max_fps: std::sync::atomic::AtomicU32::new(crate::refresh::DEFAULT),
            cursor: AtomicBool::new(true),
            watches: Mutex::new(Default::default()),
            reconciled: Mutex::new(None),
        }
    }

    /// Frames per second for a stream now: what its display and its viewers can show.
    fn fps_for(&self, refresh: u32) -> u32 {
        refresh.min(self.max_fps.load(Ordering::Relaxed)).max(1)
    }

    /// Viewers' frame rate changed: each capture delivers up to the new rate and each
    /// encoder's policy follows.
    pub fn set_max_fps(&self, fps: u32) {
        if self.max_fps.swap(fps, Ordering::Relaxed) == fps {
            return;
        }
        for (i, s) in self.streams.lock().unwrap().iter().enumerate() {
            let now = self.fps_for(s.refresh);
            s.capture.set_max_fps(now);
            info!("stream {i}: up to {now} fps");
        }
        self.rebalance();
    }

    /// The displays being streamed; index = stream number.
    pub fn streams(&self) -> Vec<DisplayInfo> {
        self.streams
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.display.clone())
            .collect()
    }

    /// Requests a keyframe on one stream, or all of them.
    pub fn request_keyframe(&self, stream: Option<u8>) {
        for (i, s) in self.streams.lock().unwrap().iter().enumerate() {
            if stream.is_none_or(|n| n as usize == i) {
                s.keyframe.store(true, Ordering::Relaxed);
                s.capture.refresh();
            }
        }
    }

    /// Restarts capture with `layout`. Blocks on ScreenCaptureKit. When it can't start, what
    /// was streaming goes on.
    pub fn set_layout(&self, layout: Layout) -> Result<Vec<DisplayInfo>> {
        let mut streams = self.streams.lock().unwrap();
        let started = if let Layout::Window(id) = layout {
            let windows = capture::list_windows()?;
            let window = windows
                .iter()
                .find(|w| w.info.id == id)
                .context("that window is gone")?;
            vec![self.start(0, &Target::Window(window), Some(self.audio.clone()))?]
        } else {
            let displays = capture::list_displays()?;
            let chosen: Vec<&Display> = match layout {
                Layout::Single(Some(id)) => {
                    vec![
                        displays
                            .iter()
                            .find(|d| d.info.id == id)
                            .context("unknown display")?,
                    ]
                }
                Layout::Single(None) => vec![&displays[0]],
                Layout::Window(_) => unreachable!("handled above"),
                Layout::All => {
                    // Match System Settings › Displays › Arrange: left to right, then top to bottom.
                    let mut all: Vec<&Display> = displays.iter().take(MAX_STREAMS).collect();
                    all.sort_by(|a, b| {
                        a.info
                            .x
                            .total_cmp(&b.info.x)
                            .then(a.info.y.total_cmp(&b.info.y))
                    });
                    all
                }
            };
            // Only the first stream carries audio, so it isn't heard once per display.
            chosen
                .into_iter()
                .enumerate()
                .map(|(index, display)| {
                    let audio = (index == 0).then(|| self.audio.clone());
                    self.start(index as u8, &Target::Display(display), audio)
                })
                .collect::<Result<Vec<_>>>()?
        };
        // The old ones stop reaching viewers first, so a stream number never has two encoders
        // feeding it.
        for s in streams.iter() {
            s.capture.retire();
        }
        let old = std::mem::replace(&mut *streams, started);
        for s in streams.iter() {
            s.go_live();
        }
        drop(old);
        Ok(streams.iter().map(|s| s.display.clone()).collect())
    }

    /// What streams should be running now.
    fn wanted_pipeline(&self) -> Pipeline {
        let hevc = self.hevc.load(Ordering::Relaxed);
        Pipeline {
            hevc,
            rext: hevc && self.rext.load(Ordering::Relaxed),
            full_colour: self.full_colour.load(Ordering::Relaxed),
        }
    }

    fn encoded_size(&self, info: &DisplayInfo, index: usize, hevc: bool) -> (usize, usize) {
        let wanted = self.target_widths.lock().unwrap().get(index).copied().flatten();
        encoded_size((info.width as f64, info.height as f64), self.config.scale, hevc, wanted)
    }

    /// Whether a stream isn't running what's wanted. One stopped from this Mac is left alone.
    fn out_of_date(&self, s: &Stream, index: usize) -> bool {
        let want = self.wanted_pipeline();
        !s.capture.user_stopped()
            && (s.pipeline != want || !same_width(s.width, self.encoded_size(&s.display, index, want.hevc).0))
    }

    fn start(
        &self,
        index: u8,
        target: &Target,
        audio: Option<Arc<Mutex<Packetizer>>>,
    ) -> Result<Stream> {
        let info = target.info();
        let pipeline = self.wanted_pipeline();
        let Pipeline { hevc, rext, full_colour } = pipeline;
        let (width, height) = self.encoded_size(&info, index as usize, hevc);

        // The display's own refresh (120 Hz ProMotion, a 144 Hz monitor…), within --fps; a
        // window follows the main display.
        let display_id = match target {
            Target::Display(d) => d.info.id,
            Target::Window(_) => objc2_core_graphics::CGMainDisplayID(),
        };
        let refresh = capture::refresh_rate(display_id).min(self.config.fps.max(1) as u32);
        let fps = self.fps_for(refresh);
        let p3 = capture::wide_colour(display_id);

        let keyframe = Arc::new(AtomicBool::new(true));
        let gate = Arc::new(Gate::default());
        let frames = self.frames.clone();
        let session = gate.clone();
        let encoder = Encoder::new(
            width as i32,
            height as i32,
            fps as i32,
            self.config.bitrate,
            hevc,
            rext,
            p3,
            keyframe.clone(),
            Arc::new(move |frame| {
                session.pass(|| {
                    let _ = frames.send(Arc::new(EncodedFrame {
                        stream: index,
                        ..frame
                    }));
                });
            }),
        )?;
        let encoder = Arc::new(encoder);
        let fps_cap = Arc::new(std::sync::atomic::AtomicU32::new(
            encoder.apply_policy(self.bitrate.load(Ordering::Relaxed), fps),
        ));
        let capture = capture::start(
            target.source(),
            index,
            width,
            height,
            fps as i32,
            encoder.clone(),
            gate,
            fps_cap.clone(),
            audio,
            full_colour,
            p3,
            self.cursor.load(Ordering::Relaxed),
        )?;
        let id = info.id;
        let window = matches!(target, Target::Window(_));
        info!(
            "stream {index}: {} {id} at {width}x{height} @ {fps}fps (display {refresh} Hz{}, {})",
            if window { "window" } else { "display" },
            if p3 { ", Display P3" } else { "" },
            if rext {
                "HEVC 4:2:2"
            } else if hevc {
                "HEVC"
            } else {
                "H.264"
            }
        );
        Ok(Stream {
            capture,
            encoder,
            display: info,
            keyframe,
            fps: fps_cap,
            activity: Default::default(),
            width,
            window,
            refresh,
            p3,
            slept: Default::default(),
            pipeline,
        })
    }

    /// Restarts the streams not running what's wanted (each starts on a keyframe), keeping
    /// any that can't: `heal` tries those again. How many it restarted.
    fn reconcile(&self, why: &str) -> usize {
        let stale = |streams: &[Stream]| streams.iter().enumerate().any(|(i, s)| self.out_of_date(s, i));
        if !stale(&self.streams.lock().unwrap()) {
            return 0;
        }
        let Ok(displays) = capture::list_displays() else {
            return 0;
        };
        let mut streams = self.streams.lock().unwrap();
        let windows = if streams.iter().any(|s| s.window) {
            capture::list_windows().unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut restarted = 0;
        for index in 0..streams.len() {
            if !self.out_of_date(&streams[index], index) {
                continue;
            }
            let id = streams[index].display.id;
            let target = if streams[index].window {
                match windows.iter().find(|w| w.info.id == id) {
                    Some(w) => Target::Window(w),
                    None => continue,
                }
            } else {
                match displays.iter().find(|d| d.info.id == id) {
                    Some(d) => Target::Display(d),
                    None => continue,
                }
            };
            let audio = (index == 0).then(|| self.audio.clone());
            match self.start(index as u8, &target, audio) {
                Ok(s) => {
                    streams[index].replace_with(s);
                    restarted += 1;
                }
                Err(e) => tracing::warn!("restarting stream {index} ({why}): {e:#}"),
            }
        }
        restarted
    }

    fn restart_all(&self, why: &str) {
        self.reconcile(why);
        self.rebalance();
    }

    /// Whether every stream runs with `running`, so what's asked for is what's in use.
    fn all_running(&self, running: impl Fn(&Pipeline) -> bool) -> bool {
        self.streams.lock().unwrap().iter().filter(|s| !s.capture.user_stopped()).all(|s| running(&s.pipeline))
    }

    pub fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Ok(capture::list_displays()?
            .into_iter()
            .map(|d| d.info)
            .collect())
    }

    pub fn windows(&self) -> Result<Vec<crate::media::WindowInfo>> {
        Ok(capture::list_windows()?
            .into_iter()
            .map(|w| w.info)
            .collect())
    }
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
    fn supports_hevc(&self) -> bool {
        super::encode::supports_hevc()
    }
    fn supports_rext(&self) -> bool {
        super::encode::supports_rext()
    }
    fn supports_lossless(&self) -> bool {
        true
    }
    /// BGRA capture, so a still screen can go out losslessly (VideoToolbox converts).
    fn set_full_colour(&self, on: bool) {
        if self.full_colour.swap(on, Ordering::Relaxed) != on {
            self.restart_all("full colour");
            info!("capture colour: {}", if on { "BGRA (lossless refresh)" } else { "4:2:0" });
        }
    }
    fn set_cursor_baked(&self, on: bool) {
        if self.cursor.swap(on, Ordering::Relaxed) != on {
            for s in self.streams.lock().unwrap().iter() {
                s.capture.set_shows_cursor(on);
            }
        }
    }
    /// A streamed display reports its changes already; another is watched while agents
    /// want it (`heal` stops it after).
    fn track_damage(&self, id: u32) -> bool {
        if self.streams.lock().unwrap().iter().any(|s| !s.window && s.display.id == id) {
            self.watches.lock().unwrap().remove(&id);
            return true;
        }
        {
            let mut watches = self.watches.lock().unwrap();
            if watches.get(&id).is_some_and(|w| !w.stopped()) {
                return true;
            }
            watches.remove(&id);
        }
        // Not holding the map: this waits on ScreenCaptureKit, and `heal` needs it every second.
        let watched = capture::list_displays()
            .and_then(|all| all.into_iter().find(|d| d.info.id == id).context("no such display"))
            .and_then(|d| capture::watch(&d));
        match watched {
            Ok(w) => {
                self.watches.lock().unwrap().insert(id, w);
                true
            }
            Err(e) => {
                tracing::debug!("display {id}: can't watch it for changes: {e:#}");
                false
            }
        }
    }
    fn picture(&self, id: u32, max: [u32; 2]) -> Option<crate::lossless::RawFrame> {
        capture::screenshot(id, max)
            .inspect_err(|e| tracing::debug!("preview of display {id}: {e:#}"))
            .ok()
    }
    fn frame_generation(&self, stream: u8) -> Option<u64> {
        let streams = self.streams.lock().unwrap();
        streams.get(stream as usize).map(|s| s.capture.generation())
    }
    fn set_max_fps(&self, fps: u32) {
        Video::set_max_fps(self, fps)
    }
    fn icc(&self, stream: u8) -> Option<Arc<Vec<u8>>> {
        static P3: std::sync::OnceLock<Option<Arc<Vec<u8>>>> = std::sync::OnceLock::new();
        let p3 = self.streams.lock().unwrap().get(stream as usize).is_some_and(|s| s.p3);
        p3.then(|| P3.get_or_init(|| capture::p3_icc().map(Arc::new)).clone()).flatten()
    }
    fn raw_frame(&self, stream: u8) -> Option<Arc<crate::lossless::RawFrame>> {
        let streams = self.streams.lock().unwrap();
        streams.get(stream as usize)?.capture.read_bgra().map(Arc::new)
    }
    /// Sharp text: HEVC 4:2:2 instead of 4:2:0 (applies while HEVC is on).
    fn set_rext(&self, on: bool) -> bool {
        let on = on && super::encode::supports_rext();
        self.rext.store(on, Ordering::Relaxed);
        if self.reconcile("chroma") > 0 {
            info!("video chroma: {}", if on { "4:2:2" } else { "4:2:0" });
            self.rebalance();
        }
        on && self.hevc.load(Ordering::Relaxed) && self.all_running(|p| p.rext)
    }
    /// Restarts every stream in the new codec (each starts on a keyframe).
    fn set_hevc(&self, hevc: bool) -> bool {
        let hevc = hevc && super::encode::supports_hevc();
        self.hevc.store(hevc, Ordering::Relaxed);
        if self.reconcile("codec") > 0 {
            info!("video codec: {}", if hevc { "HEVC" } else { "H.264" });
            self.rebalance();
        }
        // What's in use: a stream that couldn't restart is still on the other one until `heal`
        // gets it there.
        if self.all_running(|p| p.hevc == hevc) { hevc } else { !hevc }
    }
    fn supports_windows(&self) -> bool {
        true
    }
    fn windows(&self) -> Result<Vec<crate::media::WindowInfo>> {
        Video::windows(self)
    }
    /// `bps` is the total for all displays (not each): shared out by `rebalance`.
    fn set_bitrate(&self, bps: u32) {
        self.bitrate.store(bps, Ordering::Relaxed);
        self.rebalance();
    }

    /// Restarts any stream whose encoded size should change (dynamic resolution).
    fn set_target_widths(&self, widths: &[Option<u32>]) {
        *self.target_widths.lock().unwrap() = widths.to_vec();
        self.restart_all("resize");
    }

    /// Restarts streams that ended. ScreenCaptureKit stops a stream when its display sleeps
    /// (closing the lid, even for a second) and never resumes it, which froze the picture
    /// while input still worked. Also gets streams that couldn't take a new setting there.
    fn heal(&self) {
        // Watches no agent has wanted for a while end.
        self.watches.lock().unwrap().retain(|id, _| crate::damage::wanted(*id));
        {
            let mut streams = self.streams.lock().unwrap();
            let mut displays = None;
            let mut windows = None;
            for index in 0..streams.len() {
                let s = &streams[index];
                // Stopped from this Mac: it stays stopped, display asleep or not.
                if s.capture.user_stopped() {
                    continue;
                }
                let id = s.display.id;
                if s.window {
                    // A window that's gone is the server's watcher's to deal with.
                    if s.capture.stopped()
                        && let Some(window) = windows
                            .get_or_insert_with(|| capture::list_windows().unwrap_or_default())
                            .iter()
                            .find(|w| w.info.id == id)
                    {
                        let audio = (index == 0).then(|| self.audio.clone());
                        match self.start(index as u8, &Target::Window(window), audio) {
                            Ok(stream) => {
                                streams[index].replace_with(stream);
                                info!("stream {index}: window {id} capture restarted");
                            }
                            Err(e) => tracing::debug!("stream {index}: restart: {e:#}"),
                        }
                    }
                    continue;
                }
                if CGDisplayIsAsleep(id) {
                    s.slept.store(true, Ordering::Relaxed);
                    continue;
                }
                if !s.slept.swap(false, Ordering::Relaxed) && !s.capture.stopped() {
                    continue;
                }
                let list = displays.get_or_insert_with(capture::list_displays);
                let restarted = match list {
                    Ok(list) => match list.iter().find(|d| d.info.id == id) {
                        Some(display) => {
                            let audio = (index == 0).then(|| self.audio.clone());
                            self.start(index as u8, &Target::Display(display), audio)
                        }
                        None => Err(anyhow::anyhow!("display {id} isn't back yet")),
                    },
                    Err(e) => Err(anyhow::anyhow!("listing displays: {e:#}")),
                };
                match restarted {
                    Ok(stream) => {
                        // Same display and stream number, so viewers just get a new keyframe.
                        streams[index].replace_with(stream);
                        info!("stream {index}: display {id} capture restarted");
                    }
                    Err(e) => {
                        // Try again on the next check.
                        streams[index].slept.store(true, Ordering::Relaxed);
                        tracing::debug!("stream {index}: restart: {e:#}");
                    }
                }
            }
        }
        let due = {
            let mut at = self.reconciled.lock().unwrap();
            let due = at.is_none_or(|t| t.elapsed() >= RECONCILE_EVERY);
            if due {
                *at = Some(std::time::Instant::now());
            }
            due
        };
        // What's in use may now be what was asked for: the codec's owner should know.
        if due && self.reconcile("catching up") > 0 {
            crate::codec::reapply();
        }
    }

    /// Splits the total between displays by recent activity, and applies the sharpness
    /// policy (bitrate, frame-rate ceiling, quality cap) to each.
    fn rebalance(&self) {
        let streams = self.streams.lock().unwrap();
        let activity: Vec<u64> = streams
            .iter()
            .map(|s| {
                let recent = s.encoder.take_bytes() + s.activity.load(Ordering::Relaxed) / 2;
                s.activity.store(recent, Ordering::Relaxed);
                recent
            })
            .collect();
        let shares = crate::rate::split(self.bitrate.load(Ordering::Relaxed), &activity);
        for (s, bps) in streams.iter().zip(shares) {
            let fps = s.encoder.apply_policy(bps, self.fps_for(s.refresh));
            s.fps.store(fps, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_MACROBLOCKS, encoded_size, same_width};

    #[test]
    fn encoded_size_follows_the_codec_and_what_viewers_need() {
        assert_eq!(encoded_size((2560.0, 1440.0), 1.0, false, None), (2560, 1440));
        assert_eq!(encoded_size((2560.0, 1440.0), 1.0, false, Some(1280)), (1280, 720));
        assert_eq!(encoded_size((2560.0, 1440.0), 0.5, true, None), (1280, 720));
        // 5K: whole in HEVC, not in H.264.
        assert_eq!(encoded_size((5120.0, 2880.0), 1.0, true, None), (5120, 2880));
        let (w, h) = encoded_size((5120.0, 2880.0), 1.0, false, None);
        assert!(((w * h) as f64) <= MAX_MACROBLOCKS * 256.0 && w < 5120);
    }

    #[test]
    fn a_stream_held_below_native_by_a_codec_limit_isnt_out_of_date() {
        // Comparing with the display's native width would restart this one on every look.
        let running = encoded_size((5120.0, 2880.0), 1.0, false, None).0;
        assert!(same_width(running, encoded_size((5120.0, 2880.0), 1.0, false, None).0));
        assert!(!same_width(running, 5120));
        assert!(same_width(1290, 1280), "a few pixels are rounding");
    }
}
