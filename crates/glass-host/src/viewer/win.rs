//! The native viewer's Windows side: decoding (OpenH264, into BGRA images GPUI draws),
//! keyboard capture through a low-level hook (the Windows key, Alt+Tab, Win+D… reach the
//! other computer; Ctrl+Alt+Del and Win+L stay with Windows, as they must), the captured
//! mouse, and the clipboard. The pointer is drawn into the video by the host here.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use glass_proto::InputEvent;
use openh264::formats::YUVSource;
use serde_json::Value;
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCursorPos, GetForegroundWindow, GetWindowThreadProcessId, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, MSLLHOOKSTRUCT, SetWindowsHookExW,
    ShowCursor, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_MOUSEMOVE, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use super::keys::*;

pub const RELEASE_HINT: &str = "Ctrl+Alt+Esc";
pub use super::keys::swapped;

/// Capturing needs no permission on Windows.
pub fn allowed() -> bool {
    true
}

pub fn ask() {}

// ---- Pictures ----

/// A decoded frame, as GPUI draws it.
#[derive(Clone)]
pub struct Picture(pub Arc<gpui_kit::RenderImage>);

pub struct Decoder {
    dec: openh264::decoder::Decoder,
    rgba: Vec<u8>,
}

// Used from the session task only.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Result<Self> {
        Ok(Self { dec: openh264::decoder::Decoder::new().context("no H.264 decoder")?, rgba: Vec::new() })
    }

    /// One Annex B frame; `None` while the decoder is still filling.
    pub fn decode(&mut self, frame: &[u8]) -> Result<Option<Picture>> {
        let Some(yuv) = self.dec.decode(frame)? else { return Ok(None) };
        let (w, h) = yuv.dimensions();
        self.rgba.resize(w * h * 4, 0);
        yuv.write_rgba8(&mut self.rgba);
        // GPUI's images are BGRA.
        for px in self.rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let img = image::RgbaImage::from_raw(w as u32, h as u32, self.rgba.clone()).context("bad frame size")?;
        Ok(Some(Picture(Arc::new(gpui_kit::RenderImage::new(vec![image::Frame::new(img)])))))
    }
}

// ---- The pointer: drawn by the host, so nothing to do here ----

#[derive(Default)]
pub struct Cursors;

impl Cursors {
    pub fn add(&mut self, _: &Value) {}
    pub fn apply(&mut self, _: Option<glass_proto::CursorPos>) {}
    pub fn arrow(&mut self) {}
    pub fn forget(&mut self) {}
}

// ---- Clipboard ----

pub fn clipboard() -> (String, Option<String>, bool) {
    crate::platform::clipboard::read()
}

pub fn clipboard_count() -> isize {
    crate::platform::clipboard::sequence()
}

// ---- Keyboard and mouse capture ----

/// A PC key (scan code, extended) as the Mac's keycode, from the host's own table.
fn kvk_for_scan(scan: u16, extended: bool) -> Option<u16> {
    static MAP: OnceLock<HashMap<(u16, bool), u16>> = OnceLock::new();
    MAP.get_or_init(|| {
        (0u16..=0x7e)
            .filter_map(|k| match crate::platform::keymap::to_key(k) {
                Some(crate::platform::keymap::Key::Scan(s, e)) => Some(((s, e), k)),
                _ => None,
            })
            .collect()
    })
    .get(&(scan, extended))
    .copied()
}

fn kvk_for(vk: u32, scan: u16, extended: bool) -> Option<u16> {
    Some(match vk {
        0x5B => COMMAND,
        0x5C => RIGHT_COMMAND,
        0xA0 => SHIFT,
        0xA1 => RIGHT_SHIFT,
        0xA2 => CONTROL,
        0xA3 => RIGHT_CONTROL,
        0xA4 => OPTION,
        0xA5 => RIGHT_OPTION,
        0x14 => CAPS_LOCK,
        _ => return kvk_for_scan(scan, extended),
    })
}

struct Hooked {
    send: Box<dyn Fn(InputEvent)>,
    release: Box<dyn Fn()>,
    swap: Arc<AtomicBool>,
    relative: Arc<AtomicBool>,
    down: HashSet<u16>,
    keyboard: HHOOK,
    mouse: HHOOK,
}

thread_local! {
    /// The capture in force (hooks run on the thread that set them: the window's).
    static HOOK: RefCell<Option<Hooked>> = const { RefCell::new(None) };
}

fn ours_in_front() -> bool {
    let mut pid = 0u32;
    // SAFETY: plain queries.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)) };
    pid == unsafe { GetCurrentProcessId() }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 || !ours_in_front() {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    // SAFETY: for WH_KEYBOARD_LL, lparam is a KBDLLHOOKSTRUCT.
    let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    // Typed by a program (Glass's own host, when this PC is the one being viewed): taking it
    // would send it round again.
    if k.flags.contains(LLKHF_INJECTED) {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    let up = matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP);
    let Some(local) = kvk_for(k.vkCode, k.scanCode as u16, k.flags.contains(LLKHF_EXTENDED)) else {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    };
    let taken = HOOK.with(|h| {
        let mut h = h.borrow_mut();
        let Some(h) = h.as_mut() else { return false };
        // Ctrl+Alt+Esc lets go.
        let held = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0;
        if down && local == ESCAPE && held(VK_CONTROL.0) && held(VK_MENU.0) {
            (h.release)();
            return true;
        }
        let keycode = if h.swap.load(Ordering::Relaxed) { swapped(local) } else { local };
        if down {
            h.down.insert(keycode);
            (h.send)(InputEvent::KeyDown { keycode });
        } else if up {
            h.down.remove(&keycode);
            (h.send)(InputEvent::KeyUp { keycode });
        }
        down || up
    });
    if taken { LRESULT(1) } else { unsafe { CallNextHookEx(None, code, wparam, lparam) } }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_MOUSEMOVE && ours_in_front() {
        // SAFETY: for WH_MOUSE_LL, lparam is a MSLLHOOKSTRUCT.
        let m = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if m.flags & LLMHF_INJECTED != 0 {
            return unsafe { CallNextHookEx(None, code, wparam, lparam) };
        }
        let taken = HOOK.with(|h| {
            let h = h.borrow();
            let Some(h) = h.as_ref().filter(|h| h.relative.load(Ordering::Relaxed)) else { return false };
            let mut at = POINT::default();
            // SAFETY: a plain query into a local.
            let _ = unsafe { GetCursorPos(&mut at) };
            let (dx, dy) = ((m.pt.x - at.x) as f32, (m.pt.y - at.y) as f32);
            if dx != 0.0 || dy != 0.0 {
                (h.send)(InputEvent::MouseMoveBy { dx, dy });
            }
            // The pointer stays where it is: only the motion goes.
            true
        });
        if taken {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

pub struct KeyCapture;

impl KeyCapture {
    pub fn start(send: impl Fn(InputEvent) + 'static, release: impl Fn() + 'static, swap: Arc<AtomicBool>, relative: Arc<AtomicBool>) -> Option<Self> {
        // SAFETY: hooks on this thread's message loop, removed in Drop.
        let keyboard = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), None, 0) }.ok()?;
        let mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) }.unwrap_or_default();
        HOOK.with(|h| *h.borrow_mut() = Some(Hooked { send: Box::new(send), release: Box::new(release), swap, relative, down: HashSet::new(), keyboard, mouse }));
        Some(Self)
    }
}

impl Drop for KeyCapture {
    fn drop(&mut self) {
        if let Some(h) = HOOK.with(|h| h.borrow_mut().take()) {
            // SAFETY: the hooks set in start.
            unsafe {
                let _ = UnhookWindowsHookEx(h.keyboard);
                if !h.mouse.is_invalid() {
                    let _ = UnhookWindowsHookEx(h.mouse);
                }
            }
            for keycode in h.down {
                (h.send)(InputEvent::KeyUp { keycode });
            }
        }
    }
}

/// Hides the pointer while the mouse is captured (the hook keeps it in place).
pub fn lock_pointer(on: bool) {
    // SAFETY: balanced: called only when the state changes.
    unsafe { ShowCursor(!on) };
}
