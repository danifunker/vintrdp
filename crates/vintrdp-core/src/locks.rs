//! Caps, Num and Scroll Lock. RDP 5 servers take the state in a sync event.
//! NT 4.0 Terminal Server ignores the sync event's flags, so on RDP 4 the
//! locks start off (a sync of 0, for servers that do read it, and NT 4.0's
//! own state for a new session), and get there by tapping the keys. Taps sent
//! while NT 4.0 is still setting up the session are lost, so they wait for the
//! user's first keystroke. To know which to tap later on, we keep track of the
//! server's locks, counting the lock keys the user presses. Everything here
//! runs on the rdesktop thread.

use std::cell::Cell;

use crate::ffi::{KBD_FLAG_EXT, KBD_FLAG_UP, RDP_INPUT_SCANCODE, RDP_INPUT_SYNCHRONIZE, RDP_KEYPRESS, RDP_KEYRELEASE};
use crate::session::{Input, shared};
use crate::state;

/// Queued by `Session::sync_locks`; param1 holds the wanted TS_SYNC_EVENT flags.
pub(crate) const TYPE_LOCKS: u16 = 0xfffd;

const SCROLL: u16 = 1;
const NUM: u16 = 2;
const CAPS: u16 = 4;
/// Each lock and its key's scancode.
const KEYS: [(u16, u16); 3] = [(CAPS, 0x3a), (NUM, 0x45), (SCROLL, 0x46)];

thread_local! {
    /// The server's locks as far as we know (RDP 4 only).
    static SERVER: Cell<u16> = const { Cell::new(0) };
    /// Lock keys held down: a key repeating doesn't toggle again.
    static HELD: Cell<u16> = const { Cell::new(0) };
    /// The locks to set at the first keystroke (RDP 4).
    static PENDING: Cell<Option<u16>> = const { Cell::new(None) };
}

fn rdp4() -> bool {
    // SAFETY: rdesktop's global, read on its own thread.
    unsafe { state::g_rdp_version < state::RDP_V5 }
}

/// What the sync event sent while connecting carries. On RDP 4 that's "all
/// off", with the wanted state queued up as key taps for once we're in.
pub(crate) fn at_connect() -> u16 {
    let wanted = shared().locks();
    if !rdp4() {
        return wanted;
    }
    SERVER.set(0);
    HELD.set(0);
    PENDING.set(Some(wanted));
    0
}

/// The events that give the server the `wanted` locks.
pub(crate) fn expand(wanted: u16) -> Vec<Input> {
    if !rdp4() {
        return vec![Input { message_type: RDP_INPUT_SYNCHRONIZE, flags: 0, param1: wanted, param2: 0 }];
    }
    if PENDING.get().is_some() {
        PENDING.set(Some(wanted));
        return Vec::new();
    }
    taps(wanted)
}

/// Key taps from the locks we think the server has to `wanted`.
fn taps(wanted: u16) -> Vec<Input> {
    let differ = SERVER.get() ^ wanted;
    SERVER.set(wanted);
    let key = |sc: u16, flags: u16| Input { message_type: RDP_INPUT_SCANCODE, flags, param1: sc, param2: 0 };
    KEYS.iter().filter(|(lock, _)| differ & lock != 0).flat_map(|&(_, sc)| [key(sc, RDP_KEYPRESS), key(sc, RDP_KEYRELEASE)]).collect()
}

/// Taps still waiting for the first keystroke, now that it's here.
pub(crate) fn flush() -> Vec<Input> {
    PENDING.take().map(taps).unwrap_or_default()
}

/// What to send before `ev`: the waiting taps, if it's the first keystroke.
/// Modifiers and Delete don't count, so Ctrl+Alt+Del at NT 4.0's welcome
/// screen doesn't use them up.
pub(crate) fn before_key(ev: &Input) -> Vec<Input> {
    const NOT_TYPING: [u16; 7] = [0x1d, 0x38, 0x2a, 0x36, 0x5b, 0x5c, 0x53];
    let press = ev.message_type == RDP_INPUT_SCANCODE && ev.flags & KBD_FLAG_UP == 0;
    if !press || NOT_TYPING.contains(&ev.param1) {
        return Vec::new();
    }
    flush()
}

/// Follow the user's own lock keys (RDP 4), as they go to the server.
pub(crate) fn note_key(ev: &Input) {
    if ev.message_type != RDP_INPUT_SCANCODE || ev.flags & KBD_FLAG_EXT != 0 || !rdp4() {
        return;
    }
    let Some(&(lock, _)) = KEYS.iter().find(|(_, sc)| *sc == ev.param1) else { return };
    let up = ev.flags & KBD_FLAG_UP != 0;
    if up {
        HELD.set(HELD.get() & !lock);
    } else if HELD.get() & lock == 0 {
        HELD.set(HELD.get() | lock);
        SERVER.set(SERVER.get() ^ lock);
    }
}
