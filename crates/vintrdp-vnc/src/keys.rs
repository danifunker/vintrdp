//! Keys for VNC. A PC keyboard gives scancodes; VNC wants X keysyms, the
//! symbol a key makes. A US layout turns one into the other, with Shift, Caps
//! Lock and Num Lock followed here. A key goes up with the keysym it went
//! down with, so releasing Shift first leaves nothing stuck. Servers with
//! QEMU's extension get the scancode as well, which needs no layout.

use std::collections::HashMap;

/// Shift, Caps Lock and Num Lock as the server should see them, and the
/// keysyms of keys that are down.
#[derive(Default)]
pub struct Keyboard {
    shift: [bool; 2],
    caps: bool,
    num: bool,
    down: HashMap<(u8, bool), u32>,
}

impl Keyboard {
    /// The lock keys' state from the front-end (TS_SYNC_EVENT flags).
    pub fn set_locks(&mut self, flags: u16) {
        self.caps = flags & 4 != 0;
        self.num = flags & 2 != 0;
    }

    /// The keysym for a key going down or up; None for a key with none.
    pub fn key(&mut self, scancode: u8, extended: bool, down: bool) -> Option<u32> {
        if !down {
            return self.down.remove(&(scancode, extended)).or_else(|| self.keysym(scancode, extended));
        }
        let sym = self.keysym(scancode, extended)?;
        match (scancode, extended) {
            (0x2a, false) => self.shift[0] = true,
            (0x36, false) => self.shift[1] = true,
            (0x3a, false) => self.caps = !self.caps,
            (0x45, false) => self.num = !self.num,
            _ => {}
        }
        self.down.insert((scancode, extended), sym);
        Some(sym)
    }

    /// Called after a key goes up, for the modifiers' state.
    pub fn released(&mut self, scancode: u8, extended: bool) {
        match (scancode, extended) {
            (0x2a, false) => self.shift[0] = false,
            (0x36, false) => self.shift[1] = false,
            _ => {}
        }
    }

    fn keysym(&self, scancode: u8, extended: bool) -> Option<u32> {
        if let Some(sym) = special(scancode, extended, self.num) {
            return Some(sym);
        }
        if extended {
            return None;
        }
        let (plain, shifted) = printable(scancode)?;
        let shift = self.shift[0] || self.shift[1];
        let c = if plain.is_ascii_lowercase() { if shift != self.caps { shifted } else { plain } } else if shift { shifted } else { plain };
        Some(c as u32)
    }
}

/// The keysym for a character with no key behind it: Latin-1 as itself,
/// anything else as X's Unicode keysym.
pub fn char_keysym(c: char) -> u32 {
    match c as u32 {
        n @ (0x20..=0x7e | 0xa0..=0xff) => n,
        n => 0x0100_0000 | n,
    }
}

/// A key's keysyms as an X keyboard map lists them: plain and with Shift;
/// on the keypad, without and with Num Lock. 0 is none.
pub fn key_keysyms(scancode: u8, extended: bool) -> Option<[u32; 2]> {
    if !extended && (0x47..=0x53).contains(&scancode) && scancode != 0x4a && scancode != 0x4e {
        return Some([keypad(scancode, false)?, keypad(scancode, true)?]);
    }
    if let Some(sym) = special(scancode, extended, false) {
        return Some([sym, 0]);
    }
    if extended {
        return None;
    }
    let (plain, shifted) = printable(scancode)?;
    Some([plain as u32, shifted as u32])
}

/// QEMU's key event wants the scancode with the E0 prefix as the high bit.
pub fn qemu_keycode(scancode: u8, extended: bool) -> u32 {
    scancode as u32 | if extended { 0x80 } else { 0 }
}

/// Keys that make no character.
fn special(sc: u8, ext: bool, num: bool) -> Option<u32> {
    Some(match (sc, ext) {
        (0x01, false) => 0xff1b,                                // Escape
        (0x0e, false) => 0xff08,                                // BackSpace
        (0x0f, false) => 0xff09,                                // Tab
        (0x1c, false) => 0xff0d,                                // Return
        (0x1c, true) => 0xff8d,                                 // KP_Enter
        (0x1d, false) => 0xffe3,                                // Control_L
        (0x1d, true) => 0xffe4,                                 // Control_R
        (0x2a, false) => 0xffe1,                                // Shift_L
        (0x36, false) => 0xffe2,                                // Shift_R
        (0x38, false) => 0xffe9,                                // Alt_L
        (0x38, true) => 0xffea,                                 // Alt_R
        (0x3a, false) => 0xffe5,                                // Caps_Lock
        (0x3b..=0x44, false) => 0xffbe + (sc - 0x3b) as u32,    // F1 to F10
        (0x57, false) => 0xffc8,                                // F11
        (0x58, false) => 0xffc9,                                // F12
        (0x45, false) => 0xff7f,                                // Num_Lock
        (0x46, false) => 0xff14,                                // Scroll_Lock
        (0x46, true) => 0xff6b,                                 // Break
        (0x45, true) => 0xff13,                                 // Pause
        (0x37, false) => 0xffaa,                                // KP_Multiply
        (0x37, true) => 0xff61,                                 // Print
        (0x35, true) => 0xffaf,                                 // KP_Divide
        (0x4a, false) => 0xffad,                                // KP_Subtract
        (0x4e, false) => 0xffab,                                // KP_Add
        (0x5b, true) => 0xffeb,                                 // Super_L
        (0x5c, true) => 0xffec,                                 // Super_R
        (0x5d, true) => 0xff67,                                 // Menu
        // The arrows and their block, and the keypad's.
        (0x47, true) => 0xff50,                                 // Home
        (0x48, true) => 0xff52,                                 // Up
        (0x49, true) => 0xff55,                                 // Prior
        (0x4b, true) => 0xff51,                                 // Left
        (0x4d, true) => 0xff53,                                 // Right
        (0x4f, true) => 0xff57,                                 // End
        (0x50, true) => 0xff54,                                 // Down
        (0x51, true) => 0xff56,                                 // Next
        (0x52, true) => 0xff63,                                 // Insert
        (0x53, true) => 0xffff,                                 // Delete
        (0x47..=0x53, false) if sc != 0x4a && sc != 0x4e => keypad(sc, num)?,
        _ => return None,
    })
}

/// The keypad's digits with Num Lock on, its cursor keys with it off.
fn keypad(sc: u8, num: bool) -> Option<u32> {
    const DIGITS: [(u8, u32, u32); 11] = [
        (0x47, 0xffb7, 0xff95), // 7 / KP_Home
        (0x48, 0xffb8, 0xff97), // 8 / KP_Up
        (0x49, 0xffb9, 0xff9a), // 9 / KP_Prior
        (0x4b, 0xffb4, 0xff96), // 4 / KP_Left
        (0x4c, 0xffb5, 0xff9d), // 5 / KP_Begin
        (0x4d, 0xffb6, 0xff98), // 6 / KP_Right
        (0x4f, 0xffb1, 0xff9c), // 1 / KP_End
        (0x50, 0xffb2, 0xff99), // 2 / KP_Down
        (0x51, 0xffb3, 0xff9b), // 3 / KP_Next
        (0x52, 0xffb0, 0xff9e), // 0 / KP_Insert
        (0x53, 0xffae, 0xff9f), // . / KP_Delete
    ];
    DIGITS.iter().find(|d| d.0 == sc).map(|&(_, digit, other)| if num { digit } else { other })
}

/// What a key makes on a US keyboard, without and with Shift.
fn printable(sc: u8) -> Option<(char, char)> {
    const ROWS: [(u8, &str, &str); 4] = [
        (0x02, "1234567890-=", "!@#$%^&*()_+"),
        (0x10, "qwertyuiop[]", "QWERTYUIOP{}"),
        (0x1e, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
        (0x2b, "\\zxcvbnm,./", "|ZXCVBNM<>?"),
    ];
    match sc {
        0x39 => return Some((' ', ' ')),
        0x56 => return Some(('\\', '|')), // the extra key of ISO keyboards
        _ => {}
    }
    for (first, plain, shifted) in ROWS {
        if let Some(i) = sc.checked_sub(first).map(usize::from).filter(|&i| i < plain.len()) {
            return Some((plain.as_bytes()[i] as char, shifted.as_bytes()[i] as char));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_shift_and_caps() {
        let mut k = Keyboard::default();
        assert_eq!(k.key(0x1e, false, true), Some('a' as u32));
        assert_eq!(k.key(0x1e, false, false), Some('a' as u32));
        k.key(0x2a, false, true); // Shift down
        assert_eq!(k.key(0x1e, false, true), Some('A' as u32));
        assert_eq!(k.key(0x02, false, true), Some('!' as u32));
        // Shift released first: 'A' still goes up as 'A'.
        k.key(0x2a, false, false);
        k.released(0x2a, false);
        assert_eq!(k.key(0x1e, false, false), Some('A' as u32));
        k.set_locks(4); // Caps Lock on
        assert_eq!(k.key(0x1e, false, true), Some('A' as u32));
        assert_eq!(k.key(0x02, false, true), Some('1' as u32));
    }

    #[test]
    fn special_and_keypad() {
        let mut k = Keyboard::default();
        assert_eq!(k.key(0x48, true, true), Some(0xff52)); // Up
        assert_eq!(k.key(0x48, false, true), Some(0xff97)); // KP_Up
        k.set_locks(2); // Num Lock on
        assert_eq!(k.key(0x48, false, true), Some(0xffb8)); // KP_8
        assert_eq!(k.key(0x3c, false, true), Some(0xffbf)); // F2
        assert_eq!(k.key(0x1c, true, true), Some(0xff8d)); // KP_Enter
        assert_eq!(k.key(0x35, false, true), Some('/' as u32));
        assert_eq!(k.key(0x2b, false, true), Some('\\' as u32));
        assert_eq!(k.key(0x34, false, true), Some('.' as u32));
        assert_eq!(k.key(0x29, false, true), Some('`' as u32));
        assert_eq!(k.key(0x28, false, true), Some('\'' as u32));
        assert_eq!(k.key(0x7f, false, true), None);
    }

    #[test]
    fn chars() {
        assert_eq!(char_keysym('é'), 0xe9);
        assert_eq!(char_keysym('€'), 0x0100_20ac);
        assert_eq!(qemu_keycode(0x1d, true), 0x9d);
    }

    #[test]
    fn keymap() {
        assert_eq!(key_keysyms(0x1e, false), Some([0x61, 0x41])); // a A
        assert_eq!(key_keysyms(0x02, false), Some([0x31, 0x21])); // 1 !
        assert_eq!(key_keysyms(0x48, false), Some([0xff97, 0xffb8])); // KP_Up KP_8
        assert_eq!(key_keysyms(0x48, true), Some([0xff52, 0])); // Up
        assert_eq!(key_keysyms(0x2a, false), Some([0xffe1, 0])); // Shift_L
        assert_eq!(key_keysyms(0x7f, false), None);
    }
}
