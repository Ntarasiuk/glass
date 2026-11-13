//! How big the app's text is. Everything is sized from it (in rems), so the whole window
//! grows with it. ⌘+ / ⌘− / ⌘0 (Ctrl on Windows), the View menu, or Settings. Kept in
//! `app.json` in the config folder.

use std::sync::atomic::{AtomicU32, Ordering};

use gpui_kit::component::Theme;
use gpui_kit::*;

/// The sizes to step through, in points; 14 is the default.
pub const STEPS: &[f32] = &[12., 13., 14., 16., 18., 20., 22., 24.];
pub const DEFAULT: f32 = 14.;

static CURRENT: AtomicU32 = AtomicU32::new(0);

fn path() -> std::path::PathBuf {
    crate::config_dir().join("app.json")
}

fn load() -> f32 {
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["textSize"].as_f64())
        .map(|s| s as f32)
        .filter(|s| STEPS.contains(s))
        .unwrap_or(DEFAULT)
}

/// The text size now, in points.
pub fn get() -> f32 {
    match CURRENT.load(Ordering::Relaxed) {
        0 => {
            let s = load();
            CURRENT.store(s.to_bits(), Ordering::Relaxed);
            s
        }
        bits => f32::from_bits(bits),
    }
}

/// How much bigger than the default (for the few things sized in points).
pub fn scale() -> f32 {
    get() / DEFAULT
}

/// Sets it, keeps it, and redraws every window at it.
pub fn set(size: f32, cx: &mut App) {
    let size = if STEPS.contains(&size) { size } else { DEFAULT };
    CURRENT.store(size.to_bits(), Ordering::Relaxed);
    let mut v = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).unwrap_or_else(|| serde_json::json!({}));
    v["textSize"] = serde_json::json!(size);
    let _ = std::fs::create_dir_all(crate::config_dir());
    let _ = std::fs::write(path(), serde_json::to_vec_pretty(&v).unwrap_or_default());
    Theme::update(cx, |t| t.font_size = px(size));
    cx.refresh_windows();
}

/// One step bigger (`up`) or smaller.
pub fn step(up: bool, cx: &mut App) {
    let now = get();
    let i = STEPS.iter().position(|s| *s == now).unwrap_or(2);
    let i = if up { (i + 1).min(STEPS.len() - 1) } else { i.saturating_sub(1) };
    set(STEPS[i], cx);
}

/// A length that was `points` at the default size, grown with the text.
pub fn pt(points: f32) -> Rems {
    rems(points / DEFAULT)
}
