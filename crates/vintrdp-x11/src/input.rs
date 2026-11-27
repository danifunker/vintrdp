//! The keyboard and pointer: input from the front-end becomes key, button,
//! motion, crossing and focus events, through grabs (active, passive and
//! implicit), with devices frozen and thawed by AllowEvents.

use std::collections::VecDeque;
use std::rc::Rc;

use crate::cursor::Cursor;
use crate::event::{self, Builder, Event};
use crate::keymap;
use crate::server::*;
use crate::wire::Reader;
use crate::InputEvent;

// Crossing and focus details, and modes.
const ANCESTOR: u8 = 0;
const VIRTUAL: u8 = 1;
const INFERIOR: u8 = 2;
const NONLINEAR: u8 = 3;
const NONLINEAR_VIRTUAL: u8 = 4;
const POINTER: u8 = 5;
const POINTER_ROOT: u8 = 6;
const DETAIL_NONE: u8 = 7;
const NORMAL: u8 = 0;
const GRAB: u8 = 1;
const UNGRAB: u8 = 2;

const ANY_MODIFIER: u16 = 0x8000;
const NONE: u32 = 0;
const POINTER_ROOT_WIN: u32 = 1;

#[derive(Clone)]
pub struct Grab {
    pub slot: u32,
    pub window: u32,
    pub owner_events: bool,
    pub mask: u32,
    pub pointer_sync: bool,
    pub keyboard_sync: bool,
    pub confine_to: u32,
    pub cursor: Option<Rc<Cursor>>,
    pub time: u32,
    /// Ends when the buttons (or the key) that started it are released.
    pub passive: bool,
    /// The key of a passive key grab.
    pub key: u8,
    /// The event that activated a passive grab, for ReplayPointer/Keyboard.
    pub activating: Option<InputEvent>,
}

pub struct PassiveGrab {
    pub slot: u32,
    pub window: u32,
    pub keyboard: bool,
    /// A button or key; 0 for any.
    pub detail: u8,
    pub modifiers: u16,
    pub owner_events: bool,
    pub mask: u32,
    pub pointer_sync: bool,
    pub keyboard_sync: bool,
    pub confine_to: u32,
    pub cursor: Option<Rc<Cursor>>,
}

pub struct Input {
    pub x: i32,
    pub y: i32,
    /// Logical buttons down, bit 0 for button 1.
    pub buttons: u16,
    pub keys: [u8; 32],
    /// Lock and Num Lock, toggled: modifier bits.
    pub locks: u16,
    /// The window the pointer is in.
    pub sprite: u32,
    pub focus: u32,
    pub focus_revert: u8,
    pub focus_time: u32,
    pub grab_time: u32,
    pub pointer_grab: Option<Grab>,
    pub keyboard_grab: Option<Grab>,
    pub pointer_frozen: bool,
    pub keyboard_frozen: bool,
    /// Sync modes from AllowEvents: freeze again after the next event.
    pointer_once: bool,
    keyboard_once: bool,
    /// Events held while their device is frozen.
    queue: VecDeque<InputEvent>,
    /// (client, window) that got a motion hint and no more motion since.
    hinted: Vec<(u32, u32)>,
    pub shown_cursor: Option<Rc<Cursor>>,
    pub default_cursor: Option<Rc<Cursor>>,
}

impl Default for Input {
    fn default() -> Self {
        Input {
            x: 0,
            y: 0,
            buttons: 0,
            keys: [0; 32],
            locks: 0,
            sprite: ROOT,
            focus: POINTER_ROOT_WIN,
            focus_revert: 1,
            focus_time: 0,
            grab_time: 0,
            pointer_grab: None,
            keyboard_grab: None,
            pointer_frozen: false,
            keyboard_frozen: false,
            pointer_once: false,
            keyboard_once: false,
            queue: VecDeque::new(),
            hinted: Vec::new(),
            shown_cursor: None,
            default_cursor: None,
        }
    }
}

fn is_pointer(ev: &InputEvent) -> bool {
    !matches!(ev, InputEvent::Key { .. })
}

impl State {
    // State of the devices.

    fn key_down(&self, code: u8) -> bool {
        self.input.keys[code as usize / 8] >> (code % 8) & 1 != 0
    }

    /// The modifier a keycode is mapped to, if any.
    fn modifier_of(&self, code: u8) -> Option<usize> {
        (0..8).find(|&m| self.modmap[m * self.mod_per..(m + 1) * self.mod_per].contains(&code) && code != 0)
    }

    fn keysym(&self, code: u8) -> u32 {
        let i = (code - keymap::MIN_KEYCODE) as usize * self.keysyms_per;
        self.keysyms.get(i).copied().unwrap_or(0)
    }

    /// The modifier and button state, as events report it.
    pub fn state_mask(&self) -> u16 {
        let mut m = self.input.locks;
        for i in 0..8 {
            if self.modmap[i * self.mod_per..(i + 1) * self.mod_per].iter().any(|&c| c != 0 && self.key_down(c) && !self.locking(c)) {
                m |= 1 << i;
            }
        }
        m | (self.input.buttons & 0x1f) << 8
    }

    /// Caps Lock and Num Lock toggle their modifier, as X servers have them.
    fn locking(&self, code: u8) -> bool {
        matches!(self.keysym(code), 0xffe5 | 0xff7f) || self.modifier_of(code) == Some(1)
    }

    fn time(&self) -> u32 {
        self.now()
    }

    /// The deepest viewable window under (x, y), by input shape.
    fn window_at(&self, x: i32, y: i32) -> u32 {
        let mut w = ROOT;
        'down: loop {
            let win = &self.windows[&w];
            for &c in win.children.iter().rev() {
                let cw = &self.windows[&c];
                if cw.viewable && cw.input_shape().contains(x - cw.ax, y - cw.ay) {
                    w = c;
                    continue 'down;
                }
            }
            return w;
        }
    }

    /// The pointer may have changed window: crossing events, the cursor.
    pub fn pointer_moved(&mut self) {
        let w = self.window_at(self.input.x, self.input.y);
        if w != self.input.sprite {
            let old = std::mem::replace(&mut self.input.sprite, w);
            self.input.hinted.clear();
            self.crossing(old, w, NORMAL);
        }
        self.update_cursor();
    }

    /// Windows about to go: the pointer, focus and grabs leave them first.
    pub fn windows_going(&mut self, tree: &[u32], parent: u32) {
        if tree.contains(&self.input.sprite) {
            self.input.sprite = parent;
        }
        if tree.contains(&self.input.focus) {
            self.revert_focus();
        }
        if self.input.pointer_grab.as_ref().is_some_and(|g| tree.contains(&g.window) || tree.contains(&g.confine_to)) {
            self.deactivate_pointer_grab();
        }
        if self.input.keyboard_grab.as_ref().is_some_and(|g| tree.contains(&g.window)) {
            self.deactivate_keyboard_grab();
        }
    }

    pub fn window_destroyed_input(&mut self, id: u32) {
        self.input.hinted.retain(|h| h.1 != id);
    }

    pub fn client_gone_input(&mut self, slot: u32) {
        if self.input.pointer_grab.as_ref().is_some_and(|g| g.slot == slot) {
            self.deactivate_pointer_grab();
        }
        if self.input.keyboard_grab.as_ref().is_some_and(|g| g.slot == slot) {
            self.deactivate_keyboard_grab();
        }
        self.passive.retain(|g| g.slot != slot);
        self.input.hinted.retain(|h| h.0 != slot);
    }

    // Front-end input.

    pub fn device(&mut self, ev: InputEvent) {
        let frozen = if is_pointer(&ev) { self.input.pointer_frozen } else { self.input.keyboard_frozen };
        if frozen || !self.input.queue.is_empty() {
            self.input.queue.push_back(ev);
            self.thaw_queue();
            return;
        }
        self.process(ev, None);
    }

    /// Events queued while frozen, for devices that aren't any more.
    fn thaw_queue(&mut self) {
        let mut i = 0;
        while i < self.input.queue.len() {
            let ev = self.input.queue[i];
            let frozen = if is_pointer(&ev) { self.input.pointer_frozen } else { self.input.keyboard_frozen };
            if frozen {
                i += 1;
                continue;
            }
            // Keep each device's events in order: nothing of a frozen device's
            // is skipped over.
            let blocked = self.input.queue.iter().take(i).any(|e| is_pointer(e) == is_pointer(&ev));
            if blocked {
                i += 1;
                continue;
            }
            self.input.queue.remove(i);
            self.process(ev, None);
            i = 0;
        }
    }

    /// One event; `below` replays it as if grabs at that window and above
    /// weren't there (AllowEvents Replay).
    fn process(&mut self, ev: InputEvent, below: Option<u32>) {
        match ev {
            InputEvent::Motion { x, y } => {
                let (mut x, mut y) = (x.clamp(0, self.width as i32 - 1), y.clamp(0, self.height as i32 - 1));
                if let Some(c) = self.input.pointer_grab.as_ref().map(|g| g.confine_to).filter(|&c| c != 0) {
                    (x, y) = self.confine(c, x, y);
                }
                if (x, y) == (self.input.x, self.input.y) {
                    return;
                }
                (self.input.x, self.input.y) = (x, y);
                self.pointer_moved();
                self.motion();
            }
            InputEvent::Button { button, down } => {
                let Some(&logical) = self.pointer_map.get(button.wrapping_sub(1) as usize) else { return };
                if logical == 0 || logical > 5 {
                    return;
                }
                let bit = 1u16 << (logical - 1);
                if down == (self.input.buttons & bit != 0) {
                    return;
                }
                let state = self.state_mask();
                self.input.hinted.clear();
                if down {
                    if self.input.pointer_grab.is_none() && !self.activate_button_grab(&ev, logical, state, below) {
                        self.input.buttons |= bit;
                        let delivered = self.pointer_event(event::BUTTON_PRESS, logical, state, event::BUTTON_PRESS_MASK);
                        // The implicit grab: whoever got the press gets the rest.
                        if let Some((slot, win)) = delivered {
                            let mask = self.windows[&win].mask_of(slot);
                            self.input.pointer_grab = Some(Grab {
                                slot,
                                window: win,
                                owner_events: mask & event::OWNER_GRAB_BUTTON_MASK != 0,
                                mask,
                                pointer_sync: false,
                                keyboard_sync: false,
                                confine_to: 0,
                                cursor: None,
                                time: self.time(),
                                passive: true,
                                key: 0,
                                activating: None,
                            });
                        }
                        return;
                    }
                    self.input.buttons |= bit;
                    self.activating_event(false, event::BUTTON_PRESS, logical, state);
                    self.sync_once(true);
                } else {
                    self.input.buttons &= !bit;
                    self.pointer_event(event::BUTTON_RELEASE, logical, state, event::BUTTON_RELEASE_MASK);
                    self.sync_once(true);
                    if self.input.buttons == 0 && self.input.pointer_grab.as_ref().is_some_and(|g| g.passive) {
                        self.deactivate_pointer_grab();
                    }
                }
            }
            InputEvent::Key { code, down } => {
                if code < keymap::MIN_KEYCODE {
                    return;
                }
                let held = self.key_down(code);
                if down && held {
                    // The front-end's auto-repeat: X has it as release, press.
                    let (i, b) = (code as usize / 8, code % 8);
                    if !self.kbd.global_repeat || self.kbd.repeats[i] >> b & 1 == 0 {
                        return;
                    }
                    self.process(InputEvent::Key { code, down: false }, None);
                } else if !down && !held {
                    return;
                }
                let state = self.state_mask();
                self.input.hinted.clear();
                let (i, b) = (code as usize / 8, code % 8);
                if down {
                    self.input.keys[i] |= 1 << b;
                    if self.locking(code) {
                        if let Some(m) = self.modifier_of(code) {
                            self.input.locks ^= 1 << m;
                        }
                    }
                    if self.input.keyboard_grab.is_none() && self.activate_key_grab(&ev, code, state, below) {
                        self.activating_event(true, event::KEY_PRESS, code, state);
                    } else {
                        self.key_event(event::KEY_PRESS, code, state, event::KEY_PRESS_MASK);
                    }
                    self.sync_once(false);
                } else {
                    self.input.keys[i] &= !(1 << b);
                    self.key_event(event::KEY_RELEASE, code, state, event::KEY_RELEASE_MASK);
                    self.sync_once(false);
                    if self.input.keyboard_grab.as_ref().is_some_and(|g| g.passive && g.key == code) {
                        self.deactivate_keyboard_grab();
                    }
                }
            }
        }
    }

    /// After SyncPointer/SyncKeyboard: the device freezes again after one event.
    fn sync_once(&mut self, pointer: bool) {
        if pointer && std::mem::take(&mut self.input.pointer_once) {
            self.input.pointer_frozen = true;
        }
        if !pointer && std::mem::take(&mut self.input.keyboard_once) {
            self.input.keyboard_frozen = true;
        }
    }

    /// A point kept inside a window.
    fn confine(&self, w: u32, x: i32, y: i32) -> (i32, i32) {
        let Some(win) = self.windows.get(&w) else { return (x, y) };
        let e = win.border_clip.intersect(&win.clip_shape().translated(win.ax, win.ay)).extents();
        if e.is_empty() {
            return (x, y);
        }
        (x.clamp(e.x1, e.x2 - 1), y.clamp(e.y1, e.y2 - 1))
    }

    // Building and delivering device events.

    /// Key, button and motion events: code, detail, then the window fields.
    fn device_ev(&self, code: u8, detail: u8, win: u32, child: u32, state: u16) -> Event {
        let w = &self.windows[&win];
        let (x, y) = (self.input.x, self.input.y);
        Builder::new(code, detail)
            .u32(self.time())
            .u32(ROOT)
            .u32(win)
            .u32(child)
            .i16(x as i16)
            .i16(y as i16)
            .i16((x - w.ax) as i16)
            .i16((y - w.ay) as i16)
            .u16(state)
            .bool(true)
            .done()
    }

    /// The child of `ancestor` on the way down to `w`, or None.
    fn child_toward(&self, ancestor: u32, w: u32) -> u32 {
        let mut c = w;
        while let Some(win) = self.windows.get(&c) {
            if win.parent == ancestor {
                return c;
            }
            c = win.parent;
        }
        0
    }

    /// Deliver from `source` up, as the protocol propagates device events;
    /// only to `only` if given. The client and window that got it.
    fn propagate(&mut self, source: u32, code: u8, detail: u8, state: u16, mask: u32, only: Option<u32>) -> Option<(u32, u32)> {
        let mut w = source;
        loop {
            let win = self.windows.get(&w)?;
            let child = if w == source { 0 } else { self.child_toward(w, source) };
            let takers: Vec<(u32, u32)> = win.events.iter().filter(|e| e.1 & mask != 0 && only.is_none_or(|o| o == e.0)).copied().collect();
            if !takers.is_empty() {
                let mut first = None;
                for (slot, m) in takers {
                    let mut d = detail;
                    if code == event::MOTION_NOTIFY && m & event::POINTER_MOTION_HINT_MASK != 0 {
                        if self.input.hinted.contains(&(slot, w)) {
                            continue;
                        }
                        self.input.hinted.push((slot, w));
                        d = 1;
                    }
                    let ev = self.device_ev(code, d, w, child, state);
                    self.send(slot, &ev);
                    first.get_or_insert((slot, w));
                }
                return first.or(Some((0, w)));
            }
            if win.do_not_propagate & mask != 0 || win.parent == 0 {
                return None;
            }
            w = win.parent;
        }
    }

    /// Through an active grab: as normal to the grabbing client's own
    /// windows if owner_events, else to the grab window.
    fn through_grab(&mut self, g: &Grab, source: u32, code: u8, detail: u8, state: u16, mask: u32, always: bool) -> Option<(u32, u32)> {
        if g.owner_events {
            if let Some(d) = self.propagate(source, code, detail, state, mask, Some(g.slot)) {
                return Some(d);
            }
        }
        if !always && g.mask & mask == 0 {
            return None;
        }
        let in_grab = source == g.window || self.is_ancestor(g.window, source);
        let child = if in_grab && source != g.window { self.child_toward(g.window, source) } else { 0 };
        let mut d = detail;
        if code == event::MOTION_NOTIFY && g.mask & event::POINTER_MOTION_HINT_MASK != 0 {
            if self.input.hinted.contains(&(g.slot, g.window)) {
                return None;
            }
            self.input.hinted.push((g.slot, g.window));
            d = 1;
        }
        let ev = self.device_ev(code, d, g.window, child, state);
        self.send(g.slot, &ev);
        Some((g.slot, g.window))
    }

    /// The event that activated a passive grab: to the grab's window,
    /// whatever its event mask and owner-events say, as X.org's
    /// DeliverOneGrabbedEvent has it.
    fn activating_event(&mut self, keyboard: bool, code: u8, detail: u8, state: u16) {
        let g = if keyboard { self.input.keyboard_grab.clone() } else { self.input.pointer_grab.clone() };
        let Some(g) = g else { return };
        let source = if keyboard { self.key_source().unwrap_or(g.window) } else { self.input.sprite };
        let child = if source != g.window && self.is_ancestor(g.window, source) { self.child_toward(g.window, source) } else { 0 };
        let ev = self.device_ev(code, detail, g.window, child, state);
        self.send(g.slot, &ev);
    }

    fn pointer_event(&mut self, code: u8, detail: u8, state: u16, mask: u32) -> Option<(u32, u32)> {
        let source = self.input.sprite;
        match self.input.pointer_grab.clone() {
            Some(g) => self.through_grab(&g, source, code, detail, state, mask, false),
            None => self.propagate(source, code, detail, state, mask, None).filter(|d| d.0 != 0),
        }
    }

    /// MotionNotify for the masks the buttons down make it match.
    fn motion(&mut self) {
        let b = self.input.buttons;
        let mut mask = event::POINTER_MOTION_MASK;
        if b != 0 {
            mask |= event::BUTTON_MOTION_MASK;
        }
        for i in 0..5 {
            if b >> i & 1 != 0 {
                mask |= event::BUTTON1_MOTION_MASK << i;
            }
        }
        let state = self.state_mask();
        self.pointer_event(event::MOTION_NOTIFY, 0, state, mask);
    }

    /// Where key events go: the pointer's window if it's in the focus,
    /// else the focus.
    fn key_source(&self) -> Option<u32> {
        match self.input.focus {
            NONE => None,
            POINTER_ROOT_WIN => Some(self.input.sprite),
            f => Some(if self.input.sprite == f || self.is_ancestor(f, self.input.sprite) { self.input.sprite } else { f }),
        }
    }

    fn key_event(&mut self, code: u8, detail: u8, state: u16, mask: u32) {
        if let Some(g) = self.input.keyboard_grab.clone() {
            let source = self.key_source().unwrap_or(g.window);
            self.through_grab(&g, source, code, detail, state, mask, true);
            return;
        }
        let Some(source) = self.key_source() else { return };
        // Not above the focus window.
        let f = self.input.focus;
        let mut w = source;
        loop {
            let Some(win) = self.windows.get(&w) else { return };
            let child = if w == source { 0 } else { self.child_toward(w, source) };
            let takers: Vec<u32> = win.events.iter().filter(|e| e.1 & mask != 0).map(|e| e.0).collect();
            if !takers.is_empty() {
                let ev = self.device_ev(code, detail, w, child, state);
                for slot in takers {
                    self.send(slot, &ev);
                }
                return;
            }
            if win.do_not_propagate & mask != 0 || w == f || win.parent == 0 {
                return;
            }
            w = win.parent;
        }
    }

    // Passive grabs.

    fn grab_matches(g: &PassiveGrab, keyboard: bool, detail: u8, state: u16) -> bool {
        g.keyboard == keyboard && (g.detail == 0 || g.detail == detail) && (g.modifiers == ANY_MODIFIER || g.modifiers == state & 0xff)
    }

    /// The windows from the root down to `to`.
    fn path_down(&self, to: u32) -> Vec<u32> {
        let mut path = vec![to];
        let mut w = to;
        while let Some(win) = self.windows.get(&w) {
            if win.parent == 0 {
                break;
            }
            w = win.parent;
            path.push(w);
        }
        path.reverse();
        path
    }

    fn activate(&mut self, g: &PassiveGrab, ev: &InputEvent, key: u8) -> Grab {
        Grab {
            slot: g.slot,
            window: g.window,
            owner_events: g.owner_events,
            mask: g.mask,
            pointer_sync: g.pointer_sync,
            keyboard_sync: g.keyboard_sync,
            confine_to: g.confine_to,
            cursor: g.cursor.clone(),
            time: self.time(),
            passive: true,
            key,
            activating: Some(*ev),
        }
    }

    /// A passive button grab on the way down to the pointer's window.
    fn activate_button_grab(&mut self, ev: &InputEvent, button: u8, state: u16, below: Option<u32>) -> bool {
        let mut path = self.path_down(self.input.sprite);
        if let Some(b) = below {
            match path.iter().position(|&w| w == b) {
                Some(i) => path.drain(..=i),
                None => path.drain(..0),
            };
        }
        for w in path {
            let found = self.passive.iter().position(|g| g.window == w && Self::grab_matches(g, false, button, state) && (g.confine_to == 0 || self.windows.get(&g.confine_to).is_some_and(|c| c.viewable)));
            if let Some(i) = found {
                let p = &self.passive[i];
                let p = PassiveGrab { cursor: p.cursor.clone(), ..*p };
                let grab = self.activate(&p, ev, 0);
                self.start_pointer_grab(grab);
                return true;
            }
        }
        false
    }

    fn activate_key_grab(&mut self, ev: &InputEvent, code: u8, state: u16, below: Option<u32>) -> bool {
        let mut path = match self.input.focus {
            NONE => return false,
            POINTER_ROOT_WIN => self.path_down(self.input.sprite),
            f => {
                let mut p = self.path_down(f);
                if self.is_ancestor(f, self.input.sprite) {
                    let i = p.len();
                    p.extend(self.path_down(self.input.sprite).into_iter().skip(i));
                }
                p
            }
        };
        if let Some(b) = below {
            if let Some(i) = path.iter().position(|&w| w == b) {
                path.drain(..=i);
            }
        }
        for w in path {
            if let Some(i) = self.passive.iter().position(|g| g.window == w && Self::grab_matches(g, true, code, state)) {
                let p = &self.passive[i];
                let p = PassiveGrab { cursor: p.cursor.clone(), ..*p };
                let grab = self.activate(&p, ev, code);
                self.start_keyboard_grab(grab);
                return true;
            }
        }
        false
    }

    fn start_pointer_grab(&mut self, g: Grab) {
        let (window, psync, ksync) = (g.window, g.pointer_sync, g.keyboard_sync);
        let replacing = self.input.pointer_grab.is_some();
        self.input.pointer_grab = Some(g);
        self.input.grab_time = self.time();
        if !replacing {
            let sprite = self.input.sprite;
            self.crossing(sprite, window, GRAB);
        }
        self.input.pointer_frozen = psync;
        if ksync {
            self.input.keyboard_frozen = true;
        }
        self.update_cursor();
    }

    pub fn deactivate_pointer_grab(&mut self) {
        let Some(g) = self.input.pointer_grab.take() else { return };
        self.input.pointer_frozen = false;
        self.input.pointer_once = false;
        if g.keyboard_sync && self.input.keyboard_grab.is_none() {
            self.input.keyboard_frozen = false;
        }
        let sprite = self.input.sprite;
        self.crossing(g.window, sprite, UNGRAB);
        self.update_cursor();
        self.thaw_queue();
    }

    fn start_keyboard_grab(&mut self, g: Grab) {
        let (window, psync, ksync) = (g.window, g.pointer_sync, g.keyboard_sync);
        let old = self.input.keyboard_grab.as_ref().map(|g| g.window);
        self.input.keyboard_grab = Some(g);
        self.input.grab_time = self.time();
        let from = old.unwrap_or(self.input.focus);
        self.focus_events(from, window, GRAB);
        self.input.keyboard_frozen = ksync;
        if psync {
            self.input.pointer_frozen = true;
        }
    }

    pub fn deactivate_keyboard_grab(&mut self) {
        let Some(g) = self.input.keyboard_grab.take() else { return };
        self.input.keyboard_frozen = false;
        self.input.keyboard_once = false;
        if g.pointer_sync && !self.input.pointer_grab.as_ref().is_some_and(|p| p.pointer_sync) {
            self.input.pointer_frozen = false;
        }
        let focus = self.input.focus;
        self.focus_events(g.window, focus, UNGRAB);
        self.thaw_queue();
    }

    /// Grabs whose windows stopped being viewable end.
    pub fn check_grabs(&mut self) {
        let gone = |s: &Self, w: u32| w != 0 && !s.windows.get(&w).is_some_and(|w| w.viewable);
        if self.input.pointer_grab.as_ref().is_some_and(|g| gone(self, g.window) || gone(self, g.confine_to)) {
            self.deactivate_pointer_grab();
        }
        if self.input.keyboard_grab.as_ref().is_some_and(|g| gone(self, g.window)) {
            self.deactivate_keyboard_grab();
        }
    }

    // Crossing events.

    fn crossing_event(&self, code: u8, detail: u8, w: u32, child: u32, mode: u8) {
        let win = &self.windows[&w];
        let mask = if code == event::ENTER_NOTIFY { event::ENTER_WINDOW_MASK } else { event::LEAVE_WINDOW_MASK };
        let focus = self.input.focus;
        let in_focus = focus > 1 && (w == focus || self.is_ancestor(focus, w));
        let (x, y) = (self.input.x, self.input.y);
        let ev = Builder::new(code, detail)
            .u32(self.time())
            .u32(ROOT)
            .u32(w)
            .u32(child)
            .i16(x as i16)
            .i16(y as i16)
            .i16((x - win.ax) as i16)
            .i16((y - win.ay) as i16)
            .u16(self.state_mask())
            .u8(mode)
            .u8(2 | in_focus as u8)
            .done();
        for &(slot, m) in &win.events {
            let m = match &self.input.pointer_grab {
                Some(g) if g.slot != slot => 0,
                Some(g) => (if g.window == w { g.mask } else { 0 }) | if g.owner_events { m } else { 0 },
                None => m,
            };
            if m & mask != 0 {
                self.send(slot, &ev);
                if code == event::ENTER_NOTIFY {
                    self.keymap_notify(w, slot);
                }
            }
        }
    }

    fn keymap_notify(&self, w: u32, slot: u32) {
        if self.windows[&w].mask_of(slot) & event::KEYMAP_STATE_MASK != 0 {
            let ev = Builder::new(event::KEYMAP_NOTIFY, 0).done();
            let mut e = ev;
            e.0[1..32].copy_from_slice(&self.input.keys[1..32]);
            self.send(slot, &e);
        }
    }

    /// The common ancestor of two windows.
    fn common(&self, a: u32, b: u32) -> u32 {
        let pa = self.path_down(a);
        let pb = self.path_down(b);
        let mut c = ROOT;
        for (x, y) in pa.iter().zip(&pb) {
            if x != y {
                break;
            }
            c = *x;
        }
        c
    }

    /// Windows strictly between `top` and `bottom`, top first.
    fn between(&self, top: u32, bottom: u32) -> Vec<u32> {
        let path = self.path_down(bottom);
        match path.iter().position(|&w| w == top) {
            Some(i) => path[i + 1..path.len() - 1].to_vec(),
            None => Vec::new(),
        }
    }

    /// Enter and Leave events for the pointer going from window a to b.
    fn crossing(&mut self, a: u32, b: u32, mode: u8) {
        if a == b || !self.windows.contains_key(&a) || !self.windows.contains_key(&b) {
            if !self.windows.contains_key(&a) && self.windows.contains_key(&b) {
                self.crossing_event(event::ENTER_NOTIFY, NONLINEAR, b, 0, mode);
            }
            return;
        }
        use event::{ENTER_NOTIFY as EN, LEAVE_NOTIFY as LN};
        if self.is_ancestor(a, b) {
            self.crossing_event(LN, INFERIOR, a, self.child_toward(a, b), mode);
            for w in self.between(a, b) {
                self.crossing_event(EN, VIRTUAL, w, self.child_toward(w, b), mode);
            }
            self.crossing_event(EN, ANCESTOR, b, 0, mode);
        } else if self.is_ancestor(b, a) {
            self.crossing_event(LN, ANCESTOR, a, 0, mode);
            for w in self.between(b, a).into_iter().rev() {
                self.crossing_event(LN, VIRTUAL, w, self.child_toward(w, a), mode);
            }
            self.crossing_event(EN, INFERIOR, b, self.child_toward(b, a), mode);
        } else {
            let c = self.common(a, b);
            self.crossing_event(LN, NONLINEAR, a, 0, mode);
            for w in self.between(c, a).into_iter().rev() {
                self.crossing_event(LN, NONLINEAR_VIRTUAL, w, self.child_toward(w, a), mode);
            }
            for w in self.between(c, b) {
                self.crossing_event(EN, NONLINEAR_VIRTUAL, w, self.child_toward(w, b), mode);
            }
            self.crossing_event(EN, NONLINEAR, b, 0, mode);
        }
    }

    // Focus.

    fn focus_event(&self, code: u8, detail: u8, w: u32, mode: u8) {
        let Some(win) = self.windows.get(&w) else { return };
        let ev = Builder::new(code, detail).u32(w).u8(mode).done();
        for &(slot, m) in &win.events {
            if m & event::FOCUS_CHANGE_MASK != 0 {
                self.send(slot, &ev);
                if code == event::FOCUS_IN {
                    self.keymap_notify(w, slot);
                }
            }
        }
    }

    /// FocusOut with detail Pointer from the pointer's window up to `top`
    /// (excluded), or the root (included) for None.
    fn pointer_outs(&self, top: Option<u32>, mode: u8) {
        let mut w = self.input.sprite;
        while Some(w) != top {
            self.focus_event(event::FOCUS_OUT, POINTER, w, mode);
            if w == ROOT {
                break;
            }
            w = self.windows[&w].parent;
        }
    }

    fn pointer_ins(&self, top: u32, include_top: bool, mode: u8) {
        let path = self.path_down(self.input.sprite);
        let Some(i) = path.iter().position(|&w| w == top) else { return };
        for &w in &path[if include_top { i } else { i + 1 }..] {
            self.focus_event(event::FOCUS_IN, POINTER, w, mode);
        }
    }

    /// FocusIn and FocusOut for the focus going from a to b (windows, or
    /// None and PointerRoot).
    pub fn focus_events(&mut self, a: u32, b: u32, mode: u8) {
        use event::{FOCUS_IN as IN, FOCUS_OUT as OUT};
        if a == b {
            return;
        }
        let sprite = self.input.sprite;
        let real = |w: u32| w > 1;
        if !real(a) || !real(b) {
            match a {
                POINTER_ROOT_WIN => self.pointer_outs(None, mode),
                NONE => {}
                a => {
                    if self.is_ancestor(a, sprite) {
                        self.pointer_outs(Some(a), mode);
                    }
                    self.focus_event(OUT, NONLINEAR, a, mode);
                    let mut w = self.windows.get(&a).map_or(0, |w| w.parent);
                    while w != 0 {
                        self.focus_event(OUT, NONLINEAR_VIRTUAL, w, mode);
                        w = self.windows[&w].parent;
                    }
                }
            }
            if !real(a) {
                self.focus_event(OUT, if a == NONE { DETAIL_NONE } else { POINTER_ROOT }, ROOT, mode);
            }
            match b {
                NONE | POINTER_ROOT_WIN => {
                    self.focus_event(IN, if b == NONE { DETAIL_NONE } else { POINTER_ROOT }, ROOT, mode);
                    if b == POINTER_ROOT_WIN {
                        self.pointer_ins(ROOT, true, mode);
                    }
                }
                b => {
                    let mut ups = self.path_down(b);
                    ups.pop();
                    for w in ups {
                        self.focus_event(IN, NONLINEAR_VIRTUAL, w, mode);
                    }
                    self.focus_event(IN, NONLINEAR, b, mode);
                    if self.is_ancestor(b, sprite) {
                        self.pointer_ins(b, false, mode);
                    }
                }
            }
            return;
        }
        if self.is_ancestor(b, a) {
            if self.is_ancestor(a, sprite) {
                self.pointer_outs(Some(a), mode);
            }
            self.focus_event(OUT, ANCESTOR, a, mode);
            for w in self.between(b, a).into_iter().rev() {
                self.focus_event(OUT, VIRTUAL, w, mode);
            }
            self.focus_event(IN, INFERIOR, b, mode);
            if self.is_ancestor(b, sprite) && sprite != a && !self.is_ancestor(a, sprite) && !self.is_ancestor(sprite, a) {
                self.pointer_ins(b, false, mode);
            }
        } else if self.is_ancestor(a, b) {
            if self.is_ancestor(a, sprite) && sprite != b && !self.is_ancestor(b, sprite) && !self.is_ancestor(sprite, b) {
                self.pointer_outs(Some(a), mode);
            }
            self.focus_event(OUT, INFERIOR, a, mode);
            for w in self.between(a, b) {
                self.focus_event(IN, VIRTUAL, w, mode);
            }
            self.focus_event(IN, ANCESTOR, b, mode);
        } else {
            let c = self.common(a, b);
            if self.is_ancestor(a, sprite) {
                self.pointer_outs(Some(a), mode);
            }
            self.focus_event(OUT, NONLINEAR, a, mode);
            for w in self.between(c, a).into_iter().rev() {
                self.focus_event(OUT, NONLINEAR_VIRTUAL, w, mode);
            }
            for w in self.between(c, b) {
                self.focus_event(IN, NONLINEAR_VIRTUAL, w, mode);
            }
            self.focus_event(IN, NONLINEAR, b, mode);
            if self.is_ancestor(b, sprite) {
                self.pointer_ins(b, false, mode);
            }
        }
    }

    fn set_focus(&mut self, to: u32, revert: u8) {
        let from = self.input.focus;
        (self.input.focus, self.input.focus_revert) = (to, revert);
        // While the keyboard is grabbed, focus events say so.
        let mode = if self.input.keyboard_grab.is_some() { 3 } else { NORMAL };
        self.focus_events(from, to, mode);
    }

    /// The focus window can't keep it: where revert-to sends it.
    fn revert_focus(&mut self) {
        let f = self.input.focus;
        match self.input.focus_revert {
            1 => self.set_focus(POINTER_ROOT_WIN, 1),
            2 => {
                let mut w = self.windows.get(&f).map_or(ROOT, |w| w.parent);
                while let Some(win) = self.windows.get(&w) {
                    if win.viewable {
                        break;
                    }
                    w = win.parent;
                }
                self.set_focus(if w == 0 { ROOT } else { w }, 0);
            }
            _ => self.set_focus(NONE, 0),
        }
    }

    pub fn check_focus(&mut self) {
        let f = self.input.focus;
        if f > 1 && !self.windows.get(&f).is_some_and(|w| w.viewable) {
            self.revert_focus();
        }
    }

    /// A request's time: CurrentTime (0) is now; one before `last` or after
    /// now doesn't count.
    fn valid_time(&self, t: u32, last: u32) -> Option<u32> {
        let now = self.time();
        match t {
            0 => Some(now),
            t if t < last || t > now => None,
            t => Some(t),
        }
    }

    // The requests.

    pub fn set_input_focus(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let revert = ctx.data;
        let (focus, time) = (r.u32()?, r.u32()?);
        if revert > 2 {
            return Err(XError(BAD_VALUE, revert as u32));
        }
        if focus > 1 {
            let w = self.win(focus)?;
            if !w.viewable {
                return Err(XError(BAD_MATCH, focus));
            }
        }
        let Some(t) = self.valid_time(time, self.input.focus_time) else { return Ok(None) };
        self.input.focus_time = t;
        self.set_focus(focus, revert);
        Ok(None)
    }

    pub fn query_pointer(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let w = r.u32()?;
        let win = self.win(w)?;
        let (x, y) = (self.input.x, self.input.y);
        let child = win.children.iter().rev().copied().find(|&c| self.input.sprite == c || self.is_ancestor(c, self.input.sprite)).unwrap_or(0);
        let (wx, wy) = (x - win.ax, y - win.ay);
        self.input.hinted.retain(|h| h.0 != ctx.slot);
        let mut reply = ctx.reply(1);
        reply.u32(ROOT).u32(child).i16(x as i16).i16(y as i16).i16(wx as i16).i16(wy as i16).u16(self.state_mask());
        Ok(Some(reply.finish()))
    }

    pub fn warp_pointer(&mut self, _ctx: &Ctx, r: &mut Reader) -> Answer {
        let (src, dst) = (r.u32()?, r.u32()?);
        let (sx, sy, sw, sh) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32, r.u16()? as i32, r.u16()? as i32);
        let (dx, dy) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
        if src != 0 {
            let w = self.win(src)?;
            let (rw, rh) = (if sw == 0 { w.width as i32 - sx } else { sw }, if sh == 0 { w.height as i32 - sy } else { sh });
            let (px, py) = (self.input.x - w.ax, self.input.y - w.ay);
            if px < sx || py < sy || px >= sx + rw || py >= sy + rh || !w.clip.union(&w.inner).contains(self.input.x, self.input.y) {
                return Ok(None);
            }
        }
        let (x, y) = if dst != 0 {
            let w = self.win(dst)?;
            (w.ax + dx, w.ay + dy)
        } else {
            (self.input.x + dx, self.input.y + dy)
        };
        // STUB(x11): the warped pointer isn't shown: the front-end's own pointer stays where the user left it (phase 4)
        self.process(InputEvent::Motion { x, y }, None);
        Ok(None)
    }

    /// GrabPointer and GrabKeyboard's checks: the reply status, or None if fine.
    fn grab_status(&self, ctx: &Ctx, window: u32, confine: u32, time: u32, existing: &Option<Grab>, other_frozen: bool) -> Result<Option<u8>, XError> {
        // In X.org's order: AlreadyGrabbed, NotViewable, InvalidTime, Frozen.
        let w = self.win(window)?;
        let confine_ok = confine == 0 || self.win(confine)?.viewable;
        if existing.as_ref().is_some_and(|g| g.slot != ctx.slot) {
            return Ok(Some(1));
        }
        if !w.viewable || !confine_ok {
            return Ok(Some(3));
        }
        let last = existing.as_ref().map_or(self.input.grab_time, |g| g.time);
        if self.valid_time(time, last).is_none() {
            return Ok(Some(2));
        }
        if other_frozen {
            return Ok(Some(4));
        }
        Ok(None)
    }

    pub fn grab_pointer(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let owner = ctx.data != 0;
        let (window, mask) = (r.u32()?, r.u16()? as u32);
        let (pmode, kmode) = (r.u8()?, r.u8()?);
        let (confine, cursor, time) = (r.u32()?, r.u32()?, r.u32()?);
        if mask & !event::POINTER_EVENTS != 0 {
            return Err(XError(BAD_VALUE, mask));
        }
        if pmode > 1 || kmode > 1 {
            return Err(XError(BAD_VALUE, pmode.max(kmode) as u32));
        }
        let cursor = if cursor == 0 { None } else { Some(self.cursor_ref(cursor)?) };
        let frozen_by_other = self.input.keyboard_grab.as_ref().is_some_and(|g| g.slot != ctx.slot && g.pointer_sync && self.input.pointer_frozen);
        let status = match self.grab_status(ctx, window, confine, time, &self.input.pointer_grab, frozen_by_other)? {
            Some(s) => s,
            None => {
                let t = self.valid_time(time, 0).unwrap_or(0);
                let grab = Grab { slot: ctx.slot, window, owner_events: owner, mask, pointer_sync: pmode == 0, keyboard_sync: kmode == 0, confine_to: confine, cursor, time: t, passive: false, key: 0, activating: None };
                self.start_pointer_grab(grab);
                if confine != 0 {
                    let (x, y) = self.confine(confine, self.input.x, self.input.y);
                    if (x, y) != (self.input.x, self.input.y) {
                        self.process(InputEvent::Motion { x, y }, None);
                    }
                }
                0
            }
        };
        Ok(Some(ctx.reply(status).finish()))
    }

    pub fn ungrab_pointer(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let time = r.u32()?;
        if let Some(g) = &self.input.pointer_grab {
            if g.slot == ctx.slot && self.valid_time(time, g.time).is_some() {
                self.deactivate_pointer_grab();
            }
        }
        Ok(None)
    }

    pub fn change_active_pointer_grab(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let (cursor, time, mask) = (r.u32()?, r.u32()?, r.u16()? as u32);
        if mask & !event::POINTER_EVENTS != 0 {
            return Err(XError(BAD_VALUE, mask));
        }
        let cursor = if cursor == 0 { None } else { Some(self.cursor_ref(cursor)?) };
        let ok = self.input.pointer_grab.as_ref().is_some_and(|g| g.slot == ctx.slot && self.valid_time(time, g.time).is_some());
        if ok {
            let g = self.input.pointer_grab.as_mut().unwrap();
            (g.mask, g.cursor) = (mask, cursor);
            self.update_cursor();
        }
        Ok(None)
    }

    pub fn grab_keyboard(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let owner = ctx.data != 0;
        let (window, time) = (r.u32()?, r.u32()?);
        let (pmode, kmode) = (r.u8()?, r.u8()?);
        if pmode > 1 || kmode > 1 {
            return Err(XError(BAD_VALUE, pmode.max(kmode) as u32));
        }
        let frozen_by_other = self.input.pointer_grab.as_ref().is_some_and(|g| g.slot != ctx.slot && g.keyboard_sync && self.input.keyboard_frozen);
        let status = match self.grab_status(ctx, window, 0, time, &self.input.keyboard_grab, frozen_by_other)? {
            Some(s) => s,
            None => {
                let t = self.valid_time(time, 0).unwrap_or(0);
                let grab = Grab { slot: ctx.slot, window, owner_events: owner, mask: event::ALL_EVENTS, pointer_sync: pmode == 0, keyboard_sync: kmode == 0, confine_to: 0, cursor: None, time: t, passive: false, key: 0, activating: None };
                self.start_keyboard_grab(grab);
                0
            }
        };
        Ok(Some(ctx.reply(status).finish()))
    }

    pub fn ungrab_keyboard(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let time = r.u32()?;
        if let Some(g) = &self.input.keyboard_grab {
            if g.slot == ctx.slot && self.valid_time(time, g.time).is_some() {
                self.deactivate_keyboard_grab();
            }
        }
        Ok(None)
    }

    /// Two passive grabs that would both catch some press.
    fn overlap(a: &PassiveGrab, keyboard: bool, window: u32, detail: u8, modifiers: u16) -> bool {
        a.keyboard == keyboard && a.window == window && (a.detail == 0 || detail == 0 || a.detail == detail) && (a.modifiers == ANY_MODIFIER || modifiers == ANY_MODIFIER || a.modifiers == modifiers)
    }

    fn add_passive(&mut self, g: PassiveGrab) -> Answer {
        if self.passive.iter().any(|p| p.slot != g.slot && Self::overlap(p, g.keyboard, g.window, g.detail, g.modifiers)) {
            return Err(XError(BAD_ACCESS, g.window));
        }
        self.passive.retain(|p| !(p.slot == g.slot && p.keyboard == g.keyboard && p.window == g.window && p.detail == g.detail && p.modifiers == g.modifiers));
        self.passive.push(g);
        Ok(None)
    }

    fn remove_passive(&mut self, slot: u32, keyboard: bool, window: u32, detail: u8, modifiers: u16) {
        self.passive.retain(|p| {
            !(p.slot == slot && p.keyboard == keyboard && p.window == window && (detail == 0 || p.detail == detail) && (modifiers == ANY_MODIFIER || p.modifiers == modifiers))
        });
    }

    pub fn grab_button(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let owner = ctx.data != 0;
        let (window, mask) = (r.u32()?, r.u16()? as u32);
        let (pmode, kmode) = (r.u8()?, r.u8()?);
        let (confine, cursor) = (r.u32()?, r.u32()?);
        let button = r.u8()?;
        r.skip(1)?;
        let modifiers = r.u16()?;
        if mask & !event::POINTER_EVENTS != 0 {
            return Err(XError(BAD_VALUE, mask));
        }
        if pmode > 1 || kmode > 1 {
            return Err(XError(BAD_VALUE, pmode.max(kmode) as u32));
        }
        if modifiers != ANY_MODIFIER && modifiers & !0xff != 0 {
            return Err(XError(BAD_VALUE, modifiers as u32));
        }
        self.win(window)?;
        if confine != 0 {
            self.win(confine)?;
        }
        let cursor = if cursor == 0 { None } else { Some(self.cursor_ref(cursor)?) };
        let g = PassiveGrab { slot: ctx.slot, window, keyboard: false, detail: button, modifiers, owner_events: owner, mask, pointer_sync: pmode == 0, keyboard_sync: kmode == 0, confine_to: confine, cursor };
        self.add_passive(g)
    }

    pub fn ungrab_button(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let window = r.u32()?;
        let modifiers = r.u16()?;
        self.win(window)?;
        if modifiers != ANY_MODIFIER && modifiers & !0xff != 0 {
            return Err(XError(BAD_VALUE, modifiers as u32));
        }
        self.remove_passive(ctx.slot, false, window, ctx.data, modifiers);
        Ok(None)
    }

    pub fn grab_key(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let owner = ctx.data != 0;
        let window = r.u32()?;
        let modifiers = r.u16()?;
        let (key, pmode, kmode) = (r.u8()?, r.u8()?, r.u8()?);
        if key != 0 && key < keymap::MIN_KEYCODE {
            return Err(XError(BAD_VALUE, key as u32));
        }
        if pmode > 1 || kmode > 1 {
            return Err(XError(BAD_VALUE, pmode.max(kmode) as u32));
        }
        if modifiers != ANY_MODIFIER && modifiers & !0xff != 0 {
            return Err(XError(BAD_VALUE, modifiers as u32));
        }
        self.win(window)?;
        let g = PassiveGrab { slot: ctx.slot, window, keyboard: true, detail: key, modifiers, owner_events: owner, mask: event::ALL_EVENTS, pointer_sync: pmode == 0, keyboard_sync: kmode == 0, confine_to: 0, cursor: None };
        self.add_passive(g)
    }

    pub fn ungrab_key(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let window = r.u32()?;
        let modifiers = r.u16()?;
        self.win(window)?;
        let key = ctx.data;
        if key != 0 && key < keymap::MIN_KEYCODE {
            return Err(XError(BAD_VALUE, key as u32));
        }
        self.remove_passive(ctx.slot, true, window, key, modifiers);
        Ok(None)
    }

    pub fn allow_events(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let mode = ctx.data;
        let time = r.u32()?;
        if mode > 7 {
            return Err(XError(BAD_VALUE, mode as u32));
        }
        let mine = |g: &Option<Grab>| g.as_ref().filter(|g| g.slot == ctx.slot).map(|g| (g.time, g.window, g.activating));
        let pointer = mine(&self.input.pointer_grab);
        let keyboard = mine(&self.input.keyboard_grab);
        let ok = |s: &Self, g: Option<(u32, u32, Option<InputEvent>)>| g.is_some_and(|g| s.valid_time(time, g.0).is_some());
        match mode {
            0 | 6 if ok(self, pointer) || (mode == 6 && ok(self, keyboard)) => {
                self.input.pointer_frozen = false;
                if mode == 6 {
                    self.input.keyboard_frozen = false;
                }
            }
            1 | 7 if ok(self, pointer) || (mode == 7 && ok(self, keyboard)) => {
                if self.input.pointer_frozen {
                    self.input.pointer_frozen = false;
                    self.input.pointer_once = true;
                }
                if mode == 7 && self.input.keyboard_frozen {
                    self.input.keyboard_frozen = false;
                    self.input.keyboard_once = true;
                }
            }
            2 if ok(self, pointer) && self.input.pointer_frozen => {
                let (_, window, ev) = pointer.unwrap();
                self.input.pointer_grab = None;
                self.input.pointer_frozen = false;
                if let Some(ev) = ev {
                    // As if the grab hadn't been there: undo the press, replay it.
                    if let InputEvent::Button { button, .. } = ev {
                        if let Some(&l) = self.pointer_map.get(button as usize - 1) {
                            self.input.buttons &= !(1 << (l.max(1) - 1));
                        }
                    }
                    self.process(ev, Some(window));
                }
                self.update_cursor();
            }
            3 if ok(self, keyboard) => self.input.keyboard_frozen = false,
            4 if ok(self, keyboard) && self.input.keyboard_frozen => {
                self.input.keyboard_frozen = false;
                self.input.keyboard_once = true;
            }
            5 if ok(self, keyboard) && self.input.keyboard_frozen => {
                let (_, window, ev) = keyboard.unwrap();
                self.input.keyboard_grab = None;
                self.input.keyboard_frozen = false;
                if let Some(InputEvent::Key { code, down }) = ev {
                    self.input.keys[code as usize / 8] &= !(1 << (code % 8));
                    self.process(InputEvent::Key { code, down }, Some(window));
                }
            }
            _ => {}
        }
        self.thaw_queue();
        Ok(None)
    }
}
