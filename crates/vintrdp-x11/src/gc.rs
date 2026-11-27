//! Graphics contexts and drawables: the GC requests, and what drawing with a
//! GC on a drawable needs (its clip, fill and canvas).

use std::rc::Rc;

use crate::draw::{depth_mask, Canvas, Fill, Pixmap, PixmapRef};
use crate::font::Font;
use crate::mi;
use crate::region::{Box, Region};
use crate::server::*;
use crate::wire::Reader;

#[derive(Clone)]
pub struct Gc {
    /// The depth of the drawable it was made for: it draws only on those.
    pub depth: u8,
    pub function: u8,
    pub plane_mask: u32,
    pub fg: u32,
    pub bg: u32,
    pub line_width: u16,
    pub line_style: u8,
    pub cap_style: u8,
    pub join_style: u8,
    pub fill_style: u8,
    pub fill_rule: u8,
    /// None: the default, a tile of `default_tile`.
    pub tile: Option<PixmapRef>,
    pub default_tile: u32,
    /// None: the default, all ones.
    pub stipple: Option<PixmapRef>,
    pub ts_x: i16,
    pub ts_y: i16,
    /// None: the server's default font.
    pub font: Option<Rc<Font>>,
    pub subwindow_mode: u8,
    pub graphics_exposures: bool,
    pub clip_x: i16,
    pub clip_y: i16,
    /// Relative to the clip origin; None for no clip mask.
    pub clip: Option<Region>,
    pub dash_offset: u16,
    pub dashes: Vec<u8>,
    pub arc_mode: u8,
}

impl Gc {
    pub fn new(depth: u8) -> Gc {
        Gc {
            depth,
            function: 3,
            plane_mask: !0,
            fg: 0,
            bg: 1,
            line_width: 0,
            line_style: 0,
            cap_style: 1,
            join_style: 0,
            fill_style: 0,
            fill_rule: 0,
            tile: None,
            default_tile: 0,
            stipple: None,
            ts_x: 0,
            ts_y: 0,
            font: None,
            subwindow_mode: 0,
            graphics_exposures: true,
            clip_x: 0,
            clip_y: 0,
            clip: None,
            dash_offset: 0,
            dashes: vec![4],
            arc_mode: 1,
        }
    }

    /// What scan conversion needs of it.
    pub fn mi(&self) -> mi::Gc {
        let mask = depth_mask(self.depth);
        mi::Gc {
            function: self.function,
            line_width: self.line_width as u32,
            line_style: [mi::LineStyle::Solid, mi::LineStyle::OnOffDash, mi::LineStyle::DoubleDash][self.line_style as usize],
            cap_style: [mi::CapStyle::NotLast, mi::CapStyle::Butt, mi::CapStyle::Round, mi::CapStyle::Projecting][self.cap_style as usize],
            join_style: [mi::JoinStyle::Miter, mi::JoinStyle::Round, mi::JoinStyle::Bevel][self.join_style as usize],
            fill_style: [mi::FillStyle::Solid, mi::FillStyle::Tiled, mi::FillStyle::Stippled, mi::FillStyle::OpaqueStippled][self.fill_style as usize],
            fill_rule: if self.fill_rule == 0 { mi::FillRule::EvenOdd } else { mi::FillRule::Winding },
            arc_mode: if self.arc_mode == 0 { mi::ArcMode::Chord } else { mi::ArcMode::PieSlice },
            fg: self.fg & mask,
            bg: self.bg & mask,
            dashes: self.dashes.clone(),
            dash_offset: self.dash_offset as u32,
        }
    }

    /// The fill for drawing with its fill style, the drawable's origin at
    /// (dx, dy) in the buffer.
    pub fn fill(&self, dx: i32, dy: i32) -> Fill {
        let (x, y) = (dx + self.ts_x as i32, dy + self.ts_y as i32);
        match self.fill_style {
            1 => match &self.tile {
                Some(t) => Fill::tile(&t.borrow(), x, y),
                None => Fill::tile(&Pixmap { width: 1, height: 1, depth: self.depth, px: vec![self.default_tile & depth_mask(self.depth)] }, x, y),
            },
            2 | 3 => match &self.stipple {
                Some(s) => Fill::stipple(&s.borrow(), x, y, self.fill_style == 3),
                None => Fill::stipple(&Pixmap { width: 1, height: 1, depth: 1, px: vec![1] }, x, y, self.fill_style == 3),
            },
            _ => Fill::Solid,
        }
    }
}

/// Something to draw on.
#[derive(Clone)]
pub enum Drawable {
    Window(u32),
    Pixmap(PixmapRef),
}

/// Where drawing on a drawable goes: its clip (buffer coordinates) and the
/// drawable's origin in the buffer.
pub struct Place {
    pub clip: Region,
    pub dx: i32,
    pub dy: i32,
}

impl State {
    pub fn drawable(&self, id: u32) -> Result<Drawable, XError> {
        if self.windows.contains_key(&id) {
            return Ok(Drawable::Window(id));
        }
        match self.resources.get(&id) {
            Some(Resource::Pixmap(p)) => Ok(Drawable::Pixmap(p.clone())),
            _ => Err(XError(BAD_DRAWABLE, id)),
        }
    }

    pub fn depth_of(&self, d: &Drawable) -> u8 {
        match d {
            Drawable::Window(w) => self.windows[w].depth,
            Drawable::Pixmap(p) => p.borrow().depth,
        }
    }

    pub fn pixmap(&self, id: u32) -> Result<PixmapRef, XError> {
        match self.resources.get(&id) {
            Some(Resource::Pixmap(p)) => Ok(p.clone()),
            _ => Err(XError(BAD_PIXMAP, id)),
        }
    }

    pub fn pixmap_of_depth(&self, id: u32, depth: u8) -> Result<PixmapRef, XError> {
        let p = self.pixmap(id)?;
        if p.borrow().depth != depth {
            return Err(XError(BAD_MATCH, id));
        }
        Ok(p)
    }

    pub fn gc(&mut self, id: u32) -> Result<&mut Gc, XError> {
        match self.resources.get_mut(&id) {
            Some(Resource::Gc(gc)) => Ok(gc),
            _ => Err(XError(BAD_GC, id)),
        }
    }

    /// A drawable and a GC for drawing: checked against each other.
    pub fn target(&self, drawable: u32, gc: u32) -> Result<(Drawable, Gc), XError> {
        let d = self.drawable(drawable)?;
        let Some(Resource::Gc(g)) = self.resources.get(&gc) else { return Err(XError(BAD_GC, gc)) };
        if let Drawable::Window(w) = &d {
            if self.windows[w].input_only() {
                return Err(XError(BAD_MATCH, drawable));
            }
        }
        if self.depth_of(&d) != g.depth {
            return Err(XError(BAD_MATCH, gc));
        }
        Ok((d, g.clone()))
    }

    /// Where drawing goes; None if nothing would show (an unviewable window).
    pub fn place(&self, d: &Drawable, gc: &Gc) -> Option<Place> {
        let (base, dx, dy) = match d {
            Drawable::Window(w) => {
                let w = &self.windows[w];
                if !w.viewable {
                    return None;
                }
                (if gc.subwindow_mode == 1 { w.inner.clone() } else { w.clip.clone() }, w.ax, w.ay)
            }
            Drawable::Pixmap(p) => {
                let p = p.borrow();
                (Region::rect(0, 0, p.width as i32, p.height as i32), 0, 0)
            }
        };
        let clip = match &gc.clip {
            Some(c) => base.intersect(&c.translated(dx + gc.clip_x as i32, dy + gc.clip_y as i32)),
            None => base,
        };
        (!clip.is_empty()).then_some(Place { clip, dx, dy })
    }

    /// Run `f` on a canvas for the drawable, and show what it drew.
    pub fn with_canvas<R>(&mut self, d: &Drawable, place: &Place, function: u8, plane_mask: u32, fill: &Fill, bg: u32, f: impl FnOnce(&mut Canvas) -> R) -> R {
        match d {
            Drawable::Window(_) => {
                let stride = self.width as usize;
                let mask = depth_mask(ROOT_DEPTH);
                let mut c = Canvas { px: &mut self.fb, stride, dx: place.dx, dy: place.dy, clip: &place.clip, function, plane_mask: plane_mask & mask, depth_mask: mask, fill, bg: bg & mask, touched: None };
                let out = f(&mut c);
                if let Some(t) = c.touched {
                    self.damage(t);
                }
                out
            }
            Drawable::Pixmap(p) => {
                let mut p = p.borrow_mut();
                let (stride, mask) = (p.width as usize, depth_mask(p.depth));
                let mut c = Canvas { px: &mut p.px, stride, dx: 0, dy: 0, clip: &place.clip, function, plane_mask: plane_mask & mask, depth_mask: mask, fill, bg: bg & mask, touched: None };
                f(&mut c)
            }
        }
    }

    pub fn create_gc(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.new_id(ctx, r.u32()?)?;
        let d = self.drawable(r.u32()?)?;
        let depth = match &d {
            Drawable::Window(w) if self.windows[w].input_only() => return Err(XError(BAD_MATCH, 0)),
            _ => self.depth_of(&d),
        };
        let mut gc = Gc::new(depth);
        let values = self.gc_values(r, &gc)?;
        apply(&mut gc, values);
        // The default tile is the foreground the GC was made with.
        gc.default_tile = gc.fg;
        self.resources.insert(id, Resource::Gc(gc));
        Ok(None)
    }

    pub fn change_gc(&mut self, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        let gc = self.gc(id)?.clone();
        let values = self.gc_values(r, &gc)?;
        apply(self.gc(id)?, values);
        Ok(None)
    }

    pub fn copy_gc(&mut self, r: &mut Reader) -> Answer {
        let src = self.gc(r.u32()?)?.clone();
        let dst_id = r.u32()?;
        let dst = self.gc(dst_id)?;
        let mask = r.u32()?;
        if mask >> 23 != 0 {
            return Err(XError(BAD_VALUE, mask));
        }
        if src.depth != dst.depth {
            return Err(XError(BAD_MATCH, dst_id));
        }
        for bit in (0..23).filter(|b| mask >> b & 1 != 0) {
            match bit {
                0 => dst.function = src.function,
                1 => dst.plane_mask = src.plane_mask,
                2 => dst.fg = src.fg,
                3 => dst.bg = src.bg,
                4 => dst.line_width = src.line_width,
                5 => dst.line_style = src.line_style,
                6 => dst.cap_style = src.cap_style,
                7 => dst.join_style = src.join_style,
                8 => dst.fill_style = src.fill_style,
                9 => dst.fill_rule = src.fill_rule,
                10 => (dst.tile, dst.default_tile) = (src.tile.clone(), src.default_tile),
                11 => dst.stipple = src.stipple.clone(),
                12 => dst.ts_x = src.ts_x,
                13 => dst.ts_y = src.ts_y,
                14 => dst.font = src.font.clone(),
                15 => dst.subwindow_mode = src.subwindow_mode,
                16 => dst.graphics_exposures = src.graphics_exposures,
                17 => dst.clip_x = src.clip_x,
                18 => dst.clip_y = src.clip_y,
                19 => dst.clip = src.clip.clone(),
                20 => dst.dash_offset = src.dash_offset,
                21 => dst.dashes = src.dashes.clone(),
                _ => dst.arc_mode = src.arc_mode,
            }
        }
        Ok(None)
    }

    pub fn set_dashes(&mut self, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        self.gc(id)?;
        let (offset, n) = (r.u16()?, r.u16()? as usize);
        let dashes = r.bytes(n)?.to_vec();
        if n == 0 {
            return Err(XError(BAD_VALUE, 0));
        }
        if dashes.contains(&0) {
            return Err(XError(BAD_VALUE, 0));
        }
        let gc = self.gc(id)?;
        (gc.dash_offset, gc.dashes) = (offset, dashes);
        Ok(None)
    }

    pub fn set_clip_rectangles(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let ordering = ctx.data;
        let id = r.u32()?;
        self.gc(id)?;
        let (x, y) = (r.u16()? as i16, r.u16()? as i16);
        if ordering > 3 {
            return Err(XError(BAD_VALUE, ordering as u32));
        }
        if r.remaining() % 8 != 0 {
            return Err(XError(BAD_LENGTH, 0));
        }
        let mut rects = Vec::new();
        while r.remaining() > 0 {
            let (rx, ry, w, h) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32, r.u16()? as i32, r.u16()? as i32);
            rects.push(Box::new(rx, ry, w, h));
        }
        if !ordered(&rects, ordering) {
            return Err(XError(BAD_MATCH, 0));
        }
        let gc = self.gc(id)?;
        (gc.clip_x, gc.clip_y, gc.clip) = (x, y, Some(Region::from_boxes(&rects)));
        Ok(None)
    }

    /// A GC's value list, checked against the server's resources.
    fn gc_values(&self, r: &mut Reader, gc: &Gc) -> Result<Vec<(u32, u32, GcValue)>, XError> {
        let mask = r.u32()?;
        if mask >> 23 != 0 {
            return Err(XError(BAD_VALUE, mask));
        }
        let mut out = Vec::new();
        for bit in (0..23).filter(|b| mask >> b & 1 != 0) {
            let v = r.u32()?;
            let max = match bit {
                0 => 15,
                5 | 7 => 2,
                6 | 8 => 3,
                9 | 15 | 16 | 22 => 1,
                4 | 20 => 0xffff,
                21 => 0xff,
                _ => !0,
            };
            if v > max || (bit == 21 && v == 0) {
                return Err(XError(BAD_VALUE, v));
            }
            let extra = match bit {
                10 => GcValue::Pixmap(Some(self.pixmap_of_depth(v, gc.depth)?)),
                11 => GcValue::Pixmap(Some(self.pixmap_of_depth(v, 1)?)),
                14 => GcValue::Font(self.font(v)?),
                19 if v == 0 => GcValue::Pixmap(None),
                19 => GcValue::Pixmap(Some(self.pixmap_of_depth(v, 1)?)),
                _ => GcValue::None,
            };
            out.push((bit, v, extra));
        }
        Ok(out)
    }
}

pub enum GcValue {
    None,
    Pixmap(Option<PixmapRef>),
    Font(Rc<Font>),
}

fn apply(gc: &mut Gc, values: Vec<(u32, u32, GcValue)>) {
    for (bit, v, extra) in values {
        match (bit, extra) {
            (0, _) => gc.function = v as u8,
            (1, _) => gc.plane_mask = v,
            (2, _) => gc.fg = v,
            (3, _) => gc.bg = v,
            (4, _) => gc.line_width = v as u16,
            (5, _) => gc.line_style = v as u8,
            (6, _) => gc.cap_style = v as u8,
            (7, _) => gc.join_style = v as u8,
            (8, _) => gc.fill_style = v as u8,
            (9, _) => gc.fill_rule = v as u8,
            (10, GcValue::Pixmap(p)) => gc.tile = p,
            (11, GcValue::Pixmap(p)) => gc.stipple = p,
            (12, _) => gc.ts_x = v as i16,
            (13, _) => gc.ts_y = v as i16,
            (14, GcValue::Font(f)) => gc.font = Some(f),
            (15, _) => gc.subwindow_mode = v as u8,
            (16, _) => gc.graphics_exposures = v != 0,
            (17, _) => gc.clip_x = v as i16,
            (18, _) => gc.clip_y = v as i16,
            // A clip mask becomes a region when it's set, as in X.org.
            (19, GcValue::Pixmap(p)) => gc.clip = p.map(|p| bitmap_region(&p.borrow())),
            (20, _) => gc.dash_offset = v as u16,
            (21, _) => gc.dashes = vec![v as u8],
            (22, _) => gc.arc_mode = v as u8,
            _ => {}
        }
    }
}

/// The 1 bits of a bitmap, as a region.
pub fn bitmap_region(p: &Pixmap) -> Region {
    let mut boxes = Vec::new();
    for y in 0..p.height as i32 {
        let mut x = 0;
        while x < p.width as i32 {
            if p.get(x, y) == 0 {
                x += 1;
                continue;
            }
            let start = x;
            while x < p.width as i32 && p.get(x, y) != 0 {
                x += 1;
            }
            boxes.push(Box::new(start, y, x - start, 1));
        }
    }
    Region::from_boxes(&boxes)
}

/// SetClipRectangles' ordering promise: Unsorted, YSorted, YXSorted, YXBanded.
fn ordered(rects: &[Box], ordering: u8) -> bool {
    rects.windows(2).all(|w| {
        let (a, b) = (w[0], w[1]);
        match ordering {
            1 => b.y1 >= a.y1,
            2 => b.y1 > a.y1 || (b.y1 == a.y1 && b.x1 >= a.x1),
            3 => b.y1 >= a.y2 || (b.y1 == a.y1 && b.y2 == a.y2 && b.x1 >= a.x1),
            _ => true,
        }
    })
}
