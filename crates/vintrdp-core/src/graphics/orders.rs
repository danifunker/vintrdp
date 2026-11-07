//! Drawing orders: primary orders (field-encoded against the previous order
//! of the same type), secondary orders (the caches) and alternate secondary
//! orders (DrawNineGrid's bitmaps). Port of rdesktop's orders.c and
//! orders.h.

#![allow(clippy::too_many_arguments)]

use super::cache::{BrushData, FontGlyph};
use super::colour::bytes_per_pixel;
use super::surface::Glyph;
use super::{Canvas, Core};
use crate::error::Result;
use crate::stream::Reader;

// controlFlags.
pub const STANDARD: u8 = 0x01;
pub const SECONDARY: u8 = 0x02;
pub const BOUNDS: u8 = 0x04;
pub const CHANGE: u8 = 0x08;
pub const DELTA: u8 = 0x10;
pub const LASTBOUNDS: u8 = 0x20;
pub const SMALL: u8 = 0x40;
pub const TINY: u8 = 0x80;

// Primary order types.
pub const DESTBLT: u8 = 0;
pub const PATBLT: u8 = 1;
pub const SCREENBLT: u8 = 2;
pub const DRAWNINEGRID: u8 = 7;
pub const MULTI_DRAWNINEGRID: u8 = 8;
pub const LINE: u8 = 9;
pub const RECT: u8 = 10;
pub const DESKSAVE: u8 = 11;
pub const MEMBLT: u8 = 13;
pub const TRIBLT: u8 = 14;
pub const POLYGON: u8 = 20;
pub const POLYGON2: u8 = 21;
pub const POLYLINE: u8 = 22;
pub const ELLIPSE: u8 = 25;
pub const ELLIPSE2: u8 = 26;
pub const TEXT2: u8 = 27;

// Secondary order types.
const RAW_BMPCACHE: u8 = 0;
const COLCACHE: u8 = 1;
const BMPCACHE: u8 = 2;
const FONTCACHE: u8 = 3;
const RAW_BMPCACHE2: u8 = 4;
const BMPCACHE2: u8 = 5;
const BRUSHCACHE: u8 = 7;

// Alternate secondary order types.
const ALTSEC_STREAM_BITMAP_FIRST: u8 = 2;
const ALTSEC_STREAM_BITMAP_NEXT: u8 = 3;
const ALTSEC_CREATE_NINEGRID_BITMAP: u8 = 4;

// Bitmap cache v2 flags.
const ID_MASK: u16 = 0x0007;
const MODE_MASK: u16 = 0x0038;
const MODE_SHIFT: u16 = 3;
const SQUARE: u16 = 0x0080;
const PERSIST: u16 = 0x0100;
const LONG_FORMAT: u8 = 0x80;
const BUFSIZE_MASK: u16 = 0x3fff;

// Stream bitmaps (DrawNineGrid).
const STREAM_BITMAP_END: u8 = 0x01;
const STREAM_BITMAP_COMPRESSED: u8 = 0x02;
const STREAM_BITMAP_V2: u8 = 0x04;
const STREAM_BITMAP_TYPE_NINEGRID: u16 = 0x0001;
pub const NINEGRID_CACHE_ENTRIES: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    pub left: i16,
    pub top: i16,
    pub right: i16,
    pub bottom: i16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Brush {
    pub xorigin: u8,
    pub yorigin: u8,
    pub style: u8,
    pub pattern: [u8; 8],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DestBlt {
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub opcode: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatBlt {
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub opcode: u8,
    pub bgcolour: u32,
    pub fgcolour: u32,
    pub brush: Brush,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenBlt {
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub opcode: u8,
    pub srcx: i16,
    pub srcy: i16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrawNineGrid {
    pub src_left: i16,
    pub src_top: i16,
    pub src_right: i16,
    pub src_bottom: i16,
    pub bitmap_id: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MultiDrawNineGrid {
    pub grid: DrawNineGrid,
    pub n_delta_entries: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub mixmode: u16,
    pub startx: i16,
    pub starty: i16,
    pub endx: i16,
    pub endy: i16,
    pub bgcolour: u32,
    pub opcode: u8,
    pub pen_style: u8,
    pub pen_width: u8,
    pub pen_colour: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub colour: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeskSave {
    pub offset: u32,
    pub left: i16,
    pub top: i16,
    pub right: i16,
    pub bottom: i16,
    pub action: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemBlt {
    pub colour_table: u8,
    pub cache_id: u8,
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub opcode: u8,
    pub srcx: i16,
    pub srcy: i16,
    pub cache_idx: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriBlt {
    pub mem: MemBlt,
    pub bgcolour: u32,
    pub fgcolour: u32,
    pub brush: Brush,
    pub unknown: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Polygon {
    pub x: i16,
    pub y: i16,
    pub opcode: u8,
    pub fillmode: u8,
    pub fgcolour: u32,
    pub npoints: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Polygon2 {
    pub x: i16,
    pub y: i16,
    pub opcode: u8,
    pub fillmode: u8,
    pub bgcolour: u32,
    pub fgcolour: u32,
    pub brush: Brush,
    pub npoints: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Polyline {
    pub x: i16,
    pub y: i16,
    pub opcode: u8,
    pub fgcolour: u32,
    pub lines: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ellipse {
    pub left: i16,
    pub top: i16,
    pub right: i16,
    pub bottom: i16,
    pub opcode: u8,
    pub fillmode: u8,
    pub bgcolour: u32,
    pub fgcolour: u32,
    pub brush: Brush,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Text2 {
    pub font: u8,
    pub flags: u8,
    pub opcode: u8,
    pub mixmode: u8,
    pub bgcolour: u32,
    pub fgcolour: u32,
    pub clipleft: i16,
    pub cliptop: i16,
    pub clipright: i16,
    pub clipbottom: i16,
    pub boxleft: i16,
    pub boxtop: i16,
    pub boxright: i16,
    pub boxbottom: i16,
    pub brush: Brush,
    pub x: i16,
    pub y: i16,
    pub text: Vec<u8>,
}

/// What primary orders remember between orders (RDP_ORDER_STATE).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderState {
    pub order_type: u8,
    pub bounds: Bounds,
    pub destblt: DestBlt,
    pub patblt: PatBlt,
    pub screenblt: ScreenBlt,
    pub drawninegrid: DrawNineGrid,
    pub multidrawninegrid: MultiDrawNineGrid,
    pub line: Line,
    pub rect: Rect,
    pub desksave: DeskSave,
    pub memblt: MemBlt,
    pub triblt: TriBlt,
    pub polygon: Polygon,
    pub polygon2: Polygon2,
    pub polyline: Polyline,
    pub ellipse: Ellipse,
    pub ellipse2: Ellipse,
    pub text2: Text2,
}

impl Default for OrderState {
    fn default() -> Self {
        OrderState {
            order_type: PATBLT,
            bounds: Bounds::default(),
            destblt: Default::default(),
            patblt: Default::default(),
            screenblt: Default::default(),
            drawninegrid: Default::default(),
            multidrawninegrid: Default::default(),
            line: Default::default(),
            rect: Default::default(),
            desksave: Default::default(),
            memblt: Default::default(),
            triblt: Default::default(),
            polygon: Default::default(),
            polygon2: Default::default(),
            polyline: Default::default(),
            ellipse: Default::default(),
            ellipse2: Default::default(),
            text2: Default::default(),
        }
    }
}

/// The field-present flags, `size` bytes less those SMALL and TINY drop.
pub fn read_present(r: &mut Reader, flags: u8, size: usize) -> Result<u32> {
    let mut size = size;
    if flags & SMALL != 0 {
        size = size.saturating_sub(1);
    }
    if flags & TINY != 0 {
        size = size.saturating_sub(2);
    }
    let mut present = 0u32;
    for i in 0..size {
        present |= (r.u8()? as u32) << (i * 8);
    }
    Ok(present)
}

/// A coordinate: 16 bits, or a signed 8-bit change to the last value.
fn coord(r: &mut Reader, c: &mut i16, delta: bool) -> Result<()> {
    *c = if delta { c.wrapping_add(r.i8()? as i16) } else { r.i16()? };
    Ok(())
}

/// Three bytes of colour, low first.
fn colour(r: &mut Reader) -> Result<u32> {
    let b = r.array::<3>()?;
    Ok(b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16)
}

fn read_bounds(r: &mut Reader, b: &mut Bounds) -> Result<()> {
    let present = r.u8()?;
    for (i, c) in [&mut b.left, &mut b.top, &mut b.right, &mut b.bottom].into_iter().enumerate() {
        if present & (1 << i) != 0 {
            coord(r, c, false)?;
        } else if present & (16 << i) != 0 {
            coord(r, c, true)?;
        }
    }
    Ok(())
}

fn read_brush(r: &mut Reader, b: &mut Brush, present: u32) -> Result<()> {
    if present & 1 != 0 {
        b.xorigin = r.u8()?;
    }
    if present & 2 != 0 {
        b.yorigin = r.u8()?;
    }
    if present & 4 != 0 {
        b.style = r.u8()?;
    }
    if present & 8 != 0 {
        b.pattern[0] = r.u8()?;
    }
    if present & 16 != 0 {
        b.pattern[1..].copy_from_slice(r.bytes(7)?);
    }
    Ok(())
}

/// A length-prefixed run of bytes (polygon points, text).
fn read_data(r: &mut Reader, out: &mut Vec<u8>) -> Result<()> {
    let n = r.u8()? as usize;
    *out = r.bytes(n)?.to_vec();
    Ok(())
}

impl DestBlt {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        for (i, c) in [&mut self.x, &mut self.y, &mut self.cx, &mut self.cy].into_iter().enumerate() {
            if present & (1 << i) != 0 {
                coord(r, c, delta)?;
            }
        }
        if present & 0x10 != 0 {
            self.opcode = r.u8()?;
        }
        Ok(())
    }
}

impl PatBlt {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        for (i, c) in [&mut self.x, &mut self.y, &mut self.cx, &mut self.cy].into_iter().enumerate() {
            if present & (1 << i) != 0 {
                coord(r, c, delta)?;
            }
        }
        if present & 0x10 != 0 {
            self.opcode = r.u8()?;
        }
        if present & 0x20 != 0 {
            self.bgcolour = colour(r)?;
        }
        if present & 0x40 != 0 {
            self.fgcolour = colour(r)?;
        }
        read_brush(r, &mut self.brush, present >> 7)
    }
}

impl ScreenBlt {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        for (i, c) in [&mut self.x, &mut self.y, &mut self.cx, &mut self.cy].into_iter().enumerate() {
            if present & (1 << i) != 0 {
                coord(r, c, delta)?;
            }
        }
        if present & 0x10 != 0 {
            self.opcode = r.u8()?;
        }
        if present & 0x20 != 0 {
            coord(r, &mut self.srcx, delta)?;
        }
        if present & 0x40 != 0 {
            coord(r, &mut self.srcy, delta)?;
        }
        Ok(())
    }
}

impl DrawNineGrid {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        for (i, c) in [&mut self.src_left, &mut self.src_top, &mut self.src_right, &mut self.src_bottom].into_iter().enumerate() {
            if present & (1 << i) != 0 {
                coord(r, c, delta)?;
            }
        }
        if present & 0x10 != 0 {
            self.bitmap_id = r.u16()?;
        }
        Ok(())
    }
}

impl MultiDrawNineGrid {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        self.grid.parse(r, present & 0x1f, delta)?;
        if present & 0x20 != 0 {
            self.n_delta_entries = r.u8()?;
        }
        if present & 0x40 != 0 {
            let n = r.u16()? as usize;
            let data = r.bytes(n)?;
            // rdesktop's limit for the coded rectangles.
            self.data = if n > 256 { Vec::new() } else { data.to_vec() };
        }
        Ok(())
    }
}

impl Line {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        if present & 0x01 != 0 {
            self.mixmode = r.u16()?;
        }
        for (i, c) in [&mut self.startx, &mut self.starty, &mut self.endx, &mut self.endy].into_iter().enumerate() {
            if present & (2 << i) != 0 {
                coord(r, c, delta)?;
            }
        }
        if present & 0x20 != 0 {
            self.bgcolour = colour(r)?;
        }
        if present & 0x40 != 0 {
            self.opcode = r.u8()?;
        }
        // The pen.
        if present & 0x80 != 0 {
            self.pen_style = r.u8()?;
        }
        if present & 0x100 != 0 {
            self.pen_width = r.u8()?;
        }
        if present & 0x200 != 0 {
            self.pen_colour = colour(r)?;
        }
        Ok(())
    }
}

impl Rect {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        for (i, c) in [&mut self.x, &mut self.y, &mut self.cx, &mut self.cy].into_iter().enumerate() {
            if present & (1 << i) != 0 {
                coord(r, c, delta)?;
            }
        }
        // The colour comes a byte at a time, each optional.
        for i in 0..3 {
            if present & (0x10 << i) != 0 {
                let shift = 8 * i;
                self.colour = (self.colour & !(0xff << shift)) | (r.u8()? as u32) << shift;
            }
        }
        Ok(())
    }
}

impl DeskSave {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        if present & 0x01 != 0 {
            self.offset = r.u32()?;
        }
        for (i, c) in [&mut self.left, &mut self.top, &mut self.right, &mut self.bottom].into_iter().enumerate() {
            if present & (2 << i) != 0 {
                coord(r, c, delta)?;
            }
        }
        if present & 0x20 != 0 {
            self.action = r.u8()?;
        }
        Ok(())
    }
}

impl MemBlt {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        if present & 0x0001 != 0 {
            self.cache_id = r.u8()?;
            self.colour_table = r.u8()?;
        }
        for (i, c) in [&mut self.x, &mut self.y, &mut self.cx, &mut self.cy].into_iter().enumerate() {
            if present & (2 << i) != 0 {
                coord(r, c, delta)?;
            }
        }
        if present & 0x0020 != 0 {
            self.opcode = r.u8()?;
        }
        if present & 0x0040 != 0 {
            coord(r, &mut self.srcx, delta)?;
        }
        if present & 0x0080 != 0 {
            coord(r, &mut self.srcy, delta)?;
        }
        if present & 0x0100 != 0 {
            self.cache_idx = r.u16()?;
        }
        Ok(())
    }
}

impl TriBlt {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        // The same fields as MemBlt up to srcy, then the brush's.
        self.mem.parse(r, present & 0xff, delta)?;
        if present & 0x000100 != 0 {
            self.bgcolour = colour(r)?;
        }
        if present & 0x000200 != 0 {
            self.fgcolour = colour(r)?;
        }
        read_brush(r, &mut self.brush, present >> 10)?;
        if present & 0x008000 != 0 {
            self.mem.cache_idx = r.u16()?;
        }
        if present & 0x010000 != 0 {
            self.unknown = r.u16()?;
        }
        Ok(())
    }
}

impl Polygon {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        if present & 0x01 != 0 {
            coord(r, &mut self.x, delta)?;
        }
        if present & 0x02 != 0 {
            coord(r, &mut self.y, delta)?;
        }
        if present & 0x04 != 0 {
            self.opcode = r.u8()?;
        }
        if present & 0x08 != 0 {
            self.fillmode = r.u8()?;
        }
        if present & 0x10 != 0 {
            self.fgcolour = colour(r)?;
        }
        if present & 0x20 != 0 {
            self.npoints = r.u8()?;
        }
        if present & 0x40 != 0 {
            read_data(r, &mut self.data)?;
        }
        Ok(())
    }
}

impl Polygon2 {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        if present & 0x0001 != 0 {
            coord(r, &mut self.x, delta)?;
        }
        if present & 0x0002 != 0 {
            coord(r, &mut self.y, delta)?;
        }
        if present & 0x0004 != 0 {
            self.opcode = r.u8()?;
        }
        if present & 0x0008 != 0 {
            self.fillmode = r.u8()?;
        }
        if present & 0x0010 != 0 {
            self.bgcolour = colour(r)?;
        }
        if present & 0x0020 != 0 {
            self.fgcolour = colour(r)?;
        }
        read_brush(r, &mut self.brush, present >> 6)?;
        if present & 0x0800 != 0 {
            self.npoints = r.u8()?;
        }
        if present & 0x1000 != 0 {
            read_data(r, &mut self.data)?;
        }
        Ok(())
    }
}

impl Polyline {
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool) -> Result<()> {
        if present & 0x01 != 0 {
            coord(r, &mut self.x, delta)?;
        }
        if present & 0x02 != 0 {
            coord(r, &mut self.y, delta)?;
        }
        if present & 0x04 != 0 {
            self.opcode = r.u8()?;
        }
        // 0x08 is the unused brush cache entry.
        if present & 0x10 != 0 {
            self.fgcolour = colour(r)?;
        }
        if present & 0x20 != 0 {
            self.lines = r.u8()?;
        }
        if present & 0x40 != 0 {
            read_data(r, &mut self.data)?;
        }
        Ok(())
    }
}

impl Ellipse {
    /// Ellipse (solid) and Ellipse2 (with a brush) share the layout up to
    /// the colours.
    pub fn parse(&mut self, r: &mut Reader, present: u32, delta: bool, with_brush: bool) -> Result<()> {
        for (i, c) in [&mut self.left, &mut self.top, &mut self.right, &mut self.bottom].into_iter().enumerate() {
            if present & (1 << i) != 0 {
                coord(r, c, delta)?;
            }
        }
        if present & 0x10 != 0 {
            self.opcode = r.u8()?;
        }
        if present & 0x20 != 0 {
            self.fillmode = r.u8()?;
        }
        if !with_brush {
            if present & 0x40 != 0 {
                self.fgcolour = colour(r)?;
            }
            return Ok(());
        }
        if present & 0x40 != 0 {
            self.bgcolour = colour(r)?;
        }
        if present & 0x80 != 0 {
            self.fgcolour = colour(r)?;
        }
        read_brush(r, &mut self.brush, present >> 8)
    }
}

impl Text2 {
    pub fn parse(&mut self, r: &mut Reader, present: u32) -> Result<()> {
        if present & 0x000001 != 0 {
            self.font = r.u8()?;
        }
        if present & 0x000002 != 0 {
            self.flags = r.u8()?;
        }
        if present & 0x000004 != 0 {
            self.opcode = r.u8()?;
        }
        if present & 0x000008 != 0 {
            self.mixmode = r.u8()?;
        }
        if present & 0x000010 != 0 {
            self.fgcolour = colour(r)?;
        }
        if present & 0x000020 != 0 {
            self.bgcolour = colour(r)?;
        }
        let fields = [
            &mut self.clipleft,
            &mut self.cliptop,
            &mut self.clipright,
            &mut self.clipbottom,
            &mut self.boxleft,
            &mut self.boxtop,
            &mut self.boxright,
            &mut self.boxbottom,
        ];
        for (i, f) in fields.into_iter().enumerate() {
            if present & (0x40 << i) != 0 {
                *f = r.i16()?;
            }
        }
        read_brush(r, &mut self.brush, present >> 14)?;
        if present & 0x080000 != 0 {
            self.x = r.i16()?;
        }
        if present & 0x100000 != 0 {
            self.y = r.i16()?;
        }
        if present & 0x200000 != 0 {
            read_data(r, &mut self.text)?;
        }
        Ok(())
    }
}

/// One coordinate of a polyline or polygon's delta list.
fn parse_delta(data: &[u8], pos: &mut usize) -> i16 {
    let mut next = || {
        let b = data.get(*pos).copied().unwrap_or(0);
        *pos += 1;
        b as i32
    };
    let first = next();
    let mut v = if first & 0x40 != 0 { first | !0x3f } else { first & 0x3f };
    if first & 0x80 != 0 {
        v = (v << 8) | next();
    }
    v as i16
}

/// The absolute points of a polyline or polygon: `(x, y)` and `n` more,
/// each a delta from the one before. `None` if the data runs out first.
pub fn delta_points(x: i16, y: i16, n: u8, data: &[u8]) -> Option<Vec<(i32, i32)>> {
    let n = n as usize;
    let mut deltas = vec![(0i16, 0i16); n + 1];
    deltas[0] = (x, y);
    // A byte of flags per four points comes first.
    let (mut index, mut pos) = (0, n.saturating_sub(1) / 4 + 1);
    let mut flags = 0u8;
    let mut next = 1;
    while next <= n && pos < data.len() {
        if (next - 1) % 4 == 0 {
            flags = data.get(index).copied().unwrap_or(0);
            index += 1;
        }
        if flags & 0x80 == 0 {
            deltas[next].0 = parse_delta(data, &mut pos);
        }
        if flags & 0x40 == 0 {
            deltas[next].1 = parse_delta(data, &mut pos);
        }
        flags <<= 2;
        next += 1;
    }
    if next - 1 != n {
        return None;
    }
    let (mut ax, mut ay) = (0i32, 0i32);
    Some(
        deltas
            .iter()
            .map(|&(dx, dy)| {
                ax += dx as i32;
                ay += dy as i32;
                (ax, ay)
            })
            .collect(),
    )
}

fn packed_signed_delta(data: &[u8], pos: &mut usize) -> i32 {
    let Some(&first) = data.get(*pos) else { return 0 };
    *pos += 1;
    let mut v = first as i32;
    if v & 0x80 != 0 {
        let Some(&second) = data.get(*pos) else { return 0 };
        *pos += 1;
        v = ((v & 0x7f) << 8) | second as i32;
        if v & 0x4000 != 0 { -(v & 0x3fff) } else { v & 0x3fff }
    } else if v & 0x40 != 0 {
        -(v & 0x3f)
    } else {
        v & 0x3f
    }
}

/// MultiDrawNineGrid's coded rectangles: (left, top, width, height), each
/// field a delta from the rectangle before unless its zero bit is set.
pub fn delta_rects(data: &[u8], count: u8) -> Option<Vec<(i32, i32, i32, i32)>> {
    let count = count as usize;
    let zero_bytes = count.div_ceil(2);
    if count == 0 || count > 45 || data.len() < zero_bytes {
        return None;
    }
    let mut pos = zero_bytes;
    let mut v = [0i16; 4];
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let bits = data[i / 2];
        let nibble = if i & 1 != 0 { bits & 0x0f } else { bits >> 4 };
        for (k, field) in v.iter_mut().enumerate() {
            if nibble & (0x08 >> k) == 0 {
                *field = field.wrapping_add(packed_signed_delta(data, &mut pos) as i16);
            }
        }
        out.push((v[0] as i32, v[1] as i32, v[2] as i32, v[3] as i32));
    }
    Some(out)
}

/// Flip bottom-up rows of `row` bytes to top first. Short data is padded.
pub fn flip_rows(data: &[u8], row: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0; row * height];
    for (y, chunk) in data.chunks(row.max(1)).take(height).enumerate() {
        let at = (height - 1 - y) * row;
        out[at..at + chunk.len()].copy_from_slice(chunk);
    }
    out
}

/// Expand a compressed 8x8 brush: 2 bits per pixel, bottom row first, then
/// a 4-colour palette of `bpp_bytes`-byte entries.
fn expand_brush(input: &[u8], bpp_bytes: usize) -> Vec<u8> {
    let mut out = vec![0; 64 * bpp_bytes];
    let pal = &input[16.min(input.len())..];
    let mut in_index = 0;
    for y in (0..8).rev() {
        let mut x = 0;
        for _ in 0..2 {
            let byte = input.get(in_index).copied().unwrap_or(0);
            for shift in [6, 4, 2, 0] {
                let p = ((byte >> shift) & 3) as usize;
                for i in 0..bpp_bytes {
                    out[(y * 8 + x) * bpp_bytes + i] = pal.get(p * bpp_bytes + i).copied().unwrap_or(0);
                }
                x += 1;
            }
            in_index += 1;
        }
    }
    out
}

/// The stream bitmap being received for a nine-grid.
#[derive(Default)]
pub struct StreamBitmap {
    pub active: bool,
    pub flags: u8,
    pub bpp: u8,
    pub kind: u16,
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

/// A nine-grid bitmap: 32 bpp BGRA pixels, rows top first, and how to
/// stretch it.
pub struct NineGridEntry {
    pub width: u16,
    pub height: u16,
    pub flags: u32,
    pub left_width: u16,
    pub right_width: u16,
    pub top_height: u16,
    pub bottom_height: u16,
    /// The transparent colour; rdesktop ignored it too.
    #[allow(dead_code)]
    pub transparent: u32,
    pub data: Vec<u8>,
}

impl Core {
    /// Process `count` orders (process_orders). Stops quietly at an order it
    /// can't size, as rdesktop does.
    pub(super) fn process_orders(&mut self, r: &mut Reader, count: u16, cv: &mut Canvas) -> Result<()> {
        for _ in 0..count {
            let flags = r.u8()?;
            if flags & STANDARD == 0 {
                if flags & (STANDARD | SECONDARY) == SECONDARY {
                    if !self.altsec_order(r, flags >> 2)? {
                        return Ok(());
                    }
                    continue;
                }
                super::warn(format_args!("order parsing failed (flags {flags:#x})"));
                return Ok(());
            }
            if flags & SECONDARY != 0 {
                self.secondary_order(r)?;
                continue;
            }
            if !self.primary_order(r, flags, cv)? {
                return Ok(());
            }
        }
        Ok(())
    }

    /// One primary order. False if its type is unknown.
    fn primary_order(&mut self, r: &mut Reader, flags: u8, cv: &mut Canvas) -> Result<bool> {
        if flags & CHANGE != 0 {
            self.orders.order_type = r.u8()?;
        }
        let kind = self.orders.order_type;
        let size = match kind {
            TRIBLT | TEXT2 => 3,
            PATBLT | MEMBLT | LINE | POLYGON2 | ELLIPSE2 => 2,
            _ => 1,
        };
        let present = read_present(r, flags, size)?;
        if flags & BOUNDS != 0 {
            if flags & LASTBOUNDS == 0 {
                read_bounds(r, &mut self.orders.bounds)?;
            }
            let b = self.orders.bounds;
            let (l, t) = (b.left as i32, b.top as i32);
            cv.draw(|s| s.set_clip(l, t, b.right as i32 - l + 1, b.bottom as i32 - t + 1));
        }
        let delta = flags & DELTA != 0;
        let result = self.dispatch(kind, r, present, delta, cv);
        if flags & BOUNDS != 0 {
            cv.draw(|s| s.reset_clip());
        }
        result
    }

    fn dispatch(&mut self, kind: u8, r: &mut Reader, present: u32, delta: bool, cv: &mut Canvas) -> Result<bool> {
        let os = &mut self.orders;
        match kind {
            DESTBLT => {
                os.destblt.parse(r, present, delta)?;
                let o = os.destblt.clone();
                self.draw_destblt(&o, cv);
            }
            PATBLT => {
                os.patblt.parse(r, present, delta)?;
                let o = os.patblt.clone();
                self.draw_patblt(&o, cv);
            }
            SCREENBLT => {
                os.screenblt.parse(r, present, delta)?;
                let o = os.screenblt.clone();
                self.draw_screenblt(&o, cv);
            }
            DRAWNINEGRID => {
                os.drawninegrid.parse(r, present, delta)?;
                let (o, b) = (os.drawninegrid.clone(), os.bounds);
                let (l, t) = (b.left as i32, b.top as i32);
                self.draw_ninegrid(&o, l, t, b.right as i32 - l + 1, b.bottom as i32 - t + 1, cv);
            }
            MULTI_DRAWNINEGRID => {
                os.multidrawninegrid.parse(r, present, delta)?;
                let o = os.multidrawninegrid.clone();
                self.draw_multi_ninegrid(&o, cv);
            }
            LINE => {
                os.line.parse(r, present, delta)?;
                let o = os.line.clone();
                self.draw_line(&o, cv);
            }
            RECT => {
                os.rect.parse(r, present, delta)?;
                let o = os.rect.clone();
                self.draw_rect(&o, cv);
            }
            DESKSAVE => {
                os.desksave.parse(r, present, delta)?;
                let o = os.desksave.clone();
                self.draw_desksave(&o, cv);
            }
            MEMBLT => {
                os.memblt.parse(r, present, delta)?;
                let o = os.memblt.clone();
                self.draw_memblt(&o, cv);
            }
            TRIBLT => {
                os.triblt.parse(r, present, delta)?;
                let o = os.triblt.clone();
                self.draw_triblt(&o, cv);
            }
            POLYGON => {
                os.polygon.parse(r, present, delta)?;
                let o = os.polygon.clone();
                let p = Polygon2 { x: o.x, y: o.y, opcode: o.opcode, fillmode: o.fillmode, fgcolour: o.fgcolour, npoints: o.npoints, data: o.data, ..Default::default() };
                self.draw_polygon(&p, false, cv);
            }
            POLYGON2 => {
                os.polygon2.parse(r, present, delta)?;
                let o = os.polygon2.clone();
                self.draw_polygon(&o, true, cv);
            }
            POLYLINE => {
                os.polyline.parse(r, present, delta)?;
                let o = os.polyline.clone();
                self.draw_polyline(&o, cv);
            }
            ELLIPSE => {
                os.ellipse.parse(r, present, delta, false)?;
                let o = os.ellipse.clone();
                self.draw_ellipse(&o, false, cv);
            }
            ELLIPSE2 => {
                os.ellipse2.parse(r, present, delta, true)?;
                let o = os.ellipse2.clone();
                self.draw_ellipse(&o, true, cv);
            }
            TEXT2 => {
                os.text2.parse(r, present)?;
                let o = os.text2.clone();
                self.draw_text(&o, cv);
            }
            _ => {
                super::warn(format_args!("unhandled order type {kind}"));
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A secondary order: a cache update, with its own length.
    fn secondary_order(&mut self, r: &mut Reader) -> Result<()> {
        // The server miscounts very short orders, so this can be negative.
        let length = r.i16()? as i32 + 13 - 6;
        let flags = r.u16()?;
        let kind = r.u8()?;
        let Ok(length) = usize::try_from(length) else {
            // No usable length: parse in place, as rdesktop effectively does.
            return self.cache_order(r, kind, flags);
        };
        // Bounded to its own bytes; a bad one is skipped, not fatal.
        let mut sub = r.sub(length)?;
        if let Err(e) = self.cache_order(&mut sub, kind, flags) {
            super::warn(format_args!("secondary order {kind}: {e}"));
        }
        Ok(())
    }

    fn cache_order(&mut self, r: &mut Reader, kind: u8, flags: u16) -> Result<()> {
        match kind {
            RAW_BMPCACHE => self.raw_bmpcache(r),
            COLCACHE => self.colcache(r),
            BMPCACHE => self.bmpcache(r),
            FONTCACHE => self.fontcache(r),
            RAW_BMPCACHE2 => self.bmpcache2(r, flags, false),
            BMPCACHE2 => self.bmpcache2(r, flags, true),
            BRUSHCACHE => self.brushcache(r),
            _ => {
                super::warn(format_args!("unhandled secondary order {kind}"));
                Ok(())
            }
        }
    }

    fn raw_bmpcache(&mut self, r: &mut Reader) -> Result<()> {
        let cache_id = r.u8()?;
        r.skip(1)?;
        let (width, height, bpp) = (r.u8()? as usize, r.u8()? as usize, r.u8()? as u32);
        let bytes = bytes_per_pixel(bpp);
        let bufsize = r.u16()? as usize;
        let cache_idx = r.u16()?;
        let data = r.bytes(bufsize)?;
        let pixels = flip_rows(data, width * bytes, height);
        let bitmap = self.make_bitmap(bytes, width, height, &pixels);
        self.put_bitmap(cache_id, cache_idx, bitmap);
        Ok(())
    }

    fn bmpcache(&mut self, r: &mut Reader) -> Result<()> {
        let cache_id = r.u8()?;
        r.skip(1)?;
        let (width, height, bpp) = (r.u8()? as usize, r.u8()? as usize, r.u8()? as u32);
        let bufsize = r.u16()?;
        let cache_idx = r.u16()?;
        let size = if self.params.rdp_version >= 5 {
            bufsize
        } else {
            // TS_CD_HEADER: pad, main body size, row size, final size.
            r.skip(2)?;
            let size = r.u16()?;
            r.skip(4)?;
            size
        };
        let data = r.bytes(size as usize)?;
        let bytes = bytes_per_pixel(bpp);
        match self.decompress(data, width, height, bytes) {
            Some(pixels) => {
                let bitmap = self.make_bitmap(bytes, width, height, &pixels);
                self.put_bitmap(cache_id, cache_idx, bitmap);
            }
            None => super::warn(format_args!("failed to decompress a cached bitmap")),
        }
        Ok(())
    }

    fn bmpcache2(&mut self, r: &mut Reader, flags: u16, compressed: bool) -> Result<()> {
        let cache_id = (flags & ID_MASK) as u8;
        let mode = (flags & MODE_MASK) >> MODE_SHIFT;
        if !(3..=6).contains(&mode) {
            crate::bail!("bitmap cache v2 with mode {mode}");
        }
        let bytes = (mode - 2) as usize;
        let key: Option<[u8; 8]> = if flags & PERSIST != 0 { Some(r.array()?) } else { None };
        let width = r.u8()?;
        let height = if flags & SQUARE != 0 { width } else { r.u8()? };
        let bufsize = r.u16_be()? & BUFSIZE_MASK;
        let mut cache_idx = r.u8()? as u16;
        if cache_idx as u8 & LONG_FORMAT != 0 {
            cache_idx = ((cache_idx ^ LONG_FORMAT as u16) << 8) | r.u8()? as u16;
        }
        let data = r.bytes(bufsize as usize)?;
        let (w, h) = (width as usize, height as usize);
        let pixels = if compressed {
            match self.decompress(data, w, h, bytes) {
                Some(p) => p,
                None => {
                    super::warn(format_args!("failed to decompress a cached bitmap"));
                    return Ok(());
                }
            }
        } else {
            flip_rows(data, w * bytes, h)
        };
        let bitmap = self.make_bitmap(bytes, w, h, &pixels);
        self.put_bitmap(cache_id, cache_idx, bitmap);
        if let Some(key) = key {
            self.pst.save(cache_id, cache_idx, key, width, height, &pixels);
        }
        Ok(())
    }

    fn colcache(&mut self, r: &mut Reader) -> Result<()> {
        let cache_id = r.u8()?;
        let n = r.u16()? as usize;
        let mut palette = [0u32; 256];
        for i in 0..n {
            let [b, g, red, _] = r.array::<4>()?;
            if let Some(p) = palette.get_mut(i) {
                *p = (red as u32) << 16 | (g as u32) << 8 | b as u32;
            }
        }
        // As rdesktop: only a table other than the first is applied.
        if cache_id != 0 {
            self.colours.palette = palette;
        }
        Ok(())
    }

    fn fontcache(&mut self, r: &mut Reader) -> Result<()> {
        let font = r.u8()?;
        let n = r.u8()?;
        for _ in 0..n {
            let character = r.u16()?;
            let offset = r.i16()?;
            let baseline = r.i16()?;
            let width = r.u16()?;
            let height = r.u16()?;
            let stride = (width as usize).div_ceil(8);
            let size = (height as usize * stride + 3) & !3;
            let data = r.bytes(size)?;
            let glyph = Glyph { width: width as usize, height: height as usize, stride, bits: data[..stride * height as usize].to_vec() };
            self.glyphs.put(font, character, FontGlyph { offset, baseline, width, height, glyph });
        }
        Ok(())
    }

    fn brushcache(&mut self, r: &mut Reader) -> Result<()> {
        let cache_idx = r.u8()?;
        let colour_code = r.u8()?;
        let width = r.u8()?;
        let height = r.u8()?;
        let _kind = r.u8()?;
        let size = r.u8()? as usize;
        if (width, height) != (8, 8) {
            super::warn(format_args!("incompatible brush, {width}x{height}"));
            return Ok(());
        }
        match colour_code {
            1 => {
                let mut data = vec![0u8; 8];
                if size == 8 {
                    // Bottom row first.
                    for b in data.iter_mut().rev() {
                        *b = r.u8()?;
                    }
                } else {
                    super::warn(format_args!("incompatible brush, colour code 1, size {size}"));
                }
                self.brushes.put(cache_idx, BrushData { colour_code, data });
            }
            3..=6 => {
                let bytes = colour_code as usize - 2;
                let data = if size == 16 + 4 * bytes {
                    expand_brush(r.bytes(size)?, bytes)
                } else {
                    r.bytes(64 * bytes)?.to_vec()
                };
                self.brushes.put(cache_idx, BrushData { colour_code, data });
            }
            _ => super::warn(format_args!("incompatible brush, colour code {colour_code}")),
        }
        Ok(())
    }

    /// An alternate secondary order. These carry no length, so false means
    /// one we can't skip and the rest of the update is lost.
    fn altsec_order(&mut self, r: &mut Reader, kind: u8) -> Result<bool> {
        match kind {
            ALTSEC_STREAM_BITMAP_FIRST => {
                let flags = r.u8()?;
                let bpp = r.u8()?;
                let stream_kind = r.u16()?;
                let width = r.u16()?;
                let height = r.u16()?;
                if flags & STREAM_BITMAP_V2 != 0 {
                    r.u32()?;
                } else {
                    r.u16()?;
                }
                let n = r.u16()? as usize;
                let block = r.bytes(n)?;
                self.stream = StreamBitmap { active: true, flags, bpp, kind: stream_kind, width, height, data: block.to_vec() };
                if flags & STREAM_BITMAP_END != 0 {
                    self.finish_stream();
                }
            }
            ALTSEC_STREAM_BITMAP_NEXT => {
                let flags = r.u8()?;
                let stream_kind = r.u16()?;
                let n = r.u16()? as usize;
                let block = r.bytes(n)?;
                if !self.stream.active || self.stream.kind != stream_kind {
                    super::warn(format_args!("stream bitmap block without a stream"));
                    return Ok(true);
                }
                // Hostile streams can't grow without bound.
                if self.stream.data.len() + n > 4 * 0x10000 * 4 {
                    self.stream = StreamBitmap::default();
                    return Ok(true);
                }
                self.stream.flags |= flags;
                self.stream.data.extend_from_slice(block);
                if flags & STREAM_BITMAP_END != 0 {
                    self.finish_stream();
                }
            }
            ALTSEC_CREATE_NINEGRID_BITMAP => {
                let bpp = r.u8()?;
                let bitmap_id = r.u16()?;
                let width = r.u16()?;
                let height = r.u16()?;
                let flags = r.u32()?;
                let left_width = r.u16()?;
                let right_width = r.u16()?;
                let top_height = r.u16()?;
                let bottom_height = r.u16()?;
                let transparent = r.u32()?;
                self.create_ninegrid(bpp, bitmap_id, width, height, [left_width, right_width, top_height, bottom_height], flags, transparent);
            }
            _ => {
                super::warn(format_args!("unsupported alternate secondary order {kind}"));
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The stream is complete: decompress it if need be (ninegrid_finalize_stream).
    fn finish_stream(&mut self) {
        if let Some(data) = self.stream_pixels() {
            self.stream.data = data;
            self.stream.flags &= !STREAM_BITMAP_COMPRESSED;
        } else {
            super::warn(format_args!("failed to decode a stream bitmap"));
        }
    }

    fn stream_pixels(&self) -> Option<Vec<u8>> {
        let s = &self.stream;
        if !s.active || s.kind != STREAM_BITMAP_TYPE_NINEGRID || s.data.is_empty() {
            return None;
        }
        let bytes = s.bpp as usize / 8;
        if !(1..=4).contains(&bytes) {
            return None;
        }
        let (w, h) = (s.width as usize, s.height as usize);
        if s.flags & STREAM_BITMAP_COMPRESSED != 0 {
            return self.decompress(&s.data, w, h, bytes);
        }
        s.data.get(..w * h * bytes).map(<[u8]>::to_vec)
    }

    fn create_ninegrid(&mut self, bpp: u8, id: u16, width: u16, height: u16, edges: [u16; 4], flags: u32, transparent: u32) {
        if id as usize >= NINEGRID_CACHE_ENTRIES {
            return;
        }
        if bpp != 32 {
            super::warn(format_args!("unsupported nine-grid depth {bpp}"));
            return;
        }
        let data = match self.stream_pixels() {
            Some(d) => d,
            None if self.stream.active && !self.stream.data.is_empty() => self.stream.data.clone(),
            None => {
                super::warn(format_args!("nine-grid bitmap without its data"));
                return;
            }
        };
        if data.len() < width as usize * height as usize * 4 {
            super::warn(format_args!("short nine-grid bitmap"));
            return;
        }
        let [left_width, right_width, top_height, bottom_height] = edges;
        self.ninegrids[id as usize] = Some(NineGridEntry { width, height, flags, left_width, right_width, top_height, bottom_height, transparent, data });
        self.stream = StreamBitmap::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed<T: Default>(bytes: &[u8], f: impl FnOnce(&mut T, &mut Reader) -> Result<()>) -> T {
        let mut o = T::default();
        let mut r = Reader::new(bytes);
        f(&mut o, &mut r).unwrap();
        assert!(r.is_empty(), "left {} bytes", r.remaining());
        o
    }

    #[test]
    fn present_flags_and_small_tiny() {
        let mut r = Reader::new(&[0x34, 0x12, 0x56]);
        assert_eq!(read_present(&mut r, 0, 3).unwrap(), 0x561234);
        let mut r = Reader::new(&[0x34, 0x12]);
        assert_eq!(read_present(&mut r, SMALL, 3).unwrap(), 0x1234);
        let mut r = Reader::new(&[0x34]);
        assert_eq!(read_present(&mut r, TINY, 3).unwrap(), 0x34);
        let mut r = Reader::new(&[]);
        assert_eq!(read_present(&mut r, TINY | SMALL, 2).unwrap(), 0);
    }

    #[test]
    fn coordinates_absolute_then_delta() {
        let mut o = DestBlt::default();
        o.parse(&mut Reader::new(&[0x10, 0x00, 0x20, 0x00, 0x05, 0x01, 0x07, 0x00, 0x55]), 0x1f, false).unwrap();
        assert_eq!(o, DestBlt { x: 16, y: 32, cx: 261, cy: 7, opcode: 0x55 });
        // Deltas are signed bytes; absent fields keep their value.
        o.parse(&mut Reader::new(&[0xfe, 0x03]), 0x05, true).unwrap();
        assert_eq!((o.x, o.y, o.cx, o.cy), (14, 32, 264, 7));
        // Wrapping, not overflow.
        o.x = i16::MAX;
        o.parse(&mut Reader::new(&[0x01]), 0x01, true).unwrap();
        assert_eq!(o.x, i16::MIN);
        assert!(o.parse(&mut Reader::new(&[0x01]), 0x01, false).is_err());
    }

    #[test]
    fn patblt_with_brush() {
        let b = [1, 0, 2, 0, 3, 0, 4, 0, 0xf0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 7, 8, 3, 0xaa, 1, 2, 3, 4, 5, 6, 7];
        let o = parsed::<PatBlt>(&b, |o, r| o.parse(r, 0xfff, false));
        assert_eq!((o.x, o.y, o.cx, o.cy, o.opcode), (1, 2, 3, 4, 0xf0));
        assert_eq!((o.bgcolour, o.fgcolour), (0x332211, 0x665544));
        assert_eq!(o.brush, Brush { xorigin: 7, yorigin: 8, style: 3, pattern: [0xaa, 1, 2, 3, 4, 5, 6, 7] });
    }

    #[test]
    fn screenblt() {
        let o = parsed::<ScreenBlt>(&[1, 0, 2, 0, 3, 0, 4, 0, 0xcc, 5, 0, 6, 0], |o, r| o.parse(r, 0x7f, false));
        assert_eq!(o, ScreenBlt { x: 1, y: 2, cx: 3, cy: 4, opcode: 0xcc, srcx: 5, srcy: 6 });
    }

    #[test]
    fn line_and_pen() {
        let b = [0x0d, 0x00, 1, 0, 2, 0, 3, 0, 4, 0, 9, 9, 9, 0x0d, 0, 1, 0x11, 0x22, 0x33];
        let o = parsed::<Line>(&b, |o, r| o.parse(r, 0x3ff, false));
        assert_eq!((o.mixmode, o.startx, o.starty, o.endx, o.endy), (0x0d, 1, 2, 3, 4));
        assert_eq!((o.bgcolour, o.opcode, o.pen_style, o.pen_width, o.pen_colour), (0x090909, 0x0d, 0, 1, 0x332211));
    }

    #[test]
    fn rect_colour_bytes() {
        let mut o = parsed::<Rect>(&[10, 0, 20, 0, 30, 0, 40, 0, 0x11, 0x22, 0x33], |o, r| o.parse(r, 0x7f, false));
        assert_eq!(o, Rect { x: 10, y: 20, cx: 30, cy: 40, colour: 0x332211 });
        // Just the middle byte, and a delta x.
        o.parse(&mut Reader::new(&[0xff, 0x99]), 0x21, true).unwrap();
        assert_eq!((o.x, o.colour), (9, 0x339911));
    }

    #[test]
    fn desksave() {
        let o = parsed::<DeskSave>(&[0x10, 0, 0, 0, 1, 0, 2, 0, 3, 0, 4, 0, 1], |o, r| o.parse(r, 0x3f, false));
        assert_eq!(o, DeskSave { offset: 16, left: 1, top: 2, right: 3, bottom: 4, action: 1 });
    }

    #[test]
    fn memblt_and_triblt() {
        let b = [2, 0, 1, 0, 2, 0, 3, 0, 4, 0, 0xcc, 5, 0, 6, 0, 0x34, 0x12];
        let o = parsed::<MemBlt>(&b, |o, r| o.parse(r, 0x1ff, false));
        assert_eq!(o, MemBlt { colour_table: 0, cache_id: 2, x: 1, y: 2, cx: 3, cy: 4, opcode: 0xcc, srcx: 5, srcy: 6, cache_idx: 0x1234 });
        let mut b = b[..15].to_vec();
        b.extend([1, 2, 3, 4, 5, 6, 0, 0, 0, 0xaa, 0x34, 0x12, 9, 0]);
        let o = parsed::<TriBlt>(&b, |o, r| o.parse(r, 0x1_bfff, false));
        assert_eq!((o.mem.cache_id, o.mem.cache_idx, o.mem.opcode), (2, 0x1234, 0xcc));
        assert_eq!((o.bgcolour, o.fgcolour, o.brush.pattern[0], o.unknown), (0x030201, 0x060504, 0xaa, 9));
    }

    #[test]
    fn polygon_polyline_ellipse_fields() {
        let o = parsed::<Polygon>(&[1, 0, 2, 0, 0x0d, 1, 0xff, 0, 0, 3, 2, 0xab, 0xcd], |o, r| o.parse(r, 0x7f, false));
        assert_eq!((o.x, o.y, o.opcode, o.fillmode, o.fgcolour, o.npoints, o.data.as_slice()), (1, 2, 0x0d, 1, 0xff, 3, &[0xab, 0xcd][..]));
        let o = parsed::<Polygon2>(&[1, 0, 2, 0, 0x0d, 2, 1, 1, 1, 2, 2, 2, 0, 0, 0, 0x77, 4, 1, 9], |o, r| o.parse(r, 0x1fff & !0x0400, false));
        assert_eq!((o.fillmode, o.bgcolour, o.fgcolour, o.brush.pattern[0], o.npoints, o.data.as_slice()), (2, 0x010101, 0x020202, 0x77, 4, &[9][..]));
        let o = parsed::<Polyline>(&[5, 0, 6, 0, 0x0d, 1, 2, 3, 2, 0], |o, r| o.parse(r, 0x77, false));
        assert_eq!((o.x, o.y, o.opcode, o.fgcolour, o.lines, o.data.as_slice()), (5, 6, 0x0d, 0x030201, 2, &[][..]));
        let mut o = Ellipse::default();
        o.parse(&mut Reader::new(&[1, 0, 2, 0, 3, 0, 4, 0, 0x0d, 1, 7, 8, 9]), 0x7f, false, false).unwrap();
        assert_eq!((o.left, o.bottom, o.fillmode, o.fgcolour), (1, 4, 1, 0x090807));
        let mut o = Ellipse::default();
        o.parse(&mut Reader::new(&[0x0d, 1, 1, 1, 1, 2, 2, 2, 0x81]), 0x4f0, false, true).unwrap();
        assert_eq!((o.opcode, o.bgcolour, o.fgcolour, o.brush.style), (0x0d, 0x010101, 0x020202, 0x81));
    }

    #[test]
    fn text2_fields() {
        let mut b = vec![3, 0x20, 0x0d, 1, 1, 2, 3, 4, 5, 6];
        for v in 1..=8i16 {
            b.extend(v.to_le_bytes());
        }
        b.extend([100, 0, 200, 0, 2, 0x41, 0x42]);
        let present = 0x3fffff & !(0x1f << 14);
        let o = parsed::<Text2>(&b, |o, r| o.parse(r, present));
        assert_eq!((o.font, o.flags, o.opcode, o.mixmode, o.fgcolour, o.bgcolour), (3, 0x20, 0x0d, 1, 0x030201, 0x060504));
        assert_eq!((o.clipleft, o.clipbottom, o.boxleft, o.boxbottom), (1, 4, 5, 8));
        assert_eq!((o.x, o.y, o.text.as_slice()), (100, 200, &b"AB"[..]));
    }

    #[test]
    fn ninegrid_fields() {
        let o = parsed::<DrawNineGrid>(&[1, 0, 2, 0, 3, 0, 4, 0, 5, 0], |o, r| o.parse(r, 0x1f, false));
        assert_eq!(o, DrawNineGrid { src_left: 1, src_top: 2, src_right: 3, src_bottom: 4, bitmap_id: 5 });
        let o = parsed::<MultiDrawNineGrid>(&[5, 0, 2, 2, 0, 0x30, 0x41], |o, r| o.parse(r, 0x70, false));
        assert_eq!((o.grid.bitmap_id, o.n_delta_entries, o.data.as_slice()), (5, 2, &[0x30, 0x41][..]));
    }

    #[test]
    fn bounds_absolute_and_delta() {
        let mut b = Bounds::default();
        read_bounds(&mut Reader::new(&[0x0f, 1, 0, 2, 0, 3, 0, 4, 0]), &mut b).unwrap();
        assert_eq!(b, Bounds { left: 1, top: 2, right: 3, bottom: 4 });
        read_bounds(&mut Reader::new(&[0x90, 0xff, 0x10]), &mut b).unwrap();
        assert_eq!(b, Bounds { left: 0, top: 2, right: 3, bottom: 20 });
    }

    #[test]
    fn delta_point_lists() {
        // Two points: (+3, -2) then (x unchanged, +0x0123).
        let data = [0b0010_0000, 0x03, 0x7e, 0x81, 0x23];
        assert_eq!(delta_points(10, 20, 2, &data).unwrap(), [(10, 20), (13, 18), (13, 20 - 2 + 0x123)]);
        // Two-byte negative: 0xc0 0x00 is -16384.
        assert_eq!(delta_points(0, 0, 1, &[0b0100_0000, 0xc0, 0x00]).unwrap(), [(0, 0), (-16384, 0)]);
        // Too little data for the points promised.
        assert!(delta_points(0, 0, 5, &[0, 1]).is_none());
        assert_eq!(delta_points(1, 2, 0, &[]).unwrap(), [(1, 2)]);
    }

    #[test]
    fn delta_rect_lists() {
        // Two rects. First: all four fields given; second: only width.
        let data = [0x0d, 0x0a, 0x01, 0x42, 0x80, 0x10, 0x05, 0x01];
        let rects = delta_rects(&data[..], 2).unwrap();
        assert_eq!(rects[0], (0x0a, 0x01, -2, 0x10));
        let data2 = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let rects = delta_rects(&data2, 2).unwrap();
        assert_eq!(rects, [(1, 2, 3, 4), (6, 8, 10, 12)]);
        assert!(delta_rects(&[], 1).is_none());
        assert!(delta_rects(&[0; 40], 46).is_none());
    }

    #[test]
    fn flipping_rows() {
        assert_eq!(flip_rows(&[1, 2, 3, 4, 5, 6], 2, 3), [5, 6, 3, 4, 1, 2]);
        assert_eq!(flip_rows(&[1, 2, 3], 2, 2), [3, 0, 1, 2]);
    }

    #[test]
    fn compressed_brush() {
        // Bottom row: pixels 0,1,2,3,0,1,2,3; other rows all 0. 1 byte per pixel.
        let mut input = vec![0u8; 16];
        input[0] = 0b0001_1011;
        input[1] = 0b0001_1011;
        input.extend([10, 11, 12, 13]);
        let out = expand_brush(&input, 1);
        assert_eq!(&out[56..64], &[10, 11, 12, 13, 10, 11, 12, 13]);
        assert!(out[..56].iter().all(|&p| p == 10));
    }
}
