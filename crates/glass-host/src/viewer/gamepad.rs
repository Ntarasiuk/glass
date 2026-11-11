//! Game controllers on this computer (GameController on a Mac, XInput on Windows), to the
//! other computer in the browser's "standard" layout, which the host maps to a virtual pad.

use glass_proto::{GAMEPAD_AXIS, GAMEPAD_BUTTON, GAMEPAD_CONNECTED, InputEvent};

const BUTTONS: usize = 17;
const AXES: usize = 4;
const MAX_PADS: usize = 4;

/// One pad: A, B, X, Y, LB, RB, LT, RT, back, start, LS, RS, up, down, left, right, home;
/// axes left x/y, right x/y (down is positive, as on the web).
#[derive(Clone, Copy, PartialEq)]
struct State {
    buttons: [f32; BUTTONS],
    axes: [f32; AXES],
}

#[cfg(target_os = "macos")]
fn read_all() -> [Option<State>; MAX_PADS] {
    use objc2_game_controller::{GCController, GCControllerButtonInput, GCExtendedGamepad};
    fn read(pad: &GCExtendedGamepad) -> State {
        let v = |b: &GCControllerButtonInput| unsafe { b.value() };
        let opt = |b: Option<objc2::rc::Retained<GCControllerButtonInput>>| b.map_or(0.0, |b| v(&b));
        unsafe {
            let dpad = pad.dpad();
            let (ls, rs) = (pad.leftThumbstick(), pad.rightThumbstick());
            State {
                buttons: [
                    v(&pad.buttonA()),
                    v(&pad.buttonB()),
                    v(&pad.buttonX()),
                    v(&pad.buttonY()),
                    v(&pad.leftShoulder()),
                    v(&pad.rightShoulder()),
                    v(&pad.leftTrigger()),
                    v(&pad.rightTrigger()),
                    opt(pad.buttonOptions()),
                    v(&pad.buttonMenu()),
                    opt(pad.leftThumbstickButton()),
                    opt(pad.rightThumbstickButton()),
                    v(&dpad.up()),
                    v(&dpad.down()),
                    v(&dpad.left()),
                    v(&dpad.right()),
                    opt(pad.buttonHome()),
                ],
                // GameController's y points up.
                axes: [ls.xAxis().value(), -ls.yAxis().value(), rs.xAxis().value(), -rs.yAxis().value()],
            }
        }
    }
    let mut out = [None; MAX_PADS];
    let pads: Vec<_> = unsafe { GCController::controllers() }.iter().filter_map(|c| unsafe { c.extendedGamepad() }).take(MAX_PADS).collect();
    for (i, p) in pads.iter().enumerate() {
        out[i] = Some(read(p));
    }
    out
}

#[cfg(windows)]
fn read_all() -> [Option<State>; MAX_PADS] {
    let mut out = [None; MAX_PADS];
    // Windows.Gaming.Input: every standard pad (Xbox, PlayStation…) as a Gamepad, then the
    // other controllers raw. Where it isn't available, XInput (Xbox pads only).
    match wgi_gamepads() {
        Some(pads) => {
            for (slot, state) in out.iter_mut().zip(pads.into_iter().chain(raw_controllers())) {
                *slot = Some(state);
            }
        }
        None => out = xinput(),
    }
    out
}

/// Standard pads through Windows.Gaming.Input; `None` if it isn't there.
#[cfg(windows)]
fn wgi_gamepads() -> Option<Vec<State>> {
    use windows::Gaming::Input::{Gamepad, GamepadButtons as B};
    watch_raw_controllers();
    let list = Gamepad::Gamepads().ok()?;
    Some(
        list.into_iter()
            .filter_map(|g| g.GetCurrentReading().ok())
            .map(|r| {
                let b = |f: B| if r.Buttons.0 & f.0 != 0 { 1.0 } else { 0.0 };
                State {
                    buttons: [
                        b(B::A),
                        b(B::B),
                        b(B::X),
                        b(B::Y),
                        b(B::LeftShoulder),
                        b(B::RightShoulder),
                        r.LeftTrigger as f32,
                        r.RightTrigger as f32,
                        b(B::View),
                        b(B::Menu),
                        b(B::LeftThumbstick),
                        b(B::RightThumbstick),
                        b(B::DPadUp),
                        b(B::DPadDown),
                        b(B::DPadLeft),
                        b(B::DPadRight),
                        0.0,
                    ],
                    // Windows.Gaming.Input's y points up.
                    axes: [r.LeftThumbstickX as f32, -r.LeftThumbstickY as f32, r.RightThumbstickX as f32, -r.RightThumbstickY as f32],
                }
            })
            .collect(),
    )
}

#[cfg(windows)]
fn xinput() -> [Option<State>; MAX_PADS] {
    use windows::Win32::UI::Input::XboxController::*;
    let mut out = [None; MAX_PADS];
    for (i, slot) in out.iter_mut().enumerate() {
        let mut s = XINPUT_STATE::default();
        // SAFETY: a plain query into a local.
        if unsafe { XInputGetState(i as u32, &mut s) } != 0 {
            continue;
        }
        let g = s.Gamepad;
        let b = |f: XINPUT_GAMEPAD_BUTTON_FLAGS| if g.wButtons.0 & f.0 != 0 { 1.0 } else { 0.0 };
        let axis = |v: i16| (v as f32 / 32767.0).clamp(-1.0, 1.0);
        *slot = Some(State {
            buttons: [
                b(XINPUT_GAMEPAD_A),
                b(XINPUT_GAMEPAD_B),
                b(XINPUT_GAMEPAD_X),
                b(XINPUT_GAMEPAD_Y),
                b(XINPUT_GAMEPAD_LEFT_SHOULDER),
                b(XINPUT_GAMEPAD_RIGHT_SHOULDER),
                g.bLeftTrigger as f32 / 255.0,
                g.bRightTrigger as f32 / 255.0,
                b(XINPUT_GAMEPAD_BACK),
                b(XINPUT_GAMEPAD_START),
                b(XINPUT_GAMEPAD_LEFT_THUMB),
                b(XINPUT_GAMEPAD_RIGHT_THUMB),
                b(XINPUT_GAMEPAD_DPAD_UP),
                b(XINPUT_GAMEPAD_DPAD_DOWN),
                b(XINPUT_GAMEPAD_DPAD_LEFT),
                b(XINPUT_GAMEPAD_DPAD_RIGHT),
                0.0,
            ],
            // XInput's y points up.
            axes: [axis(g.sThumbLX), -axis(g.sThumbLY), axis(g.sThumbRX), -axis(g.sThumbRY)],
        });
    }
    out
}

/// Controllers that aren't standard pads, in the standard layout: a DualShock 4 / DualSense as
/// Chromium lays them out (buttons square, cross, circle, triangle…; axes LX, LY, RX, L2,
/// R2, RY; the hat for the d-pad), anything else button for button.
#[cfg(windows)]
fn raw_controllers() -> Vec<State> {
    use windows::Gaming::Input::{GameControllerSwitchPosition as Hat, Gamepad, IGameController, RawGameController};
    use windows::core::Interface;
    watch_raw_controllers();
    let Ok(list) = RawGameController::RawGameControllers() else { return Vec::new() };
    let mut found = Vec::new();
    for raw in list.into_iter() {
        // Standard pads came through as Gamepads already.
        if raw.cast::<IGameController>().ok().is_some_and(|g| Gamepad::FromGameController(&g).is_ok()) {
            continue;
        }
        let (nb, na, ns) = (raw.ButtonCount().unwrap_or(0).max(0) as usize, raw.AxisCount().unwrap_or(0).max(0) as usize, raw.SwitchCount().unwrap_or(0).max(0) as usize);
        let (mut b, mut a, mut sw) = (vec![false; nb], vec![0.5f64; na], vec![Hat::Center; ns]);
        if raw.GetCurrentReading(&mut b, &mut sw, &mut a).is_err() {
            continue;
        }
        let bt = |i: usize| if b.get(i).copied().unwrap_or(false) { 1.0 } else { 0.0 };
        // Raw axes are 0..1; sticks are -1..1, triggers 0..1.
        let stick = |i: usize| a.get(i).map_or(0.0, |v| (*v as f32 * 2.0 - 1.0).clamp(-1.0, 1.0));
        let trigger = |i: usize| a.get(i).map_or(0.0, |v| (*v as f32).clamp(0.0, 1.0));
        let hat = sw.first().copied().unwrap_or(Hat::Center);
        let dir = |hs: [Hat; 3]| if hs.contains(&hat) { 1.0 } else { 0.0 };
        let sony = raw.HardwareVendorId().unwrap_or(0) == 0x054C;
        let state = if sony {
            State {
                buttons: [
                    bt(1),
                    bt(2),
                    bt(0),
                    bt(3),
                    bt(4),
                    bt(5),
                    trigger(3),
                    trigger(4),
                    bt(8),
                    bt(9),
                    bt(10),
                    bt(11),
                    dir([Hat::Up, Hat::UpLeft, Hat::UpRight]),
                    dir([Hat::Down, Hat::DownLeft, Hat::DownRight]),
                    dir([Hat::Left, Hat::UpLeft, Hat::DownLeft]),
                    dir([Hat::Right, Hat::UpRight, Hat::DownRight]),
                    bt(12),
                ],
                axes: [stick(0), stick(1), stick(2), stick(5)],
            }
        } else {
            let mut buttons = [0.0; BUTTONS];
            for (i, slot) in buttons.iter_mut().enumerate().take(12) {
                *slot = bt(i);
            }
            buttons[12] = dir([Hat::Up, Hat::UpLeft, Hat::UpRight]);
            buttons[13] = dir([Hat::Down, Hat::DownLeft, Hat::DownRight]);
            buttons[14] = dir([Hat::Left, Hat::UpLeft, Hat::DownLeft]);
            buttons[15] = dir([Hat::Right, Hat::UpRight, Hat::DownRight]);
            buttons[16] = bt(12);
            State { buttons, axes: [stick(0), stick(1), stick(2), stick(3)] }
        };
        found.push(state);
    }
    found
}

/// Windows.Gaming.Input fills its list of controllers only once something listens for them
/// arriving (in a desktop app): listen, for as long as the process runs.
#[cfg(windows)]
fn watch_raw_controllers() {
    use windows::Foundation::EventHandler;
    use windows::Gaming::Input::RawGameController;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let added = EventHandler::<RawGameController>::new(|_, _| Ok(()));
        if let Ok(token) = RawGameController::RawGameControllerAdded(&added) {
            std::mem::forget(token);
        }
    });
}

/// The controllers Windows.Gaming.Input lists (for `glass viewer-gamepad`).
#[cfg(windows)]
pub fn describe() -> String {
    use windows::Gaming::Input::RawGameController;
    watch_raw_controllers();
    match RawGameController::RawGameControllers() {
        Ok(list) => list
            .into_iter()
            .map(|r| {
                use windows::Gaming::Input::{GameControllerSwitchPosition as Hat, Gamepad, IGameController};
                use windows::core::Interface;
                let gamepad = r.cast::<IGameController>().map(|g| Gamepad::FromGameController(&g).map(|_| ()));
                let (nb, na, ns) = (r.ButtonCount().unwrap_or(0).max(0) as usize, r.AxisCount().unwrap_or(0).max(0) as usize, r.SwitchCount().unwrap_or(0).max(0) as usize);
                let (mut b, mut a, mut sw) = (vec![false; nb], vec![0.0f64; na], vec![Hat::Center; ns]);
                let reading = r.GetCurrentReading(&mut b, &mut sw, &mut a);
                format!(
                    "{:04x}:{:04x} {nb} buttons, {na} axes, {ns} switches; as a Gamepad: {gamepad:?}; reading: {:?} axes {a:?}",
                    r.HardwareVendorId().unwrap_or(0),
                    r.HardwareProductId().unwrap_or(0),
                    reading.map(|_| ()),
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
        Err(e) => format!("RawGameControllers: {e}"),
    }
}

#[cfg(target_os = "macos")]
pub fn describe() -> String {
    String::new()
}

/// What each pad last sent, so only changes go.
#[derive(Default)]
pub struct Pads {
    last: [Option<State>; MAX_PADS],
}

impl Pads {
    /// Whether any controller is connected (no need to poll fast otherwise).
    pub fn any() -> bool {
        read_all().iter().any(Option::is_some)
    }

    /// Sends what changed since last time (call it a hundred or so times a second).
    pub fn poll(&mut self, send: &dyn Fn(InputEvent)) {
        let now_all = read_all();
        for (i, now) in now_all.into_iter().enumerate() {
            diff(i as u8, self.last[i], now, send);
            self.last[i] = now;
        }
    }
}

fn diff(pad: u8, last: Option<State>, now: Option<State>, send: &dyn Fn(InputEvent)) {
    match (last, now) {
        (None, Some(_)) => send(InputEvent::Gamepad { pad, kind: GAMEPAD_CONNECTED, index: 0, value: 1.0 }),
        (Some(_), None) => send(InputEvent::Gamepad { pad, kind: GAMEPAD_CONNECTED, index: 0, value: 0.0 }),
        _ => {}
    }
    let Some(now) = now else { return };
    let before = last.unwrap_or(State { buttons: [0.0; BUTTONS], axes: [0.0; AXES] });
    for (b, (&x, &was)) in now.buttons.iter().zip(&before.buttons).enumerate() {
        if (x - was).abs() > 0.01 || (last.is_none() && x != 0.0) {
            send(InputEvent::Gamepad { pad, kind: GAMEPAD_BUTTON, index: b as u16, value: x });
        }
    }
    for (a, (&x, &was)) in now.axes.iter().zip(&before.axes).enumerate() {
        // Bigger than a resting stick's drift (a step or two of 8 bits).
        if (x - was).abs() > 0.012 {
            send(InputEvent::Gamepad { pad, kind: GAMEPAD_AXIS, index: a as u16, value: x });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_changes_go() {
        let sent = std::cell::RefCell::new(Vec::new());
        let send = |e: InputEvent| sent.borrow_mut().push(e);
        let mut a = State { buttons: [0.0; BUTTONS], axes: [0.0; AXES] };
        diff(0, None, Some(a), &send);
        assert_eq!(sent.take(), [InputEvent::Gamepad { pad: 0, kind: GAMEPAD_CONNECTED, index: 0, value: 1.0 }]);
        a.buttons[0] = 1.0;
        a.axes[1] = 0.5;
        let b = State { buttons: [0.0; BUTTONS], axes: [0.0; AXES] };
        diff(0, Some(b), Some(a), &send);
        assert_eq!(sent.take(), [InputEvent::Gamepad { pad: 0, kind: GAMEPAD_BUTTON, index: 0, value: 1.0 }, InputEvent::Gamepad { pad: 0, kind: GAMEPAD_AXIS, index: 1, value: 0.5 }]);
        diff(0, Some(a), Some(a), &send);
        assert!(sent.take().is_empty());
        diff(0, Some(a), None, &send);
        assert_eq!(sent.take(), [InputEvent::Gamepad { pad: 0, kind: GAMEPAD_CONNECTED, index: 0, value: 0.0 }]);
    }
}
