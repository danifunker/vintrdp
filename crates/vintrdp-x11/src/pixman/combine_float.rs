//! The float combiners, from pixman-combine-float.c: what pixman uses for
//! the operators that divide (saturate, the disjoint and conjoint sets,
//! color dodge and burn, soft light, the HSL modes) and for wide formats.
//! In f32 as pixman's, with no fused multiply-adds (as on x86-64).

use super::{op, Argb};

#[inline]
fn is_zero(f: f32) -> bool {
    -f32::MIN_POSITIVE < f && f < f32::MIN_POSITIVE
}

#[inline]
fn clamp(f: f32) -> f32 {
    if f < 0.0 {
        0.0
    } else if f > 1.0 {
        1.0
    } else {
        f
    }
}

#[derive(Clone, Copy)]
enum F {
    Zero,
    One,
    SrcAlpha,
    DestAlpha,
    InvSa,
    InvDa,
    SaOverDa,
    DaOverSa,
    InvSaOverDa,
    InvDaOverSa,
    OneMinusSaOverDa,
    OneMinusDaOverSa,
    OneMinusInvDaOverSa,
    OneMinusInvSaOverDa,
}

fn factor(f: F, sa: f32, da: f32) -> f32 {
    match f {
        F::Zero => 0.0,
        F::One => 1.0,
        F::SrcAlpha => sa,
        F::DestAlpha => da,
        F::InvSa => 1.0 - sa,
        F::InvDa => 1.0 - da,
        F::SaOverDa => {
            if is_zero(da) {
                1.0
            } else {
                clamp(sa / da)
            }
        }
        F::DaOverSa => {
            if is_zero(sa) {
                1.0
            } else {
                clamp(da / sa)
            }
        }
        F::InvSaOverDa => {
            if is_zero(da) {
                1.0
            } else {
                clamp((1.0 - sa) / da)
            }
        }
        F::InvDaOverSa => {
            if is_zero(sa) {
                1.0
            } else {
                clamp((1.0 - da) / sa)
            }
        }
        F::OneMinusSaOverDa => {
            if is_zero(da) {
                0.0
            } else {
                clamp(1.0 - sa / da)
            }
        }
        F::OneMinusDaOverSa => {
            if is_zero(sa) {
                0.0
            } else {
                clamp(1.0 - da / sa)
            }
        }
        F::OneMinusInvDaOverSa => {
            if is_zero(sa) {
                0.0
            } else {
                clamp(1.0 - (1.0 - da) / sa)
            }
        }
        F::OneMinusInvSaOverDa => {
            if is_zero(da) {
                0.0
            } else {
                clamp(1.0 - (1.0 - sa) / da)
            }
        }
    }
}

/// The Porter-Duff factors of an operator (pixman's MAKE_PD_COMBINERS).
fn pd(o: u8) -> Option<(F, F)> {
    use F::*;
    Some(match o {
        op::CLEAR | op::DISJOINT_CLEAR | op::CONJOINT_CLEAR => (Zero, Zero),
        op::SRC | op::DISJOINT_SRC | op::CONJOINT_SRC => (One, Zero),
        op::DST | op::DISJOINT_DST | op::CONJOINT_DST => (Zero, One),
        op::OVER => (One, InvSa),
        op::OVER_REVERSE => (InvDa, One),
        op::IN => (DestAlpha, Zero),
        op::IN_REVERSE => (Zero, SrcAlpha),
        op::OUT => (InvDa, Zero),
        op::OUT_REVERSE => (Zero, InvSa),
        op::ATOP => (DestAlpha, InvSa),
        op::ATOP_REVERSE => (InvDa, SrcAlpha),
        op::XOR => (InvDa, InvSa),
        op::ADD => (One, One),
        op::SATURATE => (InvDaOverSa, One),
        op::DISJOINT_OVER => (One, InvSaOverDa),
        op::DISJOINT_OVER_REVERSE => (InvDaOverSa, One),
        op::DISJOINT_IN => (OneMinusInvDaOverSa, Zero),
        op::DISJOINT_IN_REVERSE => (Zero, OneMinusInvSaOverDa),
        op::DISJOINT_OUT => (InvDaOverSa, Zero),
        op::DISJOINT_OUT_REVERSE => (Zero, InvSaOverDa),
        op::DISJOINT_ATOP => (OneMinusInvDaOverSa, InvSaOverDa),
        op::DISJOINT_ATOP_REVERSE => (InvDaOverSa, OneMinusInvSaOverDa),
        op::DISJOINT_XOR => (InvDaOverSa, InvSaOverDa),
        op::CONJOINT_OVER => (One, OneMinusSaOverDa),
        op::CONJOINT_OVER_REVERSE => (OneMinusDaOverSa, One),
        op::CONJOINT_IN => (DaOverSa, Zero),
        op::CONJOINT_IN_REVERSE => (Zero, SaOverDa),
        op::CONJOINT_OUT => (OneMinusDaOverSa, Zero),
        op::CONJOINT_OUT_REVERSE => (Zero, OneMinusSaOverDa),
        op::CONJOINT_ATOP => (DaOverSa, OneMinusSaOverDa),
        op::CONJOINT_ATOP_REVERSE => (OneMinusDaOverSa, SaOverDa),
        op::CONJOINT_XOR => (OneMinusDaOverSa, OneMinusSaOverDa),
        _ => return None,
    })
}

#[inline]
fn min1(v: f32) -> f32 {
    if 1.0 < v { 1.0 } else { v }
}

fn blend(o: u8, sa: f32, s: f32, da: f32, d: f32) -> f32 {
    match o {
        op::MULTIPLY => d * s,
        op::SCREEN => d * sa + s * da - s * d,
        op::OVERLAY => {
            if 2.0 * d < da {
                2.0 * s * d
            } else {
                sa * da - 2.0 * (da - d) * (sa - s)
            }
        }
        op::DARKEN => {
            let (s, d) = (s * da, d * sa);
            if s > d { d } else { s }
        }
        op::LIGHTEN => {
            let (s, d) = (s * da, d * sa);
            if s > d { s } else { d }
        }
        op::COLOR_DODGE => {
            if is_zero(d) {
                0.0
            } else if d * sa >= sa * da - s * da {
                sa * da
            } else if is_zero(sa - s) {
                sa * da
            } else {
                sa * sa * d / (sa - s)
            }
        }
        op::COLOR_BURN => {
            if d >= da {
                sa * da
            } else if sa * (da - d) >= s * da {
                0.0
            } else if is_zero(s) {
                0.0
            } else {
                sa * (da - sa * (da - d) / s)
            }
        }
        op::HARD_LIGHT => {
            if 2.0 * s < sa {
                2.0 * s * d
            } else {
                sa * da - 2.0 * (da - d) * (sa - s)
            }
        }
        op::SOFT_LIGHT => {
            if 2.0 * s <= sa {
                if is_zero(da) {
                    d * sa
                } else {
                    d * sa - d * (da - d) * (sa - 2.0 * s) / da
                }
            } else if is_zero(da) {
                d * sa
            } else if 4.0 * d <= da {
                d * sa + (2.0 * s - sa) * d * ((16.0 * d / da - 12.0) * d / da + 3.0)
            } else {
                d * sa + ((d * da).sqrt() - d) * (2.0 * s - sa)
            }
        }
        op::DIFFERENCE => {
            let (dsa, sda) = (d * sa, s * da);
            if sda < dsa { dsa - sda } else { sda - dsa }
        }
        // EXCLUSION
        _ => s * da + d * sa - 2.0 * d * s,
    }
}

fn is_separable(o: u8) -> bool {
    (op::MULTIPLY..=op::EXCLUSION).contains(&o)
}

#[derive(Clone, Copy)]
struct Rgb {
    r: f32,
    g: f32,
    b: f32,
}

fn lum(c: &Rgb) -> f32 {
    c.r * 0.3 + c.g * 0.59 + c.b * 0.11
}

fn cmin(c: &Rgb) -> f32 {
    let m = if c.r < c.g { c.r } else { c.g };
    if m < c.b { m } else { c.b }
}

fn cmax(c: &Rgb) -> f32 {
    let m = if c.r > c.g { c.r } else { c.g };
    if m > c.b { m } else { c.b }
}

fn sat(c: &Rgb) -> f32 {
    cmax(c) - cmin(c)
}

fn clip_color(c: &mut Rgb, a: f32) {
    let l = lum(c);
    let n = cmin(c);
    let x = cmax(c);
    if n < 0.0 {
        let t = l - n;
        if is_zero(t) {
            (c.r, c.g, c.b) = (0.0, 0.0, 0.0);
        } else {
            c.r = l + (((c.r - l) * l) / t);
            c.g = l + (((c.g - l) * l) / t);
            c.b = l + (((c.b - l) * l) / t);
        }
    }
    if x > a {
        let t = x - l;
        if is_zero(t) {
            (c.r, c.g, c.b) = (a, a, a);
        } else {
            c.r = l + ((c.r - l) * (a - l) / t);
            c.g = l + ((c.g - l) * (a - l) / t);
            c.b = l + ((c.b - l) * (a - l) / t);
        }
    }
}

fn set_lum(c: &mut Rgb, sa: f32, l: f32) {
    let d = l - lum(c);
    c.r += d;
    c.g += d;
    c.b += d;
    clip_color(c, sa);
}

fn set_sat(c: &mut Rgb, s: f32) {
    // Which of r, g, b is the largest, the middle and the smallest, as
    // pixman decides it.
    let (max, mid, min) = if c.r > c.g {
        if c.r > c.b {
            if c.g > c.b { (0, 1, 2) } else { (0, 2, 1) }
        } else {
            (2, 0, 1)
        }
    } else if c.r > c.b {
        (1, 0, 2)
    } else if c.g > c.b {
        (1, 2, 0)
    } else {
        (2, 1, 0)
    };
    let mut v = [c.r, c.g, c.b];
    let t = v[max] - v[min];
    if is_zero(t) {
        v[mid] = 0.0;
        v[max] = 0.0;
    } else {
        v[mid] = ((v[mid] - v[min]) * s) / t;
        v[max] = s;
    }
    v[min] = 0.0;
    (c.r, c.g, c.b) = (v[0], v[1], v[2]);
}

fn hsl(o: u8, dc: &Rgb, da: f32, sc: &Rgb, sa: f32) -> Rgb {
    let mut r;
    match o {
        op::HSL_HUE => {
            r = Rgb { r: sc.r * da, g: sc.g * da, b: sc.b * da };
            set_sat(&mut r, sat(dc) * sa);
            set_lum(&mut r, sa * da, lum(dc) * sa);
        }
        op::HSL_SATURATION => {
            r = Rgb { r: dc.r * sa, g: dc.g * sa, b: dc.b * sa };
            set_sat(&mut r, sat(sc) * da);
            set_lum(&mut r, sa * da, lum(dc) * sa);
        }
        op::HSL_COLOR => {
            r = Rgb { r: sc.r * da, g: sc.g * da, b: sc.b * da };
            set_lum(&mut r, sa * da, lum(dc) * sa);
        }
        // HSL_LUMINOSITY
        _ => {
            r = Rgb { r: dc.r * sa, g: dc.g * sa, b: dc.b * sa };
            set_lum(&mut r, sa * da, lum(sc) * da);
        }
    }
    r
}

/// Combine `src` (by `mask`, unified or component alpha) into `dest`.
pub fn combine(o: u8, ca: bool, dest: &mut [Argb], src: &[Argb], mask: Option<&[Argb]>) {
    let n = dest.len();
    if (op::HSL_HUE..=op::HSL_LUMINOSITY).contains(&o) {
        if ca {
            // pixman has no component-alpha HSL: the destination's left.
            return;
        }
        for i in 0..n {
            let s = src[i];
            let (mut sa, mut sc) = (s.a, Rgb { r: s.r, g: s.g, b: s.b });
            let d = dest[i];
            let (da, dc) = (d.a, Rgb { r: d.r, g: d.g, b: d.b });
            if let Some(m) = mask {
                let ma = m[i].a;
                sa *= ma;
                sc.r *= ma;
                sc.g *= ma;
                sc.b *= ma;
            }
            let rc = hsl(o, &dc, da, &sc, sa);
            dest[i] = Argb {
                a: sa + da - sa * da,
                r: (1.0 - sa) * dc.r + (1.0 - da) * sc.r + rc.r,
                g: (1.0 - sa) * dc.g + (1.0 - da) * sc.g + rc.g,
                b: (1.0 - sa) * dc.b + (1.0 - da) * sc.b + rc.b,
            };
        }
        return;
    }
    let pdf = is_separable(o);
    let factors = if pdf { None } else { pd(o) };
    if !pdf && factors.is_none() {
        return;
    }
    let comb_a = |sa: f32, s: f32, da: f32, d: f32| -> f32 {
        match factors {
            Some((a, b)) => min1(s * factor(a, sa, da) + d * factor(b, sa, da)),
            None => da + sa - da * sa,
        }
    };
    let comb_c = |sa: f32, s: f32, da: f32, d: f32| -> f32 {
        match factors {
            Some((a, b)) => min1(s * factor(a, sa, da) + d * factor(b, sa, da)),
            None => {
                let f = (1.0 - sa) * d + (1.0 - da) * s;
                f + blend(o, sa, s, da, d)
            }
        }
    };
    for i in 0..n {
        let s = src[i];
        let d = dest[i];
        let (mut sa, mut sr, mut sg, mut sb) = (s.a, s.r, s.g, s.b);
        let (ma, mr, mg, mb);
        match mask {
            None => {
                (ma, mr, mg, mb) = (sa, sa, sa, sa);
            }
            Some(m) => {
                let m = m[i];
                if ca {
                    sr *= m.r;
                    sg *= m.g;
                    sb *= m.b;
                    (ma, mr, mg, mb) = (m.a * sa, m.r * sa, m.g * sa, m.b * sa);
                    sa = ma;
                } else {
                    let a = m.a;
                    sa *= a;
                    sr *= a;
                    sg *= a;
                    sb *= a;
                    (ma, mr, mg, mb) = (sa, sa, sa, sa);
                }
            }
        }
        dest[i] = Argb { a: comb_a(ma, sa, d.a, d.a), r: comb_c(mr, sr, d.a, d.r), g: comb_c(mg, sg, d.a, d.g), b: comb_c(mb, sb, d.a, d.b) };
    }
}
