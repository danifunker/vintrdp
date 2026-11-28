//! The keyboard as X clients see it. A keycode is the PC scancode plus 8, as
//! XFree86's keyboard driver had it; the E0 keys, whose scancodes repeat the
//! keypad's, get XFree86's codes 97 to 117.

pub const MIN_KEYCODE: u8 = 8;
pub const MAX_KEYCODE: u8 = 255;
/// Two keysyms per keycode: plain, and with Shift (or Num Lock, on the keypad).
pub const KEYSYMS_PER_KEYCODE: u8 = 2;

/// The keycode for a scancode; `extended` for E0-prefixed keys.
pub fn keycode(scancode: u8, extended: bool) -> Option<u8> {
    if !extended {
        return (1..=0x58).contains(&scancode).then(|| scancode + 8);
    }
    Some(match scancode {
        0x47 => 97,  // Home
        0x48 => 98,  // Up
        0x49 => 99,  // Prior
        0x4b => 100, // Left
        0x4d => 102, // Right
        0x4f => 103, // End
        0x50 => 104, // Down
        0x51 => 105, // Next
        0x52 => 106, // Insert
        0x53 => 107, // Delete
        0x1c => 108, // KP_Enter
        0x1d => 109, // Control_R
        0x45 => 110, // Pause
        0x37 => 111, // Print
        0x35 => 112, // KP_Divide
        0x38 => 113, // Alt_R
        0x46 => 114, // Break
        0x5b => 115, // Super_L
        0x5c => 116, // Super_R
        0x5d => 117, // Menu
        _ => return None,
    })
}

/// Shift, Lock, Control, Mod1 (Alt), Mod2 (Num Lock), Mod3, Mod4 (Super), Mod5.
pub const MODIFIERS: [[u8; 2]; 8] = [[50, 62], [66, 0], [37, 109], [64, 113], [77, 0], [0, 0], [115, 116], [0, 0]];

/// The keysyms of every keycode from [`MIN_KEYCODE`].
pub struct Keymap {
    syms: Vec<[u32; 2]>,
}

impl Keymap {
    /// From a function giving a key's keysyms by scancode.
    pub fn new(keysyms: impl Fn(u8, bool) -> Option<[u32; 2]>) -> Self {
        let mut syms = vec![[0; 2]; (MAX_KEYCODE - MIN_KEYCODE) as usize + 1];
        for ext in [false, true] {
            for sc in 0..0x80 {
                if let (Some(code), Some(s)) = (keycode(sc, ext), keysyms(sc, ext)) {
                    syms[(code - MIN_KEYCODE) as usize] = s;
                }
            }
        }
        Keymap { syms }
    }

    pub fn get(&self, keycode: u8) -> [u32; 2] {
        keycode.checked_sub(MIN_KEYCODE).and_then(|i| self.syms.get(i as usize)).copied().unwrap_or([0; 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(keycode(0x1e, false), Some(38)); // a, as XFree86
        assert_eq!(keycode(0x48, true), Some(98)); // Up
        assert_eq!(keycode(0x48, false), Some(80)); // KP_8
        assert_eq!(keycode(0x00, false), None);
        let k = Keymap::new(|sc, ext| (sc == 0x1e && !ext).then_some([b'a' as u32, b'A' as u32]));
        assert_eq!(k.get(38), [0x61, 0x41]);
        assert_eq!(k.get(7), [0, 0]);
        assert_eq!(k.get(255), [0, 0]);
    }
}
