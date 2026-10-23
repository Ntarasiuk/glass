//! Injects client input as system-level CGEvents. Requires Accessibility permission.

use glass_proto::InputEvent;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGEvent, CGEventField, CGEventFlags, CGEventSource,
    CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton, CGScrollEventUnit,
};

const KVK_V: u16 = 0x09;
// kVK_* keycodes for modifiers.
const KVK_COMMAND: u16 = 0x37;
const KVK_SHIFT: u16 = 0x38;
const KVK_OPTION: u16 = 0x3A;
const KVK_CONTROL: u16 = 0x3B;
const KVK_RIGHT_COMMAND: u16 = 0x36;
const KVK_RIGHT_SHIFT: u16 = 0x3C;
const KVK_RIGHT_OPTION: u16 = 0x3D;
const KVK_RIGHT_CONTROL: u16 = 0x3E;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Without Accessibility permission, posted events are silently dropped.
pub fn has_accessibility() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// kCGMouseEventClickState
const CLICK_STATE: CGEventField = CGEventField(1);

pub struct Injector {
    source: Option<CFRetained<CGEventSource>>,
    /// Global bounds of each streamed display, indexed by stream number.
    bounds: Vec<CGRect>,
    pos: CGPoint,
    buttons: [bool; 3],
    flags: CGEventFlags,
    last_click: Option<(u8, std::time::Instant, CGPoint)>,
    click_count: i64,
}

// CGEventSource and CGEventPost are safe to use from any thread; access is serialized by a Mutex.
unsafe impl Send for Injector {}

impl Injector {
    pub fn new(display: CGDirectDisplayID) -> Self {
        let bounds = CGDisplayBounds(display);
        Self {
            source: CGEventSource::new(CGEventSourceStateID::HIDSystemState),
            bounds: vec![bounds],
            pos: bounds.origin,
            buttons: [false; 3],
            flags: CGEventFlags(0),
            last_click: None,
            click_count: 1,
        }
    }

    /// Retargets absolute mouse coordinates after the streamed displays change.
    pub fn set_displays(&mut self, displays: &[CGDirectDisplayID]) {
        self.release_all();
        self.bounds = displays.iter().map(|&d| CGDisplayBounds(d)).collect();
    }

    pub fn handle(&mut self, ev: InputEvent) {
        match ev {
            InputEvent::MouseMove { x, y, stream } => {
                let Some(b) = self.bounds.get(stream as usize).or(self.bounds.first()) else {
                    return;
                };
                self.pos = CGPoint {
                    x: b.origin.x + x.clamp(0.0, 1.0) as f64 * b.size.width,
                    y: b.origin.y + y.clamp(0.0, 1.0) as f64 * b.size.height,
                };
                let (ty, button) = match self.buttons {
                    [true, _, _] => (CGEventType::LeftMouseDragged, CGMouseButton::Left),
                    [_, _, true] => (CGEventType::RightMouseDragged, CGMouseButton::Right),
                    [_, true, _] => (CGEventType::OtherMouseDragged, CGMouseButton::Center),
                    _ => (CGEventType::MouseMoved, CGMouseButton::Left),
                };
                self.mouse(ty, button);
            }
            InputEvent::MouseDown { button } | InputEvent::MouseUp { button } => {
                let down = matches!(ev, InputEvent::MouseDown { .. });
                let Some((ty, cg_button)) = mouse_button(button, down) else {
                    return;
                };
                self.buttons[button as usize] = down;
                if down {
                    self.click_count = self.next_click_count(button);
                }
                self.mouse(ty, cg_button);
            }
            InputEvent::Scroll { dx, dy } => {
                let ev = CGEvent::new_scroll_wheel_event2(
                    self.source.as_deref(),
                    CGScrollEventUnit::Pixel,
                    2,
                    -dy.round() as i32,
                    -dx.round() as i32,
                    0,
                );
                self.post(ev);
            }
            // Routed to crate::gamepad by the server, never here.
            InputEvent::Gamepad { .. } => {}
            InputEvent::KeyDown { keycode } | InputEvent::KeyUp { keycode } => {
                let down = matches!(ev, InputEvent::KeyDown { .. });
                if let Some(flag) = modifier_flag(keycode) {
                    self.flags = if down {
                        CGEventFlags(self.flags.0 | flag.0)
                    } else {
                        CGEventFlags(self.flags.0 & !flag.0)
                    };
                }
                let ev = CGEvent::new_keyboard_event(self.source.as_deref(), keycode, down);
                self.post(ev);
            }
        }
    }

    /// Pastes the clipboard: Cmd+V, using the Cmd the client is already holding.
    pub fn paste(&mut self) {
        self.tap_key(KVK_V);
    }

    /// Presses and releases a key with the currently held modifiers.
    pub fn tap_key(&mut self, keycode: u16) {
        self.handle(InputEvent::KeyDown { keycode });
        self.handle(InputEvent::KeyUp { keycode });
    }

    /// Releases anything still held, e.g. when a client disconnects mid-drag.
    pub fn release_all(&mut self) {
        for button in 0..3u8 {
            if self.buttons[button as usize] {
                self.handle(InputEvent::MouseUp { button });
            }
        }
        for keycode in [
            KVK_COMMAND,
            KVK_SHIFT,
            KVK_OPTION,
            KVK_CONTROL,
            KVK_RIGHT_COMMAND,
            KVK_RIGHT_SHIFT,
            KVK_RIGHT_OPTION,
            KVK_RIGHT_CONTROL,
        ] {
            if modifier_flag(keycode).is_some_and(|f| self.flags.0 & f.0 != 0) {
                self.handle(InputEvent::KeyUp { keycode });
            }
        }
    }

    fn next_click_count(&mut self, button: u8) -> i64 {
        let now = std::time::Instant::now();
        let count = match self.last_click {
            Some((b, t, p))
                if b == button
                    && now.duration_since(t).as_millis() < 400
                    && (p.x - self.pos.x).abs() < 5.0
                    && (p.y - self.pos.y).abs() < 5.0 =>
            {
                self.click_count + 1
            }
            _ => 1,
        };
        self.last_click = Some((button, now, self.pos));
        count
    }

    fn mouse(&self, ty: CGEventType, button: CGMouseButton) {
        let ev = CGEvent::new_mouse_event(self.source.as_deref(), ty, self.pos, button);
        CGEvent::set_integer_value_field(ev.as_deref(), CLICK_STATE, self.click_count);
        self.post(ev);
    }

    fn post(&self, ev: Option<CFRetained<CGEvent>>) {
        if let Some(ev) = ev {
            CGEvent::set_flags(Some(&ev), self.flags);
            // Lets privacy mode's event tap tell these from the local keyboard and mouse.
            CGEvent::set_integer_value_field(
                Some(&ev),
                CGEventField(super::privacy::USER_DATA_FIELD),
                super::privacy::INJECTED_MARK,
            );
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
        }
    }
}

/// DOM button numbering: 0 left, 1 middle, 2 right.
fn mouse_button(button: u8, down: bool) -> Option<(CGEventType, CGMouseButton)> {
    Some(match (button, down) {
        (0, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
        (0, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
        (2, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
        (2, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
        (1, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
        (1, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        _ => return None,
    })
}

fn modifier_flag(keycode: u16) -> Option<CGEventFlags> {
    Some(match keycode {
        KVK_COMMAND | KVK_RIGHT_COMMAND => CGEventFlags::MaskCommand,
        KVK_SHIFT | KVK_RIGHT_SHIFT => CGEventFlags::MaskShift,
        KVK_OPTION | KVK_RIGHT_OPTION => CGEventFlags::MaskAlternate,
        KVK_CONTROL | KVK_RIGHT_CONTROL => CGEventFlags::MaskControl,
        _ => return None,
    })
}

impl crate::media::InputSink for Injector {
    fn handle(&mut self, ev: glass_proto::InputEvent) {
        Injector::handle(self, ev)
    }
    fn paste(&mut self) {
        Injector::paste(self)
    }
    fn release_all(&mut self) {
        Injector::release_all(self)
    }
    fn set_displays(&mut self, displays: &[u32]) {
        Injector::set_displays(self, displays)
    }
}
