//! Keyboard capture: a session event tap that takes every key while the viewer has focus,
//! before macOS acts on it, so ⌘Tab, ⌘Space, ⌘Q, ⌃←/→ and the rest reach the remote
//! computer as they would a keyboard plugged into it. Needs Accessibility permission.
//!
//! ⌃⌥⎋ (Control-Option-Escape) always lets go of the keyboard.

use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use glass_proto::InputEvent;
use objc2_app_kit::NSRunningApplication;
use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};

use super::keys::*;

/// The key that, with Control and Option held, lets go of the keyboard.
pub const RELEASE_KEY: u16 = ESCAPE;
pub const RELEASE_HINT: &str = "⌃⌥⎋";

// Device-dependent modifier bits (IOLLEvent.h's NX_DEVICE*KEYMASK): which side is down.
const LCTL: u64 = 0x0001;
const LSHIFT: u64 = 0x0002;
const RSHIFT: u64 = 0x0004;
const LCMD: u64 = 0x0008;
const RCMD: u64 = 0x0010;
const LALT: u64 = 0x0020;
const RALT: u64 = 0x0040;
const RCTL: u64 = 0x2000;

struct Ctx {
    send: Box<dyn Fn(InputEvent)>,
    /// ⌃⌥⎋ was pressed: the owner should drop the capture (not from inside the tap).
    release: Box<dyn Fn()>,
    port: *const CFMachPort,
    /// ⌘ and Control trade places (for a Windows or Linux computer, where Ctrl does what ⌘ does).
    swap: Arc<AtomicBool>,
    /// The mouse is captured too: its motion goes as relative moves (games, 3D apps).
    relative: Arc<AtomicBool>,
    /// Keys the remote computer has down, let go when capture ends.
    down: HashSet<u16>,
}

pub struct KeyCapture {
    port: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
    ctx: *mut Ctx,
}

/// Whether this app may tap the keyboard (Accessibility).
pub fn allowed() -> bool {
    crate::input::has_accessibility()
}

/// Asks for Accessibility (the system prompt, then Settings).
pub fn ask() {
    crate::permissions::request_missing();
}

pub use super::keys::swapped;


/// Whether a modifier key is down after a FlagsChanged. The left/right bits say exactly;
/// events without them (synthetic ones, some key remappers) only have the shared flag.
fn modifier_down(keycode: u16, bit: u64, flags: u64) -> bool {
    let (sides, shared) = match keycode {
        COMMAND | RIGHT_COMMAND => (LCMD | RCMD, CGEventFlags::MaskCommand.0),
        SHIFT | RIGHT_SHIFT => (LSHIFT | RSHIFT, CGEventFlags::MaskShift.0),
        OPTION | RIGHT_OPTION => (LALT | RALT, CGEventFlags::MaskAlternate.0),
        CONTROL | RIGHT_CONTROL => (LCTL | RCTL, CGEventFlags::MaskControl.0),
        _ => return flags & bit != 0,
    };
    flags & bit != 0 || (flags & sides == 0 && flags & shared != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_read_with_or_without_side_bits() {
        let cmd = CGEventFlags::MaskCommand.0;
        assert!(modifier_down(COMMAND, LCMD, cmd | LCMD));
        // Right ⌘ held, left let go: left is up.
        assert!(!modifier_down(COMMAND, LCMD, cmd | RCMD));
        // No side bits at all: the shared flag decides.
        assert!(modifier_down(COMMAND, LCMD, cmd));
        assert!(!modifier_down(COMMAND, LCMD, 0));
        assert!(modifier_down(CAPS_LOCK, CGEventFlags::MaskAlphaShift.0, CGEventFlags::MaskAlphaShift.0));
        assert_eq!(swapped(COMMAND), CONTROL);
        assert_eq!(swapped(swapped(RIGHT_COMMAND)), RIGHT_COMMAND);
    }
}

fn modifier_bit(keycode: u16) -> Option<u64> {
    Some(match keycode {
        COMMAND => LCMD,
        RIGHT_COMMAND => RCMD,
        SHIFT => LSHIFT,
        RIGHT_SHIFT => RSHIFT,
        OPTION => LALT,
        RIGHT_OPTION => RALT,
        CONTROL => LCTL,
        RIGHT_CONTROL => RCTL,
        CAPS_LOCK => CGEventFlags::MaskAlphaShift.0,
        FUNCTION => CGEventFlags::MaskSecondaryFn.0,
        _ => return None,
    })
}

unsafe extern "C-unwind" fn on_event(_proxy: CGEventTapProxy, ty: CGEventType, event: NonNull<CGEvent>, user: *mut c_void) -> *mut CGEvent {
    let ctx = unsafe { &mut *(user as *mut Ctx) };
    let pass = event.as_ptr();
    if ty == CGEventType::TapDisabledByTimeout || ty == CGEventType::TapDisabledByUserInput {
        CGEvent::tap_enable(unsafe { &*ctx.port }, true);
        return pass;
    }
    // Only while this app is in front (the tap is system-wide).
    if !NSRunningApplication::currentApplication().isActive() {
        return pass;
    }
    let ev = unsafe { event.as_ref() };
    let local = CGEvent::integer_value_field(Some(ev), CGEventField::KeyboardEventKeycode) as u16;
    let keycode = if ctx.swap.load(Ordering::Relaxed) { swapped(local) } else { local };
    let flags = CGEvent::flags(Some(ev));
    match ty {
        CGEventType::MouseMoved | CGEventType::LeftMouseDragged | CGEventType::RightMouseDragged | CGEventType::OtherMouseDragged => {
            if ctx.relative.load(Ordering::Relaxed) {
                let dx = CGEvent::integer_value_field(Some(ev), CGEventField::MouseEventDeltaX) as f32;
                let dy = CGEvent::integer_value_field(Some(ev), CGEventField::MouseEventDeltaY) as f32;
                if dx != 0.0 || dy != 0.0 {
                    (ctx.send)(InputEvent::MouseMoveBy { dx, dy });
                }
            }
            // Clicks and the window still see it.
            return pass;
        }
        CGEventType::KeyDown => {
            let release = flags.contains(CGEventFlags::MaskControl) && flags.contains(CGEventFlags::MaskAlternate);
            if local == RELEASE_KEY && release {
                (ctx.release)();
                return ptr::null_mut();
            }
            ctx.down.insert(keycode);
            (ctx.send)(InputEvent::KeyDown { keycode });
        }
        CGEventType::KeyUp => {
            ctx.down.remove(&keycode);
            (ctx.send)(InputEvent::KeyUp { keycode });
        }
        CGEventType::FlagsChanged => {
            let Some(bit) = modifier_bit(local) else { return pass };
            if modifier_down(local, bit, flags.0) {
                ctx.down.insert(keycode);
                (ctx.send)(InputEvent::KeyDown { keycode });
            } else {
                ctx.down.remove(&keycode);
                (ctx.send)(InputEvent::KeyUp { keycode });
            }
        }
        _ => return pass,
    }
    // Taken: macOS (and this app) never see it.
    ptr::null_mut()
}

impl KeyCapture {
    /// Starts taking the keyboard, on this (the main) thread's run loop. `None` without
    /// Accessibility permission.
    pub fn start(send: impl Fn(InputEvent) + 'static, release: impl Fn() + 'static, swap: Arc<AtomicBool>, relative: Arc<AtomicBool>) -> Option<Self> {
        if !allowed() {
            return None;
        }
        let ctx = Box::into_raw(Box::new(Ctx { send: Box::new(send), release: Box::new(release), port: ptr::null(), swap, relative, down: HashSet::new() }));
        let mask: CGEventMask = [
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
            CGEventType::MouseMoved,
            CGEventType::LeftMouseDragged,
            CGEventType::RightMouseDragged,
            CGEventType::OtherMouseDragged,
        ]
        .iter()
        .fold(0, |m, t| m | (1 << t.0));
        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                mask,
                Some(on_event),
                ctx as *mut c_void,
            )
        };
        let Some(port) = port else {
            drop(unsafe { Box::from_raw(ctx) });
            tracing::warn!("viewer: couldn't tap the keyboard");
            return None;
        };
        unsafe { (*ctx).port = &*port };
        let source = CFMachPort::new_run_loop_source(None, Some(&port), 0)?;
        CFRunLoop::current()?.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        CGEvent::tap_enable(&port, true);
        Some(Self { port, source, ctx })
    }
}

impl Drop for KeyCapture {
    fn drop(&mut self) {
        CGEvent::tap_enable(&self.port, false);
        if let Some(rl) = CFRunLoop::current() {
            rl.remove_source(Some(&self.source), unsafe { kCFRunLoopCommonModes });
        }
        CFMachPort::invalidate(&self.port);
        let ctx = unsafe { Box::from_raw(self.ctx) };
        // Nothing stays held down over there.
        for keycode in ctx.down.iter().copied() {
            (ctx.send)(InputEvent::KeyUp { keycode });
        }
    }
}

/// Locks the pointer in place and hides it (relative mouse), or lets it go.
pub fn lock_pointer(on: bool) {
    use objc2_app_kit::NSCursor;
    let _ = objc2_core_graphics::CGAssociateMouseAndMouseCursorPosition(!on);
    if on {
        NSCursor::hide();
    } else {
        NSCursor::unhide();
    }
}
