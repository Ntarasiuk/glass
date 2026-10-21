//! Screen Recording and Accessibility permission checks and prompts.

use objc2_core_foundation::{CFBoolean, CFDictionary, CFString, CFType};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

pub const SCREEN_RECORDING_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
pub const ACCESSIBILITY_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: Option<&CFDictionary>) -> bool;
    static kAXTrustedCheckOptionPrompt: &'static CFString;
}

/// May stay false until the process restarts, even after the user grants it.
pub fn has_screen_recording() -> bool {
    CGPreflightScreenCaptureAccess()
}

/// Shows the system prompts for whatever is missing (each at most once per launch).
pub fn request_missing() {
    if !has_screen_recording() {
        CGRequestScreenCaptureAccess();
    }
    if !crate::input::has_accessibility() {
        let keys = unsafe { [kAXTrustedCheckOptionPrompt] };
        let options = CFDictionary::<CFString, CFType>::from_slices(&keys, &[CFBoolean::new(true)]);
        unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) };
    }
}

pub fn open_pane(pane: &str) {
    let _ = std::process::Command::new("open").arg(pane).status();
}
