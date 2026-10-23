//! Drawings from viewers, shown on the Mac itself: a transparent, click-through window over
//! each streamed display, above other windows, that the person at the computer sees and
//! nothing else notices. Strokes fade like they do in the viewer.
//!
//! The windows are marked not to be shared (`NSWindowSharingNone`), so screen capture
//! leaves them out and viewers don't see each stroke twice. (Some macOS versions capture
//! such windows anyway with ScreenCaptureKit; then viewers see the captured stroke under
//! their own, in the same place.)
//!
//! Linux has no overlay: Wayland gives a client no portable way to draw above every window.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSLineCapStyle, NSLineJoinStyle, NSScreen, NSView,
    NSWindow, NSWindowCollectionBehavior, NSWindowSharingType, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::control::ServerMsg;
use crate::media::DisplayInfo;

const FADE_AFTER: Duration = Duration::from_millis(6000);
const FADE: Duration = Duration::from_millis(2000);
const LINE_WIDTH: f64 = 4.0;
/// Above normal and floating windows and the menu bar, below screen savers.
const LEVEL: isize = 101; // NSPopUpMenuWindowLevel

struct Stroke {
    display: u32,
    color: [f64; 3],
    points: Vec<[f64; 2]>,
    updated: Instant,
}

struct Overlay {
    window: Retained<NSWindow>,
    view: Retained<OverlayView>,
    frame: NSRect,
}

#[derive(Default)]
struct State {
    /// (sender, stroke id) -> stroke
    strokes: HashMap<(u64, String), Stroke>,
    windows: HashMap<u32, Overlay>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Whether strokes are on screen (so fading needs redraws).
static ACTIVE: AtomicBool = AtomicBool::new(false);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlassOverlayView"]
    #[ivars = u32]
    struct OverlayView;

    impl OverlayView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let display = *self.ivars();
            let size = self.frame().size;
            STATE.with(|s| {
                let s = s.borrow();
                let now = Instant::now();
                for stroke in s.strokes.values().filter(|st| st.display == display) {
                    let age = now.duration_since(stroke.updated);
                    let alpha = if age < FADE_AFTER {
                        1.0
                    } else {
                        1.0 - (age - FADE_AFTER).as_secs_f64() / FADE.as_secs_f64()
                    };
                    if alpha <= 0.0 || stroke.points.is_empty() {
                        continue;
                    }
                    let [r, g, b] = stroke.color;
                    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, alpha).setStroke();
                    let path = NSBezierPath::bezierPath();
                    path.setLineWidth(LINE_WIDTH);
                    path.setLineCapStyle(NSLineCapStyle::Round);
                    path.setLineJoinStyle(NSLineJoinStyle::Round);
                    let at = |p: &[f64; 2]| NSPoint::new(p[0] * size.width, p[1] * size.height);
                    path.moveToPoint(at(&stroke.points[0]));
                    for p in &stroke.points[1..] {
                        path.lineToPoint(at(p));
                    }
                    if stroke.points.len() == 1 {
                        let p = at(&stroke.points[0]);
                        path.lineToPoint(NSPoint::new(p.x + 0.1, p.y));
                    }
                    path.stroke();
                }
            });
        }
    }
);

impl OverlayView {
    fn new(mtm: MainThreadMarker, display: u32, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(display);
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// Starts showing viewers' drawings. Call from the main thread's setup (the menu bar).
pub fn start(rt: &tokio::runtime::Handle) {
    rt.spawn(async {
        // The server publishes its shared state once it's listening.
        let shared = loop {
            if let Some(s) = crate::server::shared() {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        let mut events = shared.events.subscribe();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                msg = events.recv() => match msg {
                    Ok(ServerMsg::Annotate { sender, stroke }) => {
                        let streams = shared.video.streams();
                        DispatchQueue::main().exec_async(move || add(sender, &stroke, &streams));
                    }
                    Ok(ServerMsg::AnnotateClear { sender }) => {
                        DispatchQueue::main().exec_async(move || clear(sender));
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                _ = tick.tick() => {
                    if ACTIVE.load(Ordering::Relaxed) {
                        DispatchQueue::main().exec_async(redraw);
                    }
                }
            }
        }
    });
}

/// "#ff8800" -> sRGB components.
fn parse_color(c: Option<&str>) -> [f64; 3] {
    let c = c.unwrap_or("").trim_start_matches('#');
    let channel = |i: usize| {
        c.get(i..i + 2)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .map(|v| v as f64 / 255.0)
    };
    match (c.len(), channel(0), channel(2), channel(4)) {
        (6, Some(r), Some(g), Some(b)) => [r, g, b],
        _ => [1.0, 0.27, 0.23],
    }
}

fn add(sender: u64, stroke: &serde_json::Value, streams: &[DisplayInfo]) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let id = match &stroke["id"] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return,
    };
    let stream = stroke["stream"].as_u64().unwrap_or(0) as usize;
    let Some(display) = streams.get(stream) else { return };
    let points: Vec<[f64; 2]> = stroke["points"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some([p.get(0)?.as_f64()?.clamp(0.0, 1.0), p.get(1)?.as_f64()?.clamp(0.0, 1.0)]))
        .collect();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        ensure_window(&mut s, mtm, display);
        let entry = s.strokes.entry((sender, id)).or_insert_with(|| Stroke {
            display: display.id,
            color: parse_color(stroke["color"].as_str()),
            points: Vec::new(),
            updated: Instant::now(),
        });
        entry.points.extend(points);
        entry.updated = Instant::now();
    });
    ACTIVE.store(true, Ordering::Relaxed);
    redraw();
}

fn clear(sender: u64) {
    STATE.with(|s| s.borrow_mut().strokes.retain(|(from, _), _| *from != sender));
    redraw();
}

/// An overlay window covering `display`, made the first time it's drawn on.
fn ensure_window(s: &mut State, mtm: MainThreadMarker, display: &DisplayInfo) {
    // Display arrangement is in global points from the main display's top left; Cocoa
    // frames start at its bottom left.
    let screens = NSScreen::screens(mtm);
    let main_height = screens.iter().next().map_or(0.0, |m| m.frame().size.height);
    let frame = NSRect::new(
        NSPoint::new(display.x, main_height - display.y - display.points_height),
        NSSize::new(display.points_width, display.points_height),
    );
    if let Some(o) = s.windows.get_mut(&display.id) {
        if o.frame != frame {
            o.window.setFrame_display(frame, true);
            o.view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
            o.frame = frame;
        }
        return;
    }
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(false);
    window.setIgnoresMouseEvents(true);
    window.setLevel(LEVEL);
    // Leave it out of screen capture, so viewers don't get every stroke twice.
    window.setSharingType(NSWindowSharingType::None);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    let view = OverlayView::new(mtm, display.id, NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
    window.setContentView(Some(&view));
    s.windows.insert(display.id, Overlay { window, view, frame });
}

/// Repaints every overlay, drops faded strokes, and hides windows with nothing to show.
fn redraw() {
    // Collected first: AppKit may draw (and so read the state) during these calls.
    let windows: Vec<(Retained<NSWindow>, Retained<OverlayView>, bool)> = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let now = Instant::now();
        s.strokes.retain(|_, st| now.duration_since(st.updated) < FADE_AFTER + FADE);
        ACTIVE.store(!s.strokes.is_empty(), Ordering::Relaxed);
        let shown: Vec<u32> = s.strokes.values().map(|st| st.display).collect();
        s.windows
            .iter()
            .map(|(id, o)| (o.window.clone(), o.view.clone(), shown.contains(id)))
            .collect()
    });
    for (window, view, show) in windows {
        if show {
            window.orderFrontRegardless();
            view.setNeedsDisplay(true);
        } else {
            window.orderOut(None);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn colors() {
        assert_eq!(super::parse_color(Some("#ff0000")), [1.0, 0.0, 0.0]);
        assert_eq!(super::parse_color(Some("nope")), [1.0, 0.27, 0.23]);
    }
}
