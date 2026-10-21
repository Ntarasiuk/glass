//! Wayland clipboard (text) via wl-clipboard's `wl-copy` / `wl-paste`.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::broadcast;

use crate::control::ServerMsg;

const POLL: Duration = Duration::from_millis(500);

/// What we last put on (or saw on) the clipboard, so our own writes aren't echoed back.
static LAST: Mutex<Option<String>> = Mutex::new(None);

pub fn set_text(text: &str) {
    *LAST.lock().unwrap() = Some(text.to_owned());
    // wl-copy forks a process that keeps serving the selection after we return.
    let child = Command::new("wl-copy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    match child {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
        Err(e) => tracing::warn!("wl-copy: {e} (install wl-clipboard)"),
    }
}

fn read_text() -> Option<String> {
    let out = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text/plain"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Broadcasts clipboard text when it changes.
pub fn watch(events: broadcast::Sender<ServerMsg>) {
    std::thread::spawn(move || {
        *LAST.lock().unwrap() = read_text();
        loop {
            std::thread::sleep(POLL);
            let Some(text) = read_text() else { continue };
            let mut last = LAST.lock().unwrap();
            if last.as_deref() == Some(text.as_str()) {
                continue;
            }
            *last = Some(text.clone());
            drop(last);
            let _ = events.send(ServerMsg::Clipboard { text });
        }
    });
}
