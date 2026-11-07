//! Caps, Num and Scroll Lock. RDP 5 servers take the state in a sync event.
//! NT 4.0 Terminal Server ignores the sync event's flags, so on RDP 4 the
//! locks start off (a sync of 0, for servers that do read it, and NT 4.0's
//! own state for a new session), and get there by tapping the keys. Taps sent
//! while NT 4.0 is still setting up the session are lost, so they wait for the
//! user's first keystroke. To know which to tap later on, we keep track of the
//! server's locks, counting the lock keys the user presses. Everything here
//! runs on the session thread.

use crate::rdp::input::{Input, KBD_FLAG_EXT, KBD_FLAG_UP, RDP_INPUT_SCANCODE, RDP_INPUT_SYNCHRONIZE, RDP_KEYPRESS, RDP_KEYRELEASE};

/// Queued by `Session::sync_locks`; param1 holds the wanted TS_SYNC_EVENT flags.
pub(crate) const TYPE_LOCKS: u16 = 0xfffd;

const SCROLL: u16 = 1;
const NUM: u16 = 2;
const CAPS: u16 = 4;
/// Each lock and its key's scancode.
const KEYS: [(u16, u16); 3] = [(CAPS, 0x3a), (NUM, 0x45), (SCROLL, 0x46)];

#[derive(Default)]
pub(crate) struct Locks {
    rdp4: bool,
    /// The server's locks as far as we know (RDP 4 only).
    server: u16,
    /// Lock keys held down: a key repeating doesn't toggle again.
    held: u16,
    /// The locks to set at the first keystroke (RDP 4).
    pending: Option<u16>,
}

impl Locks {
    /// What the sync event sent while connecting carries, for the local locks
    /// `wanted`. On RDP 4 that's "all off", with the wanted state queued up as
    /// key taps for once we're in.
    pub fn at_connect(&mut self, rdp4: bool, wanted: u16) -> u16 {
        *self = Locks { rdp4, ..Locks::default() };
        if !rdp4 {
            return wanted;
        }
        self.pending = Some(wanted);
        0
    }

    /// The events that give the server the `wanted` locks.
    pub fn expand(&mut self, wanted: u16) -> Vec<Input> {
        if !self.rdp4 {
            return vec![Input { message_type: RDP_INPUT_SYNCHRONIZE, flags: 0, param1: wanted, param2: 0 }];
        }
        if self.pending.is_some() {
            self.pending = Some(wanted);
            return Vec::new();
        }
        self.taps(wanted)
    }

    /// Key taps from the locks we think the server has to `wanted`.
    fn taps(&mut self, wanted: u16) -> Vec<Input> {
        let differ = self.server ^ wanted;
        self.server = wanted;
        let key = |sc: u16, flags: u16| Input { message_type: RDP_INPUT_SCANCODE, flags, param1: sc, param2: 0 };
        KEYS.iter().filter(|(lock, _)| differ & lock != 0).flat_map(|&(_, sc)| [key(sc, RDP_KEYPRESS), key(sc, RDP_KEYRELEASE)]).collect()
    }

    /// What to send before `ev`: the waiting taps, if it's the first keystroke.
    /// Modifiers and Delete don't count, so Ctrl+Alt+Del at NT 4.0's welcome
    /// screen doesn't use them up.
    pub fn before_key(&mut self, ev: &Input) -> Vec<Input> {
        const NOT_TYPING: [u16; 7] = [0x1d, 0x38, 0x2a, 0x36, 0x5b, 0x5c, 0x53];
        let press = ev.message_type == RDP_INPUT_SCANCODE && ev.flags & KBD_FLAG_UP == 0;
        if !press || NOT_TYPING.contains(&ev.param1) {
            return Vec::new();
        }
        self.pending.take().map(|w| self.taps(w)).unwrap_or_default()
    }

    /// Follow the user's own lock keys (RDP 4), as they go to the server.
    pub fn note_key(&mut self, ev: &Input) {
        if ev.message_type != RDP_INPUT_SCANCODE || ev.flags & KBD_FLAG_EXT != 0 || !self.rdp4 {
            return;
        }
        let Some(&(lock, _)) = KEYS.iter().find(|(_, sc)| *sc == ev.param1) else { return };
        if ev.flags & KBD_FLAG_UP != 0 {
            self.held &= !lock;
        } else if self.held & lock == 0 {
            self.held |= lock;
            self.server ^= lock;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(sc: u16, up: bool) -> Input {
        Input { message_type: RDP_INPUT_SCANCODE, flags: if up { RDP_KEYRELEASE } else { RDP_KEYPRESS }, param1: sc, param2: 0 }
    }

    #[test]
    fn rdp4_taps_wait_for_typing() {
        let mut l = Locks::default();
        assert_eq!(l.at_connect(true, CAPS), 0);
        assert!(l.before_key(&key(0x1d, false)).is_empty(), "Ctrl doesn't count");
        let taps = l.before_key(&key(0x1e, false));
        assert_eq!(taps.iter().map(|i| i.param1).collect::<Vec<_>>(), [0x3a, 0x3a]);
        // The user presses Caps Lock (held, repeating): one toggle.
        l.note_key(&key(0x3a, false));
        l.note_key(&key(0x3a, false));
        l.note_key(&key(0x3a, true));
        assert!(l.expand(0).is_empty(), "the server already has them all off");
    }

    #[test]
    fn rdp5_syncs() {
        let mut l = Locks::default();
        assert_eq!(l.at_connect(false, NUM), NUM);
        assert_eq!(l.expand(CAPS)[0].message_type, RDP_INPUT_SYNCHRONIZE);
    }
}
