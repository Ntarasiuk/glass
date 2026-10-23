//! Sharp text: a viewer reading code or documents asks for the picture to favour detail
//! over motion. While any viewer has it on, that viewer's displays are encoded at native
//! size, the encoders follow a stricter text policy (fewer frames, each finer: see
//! `rate::policy`), and, when the host encodes HEVC and every viewer can decode it, colour
//! is kept at 4:2:2 instead of 4:2:0 so coloured text and thin lines don't smear.
//!
//! Every viewer shares the encoders, so this changes the picture for all of them; view-only
//! guests can't turn it on.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy)]
struct Viewer {
    on: bool,
    /// Said it can decode HEVC 4:2:2 (codec "hevc-rext" in hello).
    can_rext: bool,
}

static VIEWERS: Mutex<Option<HashMap<u64, Viewer>>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static REXT: AtomicBool = AtomicBool::new(false);

/// What the encoders should do: (stricter text policy, HEVC 4:2:2).
pub fn decide(viewers: &[(bool, bool)], hevc_on: bool, encoder_rext: bool) -> (bool, bool) {
    let sharp = viewers.iter().any(|&(on, _)| on);
    let rext = sharp && hevc_on && encoder_rext && viewers.iter().all(|&(_, can)| can);
    (sharp, rext)
}

/// The text policy is in force.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// "4:2:2" when colour is kept, "sharp" when only the policy applies, "off".
pub fn mode() -> &'static str {
    match (active(), REXT.load(Ordering::Relaxed)) {
        (true, true) => "4:2:2",
        (true, false) => "sharp",
        _ => "off",
    }
}

/// Held for a session's lifetime.
pub struct Guard(u64);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(m) = VIEWERS.lock().unwrap().as_mut() {
            m.remove(&self.0);
        }
        // Even a viewer without it leaving can change things: it may have been the one
        // that couldn't decode 4:2:2.
        recompute();
    }
}

pub fn joined(session: u64, can_rext: bool) -> Guard {
    VIEWERS
        .lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .insert(session, Viewer { on: false, can_rext });
    recompute();
    Guard(session)
}

pub fn set(session: u64, on: bool) {
    if let Some(v) = VIEWERS.lock().unwrap().as_mut().and_then(|m| m.get_mut(&session)) {
        v.on = on;
    }
    recompute();
}

/// Re-decides after a change (viewers, their choice, or the codec).
pub fn recompute() {
    let viewers: Vec<(bool, bool)> = VIEWERS
        .lock()
        .unwrap()
        .as_ref()
        .map(|m| m.values().map(|v| (v.on, v.can_rext)).collect())
        .unwrap_or_default();
    let Some(shared) = crate::server::shared() else {
        return;
    };
    let hevc_on = crate::codec::current() == "hevc";
    let (sharp, rext) = decide(&viewers, hevc_on, shared.video.supports_rext());
    let changed = ACTIVE.swap(sharp, Ordering::Relaxed) != sharp
        | (REXT.load(Ordering::Relaxed) != rext);
    if !changed {
        return;
    }
    if crate::testmode::enabled() {
        crate::testmode::record("sharp", serde_json::json!({ "on": sharp, "rext": rext }));
    }
    tracing::info!(
        "sharp text {}",
        if rext {
            "on (HEVC 4:2:2)"
        } else if sharp {
            "on"
        } else {
            "off"
        }
    );
    // Restarting encoders blocks on ScreenCaptureKit; the policy change is immediate.
    std::thread::spawn(move || {
        let used = shared.video.set_rext(rext);
        REXT.store(used, Ordering::Relaxed);
        crate::server::reapply_policy();
        let _ = shared.events.send(crate::control::ServerMsg::Sharp { mode: mode() });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_viewer_turns_it_on_and_everyone_must_decode_422() {
        let (on_rext, on_plain, off) = ((true, true), (true, false), (false, true));
        assert_eq!(decide(&[], true, true), (false, false));
        assert_eq!(decide(&[off, off], true, true), (false, false));
        assert_eq!(decide(&[on_rext, off], true, true), (true, true));
        assert_eq!(decide(&[on_rext, (false, false)], true, true), (true, false), "one can't decode it");
        assert_eq!(decide(&[on_plain], true, true), (true, false));
        assert_eq!(decide(&[on_rext], false, true), (true, false), "H.264 has no 4:2:2 here");
        assert_eq!(decide(&[on_rext], true, false), (true, false), "encoder can't");
    }
}
