//! Privacy mode: while someone is connected, this computer's own screen goes black and
//! its keyboard and mouse are ignored, so nobody in the room sees or interferes. Capture
//! is unaffected: viewers still see the desktop.
//!
//! On when `glass config privacy on` (whenever anyone is viewing), when an owner turns it
//! on from the viewer, or with `glass privacy on`; or always, or never, by policy. Always
//! off once nobody is connected.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

struct State {
    /// Set by the CLI or a viewer; `None` follows the setting.
    forced: Option<bool>,
    viewers: usize,
    engaged: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    forced: None,
    viewers: 0,
    engaged: false,
});

pub fn supported() -> bool {
    // Windows: it depends on the build (10 2004 or later), not just the target.
    #[cfg(windows)]
    let here = crate::platform::privacy::supported();
    #[cfg(not(windows))]
    let here = crate::platform::privacy::SUPPORTED;
    crate::testmode::enabled() || here
}

/// Turns it on or off for as long as anyone stays connected, so only while someone is: it
/// isn't left waiting for whoever connects next. (`glass config privacy` is for every session.)
pub fn set_forced(on: bool) -> anyhow::Result<()> {
    anyhow::ensure!(supported(), "privacy mode isn't available on this computer yet");
    crate::policy::load().check("privacy", if on { "on" } else { "off" })?;
    {
        let mut s = STATE.lock().unwrap();
        anyhow::ensure!(s.viewers > 0, "nobody is connected; to have it on for every session: glass config privacy on");
        s.forced = Some(on);
    }
    apply()
}

/// Called when the number of signed-in viewers changes.
pub fn viewers_changed(n: usize) {
    let mut s = STATE.lock().unwrap();
    s.viewers = n;
    if n == 0 {
        s.forced = None;
    }
    drop(s);
    if let Err(e) = apply() {
        tracing::warn!("privacy mode: {e:#}");
        keep_trying();
    }
}

/// How often a change that failed is tried again.
const RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// A change that failed isn't left at that: above all a screen left black with nobody connected.
fn keep_trying() {
    static TRYING: AtomicBool = AtomicBool::new(false);
    if TRYING.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(RETRY);
            if apply().is_ok() {
                break;
            }
        }
        TRYING.store(false, Ordering::Relaxed);
    });
}

/// Whether the screen should be private: only with someone connected, a policy over anyone's
/// choice, a choice over the setting.
fn wanted(viewers: usize, locked: Option<bool>, forced: Option<bool>, setting: impl FnOnce() -> bool) -> bool {
    viewers > 0 && locked.or(forced).unwrap_or_else(setting)
}

pub fn active() -> bool {
    STATE.lock().unwrap().engaged
}

fn apply() -> anyhow::Result<()> {
    let mut s = STATE.lock().unwrap();
    let locked = crate::policy::load().flag("privacy");
    let want = wanted(s.viewers, locked, s.forced, || crate::settings::load().privacy);
    if want == s.engaged {
        return Ok(());
    }
    if crate::testmode::enabled() {
        crate::testmode::record_privacy(want);
    } else {
        crate::platform::privacy::set(want)?;
    }
    s.engaged = want;
    tracing::info!("privacy mode {}", if want { "on" } else { "off" });
    drop(s);
    if let Some(shared) = crate::server::shared() {
        let _ = shared.events.send(crate::control::ServerMsg::Privacy { on: want });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_only_with_someone_connected() {
        assert!(!wanted(0, Some(true), Some(true), || true));
        assert!(wanted(1, None, None, || true));
        assert!(!wanted(1, None, None, || false));
        assert!(wanted(1, None, Some(true), || false), "a choice over the setting");
        assert!(!wanted(1, Some(false), Some(true), || true), "a policy over anyone's choice");
    }

    #[test]
    fn nothing_is_left_waiting_for_the_next_viewer() {
        // (The state is the process's; tests elsewhere don't touch it.)
        if supported() && STATE.lock().unwrap().viewers == 0 {
            assert!(set_forced(true).is_err());
            assert_eq!(STATE.lock().unwrap().forced, None);
        }
    }
}
