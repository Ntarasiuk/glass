//! The Windows port's arithmetic and naming, with no Win32 calls, so its tests run on any
//! OS: an app's id from its executable, UI Automation control types as the short roles
//! snapshots show, and a pen frame's flags, pressure and tilt.

/// An app's id, what grants and protected apps name it by: the executable's file name
/// without `.exe`, lowercase ("windowsterminal"). Stable, unlike its localized description.
pub fn app_id(exe_path: &str) -> Option<String> {
    let file = exe_path.rsplit(['\\', '/']).next()?;
    let stem = match file.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("exe") => stem,
        _ => file,
    };
    (!stem.is_empty()).then(|| stem.to_lowercase())
}

/// When a process the service keeps running may be started again: at once if it had been up a
/// while, later and later if it keeps exiting straight away. One that can't start (its pipe
/// still taken, say) would otherwise be started again the moment it exits, over and over.
#[derive(Debug, Default)]
pub struct Respawn {
    started: Option<std::time::Instant>,
    failures: u32,
    not_before: Option<std::time::Instant>,
}

/// Up this long, it wasn't failing to start.
const RESPAWN_PROVEN: std::time::Duration = std::time::Duration::from_secs(30);
const RESPAWN_MAX_SECS: u64 = 30;

impl Respawn {
    pub fn started(&mut self, now: std::time::Instant) {
        self.started = Some(now);
    }

    /// It exited by itself; how long until it may be started again.
    pub fn exited(&mut self, now: std::time::Instant) -> std::time::Duration {
        let ran = self.started.take().map_or(std::time::Duration::ZERO, |at| now.duration_since(at));
        self.failures = if ran >= RESPAWN_PROVEN { 0 } else { self.failures.saturating_add(1) };
        let wait = match self.failures {
            0 => std::time::Duration::ZERO,
            n => std::time::Duration::from_secs((1u64 << n.min(6)).min(RESPAWN_MAX_SECS)),
        };
        self.not_before = Some(now + wait);
        wait
    }

    pub fn may_start(&self, now: std::time::Instant) -> bool {
        self.not_before.is_none_or(|at| now >= at)
    }
}

/// What the input hooks' helper needs now, from whether it's running, what it last said in
/// its shared memory (1 hooks in, 2 refused) and whether it's time to try again.
#[derive(Debug, PartialEq, Eq)]
pub enum HookStep {
    Ready,
    /// Running, its hooks not in yet.
    Waiting,
    Start,
    /// Not working, and not time to try again.
    Down,
}

pub fn hook_step(running: bool, ready: u64, retry_due: bool) -> HookStep {
    match (running && ready != 2, ready) {
        (true, 1) => HookStep::Ready,
        (true, _) => HookStep::Waiting,
        (false, _) if retry_due => HookStep::Start,
        (false, _) => HookStep::Down,
    }
}

/// UIA_*ControlTypeId values (UIAutomationClient.h).
pub mod control {
    pub const BUTTON: i32 = 50000;
    pub const CALENDAR: i32 = 50001;
    pub const CHECK_BOX: i32 = 50002;
    pub const COMBO_BOX: i32 = 50003;
    pub const EDIT: i32 = 50004;
    pub const HYPERLINK: i32 = 50005;
    pub const IMAGE: i32 = 50006;
    pub const LIST_ITEM: i32 = 50007;
    pub const LIST: i32 = 50008;
    pub const MENU: i32 = 50009;
    pub const MENU_BAR: i32 = 50010;
    pub const MENU_ITEM: i32 = 50011;
    pub const PROGRESS_BAR: i32 = 50012;
    pub const RADIO_BUTTON: i32 = 50013;
    pub const SCROLL_BAR: i32 = 50014;
    pub const SLIDER: i32 = 50015;
    pub const SPINNER: i32 = 50016;
    pub const STATUS_BAR: i32 = 50017;
    pub const TAB: i32 = 50018;
    pub const TAB_ITEM: i32 = 50019;
    pub const TEXT: i32 = 50020;
    pub const TOOL_BAR: i32 = 50021;
    pub const TOOL_TIP: i32 = 50022;
    pub const TREE: i32 = 50023;
    pub const TREE_ITEM: i32 = 50024;
    pub const CUSTOM: i32 = 50025;
    pub const GROUP: i32 = 50026;
    pub const THUMB: i32 = 50027;
    pub const DATA_GRID: i32 = 50028;
    pub const DATA_ITEM: i32 = 50029;
    pub const DOCUMENT: i32 = 50030;
    pub const SPLIT_BUTTON: i32 = 50031;
    pub const WINDOW: i32 = 50032;
    pub const PANE: i32 = 50033;
    pub const HEADER: i32 = 50034;
    pub const HEADER_ITEM: i32 = 50035;
    pub const TABLE: i32 = 50036;
    pub const TITLE_BAR: i32 = 50037;
    pub const SEPARATOR: i32 = 50038;
    pub const SEMANTIC_ZOOM: i32 = 50039;
    pub const APP_BAR: i32 = 50040;
}

/// The short role agents see, in the same words as on macOS where there's a match:
/// "button", "textfield", "passwordfield", "checkbox", "link", "text".
pub fn short_role(control_type: i32, password: bool) -> &'static str {
    use control::*;
    match control_type {
        EDIT if password => "passwordfield",
        BUTTON => "button",
        CALENDAR => "calendar",
        CHECK_BOX => "checkbox",
        COMBO_BOX => "combobox",
        EDIT => "textfield",
        HYPERLINK => "link",
        IMAGE => "image",
        LIST_ITEM => "listitem",
        LIST => "list",
        MENU => "menu",
        MENU_BAR => "menubar",
        MENU_ITEM => "menuitem",
        PROGRESS_BAR => "progressbar",
        RADIO_BUTTON => "radio",
        SCROLL_BAR => "scrollbar",
        SLIDER => "slider",
        SPINNER => "spinner",
        STATUS_BAR => "statusbar",
        TAB => "tabgroup",
        TAB_ITEM => "tab",
        TEXT => "text",
        TOOL_BAR | APP_BAR => "toolbar",
        TOOL_TIP => "tooltip",
        TREE => "tree",
        TREE_ITEM => "treeitem",
        GROUP => "group",
        THUMB => "thumb",
        DATA_GRID | TABLE => "table",
        DATA_ITEM => "row",
        DOCUMENT => "document",
        SPLIT_BUTTON => "splitbutton",
        WINDOW => "window",
        PANE | SEMANTIC_ZOOM => "pane",
        HEADER => "header",
        HEADER_ITEM => "headeritem",
        TITLE_BAR => "titlebar",
        SEPARATOR => "separator",
        _ => "custom",
    }
}

/// Control types an agent can act on whatever patterns they offer.
pub fn interactive(control_type: i32) -> bool {
    use control::*;
    matches!(
        control_type,
        BUTTON | CHECK_BOX | COMBO_BOX | HYPERLINK | LIST_ITEM | MENU_ITEM | RADIO_BUTTON | SLIDER | SPINNER
            | TAB_ITEM | TREE_ITEM | SPLIT_BUTTON | DATA_ITEM | HEADER_ITEM
    )
}

/// Control types whose value is text someone can replace (when it isn't read-only).
pub fn editable(control_type: i32) -> bool {
    use control::*;
    matches!(control_type, EDIT | COMBO_BOX | DOCUMENT)
}

/// Unnamed containers that do nothing: scaffolding, not counted against the depth limit.
pub fn scaffolding(control_type: i32) -> bool {
    use control::*;
    matches!(control_type, GROUP | PANE | CUSTOM)
}

/// POINTER_FLAG_* and POINTER_CHANGE_* (WinUser.h).
pub mod pointer {
    pub const IN_RANGE: u32 = 0x2;
    pub const IN_CONTACT: u32 = 0x4;
    pub const FIRST_BUTTON: u32 = 0x10;
    pub const DOWN: u32 = 0x1_0000;
    pub const UPDATE: u32 = 0x2_0000;
    pub const UP: u32 = 0x4_0000;
    pub const CHANGE_NONE: i32 = 0;
    pub const CHANGE_FIRST_DOWN: i32 = 1;
    pub const CHANGE_FIRST_UP: i32 = 2;
}

/// Where a pen frame falls in a stroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenPhase {
    /// Near the screen, not touching it.
    Hover,
    Down,
    /// Moving while touching.
    Drag,
    Up,
    /// Out of range: the pen leaves.
    Leave,
}

/// One injected pen frame: what InjectSyntheticPointerInput's POINTER_PEN_INFO carries
/// beyond the position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PenFrame {
    pub flags: u32,
    pub button_change: i32,
    /// 0..1024; at least 1 while touching (as on macOS, a touch always has some).
    pub pressure: u32,
    /// Degrees, -90..90.
    pub tilt_x: i32,
    pub tilt_y: i32,
    /// Degrees, 0..359.
    pub rotation: u32,
}

pub fn pen_frame(phase: PenPhase, pressure: f32, tilt: (i8, i8), twist: f32) -> PenFrame {
    use pointer::*;
    let (flags, button_change) = match phase {
        PenPhase::Hover => (IN_RANGE | UPDATE, CHANGE_NONE),
        PenPhase::Down => (IN_RANGE | IN_CONTACT | FIRST_BUTTON | DOWN, CHANGE_FIRST_DOWN),
        PenPhase::Drag => (IN_RANGE | IN_CONTACT | FIRST_BUTTON | UPDATE, CHANGE_NONE),
        PenPhase::Up => (IN_RANGE | UP, CHANGE_FIRST_UP),
        PenPhase::Leave => (UPDATE, CHANGE_NONE),
    };
    let touching = matches!(phase, PenPhase::Down | PenPhase::Drag);
    let pressure = if touching {
        let p = if pressure.is_finite() { pressure.clamp(0.0, 1.0) } else { 0.0 };
        ((p * 1024.0).round() as u32).max(1)
    } else {
        0
    };
    let twist = if twist.is_finite() { twist } else { 0.0 };
    PenFrame {
        flags,
        button_change,
        pressure,
        tilt_x: (tilt.0 as i32).clamp(-90, 90),
        tilt_y: (tilt.1 as i32).clamp(-90, 90),
        rotation: (twist.round() as i32).rem_euclid(360) as u32,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    #[test]
    fn a_process_that_keeps_exiting_is_started_less_and_less() {
        let t = Instant::now();
        let mut r = super::Respawn::default();
        assert!(r.may_start(t));
        r.started(t);
        assert_eq!(r.exited(t + Duration::from_millis(50)), Duration::from_secs(2));
        assert!(!r.may_start(t + Duration::from_secs(1)));
        assert!(r.may_start(t + Duration::from_secs(3)));
        r.started(t);
        assert_eq!(r.exited(t), Duration::from_secs(4));
        for _ in 0..10 {
            r.started(t);
            r.exited(t);
        }
        r.started(t);
        assert_eq!(r.exited(t), Duration::from_secs(30), "at most half a minute");
    }

    #[test]
    fn a_process_that_ran_a_while_is_started_again_at_once() {
        let t = Instant::now();
        let mut r = super::Respawn::default();
        r.started(t);
        r.exited(t);
        r.started(t);
        assert_eq!(r.exited(t + Duration::from_secs(60)), Duration::ZERO);
        assert!(r.may_start(t + Duration::from_secs(60)));
    }

    #[test]
    fn the_hook_helper_is_started_again_when_it_dies_or_is_refused() {
        use super::{HookStep::*, hook_step};
        assert_eq!(hook_step(true, 1, false), Ready);
        assert_eq!(hook_step(true, 0, true), Waiting, "a slow start isn't a failure");
        assert_eq!(hook_step(false, 1, true), Start, "it had hooks, and died");
        assert_eq!(hook_step(false, 0, true), Start, "never started");
        assert_eq!(hook_step(true, 2, true), Start, "refused");
        assert_eq!(hook_step(false, 1, false), Down);
    }

    use super::*;

    #[test]
    fn app_ids_are_the_lowercase_exe_stem() {
        assert_eq!(app_id(r"C:\Program Files\WindowsApps\Microsoft.WindowsTerminal\WindowsTerminal.exe").as_deref(), Some("windowsterminal"));
        assert_eq!(app_id(r"C:\Windows\System32\cmd.EXE").as_deref(), Some("cmd"));
        assert_eq!(app_id(r"C:\tools\my.app.exe").as_deref(), Some("my.app"));
        assert_eq!(app_id(r"C:\tools\noext").as_deref(), Some("noext"));
        assert_eq!(app_id(r"C:\dir\"), None);
    }

    #[test]
    fn roles_read_like_the_mac_ones() {
        assert_eq!(short_role(control::EDIT, false), "textfield");
        assert_eq!(short_role(control::EDIT, true), "passwordfield");
        assert_eq!(short_role(control::RADIO_BUTTON, false), "radio");
        assert_eq!(short_role(control::TAB_ITEM, false), "tab");
        assert_eq!(short_role(control::TEXT, false), "text");
        assert_eq!(short_role(1234, false), "custom");
        assert!(interactive(control::BUTTON) && !interactive(control::TEXT));
        assert!(editable(control::EDIT) && !editable(control::BUTTON));
    }

    #[test]
    fn a_stroke_has_pressure_only_while_touching() {
        let hover = pen_frame(PenPhase::Hover, 0.7, (10, -20), 30.0);
        assert_eq!((hover.pressure, hover.flags), (0, pointer::IN_RANGE | pointer::UPDATE));
        let down = pen_frame(PenPhase::Down, 0.5, (10, -20), 30.0);
        assert_eq!((down.pressure, down.button_change), (512, pointer::CHANGE_FIRST_DOWN));
        assert!(down.flags & pointer::IN_CONTACT != 0 && down.flags & pointer::DOWN != 0);
        assert_eq!(pen_frame(PenPhase::Drag, 0.0, (0, 0), 0.0).pressure, 1);
        assert_eq!(pen_frame(PenPhase::Drag, 2.0, (0, 0), 0.0).pressure, 1024);
        assert_eq!(pen_frame(PenPhase::Drag, f32::NAN, (0, 0), 0.0).pressure, 1);
        let up = pen_frame(PenPhase::Up, 0.5, (0, 0), 0.0);
        assert_eq!((up.pressure, up.flags & pointer::IN_CONTACT), (0, 0));
        assert_eq!(pen_frame(PenPhase::Leave, 0.0, (0, 0), 0.0).flags & pointer::IN_RANGE, 0);
    }

    #[test]
    fn tilt_and_twist_stay_in_range() {
        let f = pen_frame(PenPhase::Drag, 0.5, (-128, 127), -90.0);
        assert_eq!((f.tilt_x, f.tilt_y, f.rotation), (-90, 90, 270));
        assert_eq!(pen_frame(PenPhase::Drag, 0.5, (45, -45), 359.6).rotation, 0);
        assert_eq!(pen_frame(PenPhase::Hover, 0.5, (45, -45), 720.0).rotation, 0);
    }
}
