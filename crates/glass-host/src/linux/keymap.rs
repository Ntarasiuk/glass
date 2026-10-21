//! Clients send macOS virtual keycodes (kVK_*); map them to Linux evdev KEY_* codes.
//! Physical positions match: ⌘ is the Super key, ⌥ is Alt.

/// Returns the evdev key code (input-event-codes.h) for a kVK code.
pub fn to_evdev(kvk: u16) -> Option<u16> {
    Some(match kvk {
        0x00 => 30,  // A
        0x01 => 31,  // S
        0x02 => 32,  // D
        0x03 => 33,  // F
        0x04 => 35,  // H
        0x05 => 34,  // G
        0x06 => 44,  // Z
        0x07 => 45,  // X
        0x08 => 46,  // C
        0x09 => 47,  // V
        0x0a => 86,  // ISO section → KEY_102ND
        0x0b => 48,  // B
        0x0c => 16,  // Q
        0x0d => 17,  // W
        0x0e => 18,  // E
        0x0f => 19,  // R
        0x10 => 21,  // Y
        0x11 => 20,  // T
        0x12 => 2,   // 1
        0x13 => 3,   // 2
        0x14 => 4,   // 3
        0x15 => 5,   // 4
        0x16 => 7,   // 6
        0x17 => 6,   // 5
        0x18 => 13,  // =
        0x19 => 10,  // 9
        0x1a => 8,   // 7
        0x1b => 12,  // -
        0x1c => 9,   // 8
        0x1d => 11,  // 0
        0x1e => 27,  // ]
        0x1f => 24,  // O
        0x20 => 22,  // U
        0x21 => 26,  // [
        0x22 => 23,  // I
        0x23 => 25,  // P
        0x24 => 28,  // Return
        0x25 => 38,  // L
        0x26 => 36,  // J
        0x27 => 40,  // '
        0x28 => 37,  // K
        0x29 => 39,  // ;
        0x2a => 43,  // \
        0x2b => 51,  // ,
        0x2c => 53,  // /
        0x2d => 49,  // N
        0x2e => 50,  // M
        0x2f => 52,  // .
        0x30 => 15,  // Tab
        0x31 => 57,  // Space
        0x32 => 41,  // `
        0x33 => 14,  // Backspace
        0x35 => 1,   // Escape
        0x36 => 126, // Right Command → KEY_RIGHTMETA
        0x37 => 125, // Command → KEY_LEFTMETA
        0x38 => 42,  // Shift
        0x39 => 58,  // Caps Lock
        0x3a => 56,  // Option → KEY_LEFTALT
        0x3b => 29,  // Control
        0x3c => 54,  // Right Shift
        0x3d => 100, // Right Option → KEY_RIGHTALT
        0x3e => 97,  // Right Control
        0x40 => 187, // F17
        0x41 => 83,  // Keypad .
        0x43 => 55,  // Keypad *
        0x45 => 78,  // Keypad +
        0x47 => 69,  // Clear → KEY_NUMLOCK
        0x48 => 115, // Volume Up
        0x49 => 114, // Volume Down
        0x4a => 113, // Mute
        0x4b => 98,  // Keypad /
        0x4c => 96,  // Keypad Enter
        0x4e => 74,  // Keypad -
        0x4f => 188, // F18
        0x50 => 189, // F19
        0x51 => 117, // Keypad =
        0x52 => 82,  // Keypad 0
        0x53 => 79,  // Keypad 1
        0x54 => 80,  // Keypad 2
        0x55 => 81,  // Keypad 3
        0x56 => 75,  // Keypad 4
        0x57 => 76,  // Keypad 5
        0x58 => 77,  // Keypad 6
        0x59 => 71,  // Keypad 7
        0x5a => 190, // F20
        0x5b => 72,  // Keypad 8
        0x5c => 73,  // Keypad 9
        0x60 => 63,  // F5
        0x61 => 64,  // F6
        0x62 => 65,  // F7
        0x63 => 61,  // F3
        0x64 => 66,  // F8
        0x65 => 67,  // F9
        0x67 => 87,  // F11
        0x69 => 183, // F13
        0x6a => 186, // F16
        0x6b => 184, // F14
        0x6d => 68,  // F10
        0x6f => 88,  // F12
        0x71 => 185, // F15
        0x72 => 110, // Help/Insert
        0x73 => 102, // Home
        0x74 => 104, // Page Up
        0x75 => 111, // Forward Delete
        0x76 => 62,  // F4
        0x77 => 107, // End
        0x78 => 60,  // F2
        0x79 => 109, // Page Down
        0x7a => 59,  // F1
        0x7b => 105, // Left
        0x7c => 106, // Right
        0x7d => 108, // Down
        0x7e => 103, // Up
        _ => return None,
    })
}

/// Every evdev code `to_evdev` can produce, for registering the virtual keyboard.
pub fn all_evdev() -> impl Iterator<Item = u16> {
    (0u16..=0x7e).filter_map(to_evdev)
}

pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_V: u16 = 47;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_modifiers() {
        assert_eq!(to_evdev(0x00), Some(30)); // A
        assert_eq!(to_evdev(0x37), Some(KEY_LEFTMETA));
        assert_eq!(to_evdev(0x3b), Some(KEY_LEFTCTRL));
        assert_eq!(to_evdev(0x34), None);
        // No two kVK codes map to the same key.
        let mut seen: Vec<u16> = all_evdev().collect();
        let n = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), n);
    }
}
