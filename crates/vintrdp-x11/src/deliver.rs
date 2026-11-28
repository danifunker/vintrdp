//! Selections and SendEvent: events clients send each other through the
//! server.

use crate::event::{self, Builder, Event};
use crate::server::*;
use crate::wire::Reader;

impl State {
    pub fn set_selection_owner(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let (owner, selection, time) = (r.u32()?, r.u32()?, r.u32()?);
        if owner != 0 {
            self.win(owner)?;
        }
        self.atom(selection)?;
        let now = self.now();
        let time = if time == 0 { now } else { time };
        let i = self.selections.iter().position(|s| s.atom == selection);
        if let Some(i) = i {
            let s = &self.selections[i];
            // Too old, or in the future: ignored.
            if time < s.time || time > now {
                return Ok(None);
            }
            if s.slot != ctx.slot || owner == 0 {
                let ev = Builder::new(event::SELECTION_CLEAR, 0).u32(time).u32(s.window).u32(selection).done();
                self.send(s.slot, &ev);
            }
            self.selections.remove(i);
        } else if time > now {
            return Ok(None);
        }
        if owner != 0 {
            self.selections.push(Selection { atom: selection, window: owner, slot: ctx.slot, time });
            self.selection_taken(selection, ctx.slot);
        }
        Ok(None)
    }

    pub fn convert_selection(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let (requestor, selection, target, property, time) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        self.win(requestor)?;
        self.atom(selection)?;
        self.atom(target)?;
        if property != 0 {
            self.atom(property)?;
        }
        match self.selections.iter().find(|s| s.atom == selection) {
            // The server's own: the local clipboard's text (clipboard.rs).
            Some(s) if s.slot == 0 => self.convert_own(requestor, selection, target, property, time),
            Some(s) => {
                let ev = Builder::new(event::SELECTION_REQUEST, 0).u32(time).u32(s.window).u32(requestor).u32(selection).u32(target).u32(property).done();
                self.send(s.slot, &ev);
            }
            None => {
                let ev = Builder::new(event::SELECTION_NOTIFY, 0).u32(time).u32(requestor).u32(selection).u32(target).u32(0).done();
                self.send(ctx.slot, &ev);
            }
        }
        Ok(None)
    }

    pub fn send_event(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let propagate = ctx.data != 0;
        let (dest, mask) = (r.u32()?, r.u32()?);
        let bytes: [u8; 32] = r.bytes(32)?.try_into().unwrap();
        if mask & !event::ALL_EVENTS != 0 {
            return Err(XError(BAD_VALUE, mask));
        }
        let code = bytes[0] & 0x7f;
        if code < 2 || (code >= 35 && code != event::SHAPE_NOTIFY) {
            return Err(XError(BAD_VALUE, code as u32));
        }
        let Some(mut ev) = Event::from_client(bytes, ctx.order) else { return Err(XError(BAD_VALUE, code as u32)) };
        ev.0[0] |= 0x80;
        if dest == crate::clipboard::SELECTION_WINDOW && code == event::SELECTION_NOTIFY {
            self.selection_notified(&ev);
            return Ok(None);
        }
        let sprite = self.input.sprite;
        let (window, focus_limit) = match dest {
            0 => (sprite, None),
            1 => match self.input.focus {
                0 => return Ok(None),
                1 => (sprite, None),
                f if sprite == f || self.is_ancestor(f, sprite) => (sprite, Some(f)),
                f => (f, Some(f)),
            },
            w => {
                self.win(w)?;
                (w, None)
            }
        };
        if mask == 0 {
            // To whoever made the window.
            let slot = window >> ID_BITS;
            if slot != 0 {
                self.send(slot, &ev);
            }
            return Ok(None);
        }
        let mut w = window;
        loop {
            if self.deliver(w, mask, &ev) > 0 || !propagate {
                break;
            }
            let Some(win) = self.windows.get(&w) else { break };
            if win.do_not_propagate & mask != 0 || Some(w) == focus_limit || win.parent == 0 {
                break;
            }
            w = win.parent;
        }
        Ok(None)
    }
}
