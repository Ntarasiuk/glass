//! Game controllers from the viewer (browser Gamepad API, standard mapping), as virtual
//! controllers on the host.
//!
//! Linux: one uinput device per pad, shaped like an Xbox 360 controller (same ids and
//! codes as the xpad driver), so games and SDL pick it up with no setup. Windows: one
//! virtual Xbox 360 controller per pad from the ViGEmBus driver, when it's installed (Windows
//! has no virtual controller of its own for an unsigned app). macOS: one IOHIDUserDevice
//! per pad, shaped like an Xbox Wireless Controller on Bluetooth (its ids, report
//! descriptor and reports), so SDL, Steam and GameController treat it as one. Creating it
//! takes Apple's restricted `com.apple.developer.hid.virtual.device` entitlement, so
//! controllers are offered only when the binary carries it.

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

/// Where Windows gets the driver controllers need.
pub const VIGEM_DOWNLOAD: &str = "https://github.com/nefarius/ViGEmBus/releases";

pub fn supported() -> bool {
    crate::testmode::enabled() || cfg!(target_os = "linux") || windows_supported() || macos_supported()
}

#[cfg(target_os = "macos")]
fn macos_supported() -> bool {
    mac::entitled()
}

#[cfg(not(target_os = "macos"))]
fn macos_supported() -> bool {
    false
}

#[cfg(windows)]
fn windows_supported() -> bool {
    vigem::present()
}

#[cfg(not(windows))]
fn windows_supported() -> bool {
    false
}

#[cfg(target_os = "linux")]
pub use linux::{handle, release_all};

#[cfg(windows)]
pub use vigem::{handle, release_all};

#[cfg(target_os = "macos")]
pub use mac::{handle, release_all};

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub fn handle(ev: glass_proto::InputEvent) {
    let _ = ev;
}

/// A session ended: let go of its pads.
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub fn release_all() {}

/// Xbox 360 controller (XUSB) reports, as the ViGEmBus driver takes them. Built from the
/// same standard mapping as Linux, so both hosts treat a pad alike.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod xusb {
    use glass_proto::{GAMEPAD_AXIS, GAMEPAD_BUTTON};

    use super::{Button, STICK_MAX, TRIGGER_MAX, code, hat_value, map_axis, map_button, scale};

    pub const DPAD_UP: u16 = 0x0001;
    pub const DPAD_DOWN: u16 = 0x0002;
    pub const DPAD_LEFT: u16 = 0x0004;
    pub const DPAD_RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const LEFT_THUMB: u16 = 0x0040;
    pub const RIGHT_THUMB: u16 = 0x0080;
    pub const LEFT_SHOULDER: u16 = 0x0100;
    pub const RIGHT_SHOULDER: u16 = 0x0200;
    pub const GUIDE: u16 = 0x0400;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;

    /// The XUSB bit for a button code from `map_button`.
    pub fn button(c: u16) -> Option<u16> {
        use code::*;
        Some(match c {
            BTN_SOUTH => A,
            BTN_EAST => B,
            BTN_X => X,
            BTN_Y => Y,
            BTN_TL => LEFT_SHOULDER,
            BTN_TR => RIGHT_SHOULDER,
            BTN_SELECT => BACK,
            BTN_START => START,
            BTN_MODE => GUIDE,
            BTN_THUMBL => LEFT_THUMB,
            BTN_THUMBR => RIGHT_THUMB,
            _ => return None,
        })
    }

    /// XUSB_REPORT: 12 bytes, little-endian, sticks with +y up.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Report {
        pub buttons: u16,
        pub left_trigger: u8,
        pub right_trigger: u8,
        pub lx: i16,
        pub ly: i16,
        pub rx: i16,
        pub ry: i16,
    }

    impl Report {
        pub fn to_bytes(self) -> [u8; 12] {
            let mut b = [0u8; 12];
            b[0..2].copy_from_slice(&self.buttons.to_le_bytes());
            b[2] = self.left_trigger;
            b[3] = self.right_trigger;
            for (i, v) in [self.lx, self.ly, self.rx, self.ry].into_iter().enumerate() {
                b[4 + i * 2..6 + i * 2].copy_from_slice(&v.to_le_bytes());
            }
            b
        }
    }

    /// One pad: its report, and which d-pad buttons (12..=15) are held, so two opposite
    /// ones held count as neither (as the Linux hat does).
    #[derive(Debug, Default)]
    pub struct Pad {
        pub report: Report,
        dpad: [bool; 4],
    }

    impl Pad {
        /// Applies one browser event; true if the report changed.
        pub fn apply(&mut self, kind: u8, index: u16, value: f32) -> bool {
            let before = self.report;
            let r = &mut self.report;
            match kind {
                GAMEPAD_BUTTON => match map_button(index) {
                    Some(Button::Key(c)) => {
                        let Some(bit) = button(c) else { return false };
                        if value >= 0.5 {
                            r.buttons |= bit;
                        } else {
                            r.buttons &= !bit;
                        }
                    }
                    Some(Button::Trigger(c)) => {
                        let v = scale(value, TRIGGER_MAX, false) as u8;
                        if c == code::ABS_Z {
                            r.left_trigger = v;
                        } else {
                            r.right_trigger = v;
                        }
                    }
                    Some(Button::Hat(..)) => {
                        self.dpad[(index - 12) as usize] = value >= 0.5;
                        let y = hat_value(self.dpad[0], self.dpad[1]);
                        let x = hat_value(self.dpad[2], self.dpad[3]);
                        r.buttons &= !(DPAD_UP | DPAD_DOWN | DPAD_LEFT | DPAD_RIGHT);
                        for (on, bit) in [(y < 0, DPAD_UP), (y > 0, DPAD_DOWN), (x < 0, DPAD_LEFT), (x > 0, DPAD_RIGHT)] {
                            if on {
                                r.buttons |= bit;
                            }
                        }
                    }
                    None => return false,
                },
                GAMEPAD_AXIS => {
                    let Some(c) = map_axis(index) else { return false };
                    let v = scale(value, STICK_MAX, true) as i16;
                    match c {
                        code::ABS_X => r.lx = v,
                        // The browser's +y is down, XInput's up.
                        code::ABS_Y => r.ly = -v,
                        code::ABS_RX => r.rx = v,
                        _ => r.ry = -v,
                    }
                }
                _ => return false,
            }
            self.report != before
        }
    }

    /// ViGEmBus's control codes: CTL_CODE(FILE_DEVICE_BUS_EXTENDER, 0x801 + n,
    /// METHOD_BUFFERED, FILE_WRITE_DATA).
    pub const fn ioctl(n: u32) -> u32 {
        (0x2a << 16) | (0x2 << 14) | ((0x801 + n) << 2)
    }
    pub const PLUGIN_TARGET: u32 = ioctl(0x000);
    pub const UNPLUG_TARGET: u32 = ioctl(0x001);
    pub const CHECK_VERSION: u32 = ioctl(0x002);
    pub const WAIT_DEVICE_READY: u32 = ioctl(0x003);
    pub const SUBMIT_REPORT: u32 = ioctl(0x201);

    /// VIGEM_PLUGIN_TARGET for an Xbox 360 controller (target type 0) with its usual ids.
    pub fn plugin(serial: u32) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&16u32.to_le_bytes());
        b[4..8].copy_from_slice(&serial.to_le_bytes());
        b[12..14].copy_from_slice(&0x045eu16.to_le_bytes());
        b[14..16].copy_from_slice(&0x028eu16.to_le_bytes());
        b
    }

    /// The {Size, SerialNo} (or {Size, Version}) body of the other requests.
    pub fn serial(value: u32) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[0..4].copy_from_slice(&8u32.to_le_bytes());
        b[4..8].copy_from_slice(&value.to_le_bytes());
        b
    }

    /// XUSB_SUBMIT_REPORT.
    pub fn submit(serial: u32, report: Report) -> [u8; 20] {
        let mut b = [0u8; 20];
        b[0..4].copy_from_slice(&20u32.to_le_bytes());
        b[4..8].copy_from_slice(&serial.to_le_bytes());
        b[8..20].copy_from_slice(&report.to_bytes());
        b
    }
}

#[cfg(windows)]
mod vigem {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use glass_proto::GAMEPAD_CONNECTED;
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW, CR_SUCCESS,
    };
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::core::{GUID, PCWSTR};

    use super::xusb;

    /// GUID_DEVINTERFACE_BUSENUM_VIGEM.
    const BUS: GUID = GUID::from_u128(0x96e42b22_f5e9_42f8_b043_ed0f932f014f);
    /// The interface version this speaks (VIGEM_COMMON_VERSION).
    const VERSION: u32 = 1;
    /// The bus's own limit on controllers (serials 1..=16).
    const MAX_SERIAL: u32 = 16;
    const MAX_PADS: u8 = 4;

    /// The ViGEmBus device's path, when the driver is installed and running.
    fn bus_path() -> Option<Vec<u16>> {
        let mut len = 0u32;
        // SAFETY: len is a valid out pointer; BUS lives for the call.
        let r = unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, &BUS, PCWSTR::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
        if r != CR_SUCCESS || len <= 1 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: buf is writable and its length is passed with it.
        let r = unsafe { CM_Get_Device_Interface_ListW(&BUS, PCWSTR::null(), &mut buf, CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
        if r != CR_SUCCESS {
            return None;
        }
        // A list of NUL-terminated paths: the first one, with its NUL.
        let end = buf.iter().position(|&c| c == 0)?;
        (end > 0).then(|| buf[..=end].to_vec())
    }

    pub fn present() -> bool {
        bus_path().is_some()
    }

    /// An open handle to the bus. Closing it unplugs every controller it plugged in.
    struct Bus(HANDLE);

    // SAFETY: a file handle may be used from any thread; it's only used under PADS' lock.
    unsafe impl Send for Bus {}

    impl Drop for Bus {
        fn drop(&mut self) {
            // SAFETY: the handle is open and owned here.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    impl Bus {
        fn open() -> anyhow::Result<Bus> {
            let path = bus_path().ok_or_else(|| anyhow::anyhow!("ViGEmBus isn't installed ({})", super::VIGEM_DOWNLOAD))?;
            // SAFETY: path is NUL-terminated and outlives the call. Not overlapped: each
            // request waits for its answer, one at a time under PADS' lock.
            let h = unsafe {
                CreateFileW(
                    PCWSTR(path.as_ptr()),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            }?;
            let bus = Bus(h);
            bus.call(xusb::CHECK_VERSION, &xusb::serial(VERSION))
                .map_err(|e| anyhow::anyhow!("this ViGEmBus version doesn't match ({e}); install the latest from {}", super::VIGEM_DOWNLOAD))?;
            Ok(bus)
        }

        fn call(&self, code: u32, body: &[u8]) -> windows::core::Result<()> {
            let mut got = 0u32;
            // SAFETY: body is readable for its length; there's no output buffer.
            unsafe { DeviceIoControl(self.0, code, Some(body.as_ptr().cast()), body.len() as u32, None, 0, Some(&mut got), None) }
        }

        /// Plugs in a new controller, on the first free serial.
        fn plug(&self) -> anyhow::Result<u32> {
            let mut last = None;
            for serial in 1..=MAX_SERIAL {
                match self.call(xusb::PLUGIN_TARGET, &xusb::plugin(serial)) {
                    Ok(()) => {
                        // Waits until it takes reports (drivers before 1.17 don't have
                        // this request, and are ready already).
                        if let Err(e) = self.call(xusb::WAIT_DEVICE_READY, &xusb::serial(serial)) {
                            tracing::debug!("gamepad: wait-ready: {e}");
                        }
                        return Ok(serial);
                    }
                    // Taken (by this or another app): the next one.
                    Err(e) => last = Some(e),
                }
            }
            Err(anyhow::anyhow!("no free controller slot ({})", last.map(|e| e.to_string()).unwrap_or_default()))
        }

        fn unplug(&self, serial: u32) {
            if let Err(e) = self.call(xusb::UNPLUG_TARGET, &xusb::serial(serial)) {
                tracing::warn!("gamepad: unplug {serial}: {e}");
            }
        }
    }

    struct Pad {
        serial: u32,
        state: xusb::Pad,
    }

    #[derive(Default)]
    struct State {
        bus: Option<Bus>,
        pads: HashMap<u8, Pad>,
    }

    static PADS: Mutex<Option<State>> = Mutex::new(None);

    pub fn handle(ev: glass_proto::InputEvent) {
        let glass_proto::InputEvent::Gamepad { pad, kind, index, value } = ev else { return };
        if pad >= MAX_PADS {
            return;
        }
        let mut state = PADS.lock().unwrap();
        let state = state.get_or_insert_with(State::default);
        if kind == GAMEPAD_CONNECTED && value == 0.0 {
            if let Some(p) = state.pads.remove(&pad) {
                if let Some(bus) = &state.bus {
                    bus.unplug(p.serial);
                }
                tracing::info!("gamepad {pad}: disconnected");
            }
            if state.pads.is_empty() {
                state.bus = None;
            }
            return;
        }
        if !state.pads.contains_key(&pad) {
            if state.bus.is_none() {
                match Bus::open() {
                    Ok(bus) => state.bus = Some(bus),
                    Err(e) => {
                        tracing::warn!("gamepad {pad}: {e}");
                        return;
                    }
                }
            }
            let Some(bus) = &state.bus else { return };
            match bus.plug() {
                Ok(serial) => {
                    tracing::info!("gamepad {pad}: virtual Xbox 360 controller ready");
                    state.pads.insert(pad, Pad { serial, state: xusb::Pad::default() });
                }
                Err(e) => {
                    tracing::warn!("gamepad {pad}: can't plug in a ViGEmBus controller ({e})");
                    return;
                }
            }
        }
        let (Some(bus), Some(p)) = (&state.bus, state.pads.get_mut(&pad)) else { return };
        if p.state.apply(kind, index, value)
            && let Err(e) = bus.call(xusb::SUBMIT_REPORT, &xusb::submit(p.serial, p.state.report))
        {
            tracing::warn!("gamepad {pad}: report failed: {e}");
        }
    }

    pub fn release_all() {
        let mut state = PADS.lock().unwrap();
        let Some(state) = state.as_mut() else { return };
        if !state.pads.is_empty() {
            tracing::info!("gamepads: released {}", state.pads.len());
        }
        if let Some(bus) = &state.bus {
            for p in state.pads.values() {
                bus.unplug(p.serial);
            }
        }
        state.pads.clear();
        state.bus = None;
    }
}

/// Xbox Wireless Controller (Xbox One S, Bluetooth, firmware 5.x) HID reports, for hosts
/// that make a virtual HID device (macOS). Built from the XUSB report, so every host treats
/// a pad alike. Layout as SDL's HIDAPI Xbox One driver reads it (17-byte packets).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub mod xbox_hid {
    use super::xusb;

    pub const VENDOR: u32 = 0x045e;
    /// USB_PRODUCT_XBOX_ONE_S_REV2_BLE: firmware 5.x, Back and Guide in report 1.
    pub const PRODUCT: u32 = 0x0b20;
    pub const NAME: &str = "Xbox Wireless Controller";

    /// The controller's own report descriptor, as captured from one (xpadneo,
    /// docs/descriptors/xb1s_linux.md): report 1 the state, 2 the Home key, 3 rumble
    /// (output), 4 battery.
    pub const DESCRIPTOR: [u8; 334] = [
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x09, 0x30, 0x09, 0x31,
    0x15, 0x00, 0x27, 0xff, 0xff, 0x00, 0x00, 0x95, 0x02, 0x75, 0x10, 0x81, 0x02, 0xc0, 0x09, 0x01,
    0xa1, 0x00, 0x09, 0x32, 0x09, 0x35, 0x15, 0x00, 0x27, 0xff, 0xff, 0x00, 0x00, 0x95, 0x02, 0x75,
    0x10, 0x81, 0x02, 0xc0, 0x05, 0x02, 0x09, 0xc5, 0x15, 0x00, 0x26, 0xff, 0x03, 0x95, 0x01, 0x75,
    0x0a, 0x81, 0x02, 0x15, 0x00, 0x25, 0x00, 0x75, 0x06, 0x95, 0x01, 0x81, 0x03, 0x05, 0x02, 0x09,
    0xc4, 0x15, 0x00, 0x26, 0xff, 0x03, 0x95, 0x01, 0x75, 0x0a, 0x81, 0x02, 0x15, 0x00, 0x25, 0x00,
    0x75, 0x06, 0x95, 0x01, 0x81, 0x03, 0x05, 0x01, 0x09, 0x39, 0x15, 0x01, 0x25, 0x08, 0x35, 0x00,
    0x46, 0x3b, 0x01, 0x66, 0x14, 0x00, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x75, 0x04, 0x95, 0x01,
    0x15, 0x00, 0x25, 0x00, 0x35, 0x00, 0x45, 0x00, 0x65, 0x00, 0x81, 0x03, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x0f, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0f, 0x81, 0x02, 0x15, 0x00, 0x25, 0x00,
    0x75, 0x01, 0x95, 0x01, 0x81, 0x03, 0x05, 0x0c, 0x0a, 0x24, 0x02, 0x15, 0x00, 0x25, 0x01, 0x95,
    0x01, 0x75, 0x01, 0x81, 0x02, 0x15, 0x00, 0x25, 0x00, 0x75, 0x07, 0x95, 0x01, 0x81, 0x03, 0x05,
    0x0c, 0x09, 0x01, 0x85, 0x02, 0xa1, 0x01, 0x05, 0x0c, 0x0a, 0x23, 0x02, 0x15, 0x00, 0x25, 0x01,
    0x95, 0x01, 0x75, 0x01, 0x81, 0x02, 0x15, 0x00, 0x25, 0x00, 0x75, 0x07, 0x95, 0x01, 0x81, 0x03,
    0xc0, 0x05, 0x0f, 0x09, 0x21, 0x85, 0x03, 0xa1, 0x02, 0x09, 0x97, 0x15, 0x00, 0x25, 0x01, 0x75,
    0x04, 0x95, 0x01, 0x91, 0x02, 0x15, 0x00, 0x25, 0x00, 0x75, 0x04, 0x95, 0x01, 0x91, 0x03, 0x09,
    0x70, 0x15, 0x00, 0x25, 0x64, 0x75, 0x08, 0x95, 0x04, 0x91, 0x02, 0x09, 0x50, 0x66, 0x01, 0x10,
    0x55, 0x0e, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0x09, 0xa7, 0x15,
    0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0x65, 0x00, 0x55, 0x00, 0x09, 0x7c,
    0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x01, 0x91, 0x02, 0xc0, 0x05, 0x06, 0x09, 0x20,
    0x85, 0x04, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xc0,
    ];

    pub const REPORT_LEN: usize = 17;

    /// The hat value: 1 up, clockwise to 8 up-left; 0 centred.
    fn hat(buttons: u16) -> u8 {
        let up = buttons & xusb::DPAD_UP != 0;
        let down = buttons & xusb::DPAD_DOWN != 0;
        let left = buttons & xusb::DPAD_LEFT != 0;
        let right = buttons & xusb::DPAD_RIGHT != 0;
        match (up, right, down, left) {
            (true, false, _, false) => 1,
            (true, true, _, _) => 2,
            (false, true, false, _) => 3,
            (_, true, true, _) => 4,
            (false, false, true, false) => 5,
            (_, false, true, true) => 6,
            (false, _, false, true) => 7,
            (true, false, _, true) => 8,
            _ => 0,
        }
    }

    /// Report 1. Sticks are unsigned with +y down, triggers 10-bit.
    pub fn report(r: xusb::Report) -> [u8; REPORT_LEN] {
        let mut b = [0u8; REPORT_LEN];
        b[0] = 0x01;
        let stick = |v: i16, flip: bool| -> [u8; 2] {
            let v = if flip { -(v as i32) } else { v as i32 };
            ((v + 0x8000).clamp(0, 0xffff) as u16).to_le_bytes()
        };
        b[1..3].copy_from_slice(&stick(r.lx, false));
        b[3..5].copy_from_slice(&stick(r.ly, true));
        b[5..7].copy_from_slice(&stick(r.rx, false));
        b[7..9].copy_from_slice(&stick(r.ry, true));
        let trigger = |v: u8| ((v as u32 * 1023 + 127) / 255) as u16;
        b[9..11].copy_from_slice(&trigger(r.left_trigger).to_le_bytes());
        b[11..13].copy_from_slice(&trigger(r.right_trigger).to_le_bytes());
        b[13] = hat(r.buttons);
        let held = |bit: u16| r.buttons & bit != 0;
        for (bit, byte, mask) in [
            (xusb::A, 14, 0x01),
            (xusb::B, 14, 0x02),
            (xusb::X, 14, 0x08),
            (xusb::Y, 14, 0x10),
            (xusb::LEFT_SHOULDER, 14, 0x40),
            (xusb::RIGHT_SHOULDER, 14, 0x80),
            (xusb::BACK, 15, 0x04),
            (xusb::START, 15, 0x08),
            (xusb::GUIDE, 15, 0x10),
            (xusb::LEFT_THUMB, 15, 0x20),
            (xusb::RIGHT_THUMB, 15, 0x40),
        ] {
            if held(bit) {
                b[byte] |= mask;
            }
        }
        b
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::collections::{HashMap, HashSet};
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::{Mutex, OnceLock};

    use glass_proto::GAMEPAD_CONNECTED;
    use objc2_core_foundation::{CFData, CFDictionary, CFNumber, CFString, CFType};

    use super::{xbox_hid, xusb};

    const ENTITLEMENT: &str = "com.apple.developer.hid.virtual.device";
    const MAX_PADS: u8 = 4;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOHIDUserDeviceCreateWithProperties(allocator: *const c_void, properties: *const c_void, options: u32) -> *mut c_void;
        fn IOHIDUserDeviceHandleReportWithTimeStamp(device: *mut c_void, timestamp: u64, report: *const u8, length: isize) -> i32;
    }

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecTaskCreateFromSelf(allocator: *const c_void) -> *mut c_void;
        fn SecTaskCopyValueForEntitlement(task: *mut c_void, entitlement: *const c_void, error: *mut *mut c_void) -> *mut c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
        fn CFGetTypeID(cf: *const c_void) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(boolean: *const c_void) -> bool;
    }

    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
    }

    /// Whether this binary is signed with the virtual HID entitlement (without it, the
    /// kernel refuses to make the device).
    pub fn entitled() -> bool {
        static ENTITLED: OnceLock<bool> = OnceLock::new();
        *ENTITLED.get_or_init(|| {
            let name = CFString::from_str(ENTITLEMENT);
            // SAFETY: plain CF calls; every object returned here is released here.
            unsafe {
                let task = SecTaskCreateFromSelf(std::ptr::null());
                if task.is_null() {
                    return false;
                }
                let value = SecTaskCopyValueForEntitlement(task, (&*name as *const CFString).cast(), std::ptr::null_mut());
                CFRelease(task);
                if value.is_null() {
                    return false;
                }
                let on = CFGetTypeID(value) == CFBooleanGetTypeID() && CFBooleanGetValue(value);
                CFRelease(value);
                on
            }
        })
    }

    /// A virtual controller. Never given a dispatch queue (it only sends input reports),
    /// so releasing it removes the device.
    struct Device(NonNull<c_void>);

    // SAFETY: a CF object, only used under PADS' lock.
    unsafe impl Send for Device {}

    impl Drop for Device {
        fn drop(&mut self) {
            // SAFETY: owned here (a +1 reference from Create).
            unsafe { CFRelease(self.0.as_ptr()) };
        }
    }

    impl Device {
        fn create(pad: u8) -> anyhow::Result<Device> {
            let num = |v: u32| CFNumber::new_i32(v as i32);
            let descriptor = CFData::from_bytes(&xbox_hid::DESCRIPTOR);
            let (vendor, product) = (num(xbox_hid::VENDOR), num(xbox_hid::PRODUCT));
            let version = num(0x0509);
            // Distinct per pad, so two pads don't look like one device reconnecting.
            let location = num(0x474c_0000 | pad as u32);
            let (name, maker) = (CFString::from_str(xbox_hid::NAME), CFString::from_str("Microsoft"));
            let transport = CFString::from_str("Bluetooth Low Energy");
            let serial = CFString::from_str(&format!("glass-pad-{}", pad + 1));
            let keys = [
                "ReportDescriptor",
                "VendorID",
                "ProductID",
                "VersionNumber",
                "LocationID",
                "Product",
                "Manufacturer",
                "Transport",
                "SerialNumber",
            ]
            .map(CFString::from_str);
            let values: [&CFType; 9] =
                [&descriptor, &vendor, &product, &version, &location, &name, &maker, &transport, &serial];
            let keys: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
            let props = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
            // SAFETY: props is a valid dictionary for the call; the result is +1 or NULL.
            let device = unsafe { IOHIDUserDeviceCreateWithProperties(std::ptr::null(), (&*props as *const CFDictionary<CFString, CFType>).cast(), 0) };
            let device = NonNull::new(device).ok_or_else(|| {
                anyhow::anyhow!(
                    "macOS refused to create a virtual controller{}",
                    if entitled() { "" } else { " (this build lacks the virtual HID entitlement)" }
                )
            })?;
            Ok(Device(device))
        }

        fn send(&self, report: &[u8]) -> Result<(), i32> {
            // SAFETY: the device is live; report is readable for its length.
            let r = unsafe { IOHIDUserDeviceHandleReportWithTimeStamp(self.0.as_ptr(), mach_absolute_time(), report.as_ptr(), report.len() as isize) };
            if r == 0 { Ok(()) } else { Err(r) }
        }
    }

    struct Pad {
        device: Device,
        state: xusb::Pad,
    }

    #[derive(Default)]
    struct State {
        pads: HashMap<u8, Pad>,
        /// Pads whose device couldn't be made: not retried (on every stick move) until
        /// they reconnect.
        failed: HashSet<u8>,
    }

    static PADS: Mutex<Option<State>> = Mutex::new(None);

    pub fn handle(ev: glass_proto::InputEvent) {
        let glass_proto::InputEvent::Gamepad { pad, kind, index, value } = ev else { return };
        if pad >= MAX_PADS {
            return;
        }
        let mut state = PADS.lock().unwrap();
        let State { pads, failed } = state.get_or_insert_with(State::default);
        if kind == GAMEPAD_CONNECTED {
            failed.remove(&pad);
            if value == 0.0 {
                if pads.remove(&pad).is_some() {
                    tracing::info!("gamepad {pad}: disconnected");
                }
                return;
            }
        }
        if failed.contains(&pad) {
            return;
        }
        let p = match pads.entry(pad) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => match Device::create(pad) {
                Ok(device) => {
                    tracing::info!("gamepad {pad}: virtual Xbox controller ready");
                    let state = xusb::Pad::default();
                    // Start centred: sticks at rest are 0x8000, not 0.
                    let _ = device.send(&xbox_hid::report(state.report));
                    e.insert(Pad { device, state })
                }
                Err(e) => {
                    tracing::warn!("gamepad {pad}: {e}");
                    failed.insert(pad);
                    return;
                }
            },
        };
        if p.state.apply(kind, index, value)
            && let Err(e) = p.device.send(&xbox_hid::report(p.state.report))
        {
            tracing::warn!("gamepad {pad}: report failed: {e:#x}");
        }
    }

    pub fn release_all() {
        let mut state = PADS.lock().unwrap();
        let Some(state) = state.as_mut() else { return };
        if !state.pads.is_empty() {
            tracing::info!("gamepads: released {}", state.pads.len());
        }
        state.pads.clear();
        state.failed.clear();
    }

    /// Makes a real controller and presses A. Needs a binary signed with the entitlement
    /// (on a Mac with SIP and AMFI checks off until Apple grants it):
    /// `cargo test -p glass-host makes_a_virtual_controller -- --ignored`.
    #[test]
    #[ignore]
    fn makes_a_virtual_controller() {
        println!("entitled: {}", entitled());
        let device = Device::create(0).expect("create");
        let mut pad = xusb::Pad::default();
        pad.apply(glass_proto::GAMEPAD_BUTTON, 0, 1.0);
        device.send(&xbox_hid::report(pad.report)).expect("report");
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

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

    #[test]
    fn xusb_reports_follow_the_standard_mapping() {
        use glass_proto::{GAMEPAD_AXIS, GAMEPAD_BUTTON, GAMEPAD_CONNECTED};
        let mut p = xusb::Pad::default();
        assert!(p.apply(GAMEPAD_BUTTON, 0, 1.0));
        assert!(p.apply(GAMEPAD_BUTTON, 16, 1.0));
        assert_eq!(p.report.buttons, xusb::A | xusb::GUIDE);
        assert!(!p.apply(GAMEPAD_BUTTON, 0, 0.9), "no change, no report");
        assert!(p.apply(GAMEPAD_BUTTON, 0, 0.0));
        assert_eq!(p.report.buttons, xusb::GUIDE);
        p.apply(GAMEPAD_BUTTON, 8, 1.0);
        p.apply(GAMEPAD_BUTTON, 9, 1.0);
        assert_eq!(p.report.buttons, xusb::GUIDE | xusb::BACK | xusb::START);
        p.apply(GAMEPAD_BUTTON, 6, 0.5);
        p.apply(GAMEPAD_BUTTON, 7, 1.0);
        assert_eq!((p.report.left_trigger, p.report.right_trigger), (128, 255));
        // Up and down at once: neither.
        p.apply(GAMEPAD_BUTTON, 12, 1.0);
        assert_eq!(p.report.buttons & 0xf, xusb::DPAD_UP);
        p.apply(GAMEPAD_BUTTON, 13, 1.0);
        p.apply(GAMEPAD_BUTTON, 15, 1.0);
        assert_eq!(p.report.buttons & 0xf, xusb::DPAD_RIGHT);
        // The browser's +y is down; XInput's is up.
        p.apply(GAMEPAD_AXIS, 0, -1.0);
        p.apply(GAMEPAD_AXIS, 1, 1.0);
        p.apply(GAMEPAD_AXIS, 3, -0.5);
        assert_eq!((p.report.lx, p.report.ly, p.report.ry), (-32767, -32767, 16384));
        assert!(!p.apply(GAMEPAD_AXIS, 4, 1.0));
        assert!(!p.apply(GAMEPAD_BUTTON, 17, 1.0));
        assert!(!p.apply(GAMEPAD_CONNECTED, 0, 1.0));
        // Every Linux button has an XUSB one.
        for i in (0..=11).chain([16]) {
            if let Some(Button::Key(c)) = map_button(i) {
                assert!(xusb::button(c).is_some(), "button {i}");
            }
        }
    }

    #[test]
    fn vigem_requests_are_laid_out_as_the_driver_reads_them() {
        assert_eq!(xusb::PLUGIN_TARGET, 0x2a_a004);
        assert_eq!(xusb::UNPLUG_TARGET, 0x2a_a008);
        assert_eq!(xusb::CHECK_VERSION, 0x2a_a00c);
        assert_eq!(xusb::WAIT_DEVICE_READY, 0x2a_a010);
        assert_eq!(xusb::SUBMIT_REPORT, 0x2a_a808);
        assert_eq!(xusb::plugin(3), [16, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0x5e, 0x04, 0x8e, 0x02]);
        assert_eq!(xusb::serial(1), [8, 0, 0, 0, 1, 0, 0, 0]);
        let r = xusb::Report { buttons: 0x1001, left_trigger: 1, right_trigger: 2, lx: -1, ly: 1, rx: 0, ry: 256 };
        assert_eq!(r.to_bytes(), [0x01, 0x10, 1, 2, 0xff, 0xff, 1, 0, 0, 0, 0, 1]);
        let s = xusb::submit(2, r);
        assert_eq!(&s[..8], &[20, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(&s[8..], &r.to_bytes());
    }

    #[test]
    fn xbox_hid_reports_match_sdl() {
        use glass_proto::{GAMEPAD_AXIS, GAMEPAD_BUTTON};
        let mut p = xusb::Pad::default();
        let r = xbox_hid::report(p.report);
        assert_eq!(r.len(), xbox_hid::REPORT_LEN);
        assert_eq!(r[0], 1, "report id");
        // At rest: sticks centred, triggers released, hat centred, nothing held.
        assert_eq!(&r[1..9], &[0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80]);
        assert_eq!(&r[9..], &[0; 8]);

        // Browser +y down stays +y down (SDL: raw - 0x8000, +y down).
        p.apply(GAMEPAD_AXIS, 0, -1.0);
        p.apply(GAMEPAD_AXIS, 1, 1.0);
        p.apply(GAMEPAD_AXIS, 3, -1.0);
        let r = xbox_hid::report(p.report);
        let at = |i: usize| u16::from_le_bytes([r[i], r[i + 1]]) as i32 - 0x8000;
        assert_eq!((at(1), at(3), at(5), at(7)), (-32767, 32767, 0, -32767));

        p.apply(GAMEPAD_BUTTON, 6, 1.0);
        p.apply(GAMEPAD_BUTTON, 7, 0.5);
        let r = xbox_hid::report(p.report);
        assert_eq!(u16::from_le_bytes([r[9], r[10]]), 1023);
        assert_eq!(u16::from_le_bytes([r[11], r[12]]), 514);

        // SDL's bits: byte 14 A B _ X Y _ LB RB; byte 15 _ _ Back Start Guide LS RS.
        for (i, byte, mask) in [(0, 14, 0x01), (1, 14, 0x02), (2, 14, 0x08), (3, 14, 0x10), (4, 14, 0x40), (5, 14, 0x80), (8, 15, 0x04), (9, 15, 0x08), (16, 15, 0x10), (10, 15, 0x20), (11, 15, 0x40)] {
            let mut p = xusb::Pad::default();
            p.apply(GAMEPAD_BUTTON, i, 1.0);
            let r = xbox_hid::report(p.report);
            assert_eq!((r[14], r[15]), if byte == 14 { (mask, 0) } else { (0, mask) }, "button {i}");
        }

        // Hat: 1 up, clockwise; opposite directions cancel.
        let hat = |held: &[u16]| {
            let mut p = xusb::Pad::default();
            for &i in held {
                p.apply(GAMEPAD_BUTTON, i, 1.0);
            }
            xbox_hid::report(p.report)[13]
        };
        assert_eq!(hat(&[12]), 1);
        assert_eq!(hat(&[12, 15]), 2);
        assert_eq!(hat(&[15]), 3);
        assert_eq!(hat(&[13, 15]), 4);
        assert_eq!(hat(&[13]), 5);
        assert_eq!(hat(&[13, 14]), 6);
        assert_eq!(hat(&[14]), 7);
        assert_eq!(hat(&[12, 14]), 8);
        assert_eq!(hat(&[12, 13]), 0);
        assert_eq!(hat(&[]), 0);
    }

    #[test]
    fn xbox_hid_descriptor_is_whole() {
        let d = &xbox_hid::DESCRIPTOR;
        // Gamepad application collection, report 1 first; ends closing the last one.
        assert_eq!(&d[..8], &[0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01]);
        assert_eq!(d[d.len() - 1], 0xc0);
    }
}
