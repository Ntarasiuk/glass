//! Privacy screen on macOS: every display's gamma table goes to zero (the panel shows
//! black, but ScreenCaptureKit captures the framebuffer before gamma, so viewers still see
//! the desktop), and an event tap drops local keyboard and mouse events. Glass's own
//! injected events carry a marker (`INJECTED_MARK`) and pass.
//!
//! Gamma changes revert when the process exits, and the tap dies with it, so a crash can't
//! leave the computer dark or locked.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const SUPPORTED: bool = true;

/// Set in kCGEventSourceUserData on every event Glass posts.
pub const INJECTED_MARK: i64 = 0x6c61_7373; // "lass"
/// kCGEventSourceUserData
pub const USER_DATA_FIELD: u32 = 42;

type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CGEventRef = *mut c_void;
type TapCallback = extern "C" fn(*mut c_void, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGGetOnlineDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGSetDisplayTransferByTable(
        display: u32,
        size: u32,
        red: *const f32,
        green: *const f32,
        blue: *const f32,
    ) -> i32;
    fn CGDisplayRestoreColorSyncSettings();
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: *const c_void;
    fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: CFMachPortRef, order: isize) -> *mut c_void;
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: *mut c_void, mode: *const c_void);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRelease(cf: *const c_void);
}

const SESSION_TAP: u32 = 1; // kCGSessionEventTap: sees hardware and posted events
const HEAD_INSERT: u32 = 0;
const DEFAULT_TAP: u32 = 0; // active filter: may drop events
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

/// Keyboard, flags, every mouse button, moves, drags and scrolls.
const EVENT_MASK: u64 = {
    let types = [1u32, 2, 3, 4, 5, 6, 7, 10, 11, 12, 22, 25, 26, 27];
    let mut mask = 0u64;
    let mut i = 0;
    while i < types.len() {
        mask |= 1u64 << types[i];
        i += 1;
    }
    mask
};

struct Tap {
    port: usize,
    run_loop: usize,
}

static ENGAGED: AtomicBool = AtomicBool::new(false);
static TAP: Mutex<Option<Tap>> = Mutex::new(None);

pub fn set(on: bool) -> anyhow::Result<()> {
    if on {
        engage()
    } else {
        release();
        Ok(())
    }
}

fn engage() -> anyhow::Result<()> {
    if ENGAGED.load(Ordering::SeqCst) {
        return Ok(());
    }
    anyhow::ensure!(
        crate::input::has_accessibility(),
        "privacy mode needs the Accessibility permission (to block local input)"
    );
    // The tap first: without it the person at the computer could still type blind,
    // and a dark screen alone isn't private.
    start_tap()?;
    ENGAGED.store(true, Ordering::SeqCst);
    install_panic_restore();
    blank_all();
    // Displays come and go (and macOS resets gamma on some changes): keep them dark.
    std::thread::Builder::new()
        .name("glass-privacy".into())
        .spawn(|| {
            while ENGAGED.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(1));
                if ENGAGED.load(Ordering::SeqCst) {
                    blank_all();
                }
            }
        })?;
    Ok(())
}

fn release() {
    let was = ENGAGED.swap(false, Ordering::SeqCst);
    stop_tap();
    if was {
        unsafe { CGDisplayRestoreColorSyncSettings() };
    }
}

fn online_displays() -> Vec<u32> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    if unsafe { CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
        return Vec::new();
    }
    ids[..count as usize].to_vec()
}

fn blank_all() {
    let zero = [0f32; 2];
    for d in online_displays() {
        let err = unsafe { CGSetDisplayTransferByTable(d, 2, zero.as_ptr(), zero.as_ptr(), zero.as_ptr()) };
        if err != 0 {
            tracing::warn!("privacy: couldn't blank display {d} (error {err})");
        }
    }
}

extern "C" fn on_event(_proxy: *mut c_void, ty: u32, event: CGEventRef, _info: *mut c_void) -> CGEventRef {
    if ty == TAP_DISABLED_BY_TIMEOUT || ty == TAP_DISABLED_BY_USER_INPUT {
        // macOS turns a slow tap off; turn it straight back on.
        if let Some(t) = TAP.lock().ok().as_ref().and_then(|t| t.as_ref().map(|t| t.port)) {
            unsafe { CGEventTapEnable(t as CFMachPortRef, true) };
        }
        return event;
    }
    if !ENGAGED.load(Ordering::Relaxed) {
        return event;
    }
    if unsafe { CGEventGetIntegerValueField(event, USER_DATA_FIELD) } == INJECTED_MARK {
        event
    } else {
        std::ptr::null_mut()
    }
}

fn start_tap() -> anyhow::Result<()> {
    if TAP.lock().unwrap().is_some() {
        return Ok(());
    }
    let (tx, rx) = std::sync::mpsc::channel::<Result<Tap, String>>();
    std::thread::Builder::new()
        .name("glass-privacy-tap".into())
        .spawn(move || unsafe {
            let port = CGEventTapCreate(
                SESSION_TAP,
                HEAD_INSERT,
                DEFAULT_TAP,
                EVENT_MASK,
                on_event,
                std::ptr::null_mut(),
            );
            if port.is_null() {
                let _ = tx.send(Err("couldn't block local input (event tap refused; check Accessibility)".into()));
                return;
            }
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            let _ = tx.send(Ok(Tap {
                port: port as usize,
                run_loop: rl as usize,
            }));
            CFRunLoopRun();
            CFRelease(source);
            CFRelease(port);
        })?;
    let tap = rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| anyhow::anyhow!("couldn't start blocking local input"))?
        .map_err(|e| anyhow::anyhow!(e))?;
    *TAP.lock().unwrap() = Some(tap);
    Ok(())
}

fn stop_tap() {
    if let Some(t) = TAP.lock().unwrap().take() {
        unsafe {
            CGEventTapEnable(t.port as CFMachPortRef, false);
            CFMachPortInvalidate(t.port as CFMachPortRef);
            CFRunLoopStop(t.run_loop as CFRunLoopRef);
        }
    }
}

/// A panic elsewhere shouldn't leave the screen dark until the process exits.
fn install_panic_restore() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if ENGAGED.load(Ordering::SeqCst) {
                release();
            }
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn mask_covers_keys_mouse_and_scroll() {
        let m = super::EVENT_MASK;
        for t in [1, 2, 5, 10, 11, 12, 22] {
            assert!(m & (1 << t) != 0, "event type {t}");
        }
        assert_eq!(m & (1 << 0), 0, "null events aren't blocked");
    }
}
