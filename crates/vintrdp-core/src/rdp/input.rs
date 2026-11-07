//! Input events: what front-ends queue on a `Session`, and how they go to
//! the server (TS_INPUT_EVENT, slow path).

/// TS_INPUT_EVENT messageType.
pub const RDP_INPUT_SYNCHRONIZE: u16 = 0;
pub const RDP_INPUT_SCANCODE: u16 = 4;
pub const RDP_INPUT_UNICODE: u16 = 5;
pub const RDP_INPUT_MOUSE: u16 = 0x8001;

/// TS_INPUT_CAPABILITYSET inputFlags.
pub const INPUT_FLAG_SCANCODES: u16 = 0x0001;
pub const INPUT_FLAG_MOUSEX: u16 = 0x0004;
pub const INPUT_FLAG_UNICODE: u16 = 0x0010;

pub const KBD_FLAG_EXT: u16 = 0x0100;
pub const KBD_FLAG_DOWN: u16 = 0x4000;
pub const KBD_FLAG_UP: u16 = 0x8000;
pub const RDP_KEYPRESS: u16 = 0;
pub const RDP_KEYRELEASE: u16 = KBD_FLAG_DOWN | KBD_FLAG_UP;

pub const MOUSE_FLAG_MOVE: u16 = 0x0800;
pub const MOUSE_FLAG_BUTTON1: u16 = 0x1000;
pub const MOUSE_FLAG_BUTTON2: u16 = 0x2000;
pub const MOUSE_FLAG_BUTTON3: u16 = 0x4000;
pub const MOUSE_FLAG_BUTTON4: u16 = 0x0278;
pub const MOUSE_FLAG_BUTTON5: u16 = 0x0388;
pub const MOUSE_FLAG_DOWN: u16 = 0x8000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Input {
    pub message_type: u16,
    pub flags: u16,
    pub param1: u16,
    pub param2: u16,
}

/// Queued by `Session::type_char`; never sent as is. The character is in
/// param1 (low 16 bits) and param2 (the rest).
pub(crate) const TYPE_CHAR: u16 = 0xfffe;

/// The events that type `c`: Unicode key events if the server takes them,
/// else an Alt+0nnn keypad code.
pub(crate) fn expand_char(c: char, unicode: bool) -> Vec<Input> {
    const RELEASE: u16 = 0x8000;
    let key = |sc: crate::scancode::Scancode, down: bool| Input {
        message_type: RDP_INPUT_SCANCODE,
        flags: if down { RDP_KEYPRESS } else { RDP_KEYRELEASE } | if sc.1 { KBD_FLAG_EXT } else { 0 },
        param1: sc.0 as u16,
        param2: 0,
    };
    if c == '\0' {
        return Vec::new();
    }
    if unicode {
        let mut units = [0u16; 2];
        return c
            .encode_utf16(&mut units)
            .iter()
            .flat_map(|&u| [false, true].map(|up| Input { message_type: RDP_INPUT_UNICODE, flags: if up { RELEASE } else { 0 }, param1: u, param2: 0 }))
            .collect();
    }
    let Some(byte) = crate::scancode::cp1252(c) else { return Vec::new() };
    let alt = crate::scancode::LALT;
    let mut events = vec![key(alt, true)];
    for d in crate::scancode::alt_code(byte) {
        events.extend([key(d, true), key(d, false)]);
    }
    events.push(key(alt, false));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_type_as_unicode_or_alt_codes() {
        let unicode = expand_char('\u{e9}', true);
        assert_eq!(unicode.iter().map(|e| (e.message_type, e.flags, e.param1)).collect::<Vec<_>>(), [(5, 0, 0xe9), (5, 0x8000, 0xe9)]);
        // Outside the BMP: a surrogate pair, each pressed and released.
        assert_eq!(expand_char('\u{1f600}', true).len(), 4);

        // Alt down, keypad 0 2 3 3 (each down and up), Alt up.
        let alt = expand_char('\u{e9}', false);
        assert_eq!(alt.len(), 10);
        assert_eq!((alt[0].param1, alt[0].flags), (0x38, RDP_KEYPRESS));
        assert_eq!((alt[9].param1, alt[9].flags), (0x38, RDP_KEYRELEASE));
        assert_eq!(alt[1].param1, 0x52);

        assert!(expand_char('\u{4e16}', false).is_empty());
        assert!(expand_char('\0', true).is_empty());
    }
}
