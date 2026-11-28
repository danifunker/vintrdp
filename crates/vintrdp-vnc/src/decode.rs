//! The picture: the pixel format we ask for, and the rectangle encodings.
//!
//! Raw, CopyRect, RRE, CoRRE, Hextile, Zlib, Tight (with JPEG), TRLE and ZRLE;
//! and the pseudo-encodings for the cursor shape (rich and X), desktop size
//! (plain and extended), the desktop's name, LastRect, and QEMU's scancode
//! key events. Pixels arrive in the format we set (`Pf::ours`) and are drawn
//! as `0x00RRGGBB`.

use std::io;

use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

use crate::Cursor as CursorImage;
use crate::stream::{Cursor, Src, bad};

pub(crate) const RAW: i32 = 0;
pub(crate) const COPYRECT: i32 = 1;
pub(crate) const RRE: i32 = 2;
pub(crate) const CORRE: i32 = 4;
pub(crate) const HEXTILE: i32 = 5;
pub(crate) const ZLIB: i32 = 6;
pub(crate) const TIGHT: i32 = 7;
pub(crate) const TRLE: i32 = 15;
pub(crate) const ZRLE: i32 = 16;
pub(crate) const CURSOR: i32 = -239;
pub(crate) const XCURSOR: i32 = -240;
pub(crate) const DESKTOP_SIZE: i32 = -223;
pub(crate) const LAST_RECT: i32 = -224;
pub(crate) const QEMU_KEY_EVENT: i32 = -258;
pub(crate) const DESKTOP_NAME: i32 = -307;
pub(crate) const EXTENDED_DESKTOP_SIZE: i32 = -308;
pub(crate) const FENCE: i32 = -312;
pub(crate) const CONTINUOUS_UPDATES: i32 = -313;
/// Tight's JPEG quality, 0 to 9, and zlib level, 0 to 9.
pub(crate) const fn jpeg_quality(q: i32) -> i32 {
    -32 + q
}
pub(crate) const fn compress_level(l: i32) -> i32 {
    -256 + l
}

/// A pixel format (RFB's PIXEL_FORMAT). We always ask for true colour,
/// little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pf {
    pub bpp: u8,
    pub depth: u8,
    pub max: [u16; 3],
    pub shift: [u8; 3],
}

impl Pf {
    /// Ours for a colour depth (`-a`): 8 is BGR233, 15 and 16 are RGB565, more
    /// is 32 bits holding 0x00RRGGBB.
    pub fn ours(depth: u32) -> Pf {
        match depth {
            8 => Pf { bpp: 8, depth: 8, max: [7, 7, 3], shift: [0, 3, 6] },
            15 | 16 => Pf { bpp: 16, depth: 16, max: [31, 63, 31], shift: [11, 5, 0] },
            _ => Pf { bpp: 32, depth: 24, max: [255, 255, 255], shift: [16, 8, 0] },
        }
    }

    /// The 16-byte PIXEL_FORMAT.
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0] = self.bpp;
        b[1] = self.depth;
        b[2] = 0; // little-endian
        b[3] = 1; // true colour
        for i in 0..3 {
            b[4 + 2 * i..6 + 2 * i].copy_from_slice(&self.max[i].to_be_bytes());
            b[10 + i] = self.shift[i];
        }
        b
    }

    fn bytes(&self) -> usize {
        self.bpp as usize / 8
    }

    fn rgb888(&self) -> bool {
        self.max == [255, 255, 255] && self.shift == [16, 8, 0]
    }

    /// A pixel value as 0x00RRGGBB.
    pub fn rgb(&self, v: u32) -> u32 {
        if self.rgb888() {
            return v & 0xff_ffff;
        }
        let c = |i: usize| {
            let m = self.max[i] as u32;
            ((v >> self.shift[i]) & m) * 255 / m.max(1)
        };
        (c(0) << 16) | (c(1) << 8) | c(2)
    }

    /// A pixel's value from its bytes (little-endian, as we asked).
    fn value(b: &[u8]) -> u32 {
        b.iter().rev().fold(0, |v, &x| (v << 8) | x as u32)
    }

    /// ZRLE's and TRLE's CPIXEL: 3 bytes when 32-bit pixels hold 24 bits.
    fn cpixel_len(&self) -> usize {
        if self.bpp == 32 && self.depth <= 24 { 3 } else { self.bytes() }
    }

    /// Tight's TPIXEL: 3 bytes, R G B, for 32-bit pixels holding 8-bit colours.
    fn tpixel_len(&self) -> usize {
        if self.bpp == 32 && self.depth == 24 && self.max == [255, 255, 255] { 3 } else { self.bytes() }
    }
}

/// Where decoded rectangles go: the session's framebuffer.
pub(crate) trait Canvas {
    fn put(&mut self, x: usize, y: usize, w: usize, h: usize, px: &[u32]);
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, colour: u32) {
        self.put(x, y, w, h, &vec![colour; w * h]);
    }
    fn copy(&mut self, sx: usize, sy: usize, x: usize, y: usize, w: usize, h: usize);
}

/// What a rectangle meant, besides pixels.
pub(crate) enum Pseudo {
    None,
    LastRect,
    Resize(u16, u16),
    Cursor(Option<CursorImage>),
    Name(String),
    QemuKeys,
}

/// A zlib stream that lasts the whole connection, as Zlib, ZRLE and Tight
/// have them.
struct Inflater(Box<InflateState>);

impl Inflater {
    fn new() -> Self {
        Inflater(InflateState::new_boxed(DataFormat::Zlib))
    }

    fn reset(&mut self) {
        *self = Inflater::new();
    }

    /// Everything `input` inflates to (the server flushes at each rectangle's end).
    fn inflate(&mut self, mut input: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(input.len() * 4);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let r = inflate(&mut self.0, input, &mut buf, MZFlush::None);
            input = &input[r.bytes_consumed..];
            out.extend_from_slice(&buf[..r.bytes_written]);
            match r.status {
                Ok(MZStatus::StreamEnd) => break,
                Ok(_) | Err(MZError::Buf) => {}
                Err(e) => return Err(bad(format!("zlib: {e:?}"))),
            }
            if (input.is_empty() && r.bytes_written < buf.len()) || (r.bytes_consumed == 0 && r.bytes_written == 0) {
                break;
            }
        }
        Ok(out)
    }
}

pub(crate) struct Decoder {
    pub pf: Pf,
    zlib: Inflater,
    zrle: Inflater,
    tight: [Inflater; 4],
    /// TRLE's last palette, which later tiles may reuse.
    palette: Vec<u32>,
    /// The encodings seen so far, for -v.
    seen: Vec<i32>,
}

/// Every encoding we decode, best first, and the pseudo-encodings.
/// VINTRDP_VNC_ENCODINGS (say `hextile,raw`) narrows the list, for testing.
pub(crate) fn encodings(pf: &Pf) -> Vec<i32> {
    const NAMES: [(&str, i32); 9] =
        [("tight", TIGHT), ("zrle", ZRLE), ("hextile", HEXTILE), ("trle", TRLE), ("zlib", ZLIB), ("corre", CORRE), ("rre", RRE), ("copyrect", COPYRECT), ("raw", RAW)];
    let mut e: Vec<i32> = match std::env::var("VINTRDP_VNC_ENCODINGS") {
        Ok(list) => list.split(',').filter_map(|n| NAMES.iter().find(|(name, _)| name.eq_ignore_ascii_case(n.trim())).map(|&(_, e)| e)).collect(),
        Err(_) => NAMES.iter().map(|&(_, e)| e).collect(),
    };
    e.extend([CURSOR, XCURSOR, DESKTOP_SIZE, EXTENDED_DESKTOP_SIZE, LAST_RECT, DESKTOP_NAME, QEMU_KEY_EVENT, FENCE, CONTINUOUS_UPDATES]);
    e.push(compress_level(6));
    if pf.tpixel_len() == 3 {
        e.push(jpeg_quality(8));
    }
    e
}

impl Decoder {
    pub fn new(pf: Pf) -> Decoder {
        Decoder {
            pf,
            zlib: Inflater::new(),
            zrle: Inflater::new(),
            tight: [Inflater::new(), Inflater::new(), Inflater::new(), Inflater::new()],
            palette: Vec::new(),
            seen: Vec::new(),
        }
    }

    /// True the first time `encoding` turns up.
    pub fn first_use(&mut self, encoding: i32) -> bool {
        let new = !self.seen.contains(&encoding);
        if new {
            self.seen.push(encoding);
        }
        new
    }

    fn pixel(&self, src: &mut impl Src) -> io::Result<u32> {
        let mut b = [0u8; 4];
        let n = self.pf.bytes();
        src.read_into(&mut b[..n])?;
        Ok(self.pf.rgb(Pf::value(&b[..n])))
    }

    fn pixels(&self, src: &mut impl Src, n: usize) -> io::Result<Vec<u32>> {
        let size = self.pf.bytes();
        let data = src.bytes(n * size)?;
        Ok(data.chunks_exact(size).map(|c| self.pf.rgb(Pf::value(c))).collect())
    }

    fn cpixel(&self, src: &mut impl Src) -> io::Result<u32> {
        let mut b = [0u8; 4];
        let n = self.pf.cpixel_len();
        src.read_into(&mut b[..n])?;
        Ok(self.pf.rgb(Pf::value(&b[..n])))
    }

    /// Tight's TPIXEL as a pixel value (R G B packs as 0x00RRGGBB, which is
    /// our 32-bit format's value).
    fn tpixel_value(&self, b: &[u8]) -> u32 {
        if b.len() == 3 { (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32 } else { Pf::value(b) }
    }

    /// One rectangle of a FramebufferUpdate.
    #[allow(clippy::too_many_arguments)]
    pub fn rect(&mut self, src: &mut impl Src, canvas: &mut impl Canvas, x: u16, y: u16, w: u16, h: u16, encoding: i32) -> io::Result<Pseudo> {
        let (x, y, w, h) = (x as usize, y as usize, w as usize, h as usize);
        match encoding {
            RAW => {
                let px = self.pixels(src, w * h)?;
                canvas.put(x, y, w, h, &px);
            }
            COPYRECT => {
                let (sx, sy) = (src.u16()? as usize, src.u16()? as usize);
                canvas.copy(sx, sy, x, y, w, h);
            }
            RRE | CORRE => {
                let count = src.u32()?;
                if count as usize > w * h {
                    return Err(bad(format!("RRE: {count} subrectangles in {w}x{h}")));
                }
                let mut px = vec![self.pixel(src)?; w * h];
                for _ in 0..count {
                    let c = self.pixel(src)?;
                    let (sx, sy, sw, sh) = if encoding == RRE {
                        (src.u16()? as usize, src.u16()? as usize, src.u16()? as usize, src.u16()? as usize)
                    } else {
                        (src.u8()? as usize, src.u8()? as usize, src.u8()? as usize, src.u8()? as usize)
                    };
                    fill(&mut px, w, h, sx, sy, sw, sh, c);
                }
                canvas.put(x, y, w, h, &px);
            }
            HEXTILE => {
                let px = self.hextile(src, w, h)?;
                canvas.put(x, y, w, h, &px);
            }
            ZLIB => {
                let len = src.u32()? as usize;
                let data = self.zlib.inflate(&src.bytes(len)?)?;
                let px = self.pixels(&mut Cursor { data: &data }, w * h)?;
                canvas.put(x, y, w, h, &px);
            }
            ZRLE => {
                let len = src.u32()? as usize;
                let data = self.zrle.inflate(&src.bytes(len)?)?;
                let px = self.rle(&mut Cursor { data: &data }, w, h, 64, false)?;
                canvas.put(x, y, w, h, &px);
            }
            TRLE => {
                let px = self.rle(src, w, h, 16, true)?;
                canvas.put(x, y, w, h, &px);
            }
            TIGHT => self.tight(src, canvas, x, y, w, h)?,
            CURSOR => return self.rich_cursor(src, x, y, w, h).map(Pseudo::Cursor),
            XCURSOR => return self.x_cursor(src, x, y, w, h).map(Pseudo::Cursor),
            DESKTOP_SIZE => return Ok(Pseudo::Resize(w as u16, h as u16)),
            EXTENDED_DESKTOP_SIZE => {
                let screens = src.u8()?;
                src.skip(3 + screens as usize * 16)?;
                // x is why; y, for an answer to our own request, its status.
                let refused = x == 1 && y != 0;
                return Ok(if refused || w == 0 || h == 0 { Pseudo::None } else { Pseudo::Resize(w as u16, h as u16) });
            }
            DESKTOP_NAME => return src.string().map(Pseudo::Name),
            LAST_RECT => return Ok(Pseudo::LastRect),
            QEMU_KEY_EVENT => return Ok(Pseudo::QemuKeys),
            e => return Err(bad(format!("the server used encoding {e}, which we didn't ask for"))),
        }
        Ok(Pseudo::None)
    }

    fn hextile(&self, src: &mut impl Src, w: usize, h: usize) -> io::Result<Vec<u32>> {
        const RAW: u8 = 1;
        const BACKGROUND: u8 = 2;
        const FOREGROUND: u8 = 4;
        const SUBRECTS: u8 = 8;
        const COLOURED: u8 = 16;
        let mut px = vec![0u32; w * h];
        let (mut bg, mut fg) = (0u32, 0u32);
        for ty in (0..h).step_by(16) {
            for tx in (0..w).step_by(16) {
                let (tw, th) = (16.min(w - tx), 16.min(h - ty));
                let sub = src.u8()?;
                if sub & RAW != 0 {
                    let tile = self.pixels(src, tw * th)?;
                    for row in 0..th {
                        px[(ty + row) * w + tx..][..tw].copy_from_slice(&tile[row * tw..][..tw]);
                    }
                    continue;
                }
                if sub & BACKGROUND != 0 {
                    bg = self.pixel(src)?;
                }
                if sub & FOREGROUND != 0 {
                    fg = self.pixel(src)?;
                }
                for row in 0..th {
                    px[(ty + row) * w + tx..][..tw].fill(bg);
                }
                if sub & SUBRECTS != 0 {
                    for _ in 0..src.u8()? {
                        let c = if sub & COLOURED != 0 { self.pixel(src)? } else { fg };
                        let (xy, wh) = (src.u8()?, src.u8()?);
                        let (sx, sy) = ((xy >> 4) as usize, (xy & 15) as usize);
                        let (sw, sh) = ((wh >> 4) as usize + 1, (wh & 15) as usize + 1);
                        let clip_w = sw.min(tw.saturating_sub(sx));
                        for row in sy..(sy + sh).min(th) {
                            px[(ty + row) * w + tx + sx..][..clip_w].fill(c);
                        }
                    }
                }
            }
        }
        Ok(px)
    }

    /// ZRLE (64-pixel tiles, from a zlib stream) and TRLE (16-pixel tiles,
    /// straight from the network, with palettes reused across tiles).
    fn rle(&mut self, src: &mut impl Src, w: usize, h: usize, tile: usize, trle: bool) -> io::Result<Vec<u32>> {
        let mut px = vec![0u32; w * h];
        for ty in (0..h).step_by(tile) {
            for tx in (0..w).step_by(tile) {
                let (tw, th) = (tile.min(w - tx), tile.min(h - ty));
                let mut out = Vec::with_capacity(tw * th);
                let sub = src.u8()?;
                match sub {
                    0 => {
                        for _ in 0..tw * th {
                            out.push(self.cpixel(src)?);
                        }
                    }
                    1 => out.resize(tw * th, self.cpixel(src)?),
                    2..=16 | 127 => {
                        if sub != 127 {
                            self.palette = (0..sub).map(|_| self.cpixel(src)).collect::<io::Result<_>>()?;
                        } else if !trle || self.palette.is_empty() {
                            return Err(bad("RLE: a reused palette with none to reuse"));
                        }
                        let n = self.palette.len();
                        let bits = match n {
                            2 => 1,
                            3..=4 => 2,
                            _ => 4,
                        };
                        for _ in 0..th {
                            let row = src.bytes((tw * bits).div_ceil(8))?;
                            for i in 0..tw {
                                let bit = i * bits;
                                let index = (row[bit / 8] >> (8 - bits - bit % 8)) as usize & ((1 << bits) - 1);
                                out.push(*self.palette.get(index).ok_or_else(|| bad("RLE: index past the palette"))?);
                            }
                        }
                    }
                    128 => {
                        while out.len() < tw * th {
                            let c = self.cpixel(src)?;
                            let run = run_length(src)?;
                            out.extend(std::iter::repeat_n(c, run.min(tw * th - out.len())));
                        }
                    }
                    129..=255 => {
                        if sub != 129 {
                            self.palette = (0..sub - 128).map(|_| self.cpixel(src)).collect::<io::Result<_>>()?;
                        } else if !trle || self.palette.is_empty() {
                            return Err(bad("RLE: a reused palette with none to reuse"));
                        }
                        while out.len() < tw * th {
                            let i = src.u8()?;
                            let c = *self.palette.get((i & 127) as usize).ok_or_else(|| bad("RLE: index past the palette"))?;
                            let run = if i & 128 != 0 { run_length(src)? } else { 1 };
                            out.extend(std::iter::repeat_n(c, run.min(tw * th - out.len())));
                        }
                    }
                    _ => return Err(bad(format!("RLE: tile type {sub}"))),
                }
                for row in 0..th {
                    px[(ty + row) * w + tx..][..tw].copy_from_slice(&out[row * tw..][..tw]);
                }
            }
        }
        Ok(px)
    }

    #[allow(clippy::too_many_arguments)]
    fn tight(&mut self, src: &mut impl Src, canvas: &mut impl Canvas, x: usize, y: usize, w: usize, h: usize) -> io::Result<()> {
        let control = src.u8()?;
        for (i, stream) in self.tight.iter_mut().enumerate() {
            if control & (1 << i) != 0 {
                stream.reset();
            }
        }
        let tp = self.pf.tpixel_len();
        match control >> 4 {
            8 => {
                let b = src.bytes(tp)?;
                canvas.fill(x, y, w, h, self.pf.rgb(self.tpixel_value(&b)));
            }
            9 => {
                let len = compact_length(src)?;
                let data = src.bytes(len)?;
                let px = jpeg(&data, w, h)?;
                canvas.put(x, y, w, h, &px);
            }
            kind @ 0..=7 => {
                let stream = (kind & 3) as usize;
                let filter = if kind & 4 != 0 { src.u8()? } else { 0 };
                let palette: Option<Vec<u32>> = match filter {
                    0 | 2 => None,
                    1 => {
                        let n = src.u8()? as usize + 1;
                        let raw = src.bytes(n * tp)?;
                        Some(raw.chunks_exact(tp).map(|c| self.pf.rgb(self.tpixel_value(c))).collect())
                    }
                    f => return Err(bad(format!("Tight: filter {f}"))),
                };
                let row_bytes = match &palette {
                    Some(p) if p.len() == 2 => w.div_ceil(8),
                    Some(_) => w,
                    None => w * tp,
                };
                let size = row_bytes * h;
                let data = if size < 12 {
                    src.bytes(size)?
                } else {
                    let len = compact_length(src)?;
                    self.tight[stream].inflate(&src.bytes(len)?)?
                };
                if data.len() < size {
                    return Err(bad("Tight: data ended early"));
                }
                let px: Vec<u32> = match (&palette, filter) {
                    (Some(p), _) if p.len() == 2 => {
                        (0..h).flat_map(|row| (0..w).map(move |i| (row, i))).map(|(row, i)| p[((data[row * row_bytes + i / 8] >> (7 - i % 8)) & 1) as usize]).collect()
                    }
                    (Some(p), _) => data[..size].iter().map(|&i| p.get(i as usize).copied().unwrap_or(0)).collect(),
                    (None, 2) => self.gradient(&data, w, h, tp),
                    (None, _) => data[..size].chunks_exact(tp).map(|c| self.pf.rgb(self.tpixel_value(c))).collect(),
                };
                canvas.put(x, y, w, h, &px);
            }
            other => return Err(bad(format!("Tight: compression type {other}"))),
        }
        Ok(())
    }

    /// Tight's gradient filter: each colour component was sent as its
    /// difference from left + above - above-left, clamped.
    fn gradient(&self, data: &[u8], w: usize, h: usize, tp: usize) -> Vec<u32> {
        let (max, shift): ([u32; 3], [u8; 3]) = if tp == 3 {
            ([255; 3], [16, 8, 0])
        } else {
            (self.pf.max.map(u32::from), self.pf.shift)
        };
        let mut prev = vec![[0i32; 3]; w];
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            let mut this = vec![[0i32; 3]; w];
            for i in 0..w {
                let v = self.tpixel_value(&data[(row * w + i) * tp..][..tp]);
                let mut value = 0u32;
                for c in 0..3 {
                    let left = if i > 0 { this[i - 1][c] } else { 0 };
                    let (up, up_left) = (prev[i][c], if i > 0 { prev[i - 1][c] } else { 0 });
                    let predicted = (left + up - up_left).clamp(0, max[c] as i32);
                    let actual = ((v >> shift[c]) as i32 + predicted) & max[c] as i32;
                    this[i][c] = actual;
                    value |= (actual as u32) << shift[c];
                }
                out.push(if tp == 3 { value } else { self.pf.rgb(value) });
            }
            prev = this;
        }
        out
    }

    fn rich_cursor(&self, src: &mut impl Src, hot_x: usize, hot_y: usize, w: usize, h: usize) -> io::Result<Option<CursorImage>> {
        let px = self.pixels(src, w * h)?;
        let mask = src.bytes(w.div_ceil(8) * h)?;
        Ok(cursor(w, h, hot_x, hot_y, |i, row| (px[row * w + i], bit(&mask, w, i, row))))
    }

    fn x_cursor(&self, src: &mut impl Src, hot_x: usize, hot_y: usize, w: usize, h: usize) -> io::Result<Option<CursorImage>> {
        if w * h == 0 {
            return Ok(None);
        }
        let c = src.bytes(6)?;
        let (fg, bg) = (u32::from_be_bytes([0, c[0], c[1], c[2]]), u32::from_be_bytes([0, c[3], c[4], c[5]]));
        let bitmap = src.bytes(w.div_ceil(8) * h)?;
        let mask = src.bytes(w.div_ceil(8) * h)?;
        Ok(cursor(w, h, hot_x, hot_y, |i, row| (if bit(&bitmap, w, i, row) { fg } else { bg }, bit(&mask, w, i, row))))
    }
}

/// A run length: 1 plus bytes summed until one isn't 255.
fn run_length(src: &mut impl Src) -> io::Result<usize> {
    let mut n = 1usize;
    loop {
        let b = src.u8()?;
        n += b as usize;
        if b != 255 {
            return Ok(n);
        }
        if n > 1 << 24 {
            return Err(bad("RLE: an endless run"));
        }
    }
}

/// Tight's compact length: 7 bits per byte, up to 3 bytes.
fn compact_length(src: &mut impl Src) -> io::Result<usize> {
    let mut n = 0usize;
    for i in 0..3 {
        let b = src.u8()?;
        n |= ((b & if i == 2 { 0xff } else { 0x7f }) as usize) << (7 * i);
        if b & 0x80 == 0 || i == 2 {
            break;
        }
    }
    Ok(n)
}

fn jpeg(data: &[u8], w: usize, h: usize) -> io::Result<Vec<u32>> {
    use jpeg_decoder::{Decoder, PixelFormat};
    let mut d = Decoder::new(data);
    let pixels = d.decode().map_err(|e| bad(format!("Tight JPEG: {e}")))?;
    let info = d.info().ok_or_else(|| bad("Tight JPEG: no image"))?;
    let (jw, jh) = (info.width as usize, info.height as usize);
    let at = |i: usize| -> u32 {
        match info.pixel_format {
            PixelFormat::RGB24 => (pixels[3 * i] as u32) << 16 | (pixels[3 * i + 1] as u32) << 8 | pixels[3 * i + 2] as u32,
            PixelFormat::L8 => pixels[i] as u32 * 0x01_0101,
            PixelFormat::L16 => pixels[2 * i] as u32 * 0x01_0101,
            PixelFormat::CMYK32 => {
                let (c, m, y, k) = (pixels[4 * i] as u32, pixels[4 * i + 1] as u32, pixels[4 * i + 2] as u32, pixels[4 * i + 3] as u32);
                ((255 - c) * (255 - k) / 255) << 16 | ((255 - m) * (255 - k) / 255) << 8 | (255 - y) * (255 - k) / 255
            }
        }
    };
    let mut px = vec![0u32; w * h];
    for row in 0..h.min(jh) {
        for i in 0..w.min(jw) {
            px[row * w + i] = at(row * jw + i);
        }
    }
    Ok(px)
}

/// Fill a sub-rectangle of a w x h buffer, clipped to it.
#[allow(clippy::too_many_arguments)]
fn fill(px: &mut [u32], w: usize, h: usize, x: usize, y: usize, cw: usize, ch: usize, c: u32) {
    if x >= w || y >= h {
        return;
    }
    let cw = cw.min(w - x);
    for row in y..(y + ch).min(h) {
        px[row * w + x..][..cw].fill(c);
    }
}

/// A bit of a 1-bit-per-pixel mask, rows padded to bytes, leftmost first.
fn bit(mask: &[u8], w: usize, i: usize, row: usize) -> bool {
    mask[row * w.div_ceil(8) + i / 8] & (0x80 >> (i % 8)) != 0
}

fn cursor(w: usize, h: usize, hot_x: usize, hot_y: usize, at: impl Fn(usize, usize) -> (u32, bool)) -> Option<CursorImage> {
    if w * h == 0 {
        return None;
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        for i in 0..w {
            let (c, opaque) = at(i, row);
            rgba.extend([(c >> 16) as u8, (c >> 8) as u8, c as u8, if opaque { 255 } else { 0 }]);
        }
    }
    Some(CursorImage { width: w, height: h, hot_x: hot_x.min(w - 1), hot_y: hot_y.min(h - 1), rgba })
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniz_oxide::deflate::core::{CompressorOxide, TDEFLFlush, compress, create_comp_flags_from_zip_params};

    /// A w x h picture.
    struct Pic {
        w: usize,
        px: Vec<u32>,
    }

    impl Canvas for Pic {
        fn put(&mut self, x: usize, y: usize, w: usize, h: usize, px: &[u32]) {
            for row in 0..h {
                self.px[(y + row) * self.w + x..][..w].copy_from_slice(&px[row * w..][..w]);
            }
        }
        fn copy(&mut self, sx: usize, sy: usize, x: usize, y: usize, w: usize, h: usize) {
            let old = self.px.clone();
            for row in 0..h {
                self.px[(y + row) * self.w + x..][..w].copy_from_slice(&old[(sy + row) * self.w + sx..][..w]);
            }
        }
    }

    fn pic(w: usize, h: usize) -> Pic {
        Pic { w, px: vec![0; w * h] }
    }

    /// A zlib stream that keeps going across messages, sync-flushed, as servers have it.
    struct Deflater(Box<CompressorOxide>, bool);

    impl Deflater {
        fn new() -> Self {
            Deflater(Box::new(CompressorOxide::new(create_comp_flags_from_zip_params(6, 15, 0))), false)
        }
        fn chunk(&mut self, data: &[u8]) -> Vec<u8> {
            let mut out = vec![0u8; data.len() * 2 + 64];
            let (status, _, written) = compress(&mut self.0, data, &mut out, TDEFLFlush::Sync);
            assert!(status as i32 >= 0);
            out.truncate(written);
            self.1 = true;
            out
        }
    }

    fn rgb32(c: u32) -> [u8; 4] {
        c.to_le_bytes()
    }

    #[test]
    fn pixel_formats() {
        let p = Pf::ours(32);
        assert_eq!(p.to_bytes(), [32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        assert_eq!(p.rgb(0xff12_3456), 0x12_3456);
        let p = Pf::ours(16);
        assert_eq!(p.rgb(0xffff), 0xff_ffff);
        assert_eq!(p.rgb(0xf800), 0xff_0000);
        let p = Pf::ours(8);
        assert_eq!(p.rgb(0b11_000_000), 0x00_00ff);
        assert_eq!(p.rgb(0b00_000_111), 0xff_0000);
    }

    #[test]
    fn raw_copyrect_rre_corre() {
        let mut d = Decoder::new(Pf::ours(32));
        let mut c = pic(4, 4);
        let raw: Vec<u8> = (0..4u32).flat_map(|i| rgb32(i + 1)).collect();
        d.rect(&mut Cursor { data: &raw }, &mut c, 0, 0, 2, 2, RAW).unwrap();
        assert_eq!(&c.px[..2], &[1, 2]);
        assert_eq!(&c.px[4..6], &[3, 4]);
        d.rect(&mut Cursor { data: &[0, 0, 0, 0] }, &mut c, 2, 2, 2, 2, COPYRECT).unwrap();
        assert_eq!(&c.px[10..12], &[1, 2]);
        // RRE: background 9, one 1x2 sub-rectangle of 7 at (1, 0).
        let mut rre = vec![0, 0, 0, 1];
        rre.extend(rgb32(9));
        rre.extend(rgb32(7));
        rre.extend([0, 1, 0, 0, 0, 1, 0, 2]);
        d.rect(&mut Cursor { data: &rre }, &mut c, 0, 0, 2, 2, RRE).unwrap();
        assert_eq!(&c.px[..2], &[9, 7]);
        assert_eq!(&c.px[4..6], &[9, 7]);
        let mut corre = vec![0, 0, 0, 1];
        corre.extend(rgb32(5));
        corre.extend(rgb32(6));
        corre.extend([0, 0, 1, 1]);
        d.rect(&mut Cursor { data: &corre }, &mut c, 2, 0, 2, 1, CORRE).unwrap();
        assert_eq!(&c.px[2..4], &[6, 5]);
    }

    #[test]
    fn hextile() {
        let mut d = Decoder::new(Pf::ours(32));
        let mut c = pic(20, 2);
        // Tile 1 (16x2): background 1, foreground 2, one 2x1 subrect at (3,1).
        let mut data = vec![2 | 4 | 8];
        data.extend(rgb32(1));
        data.extend(rgb32(2));
        data.extend([1, 0x31, 0x10]);
        // Tile 2 (4x2): raw.
        data.push(1);
        data.extend((0..8u32).flat_map(|i| rgb32(10 + i)));
        d.rect(&mut Cursor { data: &data }, &mut c, 0, 0, 20, 2, HEXTILE).unwrap();
        assert_eq!(c.px[0], 1);
        assert_eq!(&c.px[20 + 3..20 + 6], &[2, 2, 1]);
        assert_eq!(&c.px[16..20], &[10, 11, 12, 13]);
        assert_eq!(&c.px[36..40], &[14, 15, 16, 17]);
    }

    #[test]
    fn zrle_across_two_rects() {
        let mut d = Decoder::new(Pf::ours(32));
        let mut z = Deflater::new();
        let mut c = pic(4, 2);
        let cp = |v: u32| [v as u8, (v >> 8) as u8, (v >> 16) as u8];
        // Rect 1, 4x1: palette of 2, packed 1 bit: 0101.
        let mut t = vec![2];
        t.extend(cp(0xff0000));
        t.extend(cp(0x0000ff));
        t.push(0b0101_0000);
        let chunk = z.chunk(&t);
        let mut msg = (chunk.len() as u32).to_be_bytes().to_vec();
        msg.extend(chunk);
        d.rect(&mut Cursor { data: &msg }, &mut c, 0, 0, 4, 1, ZRLE).unwrap();
        assert_eq!(&c.px[..4], &[0xff0000, 0xff, 0xff0000, 0xff]);
        // Rect 2 on the same stream, 4x1: plain RLE, 3 of one colour, then 1.
        let mut t = vec![128];
        t.extend(cp(0x123456));
        t.push(2);
        t.extend(cp(0x654321));
        t.push(0);
        let chunk = z.chunk(&t);
        let mut msg = (chunk.len() as u32).to_be_bytes().to_vec();
        msg.extend(chunk);
        d.rect(&mut Cursor { data: &msg }, &mut c, 0, 1, 4, 1, ZRLE).unwrap();
        assert_eq!(&c.px[4..8], &[0x123456, 0x123456, 0x123456, 0x654321]);
    }

    #[test]
    fn trle_palette_rle_and_reuse() {
        let mut d = Decoder::new(Pf::ours(32));
        let mut c = pic(4, 2);
        let cp = |v: u32| [v as u8, (v >> 8) as u8, (v >> 16) as u8];
        // Palette RLE with 2 colours: index 1 run of 3, then index 0.
        let mut t = vec![130];
        t.extend(cp(7));
        t.extend(cp(8));
        t.extend([128 | 1, 2, 0]);
        d.rect(&mut Cursor { data: &t }, &mut c, 0, 0, 4, 1, TRLE).unwrap();
        assert_eq!(&c.px[..4], &[8, 8, 8, 7]);
        // Reuse that palette, packed: 1 0 0 1.
        d.rect(&mut Cursor { data: &[127, 0b1001_0000] }, &mut c, 0, 1, 4, 1, TRLE).unwrap();
        assert_eq!(&c.px[4..8], &[8, 7, 7, 8]);
    }

    #[test]
    fn tight_fill_palette_gradient() {
        let mut d = Decoder::new(Pf::ours(32));
        let mut c = pic(8, 2);
        d.rect(&mut Cursor { data: &[0x80, 0x12, 0x34, 0x56] }, &mut c, 0, 0, 8, 2, TIGHT).unwrap();
        assert!(c.px.iter().all(|&p| p == 0x123456));
        // Palette filter, 2 colours, 8x2 = 2 bytes of bits: under 12, uncompressed.
        let data = [0x40, 1, 1, 0, 0, 0, 0xff, 0xff, 0xff, 0b1010_1010, 0b0000_1111];
        d.rect(&mut Cursor { data: &data }, &mut c, 0, 0, 8, 2, TIGHT).unwrap();
        assert_eq!(&c.px[..4], &[0xffffff, 0, 0xffffff, 0]);
        assert_eq!(&c.px[12..16], &[0xffffff; 4]);
        // Gradient filter, 4x1 of a ramp, compressed on stream 1.
        let mut z = Deflater::new();
        let ramp = [10u8, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30]; // differences: +10,+20,+30 each step
        let chunk = z.chunk(&ramp);
        let mut data = vec![0x40 | 0x10, 2, chunk.len() as u8];
        data.extend(chunk);
        let mut c = pic(4, 1);
        d.rect(&mut Cursor { data: &data }, &mut c, 0, 0, 4, 1, TIGHT).unwrap();
        assert_eq!(c.px, vec![0x0a141e, 0x14283c, 0x1e3c5a, 0x285078]);
    }

    #[test]
    fn compact_lengths() {
        assert_eq!(compact_length(&mut Cursor { data: &[0x05] }).unwrap(), 5);
        assert_eq!(compact_length(&mut Cursor { data: &[0x80 | 0x01, 0x02] }).unwrap(), 1 + (2 << 7));
        assert_eq!(compact_length(&mut Cursor { data: &[0xff, 0xff, 0xff] }).unwrap(), (1 << 22) - 1);
    }

    #[test]
    fn cursors() {
        let d = Decoder::new(Pf::ours(32));
        // 2x1 rich cursor: left pixel opaque.
        let mut data: Vec<u8> = rgb32(0x112233).into_iter().chain(rgb32(0x445566)).collect();
        data.push(0b1000_0000);
        let img = d.rich_cursor(&mut Cursor { data: &data }, 1, 0, 2, 1).unwrap().unwrap();
        assert_eq!(img.rgba, vec![0x11, 0x22, 0x33, 255, 0x44, 0x55, 0x66, 0]);
        assert_eq!((img.hot_x, img.hot_y), (1, 0));
        assert!(d.x_cursor(&mut Cursor { data: &[] }, 0, 0, 0, 0).unwrap().is_none());
    }
}
