//! Trapezoids and triangles, from pixman-trap.c and pixman-edge.c: edges
//! stepped down a grid of sample rows (15 a pixel at 8 bits of alpha, 1
//! at 1 bit) and each row's coverage added up, saturating.

use super::{Bits, Dest, Format, Image, Px, FIXED_1, FIXED_E};
use crate::region::Box;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Line {
    pub p1: Point,
    pub p2: Point,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Trapezoid {
    pub top: i32,
    pub bottom: i32,
    pub left: Line,
    pub right: Line,
}

impl Trapezoid {
    pub fn valid(&self) -> bool {
        self.left.p1.y != self.left.p2.y && self.right.p1.y != self.right.p2.y && self.bottom > self.top
    }
}

/// RENDER's xTrap: a span (left, right) at the top and at the bottom.
#[derive(Clone, Copy, Debug, Default)]
pub struct Trap {
    pub top: (i32, i32, i32),
    pub bot: (i32, i32, i32),
}

fn n_y_frac(n: u32) -> i32 {
    if n == 1 { 1 } else { (1 << (n / 2)) - 1 }
}
fn n_x_frac(n: u32) -> i32 {
    if n == 1 { 1 } else { (1 << (n / 2)) + 1 }
}
fn step_y_small(n: u32) -> i32 {
    FIXED_1 / n_y_frac(n)
}
fn step_y_big(n: u32) -> i32 {
    FIXED_1 - (n_y_frac(n) - 1) * step_y_small(n)
}
fn y_frac_first(n: u32) -> i32 {
    step_y_big(n) / 2
}
fn y_frac_last(n: u32) -> i32 {
    y_frac_first(n) + (n_y_frac(n) - 1) * step_y_small(n)
}
fn step_x_small(n: u32) -> i32 {
    FIXED_1 / n_x_frac(n)
}
fn step_x_big(n: u32) -> i32 {
    FIXED_1 - (n_x_frac(n) - 1) * step_x_small(n)
}
fn x_frac_first(n: u32) -> i32 {
    step_x_big(n) / 2
}
fn samples_x(x: i32, n: u32) -> i32 {
    if n == 1 { 0 } else { ((x & 0xffff) + x_frac_first(n)) / step_x_small(n) }
}

/// pixman's DIV: division rounding towards minus infinity.
fn div(a: i32, b: i32) -> i32 {
    if (a < 0) == (b < 0) { a / b } else { (a - b + 1 - (((b < 0) as i32) << 1)) / b }
}

pub fn sample_ceil_y(y: i32, n: u32) -> i32 {
    let mut f = y & 0xffff;
    let mut i = y & !0xffff;
    f = div(f - y_frac_first(n) + (step_y_small(n) - FIXED_E), step_y_small(n)) * step_y_small(n) + y_frac_first(n);
    if f > y_frac_last(n) {
        if i >> 16 == 0x7fff {
            f = 0xffff;
        } else {
            f = y_frac_first(n);
            i = i.wrapping_add(FIXED_1);
        }
    }
    i | f
}

pub fn sample_floor_y(y: i32, n: u32) -> i32 {
    let mut f = y & 0xffff;
    let mut i = y & !0xffff;
    f = div(f - FIXED_E - y_frac_first(n), step_y_small(n)) * step_y_small(n) + y_frac_first(n);
    if f < y_frac_first(n) {
        if i >> 16 == 0xffff_8000u32 as i32 {
            f = 0;
        } else {
            f = y_frac_last(n);
            i = i.wrapping_sub(FIXED_1);
        }
    }
    i | f
}

#[derive(Clone, Copy, Default)]
struct Edge {
    x: i32,
    e: i32,
    stepx: i32,
    signdx: i32,
    dy: i32,
    dx: i32,
    stepx_small: i32,
    stepx_big: i32,
    dx_small: i32,
    dx_big: i32,
}

impl Edge {
    /// pixman_edge_step (which leaves the error term as it was when the
    /// step doesn't carry).
    fn step(&mut self, n: i32) {
        self.x = self.x.wrapping_add(n.wrapping_mul(self.stepx));
        let ne = self.e as i64 + n as i64 * self.dx as i64;
        let dy = self.dy as i64;
        if dy == 0 {
            return;
        }
        if n >= 0 {
            if ne > 0 {
                let nx = ((ne + dy - 1) / dy) as i32;
                self.e = (ne - nx as i64 * dy) as i32;
                self.x = self.x.wrapping_add(nx.wrapping_mul(self.signdx));
            }
        } else if ne <= -dy {
            let nx = ((-ne) / dy) as i32;
            self.e = (ne + nx as i64 * dy) as i32;
            self.x = self.x.wrapping_sub(nx.wrapping_mul(self.signdx));
        }
    }

    fn multi(&self, n: i32) -> (i32, i32) {
        let mut ne = n as i64 * self.dx as i64;
        let mut stepx = n.wrapping_mul(self.stepx);
        if ne > 0 {
            let nx = (ne / self.dy as i64) as i32;
            ne -= nx as i64 * self.dy as i64;
            stepx = stepx.wrapping_add(nx.wrapping_mul(self.signdx));
        }
        (stepx, ne as i32)
    }

    /// pixman_edge_init.
    fn new(n: u32, y_start: i32, x_top: i32, y_top: i32, x_bot: i32, y_bot: i32) -> Edge {
        let mut e = Edge { x: x_top, ..Default::default() };
        let dx = x_bot.wrapping_sub(x_top);
        let dy = y_bot.wrapping_sub(y_top);
        e.dy = dy;
        if dy != 0 {
            if dx >= 0 {
                e.signdx = 1;
                e.stepx = dx / dy;
                e.dx = dx % dy;
                e.e = dy.wrapping_neg();
            } else {
                e.signdx = -1;
                e.stepx = -(dx.wrapping_neg() / dy);
                e.dx = dx.wrapping_neg() % dy;
                e.e = 0;
            }
            (e.stepx_small, e.dx_small) = e.multi(step_y_small(n));
            (e.stepx_big, e.dx_big) = e.multi(step_y_big(n));
        }
        e.step(y_start.wrapping_sub(y_top));
        e
    }

    fn step_small(&mut self) {
        self.x = self.x.wrapping_add(self.stepx_small);
        self.e = self.e.wrapping_add(self.dx_small);
        if self.e > 0 {
            self.e = self.e.wrapping_sub(self.dy);
            self.x = self.x.wrapping_add(self.signdx);
        }
    }

    fn step_big(&mut self) {
        self.x = self.x.wrapping_add(self.stepx_big);
        self.e = self.e.wrapping_add(self.dx_big);
        if self.e > 0 {
            self.e = self.e.wrapping_sub(self.dy);
            self.x = self.x.wrapping_add(self.signdx);
        }
    }
}

/// An alpha mask being rasterized into: a1, a4 or a8.
pub struct Alpha<'a> {
    pub px: &'a mut [u32],
    pub stride: usize,
    pub width: i32,
    pub height: i32,
    pub bits: u32,
}

/// pixman_rasterize_edges, from row t to row b (sample rows).
fn rasterize_edges(img: &mut Alpha, l: &mut Edge, r: &mut Edge, t: i32, b: i32) {
    let n = img.bits;
    let width = img.width;
    let mut y = t;
    let mut row = (y >> 16) as usize;
    let max = (1u32 << n) - 1;
    loop {
        let (mut lx, mut rx) = (l.x, r.x);
        if n == 1 {
            lx = lx.wrapping_add(x_frac_first(1) - FIXED_E);
            rx = rx.wrapping_add(x_frac_first(1) - FIXED_E);
        }
        if lx < 0 {
            lx = 0;
        }
        if rx >> 16 >= width {
            rx = if n == 1 { width << 16 } else { (width << 16) - 1 };
        }
        if rx > lx {
            let (lxi, rxi) = (lx >> 16, rx >> 16);
            let line = &mut img.px[row * img.stride..];
            if n == 1 {
                for p in &mut line[lxi as usize..rxi as usize] {
                    *p = 1;
                }
            } else {
                let add = |p: &mut u32, v: i32| *p = (*p + v as u32).min(max);
                let (lxs, rxs) = (samples_x(lx, n), samples_x(rx, n));
                if lxi == rxi {
                    add(&mut line[lxi as usize], rxs - lxs);
                } else {
                    add(&mut line[lxi as usize], n_x_frac(n) - lxs);
                    for p in &mut line[lxi as usize + 1..rxi as usize] {
                        add(p, n_x_frac(n));
                    }
                    add(&mut line[rxi as usize], rxs);
                }
            }
        }
        if y == b {
            break;
        }
        if n > 1 && y & 0xffff != y_frac_last(n) {
            l.step_small();
            r.step_small();
            y = y.wrapping_add(step_y_small(n));
        } else {
            l.step_big();
            r.step_big();
            y = y.wrapping_add(step_y_big(n));
            row += 1;
        }
    }
}

/// The sample rows a span from top to bottom covers, if any.
fn rows(img: &Alpha, top: i32, bottom: i32) -> Option<(i32, i32)> {
    let t = sample_ceil_y(top.max(0), img.bits);
    let b = if bottom >> 16 >= img.height { (img.height << 16) - 1 } else { bottom };
    let b = sample_floor_y(b, img.bits);
    (b >= t).then_some((t, b))
}

/// pixman_rasterize_trapezoid.
pub fn rasterize_trapezoid(img: &mut Alpha, trap: &Trapezoid, x_off: i32, y_off: i32) {
    if !trap.valid() {
        return;
    }
    let (xo, yo) = (x_off << 16, y_off << 16);
    let Some((t, b)) = rows(img, trap.top.wrapping_add(yo), trap.bottom.wrapping_add(yo)) else { return };
    let edge = |line: &Line| {
        let (top, bot) = if line.p1.y <= line.p2.y { (line.p1, line.p2) } else { (line.p2, line.p1) };
        Edge::new(img.bits, t, top.x.wrapping_add(xo), top.y.wrapping_add(yo), bot.x.wrapping_add(xo), bot.y.wrapping_add(yo))
    };
    let (mut l, mut r) = (edge(&trap.left), edge(&trap.right));
    rasterize_edges(img, &mut l, &mut r, t, b);
}

/// pixman_add_traps.
pub fn add_traps(img: &mut Alpha, x_off: i32, y_off: i32, traps: &[Trap]) {
    let (xo, yo) = (x_off << 16, y_off << 16);
    for tr in traps {
        let Some((t, b)) = rows(img, tr.top.2.wrapping_add(yo), tr.bot.2.wrapping_add(yo)) else { continue };
        let (ty, by) = (tr.top.2.wrapping_add(yo), tr.bot.2.wrapping_add(yo));
        let mut l = Edge::new(img.bits, t, tr.top.0.wrapping_add(xo), ty, tr.bot.0.wrapping_add(xo), by);
        let mut r = Edge::new(img.bits, t, tr.top.1.wrapping_add(xo), ty, tr.bot.1.wrapping_add(xo), by);
        rasterize_edges(img, &mut l, &mut r, t, b);
    }
}

/// zero_src_has_no_effect: where the operator leaves the dest alone
/// with a clear source, the mask need only cover the trapezoids.
fn zero_src_no_effect(o: u8) -> bool {
    use super::op::*;
    matches!(o, DST | OVER | OVER_REVERSE | OUT_REVERSE | ATOP | XOR | ADD)
}

/// get_trap_extents.
fn extents(o: u8, dst: &Dest, traps: &[Trapezoid]) -> Option<Box> {
    if !zero_src_no_effect(o) {
        return Some(Box { x1: 0, y1: 0, x2: dst.width, y2: dst.height });
    }
    let mut b = Box { x1: i32::MAX, y1: i32::MAX, x2: i32::MIN, y2: i32::MIN };
    let ceil = |v: i32| v.wrapping_add(0xffff) >> 16;
    for t in traps.iter().filter(|t| t.valid()) {
        b.y1 = b.y1.min(t.top >> 16);
        b.y2 = b.y2.max(ceil(t.bottom));
        for x in [t.left.p1.x, t.left.p2.x, t.right.p1.x, t.right.p2.x] {
            b.x1 = b.x1.min(x >> 16);
            b.x2 = b.x2.max(ceil(x));
        }
    }
    (b.x1 < b.x2 && b.y1 < b.y2).then_some(b)
}

/// pixman_composite_trapezoids, the mask of `mask_format` (a1, a4, a8):
/// the trapezoids rasterized into a mask the size of what they cover,
/// composited from (x_src, y_src) of the source at (x_dst, y_dst).
#[allow(clippy::too_many_arguments)]
pub fn composite_trapezoids(o: u8, src: &Image, dst: &mut Dest, mask_format: Format, x_src: i32, y_src: i32, x_dst: i32, y_dst: i32, traps: &[Trapezoid]) -> Option<Box> {
    if traps.is_empty() {
        return None;
    }
    let mut b = extents(o, dst, traps)?;
    // Cut to what of the dest it can reach: the same result, in less mask.
    let reach = Box { x1: -x_dst, y1: -y_dst, x2: dst.width - x_dst, y2: dst.height - y_dst };
    b = b.intersect(&reach);
    if b.is_empty() {
        return None;
    }
    let (w, h) = (b.x2 - b.x1, b.y2 - b.y1);
    let mut px = vec![0u32; w as usize * h as usize];
    let mut alpha = Alpha { px: &mut px, stride: w as usize, width: w, height: h, bits: mask_format.a() };
    for t in traps.iter().filter(|t| t.valid()) {
        rasterize_trapezoid(&mut alpha, t, -b.x1, -b.y1);
    }
    let mask = Image::bits(Bits { px: Px::Slice(&px), stride: w as usize, width: w, height: h, format: mask_format, indexed: None });
    // pixman_image_composite: 16-bit coordinates.
    let c16 = |v: i32| v as i16 as i32;
    super::composite(o, src, Some(&mask), dst, c16(x_src + b.x1), c16(y_src + b.y1), 0, 0, c16(x_dst + b.x1), c16(y_dst + b.y1), w as u16 as i32, h as u16 as i32)
}

fn greater_y(a: &Point, b: &Point) -> bool {
    if a.y == b.y { a.x > b.x } else { a.y > b.y }
}

fn clockwise(r: &Point, a: &Point, b: &Point) -> bool {
    let (adx, ady) = (a.x.wrapping_sub(r.x), a.y.wrapping_sub(r.y));
    let (bdx, bdy) = (b.x.wrapping_sub(r.x), b.y.wrapping_sub(r.y));
    (bdy as i64 * adx as i64 - ady as i64 * bdx as i64) < 0
}

/// triangle_to_trapezoids.
pub fn triangle(p1: Point, p2: Point, p3: Point) -> [Trapezoid; 2] {
    let (mut top, mut left, mut right) = (p1, p2, p3);
    if greater_y(&top, &left) {
        std::mem::swap(&mut top, &mut left);
    }
    if greater_y(&top, &right) {
        std::mem::swap(&mut top, &mut right);
    }
    if clockwise(&top, &right, &left) {
        std::mem::swap(&mut right, &mut left);
    }
    let a = Trapezoid { top: top.y, bottom: if right.y < left.y { right.y } else { left.y }, left: Line { p1: top, p2: left }, right: Line { p1: top, p2: right } };
    let mut b = a;
    if right.y < left.y {
        b.top = right.y;
        b.bottom = left.y;
        b.right = Line { p1: right, p2: left };
    } else {
        b.top = left.y;
        b.bottom = right.y;
        b.left = Line { p1: left, p2: right };
    }
    [a, b]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_grid() {
        // At 8 bits: 15 rows, the first half a row down.
        assert_eq!(y_frac_first(8), 2185);
        assert_eq!(n_x_frac(8), 17);
        assert_eq!(sample_ceil_y(0, 8), 2185);
        assert_eq!(sample_ceil_y(5 << 16, 8), (5 << 16) + 2185);
        assert_eq!(sample_floor_y(5 << 16, 8), (4 << 16) + y_frac_last(8));
        // A pixel-aligned square is fully covered.
        let mut px = vec![0u32; 16];
        let mut a = Alpha { px: &mut px, stride: 4, width: 4, height: 4, bits: 8 };
        let t = Trapezoid { top: 1 << 16, bottom: 3 << 16, left: Line { p1: Point { x: 1 << 16, y: 0 }, p2: Point { x: 1 << 16, y: 4 << 16 } }, right: Line { p1: Point { x: 3 << 16, y: 0 }, p2: Point { x: 3 << 16, y: 4 << 16 } } };
        rasterize_trapezoid(&mut a, &t, 0, 0);
        assert_eq!(px, [0, 0, 0, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 0, 0, 0]);
    }
}
