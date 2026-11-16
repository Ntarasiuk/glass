//! Keys for the wire, which carries macOS virtual keycodes (kVK_*). With the event tap
//! (`capture`) they're the Mac's own; this maps GPUI's key names for when there's no tap
//! (no Accessibility permission yet), assuming a US layout like the browser's `code`.

/// kVK_* for a GPUI key name (`Keystroke::key`).
pub fn keycode(key: &str) -> Option<u16> {
    Some(match key {
        "a" => 0x00,
        "s" => 0x01,
        "d" => 0x02,
        "f" => 0x03,
        "h" => 0x04,
        "g" => 0x05,
        "z" => 0x06,
        "x" => 0x07,
        "c" => 0x08,
        "v" => 0x09,
        "b" => 0x0B,
        "q" => 0x0C,
        "w" => 0x0D,
        "e" => 0x0E,
        "r" => 0x0F,
        "y" => 0x10,
        "t" => 0x11,
        "1" | "!" => 0x12,
        "2" | "@" => 0x13,
        "3" | "#" => 0x14,
        "4" | "$" => 0x15,
        "6" | "^" => 0x16,
        "5" | "%" => 0x17,
        "=" | "+" => 0x18,
        "9" | "(" => 0x19,
        "7" | "&" => 0x1A,
        "-" | "_" => 0x1B,
        "8" | "*" => 0x1C,
        "0" | ")" => 0x1D,
        "]" | "}" => 0x1E,
        "o" => 0x1F,
        "u" => 0x20,
        "[" | "{" => 0x21,
        "i" => 0x22,
        "p" => 0x23,
        "enter" => 0x24,
        "l" => 0x25,
        "j" => 0x26,
        "'" | "\"" => 0x27,
        "k" => 0x28,
        ";" | ":" => 0x29,
        "\\" | "|" => 0x2A,
        "," | "<" => 0x2B,
        "/" | "?" => 0x2C,
        "n" => 0x2D,
        "m" => 0x2E,
        "." | ">" => 0x2F,
        "tab" => 0x30,
        "space" => 0x31,
        "`" | "~" => 0x32,
        "backspace" => 0x33,
        "escape" => 0x35,
        "f1" => 0x7A,
        "f2" => 0x78,
        "f3" => 0x63,
        "f4" => 0x76,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f8" => 0x64,
        "f9" => 0x65,
        "f10" => 0x6D,
        "f11" => 0x67,
        "f12" => 0x6F,
        "home" => 0x73,
        "pageup" => 0x74,
        "delete" => 0x75,
        "end" => 0x77,
        "pagedown" => 0x79,
        "left" => 0x7B,
        "right" => 0x7C,
        "down" => 0x7D,
        "up" => 0x7E,
        _ => return None,
    })
}

/// The key (and whether Shift is needed) that types a character on a US layout.
pub fn for_char(c: char) -> Option<(u16, bool)> {
    const SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";
    const PLAIN: &str = "`1234567890-=[]\\;',./";
    if c == '\n' {
        return Some((0x24, false));
    }
    if c == '\t' {
        return Some((0x30, false));
    }
    if c == ' ' {
        return Some((0x31, false));
    }
    if c.is_ascii_uppercase() {
        return keycode(&c.to_ascii_lowercase().to_string()).map(|k| (k, true));
    }
    if let Some(i) = SHIFTED.find(c) {
        let plain = PLAIN.chars().nth(i)?;
        return keycode(&plain.to_string()).map(|k| (k, true));
    }
    keycode(&c.to_string()).map(|k| (k, false))
}

pub const COMMAND: u16 = 0x37;
pub const SHIFT: u16 = 0x38;
pub const CAPS_LOCK: u16 = 0x39;
pub const OPTION: u16 = 0x3A;
pub const CONTROL: u16 = 0x3B;
pub const RIGHT_SHIFT: u16 = 0x3C;
pub const RIGHT_OPTION: u16 = 0x3D;
pub const RIGHT_CONTROL: u16 = 0x3E;
pub const FUNCTION: u16 = 0x3F;
pub const RIGHT_COMMAND: u16 = 0x36;
pub const ESCAPE: u16 = 0x35;

/// ⌘ for Control and back, either side.
pub fn swapped(keycode: u16) -> u16 {
    match keycode {
        COMMAND => CONTROL,
        CONTROL => COMMAND,
        RIGHT_COMMAND => RIGHT_CONTROL,
        RIGHT_CONTROL => RIGHT_COMMAND,
        k => k,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_shifted_symbols_share_keys() {
        assert_eq!(keycode("a"), Some(0));
        assert_eq!(keycode("1"), keycode("!"));
        assert_eq!(keycode("enter"), Some(0x24));
        assert_eq!(keycode("unknown"), None);
        assert_eq!(for_char('A'), Some((0x00, true)));
        assert_eq!(for_char('!'), Some((0x12, true)));
        assert_eq!(for_char('?'), Some((0x2C, true)));
        assert_eq!(for_char('\n'), Some((0x24, false)));
    }
}
