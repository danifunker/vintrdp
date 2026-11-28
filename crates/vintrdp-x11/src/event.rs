//! Events: 32 bytes, built least significant byte first and swapped field by
//! field for big-endian clients (and SendEvent from them), by the layouts here.

use crate::wire::Order;

// Event codes.
pub const KEY_PRESS: u8 = 2;
pub const KEY_RELEASE: u8 = 3;
pub const BUTTON_PRESS: u8 = 4;
pub const BUTTON_RELEASE: u8 = 5;
pub const MOTION_NOTIFY: u8 = 6;
pub const ENTER_NOTIFY: u8 = 7;
pub const LEAVE_NOTIFY: u8 = 8;
pub const FOCUS_IN: u8 = 9;
pub const FOCUS_OUT: u8 = 10;
pub const KEYMAP_NOTIFY: u8 = 11;
pub const EXPOSE: u8 = 12;
pub const GRAPHICS_EXPOSURE: u8 = 13;
pub const NO_EXPOSURE: u8 = 14;
pub const VISIBILITY_NOTIFY: u8 = 15;
pub const CREATE_NOTIFY: u8 = 16;
pub const DESTROY_NOTIFY: u8 = 17;
pub const UNMAP_NOTIFY: u8 = 18;
pub const MAP_NOTIFY: u8 = 19;
pub const MAP_REQUEST: u8 = 20;
pub const REPARENT_NOTIFY: u8 = 21;
pub const CONFIGURE_NOTIFY: u8 = 22;
pub const CONFIGURE_REQUEST: u8 = 23;
pub const GRAVITY_NOTIFY: u8 = 24;
pub const RESIZE_REQUEST: u8 = 25;
pub const CIRCULATE_NOTIFY: u8 = 26;
pub const CIRCULATE_REQUEST: u8 = 27;
pub const PROPERTY_NOTIFY: u8 = 28;
pub const SELECTION_CLEAR: u8 = 29;
pub const SELECTION_REQUEST: u8 = 30;
pub const SELECTION_NOTIFY: u8 = 31;
pub const COLORMAP_NOTIFY: u8 = 32;
pub const CLIENT_MESSAGE: u8 = 33;
pub const MAPPING_NOTIFY: u8 = 34;
/// The first extension event: SHAPE's ShapeNotify.
pub const SHAPE_NOTIFY: u8 = 64;
/// Display PostScript's: output, status, ready (dps/mod.rs).
pub const DPS_OUTPUT: u8 = 65;
pub const DPS_STATUS: u8 = 66;
pub const DPS_READY: u8 = 67;

// Event masks.
pub const KEY_PRESS_MASK: u32 = 1;
pub const KEY_RELEASE_MASK: u32 = 1 << 1;
pub const BUTTON_PRESS_MASK: u32 = 1 << 2;
pub const BUTTON_RELEASE_MASK: u32 = 1 << 3;
pub const ENTER_WINDOW_MASK: u32 = 1 << 4;
pub const LEAVE_WINDOW_MASK: u32 = 1 << 5;
pub const POINTER_MOTION_MASK: u32 = 1 << 6;
pub const POINTER_MOTION_HINT_MASK: u32 = 1 << 7;
pub const BUTTON1_MOTION_MASK: u32 = 1 << 8;
pub const BUTTON_MOTION_MASK: u32 = 1 << 13;
pub const KEYMAP_STATE_MASK: u32 = 1 << 14;
pub const EXPOSURE_MASK: u32 = 1 << 15;
pub const VISIBILITY_CHANGE_MASK: u32 = 1 << 16;
pub const STRUCTURE_NOTIFY_MASK: u32 = 1 << 17;
pub const RESIZE_REDIRECT_MASK: u32 = 1 << 18;
pub const SUBSTRUCTURE_NOTIFY_MASK: u32 = 1 << 19;
pub const SUBSTRUCTURE_REDIRECT_MASK: u32 = 1 << 20;
pub const FOCUS_CHANGE_MASK: u32 = 1 << 21;
pub const PROPERTY_CHANGE_MASK: u32 = 1 << 22;
pub const COLORMAP_CHANGE_MASK: u32 = 1 << 23;
pub const OWNER_GRAB_BUTTON_MASK: u32 = 1 << 24;
pub const ALL_EVENTS: u32 = (1 << 25) - 1;
/// The events a device (key, button, motion) can be reported as.
pub const DEVICE_EVENTS: u32 = 0x3f4f;
/// What a pointer grab may ask for.
pub const POINTER_EVENTS: u32 = 0x7ffc;

/// An event, little-endian, its sequence number filled in when sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event(pub [u8; 32]);

/// Builds an event from byte 4 on.
pub struct Builder {
    ev: [u8; 32],
    pos: usize,
}

impl Builder {
    pub fn new(code: u8, detail: u8) -> Builder {
        let mut ev = [0; 32];
        ev[0] = code;
        ev[1] = detail;
        Builder { ev, pos: 4 }
    }

    pub fn u8(mut self, v: u8) -> Self {
        self.ev[self.pos] = v;
        self.pos += 1;
        self
    }

    pub fn bool(self, v: bool) -> Self {
        self.u8(v as u8)
    }

    pub fn u16(mut self, v: u16) -> Self {
        self.ev[self.pos..self.pos + 2].copy_from_slice(&v.to_le_bytes());
        self.pos += 2;
        self
    }

    pub fn i16(self, v: i16) -> Self {
        self.u16(v as u16)
    }

    pub fn u32(mut self, v: u32) -> Self {
        self.ev[self.pos..self.pos + 4].copy_from_slice(&v.to_le_bytes());
        self.pos += 4;
        self
    }

    pub fn done(self) -> Event {
        Event(self.ev)
    }
}

impl Event {
    pub fn code(&self) -> u8 {
        self.0[0] & 0x7f
    }

    pub fn set_u32(&mut self, at: usize, v: u32) {
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// The bytes for a client, with its last request's sequence number.
    pub fn encode(&self, order: Order, seq: u16) -> [u8; 32] {
        let mut b = self.0;
        if self.code() == KEYMAP_NOTIFY {
            return b;
        }
        b[2..4].copy_from_slice(&seq.to_le_bytes());
        if order == Order::Msb {
            swap(&mut b);
        }
        b
    }

    /// An event a client sent (SendEvent), in its order; None if this server
    /// doesn't know how to swap it.
    pub fn from_client(b: [u8; 32], order: Order) -> Option<Event> {
        let mut b = b;
        layout(b[0] & 0x7f, b[1])?;
        if order == Order::Msb {
            swap(&mut b);
        }
        Some(Event(b))
    }
}

/// Swap each field after the header's code and detail, both ways.
fn swap(b: &mut [u8; 32]) {
    let Some(fields) = layout(b[0] & 0x7f, b[1]) else { return };
    b.swap(2, 3);
    let mut at = 4;
    for &size in fields {
        let n = size as usize;
        if n > 1 {
            b[at..at + n].reverse();
        }
        at += n;
    }
    if b[0] & 0x7f == CLIENT_MESSAGE {
        let n = match b[1] {
            16 => 2,
            32 => 4,
            _ => 1,
        };
        if n > 1 {
            b[12..32].chunks_exact_mut(n).for_each(|c| c.reverse());
        }
    }
}

/// The sizes of an event's fields from byte 4; what's left is padding.
fn layout(code: u8, detail: u8) -> Option<&'static [u8]> {
    const DEVICE: &[u8] = &[4, 4, 4, 4, 2, 2, 2, 2, 2, 1, 1];
    Some(match code {
        KEY_PRESS..=LEAVE_NOTIFY => DEVICE,
        FOCUS_IN | FOCUS_OUT => &[4, 1],
        KEYMAP_NOTIFY => &[],
        EXPOSE => &[4, 2, 2, 2, 2, 2],
        GRAPHICS_EXPOSURE => &[4, 2, 2, 2, 2, 2, 2, 1],
        NO_EXPOSURE => &[4, 2, 1],
        VISIBILITY_NOTIFY => &[4, 1],
        CREATE_NOTIFY => &[4, 4, 2, 2, 2, 2, 2, 1],
        DESTROY_NOTIFY => &[4, 4],
        UNMAP_NOTIFY | MAP_NOTIFY => &[4, 4, 1],
        MAP_REQUEST => &[4, 4],
        REPARENT_NOTIFY => &[4, 4, 4, 2, 2, 1],
        CONFIGURE_NOTIFY => &[4, 4, 4, 2, 2, 2, 2, 2, 1],
        CONFIGURE_REQUEST => &[4, 4, 4, 2, 2, 2, 2, 2, 2],
        GRAVITY_NOTIFY => &[4, 4, 2, 2],
        RESIZE_REQUEST => &[4, 2, 2],
        CIRCULATE_NOTIFY | CIRCULATE_REQUEST => &[4, 4, 4, 1],
        PROPERTY_NOTIFY => &[4, 4, 4, 1],
        SELECTION_CLEAR => &[4, 4, 4],
        SELECTION_REQUEST => &[4, 4, 4, 4, 4, 4],
        SELECTION_NOTIFY => &[4, 4, 4, 4, 4],
        COLORMAP_NOTIFY => &[4, 4, 1, 1],
        CLIENT_MESSAGE if matches!(detail, 8 | 16 | 32) => &[4, 4],
        MAPPING_NOTIFY => &[1, 1, 1],
        SHAPE_NOTIFY => &[4, 2, 2, 2, 2, 4, 1],
        // The output's bytes are the context's own, not swapped.
        DPS_OUTPUT | DPS_STATUS => &[4],
        DPS_READY => &[4, 4, 4, 4, 4],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swapping() {
        let ev = Builder::new(CONFIGURE_NOTIFY, 0).u32(0x0102_0304).u32(5).u32(0).i16(-2).u16(7).u16(8).u16(9).u16(1).bool(true).done();
        let be = ev.encode(Order::Msb, 0x0a0b);
        assert_eq!(be[..12], [22, 0, 0x0a, 0x0b, 1, 2, 3, 4, 0, 0, 0, 5]);
        assert_eq!(be[16..18], [0xff, 0xfe]);
        assert_eq!(be[26], 1);
        let back = Event::from_client(be, Order::Msb).unwrap();
        assert_eq!(back.0[4..], ev.0[4..]);
        let le = ev.encode(Order::Lsb, 3);
        assert_eq!(le[2..8], [3, 0, 4, 3, 2, 1]);
        // ClientMessage data in its format.
        let cm = Builder::new(CLIENT_MESSAGE, 32).u32(1).u32(2).u32(0x11223344).done();
        assert_eq!(cm.encode(Order::Msb, 0)[12..16], [0x11, 0x22, 0x33, 0x44]);
        assert!(Event::from_client([99; 32], Order::Lsb).is_none());
    }
}
