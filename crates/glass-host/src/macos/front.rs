//! What an agent is about to touch: the app in front, and whether a password field has
//! the keyboard (secure input).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use objc2_app_kit::NSWorkspace;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

/// The front app's bundle id (asked at most every 200 ms: a drag checks at every step).
pub fn frontmost_app() -> Option<String> {
    static CACHE: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    if let Some((at, app)) = cache.as_ref()
        && at.elapsed() < Duration::from_millis(200)
    {
        return app.clone();
    }
    let app = NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|a| a.bundleIdentifier())
        .map(|id| id.to_string());
    *cache = Some((Instant::now(), app.clone()));
    app
}

/// Some app turned on secure input (a password field has focus): typing is private.
pub fn secure_input() -> bool {
    // SAFETY: a plain query with no arguments.
    unsafe { IsSecureEventInputEnabled() != 0 }
}
