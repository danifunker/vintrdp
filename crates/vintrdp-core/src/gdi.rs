//! Software rasterizer standing in for the X11 drawing rdesktop used to do.
//!
//! Pixels are `0x00RRGGBB`. All raster operations are the 16 ROP2 codes
//! (rdesktop maps ROP3 down to ROP2 before calling the UI layer, except for
//! TriBlt, which `shim.rs` decomposes the same way xwin.c did).

#![allow(clippy::too_many_arguments)]

/// The source of colour for a fill: solid, an 8x8 stipple or an 8x8 tile.
#[derive(Clone)]
pub enum Pattern {
    Solid(u32),
    /// 1bpp 8x8 pattern, MSB-first rows. Set bits use `set`, clear bits use
    /// `clear` (or are left untouched when `clear` is `None`).
    Stipple {
        rows: [u8; 8],
        set: u32,
        clear: Option<u32>,
        ox: i32,
        oy: i32,
    },
    Tile {
        px: Box<[u32; 64]>,
        ox: i32,
        oy: i32,
    },
}

impl Pattern {
    #[inline]
    fn at(&self, x: i32, y: i32) -> Option<u32> {
        match self {
            Pattern::Solid(c) => Some(*c),
            Pattern::Stipple { rows, set, clear, ox, oy } => {
                let r = (y - oy).rem_euclid(8) as usize;
                let c = (x - ox).rem_euclid(8);
                if rows[r] & (0x80 >> c) != 0 { Some(*set) } else { *clear }
            }
            Pattern::Tile { px, ox, oy } => {
                let r = (y - oy).rem_euclid(8) as usize;
                let c = (x - ox).rem_euclid(8) as usize;
                Some(px[r * 8 + c])
            }
        }
    }
}

/// Apply ROP2 `op` with destination `d` and pen/source `p`.
#[inline]
pub fn rop2(op: u8, d: u32, p: u32) -> u32 {
    let r = match op & 0xf {
        0x0 => 0,
        0x1 => !(d | p),
        0x2 => d & !p,
        0x3 => !p,
        0x4 => p & !d,
        0x5 => !d,
        0x6 => d ^ p,
        0x7 => !(d & p),
        0x8 => d & p,
        0x9 => !(d ^ p),
        0xa => d,
        0xb => d | !p,
        0xc => p,
        0xd => p | !d,
        0xe => d | p,
        _ => 0xffffff,
    };
    r & 0x00ff_ffff
}

#[derive(Clone, Copy)]
struct Rect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

pub struct Surface {
    pub width: usize,
    pub height: usize,
    pub px: Vec<u32>,
    clip: Option<Rect>,
}

impl Surface {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, px: vec![0; width * height], clip: None }
    }

    /// Resize, keeping the overlapping top-left content.
    pub fn resize(&mut self, width: usize, height: usize) {
        let mut px = vec![0; width * height];
        for y in 0..height.min(self.height) {
            let n = width.min(self.width);
            px[y * width..y * width + n].copy_from_slice(&self.px[y * self.width..y * self.width + n]);
        }
        *self = Self { width, height, px, clip: self.clip };
    }

    pub fn set_clip(&mut self, x: i32, y: i32, cx: i32, cy: i32) {
        self.clip = Some(Rect { x0: x, y0: y, x1: x + cx, y1: y + cy });
    }

    pub fn reset_clip(&mut self) {
        self.clip = None;
    }

    /// Intersect a rectangle with the surface and the clip region.
    fn bounds(&self, x: i32, y: i32, cx: i32, cy: i32) -> Option<Rect> {
        let mut r = Rect { x0: x.max(0), y0: y.max(0), x1: (x + cx).min(self.width as i32), y1: (y + cy).min(self.height as i32) };
        if let Some(c) = self.clip {
            r.x0 = r.x0.max(c.x0);
            r.y0 = r.y0.max(c.y0);
            r.x1 = r.x1.min(c.x1);
            r.y1 = r.y1.min(c.y1);
        }
        (r.x0 < r.x1 && r.y0 < r.y1).then_some(r)
    }

    #[inline]
    fn visible(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return false;
        }
        match self.clip {
            Some(c) => x >= c.x0 && x < c.x1 && y >= c.y0 && y < c.y1,
            None => true,
        }
    }

    #[inline]
    fn plot(&mut self, op: u8, x: i32, y: i32, p: u32) {
        if self.visible(x, y) {
            let i = y as usize * self.width + x as usize;
            self.px[i] = rop2(op, self.px[i], p);
        }
    }

    /// Fill a rectangle from a pattern through ROP2 `op`.
    pub fn fill(&mut self, op: u8, x: i32, y: i32, cx: i32, cy: i32, pat: &Pattern) {
        let Some(r) = self.bounds(x, y, cx, cy) else { return };
        for yy in r.y0..r.y1 {
            let row = yy as usize * self.width;
            for xx in r.x0..r.x1 {
                if let Some(p) = pat.at(xx, yy) {
                    let d = &mut self.px[row + xx as usize];
                    *d = rop2(op, *d, p);
                }
            }
        }
    }

    /// Combine pixels from `src` (row stride `src_w`) into the surface.
    pub fn blit(&mut self, op: u8, x: i32, y: i32, cx: i32, cy: i32, src: &[u32], src_w: usize, src_h: usize, sx: i32, sy: i32) {
        // Restrict to the part of the source that exists.
        let cx = cx.min(src_w as i32 - sx);
        let cy = cy.min(src_h as i32 - sy);
        let Some(r) = self.bounds(x, y, cx, cy) else { return };
        for yy in r.y0..r.y1 {
            let srow = (sy + yy - y) as usize * src_w;
            let drow = yy as usize * self.width;
            for xx in r.x0..r.x1 {
                let sxx = sx + xx - x;
                if sxx < 0 || sy + yy - y < 0 {
                    continue;
                }
                let s = src[srow + sxx as usize];
                let d = &mut self.px[drow + xx as usize];
                *d = rop2(op, *d, s);
            }
        }
    }

    /// Copy a region of the surface onto itself (ScreenBlt). Overlap-safe.
    pub fn screen_blt(&mut self, op: u8, x: i32, y: i32, cx: i32, cy: i32, sx: i32, sy: i32) {
        let tmp = self.read(sx, sy, cx, cy);
        self.blit(op, x, y, cx, cy, &tmp, cx.max(0) as usize, cy.max(0) as usize, 0, 0);
    }

    /// Read a rectangle (pixels outside the surface read as black).
    pub fn read(&self, x: i32, y: i32, cx: i32, cy: i32) -> Vec<u32> {
        let (w, h) = (cx.max(0) as usize, cy.max(0) as usize);
        let mut out = vec![0; w * h];
        for j in 0..h {
            let yy = y + j as i32;
            if yy < 0 || yy >= self.height as i32 {
                continue;
            }
            for i in 0..w {
                let xx = x + i as i32;
                if xx >= 0 && xx < self.width as i32 {
                    out[j * w + i] = self.px[yy as usize * self.width + xx as usize];
                }
            }
        }
        out
    }

    /// Windows-style line: the final point is not drawn.
    pub fn line(&mut self, op: u8, x0: i32, y0: i32, x1: i32, y1: i32, colour: u32) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (stepx, stepy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        while x != x1 || y != y1 {
            self.plot(op, x, y, colour);
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += stepx;
            }
            if e2 <= dx {
                err += dx;
                y += stepy;
            }
        }
    }

    /// Scanline polygon fill with even-odd (`alternate`) or non-zero winding.
    pub fn polygon(&mut self, op: u8, pts: &[(i32, i32)], alternate: bool, pat: &Pattern) {
        if pts.len() < 3 {
            return;
        }
        let ymin = pts.iter().map(|p| p.1).min().unwrap().max(0);
        let ymax = pts.iter().map(|p| p.1).max().unwrap().min(self.height as i32 - 1);
        let mut xs: Vec<(f32, i32)> = Vec::new();
        for y in ymin..=ymax {
            xs.clear();
            let sy = y as f32 + 0.5;
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                if a.1 == b.1 {
                    continue;
                }
                let (lo, hi, dir) = if a.1 < b.1 { (a, b, 1) } else { (b, a, -1) };
                if sy >= lo.1 as f32 && sy < hi.1 as f32 {
                    let t = (sy - lo.1 as f32) / (hi.1 - lo.1) as f32;
                    xs.push((lo.0 as f32 + t * (hi.0 - lo.0) as f32, dir));
                }
            }
            xs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0;
            for k in 0..xs.len().saturating_sub(1) {
                wind += xs[k].1;
                let inside = if alternate { k % 2 == 0 } else { wind != 0 };
                if inside {
                    let xa = xs[k].0.round() as i32;
                    let xb = xs[k + 1].0.round() as i32;
                    self.fill(op, xa, y, xb - xa, 1, pat);
                }
            }
        }
    }

    /// Axis-aligned ellipse inscribed in the given box.
    pub fn ellipse(&mut self, op: u8, x: i32, y: i32, cx: i32, cy: i32, filled: bool, pat: &Pattern) {
        if cx <= 0 || cy <= 0 {
            return;
        }
        let (a, b) = (cx as f32 / 2.0, cy as f32 / 2.0);
        let (mx, my) = (x as f32 + a, y as f32 + b);
        let mut prev: Option<(i32, i32)> = None;
        for j in 0..cy {
            let dy = (y + j) as f32 + 0.5 - my;
            let t = 1.0 - (dy * dy) / (b * b);
            if t < 0.0 {
                continue;
            }
            let half = a * t.sqrt();
            let xa = (mx - half).round() as i32;
            let xb = (mx + half).round() as i32;
            if filled {
                self.fill(op, xa, y + j, xb - xa, 1, pat);
            } else {
                // Edge pixels, plus horizontal runs where the curve flattens.
                let (pa, pb) = prev.unwrap_or((xa, xb));
                let (l0, l1) = (xa.min(pa), xa.max(pa).max(xa + 1));
                let (r0, r1) = (xb.min(pb).min(xb - 1), xb.max(pb));
                self.fill(op, l0, y + j, l1 - l0, 1, pat);
                self.fill(op, r0, y + j, r1 - r0, 1, pat);
                prev = Some((xa, xb));
            }
        }
    }

    /// Draw a 1bpp glyph: set bits in `fg`, clear bits in `bg` when opaque.
    pub fn glyph(&mut self, x: i32, y: i32, cx: i32, cy: i32, g: &Glyph, fg: u32, bg: Option<u32>) {
        let Some(r) = self.bounds(x, y, cx.min(g.width as i32), cy.min(g.height as i32)) else { return };
        for yy in r.y0..r.y1 {
            let gy = (yy - y) as usize;
            for xx in r.x0..r.x1 {
                let gx = (xx - x) as usize;
                let on = g.bits[gy * g.stride + gx / 8] & (0x80 >> (gx % 8)) != 0;
                let c = if on { Some(fg) } else { bg };
                if let Some(c) = c {
                    self.px[yy as usize * self.width + xx as usize] = c;
                }
            }
        }
    }
}

/// A server bitmap converted to our pixel format.
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub px: Vec<u32>,
}

/// A 1bpp, MSB-first glyph with rows padded to whole bytes.
pub struct Glyph {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bits: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rop_identities() {
        assert_eq!(rop2(ROP_COPY, 0x123456, 0xabcdef), 0xabcdef);
        assert_eq!(rop2(0xa, 0x123456, 0xabcdef), 0x123456);
        assert_eq!(rop2(0x6, 0xff00ff, 0x00ffff), 0xffff00);
        assert_eq!(rop2(0x5, 0x000000, 0), 0xffffff);
    }
    const ROP_COPY: u8 = 0xc;

    #[test]
    fn clip_limits_fill() {
        let mut s = Surface::new(4, 4);
        s.set_clip(1, 1, 2, 2);
        s.fill(0xc, 0, 0, 4, 4, &Pattern::Solid(1));
        assert_eq!(s.px.iter().filter(|&&p| p == 1).count(), 4);
        assert_eq!(s.px[5], 1);
        assert_eq!(s.px[0], 0);
    }

    #[test]
    fn screen_blt_overlap() {
        let mut s = Surface::new(4, 1);
        s.px = vec![1, 2, 3, 4];
        s.screen_blt(0xc, 1, 0, 3, 1, 0, 0);
        assert_eq!(s.px, vec![1, 1, 2, 3]);
    }

    #[test]
    fn stipple_origin() {
        let p = Pattern::Stipple { rows: [0x80; 8], set: 7, clear: Some(0), ox: 2, oy: 0 };
        assert_eq!(p.at(2, 0), Some(7));
        assert_eq!(p.at(3, 0), Some(0));
        assert_eq!(p.at(10, 5), Some(7));
    }

    #[test]
    fn line_excludes_last_point() {
        let mut s = Surface::new(4, 1);
        s.line(0xc, 0, 0, 3, 0, 9);
        assert_eq!(s.px, vec![9, 9, 9, 0]);
    }

    #[test]
    fn polygon_square() {
        let mut s = Surface::new(4, 4);
        s.polygon(0xc, &[(0, 0), (4, 0), (4, 4), (0, 4)], true, &Pattern::Solid(1));
        assert!(s.px.iter().all(|&p| p == 1));
    }
}
