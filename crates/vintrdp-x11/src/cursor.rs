//! Cursors: from bitmaps or the cursor font's glyphs, shown through the
//! host as the one for wherever the pointer is (or the pointer grab's).

use std::cell::Cell;
use std::rc::Rc;

use crate::server::*;
use crate::wire::Reader;
use crate::CursorImage;

pub struct Cursor {
    pub width: usize,
    pub height: usize,
    pub hot_x: usize,
    pub hot_y: usize,
    pub source: Vec<bool>,
    pub mask: Vec<bool>,
    pub fore: Cell<(u16, u16, u16)>,
    pub back: Cell<(u16, u16, u16)>,
}

impl Cursor {
    pub fn image(&self) -> CursorImage {
        let (f, b) = (self.fore.get(), self.back.get());
        let mut rgba = Vec::with_capacity(self.width * self.height * 4);
        for i in 0..self.width * self.height {
            let (r, g, bl) = if self.source[i] { f } else { b };
            rgba.extend_from_slice(&[(r >> 8) as u8, (g >> 8) as u8, (bl >> 8) as u8, if self.mask[i] { 255 } else { 0 }]);
        }
        CursorImage { width: self.width, height: self.height, hot_x: self.hot_x, hot_y: self.hot_y, rgba }
    }
}

/// XC_X_cursor: what the root shows when it has no cursor.
const X_CURSOR: u16 = 0;

impl State {
    pub fn cursor_ref(&self, id: u32) -> Result<Rc<Cursor>, XError> {
        match self.resources.get(&id) {
            Some(Resource::Cursor(c)) => Ok(c.clone()),
            _ => Err(XError(BAD_CURSOR, id)),
        }
    }

    fn rgb(r: &mut Reader) -> Result<((u16, u16, u16), (u16, u16, u16)), XError> {
        Ok(((r.u16()?, r.u16()?, r.u16()?), (r.u16()?, r.u16()?, r.u16()?)))
    }

    pub fn create_cursor(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.new_id(ctx, r.u32()?)?;
        let source = self.pixmap(r.u32()?)?;
        let mask_id = r.u32()?;
        let mask = if mask_id == 0 { None } else { Some(self.pixmap(mask_id)?) };
        let (fore, back) = Self::rgb(r)?;
        let (x, y) = (r.u16()? as usize, r.u16()? as usize);
        let s = source.borrow();
        if s.depth != 1 {
            return Err(XError(BAD_MATCH, 0));
        }
        let (w, h) = (s.width as usize, s.height as usize);
        let mask = match &mask {
            Some(m) => {
                let m = m.borrow();
                if m.depth != 1 || (m.width as usize, m.height as usize) != (w, h) {
                    return Err(XError(BAD_MATCH, 0));
                }
                m.px.iter().map(|&p| p != 0).collect()
            }
            None => vec![true; w * h],
        };
        if x >= w || y >= h {
            return Err(XError(BAD_MATCH, 0));
        }
        let c = Cursor { width: w, height: h, hot_x: x, hot_y: y, source: s.px.iter().map(|&p| p != 0).collect(), mask, fore: Cell::new(fore), back: Cell::new(back) };
        drop(s);
        self.resources.insert(id, Resource::Cursor(Rc::new(c)));
        Ok(None)
    }

    pub fn create_glyph_cursor(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.new_id(ctx, r.u32()?)?;
        let source = self.font(r.u32()?)?;
        let mask_id = r.u32()?;
        let mask = if mask_id == 0 { None } else { Some(self.font(mask_id)?) };
        let (sc, mc) = (r.u16()?, r.u16()?);
        let (fore, back) = Self::rgb(r)?;
        let c = glyph_cursor(&source, sc, mask.as_deref().map(|m| (m, mc)), fore, back)?;
        self.resources.insert(id, Resource::Cursor(Rc::new(c)));
        Ok(None)
    }

    pub fn recolor_cursor(&mut self, r: &mut Reader) -> Answer {
        let c = self.cursor_ref(r.u32()?)?;
        let (fore, back) = Self::rgb(r)?;
        c.fore.set(fore);
        c.back.set(back);
        self.input.shown_cursor = None;
        self.update_cursor();
        Ok(None)
    }

    /// The cursor for where the pointer is: the pointer grab's, or the
    /// window's or its nearest ancestor's.
    pub fn current_cursor(&mut self) -> Rc<Cursor> {
        if let Some(c) = self.input.pointer_grab.as_ref().and_then(|g| g.cursor.clone()) {
            return c;
        }
        let mut w = self.input.sprite;
        while let Some(win) = self.windows.get(&w) {
            if let Some(c) = &win.cursor {
                return c.clone();
            }
            w = win.parent;
        }
        if let Some(c) = &self.input.default_cursor {
            return c.clone();
        }
        let font = self.fonts.open(b"cursor").expect("cursor is bundled");
        let c = Rc::new(glyph_cursor(&font, X_CURSOR, Some((&font, X_CURSOR + 1)), (0, 0, 0), (0xffff, 0xffff, 0xffff)).expect("X_cursor"));
        self.input.default_cursor = Some(c.clone());
        c
    }

    /// Show the current cursor, if it changed.
    pub fn update_cursor(&mut self) {
        let c = self.current_cursor();
        if self.input.shown_cursor.as_ref().is_some_and(|s| Rc::ptr_eq(s, &c)) {
            return;
        }
        self.host.cursor(&c.image());
        self.input.shown_cursor = Some(c);
    }
}

/// A cursor from a glyph and its mask glyph, as X.org's ServerBitsFromGlyph:
/// the box both cover, the hot spot at the glyphs' origin.
fn glyph_cursor(font: &crate::font::Font, sc: u16, mask: Option<(&crate::font::Font, u16)>, fore: (u16, u16, u16), back: (u16, u16, u16)) -> Result<Cursor, XError> {
    let exists = |f: &crate::font::Font, c: u16| f.exact(c).is_some();
    if !exists(font, sc) {
        return Err(XError(BAD_VALUE, sc as u32));
    }
    let src = font.glyph(sc).unwrap();
    let msk = match mask {
        Some((f, c)) => {
            if !exists(f, c) {
                return Err(XError(BAD_VALUE, c as u32));
            }
            Some(f.glyph(c).unwrap())
        }
        None => None,
    };
    // The cursor's box is the mask's (or the source's), as X.org has it.
    let bm = msk.map_or(src.m, |m| m.m);
    let (x0, y0) = (bm.lb as i32, -(bm.ascent as i32));
    let (w, h) = ((bm.rb - bm.lb).max(1) as usize, (bm.ascent + bm.descent).max(1) as usize);
    let plot = |g: &crate::font::Glyph, out: &mut Vec<bool>| {
        for row in 0..g.height() {
            let bits = g.row(row);
            for col in 0..bits.len() as i32 * 64 {
                if bits[col as usize / 64] >> (col % 64) & 1 == 0 {
                    continue;
                }
                let (x, y) = (g.m.lb as i32 + col - x0, row as i32 - g.m.ascent as i32 - y0);
                if (0..w as i32).contains(&x) && (0..h as i32).contains(&y) {
                    out[y as usize * w + x as usize] = true;
                }
            }
        }
    };
    let mut source = vec![false; w * h];
    plot(src, &mut source);
    let mask = match msk {
        Some(m) => {
            let mut v = vec![false; w * h];
            plot(m, &mut v);
            v
        }
        None => source.clone(),
    };
    Ok(Cursor { width: w, height: h, hot_x: (-x0).clamp(0, w as i32 - 1) as usize, hot_y: (-y0).clamp(0, h as i32 - 1) as usize, source, mask, fore: Cell::new(fore), back: Cell::new(back) })
}
