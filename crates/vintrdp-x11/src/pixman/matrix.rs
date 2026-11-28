//! Transforms, from pixman-matrix.c: points through a 3x3 matrix of 16.16
//! numbers, the projective ones divided with rounding as pixman does it.

use super::{Transform, FIXED_1};

/// pixman_transform_point_31_16_3d: no division, 48.16 results.
pub fn point_3d_48(t: &Transform, v: [i64; 3]) -> [i64; 3] {
    let mut out = [0i64; 3];
    for (i, o) in out.iter_mut().enumerate() {
        let mut hi = 0i64;
        let mut lo = 0i64;
        for (j, &vj) in v.iter().enumerate() {
            let m = t[i][j] as i64;
            hi = hi.wrapping_add(m.wrapping_mul(vj >> 16));
            lo = lo.wrapping_add(m.wrapping_mul(vj & 0xffff));
        }
        *o = hi.wrapping_add(lo.wrapping_add(0x8000) >> 16);
    }
    out
}

/// pixman_transform_point_3d: the results in 16.16, or None if one of them
/// doesn't fit (pixman then leaves the scanline as it was).
pub fn point_3d(t: &Transform, v: [i32; 3]) -> Option<[i32; 3]> {
    let r = point_3d_48(t, [v[0] as i64, v[1] as i64, v[2] as i64]);
    let out = [r[0] as i32, r[1] as i32, r[2] as i32];
    (out[0] as i64 == r[0] && out[1] as i64 == r[1] && out[2] as i64 == r[2]).then_some(out)
}

/// Rounded signed division of a 128-bit number, as rounded_sdiv_128_by_49:
/// the magnitude rounded half up, then the sign.
fn rounded_sdiv(n: i128, d: i128) -> i128 {
    let neg = (n < 0) != (d < 0);
    let (n, d) = (n.unsigned_abs(), d.unsigned_abs());
    let mut q = n / d;
    if (n % d) * 2 >= d {
        q += 1;
    }
    if neg { -(q as i128) } else { q as i128 }
}

/// fixed_64_16_to_int128: (hi + lo / 65536) * 2^scalebits, floored.
fn to_128(hi: i64, lo: i64, scalebits: i32) -> i128 {
    let hi = hi.wrapping_add(lo >> 16);
    let lo = lo & 0xffff;
    if scalebits <= 0 {
        (hi >> (-scalebits)) as i128
    } else {
        let x = ((hi as i128) << 16) | lo as i128;
        (x << scalebits) >> 16
    }
}

fn clamp_48(v: i128, clamped: &mut bool) -> i64 {
    if v > i64::MAX as i128 || v < i64::MIN as i128 {
        *clamped = true;
        if v >= 0 { i64::MAX } else { i64::MIN }
    } else {
        v as i64
    }
}

/// pixman_transform_point_31_16: divided by w, 48.16; false if clamped.
pub fn point_31_16(t: &Transform, v: [i64; 3]) -> ([i64; 3], bool) {
    let mut clamped = false;
    let mut tmp = [[0i64; 2]; 3];
    for (i, row) in tmp.iter_mut().enumerate() {
        for (j, &vj) in v.iter().enumerate() {
            let m = t[i][j] as i64;
            row[0] = row[0].wrapping_add(m.wrapping_mul(vj >> 16));
            row[1] = row[1].wrapping_add(m.wrapping_mul(vj & 0xffff));
        }
    }
    let divint = tmp[2][0].wrapping_add(tmp[2][1] >> 16);
    let divfrac = (tmp[2][1] & 0xffff) as u16;
    let mut r = [0i64; 3];
    if divint == FIXED_1 as i64 && divfrac == 0 {
        r[0] = tmp[0][0].wrapping_add(tmp[0][1].wrapping_add(0x8000) >> 16);
        r[1] = tmp[1][0].wrapping_add(tmp[1][1].wrapping_add(0x8000) >> 16);
    } else if divint == 0 && divfrac == 0 {
        clamped = true;
        for i in 0..2 {
            let x = tmp[i][0].wrapping_add(tmp[i][1].wrapping_add(0x8000) >> 16);
            r[i] = match x {
                x if x > 0 => i64::MAX,
                x if x < 0 => i64::MIN,
                _ => 0,
            };
        }
    } else {
        let mut hi32 = (divint >> 32) as i32;
        if hi32 < 0 {
            hi32 = !hi32;
        }
        if hi32 == 0 {
            let div = (((divint as u64) << 16) + divfrac as u64) as i64 as i128;
            for i in 0..2 {
                let n = to_128(tmp[i][0], tmp[i][1], 32);
                r[i] = clamp_48(rounded_sdiv(n, div), &mut clamped);
            }
        } else {
            let shift = 32 - (hi32 as u32).leading_zeros() as i32;
            let div = to_128(divint, divfrac as i64, 16 - shift) as i64 as i128;
            for i in 0..2 {
                let n = to_128(tmp[i][0], tmp[i][1], 32 - shift);
                r[i] = clamp_48(rounded_sdiv(n, div), &mut clamped);
            }
        }
    }
    r[2] = FIXED_1 as i64;
    (r, !clamped)
}

/// pixman_transform_point: the point divided through, in 16.16, or None.
pub fn point(t: &Transform, v: [i32; 3]) -> Option<[i32; 3]> {
    let (r, ok) = point_31_16(t, [v[0] as i64, v[1] as i64, v[2] as i64]);
    if !ok {
        return None;
    }
    let out = [r[0] as i32, r[1] as i32, r[2] as i32];
    (out[0] as i64 == r[0] && out[1] as i64 == r[1] && out[2] as i64 == r[2]).then_some(out)
}

/// pixman_transform_multiply: None when a value overflows 16.16.
pub fn multiply(l: &Transform, r: &Transform) -> Option<Transform> {
    let mut d = [[0i32; 3]; 3];
    for dy in 0..3 {
        for dx in 0..3 {
            let mut v = 0i64;
            for o in 0..3 {
                let partial = l[dy][o] as i64 * r[o][dx] as i64;
                v += (partial + 0x8000) >> 16;
            }
            if v > 0x7fff_ffff || v < -0x8000_0000 {
                return None;
            }
            d[dy][dx] = v as i32;
        }
    }
    Some(d)
}

/// pixman_transform_translate of the forward transform: translate after it.
pub fn translate(t: &Transform, tx: i32, ty: i32) -> Transform {
    let tr = [[FIXED_1, 0, tx], [0, FIXED_1, ty], [0, 0, FIXED_1]];
    multiply(&tr, t).unwrap_or(*t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projective_points() {
        let id = [[FIXED_1, 0, 0], [0, FIXED_1, 0], [0, 0, FIXED_1]];
        assert_eq!(point(&id, [5 << 16, 7 << 16, FIXED_1]), Some([5 << 16, 7 << 16, FIXED_1]));
        // Halved through w = 2: rounded.
        let half = [[FIXED_1, 0, 0], [0, FIXED_1, 0], [0, 0, 2 * FIXED_1]];
        assert_eq!(point(&half, [3, 5 << 16, FIXED_1]), Some([2, 5 << 15, FIXED_1]));
        assert_eq!(point(&half, [-3, 0, FIXED_1]), Some([-2, 0, FIXED_1]));
        let zero = [[FIXED_1, 0, 0], [0, FIXED_1, 0], [0, 0, 0]];
        assert_eq!(point(&zero, [1, 1, FIXED_1]), None);
        assert_eq!(translate(&id, 3 << 16, -2 << 16), [[FIXED_1, 0, 3 << 16], [0, FIXED_1, -2 << 16], [0, 0, FIXED_1]]);
    }
}
