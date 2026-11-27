//! The SHAPE extension, version 1.1: windows of any shape (xeyes, xlogo
//! -shape), and input shapes.

use crate::event::{self, Builder};
use crate::gc::bitmap_region;
use crate::region::{Box, Region};
use crate::server::*;
use crate::wire::Reader;

impl State {
    /// A window's shape of a kind as set, or its default when it has none.
    fn shape_or_default(&self, w: u32, kind: u8) -> Region {
        let win = &self.windows[&w];
        match &win.shape[kind as usize] {
            Some(s) => s.clone(),
            None if kind == 1 => win.default_clip(),
            None => win.default_bounding(),
        }
    }

    fn set_shape(&mut self, w: u32, kind: u8, op: u8, src: Option<Region>) -> Result<(), XError> {
        let base = || {
            self.windows[&w].shape[kind as usize]
                .clone()
                .unwrap_or_else(|| self.shape_or_default(w, kind))
        };
        let new = match (op, src) {
            (0, src) => src,
            (_, None) => return Err(XError(BAD_MATCH, 0)),
            (1, Some(s)) => Some(base().union(&s)),
            (2, Some(s)) => Some(base().intersect(&s)),
            (3, Some(s)) => Some(base().subtract(&s)),
            (_, Some(s)) => Some(s.subtract(&base())),
        };
        let snap = self.snapshot();
        self.windows.get_mut(&w).unwrap().shape[kind as usize] = new;
        self.shape_notify(w, kind);
        self.exposures(snap);
        self.pointer_moved();
        Ok(())
    }

    fn shape_notify(&self, w: u32, kind: u8) {
        let win = &self.windows[&w];
        let e = self.shape_or_default(w, kind).extents();
        let ev = Builder::new(event::SHAPE_NOTIFY, kind)
            .u32(w)
            .i16(e.x1 as i16)
            .i16(e.y1 as i16)
            .u16(e.width() as u16)
            .u16(e.height() as u16)
            .u32(self.now())
            .bool(win.shape[kind as usize].is_some())
            .done();
        for &slot in &win.shape_events {
            self.send(slot, &ev);
        }
    }

    pub fn shape(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let check = |op: u8, kind: u8| {
            if op > 4 {
                Err(XError(BAD_VALUE, op as u32))
            } else if kind > 2 {
                Err(XError(BAD_VALUE, kind as u32))
            } else {
                Ok(())
            }
        };
        match ctx.data {
            // QueryVersion: 1.1.
            0 => {
                let mut reply = ctx.reply(0);
                reply.u16(1).u16(1);
                Ok(Some(reply.finish()))
            }
            // Rectangles.
            1 => {
                let (op, kind, ordering) = (r.u8()?, r.u8()?, r.u8()?);
                r.skip(1)?;
                let w = r.u32()?;
                let (x, y) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
                check(op, kind)?;
                if ordering > 3 {
                    return Err(XError(BAD_VALUE, ordering as u32));
                }
                self.win(w)?;
                let mut boxes = Vec::new();
                while r.remaining() >= 8 {
                    let (rx, ry, rw, rh) = (
                        r.u16()? as i16 as i32,
                        r.u16()? as i16 as i32,
                        r.u16()? as i32,
                        r.u16()? as i32,
                    );
                    boxes.push(Box::new(rx + x, ry + y, rw, rh));
                }
                self.set_shape(w, kind, op, Some(Region::from_boxes(&boxes)))?;
                Ok(None)
            }
            // Mask.
            2 => {
                let (op, kind) = (r.u8()?, r.u8()?);
                r.skip(2)?;
                let w = r.u32()?;
                let (x, y) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
                let src = r.u32()?;
                check(op, kind)?;
                self.win(w)?;
                let region = if src == 0 {
                    None
                } else {
                    let p = self.pixmap_of_depth(src, 1)?;
                    let r = bitmap_region(&p.borrow()).translated(x, y);
                    Some(r)
                };
                self.set_shape(w, kind, op, region)?;
                Ok(None)
            }
            // Combine.
            3 => {
                let (op, kind, src_kind) = (r.u8()?, r.u8()?, r.u8()?);
                r.skip(1)?;
                let w = r.u32()?;
                let (x, y) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
                let src = r.u32()?;
                check(op, kind)?;
                check(0, src_kind)?;
                self.win(w)?;
                self.win(src)?;
                let region = self.shape_or_default(src, src_kind).translated(x, y);
                self.set_shape(w, kind, op, Some(region))?;
                Ok(None)
            }
            // Offset.
            4 => {
                let kind = r.u8()?;
                r.skip(3)?;
                let w = r.u32()?;
                let (x, y) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
                check(0, kind)?;
                self.win(w)?;
                if self.windows[&w].shape[kind as usize].is_some() && (x, y) != (0, 0) {
                    let snap = self.snapshot();
                    if let Some(s) = &mut self.windows.get_mut(&w).unwrap().shape[kind as usize] {
                        s.translate(x, y);
                    }
                    self.shape_notify(w, kind);
                    self.exposures(snap);
                    self.pointer_moved();
                }
                Ok(None)
            }
            // QueryExtents.
            5 => {
                let w = r.u32()?;
                let win = self.win(w)?;
                let (b, c) = (
                    self.shape_or_default(w, 0).extents(),
                    self.shape_or_default(w, 1).extents(),
                );
                let mut reply = ctx.reply(0);
                reply
                    .u8(win.shape[0].is_some() as u8)
                    .u8(win.shape[1].is_some() as u8)
                    .zeros(2);
                for e in [b, c] {
                    reply
                        .i16(e.x1 as i16)
                        .i16(e.y1 as i16)
                        .u16(e.width() as u16)
                        .u16(e.height() as u16);
                }
                Ok(Some(reply.finish()))
            }
            // SelectInput.
            6 => {
                let w = r.u32()?;
                let enable = r.u8()?;
                if enable > 1 {
                    return Err(XError(BAD_VALUE, enable as u32));
                }
                let win = self.win_mut(w)?;
                win.shape_events.retain(|&s| s != ctx.slot);
                if enable == 1 {
                    win.shape_events.push(ctx.slot);
                }
                Ok(None)
            }
            // InputSelected.
            7 => {
                let w = r.u32()?;
                let on = self.win(w)?.shape_events.contains(&ctx.slot);
                Ok(Some(ctx.reply(on as u8).finish()))
            }
            // GetRectangles.
            8 => {
                let w = r.u32()?;
                let kind = r.u8()?;
                check(0, kind)?;
                self.win(w)?;
                let region = self.shape_or_default(w, kind);
                let mut reply = ctx.reply(3); // YXBanded
                reply.u32(region.boxes().len() as u32).zeros(20);
                for b in region.boxes() {
                    reply
                        .i16(b.x1 as i16)
                        .i16(b.y1 as i16)
                        .u16(b.width() as u16)
                        .u16(b.height() as u16);
                }
                Ok(Some(reply.finish()))
            }
            _ => Err(XError(BAD_REQUEST, 0)),
        }
    }
}
