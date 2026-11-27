//! The drawing requests: pixmaps, the poly requests (through the mi port),
//! ClearArea, CopyArea and CopyPlane, PutImage and GetImage. Images are in
//! the order the setup announces: bytes and bits least significant first,
//! scanlines padded to 32 bits; Xlib converts for big-endian clients.

use std::cell::RefCell;
use std::rc::Rc;

use x11rb_protocol::protocol::xproto;

use crate::draw::{depth_mask, read, Fill, Pixmap};
use crate::event::{self, Builder};
use crate::gc::Drawable;
use crate::mi;
use crate::region::{Box, Region};
use crate::server::*;
use crate::wire::Reader;

/// Bits per pixel in a ZPixmap of this depth.
pub fn bpp(depth: u8) -> Option<usize> {
    FORMATS.iter().find(|f| f.0 == depth).map(|f| f.1 as usize)
}

/// Bytes in a scanline of `bits` bits, padded to 32.
fn line_bytes(bits: usize) -> usize {
    bits.div_ceil(32) * 4
}

impl State {
    pub fn create_pixmap(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let depth = ctx.data;
        let id = self.new_id(ctx, r.u32()?)?;
        let d = r.u32()?;
        self.drawable(d)?;
        let (w, h) = (r.u16()?, r.u16()?);
        if w == 0 || h == 0 {
            return Err(XError(BAD_VALUE, 0));
        }
        if bpp(depth).is_none() {
            return Err(XError(BAD_VALUE, depth as u32));
        }
        // Each pixel is a u32 here: refuse what would take more than 256 MB.
        if w as usize * h as usize > 64 << 20 {
            return Err(XError(BAD_ALLOC, 0));
        }
        self.resources.insert(id, Resource::Pixmap(Rc::new(RefCell::new(Pixmap::new(w, h, depth)))));
        Ok(None)
    }

    pub fn poly(&mut self, ctx: &Ctx, major: u8, r: &mut Reader) -> Answer {
        let (d, gc) = self.target(r.u32()?, r.u32()?)?;
        let mode = |m: u8| match m {
            0 => Ok(mi::CoordMode::Origin),
            1 => Ok(mi::CoordMode::Previous),
            m => Err(XError(BAD_VALUE, m as u32)),
        };
        let s16 = |r: &mut Reader| -> Result<i32, XError> { Ok(r.u16()? as i16 as i32) };
        let u16_ = |r: &mut Reader| -> Result<i32, XError> { Ok(r.u16()? as i32) };
        enum Op {
            Points(mi::CoordMode, Vec<mi::Point>),
            Lines(mi::CoordMode, Vec<mi::Point>),
            Segments(Vec<mi::Segment>),
            Rects(Vec<mi::Rect>, bool),
            Arcs(Vec<mi::Arc>, bool),
            Poly(mi::Shape, mi::CoordMode, Vec<mi::Point>),
        }
        let points = |r: &mut Reader| -> Result<Vec<mi::Point>, XError> {
            let mut v = Vec::with_capacity(r.remaining() / 4);
            while r.remaining() >= 4 {
                v.push(mi::Point { x: s16(r)?, y: s16(r)? });
            }
            Ok(v)
        };
        let op = match major {
            xproto::POLY_POINT_REQUEST => Op::Points(mode(ctx.data)?, points(r)?),
            xproto::POLY_LINE_REQUEST => Op::Lines(mode(ctx.data)?, points(r)?),
            xproto::POLY_SEGMENT_REQUEST => {
                let mut v = Vec::new();
                while r.remaining() >= 8 {
                    v.push(mi::Segment { x1: s16(r)?, y1: s16(r)?, x2: s16(r)?, y2: s16(r)? });
                }
                Op::Segments(v)
            }
            xproto::POLY_RECTANGLE_REQUEST | xproto::POLY_FILL_RECTANGLE_REQUEST => {
                let mut v = Vec::new();
                while r.remaining() >= 8 {
                    v.push(mi::Rect { x: s16(r)?, y: s16(r)?, w: u16_(r)?, h: u16_(r)? });
                }
                Op::Rects(v, major == xproto::POLY_FILL_RECTANGLE_REQUEST)
            }
            xproto::POLY_ARC_REQUEST | xproto::POLY_FILL_ARC_REQUEST => {
                let mut v = Vec::new();
                while r.remaining() >= 12 {
                    v.push(mi::Arc { x: s16(r)?, y: s16(r)?, w: u16_(r)?, h: u16_(r)?, angle1: s16(r)?, angle2: s16(r)? });
                }
                Op::Arcs(v, major == xproto::POLY_FILL_ARC_REQUEST)
            }
            _ => {
                let shape = match r.u8()? {
                    0 => mi::Shape::Complex,
                    1 => mi::Shape::Nonconvex,
                    2 => mi::Shape::Convex,
                    s => return Err(XError(BAD_VALUE, s as u32)),
                };
                let m = mode(r.u8()?)?;
                r.skip(2)?;
                Op::Poly(shape, m, points(r)?)
            }
        };
        let Some(place) = self.place(&d, &gc) else { return Ok(None) };
        let fill = gc.fill(place.dx, place.dy);
        let g = gc.mi();
        self.with_canvas(&d, &place, gc.function, gc.plane_mask, &fill, gc.bg, |c| match &op {
            Op::Points(m, p) => mi::poly_point(c, &g, *m, p),
            Op::Lines(m, p) => mi::poly_line(c, &g, *m, p),
            Op::Segments(s) => mi::poly_segment(c, &g, s),
            Op::Rects(rs, false) => mi::poly_rectangle(c, &g, rs),
            Op::Rects(rs, true) => mi::poly_fill_rect(c, &g, rs),
            Op::Arcs(a, false) => mi::poly_arc(c, &g, a),
            Op::Arcs(a, true) => mi::poly_fill_arc(c, &g, a),
            Op::Poly(s, m, p) => mi::fill_polygon(c, &g, *s, *m, p),
        });
        Ok(None)
    }

    pub fn clear_area(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let exposures = ctx.data;
        let id = r.u32()?;
        let w = self.win(id)?;
        let (x, y) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
        let (mut cw, mut ch) = (r.u16()? as i32, r.u16()? as i32);
        if exposures > 1 {
            return Err(XError(BAD_VALUE, exposures as u32));
        }
        if w.input_only() {
            return Err(XError(BAD_MATCH, id));
        }
        if !w.viewable {
            return Ok(None);
        }
        if cw == 0 {
            cw = w.width as i32 - x;
        }
        if ch == 0 {
            ch = w.height as i32 - y;
        }
        let region = w.clip.intersect_box(Box::new(w.ax + x, w.ay + y, cw, ch));
        if region.is_empty() {
            return Ok(None);
        }
        self.paint_background(id, &region);
        if exposures != 0 {
            self.send_exposes(id, &region);
        }
        Ok(None)
    }

    /// What of a drawable can be read, in its own coordinates.
    fn readable(&self, d: &Drawable, include_inferiors: bool) -> Region {
        match d {
            Drawable::Window(w) => {
                let w = &self.windows[w];
                if !w.viewable {
                    return Region::new();
                }
                (if include_inferiors { &w.inner } else { &w.clip }).translated(-w.ax, -w.ay)
            }
            Drawable::Pixmap(p) => {
                let p = p.borrow();
                Region::rect(0, 0, p.width as i32, p.height as i32)
            }
        }
    }

    /// Pixels of a drawable, `None` where they can't be read.
    fn read_pixels(&self, d: &Drawable, avail: &Region, x: i32, y: i32, w: i32, h: i32) -> Vec<Vec<Option<u32>>> {
        match d {
            Drawable::Window(win) => {
                let win = &self.windows[win];
                let (px, stride, ox, oy, bounds) = self.buffer(win.buf);
                let (dx, dy) = (win.ax - ox, win.ay - oy);
                read(px, stride, &avail.translated(dx, dy).intersect(&bounds), x + dx, y + dy, w, h)
            }
            Drawable::Pixmap(p) => {
                let p = p.borrow();
                read(&p.px, p.width as usize, avail, x, y, w, h)
            }
        }
    }

    pub fn copy_area(&mut self, ctx: &Ctx, r: &mut Reader, plane: bool) -> Answer {
        let src = self.drawable(r.u32()?)?;
        let dst_id = r.u32()?;
        let (dst, gc) = self.target(dst_id, r.u32()?)?;
        let (sx, sy, dx, dy) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32, r.u16()? as i16 as i32, r.u16()? as i16 as i32);
        let (w, h) = (r.u16()? as i32, r.u16()? as i32);
        let src_depth = self.depth_of(&src);
        let bit = if plane {
            let bit = r.u32()?;
            if bit.count_ones() != 1 || bit > depth_mask(src_depth) {
                return Err(XError(BAD_VALUE, bit));
            }
            bit
        } else {
            if src_depth != gc.depth {
                return Err(XError(BAD_MATCH, 0));
            }
            0
        };
        if let Drawable::Window(s) = &src {
            if self.windows[s].input_only() {
                return Err(XError(BAD_MATCH, 0));
            }
        }
        let rect = Region::rect(sx, sy, w, h);
        let avail = self.readable(&src, gc.subwindow_mode == 1).intersect(&rect);
        let rows = self.read_pixels(&src, &avail, sx, sy, w, h);
        let place = self.place(&dst, &gc);
        if let Some(place) = &place {
            let (fg, bg) = (gc.fg & depth_mask(gc.depth), gc.bg & depth_mask(gc.depth));
            self.with_canvas(&dst, place, gc.function, gc.plane_mask, &Fill::Solid, 0, |c| {
                for (i, row) in rows.iter().enumerate() {
                    if plane {
                        let row: Vec<Option<u32>> = row.iter().map(|p| p.map(|v| if v & bit != 0 { fg } else { bg })).collect();
                        c.put_row(dx, dy + i as i32, &row);
                    } else {
                        c.put_row(dx, dy + i as i32, row);
                    }
                }
            });
        }
        // What couldn't be copied, where it would have gone.
        if gc.graphics_exposures {
            let mut lost = rect.subtract(&avail).translated(dx - sx, dy - sy);
            if let Drawable::Window(w) = &dst {
                let win = &self.windows[w];
                let clip = match &place {
                    Some(p) => p.clip.translated(-win.ax, -win.ay),
                    None => Region::new(),
                };
                lost = lost.intersect(&clip);
            }
            let n = lost.boxes().len();
            if n == 0 {
                let ev = Builder::new(event::NO_EXPOSURE, 0).u32(dst_id).u16(ctx.minor).u8(ctx.major).done();
                self.send(ctx.slot, &ev);
            }
            for (i, b) in lost.boxes().iter().enumerate() {
                let ev = Builder::new(event::GRAPHICS_EXPOSURE, 0)
                    .u32(dst_id)
                    .u16(b.x1 as u16)
                    .u16(b.y1 as u16)
                    .u16(b.width() as u16)
                    .u16(b.height() as u16)
                    .u16(ctx.minor)
                    .u16((n - 1 - i) as u16)
                    .u8(ctx.major)
                    .done();
                self.send(ctx.slot, &ev);
            }
        }
        Ok(None)
    }

    pub fn put_image(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let format = ctx.data;
        let (d, gc) = self.target(r.u32()?, r.u32()?)?;
        let (w, h) = (r.u16()? as usize, r.u16()? as usize);
        let (x, y) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
        let (left_pad, depth) = (r.u8()? as usize, r.u8()?);
        r.skip(2)?;
        let dd = self.depth_of(&d);
        let (fg, bg) = (gc.fg & depth_mask(dd), gc.bg & depth_mask(dd));
        let mut rows: Vec<Vec<Option<u32>>> = Vec::with_capacity(h);
        match format {
            0 | 1 => {
                if (format == 0 && depth != 1) || (format == 1 && depth != dd) {
                    return Err(XError(BAD_MATCH, 0));
                }
                if left_pad >= 32 {
                    return Err(XError(BAD_MATCH, 0));
                }
                let bpl = line_bytes(w + left_pad);
                let planes = if format == 0 { 1 } else { depth as usize };
                let data = r.bytes(bpl * h * planes)?;
                for row in 0..h {
                    let mut px = vec![0u32; w];
                    for p in 0..planes {
                        let line = &data[(p * h + row) * bpl..][..bpl];
                        // Most significant plane first.
                        let value = 1u32 << (planes - 1 - p);
                        for (i, v) in px.iter_mut().enumerate() {
                            let b = i + left_pad;
                            if line[b / 8] >> (b % 8) & 1 != 0 {
                                *v |= value;
                            }
                        }
                    }
                    rows.push(px.into_iter().map(|v| Some(if format == 0 { if v != 0 { fg } else { bg } } else { v })).collect());
                }
            }
            2 => {
                if depth != dd || left_pad != 0 {
                    return Err(XError(BAD_MATCH, 0));
                }
                let bits = bpp(depth).unwrap();
                let bpl = line_bytes(w * bits);
                let data = r.bytes(bpl * h)?;
                let mask = depth_mask(depth);
                for row in 0..h {
                    let line = &data[row * bpl..][..bpl];
                    let px = (0..w).map(|i| {
                        let v = match bits {
                            1 => (line[i / 8] >> (i % 8) & 1) as u32,
                            8 => line[i] as u32,
                            16 => u16::from_le_bytes([line[2 * i], line[2 * i + 1]]) as u32,
                            _ => u32::from_le_bytes(line[4 * i..4 * i + 4].try_into().unwrap()),
                        };
                        Some(v & mask)
                    });
                    rows.push(px.collect());
                }
            }
            f => return Err(XError(BAD_VALUE, f as u32)),
        }
        if r.remaining() > 3 {
            return Err(XError(BAD_LENGTH, 0));
        }
        let Some(place) = self.place(&d, &gc) else { return Ok(None) };
        self.with_canvas(&d, &place, gc.function, gc.plane_mask, &Fill::Solid, 0, |c| {
            for (i, row) in rows.iter().enumerate() {
                c.put_row(x, y + i as i32, row);
            }
        });
        Ok(None)
    }

    pub fn get_image(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let format = ctx.data;
        let d = self.drawable(r.u32()?)?;
        let (x, y, w, h) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32, r.u16()? as i32, r.u16()? as i32);
        let plane_mask = r.u32()?;
        if !(1..=2).contains(&format) {
            return Err(XError(BAD_VALUE, format as u32));
        }
        let (depth, visual, avail) = match &d {
            Drawable::Window(id) => {
                let win = &self.windows[id];
                let bw = win.border_width as i32;
                let (ax, ay) = (win.ax + x, win.ay + y);
                if !win.viewable
                    || win.input_only()
                    || x < -bw
                    || y < -bw
                    || x + w > win.width as i32 + bw
                    || y + h > win.height as i32 + bw
                    || ax < 0
                    || ay < 0
                    || ax + w > self.width as i32
                    || ay + h > self.height as i32
                {
                    return Err(XError(BAD_MATCH, 0));
                }
                // The screen as it is there, whatever covers the window.
                (win.depth, win.visual, Region::rect(x, y, w, h))
            }
            Drawable::Pixmap(p) => {
                let p = p.borrow();
                if x < 0 || y < 0 || x + w > p.width as i32 || y + h > p.height as i32 {
                    return Err(XError(BAD_MATCH, 0));
                }
                (p.depth, 0, Region::rect(x, y, w, h))
            }
        };
        let rows = self.read_pixels(&d, &avail, x, y, w, h);
        let mask = plane_mask & depth_mask(depth);
        let (w, h) = (w as usize, h as usize);
        let mut out = Vec::new();
        if format == 2 {
            let bits = bpp(depth).unwrap();
            let bpl = line_bytes(w * bits);
            out.resize(bpl * h, 0);
            for (row, px) in rows.iter().enumerate() {
                let line = &mut out[row * bpl..][..bpl];
                for (i, p) in px.iter().enumerate() {
                    let v = p.unwrap_or(0) & mask;
                    match bits {
                        1 => line[i / 8] |= ((v & 1) as u8) << (i % 8),
                        8 => line[i] = v as u8,
                        16 => line[2 * i..2 * i + 2].copy_from_slice(&(v as u16).to_le_bytes()),
                        _ => line[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes()),
                    }
                }
            }
        } else {
            let bpl = line_bytes(w);
            for plane in (0..depth as u32).rev().filter(|p| mask >> p & 1 != 0) {
                let start = out.len();
                out.resize(start + bpl * h, 0);
                for (row, px) in rows.iter().enumerate() {
                    let line = &mut out[start + row * bpl..][..bpl];
                    for (i, p) in px.iter().enumerate() {
                        if p.unwrap_or(0) >> plane & 1 != 0 {
                            line[i / 8] |= 1 << (i % 8);
                        }
                    }
                }
            }
        }
        let mut reply = ctx.reply(depth);
        reply.u32(visual).zeros(20).bytes(&out);
        Ok(Some(reply.finish()))
    }
}
