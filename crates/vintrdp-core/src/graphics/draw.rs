//! Drawing parsed orders into the framebuffer: brushes, blits, TriBlt's
//! ROP3 decomposition, lines and shapes, text with the glyph and fragment
//! caches, the desktop save area and nine-grids. Port of rdesktop's X.c
//! (ui_patblt, ui_triblt, ui_draw_text and DO_GLYPH, ...), by way of the old
//! shim.rs.

#![allow(clippy::too_many_arguments)]

use super::cache::DeskCache;
use super::orders::{self, DeskSave, DestBlt, DrawNineGrid, Ellipse, Line, MemBlt, MultiDrawNineGrid, NineGridEntry, PatBlt, Polygon2, Polyline, Rect, ScreenBlt, Text2, TriBlt};
use super::surface::Pattern;
use super::{Canvas, Core};

pub const ROP2_COPY: u8 = 0xc;
const ROP2_XOR: u8 = 0x6;
const ROP2_AND: u8 = 0x8;
const ROP2_NXOR: u8 = 0x9;

const MIX_OPAQUE: u8 = 1;
const ALTERNATE: u8 = 1;
const TEXT2_VERTICAL: u8 = 0x04;
const TEXT2_IMPLICIT_X: u8 = 0x20;
const NINEGRID_FLAG_TILE: u32 = 0x0000_0002;

/// The ROP2 a ROP3 is when only the source matters.
fn rop2_s(rop3: u8) -> u8 {
    rop3 & 0xf
}

/// The ROP2 a ROP3 is when only the pattern matters.
fn rop2_p(rop3: u8) -> u8 {
    (rop3 & 0x3) | ((rop3 & 0x30) >> 2)
}

const HATCH_PATTERNS: [[u8; 8]; 6] = [
    [0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0x00], // horizontal
    [0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08], // vertical
    [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01], // forward diagonal
    [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80], // backward diagonal
    [0x08, 0x08, 0x08, 0xff, 0x08, 0x08, 0x08, 0x08], // cross
    [0x81, 0x42, 0x24, 0x18, 0x18, 0x24, 0x42, 0x81], // diagonal cross
];

impl Core {
    /// The fill for a brush (setup_brush and xwin.c's reading of it). No
    /// brush is solid foreground.
    pub(super) fn pattern(&self, brush: Option<&orders::Brush>, bg: u32, fg: u32) -> Option<Pattern> {
        let Some(b) = brush else { return Some(Pattern::Solid(self.colours.colour(fg))) };
        let (ox, oy) = (b.xorigin as i32, b.yorigin as i32);
        let mut pattern = b.pattern;
        let mut style = b.style;
        let mut cached = None;
        if style & 0x80 != 0 {
            cached = self.brushes.get(style & 0x0f, pattern[0]);
            if cached.is_none() {
                super::warn(format_args!("brush {} isn't cached", pattern[0]));
                pattern = [0; 8];
            }
            style = 3;
        }
        let (fg, bg) = (self.colours.colour(fg), self.colours.colour(bg));
        match (style, cached) {
            (0, _) => Some(Pattern::Solid(fg)),
            (2, _) => Some(Pattern::Stipple { rows: HATCH_PATTERNS[(pattern[0] as usize).min(5)], set: fg, clear: Some(bg), ox, oy }),
            (3, None) => {
                // An RDP 4 brush: rows bottom-up, and the colours swapped.
                let mut rows = pattern;
                rows.reverse();
                Some(Pattern::Stipple { rows, set: bg, clear: Some(fg), ox, oy })
            }
            (3, Some(bd)) if bd.colour_code > 1 => {
                let bytes = bd.colour_code as usize - 2;
                let px = self.colours.convert(self.colours.depth_for(bytes), 8, 8, &bd.data);
                Some(Pattern::Tile { px: px.into_boxed_slice().try_into().ok()?, ox, oy })
            }
            (3, Some(bd)) => {
                let mut rows = [0u8; 8];
                let n = bd.data.len().min(8);
                rows[..n].copy_from_slice(&bd.data[..n]);
                Some(Pattern::Stipple { rows, set: bg, clear: Some(fg), ox, oy })
            }
            (style, _) => {
                super::warn(format_args!("unimplemented brush style {style}"));
                None
            }
        }
    }

    fn patblt(&self, op: u8, x: i32, y: i32, cx: i32, cy: i32, brush: &orders::Brush, bg: u32, fg: u32, cv: &mut Canvas) {
        if let Some(p) = self.pattern(Some(brush), bg, fg) {
            cv.draw(|s| s.fill(op, x, y, cx, cy, &p));
        }
    }

    pub(super) fn draw_destblt(&mut self, o: &DestBlt, cv: &mut Canvas) {
        cv.draw(|s| s.fill(rop2_s(o.opcode), o.x as i32, o.y as i32, o.cx as i32, o.cy as i32, &Pattern::Solid(0)));
    }

    pub(super) fn draw_patblt(&mut self, o: &PatBlt, cv: &mut Canvas) {
        self.patblt(rop2_p(o.opcode), o.x as i32, o.y as i32, o.cx as i32, o.cy as i32, &o.brush, o.bgcolour, o.fgcolour, cv);
    }

    pub(super) fn draw_screenblt(&mut self, o: &ScreenBlt, cv: &mut Canvas) {
        cv.draw(|s| s.screen_blt(rop2_s(o.opcode), o.x as i32, o.y as i32, o.cx as i32, o.cy as i32, o.srcx as i32, o.srcy as i32));
    }

    pub(super) fn draw_line(&mut self, o: &Line, cv: &mut Canvas) {
        if !(1..=0x10).contains(&o.opcode) {
            super::warn(format_args!("bad ROP2 {:#x}", o.opcode));
            return;
        }
        let c = self.colours.colour(o.pen_colour);
        cv.draw(|s| s.line(o.opcode - 1, o.startx as i32, o.starty as i32, o.endx as i32, o.endy as i32, c));
    }

    pub(super) fn draw_rect(&mut self, o: &Rect, cv: &mut Canvas) {
        let c = self.colours.colour(o.colour);
        cv.draw(|s| s.fill(ROP2_COPY, o.x as i32, o.y as i32, o.cx as i32, o.cy as i32, &Pattern::Solid(c)));
    }

    pub(super) fn draw_desksave(&mut self, o: &DeskSave, cv: &mut Canvas) {
        let (x, y) = (o.left as i32, o.top as i32);
        let (cx, cy) = (o.right as i32 - x + 1, o.bottom as i32 - y + 1);
        if !DeskCache::fits(o.offset, cx, cy) {
            super::warn(format_args!("desktop save area {cx}x{cy} at {} doesn't fit", o.offset));
            return;
        }
        if o.action == 0 {
            if let Some(s) = cv.surface() {
                let px = s.read(x, y, cx, cy);
                self.desk.put(o.offset, &px);
            }
        } else if let Some(px) = self.desk.get(o.offset, (cx * cy) as usize) {
            cv.draw(|s| s.blit(ROP2_COPY, x, y, cx, cy, px, cx as usize, cy as usize, 0, 0));
        }
    }

    pub(super) fn draw_memblt(&mut self, o: &MemBlt, cv: &mut Canvas) {
        self.memblt(rop2_s(o.opcode), o, cv);
    }

    fn memblt(&mut self, op: u8, o: &MemBlt, cv: &mut Canvas) {
        if let Some(b) = self.cached_bitmap(o.cache_id, o.cache_idx) {
            cv.draw(|s| s.blit(op, o.x as i32, o.y as i32, o.cx as i32, o.cy as i32, &b.px, b.width, b.height, o.srcx as i32, o.srcy as i32));
        }
    }

    /// TriBlt, broken into ROP2 steps exactly as xwin.c does.
    pub(super) fn draw_triblt(&mut self, o: &TriBlt, cv: &mut Canvas) {
        if self.cached_bitmap(o.mem.cache_id, o.mem.cache_idx).is_none() {
            return;
        }
        let m = &o.mem;
        let (x, y, cx, cy) = (m.x as i32, m.y as i32, m.cx as i32, m.cy as i32);
        let pat = |core: &Core, op: u8, cv: &mut Canvas| core.patblt(op, x, y, cx, cy, &o.brush, o.bgcolour, o.fgcolour, cv);
        match m.opcode {
            0x69 => {
                // PDSxxn
                self.memblt(ROP2_XOR, m, cv);
                pat(self, ROP2_NXOR, cv);
            }
            0xb8 => {
                // PSDPxax
                pat(self, ROP2_XOR, cv);
                self.memblt(ROP2_AND, m, cv);
                pat(self, ROP2_XOR, cv);
            }
            0xc0 => {
                // PSa
                self.memblt(ROP2_COPY, m, cv);
                pat(self, ROP2_AND, cv);
            }
            op => {
                super::warn(format_args!("unimplemented triblt opcode {op:#x}"));
                self.memblt(ROP2_COPY, m, cv);
            }
        }
    }

    /// Polygon (solid) and Polygon2 (with a brush).
    pub(super) fn draw_polygon(&mut self, o: &Polygon2, with_brush: bool, cv: &mut Canvas) {
        if !(1..=0x10).contains(&o.opcode) {
            super::warn(format_args!("bad ROP2 {:#x}", o.opcode));
            return;
        }
        let Some(pts) = orders::delta_points(o.x, o.y, o.npoints, &o.data) else {
            super::warn(format_args!("polygon parse error"));
            return;
        };
        if let Some(p) = self.pattern(with_brush.then_some(&o.brush), o.bgcolour, o.fgcolour) {
            cv.draw(|s| s.polygon(o.opcode - 1, &pts, o.fillmode == ALTERNATE, &p));
        }
    }

    pub(super) fn draw_polyline(&mut self, o: &Polyline, cv: &mut Canvas) {
        if !(1..=0x10).contains(&o.opcode) {
            super::warn(format_args!("bad ROP2 {:#x}", o.opcode));
            return;
        }
        let Some(pts) = orders::delta_points(o.x, o.y, o.lines, &o.data) else {
            super::warn(format_args!("polyline parse error"));
            return;
        };
        let c = self.colours.colour(o.fgcolour);
        cv.draw(|s| {
            for w in pts.windows(2) {
                s.line(o.opcode - 1, w[0].0, w[0].1, w[1].0, w[1].1, c);
            }
        });
    }

    /// Ellipse (solid) and Ellipse2 (with a brush). Fill mode 1 fills.
    pub(super) fn draw_ellipse(&mut self, o: &Ellipse, with_brush: bool, cv: &mut Canvas) {
        let (x, y) = (o.left as i32, o.top as i32);
        let (cx, cy) = (o.right as i32 - x, o.bottom as i32 - y);
        if let Some(p) = self.pattern(with_brush.then_some(&o.brush), o.bgcolour, o.fgcolour) {
            cv.draw(|s| s.ellipse(o.opcode.wrapping_sub(1), x, y, cx, cy, o.fillmode == 1, &p));
        }
    }

    /// Text: the background box, then each glyph, replaying and recording
    /// fragments (ui_draw_text with DO_GLYPH).
    pub(super) fn draw_text(&mut self, o: &Text2, cv: &mut Canvas) {
        let (bg, fg) = (self.colours.colour(o.bgcolour), self.colours.colour(o.fgcolour));
        let (clipx, clipy) = (o.clipleft as i32, o.cliptop as i32);
        let (clipcx, clipcy) = (o.clipright as i32 - clipx, o.clipbottom as i32 - clipy);
        let (boxx, boxy) = (o.boxleft as i32, o.boxtop as i32);
        let (mut boxcx, boxcy) = (o.boxright as i32 - boxx, o.boxbottom as i32 - boxy);
        let Some(width) = cv.surface().map(|s| s.width as i32) else {
            // Still record the fragments for later.
            self.text_fragments(o, None);
            return;
        };
        if boxx + boxcx > width {
            boxcx = width - boxx;
        }
        if boxcx > 1 {
            cv.draw(|s| s.fill(ROP2_COPY, boxx, boxy, boxcx, boxcy, &Pattern::Solid(bg)));
        } else if o.mixmode == MIX_OPAQUE {
            cv.draw(|s| s.fill(ROP2_COPY, clipx, clipy, clipcx, clipcy, &Pattern::Solid(bg)));
        }
        self.text_fragments(o, Some((cv, fg)));
    }

    /// Walk the text: glyph indices with their offsets, 0xff (store the run
    /// so far as a fragment) and 0xfe (replay a fragment).
    fn text_fragments(&mut self, o: &Text2, mut out: Option<(&mut Canvas, u32)>) {
        let mut pos = (o.x as i32, o.y as i32);
        let mut t: &[u8] = &o.text;
        let mut i = 0;
        while i < t.len() {
            match t[i] {
                0xff => {
                    if i + 3 > t.len() {
                        break;
                    }
                    let n = (t[i + 2] as usize).min(t.len());
                    self.text.put(t[i + 1], &t[..n]);
                    t = &t[i + 3..];
                    i = 0;
                }
                0xfe => {
                    if i + 2 > t.len() {
                        break;
                    }
                    let entry = self.text.get(t[i + 1]).to_vec();
                    if !entry.is_empty() {
                        if entry.get(1) == Some(&0) && o.flags & TEXT2_IMPLICIT_X == 0 && i + 2 < t.len() {
                            if o.flags & TEXT2_VERTICAL != 0 {
                                pos.1 += t[i + 2] as i32;
                            } else {
                                pos.0 += t[i + 2] as i32;
                            }
                        }
                        let mut j = 0;
                        while j < entry.len() {
                            self.glyph(o, &mut pos, &entry, &mut j, &mut out);
                            j += 1;
                        }
                    }
                    t = &t[if i + 2 < t.len() { i + 3 } else { i + 2 }..];
                    i = 0;
                }
                _ => {
                    self.glyph(o, &mut pos, t, &mut i, &mut out);
                    i += 1;
                }
            }
        }
    }

    /// One glyph: move by its offset (unless spacing is implicit) and draw it
    /// (DO_GLYPH).
    fn glyph(&self, o: &Text2, pos: &mut (i32, i32), t: &[u8], idx: &mut usize, out: &mut Option<(&mut Canvas, u32)>) {
        let at = |i: usize| t.get(i).copied().unwrap_or(0) as i32;
        let glyph = self.glyphs.get(o.font, at(*idx) as u16);
        let implicit_x = o.flags & TEXT2_IMPLICIT_X != 0;
        if !implicit_x {
            *idx += 1;
            let xyoffset = at(*idx);
            let delta = if xyoffset & 0x80 != 0 {
                let d = at(*idx + 1) | (at(*idx + 2) << 8);
                *idx += 2;
                d
            } else {
                xyoffset
            };
            if o.flags & TEXT2_VERTICAL != 0 {
                pos.1 += delta;
            } else {
                pos.0 += delta;
            }
        }
        if let Some(g) = glyph {
            if let Some((cv, fg)) = out {
                let (x, y) = (pos.0 + g.offset as i32, pos.1 + g.baseline as i32);
                let fg = *fg;
                cv.draw(|s| s.glyph(x, y, g.width as i32, g.height as i32, &g.glyph, fg, None));
            }
            if implicit_x {
                pos.0 += g.width as i32;
            }
        }
    }

    pub(super) fn draw_ninegrid(&mut self, o: &DrawNineGrid, x: i32, y: i32, cx: i32, cy: i32, cv: &mut Canvas) {
        if let Some(Some(entry)) = self.ninegrids.get(o.bitmap_id as usize) {
            render_ninegrid(entry, o, x, y, cx, cy, cv);
        }
    }

    pub(super) fn draw_multi_ninegrid(&mut self, o: &MultiDrawNineGrid, cv: &mut Canvas) {
        if o.data.is_empty() {
            return;
        }
        let Some(Some(entry)) = self.ninegrids.get(o.grid.bitmap_id as usize) else { return };
        let Some(rects) = orders::delta_rects(&o.data, o.n_delta_entries) else {
            super::warn(format_args!("failed to parse nine-grid rectangles"));
            return;
        };
        for (x, y, cx, cy) in rects {
            render_ninegrid(entry, &o.grid, x, y, cx, cy, cv);
        }
    }
}

/// Stretch (or tile) a nine-grid bitmap's middle over the destination,
/// keeping its edges and corners (ninegrid_render_rect). Only what lands
/// on the screen is computed.
fn render_ninegrid(e: &NineGridEntry, o: &DrawNineGrid, left: i32, top: i32, width: i32, height: i32, cv: &mut Canvas) {
    let Some((sw, sh)) = cv.surface().map(|s| (s.width as i32, s.height as i32)) else { return };
    if width <= 0 || height <= 0 || o.src_right < o.src_left || o.src_bottom < o.src_top || e.width == 0 || e.height == 0 {
        return;
    }
    let (src_l, src_t, src_r, src_b) = (o.src_left as i32, o.src_top as i32, o.src_right as i32, o.src_bottom as i32);
    let (src_w, src_h) = (src_r - src_l + 1, src_b - src_t + 1);
    let (mut lw, mut rw, mut th, mut bh) = (e.left_width as i32, e.right_width as i32, e.top_height as i32, e.bottom_height as i32);
    if lw + rw > src_w {
        lw = src_w / 2;
        rw = src_w - lw;
    }
    if th + bh > src_h {
        th = src_h / 2;
        bh = src_h - th;
    }
    let dst_l = lw.min(width);
    let dst_r = rw.min(width - dst_l);
    let dst_t = th.min(height);
    let dst_b = bh.min(height - dst_t);
    let (src_cw, src_ch) = (src_w - lw - rw, src_h - th - bh);
    let (dst_cw, dst_ch) = (width - dst_l - dst_r, height - dst_t - dst_b);
    let tile = e.flags & NINEGRID_FLAG_TILE != 0;

    let map = |d: i32, size: i32, near: i32, far: i32, src0: i32, src1: i32, edge: i32, src_c: i32, dst_c: i32| -> i32 {
        if d < near {
            src0 + d
        } else if d >= size - far {
            src1 - (size - 1 - d)
        } else if src_c <= 0 || dst_c <= 0 {
            src0 + edge
        } else if tile {
            src0 + edge + (d - near) % src_c
        } else {
            src0 + edge + ((d - near) as i64 * src_c as i64 / dst_c as i64) as i32
        }
    };

    // The visible part of the destination.
    let (x0, y0) = ((-left).max(0), (-top).max(0));
    let (x1, y1) = (width.min(sw.saturating_sub(left)), height.min(sh.saturating_sub(top)));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let (vw, vh) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut px = Vec::with_capacity(vw * vh);
    for y in y0..y1 {
        let sy = map(y, height, dst_t, dst_b, src_t, src_b, th, src_ch, dst_ch).clamp(0, e.height as i32 - 1);
        for x in x0..x1 {
            let sx = map(x, width, dst_l, dst_r, src_l, src_r, lw, src_cw, dst_cw).clamp(0, e.width as i32 - 1);
            let i = (sy as usize * e.width as usize + sx as usize) * 4;
            let p = e.data.get(i..i + 3).unwrap_or(&[0, 0, 0]);
            px.push((p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32);
        }
    }
    cv.draw(|s| s.blit(ROP2_COPY, left + x0, top + y0, vw as i32, vh as i32, &px, vw, vh, 0, 0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rop3_to_rop2() {
        assert_eq!(rop2_s(0xcc), ROP2_COPY);
        assert_eq!(rop2_p(0xf0), ROP2_COPY);
        assert_eq!(rop2_s(0x55), 0x5);
        assert_eq!(rop2_p(0x5a), 0x6);
    }
}
