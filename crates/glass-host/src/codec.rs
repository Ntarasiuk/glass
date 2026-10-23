//! Which codec the shared encoders use. Every viewer gets the same stream, so HEVC is on
//! only when `glass config hevc on`, the host can encode it, and every connected viewer
//! said it can decode it (WebRTC viewers get RTP H.264, so they keep it H.264).
//!
//! Frames carry FLAG_HEVC, so a viewer switches decoders on the first keyframe in the
//! new codec; the `codec` message is only a heads-up.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy)]
struct Viewer {
    session: u64,
    can_hevc: bool,
    rtp: bool,
}

static VIEWERS: Mutex<Vec<Viewer>> = Mutex::new(Vec::new());
static HEVC: AtomicBool = AtomicBool::new(false);
/// Serializes switches (each restarts every encoder).
static SWITCH: Mutex<()> = Mutex::new(());

/// Held for a session's lifetime.
pub struct Guard(pub u64);

impl Drop for Guard {
    fn drop(&mut self) {
        VIEWERS.lock().unwrap().retain(|v| v.session != self.0);
        reapply();
    }
}

pub fn joined(session: u64, can_hevc: bool, rtp: bool) -> Guard {
    VIEWERS.lock().unwrap().push(Viewer {
        session,
        can_hevc,
        rtp,
    });
    reapply();
    Guard(session)
}

pub fn current() -> &'static str {
    name(HEVC.load(Ordering::Relaxed))
}

fn name(hevc: bool) -> &'static str {
    if hevc { "hevc" } else { "h264" }
}

/// HEVC only if it's turned on, the host can encode it, and every viewer (there's at
/// least one) can decode it and isn't on RTP.
pub fn decide(enabled: bool, supported: bool, viewers: &[(bool, bool)]) -> bool {
    enabled
        && supported
        && !viewers.is_empty()
        && viewers.iter().all(|&(can_hevc, rtp)| can_hevc && !rtp)
}

/// Recomputes the codec after a viewer joins or leaves; switching restarts capture, so
/// it happens off the async runtime.
fn reapply() {
    let Some(shared) = crate::server::shared() else {
        return;
    };
    std::thread::spawn(move || {
        let _switching = SWITCH.lock().unwrap_or_else(|e| e.into_inner());
        let viewers: Vec<(bool, bool)> = VIEWERS
            .lock()
            .unwrap()
            .iter()
            .map(|v| (v.can_hevc, v.rtp))
            .collect();
        // Nobody watching: keep the current codec rather than restart encoders for no one.
        if viewers.is_empty() {
            return;
        }
        let want = decide(
            crate::settings::load().hevc,
            shared.video.supports_hevc(),
            &viewers,
        );
        if want == HEVC.load(Ordering::Relaxed) {
            return;
        }
        let now = shared.video.set_hevc(want);
        HEVC.store(now, Ordering::Relaxed);
        tracing::info!("viewers' codec: {}", name(now));
        let _ = shared.events.send(crate::control::ServerMsg::Codec { codec: name(now) });
        shared.video.request_keyframe(None);
        // Sharp text's 4:2:2 depends on HEVC.
        crate::sharp::recompute();
    });
}

#[cfg(test)]
mod tests {
    use super::decide;

    #[test]
    fn hevc_only_when_everyone_can() {
        let hevc_viewer = (true, false);
        assert!(decide(true, true, &[hevc_viewer, hevc_viewer]));
        assert!(!decide(false, true, &[hevc_viewer]), "setting off");
        assert!(!decide(true, false, &[hevc_viewer]), "encoder can't");
        assert!(!decide(true, true, &[]), "nobody watching");
        assert!(!decide(true, true, &[hevc_viewer, (false, false)]), "one can't decode");
        assert!(!decide(true, true, &[hevc_viewer, (true, true)]), "RTP is H.264");
    }
}
