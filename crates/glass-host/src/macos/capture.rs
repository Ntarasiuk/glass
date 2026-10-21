//! Display capture with ScreenCaptureKit. Frames arrive as IOSurface-backed
//! pixel buffers and go straight to the hardware encoder without a copy.

use std::sync::{Arc, mpsc};

use anyhow::{Context, Result, anyhow, bail};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayIsBuiltin, CGDisplayIsMain, CGDisplayPixelsHigh,
    CGDisplayPixelsWide,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::CVImageBuffer;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType,
};

use super::audio::Packetizer;
use super::clock::{cmtime_us, now_us};
use super::encode::Encoder;
pub use crate::media::DisplayInfo;

/// kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange ('420v'), the encoder's native input.
const PIXEL_FORMAT_420V: u32 = u32::from_be_bytes(*b"420v");

/// The most recent captured frame, kept so a keyframe can be produced on demand: a static
/// screen yields no new frames, and a client that just connected would otherwise wait.
struct LastFrame(Option<CFRetained<CVImageBuffer>>);
// CVPixelBuffers are thread-safe, reference-counted CF objects.
unsafe impl Send for LastFrame {}

pub struct FrameState {
    encoder: Arc<Encoder>,
    last: std::sync::Mutex<LastFrame>,
    /// Frame-rate ceiling from the sharpness policy (fewer, sharper frames on a slow link).
    fps_cap: Arc<std::sync::atomic::AtomicU32>,
    /// Capture time of the last frame actually encoded.
    last_encoded_us: std::sync::atomic::AtomicU64,
    /// Bumped on every new image; a pending check that sees it unchanged knows the screen
    /// has gone still.
    generation: std::sync::atomic::AtomicU64,
    /// The newest image was skipped by the frame-rate ceiling and still needs sending.
    unsent: std::sync::atomic::AtomicBool,
    /// Idle refreshes done since the screen went still, and how many came out tiny.
    refreshes: std::sync::atomic::AtomicU32,
    tiny: std::sync::atomic::AtomicU32,
    queue: dispatch2::DispatchRetained<DispatchQueue>,
}

/// A still screen is re-encoded this often for a while, so it sharpens to near-lossless
/// after motion (each pass refines the previous one).
const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(83);
/// At most this many refreshes (3 s), and stop after 3 tiny outputs (already sharp).
const MAX_REFRESHES: u32 = 36;
const TINY_FRAME: u64 = 1024;

impl FrameState {
    fn encode_last(&self) {
        let Some(image) = self.last.lock().unwrap().0.clone() else {
            return;
        };
        let now = now_us();
        self.last_encoded_us
            .store(now, std::sync::atomic::Ordering::Relaxed);
        self.encoder
            .encode(&image, unsafe { CMTime::new(now as i64, 1_000_000) }, now);
    }

    /// After a frame (sent or skipped): if nothing newer arrives, send what was skipped,
    /// then keep refining the still picture for a while.
    fn schedule_idle_check(self: &Arc<Self>) {
        use std::sync::atomic::Ordering::Relaxed;
        let fps = self.fps_cap.load(Relaxed).max(1);
        let wait = REFRESH_INTERVAL.max(std::time::Duration::from_micros(1_000_000 / fps as u64));
        let generation = self.generation.load(Relaxed);
        let state = self.clone();
        let Ok(when) = dispatch2::DispatchTime::try_from(wait) else {
            return;
        };
        let _ = self.queue.after(when, move || {
            if state.generation.load(Relaxed) != generation {
                return; // newer frames took over
            }
            if state.unsent.swap(false, Relaxed) {
                state.encode_last();
            } else {
                if state.encoder.last_frame_size() < TINY_FRAME {
                    state.tiny.fetch_add(1, Relaxed);
                }
                if state.refreshes.fetch_add(1, Relaxed) >= MAX_REFRESHES
                    || state.tiny.load(Relaxed) >= 3
                {
                    return;
                }
                state.encode_last();
            }
            state.schedule_idle_check();
        });
    }
}

pub struct Ivars {
    state: Arc<FrameState>,
    audio: Option<Arc<std::sync::Mutex<Packetizer>>>,
    display: CGDirectDisplayID,
    started: std::time::Instant,
    frames: std::sync::atomic::AtomicU64,
    idle: std::sync::atomic::AtomicU64,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "GlassStreamOutput"]
    #[ivars = Ivars]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Audio {
                if let Some(audio) = &self.ivars().audio {
                    audio.lock().unwrap().push(sample);
                }
                return;
            }
            if kind != SCStreamOutputType::Screen {
                return;
            }
            use std::sync::atomic::Ordering::Relaxed;
            let ivars = self.ivars();
            // Idle frames (screen unchanged) carry no pixel buffer.
            let Some(image) = (unsafe { sample.image_buffer() }) else {
                if ivars.idle.fetch_add(1, Relaxed) == 0 && ivars.frames.load(Relaxed) == 0 {
                    tracing::info!("display {}: idle before any frame", ivars.display);
                }
                return;
            };
            if ivars.frames.fetch_add(1, Relaxed) == 0 {
                tracing::info!(
                    "display {}: first frame after {} ms",
                    ivars.display,
                    ivars.started.elapsed().as_millis()
                );
            }
            let pts = unsafe { sample.presentation_time_stamp() };
            let capture_us = match cmtime_us(pts) {
                0 => now_us(),
                us => us,
            };
            let state = &ivars.state;
            state.generation.fetch_add(1, Relaxed);
            state.refreshes.store(0, Relaxed);
            state.tiny.store(0, Relaxed);
            // Over the frame-rate ceiling: keep the image, send it later if it's the last.
            let fps = state.fps_cap.load(Relaxed);
            let since = capture_us.saturating_sub(state.last_encoded_us.load(Relaxed));
            let too_soon = fps > 0 && since < 900_000 / fps as u64;
            if too_soon && !state.encoder.keyframe_pending() {
                state.unsent.store(true, Relaxed);
            } else {
                state.unsent.store(false, Relaxed);
                state.last_encoded_us.store(capture_us, Relaxed);
                state.encoder.encode(&image, pts, capture_us);
            }
            state.last.lock().unwrap().0 = Some(image);
            state.schedule_idle_check();
        }
    }
);

impl StreamOutput {
    fn new(
        state: Arc<FrameState>,
        audio: Option<Arc<std::sync::Mutex<Packetizer>>>,
        display: CGDirectDisplayID,
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars {
            state,
            audio,
            display,
            started: std::time::Instant::now(),
            frames: Default::default(),
            idle: Default::default(),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// Keeps the stream and its output alive; capture stops when dropped.
pub struct Capture {
    stream: Retained<SCStream>,
    state: Arc<FrameState>,
    _output: Retained<StreamOutput>,
    queue: dispatch2::DispatchRetained<DispatchQueue>,
    _audio_queue: dispatch2::DispatchRetained<DispatchQueue>,
}

// SCStream is safe to stop from any thread; the manager serializes access.
unsafe impl Send for Capture {}

impl Capture {
    /// Re-encodes the last captured frame (on the capture queue, so it stays ordered with
    /// new frames). With a keyframe pending, this delivers it even if the screen is static.
    pub fn refresh(&self) {
        let state = self.state.clone();
        self.queue.exec_async(move || state.encode_last());
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new(move |_err: *mut NSError| {
            let _ = tx.send(());
        });
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&handler)) };
        let _ = rx.recv_timeout(std::time::Duration::from_secs(2));
    }
}

pub struct Display {
    pub sc: Retained<SCDisplay>,
    pub info: DisplayInfo,
}

unsafe impl Send for Display {}

/// All capturable displays, main display first. Sleeping displays are not listed.
pub fn list_displays() -> Result<Vec<Display>> {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        let result = if content.is_null() {
            Err(match unsafe { err.as_ref() } {
                Some(e) => anyhow!("{}", e.localizedDescription()),
                None => anyhow!("no shareable content"),
            })
        } else {
            Ok(unsafe { Retained::retain(content).unwrap() })
        };
        let _ = tx.send(result);
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    let content = rx.recv()?.context(
        "Screen Recording permission is required (System Settings → Privacy & Security)",
    )?;

    let mut displays: Vec<Display> = unsafe { content.displays() }
        .iter()
        .map(|sc| {
            let id = unsafe { sc.displayID() };
            let bounds = CGDisplayBounds(id);
            let info = DisplayInfo {
                id,
                width: CGDisplayPixelsWide(id),
                height: CGDisplayPixelsHigh(id),
                x: bounds.origin.x,
                y: bounds.origin.y,
                points_width: bounds.size.width,
                points_height: bounds.size.height,
                main: CGDisplayIsMain(id),
                builtin: CGDisplayIsBuiltin(id),
            };
            Display { sc, info }
        })
        .collect();
    displays.sort_by_key(|d| !d.info.main);
    if displays.is_empty() {
        bail!("no displays");
    }
    Ok(displays)
}

pub fn start(
    display: &SCDisplay,
    width: usize,
    height: usize,
    fps: i32,
    encoder: Arc<Encoder>,
    fps_cap: Arc<std::sync::atomic::AtomicU32>,
    audio: Option<Arc<std::sync::Mutex<Packetizer>>>,
) -> Result<Capture> {
    let captures_audio = audio.is_some();
    let filter = unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            display,
            &NSArray::new(),
        )
    };
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(width);
        config.setHeight(height);
        config.setMinimumFrameInterval(CMTime::new(1, fps));
        config.setPixelFormat(PIXEL_FORMAT_420V);
        config.setShowsCursor(true);
        // One buffer is held as the last frame, so leave the pool room for new ones.
        config.setQueueDepth(4);
        config.setCapturesAudio(captures_audio);
        config.setExcludesCurrentProcessAudio(true);
        config.setSampleRate(glass_proto::AUDIO_SAMPLE_RATE as isize);
        config.setChannelCount(2);
    }

    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None)
    };
    let queue = DispatchQueue::new("glass.capture", DispatchQueueAttr::SERIAL);
    let state = Arc::new(FrameState {
        encoder,
        last: std::sync::Mutex::new(LastFrame(None)),
        fps_cap,
        last_encoded_us: Default::default(),
        generation: Default::default(),
        unsent: Default::default(),
        refreshes: Default::default(),
        tiny: Default::default(),
        queue: queue.clone(),
    });
    let output = StreamOutput::new(state.clone(), audio, unsafe { display.displayID() });
    // Separate queue so audio never waits behind a frame being encoded.
    let audio_queue = DispatchQueue::new("glass.audio", DispatchQueueAttr::SERIAL);
    unsafe {
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
            .map_err(|e| anyhow!("addStreamOutput: {}", e.localizedDescription()))?;
        if captures_audio {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Audio,
                    Some(&audio_queue),
                )
                .map_err(|e| anyhow!("addStreamOutput(audio): {}", e.localizedDescription()))?;
        }
    }

    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |err: *mut NSError| {
        let _ = tx.send(unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string()));
    });
    unsafe { stream.startCaptureWithCompletionHandler(Some(&handler)) };
    if let Some(err) = rx.recv()? {
        bail!("startCapture: {err}");
    }

    Ok(Capture {
        stream,
        state,
        _output: output,
        queue,
        _audio_queue: audio_queue,
    })
}
