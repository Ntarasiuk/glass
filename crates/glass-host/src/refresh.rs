//! Frame rate: a viewer on a fast screen (120 Hz) asks for more frames; one on an ordinary
//! screen, or one that says nothing, gets 60. Displays are shared by every viewer, so the
//! slowest one sets the pace (frames a screen can't show only cost bandwidth).

use std::collections::HashMap;
use std::sync::Mutex;

/// What a viewer gets unless it asks for more.
pub const DEFAULT: u32 = 60;
/// The most anyone gets.
pub const MAX: u32 = 120;
/// The least a request can lower it to.
const MIN: u32 = 30;

static WANTS: Mutex<Option<HashMap<u64, u32>>> = Mutex::new(None);

/// A session's frame rate (`None`: it ended). Applies the new pace if it changed.
pub fn set(session: u64, fps: Option<u32>) {
    let (before, after) = {
        let mut all = WANTS.lock().unwrap();
        let all = all.get_or_insert_with(Default::default);
        let before = combine(all.values().copied());
        match fps {
            Some(f) => all.insert(session, f),
            None => all.remove(&session),
        };
        (before, combine(all.values().copied()))
    };
    if before != after
        && let Some(shared) = crate::server::shared()
    {
        tracing::info!("frame rate: up to {after} fps for this host's viewers");
        // Reconfiguring capture talks to ScreenCaptureKit, which blocks.
        std::thread::spawn(move || shared.video.set_max_fps(after));
    }
}

/// The slowest viewer's rate (each clamped to [MIN, MAX]); [`DEFAULT`] with nobody.
pub fn combine(wants: impl Iterator<Item = u32>) -> u32 {
    wants.map(|f| f.clamp(MIN, MAX)).min().unwrap_or(DEFAULT)
}

/// Common panel refresh rates.
const PANELS: [u32; 8] = [60, 75, 90, 100, 120, 144, 165, 240];

/// The rate to ask for, from a screen's measured refresh (timing is noisy): the nearest
/// panel rate, at most [`MAX`] (a 144 Hz monitor gets 120).
pub fn snap(hz: f64) -> u32 {
    if !hz.is_finite() || hz <= 0.0 {
        return DEFAULT;
    }
    let nearest = PANELS
        .into_iter()
        .min_by(|a, b| (*a as f64 - hz).abs().total_cmp(&(*b as f64 - hz).abs()))
        .unwrap_or(DEFAULT);
    nearest.clamp(DEFAULT, MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slowest_viewer_sets_the_pace() {
        assert_eq!(combine([].into_iter()), 60);
        assert_eq!(combine([120].into_iter()), 120);
        assert_eq!(combine([120, 60].into_iter()), 60);
        assert_eq!(combine([240].into_iter()), 120, "never more than MAX");
        assert_eq!(combine([5].into_iter()), 30, "never less than MIN");
    }

    #[test]
    fn measured_refresh_snaps_to_a_panel_rate() {
        assert_eq!(snap(59.9), 60);
        assert_eq!(snap(f64::NAN), 60);
        assert_eq!(snap(89.0), 90);
        assert_eq!(snap(114.0), 120, "a jittery 120 Hz measurement");
        assert_eq!(snap(119.6), 120);
        assert_eq!(snap(30.0), 60);
        assert_eq!(snap(144.0), 120);
        assert_eq!(snap(164.0), 120);
    }
}
