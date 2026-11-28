//! The 8-bit combiners, from pixman-combine32.c: each operator on a8r8g8b8
//! scanlines, with a unified (alpha) or component-alpha mask, in pixman's
//! integer arithmetic (x * a / 255 rounded as MUL_UN8, saturating adds).

use super::op;

#[inline]
fn a8(x: u32) -> u32 {
    x >> 24
}

/// MUL_UN8: a * b / 255, rounded.
#[inline]
fn mul(a: u32, b: u32) -> u32 {
    let t = a * b + 0x80;
    ((t >> 8) + t) >> 8
}

/// Each channel of x times a.
#[inline]
fn mul4(x: u32, a: u32) -> u32 {
    let mut r = 0;
    for s in [0, 8, 16, 24] {
        r |= mul(x >> s & 0xff, a) << s;
    }
    r
}

/// Each channel of x times the same channel of a.
#[inline]
fn mul4x4(x: u32, a: u32) -> u32 {
    let mut r = 0;
    for s in [0, 8, 16, 24] {
        r |= mul(x >> s & 0xff, a >> s & 0xff) << s;
    }
    r
}

/// Each channel of x plus y's, saturating.
#[inline]
fn add4(x: u32, y: u32) -> u32 {
    let mut r = 0;
    for s in [0, 8, 16, 24] {
        r |= ((x >> s & 0xff) + (y >> s & 0xff)).min(0xff) << s;
    }
    r
}

/// UN8x4_MUL_UN8_ADD_UN8x4: x * a + y.
#[inline]
fn mul_add(x: u32, a: u32, y: u32) -> u32 {
    add4(mul4(x, a), y)
}

/// UN8x4_MUL_UN8_ADD_UN8x4_MUL_UN8: x * a + y * b.
#[inline]
fn mul_add_mul(x: u32, a: u32, y: u32, b: u32) -> u32 {
    add4(mul4(x, a), mul4(y, b))
}

/// UN8x4_MUL_UN8x4_ADD_UN8x4: x * a (per channel) + y.
#[inline]
fn mul4x4_add(x: u32, a: u32, y: u32) -> u32 {
    add4(mul4x4(x, a), y)
}

/// UN8x4_MUL_UN8x4_ADD_UN8x4_MUL_UN8: x * a (per channel) + y * b.
#[inline]
fn mul4x4_add_mul(x: u32, a: u32, y: u32, b: u32) -> u32 {
    add4(mul4x4(x, a), mul4(y, b))
}

#[inline]
fn combine_mask(src: &[u32], mask: Option<&[u32]>, i: usize) -> u32 {
    match mask {
        Some(m) => {
            let m = a8(m[i]);
            if m == 0 {
                return 0;
            }
            mul4(src[i], m)
        }
        None => src[i],
    }
}

/// combine_mask_ca: src by the mask, the mask by src's alpha.
#[inline]
fn mask_ca(s: &mut u32, m: &mut u32) {
    let a = *m;
    if a == 0 {
        *s = 0;
        return;
    }
    let x = *s;
    if a == !0 {
        let x = x >> 24;
        *m = x | x << 8 | x << 16 | x << 24;
        return;
    }
    let xa = x >> 24;
    *s = mul4x4(x, a);
    *m = mul4(a, xa);
}

#[inline]
fn mask_value_ca(s: &mut u32, m: u32) {
    if m == 0 {
        *s = 0;
        return;
    }
    if m == !0 {
        return;
    }
    *s = mul4x4(*s, m);
}

#[inline]
fn mask_alpha_ca(s: u32, m: &mut u32) {
    let a = *m;
    if a == 0 {
        return;
    }
    let x = s >> 24;
    if x == 0xff {
        return;
    }
    if a == !0 {
        *m = x | x << 8 | x << 16 | x << 24;
        return;
    }
    *m = mul4(a, x);
}

fn blend(op: u8, d: i32, ad: i32, s: i32, as_: i32) -> i32 {
    match op {
        op::SCREEN => s * ad + d * as_ - s * d,
        op::OVERLAY => {
            if 2 * d < ad {
                2 * s * d
            } else {
                (as_ * ad).wrapping_sub(2 * (ad - d) * (as_ - s))
            }
        }
        op::DARKEN => {
            let (s, d) = (ad * s, as_ * d);
            if s > d { d } else { s }
        }
        op::LIGHTEN => {
            let (s, d) = (ad * s, as_ * d);
            if s > d { s } else { d }
        }
        op::HARD_LIGHT => {
            if 2 * s < as_ {
                2 * s * d
            } else {
                as_ * ad - 2 * (ad - d) * (as_ - s)
            }
        }
        op::DIFFERENCE => {
            let (das, sad) = (d * as_, s * ad);
            if sad < das { das - sad } else { sad - das }
        }
        // EXCLUSION
        _ => s * ad + d * as_ - 2 * d * s,
    }
}

/// PDF_SEPARABLE_BLEND_MODE's sums, in unsigned arithmetic as pixman's.
#[inline]
fn pdf(op: u8, s: u32, d: u32, ms: [u32; 3], sa: u32) -> u32 {
    let da = a8(d);
    let ida = 255 - da;
    let ra = (da * 0xff + sa * 0xff).wrapping_sub(sa * da);
    let mut out = ra.min(255 * 255);
    out = div_one(out) << 24;
    for (k, sh) in [16, 8, 0].into_iter().enumerate() {
        let (dc, sc) = (d >> sh & 0xff, s >> sh & 0xff);
        let m = ms[k];
        let mut v = (255 - m) * dc + ida * sc;
        v = v.wrapping_add(blend(op, dc as i32, da as i32, sc as i32, m as i32) as u32);
        out |= div_one(v.min(255 * 255)) << sh;
    }
    out
}

/// DIV_ONE_UN8.
#[inline]
fn div_one(x: u32) -> u32 {
    (x + 0x80 + ((x + 0x80) >> 8)) >> 8
}

/// Unified alpha: mask is None, or each pixel's alpha counts.
pub fn combine_u(o: u8, dest: &mut [u32], src: &[u32], mask: Option<&[u32]>) {
    let n = dest.len();
    match o {
        op::CLEAR => dest.fill(0),
        op::SRC => match mask {
            None => dest.copy_from_slice(&src[..n]),
            Some(_) => {
                for i in 0..n {
                    dest[i] = combine_mask(src, mask, i);
                }
            }
        },
        op::DST => {}
        op::OVER => {
            for i in 0..n {
                let s = match mask {
                    None => src[i],
                    Some(m) => {
                        let m = a8(m[i]);
                        if m == 0xff {
                            src[i]
                        } else if m != 0 {
                            let s = src[i];
                            if s != 0 {
                                let s = mul4(s, m);
                                dest[i] = mul_add(dest[i], a8(!s), s);
                            }
                            continue;
                        } else {
                            continue;
                        }
                    }
                };
                let a = a8(s);
                if a == 0xff {
                    dest[i] = s;
                } else if s != 0 {
                    dest[i] = mul_add(dest[i], a ^ 0xff, s);
                }
            }
        }
        op::OVER_REVERSE => {
            for i in 0..n {
                let s = combine_mask(src, mask, i);
                let d = dest[i];
                dest[i] = mul_add(s, a8(!d), d);
            }
        }
        op::IN => {
            for i in 0..n {
                dest[i] = mul4(combine_mask(src, mask, i), a8(dest[i]));
            }
        }
        op::IN_REVERSE => {
            for i in 0..n {
                let s = combine_mask(src, mask, i);
                dest[i] = mul4(dest[i], a8(s));
            }
        }
        op::OUT => {
            for i in 0..n {
                dest[i] = mul4(combine_mask(src, mask, i), a8(!dest[i]));
            }
        }
        op::OUT_REVERSE => {
            for i in 0..n {
                let s = combine_mask(src, mask, i);
                dest[i] = mul4(dest[i], a8(!s));
            }
        }
        op::ATOP => {
            for i in 0..n {
                let s = combine_mask(src, mask, i);
                let d = dest[i];
                dest[i] = mul_add_mul(s, a8(d), d, a8(!s));
            }
        }
        op::ATOP_REVERSE => {
            for i in 0..n {
                let s = combine_mask(src, mask, i);
                let d = dest[i];
                dest[i] = mul_add_mul(s, a8(!d), d, a8(s));
            }
        }
        op::XOR => {
            for i in 0..n {
                let s = combine_mask(src, mask, i);
                let d = dest[i];
                dest[i] = mul_add_mul(s, a8(!d), d, a8(!s));
            }
        }
        op::ADD => {
            for i in 0..n {
                dest[i] = add4(dest[i], combine_mask(src, mask, i));
            }
        }
        op::MULTIPLY => {
            for i in 0..n {
                let s = combine_mask(src, mask, i);
                let d = dest[i];
                let ss = mul_add_mul(s, a8(!d), d, a8(!s));
                dest[i] = add4(mul4x4(d, s), ss);
            }
        }
        op::SCREEN | op::OVERLAY | op::DARKEN | op::LIGHTEN | op::HARD_LIGHT | op::DIFFERENCE | op::EXCLUSION => {
            for i in 0..n {
                let s = combine_mask(src, mask, i);
                let sa = a8(s);
                dest[i] = pdf(o, s, dest[i], [sa; 3], sa);
            }
        }
        // No 8-bit combiner (pixman's dummy_combine): nothing happens.
        _ => {}
    }
}

/// Component alpha: each channel of the mask counts.
pub fn combine_ca(o: u8, dest: &mut [u32], src: &[u32], mask: &[u32]) {
    let n = dest.len();
    match o {
        op::CLEAR => dest.fill(0),
        op::SRC => {
            for i in 0..n {
                let mut s = src[i];
                mask_value_ca(&mut s, mask[i]);
                dest[i] = s;
            }
        }
        op::OVER => {
            for i in 0..n {
                let (mut s, mut m) = (src[i], mask[i]);
                mask_ca(&mut s, &mut m);
                let a = !m;
                if a != 0 {
                    s = mul4x4_add(dest[i], a, s);
                }
                dest[i] = s;
            }
        }
        op::OVER_REVERSE => {
            for i in 0..n {
                let d = dest[i];
                let a = !d >> 24;
                if a != 0 {
                    let s = mul4x4(src[i], mask[i]);
                    dest[i] = mul_add(s, a, d);
                }
            }
        }
        op::IN => {
            for i in 0..n {
                let a = dest[i] >> 24;
                let mut s = 0;
                if a != 0 {
                    s = src[i];
                    mask_value_ca(&mut s, mask[i]);
                    if a != 0xff {
                        s = mul4(s, a);
                    }
                }
                dest[i] = s;
            }
        }
        op::IN_REVERSE => {
            for i in 0..n {
                let mut m = mask[i];
                mask_alpha_ca(src[i], &mut m);
                if m != !0 {
                    let mut d = 0;
                    if m != 0 {
                        d = mul4x4(dest[i], m);
                    }
                    dest[i] = d;
                }
            }
        }
        op::OUT => {
            for i in 0..n {
                let a = (!dest[i] >> 24) & 0xff;
                let mut s = 0;
                if a != 0 {
                    s = src[i];
                    mask_value_ca(&mut s, mask[i]);
                    if a != 0xff {
                        s = mul4(s, a);
                    }
                }
                dest[i] = s;
            }
        }
        op::OUT_REVERSE => {
            for i in 0..n {
                let mut m = mask[i];
                mask_alpha_ca(src[i], &mut m);
                let a = !m;
                if a != !0 {
                    let mut d = 0;
                    if a != 0 {
                        d = mul4x4(dest[i], a);
                    }
                    dest[i] = d;
                }
            }
        }
        op::ATOP => {
            for i in 0..n {
                let (d, mut s, mut m) = (dest[i], src[i], mask[i]);
                let as_ = d >> 24;
                mask_ca(&mut s, &mut m);
                dest[i] = mul4x4_add_mul(d, !m, s, as_);
            }
        }
        op::ATOP_REVERSE => {
            for i in 0..n {
                let (d, mut s, mut m) = (dest[i], src[i], mask[i]);
                let as_ = (!d >> 24) & 0xff;
                mask_ca(&mut s, &mut m);
                dest[i] = mul4x4_add_mul(d, m, s, as_);
            }
        }
        op::XOR => {
            for i in 0..n {
                let (d, mut s, mut m) = (dest[i], src[i], mask[i]);
                let as_ = (!d >> 24) & 0xff;
                mask_ca(&mut s, &mut m);
                dest[i] = mul4x4_add_mul(d, !m, s, as_);
            }
        }
        op::ADD => {
            for i in 0..n {
                let mut s = src[i];
                mask_value_ca(&mut s, mask[i]);
                dest[i] = add4(dest[i], s);
            }
        }
        op::MULTIPLY => {
            for i in 0..n {
                let (mut m, mut s, d) = (mask[i], src[i], dest[i]);
                mask_ca(&mut s, &mut m);
                let r = mul4x4_add_mul(d, !m, s, a8(!d));
                dest[i] = add4(r, mul4x4(d, s));
            }
        }
        op::SCREEN | op::OVERLAY | op::DARKEN | op::LIGHTEN | op::HARD_LIGHT | op::DIFFERENCE | op::EXCLUSION => {
            for i in 0..n {
                let (mut m, mut s, d) = (mask[i], src[i], dest[i]);
                mask_ca(&mut s, &mut m);
                dest[i] = pdf(o, s, d, [m >> 16 & 0xff, m >> 8 & 0xff, m & 0xff], a8(s));
            }
        }
        // DST, and the operators pixman has no 8-bit combiner for.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_as_pixman() {
        // MUL_UN8 is x * a / 255 rounded to nearest.
        for x in 0..256 {
            for a in 0..256 {
                assert_eq!(mul(x, a), (x * a + 127) / 255, "{x} {a}");
            }
        }
        let mut d = [0x80ff_0000u32];
        combine_u(op::OVER, &mut d, &[0x8000_0080], None);
        assert_eq!(d, [0xc07f_0080]);
        let mut d = [0x2020_2020];
        combine_u(op::ADD, &mut d, &[0xf0f0_1010], None);
        assert_eq!(d, [0xffff_3030]);
    }
}
