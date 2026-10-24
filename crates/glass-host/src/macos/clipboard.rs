//! Mac clipboard access and change polling: text, rich text (HTML) and images.

use std::time::Duration;

use objc2::AllocAnyThread;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSData, NSDictionary, NSString};
use tokio::sync::broadcast;

use crate::clip::{self, Clip};
use crate::control::ServerMsg;

const POLL: Duration = Duration::from_millis(300);

pub fn set_text(text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
}

/// Text, HTML and an image at once. The image goes on as PNG and TIFF (older apps only
/// read TIFF), whatever format it came in.
pub fn set(c: &Clip) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    if !c.text.is_empty() {
        pb.setString_forType(&NSString::from_str(&c.text), unsafe { NSPasteboardTypeString });
    }
    if let Some(html) = &c.html {
        pb.setString_forType(&NSString::from_str(html), unsafe { NSPasteboardTypeHTML });
    }
    if let Some(bytes) = &c.image {
        let data = NSData::with_bytes(bytes);
        let tiff = NSImage::initWithData(NSImage::alloc(), &data).and_then(|i| i.TIFFRepresentation());
        let png = if clip::is_png(bytes) { Some(data) } else { tiff.as_deref().and_then(png_from) };
        if let Some(png) = &png {
            pb.setData_forType(Some(png), unsafe { NSPasteboardTypePNG });
        }
        if let Some(tiff) = &tiff {
            pb.setData_forType(Some(tiff), unsafe { NSPasteboardTypeTIFF });
        }
    }
}

fn png_from(data: &NSData) -> Option<objc2::rc::Retained<NSData>> {
    let rep = NSBitmapImageRep::imageRepWithData(data)?;
    unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
}

/// The clipboard's image as PNG (converted from TIFF when that's all there is).
pub fn image_png() -> Option<Vec<u8>> {
    let pb = NSPasteboard::generalPasteboard();
    if let Some(png) = pb.dataForType(unsafe { NSPasteboardTypePNG }) {
        return Some(png.to_vec());
    }
    let tiff = pb.dataForType(unsafe { NSPasteboardTypeTIFF })?;
    png_from(&tiff).map(|d| d.to_vec())
}

/// What a viewer hears about a change: text, rich text, and whether there's an image.
fn snapshot() -> ServerMsg {
    let pb = NSPasteboard::generalPasteboard();
    let text = pb.stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string()).unwrap_or_default();
    let html = clip::html_for_viewer(pb.stringForType(unsafe { NSPasteboardTypeHTML }).map(|s| s.to_string()));
    let types = pb.types().map(|t| t.iter().map(|t| t.to_string()).collect::<Vec<_>>()).unwrap_or_default();
    let image = [unsafe { NSPasteboardTypePNG }, unsafe { NSPasteboardTypeTIFF }]
        .iter()
        .any(|t| types.iter().any(|x| *x == t.to_string()));
    ServerMsg::Clipboard { text, html, image }
}

/// Broadcasts the clipboard whenever it changes on the Mac.
pub fn watch(events: broadcast::Sender<ServerMsg>) {
    std::thread::spawn(move || {
        let mut last = NSPasteboard::generalPasteboard().changeCount();
        loop {
            std::thread::sleep(POLL);
            let count = NSPasteboard::generalPasteboard().changeCount();
            if count == last {
                continue;
            }
            last = count;
            if let msg @ ServerMsg::Clipboard { .. } = snapshot()
                && matches!(&msg, ServerMsg::Clipboard { text, html, image } if !text.is_empty() || html.is_some() || *image)
            {
                let _ = events.send(msg);
            }
        }
    });
}

/// The real system clipboard.
pub struct SystemClipboard;

impl crate::media::Clipboard for SystemClipboard {
    fn set_text(&self, text: &str) {
        set_text(text)
    }

    fn set(&self, c: &Clip) {
        set(c)
    }

    fn image_png(&self) -> Option<Vec<u8>> {
        image_png()
    }
}
