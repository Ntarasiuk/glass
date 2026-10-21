//! Mac clipboard (text) access and change polling.

use std::time::Duration;

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;
use tokio::sync::broadcast;

use crate::control::ServerMsg;

const POLL: Duration = Duration::from_millis(300);

pub fn set_text(text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
}

fn snapshot() -> (isize, Option<String>) {
    let pb = NSPasteboard::generalPasteboard();
    let text = pb
        .stringForType(unsafe { NSPasteboardTypeString })
        .map(|s| s.to_string());
    (pb.changeCount(), text)
}

/// Broadcasts the clipboard text whenever it changes on the Mac.
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
            if let (_, Some(text)) = snapshot() {
                let _ = events.send(ServerMsg::Clipboard { text });
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
}
