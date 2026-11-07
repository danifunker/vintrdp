//! Bitmap decompression: RDP's interleaved RLE (8, 15, 16 and 24 bpp) and
//! RDP 6 planar (32 bpp). Port of rdesktop's bitmap.c.
//!
//! The wire sends rows bottom-up. Like bitmap.c, the decoders write the first
//! row they decode into the last row of the output, so the result comes out
//! top row first with no separate flip.

use crate::stream::Reader;

/// Decompress `input` into a `width` x `height` bitmap of `bpp` bits per
/// pixel (8, 15, 16, 24 or 32), in the server's own pixel format, rows top
/// first (the wire's bottom-up order already flipped), each row `width *
/// bytes_per_pixel` bytes with no padding. `None` if the data is corrupt.
///
/// 15 and 16 bpp are two little-endian bytes a pixel, 24 bpp three, and
/// 32 bpp is B, G, R, A. Rows the data doesn't reach are left zero, as
/// rdesktop leaves them untouched.
pub fn decompress(input: &[u8], width: usize, height: usize, bpp: u32) -> Option<Vec<u8>> {
    match bpp {
        8 => interleaved(input, width, height, 1),
        15 | 16 => interleaved(input, width, height, 2),
        24 => interleaved(input, width, height, 3),
        32 => planar(input, width, height),
        _ => None,
    }
}

/// A zeroed output buffer, refusing sizes that overflow or can't be had
/// rather than aborting.
fn alloc(width: usize, height: usize, bytes_pp: usize) -> Option<Vec<u8>> {
    let size = width.checked_mul(height)?.checked_mul(bytes_pp)?;
    let mut out = Vec::new();
    out.try_reserve_exact(size).ok()?;
    out.resize(size, 0);
    Some(out)
}

/// The order codes, after rdesktop folds the SetMix and FillOrMix variants.
const FILL: u32 = 0;
const MIX: u32 = 1;
const FILL_OR_MIX: u32 = 2;
const COLOUR: u32 = 3;
const COPY: u32 = 4;
const BICOLOUR: u32 = 8;
const WHITE: u32 = 0xd;
const BLACK: u32 = 0xe;

/// bitmap_decompress1/2/3 in one: pixels are handled as `bpp` bytes, which
/// gives the same result as the C's per-depth integer code since every
/// operation (copy, XOR with mix) is bytewise.
fn interleaved(input: &[u8], width: usize, height: usize, bpp: usize) -> Option<Vec<u8>> {
    if width == 0 {
        // Any pixel would loop forever in the C; only empty data is valid.
        return if input.is_empty() { Some(Vec::new()) } else { None };
    }
    let mut out = alloc(width, height, bpp)?;
    let stride = width * bpp;
    let mut r = Reader::new(input);

    // Byte offsets of the current and previous rows.
    let mut line: Option<usize> = None;
    let mut prevline: Option<usize> = None;
    let mut rows_left = height;
    let mut x = width;

    let mut lastopcode: Option<u32> = None;
    let mut insertmix = false;
    let mut bicolour = false;
    let mut colour1 = [0u8; 3];
    let mut colour2 = [0u8; 3];
    let mut mix = [0xffu8; 3];
    let mut mask = 0u8;

    let pixel = |r: &mut Reader| -> Option<[u8; 3]> {
        let mut p = [0u8; 3];
        p[..bpp].copy_from_slice(r.bytes(bpp).ok()?);
        Some(p)
    };

    while !r.is_empty() {
        let mut fom_mask = 0u8;
        let code = r.u8().ok()?;
        let (mut opcode, mut count, offset) = match code >> 4 {
            0xc..=0xe => (u32::from(code >> 4) - 6, u32::from(code & 0xf), 16),
            0xf => {
                let op = u32::from(code & 0xf);
                let count = if op < 9 {
                    u32::from(r.u16().ok()?)
                } else if op < 0xb {
                    8
                } else {
                    1
                };
                (op, count, 0)
            }
            _ => (u32::from(code >> 5), u32::from(code & 0x1f), 32),
        };

        // Short forms: a zero count means the count follows; fill-or-mix
        // counts are in units of eight pixels.
        if offset != 0 {
            let isfillormix = opcode == 2 || opcode == 7;
            if count == 0 {
                let n = u32::from(r.u8().ok()?);
                count = if isfillormix { n + 1 } else { n + offset };
            } else if isfillormix {
                count <<= 3;
            }
        }

        match opcode {
            FILL => {
                // Two fills in a row: the second starts with one mix pixel,
                // except at the very start of the bitmap.
                if lastopcode == Some(FILL) && !(x == width && prevline.is_none()) {
                    insertmix = true;
                }
            }
            BICOLOUR => {
                colour1 = pixel(&mut r)?;
                colour2 = pixel(&mut r)?;
            }
            COLOUR => colour2 = pixel(&mut r)?,
            6 | 7 => {
                mix = pixel(&mut r)?;
                opcode -= 5;
            }
            9 => {
                mask = 0x03;
                opcode = FILL_OR_MIX;
                fom_mask = 3;
            }
            0xa => {
                mask = 0x05;
                opcode = FILL_OR_MIX;
                fom_mask = 5;
            }
            _ => {}
        }
        lastopcode = Some(opcode);
        let mut mixmask = 0u8;

        while count > 0 {
            if x >= width {
                if rows_left == 0 {
                    return None;
                }
                x = 0;
                rows_left -= 1;
                prevline = line;
                line = Some(rows_left * stride);
            }
            let l = line?;
            // `x < width` holds for every write below, so all indexing is
            // within `out`.
            let at = |x: usize| l + x * bpp;
            let prev_at = |x: usize| prevline.map(|p| p + x * bpp);
            match opcode {
                FILL => {
                    if insertmix {
                        put_mixed(&mut out, at(x), prev_at(x), &mix[..bpp]);
                        insertmix = false;
                        count -= 1;
                        x += 1;
                    }
                    while count > 0 && x < width {
                        put_prev(&mut out, at(x), prev_at(x), bpp);
                        count -= 1;
                        x += 1;
                    }
                }
                MIX => {
                    while count > 0 && x < width {
                        put_mixed(&mut out, at(x), prev_at(x), &mix[..bpp]);
                        count -= 1;
                        x += 1;
                    }
                }
                FILL_OR_MIX => {
                    while count > 0 && x < width {
                        mixmask <<= 1;
                        if mixmask == 0 {
                            mask = if fom_mask != 0 { fom_mask } else { r.u8().ok()? };
                            mixmask = 1;
                        }
                        if mask & mixmask != 0 {
                            put_mixed(&mut out, at(x), prev_at(x), &mix[..bpp]);
                        } else {
                            put_prev(&mut out, at(x), prev_at(x), bpp);
                        }
                        count -= 1;
                        x += 1;
                    }
                }
                COLOUR => {
                    while count > 0 && x < width {
                        put(&mut out, at(x), &colour2[..bpp]);
                        count -= 1;
                        x += 1;
                    }
                }
                COPY => {
                    while count > 0 && x < width {
                        let p = r.bytes(bpp).ok()?;
                        put(&mut out, at(x), p);
                        count -= 1;
                        x += 1;
                    }
                }
                BICOLOUR => {
                    // The count is in pairs: the first of each pair doesn't
                    // use it up.
                    while count > 0 && x < width {
                        if bicolour {
                            put(&mut out, at(x), &colour2[..bpp]);
                            bicolour = false;
                        } else {
                            put(&mut out, at(x), &colour1[..bpp]);
                            bicolour = true;
                            count += 1;
                        }
                        count -= 1;
                        x += 1;
                    }
                }
                WHITE => {
                    while count > 0 && x < width {
                        put(&mut out, at(x), &[0xff; 3][..bpp]);
                        count -= 1;
                        x += 1;
                    }
                }
                BLACK => {
                    while count > 0 && x < width {
                        put(&mut out, at(x), &[0; 3][..bpp]);
                        count -= 1;
                        x += 1;
                    }
                }
                _ => return None,
            }
        }
    }
    Some(out)
}

fn put(out: &mut [u8], at: usize, px: &[u8]) {
    out[at..at + px.len()].copy_from_slice(px);
}

/// The pixel above, or black on the first row.
fn put_prev(out: &mut [u8], at: usize, prev: Option<usize>, bpp: usize) {
    match prev {
        Some(p) => out.copy_within(p..p + bpp, at),
        None => out[at..at + bpp].fill(0),
    }
}

/// The pixel above XOR the mix colour, or the mix colour on the first row.
fn put_mixed(out: &mut [u8], at: usize, prev: Option<usize>, mix: &[u8]) {
    for (i, &m) in mix.iter().enumerate() {
        let above = prev.map_or(0, |p| out[p + i]);
        out[at + i] = above ^ m;
    }
}

/// Planar format header bits (MS-RDPEGDI 2.2.2.5.1).
const PLANAR_CLL_MASK: u8 = 0x07;
const PLANAR_CS: u8 = 0x08;
const PLANAR_RLE: u8 = 0x10;
const PLANAR_NA: u8 = 0x20;

/// bitmap_decompress4. rdesktop accepts only header 0x10 (RLE with alpha);
/// this also takes the no-alpha flag and raw planes, which servers may send
/// for 32 bpp. Colour loss and chroma subsampling are refused as before.
fn planar(input: &[u8], width: usize, height: usize) -> Option<Vec<u8>> {
    let mut out = alloc(width, height, 4)?;
    let mut r = Reader::new(input);
    let header = r.u8().ok()?;
    if header & (PLANAR_CLL_MASK | PLANAR_CS) != 0 {
        return None;
    }
    let alpha = header & PLANAR_NA == 0;
    if !alpha {
        for px in out.chunks_exact_mut(4) {
            px[3] = 0xff;
        }
    }
    // Planes come A (if present), R, G, B; the output is B, G, R, A.
    let planes: &[usize] = if alpha { &[3, 2, 1, 0] } else { &[2, 1, 0] };

    if header & PLANAR_RLE != 0 {
        for &plane in planes {
            rle_plane(&mut r, &mut out, width, height, plane)?;
        }
        // As in rdesktop, the planes must use up the data exactly.
        if !r.is_empty() {
            return None;
        }
    } else {
        for &plane in planes {
            let src = r.bytes(width.checked_mul(height)?).ok()?;
            for (i, row) in src.chunks_exact(width.max(1)).enumerate() {
                let start = (height - 1 - i) * width * 4;
                for (x, &v) in row.iter().enumerate() {
                    out[start + x * 4 + plane] = v;
                }
            }
        }
        // Raw planes are followed by one pad byte.
        if r.remaining() > 1 {
            return None;
        }
    }
    Some(out)
}

/// process_plane: one RLE colour plane into byte `plane` of each output
/// pixel. The first row decoded is absolute, the rest are deltas from the
/// row decoded before them (the one below on screen).
fn rle_plane(r: &mut Reader, out: &mut [u8], width: usize, height: usize, plane: usize) -> Option<()> {
    let stride = width * 4;
    for indexh in 0..height {
        let this = (height - 1 - indexh) * stride + plane;
        let last = if indexh == 0 { None } else { Some(this + stride) };
        let mut colour = 0u8;
        let mut indexw = 0;
        while indexw < width {
            let code = r.u8().ok()?;
            let mut replen = usize::from(code & 0xf);
            let mut collen = usize::from(code >> 4);
            // A run length of 1 or 2 makes the raw count part of a long run.
            let revcode = (replen << 4) | collen;
            if (16..=47).contains(&revcode) {
                replen = revcode;
                collen = 0;
            }
            while indexw < width && collen > 0 {
                let v = r.u8().ok()?;
                colour = match last {
                    None => v,
                    // Sign and magnitude, the sign in the low bit.
                    Some(_) if v & 1 != 0 => ((v >> 1) + 1).wrapping_neg(),
                    Some(_) => v >> 1,
                };
                out[this + indexw * 4] = pixel_value(out, last, indexw, colour);
                indexw += 1;
                collen -= 1;
            }
            while indexw < width && replen > 0 {
                out[this + indexw * 4] = pixel_value(out, last, indexw, colour);
                indexw += 1;
                replen -= 1;
            }
        }
    }
    Some(())
}

fn pixel_value(out: &[u8], last: Option<usize>, x: usize, colour: u8) -> u8 {
    match last {
        None => colour,
        Some(l) => out[l + x * 4].wrapping_add(colour),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rle_8bpp_copy_mix_fill_or_mix() {
        let data = [
            0x84, 1, 2, 3, 4, // copy 4: bottom row
            0x24, // mix 4 with the default 0xff
            0x40, 3, 0b0101, // fill-or-mix, count 3+1, mask
        ];
        let out = decompress(&data, 4, 3, 8).unwrap();
        assert_eq!(
            out,
            [
                1, 0xfd, 3, 0xfb, // top: from fill-or-mix
                0xfe, 0xfd, 0xfc, 0xfb, // middle: mix
                1, 2, 3, 4, // bottom: copy
            ]
        );
    }

    #[test]
    fn rle_8bpp_fill_inserts_mix() {
        // Fill 2 then fill 2 again: the second fill's first pixel is mixed.
        let data = [0x84, 1, 2, 3, 4, 0x02, 0x02];
        let out = decompress(&data, 4, 2, 8).unwrap();
        assert_eq!(out, [1, 2, 0xfc, 4, 1, 2, 3, 4]);
    }

    #[test]
    fn rle_8bpp_first_row_fill_is_black_and_long_counts() {
        let data = [
            0x00, 0, // fill, count 0 + 32: four black rows
            0xf3, 8, 0, 0x55, // colour, 16-bit count 8
            0x60, 0, 0x66, // colour, count 0 + 32: covers more than left
        ];
        // 32 + 8 = 40 pixels fill five rows; the final 32 overruns.
        assert!(decompress(&data, 8, 6, 8).is_none());
        let out = decompress(&data[..6], 8, 5, 8).unwrap();
        assert_eq!(&out[..8], &[0x55; 8]);
        assert!(out[8..].iter().all(|&b| b == 0));
    }

    #[test]
    fn rle_8bpp_fill_or_mix_fixed_masks() {
        // FillOrMix_2 (mask 0b101) on the first row, then a SetMix/FillOrMix.
        let data = [0xfa, 0xd1, 0x0f, 0x80];
        let out = decompress(&data, 8, 2, 8).unwrap();
        assert_eq!(&out[8..], &[0xff, 0, 0xff, 0, 0, 0, 0, 0]);
        assert_eq!(&out[..8], &[0xff, 0, 0xff, 0, 0, 0, 0, 0x0f]);
    }

    #[test]
    fn rle_16bpp_bicolour_white_black_setmix_colour() {
        let data = [
            0xe2, 0x34, 0x12, 0x78, 0x56, // bicolour, 2 pairs
            0xfd, // white
            0xfe, // black
            0xc1, 0xff, 0x00, // set mix 0x00ff, mix 1
            0x61, 0xcd, 0xab, // colour 1
        ];
        for bpp in [15, 16] {
            let out = decompress(&data, 4, 2, bpp).unwrap();
            assert_eq!(
                out,
                [
                    0xff, 0xff, 0, 0, 0xcb, 0x12, 0xcd, 0xab, //
                    0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0x78, 0x56,
                ]
            );
        }
    }

    #[test]
    fn rle_24bpp_fill_or_mix_and_copy() {
        let data = [
            0xf9, // FillOrMix_1 (mask 0b11), 8 pixels
            0xd1, 1, 2, 3, 0x80, // set mix, fill-or-mix 8, mask
            0x81, 9, 8, 7, // copy 1 on a third row
        ];
        let out = decompress(&data, 8, 3, 24).unwrap();
        let mut bottom = vec![0xff; 6];
        bottom.extend([0; 18]);
        let mut middle = bottom.clone();
        middle[21..24].copy_from_slice(&[1, 2, 3]);
        let mut top = vec![9, 8, 7];
        top.extend([0; 21]);
        assert_eq!(out, [top, middle, bottom].concat());
    }

    #[test]
    fn rle_rejects_bad_input() {
        assert!(decompress(&[0x84, 1, 2], 4, 1, 8).is_none()); // truncated copy
        assert!(decompress(&[0xa1], 4, 1, 8).is_none()); // opcode 5
        assert!(decompress(&[0xfb], 4, 1, 8).is_none()); // opcode 0xb
        assert!(decompress(&[0x64, 0], 4, 1, 16).is_none()); // short colour
        assert!(decompress(&[0x65, 0], 4, 1, 8).is_none()); // past the last row
        assert!(decompress(&[0x40], 4, 1, 8).is_none()); // missing count
        assert!(decompress(&[0x41], 4, 1, 8).is_none()); // missing mask
        assert!(decompress(&[], 4, 1, 12).is_none()); // bad depth
        assert!(decompress(&[0x61, 0], usize::MAX, 2, 8).is_none()); // overflow
        assert_eq!(decompress(&[], 0, 5, 8), Some(vec![]));
        assert!(decompress(&[0x61, 0], 0, 5, 8).is_none());
    }

    #[test]
    fn planar_rle() {
        let data = [
            0x10, // RLE, with alpha
            0x30, 0xff, 0xff, 0xff, 0x03, // A: 3 raw, then a zero-delta run
            0x13, 0x10, 0x30, 0x02, 0x01, 0x00, // R: raw + run; deltas +1 -1 0
            0x01, 0x03, // G: long run (16) of 0, cut at the width; delta run
            0x30, 1, 2, 3, 0x13, 0x04, // B: raw; delta +2 then repeated
        ];
        let out = decompress(&data, 3, 2, 32).unwrap();
        assert_eq!(
            out,
            [
                3, 0, 0x11, 0xff, 4, 0, 0x0f, 0xff, 5, 0, 0x10, 0xff, // top
                1, 0, 0x10, 0xff, 2, 0, 0x10, 0xff, 3, 0, 0x10, 0xff, // bottom
            ]
        );
        // Leftover or missing bytes are an error.
        assert!(decompress(&[&data[..], &[0]].concat(), 3, 2, 32).is_none());
        assert!(decompress(&data[..data.len() - 1], 3, 2, 32).is_none());
    }

    #[test]
    fn planar_raw_no_alpha() {
        let data = [
            0x20, // no alpha, no RLE
            1, 2, 3, 4, // R, bottom row first
            5, 6, 7, 8, // G
            9, 10, 11, 12, // B
            0,    // pad
        ];
        let out = decompress(&data, 2, 2, 32).unwrap();
        assert_eq!(out, [11, 7, 3, 0xff, 12, 8, 4, 0xff, 9, 5, 1, 0xff, 10, 6, 2, 0xff]);
        assert!(decompress(&data[..13], 2, 2, 32).is_some()); // pad optional
        assert!(decompress(&data[..12], 2, 2, 32).is_none());
    }

    #[test]
    fn planar_rejects_unsupported() {
        assert!(decompress(&[], 1, 1, 32).is_none());
        assert!(decompress(&[0x18, 0x10, 0, 0x10, 0, 0x10, 0, 0x10, 0], 1, 1, 32).is_none());
        assert!(decompress(&[0x11], 1, 1, 32).is_none());
    }

    #[test]
    fn garbage_never_panics() {
        let mut seed = 0x1234_5678u32;
        for _ in 0..20000 {
            let len = (lcg(&mut seed) % 64) as usize;
            let data: Vec<u8> = (0..len).map(|_| lcg(&mut seed) as u8).collect();
            let w = (lcg(&mut seed) % 9) as usize;
            let h = (lcg(&mut seed) % 9) as usize;
            for bpp in [8, 16, 24, 32] {
                if let Some(out) = decompress(&data, w, h, bpp) {
                    assert_eq!(out.len(), w * h * (bpp as usize).div_ceil(8));
                }
            }
        }
    }

    fn lcg(s: &mut u32) -> u32 {
        *s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
        *s >> 8
    }
}
