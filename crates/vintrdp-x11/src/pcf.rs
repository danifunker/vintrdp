//! PCF, the X.org bitmap font format (what bdftopcf writes), read as X.org's
//! libXfont reads it: its metrics, ink metrics, bitmaps, encodings,
//! accelerators and properties, whatever byte and bit order it was written in.

use crate::font::{Font, Glyph, Metrics, PropValue};

const PROPERTIES: u32 = 1 << 0;
const ACCELERATORS: u32 = 1 << 1;
const METRICS: u32 = 1 << 2;
const BITMAPS: u32 = 1 << 3;
const INK_METRICS: u32 = 1 << 4;
const BDF_ENCODINGS: u32 = 1 << 5;
const BDF_ACCELERATORS: u32 = 1 << 8;

/// Format bits.
const COMPRESSED_METRICS: u32 = 0x100;
const ACCEL_W_INKBOUNDS: u32 = 0x100;
const BYTE_MSB: u32 = 1 << 2;
const BIT_MSB: u32 = 1 << 3;

/// A gzip-compressed PCF file (`.pcf.gz`), or a plain one.
pub fn load(data: &[u8]) -> Result<Font, String> {
    if data.starts_with(&[0x1f, 0x8b]) {
        parse(&gunzip(data)?)
    } else {
        parse(data)
    }
}

/// gzip's members, inflated (RFC 1952).
fn gunzip(data: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "not a gzip file".to_string();
    if data.len() < 18 || data[2] != 8 {
        return Err(bad());
    }
    let flags = data[3];
    let mut at = 10;
    if flags & 4 != 0 {
        let n = u16::from_le_bytes([
            *data.get(at).ok_or_else(bad)?,
            *data.get(at + 1).ok_or_else(bad)?,
        ]) as usize;
        at += 2 + n;
    }
    for bit in [8, 16] {
        if flags & bit != 0 {
            at += data
                .get(at..)
                .ok_or_else(bad)?
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(bad)?
                + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    miniz_oxide::inflate::decompress_to_vec(data.get(at..).ok_or_else(bad)?)
        .map_err(|e| format!("gzip: {e:?}"))
}

struct Table<'a> {
    format: u32,
    data: &'a [u8],
    at: usize,
}

impl<'a> Table<'a> {
    fn msb(&self) -> bool {
        self.format & BYTE_MSB != 0
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self
            .data
            .get(self.at..self.at + n)
            .ok_or("a PCF table is cut short")?;
        self.at += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(if self.msb() {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        })
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        let b = [b[0], b[1], b[2], b[3]];
        Ok(if self.msb() {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }

    fn metric(&mut self, compressed: bool) -> Result<Metrics, String> {
        if compressed {
            let mut c = || -> Result<i16, String> { Ok(self.u8()? as i16 - 0x80) };
            Ok(Metrics {
                lb: c()?,
                rb: c()?,
                width: c()?,
                ascent: c()?,
                descent: c()?,
                attributes: 0,
            })
        } else {
            let mut s = || -> Result<i16, String> { Ok(self.u16()? as i16) };
            Ok(Metrics {
                lb: s()?,
                rb: s()?,
                width: s()?,
                ascent: s()?,
                descent: s()?,
                attributes: self.u16()?,
            })
        }
    }

    fn metrics(&mut self) -> Result<Vec<Metrics>, String> {
        let compressed = self.format & COMPRESSED_METRICS != 0;
        let n = if compressed {
            self.u16()? as u32
        } else {
            self.u32()?
        };
        (0..n).map(|_| self.metric(compressed)).collect()
    }
}

fn parse(data: &[u8]) -> Result<Font, String> {
    if !data.starts_with(b"\x01fcp") || data.len() < 8 {
        return Err("not a PCF font".into());
    }
    let le = |at: usize| {
        data.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let count = le(4).unwrap() as usize;
    let mut tables = Vec::new();
    for i in 0..count {
        let e = 8 + i * 16;
        let (kind, size, offset) = (
            le(e).ok_or("bad PCF table of contents")?,
            le(e + 8).ok_or("bad PCF")?,
            le(e + 12).ok_or("bad PCF")?,
        );
        // bdftopcf's sizes may run past the end; libXfont reads what it needs.
        let end = (offset as usize)
            .saturating_add(size as usize)
            .min(data.len());
        let body = data
            .get(offset as usize..end)
            .ok_or("a PCF table lies outside the file")?;
        tables.push((kind, body));
    }
    let table = |kind: u32| -> Result<Option<Table>, String> {
        let Some(&(_, body)) = tables.iter().find(|t| t.0 == kind) else {
            return Ok(None);
        };
        let format = u32::from_le_bytes(
            body.get(..4)
                .ok_or("an empty PCF table")?
                .try_into()
                .unwrap(),
        );
        Ok(Some(Table {
            format,
            data: body,
            at: 4,
        }))
    };

    // Properties.
    let mut properties = Vec::new();
    if let Some(mut t) = table(PROPERTIES)? {
        let n = t.u32()? as usize;
        let mut raw = Vec::with_capacity(n);
        for _ in 0..n {
            raw.push((t.u32()?, t.u8()? != 0, t.u32()?));
        }
        if n & 3 != 0 {
            t.take(4 - (n & 3))?;
        }
        let size = t.u32()? as usize;
        let strings = t.take(size)?;
        let string = |at: u32| -> String {
            let s = strings.get(at as usize..).unwrap_or(&[]);
            s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())]
                .iter()
                .map(|&b| b as char)
                .collect()
        };
        for (name, is_string, value) in raw {
            let v = if is_string {
                PropValue::Atom(string(value))
            } else {
                PropValue::Int(value as i32)
            };
            properties.push((string(name), v));
        }
    }

    let metrics = table(METRICS)?
        .ok_or("a PCF font with no metrics")?
        .metrics()?;
    let ink = match table(INK_METRICS)? {
        Some(mut t) => Some(t.metrics()?),
        None => None,
    };

    // Bitmaps: rows padded to the glyph pad, in the table's bit and byte order.
    let mut bt = table(BITMAPS)?.ok_or("a PCF font with no bitmaps")?;
    let n = bt.u32()? as usize;
    if n != metrics.len() {
        return Err("PCF bitmaps and metrics differ in number".into());
    }
    let offsets: Vec<u32> = (0..n).map(|_| bt.u32()).collect::<Result<_, _>>()?;
    let mut sizes = [0u32; 4];
    for s in &mut sizes {
        *s = bt.u32()?;
    }
    let pad = 1usize << (bt.format & 3);
    let unit = 1usize << (bt.format >> 4 & 3);
    let bits = bt.take(sizes[bt.format as usize & 3] as usize)?;
    let (bit_msb, byte_msb) = (bt.format & BIT_MSB != 0, bt.format & BYTE_MSB != 0);
    let mut glyphs = Vec::with_capacity(n);
    for (i, m) in metrics.iter().enumerate() {
        let (w, h) = (
            (m.rb - m.lb).max(0) as usize,
            (m.ascent + m.descent).max(0) as usize,
        );
        let stride = (w + 7) / 8;
        let padded = stride.div_ceil(pad) * pad;
        let words = w.div_ceil(64).max(1);
        let mut rows = vec![0u64; words * h];
        let start = offsets[i] as usize;
        for y in 0..h {
            let Some(src) = bits.get(start + y * padded..start + (y + 1) * padded) else {
                break;
            };
            let mut row = src.to_vec();
            // Bytes in bit order, as libXfont makes them.
            if bit_msb != byte_msb && unit > 1 {
                for c in row.chunks_mut(unit) {
                    c.reverse();
                }
            }
            for x in 0..w {
                let b = row[x / 8];
                let on = if bit_msb {
                    b >> (7 - x % 8) & 1
                } else {
                    b >> (x % 8) & 1
                };
                if on != 0 {
                    rows[y * words + x / 64] |= 1 << (x % 64);
                }
            }
        }
        let ink = match &ink {
            Some(ink) => *ink
                .get(i)
                .ok_or("PCF ink metrics and metrics differ in number")?,
            None => *m,
        };
        glyphs.push(Some(Glyph {
            m: *m,
            ink,
            words,
            rows,
        }));
    }

    // Encodings: which glyph each character is.
    let mut et = table(BDF_ENCODINGS)?.ok_or("a PCF font with no encodings")?;
    let (min2, max2, min1, max1, default_char) =
        (et.u16()?, et.u16()?, et.u16()?, et.u16()?, et.u16()?);
    if min2 > max2 || min1 > max1 || max1 > 255 || max2 > 255 {
        return Err("bad PCF encodings".into());
    }
    let cells = (max2 - min2 + 1) as usize * (max1 - min1 + 1) as usize;
    let mut by_char = Vec::with_capacity(cells);
    let mut all_exist = true;
    let mut taken = vec![false; glyphs.len()];
    for _ in 0..cells {
        let g = et.u16()?;
        if g == 0xffff || g as usize >= glyphs.len() {
            all_exist = false;
            by_char.push(None);
        } else {
            // A glyph used twice is copied the second time.
            let glyph = if taken[g as usize] {
                let o = glyphs[g as usize].as_ref().unwrap();
                Some(Glyph {
                    m: o.m,
                    ink: o.ink,
                    words: o.words,
                    rows: o.rows.clone(),
                })
            } else {
                taken[g as usize] = true;
                None
            };
            by_char.push(Some((g, glyph)));
        }
    }
    let by_char: Vec<Option<Glyph>> = by_char
        .into_iter()
        .map(|c| c.map(|(g, copy)| copy.unwrap_or_else(|| glyphs[g as usize].take().unwrap())))
        .collect();

    // Accelerators: the font's ascent and descent, and the ink bounds that
    // QueryFont reports.
    let mut at = match table(BDF_ACCELERATORS)? {
        Some(t) => t,
        None => table(ACCELERATORS)?.ok_or("a PCF font with no accelerators")?,
    };
    at.take(8)?;
    let (ascent, descent) = (at.u32()? as i32 as i16, at.u32()? as i32 as i16);
    at.u32()?; // max overlap
    let (min_bounds, max_bounds) = (at.metric(false)?, at.metric(false)?);
    let (min_bounds, max_bounds) = if at.format & ACCEL_W_INKBOUNDS != 0 {
        (at.metric(false)?, at.metric(false)?)
    } else {
        (min_bounds, max_bounds)
    };

    Ok(Font {
        glyphs: by_char,
        min_char: min2,
        max_char: max2,
        min_byte1: min1 as u8,
        max_byte1: max1 as u8,
        default_char,
        ascent,
        descent,
        min_bounds,
        max_bounds,
        all_exist,
        properties,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_bundled() {
        let f = super::load(include_bytes!("../data/fonts/misc/6x13-ISO8859-1.pcf.gz")).unwrap();
        assert_eq!(
            (
                f.ascent,
                f.descent,
                f.min_char,
                f.max_char,
                f.min_byte1,
                f.max_byte1
            ),
            (11, 2, 0, 255, 0, 0)
        );
        let a = f.glyph(b'A' as u16).unwrap();
        assert_eq!(a.m.width, 6);
        assert!(a.rows.iter().any(|&r| r != 0));
        assert!(f.properties.iter().any(|p| p.0 == "FONT"));
    }
}
