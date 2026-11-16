//! The other computer's pointer as this Mac's own (its shapes as `NSCursor`s, so it moves
//! with no delay at all), and this Mac's clipboard.

use std::collections::HashMap;

use base64::Engine;
use glass_proto::{CURSOR_BAKED, CURSOR_VISIBLE, CursorPos};
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_app_kit::{NSCursor, NSImage, NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF};
use objc2_foundation::{NSData, NSPoint, NSSize};
use serde_json::Value;

fn png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    drop(writer);
    Some(out)
}

fn cursor(png: &[u8], points: (f64, f64), hotspot: (f64, f64)) -> Option<Retained<NSCursor>> {
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png))?;
    image.setSize(NSSize::new(points.0, points.1));
    Some(NSCursor::initWithImage_hotSpot(NSCursor::alloc(), &image, NSPoint::new(hotspot.0, hotspot.1)))
}

/// The host's pointer shapes, by the id its datagrams name.
pub struct Cursors {
    shapes: HashMap<u32, Retained<NSCursor>>,
    hidden: Retained<NSCursor>,
    /// What's set now (shape, flags), so it's only set when it changes.
    current: Option<(u32, u8)>,
}

impl Default for Cursors {
    fn default() -> Self {
        let clear = png(1, 1, &[0, 0, 0, 0]).and_then(|p| cursor(&p, (1.0, 1.0), (0.0, 0.0))).unwrap_or_else(NSCursor::arrowCursor);
        Self { shapes: HashMap::new(), hidden: clear, current: None }
    }
}

impl Cursors {
    /// A `cursor` message: one more shape.
    pub fn add(&mut self, msg: &Value) {
        let n = |k: &str| msg[k].as_u64().unwrap_or(0) as u32;
        let (id, w, h) = (n("id"), n("w"), n("h"));
        let scale = msg["scale"].as_f64().unwrap_or(1.0).max(0.1);
        let Some(rgba) = msg["rgba"].as_str().and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok()) else { return };
        if w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
            return;
        }
        let Some(p) = png(w, h, &rgba) else { return };
        let points = (w as f64 / scale, h as f64 / scale);
        let hot = (n("hx") as f64 / scale, n("hy") as f64 / scale);
        if let Some(c) = cursor(&p, points, hot) {
            self.shapes.insert(id, c);
            self.current = None;
        }
    }

    /// Shows the host's pointer as it is now (over the picture).
    pub fn apply(&mut self, pos: Option<CursorPos>) {
        let key = pos.map_or((u32::MAX, CURSOR_VISIBLE), |p| (p.shape, p.flags));
        if self.current == Some(key) {
            return;
        }
        self.current = Some(key);
        let c = match pos {
            // Hidden there (typing, a video), or drawn into the picture for someone else.
            Some(p) if p.flags & CURSOR_VISIBLE == 0 || p.flags & CURSOR_BAKED != 0 => &self.hidden,
            Some(p) => match self.shapes.get(&p.shape) {
                Some(c) => c,
                None => return self.arrow(),
            },
            None => return self.arrow(),
        };
        c.set();
    }

    /// Back to the arrow (off the picture).
    pub fn arrow(&mut self) {
        self.current = None;
        NSCursor::arrowCursor().set();
    }

    /// Forgets what's set, so the next `apply` sets it again (GPUI may have changed it).
    pub fn forget(&mut self) {
        self.current = None;
    }
}

/// Changes each time something is copied on this Mac.
pub fn clipboard_count() -> isize {
    NSPasteboard::generalPasteboard().changeCount()
}

/// This Mac's clipboard: text, HTML, and whether there's an image.
pub fn clipboard() -> (String, Option<String>, bool) {
    let pb = NSPasteboard::generalPasteboard();
    let text = pb.stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string()).unwrap_or_default();
    let html = pb.stringForType(unsafe { NSPasteboardTypeHTML }).map(|s| s.to_string()).filter(|h| h.len() <= crate::clip::MAX_HTML);
    let types = pb.types().map(|t| t.iter().map(|t| t.to_string()).collect::<Vec<_>>()).unwrap_or_default();
    let image = [unsafe { NSPasteboardTypePNG }, unsafe { NSPasteboardTypeTIFF }].iter().any(|t| types.iter().any(|x| *x == t.to_string()));
    (text, html, image)
}
