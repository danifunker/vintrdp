//! Native key codes → PC/AT set-1 scancodes, which RDP sends.
//!
//! Each function returns `(scancode, extended)`; `extended` means the key is
//! E0-prefixed (KBD_FLAG_EXT). Mapping by physical position lets the server
//! apply its own keyboard layout. Windows front-ends don't need a table:
//! WM_KEYDOWN's lParam already carries the scancode and extended bit.

pub type Scancode = (u8, bool);

const fn sc(code: u8) -> Option<Scancode> {
    Some((code, false))
}
const fn ext(code: u8) -> Option<Scancode> {
    Some((code, true))
}

pub const LSHIFT: Scancode = (0x2a, false);
pub const RSHIFT: Scancode = (0x36, false);
pub const LCTRL: Scancode = (0x1d, false);
pub const RCTRL: Scancode = (0x1d, true);
pub const LALT: Scancode = (0x38, false);
pub const RALT: Scancode = (0x38, true);
pub const LWIN: Scancode = (0x5b, true);
pub const RWIN: Scancode = (0x5c, true);
pub const CAPS_LOCK: Scancode = (0x3a, false);

/// macOS virtual key code (`NSEvent.keyCode`, kVK_* in Carbon's Events.h).
///
/// Command maps to Ctrl so the usual shortcuts (Cmd-C, Cmd-V, ...) do what a
/// Mac user expects on the remote side; Control also maps to Ctrl.
pub fn from_mac_keycode(code: u16) -> Option<Scancode> {
    match code {
        0x00 => sc(0x1e), // A
        0x01 => sc(0x1f), // S
        0x02 => sc(0x20), // D
        0x03 => sc(0x21), // F
        0x04 => sc(0x23), // H
        0x05 => sc(0x22), // G
        0x06 => sc(0x2c), // Z
        0x07 => sc(0x2d), // X
        0x08 => sc(0x2e), // C
        0x09 => sc(0x2f), // V
        0x0a => sc(0x56), // ISO §/< key
        0x0b => sc(0x30), // B
        0x0c => sc(0x10), // Q
        0x0d => sc(0x11), // W
        0x0e => sc(0x12), // E
        0x0f => sc(0x13), // R
        0x10 => sc(0x15), // Y
        0x11 => sc(0x14), // T
        0x12 => sc(0x02), // 1
        0x13 => sc(0x03), // 2
        0x14 => sc(0x04), // 3
        0x15 => sc(0x05), // 4
        0x16 => sc(0x07), // 6
        0x17 => sc(0x06), // 5
        0x18 => sc(0x0d), // =
        0x19 => sc(0x0a), // 9
        0x1a => sc(0x08), // 7
        0x1b => sc(0x0c), // -
        0x1c => sc(0x09), // 8
        0x1d => sc(0x0b), // 0
        0x1e => sc(0x1b), // ]
        0x1f => sc(0x18), // O
        0x20 => sc(0x16), // U
        0x21 => sc(0x1a), // [
        0x22 => sc(0x17), // I
        0x23 => sc(0x19), // P
        0x24 => sc(0x1c), // Return
        0x25 => sc(0x26), // L
        0x26 => sc(0x24), // J
        0x27 => sc(0x28), // '
        0x28 => sc(0x25), // K
        0x29 => sc(0x27), // ;
        0x2a => sc(0x2b), // backslash
        0x2b => sc(0x33), // ,
        0x2c => sc(0x35), // /
        0x2d => sc(0x31), // N
        0x2e => sc(0x32), // M
        0x2f => sc(0x34), // .
        0x30 => sc(0x0f), // Tab
        0x31 => sc(0x39), // Space
        0x32 => sc(0x29), // `
        0x33 => sc(0x0e), // Backspace
        0x35 => sc(0x01), // Escape
        0x36 => Some(RCTRL), // Right Command
        0x37 => Some(LCTRL), // Command
        0x38 => Some(LSHIFT),
        0x39 => Some(CAPS_LOCK),
        0x3a => Some(LALT), // Option
        0x3b => Some(LCTRL), // Control
        0x3c => Some(RSHIFT),
        0x3d => Some(RALT),
        0x3e => Some(RCTRL),
        0x40 => sc(0x68),  // F17
        0x41 => sc(0x53),  // keypad .
        0x43 => sc(0x37),  // keypad *
        0x45 => sc(0x4e),  // keypad +
        0x47 => sc(0x45),  // keypad Clear -> Num Lock
        0x4b => ext(0x35), // keypad /
        0x4c => ext(0x1c), // keypad Enter
        0x4e => sc(0x4a),  // keypad -
        0x4f => sc(0x69),  // F18
        0x50 => sc(0x6a),  // F19
        0x52 => sc(0x52),  // keypad 0
        0x53 => sc(0x4f),  // keypad 1
        0x54 => sc(0x50),  // keypad 2
        0x55 => sc(0x51),  // keypad 3
        0x56 => sc(0x4b),  // keypad 4
        0x57 => sc(0x4c),  // keypad 5
        0x58 => sc(0x4d),  // keypad 6
        0x59 => sc(0x47),  // keypad 7
        0x5b => sc(0x48),  // keypad 8
        0x5c => sc(0x49),  // keypad 9
        0x5d => sc(0x7d),  // JIS Yen
        0x5e => sc(0x73),  // JIS underscore (Ro)
        0x60 => sc(0x3f),  // F5
        0x61 => sc(0x40),  // F6
        0x62 => sc(0x41),  // F7
        0x63 => sc(0x3d),  // F3
        0x64 => sc(0x42),  // F8
        0x65 => sc(0x43),  // F9
        0x67 => sc(0x57),  // F11
        0x68 => sc(0x70),  // JIS Kana
        0x69 => sc(0x64),  // F13
        0x6a => sc(0x67),  // F16
        0x6b => sc(0x65),  // F14
        0x6d => sc(0x44),  // F10
        0x6f => sc(0x58),  // F12
        0x71 => sc(0x66),  // F15
        0x72 => ext(0x52), // Help -> Insert
        0x73 => ext(0x47), // Home
        0x74 => ext(0x49), // Page Up
        0x75 => ext(0x53), // Forward Delete
        0x76 => sc(0x3e),  // F4
        0x77 => ext(0x4f), // End
        0x78 => sc(0x3c),  // F2
        0x79 => ext(0x51), // Page Down
        0x7a => sc(0x3b),  // F1
        0x7b => ext(0x4b), // Left
        0x7c => ext(0x4d), // Right
        0x7d => ext(0x50), // Down
        0x7e => ext(0x48), // Up
        _ => None,
    }
}

/// Linux evdev key code (`KEY_*` in linux/input-event-codes.h). X11 and
/// Wayland (via XKB) report these plus 8 as the hardware keycode; FreeBSD's
/// evdev-based input does the same.
pub fn from_evdev(code: u32) -> Option<Scancode> {
    match code {
        // The main block, F1-F12 and the keypad match set 1 one-for-one.
        1..=88 => sc(code as u8),
        89 => sc(0x73),  // KEY_RO
        92 => sc(0x79),  // KEY_HENKAN
        93 => sc(0x70),  // KEY_KATAKANAHIRAGANA
        94 => sc(0x7b),  // KEY_MUHENKAN
        96 => ext(0x1c), // keypad Enter
        97 => Some(RCTRL),
        98 => ext(0x35), // keypad /
        99 => ext(0x37), // Print Screen / SysRq
        100 => Some(RALT),
        102 => ext(0x47), // Home
        103 => ext(0x48), // Up
        104 => ext(0x49), // Page Up
        105 => ext(0x4b), // Left
        106 => ext(0x4d), // Right
        107 => ext(0x4f), // End
        108 => ext(0x50), // Down
        109 => ext(0x51), // Page Down
        110 => ext(0x52), // Insert
        111 => ext(0x53), // Delete
        124 => sc(0x7d),  // KEY_YEN
        125 => Some(LWIN),
        126 => Some(RWIN),
        127 => ext(0x5d), // Menu
        183..=189 => sc(0x64 + (code - 183) as u8), // F13-F19
        _ => None,
    }
}

/// Split a Windows WM_KEYDOWN/WM_KEYUP `lParam` into a scancode.
pub fn from_windows_lparam(lparam: isize) -> Option<Scancode> {
    let code = ((lparam >> 16) & 0xff) as u8;
    (code != 0).then_some((code, (lparam >> 24) & 1 != 0))
}

/// A character typed on a US keyboard: its scancode and whether Shift is
/// needed. For input that arrives as text without a hardware key code
/// (remote-control apps such as RustDesk, on-screen keyboards).
pub fn from_us_char(c: char) -> Option<(Scancode, bool)> {
    const ROWS: [(&str, &str, u8); 4] = [
        ("`1234567890-=", "~!@#$%^&*()_+", 0x29),
        ("qwertyuiop[]\\", "QWERTYUIOP{}|", 0x10),
        ("asdfghjkl;'", "ASDFGHJKL:\"", 0x1e),
        ("zxcvbnm,./", "ZXCVBNM<>?", 0x2c),
    ];
    match c {
        ' ' => return Some(((0x39, false), false)),
        '\t' => return Some(((0x0f, false), false)),
        '\r' | '\n' => return Some(((0x1c, false), false)),
        '\u{8}' | '\u{7f}' => return Some(((0x0e, false), false)),
        '\u{1b}' => return Some(((0x01, false), false)),
        _ => {}
    }
    for (plain, shifted, first) in ROWS {
        // The number row starts with ` (0x29), then 1-0 are 0x02-0x0b.
        let code = |i: usize| if first == 0x29 { if i == 0 { 0x29 } else { i as u8 + 1 } } else if first == 0x10 && i == 12 { 0x2b } else { first + i as u8 };
        if let Some(i) = plain.chars().position(|p| p == c) {
            return Some(((code(i), false), false));
        }
        if let Some(i) = shifted.chars().position(|p| p == c) {
            return Some(((code(i), false), true));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_key_same_scancode_everywhere() {
        // A, Escape, Up arrow, keypad Enter
        assert_eq!(from_mac_keycode(0x00), from_evdev(30));
        assert_eq!(from_mac_keycode(0x35), from_evdev(1));
        assert_eq!(from_mac_keycode(0x7e), from_evdev(103));
        assert_eq!(from_mac_keycode(0x4c), from_evdev(96));
        assert_eq!(from_windows_lparam(0x0148_0001), Some((0x48, true)));
    }

    #[test]
    fn us_characters_match_physical_keys() {
        // Same scancodes as the macOS key codes for those keys.
        for (c, mac) in [('a', 0x00), ('q', 0x0c), ('1', 0x12), ('0', 0x1d), ('-', 0x1b), ('[', 0x21), (']', 0x1e), ('\\', 0x2a), (';', 0x29), ('\'', 0x27), ('`', 0x32), ('/', 0x2c), ('m', 0x2e)] {
            assert_eq!(from_us_char(c).map(|(sc, _)| sc), from_mac_keycode(mac), "{c:?}");
        }
        assert_eq!(from_us_char('!'), Some(((0x02, false), true)));
        assert_eq!(from_us_char('|'), Some(((0x2b, false), true)));
        assert_eq!(from_us_char('Z'), Some(((0x2c, false), true)));
        assert_eq!(from_us_char('é'), None);
    }
}
