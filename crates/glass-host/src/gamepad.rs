//! Game controllers from the viewer (browser Gamepad API, standard mapping), as virtual
//! controllers on the host.
//!
//! Linux: one uinput device per pad, shaped like an Xbox 360 controller (same ids and
//! codes as the xpad driver), so games and SDL pick it up with no setup. macOS has no
//! public way to create a virtual HID controller (that needs a DriverKit extension with a
//! special entitlement), so controllers aren't offered there.

/// Linux input codes, kept here (not from evdev) so the mapping is testable everywhere.
pub mod code {
    pub const BTN_SOUTH: u16 = 0x130;
    pub const BTN_EAST: u16 = 0x131;
    /// xpad's BTN_X / BTN_Y (0x133 / 0x134): the left and top face buttons.
    pub const BTN_X: u16 = 0x133;
    pub const BTN_Y: u16 = 0x134;
    pub const BTN_TL: u16 = 0x136;
    pub const BTN_TR: u16 = 0x137;
    pub const BTN_SELECT: u16 = 0x13a;
    pub const BTN_START: u16 = 0x13b;
    pub const BTN_MODE: u16 = 0x13c;
    pub const BTN_THUMBL: u16 = 0x13d;
    pub const BTN_THUMBR: u16 = 0x13e;
    pub const ABS_X: u16 = 0x00;
    pub const ABS_Y: u16 = 0x01;
    pub const ABS_Z: u16 = 0x02;
    pub const ABS_RX: u16 = 0x03;
    pub const ABS_RY: u16 = 0x04;
    pub const ABS_RZ: u16 = 0x05;
    pub const ABS_HAT0X: u16 = 0x10;
    pub const ABS_HAT0Y: u16 = 0x11;
}

/// Where a standard-mapping button goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Key(u16),
    /// An analog trigger: an absolute axis, 0..=TRIGGER_MAX.
    Trigger(u16),
    /// A d-pad direction: hat axis and -1 / +1.
    Hat(u16, i32),
}

pub const STICK_MAX: i32 = 32767;
pub const TRIGGER_MAX: i32 = 255;

/// https://w3c.github.io/gamepad/#remapping
pub fn map_button(index: u16) -> Option<Button> {
    use code::*;
    Some(match index {
        0 => Button::Key(BTN_SOUTH),
        1 => Button::Key(BTN_EAST),
        2 => Button::Key(BTN_X),
        3 => Button::Key(BTN_Y),
        4 => Button::Key(BTN_TL),
        5 => Button::Key(BTN_TR),
        6 => Button::Trigger(ABS_Z),
        7 => Button::Trigger(ABS_RZ),
        8 => Button::Key(BTN_SELECT),
        9 => Button::Key(BTN_START),
        10 => Button::Key(BTN_THUMBL),
        11 => Button::Key(BTN_THUMBR),
        12 => Button::Hat(ABS_HAT0Y, -1),
        13 => Button::Hat(ABS_HAT0Y, 1),
        14 => Button::Hat(ABS_HAT0X, -1),
        15 => Button::Hat(ABS_HAT0X, 1),
        16 => Button::Key(BTN_MODE),
        _ => return None,
    })
}

/// Stick axes: left x/y, right x/y (-1..1, +y down, as in both the browser and evdev).
pub fn map_axis(index: u16) -> Option<u16> {
    use code::*;
    [ABS_X, ABS_Y, ABS_RX, ABS_RY].get(index as usize).copied()
}

/// Scales a browser value to the device's range.
pub fn scale(value: f32, max: i32, signed: bool) -> i32 {
    let v = if signed {
        value.clamp(-1.0, 1.0)
    } else {
        value.clamp(0.0, 1.0)
    };
    (v * max as f32).round() as i32
}

/// The hat value from the two opposing buttons (both held: centred).
pub fn hat_value(negative: bool, positive: bool) -> i32 {
    positive as i32 - negative as i32
}

pub fn supported() -> bool {
    crate::testmode::enabled() || cfg!(target_os = "linux")
}

#[cfg(target_os = "linux")]
pub use linux::{handle, release_all};

#[cfg(not(target_os = "linux"))]
pub fn handle(ev: glass_proto::InputEvent) {
    let _ = ev;
}

/// A session ended: let go of its pads.
#[cfg(not(target_os = "linux"))]
pub fn release_all() {}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use evdev::uinput::VirtualDevice;
    use evdev::{
        AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent, InputId,
        KeyCode, UinputAbsSetup,
    };
    use glass_proto::{GAMEPAD_AXIS, GAMEPAD_BUTTON, GAMEPAD_CONNECTED};

    use super::{Button, STICK_MAX, TRIGGER_MAX, code, hat_value, map_axis, map_button, scale};

    /// Browsers report at most a handful of pads; more are ignored.
    const MAX_PADS: u8 = 4;

    struct Pad {
        device: VirtualDevice,
        /// D-pad buttons 12..=15 held.
        dpad: [bool; 4],
    }

    static PADS: Mutex<Option<HashMap<u8, Pad>>> = Mutex::new(None);

    pub fn handle(ev: glass_proto::InputEvent) {
        let glass_proto::InputEvent::Gamepad {
            pad,
            kind,
            index,
            value,
        } = ev
        else {
            return;
        };
        if pad >= MAX_PADS {
            return;
        }
        let mut pads = PADS.lock().unwrap();
        let pads = pads.get_or_insert_with(HashMap::new);
        if kind == GAMEPAD_CONNECTED && value == 0.0 {
            if pads.remove(&pad).is_some() {
                tracing::info!("gamepad {pad}: disconnected");
            }
            return;
        }
        if !pads.contains_key(&pad) {
            match build(pad) {
                Ok(device) => {
                    tracing::info!("gamepad {pad}: virtual controller ready");
                    pads.insert(
                        pad,
                        Pad {
                            device,
                            dpad: [false; 4],
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!("gamepad {pad}: can't create a uinput device ({e})");
                    return;
                }
            }
        }
        let Some(p) = pads.get_mut(&pad) else { return };
        let events = match kind {
            GAMEPAD_BUTTON => match map_button(index) {
                Some(Button::Key(c)) => vec![InputEvent::new(
                    EventType::KEY.0,
                    c,
                    (value >= 0.5) as i32,
                )],
                Some(Button::Trigger(c)) => vec![InputEvent::new(
                    EventType::ABSOLUTE.0,
                    c,
                    scale(value, TRIGGER_MAX, false),
                )],
                Some(Button::Hat(axis, _)) => {
                    p.dpad[(index - 12) as usize] = value >= 0.5;
                    let (neg, pos) = if axis == code::ABS_HAT0Y {
                        (p.dpad[0], p.dpad[1])
                    } else {
                        (p.dpad[2], p.dpad[3])
                    };
                    vec![InputEvent::new(
                        EventType::ABSOLUTE.0,
                        axis,
                        hat_value(neg, pos),
                    )]
                }
                None => return,
            },
            GAMEPAD_AXIS => match map_axis(index) {
                Some(c) => vec![InputEvent::new(
                    EventType::ABSOLUTE.0,
                    c,
                    scale(value, STICK_MAX, true),
                )],
                None => return,
            },
            _ => return,
        };
        if let Err(e) = p.device.emit(&events) {
            tracing::warn!("gamepad {pad}: emit failed: {e}");
        }
    }

    pub fn release_all() {
        if let Some(pads) = PADS.lock().unwrap().as_mut()
            && !pads.is_empty()
        {
            tracing::info!("gamepads: released {}", pads.len());
            pads.clear();
        }
    }

    fn build(pad: u8) -> std::io::Result<VirtualDevice> {
        use code::*;
        let keys: AttributeSet<KeyCode> = [
            BTN_SOUTH, BTN_EAST, BTN_X, BTN_Y, BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE,
            BTN_THUMBL, BTN_THUMBR,
        ]
        .into_iter()
        .map(KeyCode::new)
        .collect();
        let abs = |c: u16, min, max, flat| {
            UinputAbsSetup::new(AbsoluteAxisCode(c), AbsInfo::new(0, min, max, 16, flat, 0))
        };
        let stick = |c| abs(c, -STICK_MAX - 1, STICK_MAX, 128);
        let trigger = |c| abs(c, 0, TRIGGER_MAX, 0);
        let hat = |c| abs(c, -1, 1, 0);
        VirtualDevice::builder()?
            .name(&format!("Glass controller {}", pad + 1))
            // An Xbox 360 controller's ids, so SDL and Steam map it with no setup.
            .input_id(InputId::new(BusType::BUS_USB, 0x045e, 0x028e, 0x0110))
            .with_keys(&keys)?
            .with_absolute_axis(&stick(ABS_X))?
            .with_absolute_axis(&stick(ABS_Y))?
            .with_absolute_axis(&stick(ABS_RX))?
            .with_absolute_axis(&stick(ABS_RY))?
            .with_absolute_axis(&trigger(ABS_Z))?
            .with_absolute_axis(&trigger(ABS_RZ))?
            .with_absolute_axis(&hat(ABS_HAT0X))?
            .with_absolute_axis(&hat(ABS_HAT0Y))?
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_mapping() {
        assert_eq!(map_button(0), Some(Button::Key(code::BTN_SOUTH)));
        assert_eq!(map_button(3), Some(Button::Key(code::BTN_Y)));
        assert_eq!(map_button(6), Some(Button::Trigger(code::ABS_Z)));
        assert_eq!(map_button(7), Some(Button::Trigger(code::ABS_RZ)));
        assert_eq!(map_button(12), Some(Button::Hat(code::ABS_HAT0Y, -1)));
        assert_eq!(map_button(15), Some(Button::Hat(code::ABS_HAT0X, 1)));
        assert_eq!(map_button(16), Some(Button::Key(code::BTN_MODE)));
        assert_eq!(map_button(17), None);
        assert_eq!(map_axis(0), Some(code::ABS_X));
        assert_eq!(map_axis(3), Some(code::ABS_RY));
        assert_eq!(map_axis(4), None);
    }

    #[test]
    fn values_scale_and_clamp() {
        assert_eq!(scale(1.0, STICK_MAX, true), STICK_MAX);
        assert_eq!(scale(-2.0, STICK_MAX, true), -STICK_MAX);
        assert_eq!(scale(0.5, TRIGGER_MAX, false), 128);
        assert_eq!(scale(-0.5, TRIGGER_MAX, false), 0);
        assert_eq!(hat_value(true, false), -1);
        assert_eq!(hat_value(false, true), 1);
        assert_eq!(hat_value(true, true), 0);
    }
}
