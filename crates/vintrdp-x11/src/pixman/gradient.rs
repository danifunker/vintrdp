//! Gradients, from pixman-gradient-walker.c and pixman-{linear,radial,
//! conical}-gradient.c: the walker interpolates between stops in f32, the
//! positions are worked out in f64 and 16.16 as pixman's.

use super::{matrix, Argb, Repeat, Stop, Transform, FIXED_1};

/// The stops with pixman's two extra, before and after, for the repeat.
pub fn with_sentinels(stops: &[Stop], repeat: Repeat) -> Vec<Stop> {
    let n = stops.len();
    let clear = [0u16; 4];
    let (begin, end) = match repeat {
        Repeat::None => (Stop { x: i32::MIN, color: clear }, Stop { x: i32::MAX, color: clear }),
        Repeat::Normal => (Stop { x: stops[n - 1].x.wrapping_sub(FIXED_1), color: stops[n - 1].color }, Stop { x: stops[0].x.wrapping_add(FIXED_1), color: stops[0].color }),
        Repeat::Reflect => (Stop { x: stops[0].x.wrapping_neg(), color: stops[0].color }, Stop { x: (2 << 16) - stops[n - 1].x, color: stops[n - 1].color }),
        Repeat::Pad => (Stop { x: i32::MIN, color: stops[0].color }, Stop { x: i32::MAX, color: stops[n - 1].color }),
    };
    let mut v = Vec::with_capacity(n + 2);
    v.push(begin);
    v.extend_from_slice(stops);
    v.push(end);
    v
}

pub struct Walker<'a> {
    /// With the sentinels: the user's stops are 1..=n.
    stops: &'a [Stop],
    left_x: i64,
    right_x: i64,
    a_s: f32,
    a_b: f32,
    r_s: f32,
    r_b: f32,
    g_s: f32,
    g_b: f32,
    b_s: f32,
    b_b: f32,
    repeat: Repeat,
    need_reset: bool,
}

const IS_ZERO: fn(f32) -> bool = |f| -f32::MIN_POSITIVE < f && f < f32::MIN_POSITIVE;

impl<'a> Walker<'a> {
    pub fn new(stops: &'a [Stop], repeat: Repeat) -> Walker<'a> {
        Walker { stops, left_x: 0, right_x: 0x10000, a_s: 0.0, a_b: 0.0, r_s: 0.0, r_b: 0.0, g_s: 0.0, g_b: 0.0, b_s: 0.0, b_b: 0.0, repeat, need_reset: true }
    }

    fn reset(&mut self, pos: i64) {
        let count = self.stops.len() - 2;
        let x: i64 = match self.repeat {
            Repeat::Normal => (pos as i32 & 0xffff) as i64,
            Repeat::Reflect => {
                let x = (pos as i32 & 0xffff) as i64;
                if pos as i32 & 0x10000 != 0 { 0x10000 - x } else { x }
            }
            _ => pos,
        };
        // stops[n] is pixman's stops[n - 1]: find the first user stop past x.
        let mut n = 0;
        while n < count {
            if x < self.stops[n + 1].x as i64 {
                break;
            }
            n += 1;
        }
        let (mut left_x, mut left_c) = (self.stops[n].x as i64, self.stops[n].color);
        let (mut right_x, mut right_c) = (self.stops[n + 1].x as i64, self.stops[n + 1].color);
        match self.repeat {
            Repeat::Normal => {
                left_x += pos - x;
                right_x += pos - x;
            }
            Repeat::Reflect => {
                let mut x = x;
                if pos as i32 & 0x10000 != 0 {
                    let tmp_x = 0x10000 - right_x;
                    right_x = 0x10000 - left_x;
                    left_x = tmp_x;
                    std::mem::swap(&mut left_c, &mut right_c);
                    x = 0x10000 - x;
                }
                left_x += pos - x;
                right_x += pos - x;
            }
            Repeat::None => {
                if n == 0 {
                    right_c = left_c;
                } else if n == count {
                    left_c = right_c;
                }
            }
            Repeat::Pad => {}
        }
        // Stop colours are red, green, blue, alpha.
        let k = 1.0f32 / 257.0;
        let (la, lr, lg, lb) = (left_c[3] as f32 * k, left_c[0] as f32 * k, left_c[1] as f32 * k, left_c[2] as f32 * k);
        let (ra, rr, rg, rb) = (right_c[3] as f32 * k, right_c[0] as f32 * k, right_c[1] as f32 * k, right_c[2] as f32 * k);
        let lx = left_x as f32 * (1.0f32 / 65536.0);
        let rx = right_x as f32 * (1.0f32 / 65536.0);
        if IS_ZERO(rx - lx) || left_x == i32::MIN as i64 || right_x == i32::MAX as i64 {
            self.a_s = 0.0;
            self.r_s = 0.0;
            self.g_s = 0.0;
            self.b_s = 0.0;
            self.a_b = (la + ra) / 510.0;
            self.r_b = (lr + rr) / 510.0;
            self.g_b = (lg + rg) / 510.0;
            self.b_b = (lb + rb) / 510.0;
        } else {
            let w_rec = 1.0f32 / (rx - lx);
            let k = 1.0f32 / 255.0;
            self.a_b = (la * rx - ra * lx) * w_rec * k;
            self.r_b = (lr * rx - rr * lx) * w_rec * k;
            self.g_b = (lg * rx - rg * lx) * w_rec * k;
            self.b_b = (lb * rx - rb * lx) * w_rec * k;
            self.a_s = (ra - la) * w_rec * k;
            self.r_s = (rr - lr) * w_rec * k;
            self.g_s = (rg - lg) * w_rec * k;
            self.b_s = (rb - lb) * w_rec * k;
        }
        self.left_x = left_x;
        self.right_x = right_x;
        self.need_reset = false;
    }

    fn check(&mut self, x: i64) {
        if self.need_reset || x < self.left_x || x >= self.right_x {
            self.reset(x);
        }
    }

    pub fn pixel_32(&mut self, x: i64) -> u32 {
        self.check(x);
        let y = x as f32 * (1.0f32 / 65536.0);
        let a = 255.0f32 * (self.a_s * y + self.a_b);
        let r = a * (self.r_s * y + self.r_b);
        let g = a * (self.g_s * y + self.g_b);
        let b = a * (self.b_s * y + self.b_b);
        (((a + 0.5) as u32) << 24 & 0xff00_0000) | (((r + 0.5) as u32) << 16 & 0xff_0000) | (((g + 0.5) as u32) << 8 & 0xff00) | ((b + 0.5) as u32 & 0xff)
    }

    pub fn pixel_float(&mut self, x: i64) -> Argb {
        self.check(x);
        let y = x as f32 * (1.0f32 / 65536.0);
        let a = self.a_s * y + self.a_b;
        Argb { a, r: a * (self.r_s * y + self.r_b), g: a * (self.g_s * y + self.g_b), b: a * (self.b_s * y + self.b_b) }
    }
}

/// A scanline's pixels, narrow or wide.
pub trait Out {
    fn write(&mut self, i: usize, w: &mut Walker, x: i64);
    fn clear(&mut self, i: usize);
    fn len(&self) -> usize;
}

impl Out for [u32] {
    fn write(&mut self, i: usize, w: &mut Walker, x: i64) {
        self[i] = w.pixel_32(x);
    }
    fn clear(&mut self, i: usize) {
        self[i] = 0;
    }
    fn len(&self) -> usize {
        <[u32]>::len(self)
    }
}

impl Out for [Argb] {
    fn write(&mut self, i: usize, w: &mut Walker, x: i64) {
        self[i] = w.pixel_float(x);
    }
    fn clear(&mut self, i: usize) {
        self[i] = Argb::default();
    }
    fn len(&self) -> usize {
        <[Argb]>::len(self)
    }
}

/// The start of a scanline through the transform, and the step across.
fn start(t: Option<&Transform>, x: i32, y: i32) -> Option<([i32; 3], [i32; 3])> {
    let v = [(x << 16).wrapping_add(FIXED_1 / 2), (y << 16).wrapping_add(FIXED_1 / 2), FIXED_1];
    match t {
        Some(t) => Some((matrix::point_3d(t, v)?, [t[0][0], t[1][0], t[2][0]])),
        None => Some((v, [FIXED_1, 0, 0])),
    }
}

pub struct Linear {
    pub p1: (i32, i32),
    pub p2: (i32, i32),
}

impl Linear {
    /// linear_gradient_is_horizontal: every row of the box the same.
    pub fn is_horizontal(&self, t: Option<&Transform>, height: i32) -> bool {
        let v = match t {
            Some(t) => {
                if t[2][0] != 0 || t[2][1] != 0 || t[2][2] == 0 {
                    return false;
                }
                [t[0][1], t[1][1], t[2][2]]
            }
            None => [0, FIXED_1, FIXED_1],
        };
        let dx = self.p2.0 as i64 - self.p1.0 as i64;
        let dy = self.p2.1 as i64 - self.p1.1 as i64;
        let l = dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy));
        if l == 0 {
            return false;
        }
        let inc = height as f64 * 65536.0 * 65536.0 * dx.wrapping_mul(v[0] as i64).wrapping_add(dy.wrapping_mul(v[1] as i64)) as f64 / (v[2] as f64 * l as f64);
        -1.0 < inc && inc < 1.0
    }

    pub fn scanline<O: Out + ?Sized>(&self, out: &mut O, w: &mut Walker, t: Option<&Transform>, x: i32, y: i32) {
        let Some((mut v, unit)) = start(t, x, y) else { return };
        let width = out.len();
        let dx = self.p2.0 as i64 - self.p1.0 as i64;
        let dy = self.p2.1 as i64 - self.p1.1 as i64;
        let l = dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy));
        let p1 = (self.p1.0 as i64, self.p1.1 as i64);
        let t_at = |v: &[i32; 3]| -> f64 {
            let invden = 65536.0 * 65536.0f64 / (l as f64 * v[2] as f64);
            let v2 = v[2] as f64 * (1.0 / 65536.0);
            (dx.wrapping_mul(v[0] as i64).wrapping_add(dy.wrapping_mul(v[1] as i64)) as f64 - dx.wrapping_mul(p1.0).wrapping_add(dy.wrapping_mul(p1.1)) as f64 * v2) * invden
        };
        if l == 0 || unit[2] == 0 {
            let (t0, inc): (i64, f64) = if l == 0 || v[2] == 0 {
                (0, 0.0)
            } else {
                let invden = 65536.0 * 65536.0f64 / (l as f64 * v[2] as f64);
                (t_at(&v) as i64, dx.wrapping_mul(unit[0] as i64).wrapping_add(dy.wrapping_mul(unit[1] as i64)) as f64 * invden)
            };
            if (inc * width as f64) as i64 == 0 {
                for i in 0..width {
                    out.write(i, w, t0);
                }
            } else {
                let mut next_inc = 0i64;
                for i in 0..width {
                    out.write(i, w, t0.wrapping_add(next_inc));
                    next_inc = (inc * (i + 1) as f64) as i64;
                }
            }
        } else {
            let mut tv = 0.0f64;
            for i in 0..width {
                if v[2] != 0 {
                    tv = t_at(&v);
                }
                out.write(i, w, tv as i64);
                v[0] = v[0].wrapping_add(unit[0]);
                v[1] = v[1].wrapping_add(unit[1]);
                v[2] = v[2].wrapping_add(unit[2]);
            }
        }
    }
}

pub struct Radial {
    pub c1: (i32, i32, i32),
    delta: (i32, i32, i32),
    a: f64,
    inva: f64,
    mindr: f64,
}

impl Radial {
    pub fn new(c1: (i32, i32, i32), c2: (i32, i32, i32)) -> Radial {
        let delta = (c2.0.wrapping_sub(c1.0), c2.1.wrapping_sub(c1.1), c2.2.wrapping_sub(c1.2));
        let a = dot(delta.0 as i64, delta.1 as i64, -(delta.2 as i64), delta.0 as i64, delta.1 as i64, delta.2 as i64) as f64;
        let inva = if a != 0.0 { 65536.0 / a } else { 0.0 };
        Radial { c1, delta, a, inva, mindr: -65536.0 * c1.2 as f64 }
    }

    /// Whether every point has a colour: one circle holds the other.
    pub fn covers(&self) -> bool {
        self.a < 0.0
    }

    fn write<O: Out + ?Sized>(&self, out: &mut O, i: usize, wk: &mut Walker, repeat: Repeat, b: f64, c: f64) {
        let dr = self.delta.2 as f64;
        let ok = |t: f64| if repeat == Repeat::None { (0.0..=65536.0).contains(&t) } else { t * dr >= self.mindr };
        if self.a == 0.0 {
            if b == 0.0 {
                out.clear(i);
                return;
            }
            let t = 32768.0 * c / b;
            if ok(t) {
                out.write(i, wk, t as i64);
            } else {
                out.clear(i);
            }
            return;
        }
        let discr = b * b + self.a * -c + 0.0 * 0.0;
        if discr >= 0.0 {
            let sq = discr.sqrt();
            let t0 = (b + sq) * self.inva;
            let t1 = (b - sq) * self.inva;
            if ok(t0) {
                out.write(i, wk, t0 as i64);
                return;
            }
            if ok(t1) {
                out.write(i, wk, t1 as i64);
                return;
            }
        }
        out.clear(i);
    }

    pub fn scanline<O: Out + ?Sized>(&self, out: &mut O, wk: &mut Walker, t: Option<&Transform>, repeat: Repeat, x: i32, y: i32) {
        let Some((mut v, unit)) = start(t, x, y) else { return };
        let width = out.len();
        let (c1, d) = (self.c1, self.delta);
        if unit[2] == 0 && v[2] == FIXED_1 {
            let vx = v[0].wrapping_sub(c1.0) as i64;
            let vy = v[1].wrapping_sub(c1.1) as i64;
            let (ux, uy) = (unit[0] as i64, unit[1] as i64);
            let mut b = dot(vx, vy, c1.2 as i64, d.0 as i64, d.1 as i64, d.2 as i64);
            let db = dot(ux, uy, 0, d.0 as i64, d.1 as i64, 0);
            let mut c = dot(vx, vy, -(c1.2 as i64), vx, vy, c1.2 as i64);
            let mut dc = dot(2 * vx + ux, 2 * vy + uy, 0, ux, uy, 0);
            let ddc = 2i64.wrapping_mul(dot(ux, uy, 0, ux, uy, 0));
            for i in 0..width {
                self.write(out, i, wk, repeat, b as f64, c as f64);
                b = b.wrapping_add(db);
                c = c.wrapping_add(dc);
                dc = dc.wrapping_add(ddc);
            }
        } else {
            for i in 0..width {
                if v[2] != 0 {
                    let invv2 = 65536.0 / v[2] as f64;
                    let pdx = v[0] as f64 * invv2 - c1.0 as f64;
                    let pdy = v[1] as f64 * invv2 - c1.1 as f64;
                    let b = pdx * d.0 as f64 + pdy * d.1 as f64 + c1.2 as f64 * d.2 as f64;
                    let c = pdx * pdx + pdy * pdy + -(c1.2 as f64) * c1.2 as f64;
                    self.write(out, i, wk, repeat, b, c);
                } else {
                    out.clear(i);
                }
                v[0] = v[0].wrapping_add(unit[0]);
                v[1] = v[1].wrapping_add(unit[1]);
                v[2] = v[2].wrapping_add(unit[2]);
            }
        }
    }
}

fn dot(x1: i64, y1: i64, z1: i64, x2: i64, y2: i64, z2: i64) -> i64 {
    x1.wrapping_mul(x2).wrapping_add(y1.wrapping_mul(y2)).wrapping_add(z1.wrapping_mul(z2))
}

pub struct Conical {
    pub center: (i32, i32),
    /// In radians, as pixman keeps it.
    pub angle: f64,
}

impl Conical {
    pub fn new(center: (i32, i32), angle: i32) -> Conical {
        let m = 360 << 16;
        let angle = if angle < 0 { (m - ((-(angle as i64) - 1) % m as i64) as i32) - 1 } else { angle % m };
        Conical { center, angle: (angle as f64 / 65536.0 / 180.0) * std::f64::consts::PI }
    }

    fn parameter(x: f64, y: f64, angle: f64) -> f64 {
        let pi2 = 2.0 * std::f64::consts::PI;
        let mut t = y.atan2(x) + angle;
        while t < 0.0 {
            t += pi2;
        }
        while t >= pi2 {
            t -= pi2;
        }
        1.0 - t * (1.0 / pi2)
    }

    pub fn scanline<O: Out + ?Sized>(&self, out: &mut O, wk: &mut Walker, t: Option<&Transform>, x: i32, y: i32) {
        let width = out.len();
        let (mut cx, mut cy, mut cz) = (1.0f64, 0.0f64, 0.0f64);
        let (mut rx, mut ry, mut rz) = (x as f64 + 0.5, y as f64 + 0.5, 1.0f64);
        let mut affine = true;
        if let Some(t) = t {
            let v = [(x << 16).wrapping_add(FIXED_1 / 2), (y << 16).wrapping_add(FIXED_1 / 2), FIXED_1];
            let Some(v) = matrix::point_3d(t, v) else { return };
            cx = t[0][0] as f64 / 65536.0;
            cy = t[1][0] as f64 / 65536.0;
            cz = t[2][0] as f64 / 65536.0;
            rx = v[0] as f64 / 65536.0;
            ry = v[1] as f64 / 65536.0;
            rz = v[2] as f64 / 65536.0;
            affine = t[2][0] == 0 && v[2] == FIXED_1;
        }
        let (ox, oy) = (self.center.0 as f64 / 65536.0, self.center.1 as f64 / 65536.0);
        if affine {
            rx -= ox;
            ry -= oy;
            for i in 0..width {
                let t = Self::parameter(rx, ry, self.angle);
                out.write(i, wk, (t * 65536.0) as i32 as i64);
                rx += cx;
                ry += cy;
            }
        } else {
            for i in 0..width {
                let (mut x, mut y) = if rz != 0.0 { (rx / rz, ry / rz) } else { (0.0, 0.0) };
                x -= ox;
                y -= oy;
                let t = Self::parameter(x, y, self.angle);
                out.write(i, wk, (t * 65536.0) as i32 as i64);
                rx += cx;
                ry += cy;
                rz += cz;
            }
        }
    }
}
