//! Pointer shapes: the server's AND and XOR masks to an RGBA image. Port of
//! rdesktop's X.c ui_create_cursor() and get_pixel(), as the old shim.rs
//! did it.

#![allow(clippy::too_many_arguments)]

use super::colour::{Colours, rgb15, rgb16};
use crate::session::CursorImage;

/// Bytes per mask row: rows are padded to 16 bits.
fn stride(width: usize, bpp: usize) -> usize {
    (width * bpp).div_ceil(16) * 2
}

fn bit(mask: &[u8], row: usize, x: usize) -> bool {
    mask.get(row + x / 8).is_some_and(|b| b & (0x80 >> (x % 8)) != 0)
}

/// The largest pointer RDP has (a large pointer's limit).
const MAX_SIDE: u16 = 384;

/// Make a cursor from its masks. `None` for a depth we can't draw, an empty
/// or oversized pointer, or an XOR mask too short for its size (the AND mask
/// may be short: missing bits are opaque, as before).
///
/// Rows are bottom-up, except 1 bpp XOR masks, which are kept as xwin.c
/// had them. An AND bit with a non-black XOR pixel inverts the screen; we
/// can't, so those pixels are black with a white outline.
pub fn decode(colours: &Colours, hot_x: u16, hot_y: u16, width: u16, height: u16, and: &[u8], xor: &[u8], bpp: u32) -> Option<CursorImage> {
    let (w, h) = (width as usize, height as usize);
    if !matches!(bpp, 1 | 8 | 15 | 16 | 24 | 32) || w == 0 || h == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let bpp = bpp as usize;
    let (and_stride, xor_stride) = (stride(w, 1), stride(w, bpp));
    if xor.len() < xor_stride * h {
        return None;
    }

    let mut argb = vec![0u32; w * h];
    let mut inverted = false;
    for y in 0..h {
        let flipped = h - 1 - y;
        let out_row = if bpp == 1 { y } else { flipped };
        let (arow, xrow) = (y * and_stride, y * xor_stride);
        for x in 0..w {
            let transparent = bit(and, arow, x);
            let rgb = match bpp {
                1 => {
                    if bit(xor, xrow, x) {
                        0xffffff
                    } else {
                        0
                    }
                }
                8 => colours.palette[xor[xrow + x] as usize],
                15 | 16 => {
                    let p = xrow + x * 2;
                    let c = u16::from_le_bytes([xor[p], xor[p + 1]]) as u32;
                    if bpp == 15 { rgb15(c) } else { rgb16(c) }
                }
                24 => {
                    let p = &xor[xrow + x * 3..xrow + x * 3 + 3];
                    (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32
                }
                _ => {
                    // 32 bpp carries its own alpha.
                    let p = &xor[xrow + x * 4..xrow + x * 4 + 4];
                    argb[out_row * w + x] = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                    continue;
                }
            };
            argb[out_row * w + x] = if transparent && rgb != 0 {
                inverted = true;
                0xff00_0000
            } else if transparent {
                0
            } else {
                0xff00_0000 | rgb
            };
        }
    }
    if inverted {
        outline(&mut argb, w, h);
    }

    let rgba = argb.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]).collect();
    // Keep the hotspot inside the image.
    let (hot_x, hot_y) = (hot_x.min(width - 1) as usize, hot_y.min(height - 1) as usize);
    Some(CursorImage { width: w, height: h, hot_x, hot_y, rgba })
}

/// Make transparent pixels next to opaque ones white, so a black shape shows
/// on a dark background.
fn outline(argb: &mut [u32], w: usize, h: usize) {
    let src = argb.to_vec();
    let opaque = |x: i64, y: i64| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && src[y as usize * w + x as usize] >> 24 != 0;
    for y in 0..h {
        for x in 0..w {
            if src[y * w + x] >> 24 != 0 {
                continue;
            }
            let (xi, yi) = (x as i64, y as i64);
            if opaque(xi - 1, yi) || opaque(xi + 1, yi) || opaque(xi, yi - 1) || opaque(xi, yi + 1) {
                argb[y * w + x] = 0xffff_ffff;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(c: &CursorImage, x: usize, y: usize) -> [u8; 4] {
        let i = (y * c.width + x) * 4;
        c.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn and_mask_with_24bpp_xor() {
        // 2x2, rows bottom-up. Bottom row: red opaque, transparent.
        // Top row: inverting (AND set, XOR white), opaque black.
        let and = [0b0100_0000, 0, 0b1000_0000, 0];
        let xor = [0, 0, 0xff, 0, 0, 0, 0xff, 0xff, 0xff, 0, 0, 0];
        let c = decode(&Colours::new(24), 1, 9, 2, 2, &and, &xor, 24).unwrap();
        assert_eq!((c.hot_x, c.hot_y), (1, 1));
        assert_eq!(px(&c, 0, 1), [0xff, 0, 0, 0xff]);
        assert_eq!(px(&c, 1, 0), [0, 0, 0, 0xff]);
        // The inverting pixel is black; the transparent one beside it
        // becomes the white outline.
        assert_eq!(px(&c, 0, 0), [0, 0, 0, 0xff]);
        assert_eq!(px(&c, 1, 1), [0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn xor_32bpp_has_alpha() {
        // One row of 1 pixel: BGRA.
        let c = decode(&Colours::new(32), 0, 0, 1, 1, &[0, 0], &[0x10, 0x20, 0x30, 0x80], 32).unwrap();
        assert_eq!(px(&c, 0, 0), [0x30, 0x20, 0x10, 0x80]);
    }

    #[test]
    fn mono_xor_is_not_flipped() {
        // 1x2: top row XOR set, bottom row clear; nothing transparent.
        let c = decode(&Colours::new(8), 0, 0, 1, 2, &[0, 0, 0, 0], &[0x80, 0, 0, 0], 1).unwrap();
        assert_eq!(px(&c, 0, 0), [0xff, 0xff, 0xff, 0xff]);
        assert_eq!(px(&c, 0, 1), [0, 0, 0, 0xff]);
    }

    #[test]
    fn rows_are_padded_to_16_bits() {
        // 3 pixels at 24 bpp is 9 bytes, padded to 10.
        let mut xor = vec![0u8; 20];
        xor[10] = 0xff; // first pixel of the second (top) row: blue
        let c = decode(&Colours::new(24), 0, 0, 3, 2, &[], &xor, 24).unwrap();
        assert_eq!(px(&c, 0, 0), [0, 0, 0xff, 0xff]);
    }

    #[test]
    fn bad_pointers_are_refused() {
        let cl = Colours::new(24);
        assert!(decode(&cl, 0, 0, 32, 32, &[0; 128], &[0; 100], 24).is_none());
        assert!(decode(&cl, 0, 0, 0, 32, &[], &[], 24).is_none());
        assert!(decode(&cl, 0, 0, 1, 1, &[0; 2], &[0; 4], 4).is_none());
        assert!(decode(&cl, 0, 0, 4096, 4096, &[], &vec![0; 4096 * 512], 1).is_none());
    }
}
