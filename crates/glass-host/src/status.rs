//! A small status file the running host keeps current, so `glass setup` (a separate
//! process) can show what the background service can and can't do yet. macOS grants
//! Screen Recording and Accessibility per process, so only the host itself knows.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const REFRESH: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub pid: u32,
    pub screen_recording: bool,
    pub accessibility: bool,
    /// At least one display is being captured.
    pub capturing: bool,
    /// Where to open the client, if known.
    pub url: Option<String>,
    /// Unix seconds of the last write; a stale value means the host isn't running.
    pub updated: u64,
}

pub fn path() -> PathBuf {
    crate::config_dir().join("status.json")
}

pub fn read() -> Option<Status> {
    serde_json::from_slice(&std::fs::read(path()).ok()?).ok()
}

impl Status {
    /// True when written recently by a live process.
    pub fn is_fresh(&self) -> bool {
        unix_now().saturating_sub(self.updated) <= 5
    }
}

static CURRENT: Mutex<Option<Status>> = Mutex::new(None);

/// Starts keeping the status file current. `update` may be called from anywhere after.
pub fn start(url: Option<String>) {
    *CURRENT.lock().unwrap() = Some(Status {
        version: env!("CARGO_PKG_VERSION").into(),
        pid: std::process::id(),
        url,
        ..Default::default()
    });
    std::thread::spawn(|| {
        loop {
            let snapshot = CURRENT.lock().unwrap().as_mut().map(|s| {
                s.accessibility = crate::input::has_accessibility();
                s.screen_recording = s.capturing || crate::permissions::has_screen_recording();
                s.updated = unix_now();
                s.clone()
            });
            if let Some(s) = snapshot {
                let _ = std::fs::create_dir_all(path().parent().unwrap());
                let tmp = path().with_extension("tmp");
                if std::fs::write(&tmp, serde_json::to_vec_pretty(&s).unwrap()).is_ok() {
                    let _ = std::fs::rename(&tmp, path());
                }
            }
            std::thread::sleep(REFRESH);
        }
    });
}

pub fn set_capturing(capturing: bool) {
    if let Some(s) = CURRENT.lock().unwrap().as_mut() {
        s.capturing = capturing;
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
