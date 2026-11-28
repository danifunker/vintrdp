//! The RENDER extension, 0.11 as X.org's Xvfb offers it: picture formats
//! for every visual and depth, pictures on windows and pixmaps (and solid
//! fills and gradients), Composite with every operator, trapezoids and
//! triangles, glyph sets, ARGB and animated cursors. The pixels are
//! pixman's (src/pixman); what X.org's render/, mi and fb do around it is
//! here: picture attributes and their errors, the composite clip, the
//! operator reductions, the glyph mask.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::cmap::{Class, Cmap};
use crate::cursor::Cursor;
use crate::draw::{depth_mask, PixmapRef};
use crate::gc::{Drawable, Gc};
use crate::pixman::{self, op, trap, Bits, Dest, DestAlpha, Filter, Format, Image, Indexed, Kind, Px, Repeat, Stop, Transform};
use crate::region::{Box, Region};
use crate::server::*;
use crate::wire::{self, Reader};

/// The first error code: X.org gives the first extension with errors 128.
pub const ERROR_BASE: u8 = 128;
const BAD_PICT_FORMAT: u8 = ERROR_BASE;
const BAD_PICTURE: u8 = ERROR_BASE + 1;
const BAD_GLYPH_SET: u8 = ERROR_BASE + 3;
const BAD_GLYPH: u8 = ERROR_BASE + 4;

/// Picture format and colormap ids, in the server's range.
const FORMAT_ID: u32 = 0x200;
const FORMAT_CMAP_ID: u32 = 0x280;

/// A picture format, as QueryPictFormats tells it.
pub struct PictFormat {
    pub id: u32,
    pub depth: u8,
    pub direct: bool,
    /// Red, red mask, green, green mask, blue, blue mask, alpha, alpha mask.
    pub d: [u16; 8],
    /// pixman's code for it, bits per pixel 0 as X.org keeps it.
    pub code: Format,
    /// Indexed: its visual and colormap, and what the pixels are.
    pub visual: u32,
    pub colormap: u32,
    pub table: Option<Rc<Indexed>>,
    pub values: Vec<(u32, u16, u16, u16)>,
}

impl PictFormat {
    /// The code with the bits per pixel of a drawable of its depth.
    pub fn code_bpp(&self) -> Format {
        self.code.with_bpp(crate::render::bpp(self.depth).unwrap_or(32) as u32)
    }
}

#[derive(Default)]
pub struct Render {
    pub formats: Vec<PictFormat>,
    /// Each client's QueryVersion.
    pub versions: HashMap<u32, (u32, u32)>,
    /// The animated cursor being shown: its frame and when the next is.
    pub anim: Option<(Rc<Cursor>, usize, Instant)>,
}

#[derive(Clone)]
pub enum Source {
    /// Red, green, blue, alpha.
    Solid([u16; 4]),
    Linear((i32, i32), (i32, i32), Vec<Stop>),
    Radial((i32, i32, i32), (i32, i32, i32), Vec<Stop>),
    Conical((i32, i32), i32, Vec<Stop>),
}

#[derive(Clone)]
pub enum PicDrawable {
    Window(u32),
    Pixmap(PixmapRef),
}

pub struct Picture {
    pub drawable: Option<PicDrawable>,
    /// Index into the formats; None for the source pictures.
    pub format: Option<usize>,
    pub source: Option<Source>,
    pub repeat: u8,
    pub alpha_map: Option<PicRef>,
    pub alpha_origin: (i32, i32),
    pub clip_origin: (i32, i32),
    /// The client's clip, relative to the clip origin.
    pub clip: Option<Region>,
    pub graphics_exposures: bool,
    pub subwindow_mode: u8,
    pub poly_edge: u8,
    pub poly_mode: u8,
    pub component_alpha: bool,
    pub transform: Option<Transform>,
    /// PictFilterNearest (0), Bilinear (1) or Convolution (5).
    pub filter: u8,
    pub params: Vec<i32>,
}

pub type PicRef = Rc<RefCell<Picture>>;

impl Picture {
    fn new(drawable: Option<PicDrawable>, format: Option<usize>, source: Option<Source>) -> Picture {
        Picture {
            drawable,
            format,
            source,
            repeat: 0,
            alpha_map: None,
            alpha_origin: (0, 0),
            clip_origin: (0, 0),
            clip: None,
            graphics_exposures: false,
            subwindow_mode: 0,
            poly_edge: 0,
            poly_mode: 0,
            component_alpha: false,
            transform: None,
            filter: 0,
            params: Vec::new(),
        }
    }
}

pub struct Glyph {
    pub width: u16,
    pub height: u16,
    pub x: i16,
    pub y: i16,
    pub x_off: i16,
    pub y_off: i16,
    /// The glyph's picture: its format's pixels, as pixman's glyph cache
    /// keeps them.
    pub px: Vec<u32>,
}

pub struct GlyphSet {
    pub format: usize,
    pub glyphs: HashMap<u32, Rc<Glyph>>,
}

pub type GlyphSetRef = Rc<RefCell<GlyphSet>>;

/// Where an image's pixels are kept.
#[derive(Clone)]
enum Store {
    Pixmap(PixmapRef),
    /// A window's buffer: the screen (0) or a backing pixmap.
    Buffer(u32),
    /// A scratch buffer of the caller's.
    Scratch,
}

impl Store {
    fn same(&self, o: &Store) -> bool {
        match (self, o) {
            (Store::Pixmap(a), Store::Pixmap(b)) => Rc::ptr_eq(a, b),
            (Store::Buffer(a), Store::Buffer(b)) => a == b,
            (Store::Scratch, Store::Scratch) => true,
            _ => false,
        }
    }
}

#[derive(Clone)]
struct BitsSpec {
    store: Store,
    stride: usize,
    width: i32,
    height: i32,
    format: Format,
    indexed: Option<Rc<Indexed>>,
}

enum KindSpec {
    Bits(BitsSpec),
    Solid([u16; 4]),
    Linear((i32, i32), (i32, i32), Vec<Stop>),
    Radial((i32, i32, i32), (i32, i32, i32), Vec<Stop>),
    Conical((i32, i32), i32, Vec<Stop>),
    /// A glyph's pixels.
    Glyph(Rc<Glyph>, Format),
}

struct ImageSpec {
    kind: KindSpec,
    transform: Option<Transform>,
    filter: Filter,
    repeat: Repeat,
    alpha: Option<(BitsSpec, i32, i32)>,
    ca: bool,
    /// Where the picture's origin is in its pixels.
    xoff: i32,
    yoff: i32,
}

struct DestSpec {
    bits: BitsSpec,
    clip: Region,
    xoff: i32,
    yoff: i32,
    /// The screen position of the buffer, for the damage.
    origin: (i32, i32),
    alpha: Option<(BitsSpec, i32, i32)>,
    saved: Vec<(u32, Region)>,
    repeat: Repeat,
    convolution: bool,
    ca: bool,
}

/// A source's pixels, held for the length of an operation.
enum Hold<'p> {
    Ref(std::cell::Ref<'p, crate::draw::Pixmap>),
    Own(Vec<u32>),
    Dest,
    None,
}

fn repeat_of(r: u8) -> Repeat {
    match r {
        1 => Repeat::Normal,
        2 => Repeat::Pad,
        3 => Repeat::Reflect,
        _ => Repeat::None,
    }
}

fn ones(m: u32) -> u32 {
    m.count_ones()
}

fn mask_bits(n: u32) -> u16 {
    ((1u32 << n) - 1) as u16
}

/// A picture of the format on a drawable of the depth, or BadMatch.
fn err(code: u8, v: u32) -> XError {
    XError(code, v)
}

impl State {
    // -----------------------------------------------------------------
    // Formats.

    /// PictureCreateDefaultFormats and PictureInit, for the screen's
    /// visuals and depths, and miInitIndexed for the indexed ones.
    pub fn render_init(&mut self, default_pixels: Vec<u32>) {
        let bpp = |d: u8| crate::render::bpp(d).unwrap_or(32) as u32;
        let mut list: Vec<(Format, u8, Option<u32>)> = Vec::new();
        let add = |list: &mut Vec<(Format, u8, Option<u32>)>, f: Format, d: u8, v: Option<u32>| {
            if !list.iter().any(|e| e.0 == f && e.1 == d && e.2 == v) {
                list.push((f, d, v));
            }
        };
        use pixman::{TYPE_A, TYPE_ABGR, TYPE_ARGB, TYPE_BGRA, TYPE_COLOR, TYPE_GRAY};
        for (f, d) in [(Format::A1, 1), (Format::A8, 8), (Format::A8R8G8B8, 32), (Format::X8R8G8B8, 32), (Format::new(32, TYPE_BGRA, 8, 8, 8, 8), 32), (Format::new(32, TYPE_BGRA, 0, 8, 8, 8), 32)] {
            add(&mut list, f, d, None);
        }
        for (i, v) in self.visuals.clone().iter().enumerate() {
            let d = v.depth;
            let vv = v.v;
            match vv.class {
                Class::TrueColor | Class::DirectColor => {
                    let (r, g, b) = (ones(vv.red_mask), ones(vv.green_mask), ones(vv.blue_mask));
                    let (or, og, ob) = (vv.red_mask.trailing_zeros(), vv.green_mask.trailing_zeros(), vv.blue_mask.trailing_zeros());
                    let ty = if ob == 0 && og == b && or == b + g {
                        TYPE_ARGB
                    } else if or == 0 && og == r && ob == r + g {
                        TYPE_ABGR
                    } else if or == og.wrapping_sub(r) && og == ob.wrapping_sub(g) && ob == bpp(d) - b {
                        TYPE_BGRA
                    } else {
                        continue;
                    };
                    add(&mut list, Format::new(bpp(d), ty, 0, r, g, b), d, None);
                }
                Class::StaticColor | Class::PseudoColor => add(&mut list, Format::new(bpp(d), TYPE_COLOR, 0, 0, 0, 0), d, Some(i as u32)),
                Class::StaticGray | Class::GrayScale => add(&mut list, Format::new(bpp(d), TYPE_GRAY, 0, 0, 0, 0), d, Some(i as u32)),
            }
        }
        for d in self.screen_depths() {
            match bpp(d) {
                16 => {
                    if d >= 12 {
                        add(&mut list, Format::new(16, TYPE_ARGB, 0, 4, 4, 4), d, None);
                        add(&mut list, Format::new(16, TYPE_ABGR, 0, 4, 4, 4), d, None);
                    }
                    if d >= 15 {
                        add(&mut list, Format::new(16, TYPE_ARGB, 0, 5, 5, 5), d, None);
                        add(&mut list, Format::new(16, TYPE_ABGR, 0, 5, 5, 5), d, None);
                    }
                    if d >= 16 {
                        for f in [Format::new(16, TYPE_ARGB, 1, 5, 5, 5), Format::new(16, TYPE_ABGR, 1, 5, 5, 5), Format::new(16, TYPE_ARGB, 0, 5, 6, 5), Format::new(16, TYPE_ABGR, 0, 5, 6, 5), Format::new(16, TYPE_ARGB, 4, 4, 4, 4), Format::new(16, TYPE_ABGR, 4, 4, 4, 4)] {
                            add(&mut list, f, d, None);
                        }
                    }
                }
                32 => {
                    if d >= 24 {
                        add(&mut list, Format::X8R8G8B8, d, None);
                        add(&mut list, Format::new(32, TYPE_ABGR, 0, 8, 8, 8), d, None);
                    }
                    if d >= 30 {
                        for f in [Format::new(32, TYPE_ARGB, 2, 10, 10, 10), Format::new(32, TYPE_ARGB, 0, 10, 10, 10), Format::new(32, TYPE_ABGR, 2, 10, 10, 10), Format::new(32, TYPE_ABGR, 0, 10, 10, 10)] {
                            add(&mut list, f, d, None);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut formats = Vec::new();
        for (n, (f, depth, vis)) in list.into_iter().enumerate() {
            let (a, r, g, b) = (f.a(), f.r(), f.g(), f.b());
            let mut d = [0u16; 8];
            match f.ty() {
                TYPE_ARGB => {
                    d = [(g + b) as u16, mask_bits(r), b as u16, mask_bits(g), 0, mask_bits(b), if a != 0 { (r + g + b) as u16 } else { 0 }, mask_bits(a)];
                }
                TYPE_ABGR => {
                    d = [0, mask_bits(r), r as u16, mask_bits(g), (g + r) as u16, mask_bits(b), if a != 0 { (r + g + b) as u16 } else { 0 }, mask_bits(a)];
                }
                TYPE_BGRA => {
                    let bp = f.bpp();
                    d = [(bp - b - g - r) as u16, mask_bits(r), (bp - b - g) as u16, mask_bits(g), (bp - b) as u16, mask_bits(b), 0, mask_bits(a)];
                }
                TYPE_A => d[7] = mask_bits(a),
                _ => {}
            }
            let id = FORMAT_ID + n as u32;
            let mut pf = PictFormat { id, depth, direct: vis.is_none(), d, code: Format(f.0 & 0xff_ffff), visual: 0, colormap: 0, table: None, values: Vec::new() };
            if let Some(vi) = vis {
                let v = self.visuals[vi as usize];
                pf.visual = v.id;
                let ty = if matches!(v.v.class, Class::StaticColor | Class::PseudoColor) { TYPE_COLOR } else { TYPE_GRAY };
                pf.code = Format::new(0, ty, 0, 0, 0, 0);
                self.init_indexed(&mut pf, vi as usize, n, &default_pixels);
            } else {
                // PictureInit's type, from the masks.
                let ty = if d[1] | d[3] | d[5] == 0 {
                    TYPE_A
                } else if d[0] > d[4] {
                    TYPE_ARGB
                } else if d[0] == 0 {
                    TYPE_ABGR
                } else {
                    TYPE_BGRA
                };
                pf.code = Format::new(0, ty, ones(d[7] as u32), ones(d[1] as u32), ones(d[3] as u32), ones(d[5] as u32));
            }
            formats.push(pf);
        }
        self.render.formats = formats;
    }

    /// The screen's depths, the root's first, as the setup lists them.
    pub fn screen_depths(&self) -> Vec<u8> {
        std::iter::once(self.depth).chain(DEPTHS.iter().copied().filter(|&d| d != self.depth)).collect()
    }

    /// miInitIndexed: the format's pixels and the colour each 15-bit
    /// colour (or gray) goes to. The root's visual uses the default map
    /// (and the cube RENDER allocated in it); the others a map of their own.
    fn init_indexed(&mut self, pf: &mut PictFormat, vi: usize, n: usize, default_pixels: &[u32]) {
        let v = self.visuals[vi];
        let (pixels, rgb): (Vec<u32>, Vec<(u16, u16, u16)>) = if vi == 0 {
            pf.colormap = COLORMAP;
            let pixels = if v.v.class.dynamic() { default_pixels.to_vec() } else { (0..v.v.entries as u32).collect() };
            let Ok(c) = self.cmap(COLORMAP) else { return };
            let rgb = c.map.query_colors(&pixels).unwrap_or_default();
            (pixels, rgb)
        } else {
            pf.colormap = FORMAT_CMAP_ID + n as u32;
            let Ok(mut map) = Cmap::new(v.v, false, 0) else { return };
            let pixels: Vec<u32> = if v.v.class.dynamic() {
                // PictureCmapPolicyAll: no cube but on PseudoColor; grays.
                let entries = v.v.entries as u32;
                let cube = if v.v.class == Class::PseudoColor {
                    let mut c = 1;
                    while c * c * c < entries {
                        c += 1;
                    }
                    c -= 1;
                    if c == 1 { 0 } else { c }
                } else {
                    0
                };
                let gray = if cube != 0 { ((entries - cube * cube * cube) / (cube - 1) + 1) * (cube - 1) + 1 } else { entries };
                let level = |i: u32, n: u32| ((i * 65535 + (n - 1) / 2) / (n - 1)) as u16;
                let mut used = vec![false; entries as usize];
                for r in 0..cube {
                    for g in 0..cube {
                        for b in 0..cube {
                            if let Ok((p, ..)) = map.alloc_color(0, level(r, cube), level(g, cube), level(b, cube), 0) {
                                used[p as usize] = true;
                            }
                        }
                    }
                }
                for g in 0..gray {
                    let l = level(g, gray);
                    if let Ok((p, ..)) = map.alloc_color(0, l, l, l, 0) {
                        used[p as usize] = true;
                    }
                }
                (0..entries).filter(|&p| used[p as usize]).collect()
            } else {
                (0..v.v.entries as u32).collect()
            };
            let rgb = map.query_colors(&pixels).unwrap_or_default();
            // A colormap of the server's, as X.org's CreateColormap makes it.
            self.resources.insert(pf.colormap, Resource::Colormap(crate::color::Colormap { window: ROOT, visual: v.id, map }));
            (pixels, rgb)
        };
        let mut ix = Indexed { color: false, rgba: [0; 256], ent: vec![0; 32768] };
        for (&p, &(r, g, b)) in pixels.iter().zip(&rgb) {
            pf.values.push((p, r, g, b));
            ix.rgba[(p & 0xff) as usize] = 0xff00_0000 | ((r as u32 & 0xff00) << 8) | (g as u32 & 0xff00) | ((b as u32 & 0xff00) >> 8);
        }
        if pixels.is_empty() {
            pf.table = Some(Rc::new(ix));
            return;
        }
        if matches!(v.v.class, Class::GrayScale | Class::StaticGray) {
            for r in 0..32768u32 {
                let mut best = (pixels[0], 1i32 << 30);
                for &p in &pixels {
                    let c = (ix.rgba[p as usize] & 0xff) as i32;
                    let dr = r as i32 - ((c | c << 8) >> 1);
                    if dr * dr < best.1 {
                        best = (p, dr * dr);
                    }
                }
                ix.ent[r as usize] = best.0 as u8;
            }
        } else {
            ix.color = true;
            let mut i = 0;
            for r in 0..32 {
                for g in 0..32 {
                    for b in 0..32 {
                        let mut best = (pixels[0], 1i32 << 30);
                        for &p in &pixels {
                            let c = ix.rgba[p as usize];
                            let (dr, dg, db) = ((c >> 19 & 0x1f) as i32 - r, (c >> 11 & 0x1f) as i32 - g, (c >> 3 & 0x1f) as i32 - b);
                            let dist = dr * dr + dg * dg + db * db;
                            if dist < best.1 {
                                best = (p, dist);
                            }
                        }
                        ix.ent[i] = best.0 as u8;
                        i += 1;
                    }
                }
            }
        }
        pf.table = Some(Rc::new(ix));
    }

    /// miUpdateIndexed: StoreColors in an indexed format's map changes
    /// what its pixels are (but not which pixel a colour goes to).
    pub fn render_store_colors(&mut self, cmap: u32, items: &[(u32, u16, u16, u16, u8)]) {
        let Some(class) = self.cmap(cmap).ok().map(|c| c.map.visual().class) else { return };
        if !matches!(class, Class::PseudoColor | Class::GrayScale) {
            return;
        }
        let colors: Vec<(u32, u32)> = items.iter().filter_map(|&(p, ..)| self.cmap(cmap).ok().and_then(|c| c.map.query_colors(&[p]).ok()).map(|v| (p, v[0]))).map(|(p, (r, g, b))| (p, 0xff00_0000 | ((r as u32 & 0xff00) << 8) | (g as u32 & 0xff00) | ((b as u32 & 0xff00) >> 8))).collect();
        for f in &mut self.render.formats {
            if f.direct || f.colormap != cmap {
                continue;
            }
            if let Some(t) = &mut f.table {
                let t = Rc::make_mut(t);
                for &(p, c) in &colors {
                    t.rgba[(p & 0xff) as usize] = c;
                }
            }
            break;
        }
    }

    fn pict_format(&self, id: u32) -> Result<usize, XError> {
        self.render.formats.iter().position(|f| f.id == id).ok_or(err(BAD_PICT_FORMAT, id))
    }

    /// PictureMatchVisual.
    fn visual_format(&self, depth: u8, visual: u32) -> Option<usize> {
        let v = self.visual_def(visual)?;
        let indexed = !matches!(v.v.class, Class::TrueColor | Class::DirectColor);
        self.render.formats.iter().position(|f| {
            f.depth == depth
                && f.direct != indexed
                && if indexed { f.visual == visual } else { (f.d[1] as u32) << f.d[0] == v.v.red_mask && (f.d[3] as u32) << f.d[2] == v.v.green_mask && (f.d[5] as u32) << f.d[4] == v.v.blue_mask }
        })
    }

    pub fn picture(&self, id: u32) -> Result<PicRef, XError> {
        match self.resources.get(&id) {
            Some(Resource::Picture(p)) => Ok(p.clone()),
            _ => Err(err(BAD_PICTURE, id)),
        }
    }

    fn glyph_set(&self, id: u32) -> Result<GlyphSetRef, XError> {
        match self.resources.get(&id) {
            Some(Resource::GlyphSet(g)) => Ok(g.clone()),
            _ => Err(err(BAD_GLYPH_SET, id)),
        }
    }

    /// PictureDestroyWindow: a window's pictures go with it.
    pub fn render_window_gone(&mut self, w: u32) {
        let ids: Vec<u32> = self.resources.iter().filter(|(_, r)| matches!(r, Resource::Picture(p) if matches!(p.borrow().drawable, Some(PicDrawable::Window(x)) if x == w))).map(|(&id, _)| id).collect();
        for id in ids {
            self.resources.remove(&id);
        }
    }

    // -----------------------------------------------------------------
    // Requests.

    pub fn render(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let exact = |r: &Reader, n: usize| if r.remaining() == n { Ok(()) } else { Err(XError(BAD_LENGTH, 0)) };
        match ctx.data {
            0 => {
                exact(r, 8)?;
                let (major, minor) = (r.u32()?, r.u32()?);
                self.render.versions.insert(ctx.slot, (major, minor));
                let (ma, mi) = if major.wrapping_mul(1000).wrapping_add(minor) < 11 { (major, minor) } else { (0, 11) };
                let mut reply = ctx.reply(0);
                reply.u32(ma).u32(mi);
                Ok(Some(reply.finish()))
            }
            1 => {
                exact(r, 0)?;
                Ok(Some(self.query_pict_formats(ctx)))
            }
            2 => {
                let id = r.u32()?;
                let f = &self.render.formats[self.pict_format(id)?];
                if f.direct {
                    return Err(XError(BAD_MATCH, id));
                }
                let mut reply = ctx.reply(0);
                reply.u32(f.values.len() as u32).zeros(20);
                for &(p, red, g, b) in &f.values {
                    reply.u32(p).u16(red).u16(g).u16(b).u16(0xffff);
                }
                Ok(Some(reply.finish()))
            }
            4 => self.create_picture(ctx, r),
            5 => {
                let p = self.picture(r.u32()?)?;
                let mask = r.u32()?;
                if ones(mask) as usize * 4 != r.remaining() {
                    return Err(XError(BAD_LENGTH, 0));
                }
                self.change_picture(ctx, &p, mask, r).map(|_| None)
            }
            6 => {
                let p = self.picture(r.u32()?)?;
                if p.borrow().drawable.is_none() {
                    return Err(err(BAD_PICTURE, 0));
                }
                let (x, y) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
                if r.remaining() & 4 != 0 {
                    return Err(XError(BAD_LENGTH, 0));
                }
                let mut boxes = Vec::new();
                while r.remaining() >= 8 {
                    let (rx, ry, w, h) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32, r.u16()? as i32, r.u16()? as i32);
                    boxes.push(Box::new(rx, ry, w, h));
                }
                let mut p = p.borrow_mut();
                p.clip = Some(Region::from_boxes(&boxes));
                p.clip_origin = (x, y);
                Ok(None)
            }
            7 => {
                exact(r, 4)?;
                let id = r.u32()?;
                self.picture(id)?;
                self.resources.remove(&id);
                Ok(None)
            }
            8 => self.composite_request(r),
            10..=13 => self.shapes(ctx.data, r),
            17 => {
                exact(r, 8)?;
                let id = self.new_id(ctx, r.u32()?)?;
                let fid = r.u32()?;
                let f = self.pict_format(fid)?;
                let pf = &self.render.formats[f];
                if !matches!(pf.depth, 1 | 4 | 8 | 16 | 32) || !pf.direct {
                    return Err(XError(BAD_MATCH, 0));
                }
                self.resources.insert(id, Resource::GlyphSet(Rc::new(RefCell::new(GlyphSet { format: f, glyphs: HashMap::new() }))));
                Ok(None)
            }
            18 => {
                exact(r, 8)?;
                let id = self.new_id(ctx, r.u32()?)?;
                let g = self.glyph_set(r.u32()?)?;
                self.resources.insert(id, Resource::GlyphSet(g));
                Ok(None)
            }
            19 => {
                exact(r, 4)?;
                let id = r.u32()?;
                self.glyph_set(id)?;
                self.resources.remove(&id);
                Ok(None)
            }
            20 => self.add_glyphs(r),
            22 => {
                let gs = self.glyph_set(r.u32()?)?;
                let mut gs = gs.borrow_mut();
                while r.remaining() >= 4 {
                    let g = r.u32()?;
                    if gs.glyphs.remove(&g).is_none() {
                        return Err(err(BAD_GLYPH, g));
                    }
                }
                Ok(None)
            }
            23..=25 => self.composite_glyphs(ctx, r),
            26 => self.fill_rectangles(r),
            27 => self.render_cursor(ctx, r),
            28 => {
                exact(r, 40)?;
                let p = self.picture(r.u32()?)?;
                let mut t = [[0i32; 3]; 3];
                for row in &mut t {
                    for v in row.iter_mut() {
                        *v = r.u32()? as i32;
                    }
                }
                // transformIsIdentity: a uniform scale is none.
                let id = t[0][0] == t[1][1] && t[0][0] == t[2][2] && t[0][0] != 0 && t[0][1] == 0 && t[0][2] == 0 && t[1][0] == 0 && t[1][2] == 0 && t[2][0] == 0 && t[2][1] == 0;
                p.borrow_mut().transform = (!id).then_some(t);
                Ok(None)
            }
            29 => {
                exact(r, 4)?;
                let d = r.u32()?;
                match self.drawable(d)? {
                    Drawable::Window(w) if self.windows[&w].input_only() => return Err(XError(BAD_MATCH, d)),
                    _ => {}
                }
                let names: [&[u8]; 6] = [b"nearest", b"bilinear", b"convolution", b"fast", b"good", b"best"];
                let aliases: [u16; 6] = [0xffff, 0xffff, 0xffff, 0, 1, 1];
                let mut reply = ctx.reply(0);
                reply.u32(6).u32(6).zeros(16);
                for a in aliases {
                    reply.u16(a);
                }
                for n in names {
                    reply.u8(n.len() as u8).bytes(n);
                }
                Ok(Some(reply.finish()))
            }
            30 => {
                let p = self.picture(r.u32()?)?;
                let n = r.u16()? as usize;
                r.skip(2)?;
                let name = r.bytes(n)?.to_ascii_lowercase();
                r.skip(wire::pad(n)).map_err(|_| XError(BAD_LENGTH, 0))?;
                let mut params = Vec::new();
                while r.remaining() >= 4 {
                    params.push(r.u32()? as i32);
                }
                let filter = match &name[..] {
                    b"nearest" | b"fast" => 0,
                    b"bilinear" | b"good" | b"best" => 1,
                    b"convolution" => 5,
                    _ => return Err(XError(BAD_NAME, 0)),
                };
                if filter == 5 {
                    if params.len() < 3 || params[0] & 0xffff != 0 || params[1] & 0xffff != 0 || (params[0] >> 16) as i64 * (params[1] >> 16) as i64 > params.len() as i64 - 2 {
                        return Err(XError(BAD_MATCH, 0));
                    }
                } else if !params.is_empty() {
                    return Err(XError(BAD_MATCH, 0));
                }
                let mut p = p.borrow_mut();
                p.filter = filter;
                p.params = params;
                Ok(None)
            }
            31 => self.create_anim_cursor(ctx, r),
            32 => self.add_traps(r),
            33..=36 => self.create_source(ctx, r),
            3 | 9 | 14 | 15 | 16 | 21 => Err(XError(BAD_IMPLEMENTATION, 0)),
            _ => Err(XError(BAD_REQUEST, 0)),
        }
    }

    fn query_pict_formats(&self, ctx: &Ctx) -> Vec<u8> {
        let version = self.render.versions.get(&ctx.slot).copied().unwrap_or((0, 0));
        let subpixel = !(version.0 == 0 && version.1 < 6);
        let depths = self.screen_depths();
        let visuals: Vec<(u8, Vec<(u32, u32)>)> = depths
            .iter()
            .map(|&d| (d, self.visuals.iter().filter(|v| v.depth == d).filter_map(|v| self.visual_format(d, v.id).map(|f| (v.id, self.render.formats[f].id))).collect()))
            .collect();
        let nvisual: usize = visuals.iter().map(|v| v.1.len()).sum();
        let mut reply = ctx.reply(0);
        reply.u32(self.render.formats.len() as u32).u32(1).u32(depths.len() as u32).u32(nvisual as u32).u32(subpixel as u32).zeros(4);
        for f in &self.render.formats {
            reply.u32(f.id).u8(f.direct as u8).u8(f.depth).zeros(2);
            for v in f.d {
                reply.u16(v);
            }
            reply.u32(if f.direct { 0 } else { f.colormap });
        }
        reply.u32(depths.len() as u32).u32(self.render.formats.first().map_or(0, |f| f.id));
        for (d, vs) in &visuals {
            reply.u8(*d).u8(0).u16(vs.len() as u16).zeros(4);
            for &(v, f) in vs {
                reply.u32(v).u32(f);
            }
        }
        if subpixel {
            reply.u32(0);
        }
        reply.finish()
    }

    fn create_picture(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.new_id(ctx, r.u32()?)?;
        let did = r.u32()?;
        let d = self.drawable(did)?;
        if let Drawable::Window(w) = &d {
            if self.windows[w].input_only() {
                return Err(XError(BAD_MATCH, did));
            }
        }
        let f = self.pict_format(r.u32()?)?;
        if self.render.formats[f].depth != self.depth_of(&d) {
            return Err(XError(BAD_MATCH, 0));
        }
        let mask = r.u32()?;
        if ones(mask) as usize * 4 != r.remaining() {
            return Err(XError(BAD_LENGTH, 0));
        }
        let pd = match d {
            Drawable::Window(w) => PicDrawable::Window(w),
            Drawable::Pixmap(p) => PicDrawable::Pixmap(p),
        };
        let p = Rc::new(RefCell::new(Picture::new(Some(pd), Some(f), None)));
        self.change_picture(ctx, &p, mask, r)?;
        self.resources.insert(id, Resource::Picture(p));
        Ok(None)
    }

    /// ChangePicture: the values in order, up to the first bad one.
    fn change_picture(&mut self, _ctx: &Ctx, p: &PicRef, mask: u32, r: &mut Reader) -> Result<(), XError> {
        for bit in 0..32 {
            if mask >> bit & 1 == 0 {
                continue;
            }
            let v = r.u32()?;
            match bit {
                0 => {
                    if v > 3 {
                        return Err(XError(BAD_VALUE, v));
                    }
                    p.borrow_mut().repeat = v as u8;
                }
                1 => {
                    let a = if v == 0 {
                        None
                    } else {
                        let a = self.picture(v)?;
                        if !matches!(a.borrow().drawable, Some(PicDrawable::Pixmap(_))) {
                            return Err(XError(BAD_MATCH, v));
                        }
                        Some(a)
                    };
                    p.borrow_mut().alpha_map = a;
                }
                2 => p.borrow_mut().alpha_origin.0 = v as i16 as i32,
                3 => p.borrow_mut().alpha_origin.1 = v as i16 as i32,
                4 => p.borrow_mut().clip_origin.0 = v as i16 as i32,
                5 => p.borrow_mut().clip_origin.1 = v as i16 as i32,
                6 => {
                    if p.borrow().drawable.is_none() {
                        return Err(XError(BAD_DRAWABLE, 0));
                    }
                    let clip = if v == 0 {
                        None
                    } else {
                        let pm = self.pixmap(v)?;
                        let pm = pm.borrow();
                        if pm.depth != 1 {
                            return Err(XError(BAD_MATCH, 0));
                        }
                        Some(crate::gc::bitmap_region(&pm))
                    };
                    p.borrow_mut().clip = clip;
                }
                7 => {
                    if v > 1 {
                        return Err(XError(BAD_VALUE, v));
                    }
                    p.borrow_mut().graphics_exposures = v != 0;
                }
                8 => {
                    if v > 1 {
                        return Err(XError(BAD_VALUE, v));
                    }
                    p.borrow_mut().subwindow_mode = v as u8;
                }
                9 => {
                    if v > 1 {
                        return Err(XError(BAD_VALUE, v));
                    }
                    p.borrow_mut().poly_edge = v as u8;
                }
                10 => {
                    if v > 1 {
                        return Err(XError(BAD_VALUE, v));
                    }
                    p.borrow_mut().poly_mode = v as u8;
                }
                11 => {}
                12 => {
                    if v > 1 {
                        return Err(XError(BAD_VALUE, v));
                    }
                    p.borrow_mut().component_alpha = v != 0;
                }
                _ => return Err(XError(BAD_VALUE, mask)),
            }
        }
        Ok(())
    }

    fn create_source(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.new_id(ctx, r.u32()?)?;
        let fixed = |r: &mut Reader| -> Result<i32, XError> { Ok(r.u32()? as i32) };
        let source = if ctx.data == 33 {
            let c = [r.u16()?, r.u16()?, r.u16()?, r.u16()?];
            Source::Solid(c)
        } else {
            let head: Vec<i32> = match ctx.data {
                34 => (0..4).map(|_| fixed(r)).collect::<Result<_, _>>()?,
                35 => (0..6).map(|_| fixed(r)).collect::<Result<_, _>>()?,
                _ => (0..3).map(|_| fixed(r)).collect::<Result<_, _>>()?,
            };
            let n = r.u32()? as usize;
            if n.checked_mul(12) != Some(r.remaining()) {
                return Err(XError(BAD_LENGTH, 0));
            }
            if n == 0 {
                return Err(XError(BAD_VALUE, 0));
            }
            let xs: Vec<i32> = (0..n).map(|_| fixed(r)).collect::<Result<_, _>>()?;
            let mut stops = Vec::with_capacity(n);
            let mut last = -1;
            for &x in &xs {
                if x < last || x > 1 << 16 {
                    return Err(XError(BAD_VALUE, 0));
                }
                last = x;
                stops.push(Stop { x, color: [r.u16()?, r.u16()?, r.u16()?, r.u16()?] });
            }
            match ctx.data {
                34 => Source::Linear((head[0], head[1]), (head[2], head[3]), stops),
                35 => Source::Radial((head[0], head[1], head[4]), (head[2], head[3], head[5]), stops),
                _ => Source::Conical((head[0], head[1]), head[2], stops),
            }
        };
        self.resources.insert(id, Resource::Picture(Rc::new(RefCell::new(Picture::new(None, None, Some(source))))));
        Ok(None)
    }

    // -----------------------------------------------------------------
    // Pictures as pixman images.

    /// A picture's format code, with the drawable's bits per pixel.
    fn code_of(&self, p: &Picture) -> Format {
        p.format.map_or(Format::A8R8G8B8, |f| self.render.formats[f].code_bpp())
    }

    fn table_of(&self, p: &Picture) -> Option<Rc<Indexed>> {
        p.format.and_then(|f| self.render.formats[f].table.clone())
    }

    /// The bits of a picture's drawable, and where its origin is in them.
    fn bits_of(&self, p: &Picture) -> Option<(BitsSpec, i32, i32)> {
        let (format, indexed) = (self.code_of(p), self.table_of(p));
        match p.drawable.as_ref()? {
            PicDrawable::Pixmap(pm) => {
                let b = pm.borrow();
                Some((BitsSpec { store: Store::Pixmap(pm.clone()), stride: b.width as usize, width: b.width as i32, height: b.height as i32, format, indexed }, 0, 0))
            }
            PicDrawable::Window(w) => {
                let win = self.windows.get(w)?;
                let (_, stride, ox, oy, bounds) = self.buffer(win.buf);
                let e = bounds.extents();
                Some((BitsSpec { store: Store::Buffer(win.buf), stride, width: e.width(), height: e.height(), format, indexed }, win.ax - ox, win.ay - oy))
            }
        }
    }

    fn filter_of(p: &Picture) -> Filter {
        match p.filter {
            1 => Filter::Bilinear,
            5 => Filter::Convolution(p.params.clone()),
            _ => Filter::Nearest,
        }
    }

    /// image_from_pict for a source (or mask): no clip, and the drawable's
    /// offset folded into the transform if there is one.
    fn source_spec(&self, pr: &PicRef) -> Option<ImageSpec> {
        let p = pr.borrow();
        let (kind, mut xoff, mut yoff) = match &p.source {
            Some(Source::Solid(c)) => (KindSpec::Solid(*c), 0, 0),
            Some(Source::Linear(a, b, s)) => (KindSpec::Linear(*a, *b, s.clone()), 0, 0),
            Some(Source::Radial(a, b, s)) => (KindSpec::Radial(*a, *b, s.clone()), 0, 0),
            Some(Source::Conical(c, a, s)) => (KindSpec::Conical(*c, *a, s.clone()), 0, 0),
            None => {
                let (b, x, y) = self.bits_of(&p)?;
                (KindSpec::Bits(b), x, y)
            }
        };
        let mut transform = p.transform;
        if let Some(t) = &transform {
            if p.drawable.is_some() {
                transform = Some(pixman::matrix::translate(t, xoff << 16, yoff << 16));
            }
            (xoff, yoff) = (0, 0);
        }
        let alpha = match &p.alpha_map {
            Some(a) if !Rc::ptr_eq(a, pr) => self.bits_of(&a.borrow()).map(|(b, ..)| (b, p.alpha_origin.0, p.alpha_origin.1)),
            _ => None,
        };
        Some(ImageSpec { kind, transform, filter: Self::filter_of(&p), repeat: repeat_of(p.repeat), alpha, ca: p.component_alpha, xoff, yoff })
    }

    /// image_from_pict for the destination: its composite clip.
    fn dest_spec(&self, pr: &PicRef) -> Option<DestSpec> {
        let p = pr.borrow();
        let d = match p.drawable.as_ref()? {
            PicDrawable::Window(w) => Drawable::Window(*w),
            PicDrawable::Pixmap(pm) => Drawable::Pixmap(pm.clone()),
        };
        let mut gc = Gc::new(self.depth_of(&d));
        gc.subwindow_mode = p.subwindow_mode;
        gc.clip = p.clip.clone();
        (gc.clip_x, gc.clip_y) = (p.clip_origin.0 as i16, p.clip_origin.1 as i16);
        let place = self.place(&d, &gc)?;
        let (bits, xoff, yoff) = self.bits_of(&p)?;
        let origin = (place.dx - xoff, place.dy - yoff);
        let alpha = p.alpha_map.as_ref().and_then(|a| self.bits_of(&a.borrow()).map(|(b, ..)| (b, p.alpha_origin.0, p.alpha_origin.1)));
        Some(DestSpec {
            bits,
            clip: place.clip.translated(-origin.0, -origin.1),
            xoff,
            yoff,
            origin,
            alpha,
            saved: place.saved,
            repeat: repeat_of(p.repeat),
            convolution: p.filter == 5,
            ca: p.component_alpha,
        })
    }

    /// Hold a source's pixels: the destination's own, a pixmap's
    /// (borrowed), or a copy (a window buffer, or the dest's alpha map).
    fn hold<'p>(&self, b: &'p BitsSpec, dest: &Store, dest_alpha: Option<&Store>, fb_copy: bool) -> Hold<'p> {
        if b.store.same(dest) && !(fb_copy && matches!(b.store, Store::Buffer(0))) {
            return Hold::Dest;
        }
        let copy = dest_alpha.is_some_and(|a| a.same(&b.store));
        match &b.store {
            Store::Pixmap(pm) if !copy => match pm.try_borrow() {
                Ok(r) => Hold::Ref(r),
                Err(_) => Hold::None,
            },
            Store::Pixmap(pm) => Hold::Own(pm.borrow().px.clone()),
            Store::Buffer(buf) => Hold::Own(self.buffer(*buf).0.to_vec()),
            Store::Scratch => Hold::None,
        }
    }

    /// Run `f` on the destination with the sources as pixman images, then
    /// show what was drawn. Beneath a save-under, into the save too.
    fn run_render(&mut self, dest: &DestSpec, srcs: &[Option<ImageSpec>], scratch: Option<&mut [u32]>, f: &mut dyn FnMut(&mut Dest, &[Option<(Image, i32, i32)>]) -> Option<Box>) {
        let alpha_store = dest.alpha.as_ref().map(|a| a.0.store.clone()).filter(|s| !s.same(&dest.bits.store));
        // Save-unders first, from the screen as it is.
        for (sid, region) in &dest.saved {
            let holds = self.holds(srcs, &dest.bits.store, alpha_store.as_ref(), true);
            let images = build_images(srcs, &holds);
            let (w, h) = (self.width as i32, self.height as i32);
            let Some(save) = self.saves.get_mut(sid) else { continue };
            let mut d = Dest { px: &mut save.px, stride: w as usize, width: w, height: h, format: dest.bits.format, indexed: dest.bits.indexed.as_deref(), clip: Some(region.clone()), alpha: None, repeat: dest.repeat, convolution: dest.convolution, component_alpha: dest.ca };
            let _ = f(&mut d, &images);
        }
        if dest.clip.is_empty() {
            return;
        }
        let holds = self.holds(srcs, &dest.bits.store, alpha_store.as_ref(), false);
        let images = build_images(srcs, &holds);
        let alpha_pm = match (&dest.alpha, &alpha_store) {
            (Some((a, ..)), Some(Store::Pixmap(pm))) => Some((a.clone(), pm.clone())),
            _ => None,
        };
        let mut alpha_ref = alpha_pm.as_ref().and_then(|(_, pm)| pm.try_borrow_mut().ok());
        let ax = dest.alpha.as_ref().map_or((0, 0), |a| (a.1, a.2));
        let alpha_bits = alpha_pm.as_ref().map(|(a, _)| a);
        let b = &dest.bits;
        let drawn = match &b.store {
            Store::Pixmap(pm) => {
                let Ok(mut pm) = pm.try_borrow_mut() else { return };
                let alpha = dest_alpha(alpha_ref.as_mut().map(|r| &mut r.px[..]), alpha_bits, ax);
                let mut d = Dest { px: &mut pm.px, stride: b.stride, width: b.width, height: b.height, format: b.format, indexed: b.indexed.as_deref(), clip: Some(dest.clip.clone()), alpha, repeat: dest.repeat, convolution: dest.convolution, component_alpha: dest.ca };
                f(&mut d, &images);
                None
            }
            Store::Buffer(buf) => {
                let buf = *buf;
                let (px, ..) = self.buffer_mut(buf);
                let alpha = dest_alpha(alpha_ref.as_mut().map(|r| &mut r.px[..]), alpha_bits, ax);
                let mut d = Dest { px, stride: b.stride, width: b.width, height: b.height, format: b.format, indexed: b.indexed.as_deref(), clip: Some(dest.clip.clone()), alpha, repeat: dest.repeat, convolution: dest.convolution, component_alpha: dest.ca };
                f(&mut d, &images).map(|e| (buf, e))
            }
            Store::Scratch => {
                if let Some(px) = scratch {
                    let mut d = Dest { px, stride: b.stride, width: b.width, height: b.height, format: b.format, indexed: None, clip: Some(dest.clip.clone()), alpha: None, repeat: dest.repeat, convolution: false, component_alpha: false };
                    f(&mut d, &images);
                }
                None
            }
        };
        drop(images);
        drop(holds);
        drop(alpha_ref);
        if let Some((buf, e)) = drawn {
            let (ox, oy) = dest.origin;
            self.touch_buffer(buf, Box { x1: e.x1 + ox, y1: e.y1 + oy, x2: e.x2 + ox, y2: e.y2 + oy });
        }
    }

    fn holds<'p>(&self, srcs: &'p [Option<ImageSpec>], dest: &Store, alpha: Option<&Store>, fb_copy: bool) -> Vec<(Hold<'p>, Hold<'p>)> {
        srcs.iter()
            .map(|s| match s {
                Some(s) => {
                    let main = match &s.kind {
                        KindSpec::Bits(b) => self.hold(b, dest, alpha, fb_copy),
                        _ => Hold::None,
                    };
                    let am = s.alpha.as_ref().map_or(Hold::None, |(b, ..)| self.hold(b, dest, alpha, fb_copy));
                    (main, am)
                }
                None => (Hold::None, Hold::None),
            })
            .collect()
    }

    // -----------------------------------------------------------------
    // Composite.

    /// ReduceCompositeOp.
    fn reduce_op(&self, o: u8, src: &Picture, mask: bool, dst: &Picture, xs: i32, ys: i32, w: i32, h: i32) -> u8 {
        let (sf, df) = (self.code_of(src), self.code_of(dst));
        let no_src_alpha = sf.is_color()
            && sf.a() == 0
            && (src.repeat != 0
                || (src.transform.is_none() && xs >= 0 && ys >= 0 && {
                    let (dw, dh) = self.picture_size(src);
                    xs + w <= dw && ys + h <= dh
                }))
            && src.alpha_map.is_none()
            && !mask;
        let no_dst_alpha = df.is_color() && df.a() == 0 && dst.alpha_map.is_none();
        let mut o = o;
        if no_src_alpha {
            o = match o {
                op::OVER => op::SRC,
                op::IN_REVERSE => op::DST,
                op::OUT_REVERSE => op::CLEAR,
                op::ATOP => op::IN,
                op::ATOP_REVERSE => op::OVER_REVERSE,
                op::XOR => op::OUT,
                o => o,
            };
        }
        if no_dst_alpha {
            o = match o {
                op::OVER_REVERSE => op::DST,
                op::IN => op::SRC,
                op::OUT => op::CLEAR,
                op::ATOP => op::OVER,
                op::XOR => op::OUT_REVERSE,
                o => o,
            };
        }
        match o {
            op::DISJOINT_CLEAR | op::CONJOINT_CLEAR => op::CLEAR,
            op::DISJOINT_SRC | op::CONJOINT_SRC => op::SRC,
            op::DISJOINT_DST | op::CONJOINT_DST => op::DST,
            o => o,
        }
    }

    fn picture_size(&self, p: &Picture) -> (i32, i32) {
        match &p.drawable {
            Some(PicDrawable::Pixmap(pm)) => {
                let b = pm.borrow();
                (b.width as i32, b.height as i32)
            }
            Some(PicDrawable::Window(w)) => self.windows.get(w).map_or((0, 0), |w| (w.width as i32, w.height as i32)),
            None => (0, 0),
        }
    }

    /// CompositePicture: reduce the operator, then fbComposite.
    #[allow(clippy::too_many_arguments)]
    fn composite_picture(&mut self, o: u8, src: &PicRef, mask: Option<&PicRef>, dst: &PicRef, xs: i32, ys: i32, xm: i32, ym: i32, xd: i32, yd: i32, w: i32, h: i32) {
        let o = self.reduce_op(o, &src.borrow(), mask.is_some(), &dst.borrow(), xs, ys, w, h);
        if o == op::DST {
            return;
        }
        let Some(dest) = self.dest_spec(dst) else { return };
        let Some(s) = self.source_spec(src) else { return };
        let m = match mask {
            Some(m) => match self.source_spec(m) {
                Some(m) => Some(m),
                None => return,
            },
            None => None,
        };
        let (dx, dy) = (dest.xoff, dest.yoff);
        self.run_render(&dest, &[Some(s), m], None, &mut |d, im| {
            let (s, sx, sy) = im[0].as_ref()?;
            let m = im[1].as_ref();
            let (mx, my) = m.map_or((0, 0), |m| (m.1, m.2));
            pixman::composite(o, s, m.map(|m| &m.0), d, xs + sx, ys + sy, xm + mx, ym + my, xd + dx, yd + dy, w, h)
        });
    }

    fn composite_request(&mut self, r: &mut Reader) -> Answer {
        if r.remaining() != 32 {
            return Err(XError(BAD_LENGTH, 0));
        }
        let o = r.u8()?;
        r.skip(3)?;
        let (s, m, d) = (r.u32()?, r.u32()?, r.u32()?);
        if !op::valid(o) {
            return Err(XError(BAD_VALUE, o as u32));
        }
        let dst = self.picture(d)?;
        if dst.borrow().drawable.is_none() {
            return Err(XError(BAD_DRAWABLE, 0));
        }
        let src = self.picture(s)?;
        let mask = if m == 0 { None } else { Some(self.picture(m)?) };
        let s16 = |r: &mut Reader| -> Result<i32, XError> { Ok(r.u16()? as i16 as i32) };
        let (xs, ys, xm, ym, xd, yd) = (s16(r)?, s16(r)?, s16(r)?, s16(r)?, s16(r)?, s16(r)?);
        let (w, h) = (r.u16()? as i32, r.u16()? as i32);
        self.composite_picture(o, &src, mask.as_ref(), &dst, xs, ys, xm, ym, xd, yd, w, h);
        Ok(None)
    }

    /// Trapezoids, Triangles, TriStrip, TriFan: fbTrapezoids and
    /// fbTriangles (through pixman's trapezoids).
    fn shapes(&mut self, req: u8, r: &mut Reader) -> Answer {
        let o = r.u8()?;
        r.skip(3)?;
        let (s, d, mf) = (r.u32()?, r.u32()?, r.u32()?);
        let (xs, ys) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
        if !op::valid(o) {
            return Err(XError(BAD_VALUE, o as u32));
        }
        let src = self.picture(s)?;
        let dst = self.picture(d)?;
        if dst.borrow().drawable.is_none() {
            return Err(XError(BAD_DRAWABLE, 0));
        }
        let mask_format = if mf == 0 { None } else { Some(self.render.formats[self.pict_format(mf)?].code) };
        let fixed = |r: &mut Reader| -> Result<i32, XError> { Ok(r.u32()? as i32) };
        let point = |r: &mut Reader| -> Result<trap::Point, XError> { Ok(trap::Point { x: fixed(r)?, y: fixed(r)? }) };
        let rem = r.remaining();
        let (traps, first): (Vec<trap::Trapezoid>, (i32, i32)) = match req {
            10 => {
                if rem % 40 != 0 {
                    return Err(XError(BAD_LENGTH, 0));
                }
                let mut v = Vec::new();
                while r.remaining() >= 40 {
                    let (top, bottom) = (fixed(r)?, fixed(r)?);
                    let left = trap::Line { p1: point(r)?, p2: point(r)? };
                    let right = trap::Line { p1: point(r)?, p2: point(r)? };
                    v.push(trap::Trapezoid { top, bottom, left, right });
                }
                let first = v.first().map_or((0, 0), |t| (t.left.p1.x >> 16, t.left.p1.y >> 16));
                (v, first)
            }
            _ => {
                let tris: Vec<[trap::Point; 3]> = if req == 11 {
                    if rem % 24 != 0 {
                        return Err(XError(BAD_LENGTH, 0));
                    }
                    let mut v = Vec::new();
                    while r.remaining() >= 24 {
                        v.push([point(r)?, point(r)?, point(r)?]);
                    }
                    v
                } else {
                    if rem & 4 != 0 {
                        return Err(XError(BAD_LENGTH, 0));
                    }
                    let mut pts = Vec::new();
                    while r.remaining() >= 8 {
                        pts.push(point(r)?);
                    }
                    if pts.len() < 3 {
                        return Ok(None);
                    }
                    if req == 12 { pts.windows(3).map(|w| [w[0], w[1], w[2]]).collect() } else { pts[1..].windows(2).map(|w| [pts[0], w[0], w[1]]).collect() }
                };
                let first = tris.first().map_or((0, 0), |t| (t[0].x >> 16, t[0].y >> 16));
                (tris.iter().flat_map(|t| trap::triangle(t[0], t[1], t[2])).collect(), first)
            }
        };
        if traps.is_empty() {
            return Ok(None);
        }
        let (xs, ys) = (xs - first.0, ys - first.1);
        let Some(dest) = self.dest_spec(&dst) else { return Ok(None) };
        let Some(sspec) = self.source_spec(&src) else { return Ok(None) };
        let (dx, dy) = (dest.xoff, dest.yoff);
        let smooth = dst.borrow().poly_edge == 1;
        let per = if req == 10 { 1 } else { 2 };
        self.run_render(&dest, &[Some(sspec)], None, &mut |d, im| {
            let (s, sx, sy) = im[0].as_ref()?;
            let mut drawn: Option<Box> = None;
            let mut join = |b: Option<Box>| {
                if let Some(b) = b {
                    drawn = Some(drawn.map_or(b, |a| Box { x1: a.x1.min(b.x1), y1: a.y1.min(b.y1), x2: a.x2.max(b.x2), y2: a.y2.max(b.y2) }));
                }
            };
            match mask_format {
                None => {
                    let f = if smooth { Format::A8 } else { Format::A1 };
                    // One shape at a time: a triangle is its two trapezoids.
                    for t in traps.chunks(per) {
                        join(trap::composite_trapezoids(o, s, d, f, xs + sx, ys + sy, dx, dy, t));
                    }
                }
                Some(mf) => {
                    let f = match mf.a() {
                        1 => Format::A1,
                        4 => Format::new(4, pixman::TYPE_A, 4, 0, 0, 0),
                        _ => Format::A8,
                    };
                    join(trap::composite_trapezoids(o, s, d, f, xs + sx, ys + sy, dx, dy, &traps));
                }
            }
            drawn
        });
        Ok(None)
    }

    /// AddTraps: straight into an alpha picture's pixels, unclipped.
    fn add_traps(&mut self, r: &mut Reader) -> Answer {
        let p = self.picture(r.u32()?)?;
        if p.borrow().drawable.is_none() {
            return Err(XError(BAD_DRAWABLE, 0));
        }
        let (xo, yo) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
        if r.remaining() % 24 != 0 {
            return Err(XError(BAD_LENGTH, 0));
        }
        let mut traps = Vec::new();
        while r.remaining() >= 24 {
            let mut v = [0i32; 6];
            for x in &mut v {
                *x = r.u32()? as i32;
            }
            traps.push(trap::Trap { top: (v[0], v[1], v[2]), bot: (v[3], v[4], v[5]) });
        }
        let p = p.borrow();
        let f = self.code_of(&p);
        if f.ty() != pixman::TYPE_A || !matches!(f.bpp(), 1 | 4 | 8) {
            return Ok(None);
        }
        if let Some(PicDrawable::Pixmap(pm)) = &p.drawable {
            let mut pm = pm.borrow_mut();
            let (w, h) = (pm.width as i32, pm.height as i32);
            let mut a = trap::Alpha { px: &mut pm.px, stride: w as usize, width: w, height: h, bits: f.bpp() };
            trap::add_traps(&mut a, xo as i16 as i32, yo as i16 as i32, &traps);
        }
        Ok(None)
    }

    /// FillRectangles: miCompositeRects.
    fn fill_rectangles(&mut self, r: &mut Reader) -> Answer {
        let o = r.u8()?;
        r.skip(3)?;
        let d = r.u32()?;
        let mut c = [r.u16()?, r.u16()?, r.u16()?, r.u16()?];
        if !op::valid(o) {
            return Err(XError(BAD_VALUE, o as u32));
        }
        let dst = self.picture(d)?;
        if dst.borrow().drawable.is_none() {
            return Err(XError(BAD_DRAWABLE, 0));
        }
        if r.remaining() & 4 != 0 {
            return Err(XError(BAD_LENGTH, 0));
        }
        let mut rects = Vec::new();
        while r.remaining() >= 8 {
            rects.push((r.u16()? as i16 as i32, r.u16()? as i16 as i32, r.u16()? as i32, r.u16()? as i32));
        }
        let mut o = o;
        if c[3] == 0xffff && o == op::OVER {
            o = op::SRC;
        }
        if o == op::CLEAR {
            c = [0; 4];
        }
        if o == op::SRC || o == op::CLEAR {
            self.color_rects(&dst, &dst, c, &rects, 0, 0);
            let am = dst.borrow().alpha_map.clone();
            if let Some(a) = am {
                let origin = dst.borrow().alpha_origin;
                self.color_rects(&a, &dst, c, &rects, origin.0, origin.1);
            }
        } else {
            let src = Rc::new(RefCell::new(Picture::new(None, None, Some(Source::Solid(c)))));
            for &(x, y, w, h) in &rects {
                self.composite_picture(o, &src, None, &dst, 0, 0, 0, 0, x, y, w, h);
            }
        }
        Ok(None)
    }

    /// miColorRects: PolyFillRectangle with the colour's pixel, the
    /// picture's subwindow mode and the clip of `clip_pict`.
    fn color_rects(&mut self, dst: &PicRef, clip_pict: &PicRef, c: [u16; 4], rects: &[(i32, i32, i32, i32)], xoff: i32, yoff: i32) {
        let p = dst.borrow();
        let Some(f) = p.format else { return };
        let pf = &self.render.formats[f];
        let pixel = if pf.direct {
            let ch = |v: u16, mask: u16, shift: u16| ((v as u32) >> (16 - ones(mask as u32))) << shift;
            ch(c[0], pf.d[1], pf.d[0]) | ch(c[1], pf.d[3], pf.d[2]) | ch(c[2], pf.d[5], pf.d[4]) | ch(c[3], pf.d[7], pf.d[6])
        } else {
            let t = pf.table.as_ref();
            t.map_or(0, |t| {
                if t.color {
                    t.ent[(((c[0] as u32 >> 11) << 10) | ((c[1] as u32 >> 11) << 5) | (c[2] as u32 >> 11)) as usize] as u32
                } else {
                    t.ent[pixman::y15(((c[0] as u32 >> 8) << 16) | ((c[1] as u32 >> 8) << 8) | (c[2] as u32 >> 8)) as usize] as u32
                }
            })
        };
        let d = match &p.drawable {
            Some(PicDrawable::Window(w)) => Drawable::Window(*w),
            Some(PicDrawable::Pixmap(pm)) => Drawable::Pixmap(pm.clone()),
            None => return,
        };
        let mut gc = Gc::new(self.depth_of(&d));
        gc.fg = pixel & depth_mask(gc.depth);
        gc.subwindow_mode = p.subwindow_mode;
        {
            let cp = clip_pict.borrow();
            if cp.clip.is_some() {
                gc.clip = cp.clip.clone();
                (gc.clip_x, gc.clip_y) = ((cp.clip_origin.0 - xoff) as i16, (cp.clip_origin.1 - yoff) as i16);
            }
        }
        drop(p);
        let Some(place) = self.place(&d, &gc) else { return };
        let mr: Vec<crate::mi::Rect> = rects.iter().map(|&(x, y, w, h)| crate::mi::Rect { x: (x - xoff) as i16 as i32, y: (y - yoff) as i16 as i32, w, h }).collect();
        let fill = gc.fill(place.dx, place.dy);
        let g = gc.mi();
        self.with_canvas(&d, &place, gc.function, gc.plane_mask, &fill, gc.bg, |cv| crate::mi::poly_fill_rect(cv, &g, &mr));
    }

    // -----------------------------------------------------------------
    // Glyphs.

    fn add_glyphs(&mut self, r: &mut Reader) -> Answer {
        let gs = self.glyph_set(r.u32()?)?;
        let n = r.u32()? as usize;
        if n & 0xe000_0000 != 0 || n.checked_mul(16).is_none_or(|b| b > r.remaining()) {
            return Err(XError(BAD_LENGTH, 0));
        }
        let ids: Vec<u32> = (0..n).map(|_| r.u32()).collect::<Result<_, _>>()?;
        let mut infos = Vec::with_capacity(n);
        for _ in 0..n {
            infos.push((r.u16()?, r.u16()?, r.u16()? as i16, r.u16()? as i16, r.u16()? as i16, r.u16()? as i16));
        }
        let pf = &self.render.formats[gs.borrow().format];
        let (depth, code) = (pf.depth, pf.code_bpp());
        let bpp = crate::render::bpp(depth).unwrap_or(32);
        let mut glyphs = Vec::with_capacity(n);
        for &(w, h, x, y, xo, yo) in &infos {
            let stride = (w as usize * bpp).div_ceil(32) * 4;
            let size = stride * h as usize;
            if size > r.remaining() {
                return Err(XError(BAD_LENGTH, 0));
            }
            let data = r.bytes(size)?;
            r.skip(wire::pad(size)).map_err(|_| XError(BAD_LENGTH, 0))?;
            let mut px = Vec::with_capacity(w as usize * h as usize);
            for row in 0..h as usize {
                let line = &data[row * stride..];
                for col in 0..w as usize {
                    let raw = match bpp {
                        1 => (line[col / 8] >> (col % 8) & 1) as u32,
                        8 => line[col] as u32,
                        16 => u16::from_le_bytes([line[col * 2], line[col * 2 + 1]]) as u32,
                        _ => u32::from_le_bytes([line[col * 4], line[col * 4 + 1], line[col * 4 + 2], line[col * 4 + 3]]),
                    };
                    // The glyph's picture is made by compositing (Src) from
                    // the image, then copied (Src) into pixman's cache.
                    let p = pixman::from_argb(code, None, pixman::to_argb(code, None, raw));
                    px.push(pixman::from_argb(code, None, pixman::to_argb(code, None, p)));
                }
            }
            glyphs.push(Glyph { width: w, height: h, x, y, x_off: xo, y_off: yo, px });
        }
        if r.remaining() != 0 {
            return Err(XError(BAD_LENGTH, 0));
        }
        let mut gs = gs.borrow_mut();
        for (id, g) in ids.into_iter().zip(glyphs) {
            gs.glyphs.insert(id, Rc::new(g));
        }
        Ok(None)
    }

    /// CompositeGlyphs8/16/32: fbGlyphs, through pixman's glyph code.
    fn composite_glyphs(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let size = match ctx.data {
            23 => 1,
            24 => 2,
            _ => 4,
        };
        let o = r.u8()?;
        r.skip(3)?;
        let (s, d, mf, g) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        let (xs, ys) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
        if !op::valid(o) {
            return Err(XError(BAD_VALUE, o as u32));
        }
        let src = self.picture(s)?;
        let dst = self.picture(d)?;
        if dst.borrow().drawable.is_none() {
            return Err(XError(BAD_DRAWABLE, 0));
        }
        let mask_format = if mf == 0 { None } else { Some(self.pict_format(mf)?) };
        let mut gs = self.glyph_set(g)?;
        // The lists: each element's offset and the glyphs found.
        let buf = r.bytes(r.remaining())?;
        let end = buf.len();
        let mut i = 0;
        let mut lists: Vec<(i32, i32, Vec<(Rc<Glyph>, Format, bool)>)> = Vec::new();
        let order = ctx.order;
        let get16 = |b: &[u8]| order.u16([b[0], b[1]]);
        let get32 = |b: &[u8]| order.u32([b[0], b[1], b[2], b[3]]);
        while i + 8 < end {
            let len = buf[i];
            let (dx, dy) = (get16(&buf[i + 4..]) as i16 as i32, get16(&buf[i + 6..]) as i16 as i32);
            i += 8;
            if len == 0xff {
                if i + 4 < end {
                    gs = self.glyph_set(get32(&buf[i..]))?;
                }
                i += 4;
                continue;
            }
            let gsb = gs.borrow();
            let pf = &self.render.formats[gsb.format];
            let code = pf.code_bpp();
            let ca = code.a() != 0 && code.rgb() != 0;
            let mut list = Vec::new();
            for _ in 0..len {
                if i + size <= end {
                    let id = match size {
                        1 => buf[i] as u32,
                        2 => get16(&buf[i..]) as u32,
                        _ => get32(&buf[i..]),
                    };
                    if let Some(gl) = gsb.glyphs.get(&id) {
                        list.push((gl.clone(), code, ca));
                    }
                }
                i += size;
            }
            i += wire::pad(size * len as usize);
            lists.push((dx, dy, list));
        }
        if i > end {
            return Err(XError(BAD_LENGTH, 0));
        }
        if lists.is_empty() {
            return Ok(None);
        }
        // The glyphs' places, relative to the first list's origin.
        let (xdst, ydst) = (lists[0].0, lists[0].1);
        let (mut x, mut y) = (0i32, 0i32);
        let mut placed: Vec<(i32, i32, Rc<Glyph>, Format, bool)> = Vec::new();
        for (lx, ly, list) in &lists {
            x = x.wrapping_add(*lx);
            y = y.wrapping_add(*ly);
            for (gl, code, ca) in list {
                if gl.width != 0 && gl.height != 0 {
                    placed.push((x, y, gl.clone(), *code, *ca));
                }
                x = x.wrapping_add(gl.x_off as i32);
                y = y.wrapping_add(gl.y_off as i32);
            }
        }
        let Some(dest) = self.dest_spec(&dst) else { return Ok(None) };
        let Some(sspec) = self.source_spec(&src) else { return Ok(None) };
        let (dxo, dyo) = (dest.xoff, dest.yoff);
        let mut specs = vec![Some(sspec)];
        for (_, _, gl, code, ca) in &placed {
            specs.push(Some(ImageSpec { kind: KindSpec::Glyph(gl.clone(), *code), transform: None, filter: Filter::Nearest, repeat: Repeat::None, alpha: None, ca: *ca, xoff: 0, yoff: 0 }));
        }
        let mask_code = mask_format.map(|f| {
            let pf = &self.render.formats[f];
            pf.code.with_bpp(pf.depth as u32)
        });
        let origins: Vec<(i32, i32, i32, i32)> = placed.iter().map(|(x, y, gl, ..)| (*x, *y, gl.x as i32, gl.y as i32)).collect();
        self.run_render(&dest, &specs, None, &mut |d, im| {
            let (s, sx, sy) = im[0].as_ref()?;
            let glyphs: Vec<(&Image, i32, i32, i32, i32)> = im[1..].iter().zip(&origins).filter_map(|(g, o)| g.as_ref().map(|g| (&g.0, o.0, o.1, o.2, o.3))).collect();
            match mask_code {
                Some(mc) => glyphs_masked(o, s, d, mc, xs + sx, ys + sy, xdst, ydst, dxo, dyo, &glyphs),
                None => glyphs_no_mask(o, s, d, xs + sx - xdst, ys + sy - ydst, dxo, dyo, &glyphs),
            }
        });
        Ok(None)
    }

    // -----------------------------------------------------------------
    // Cursors.

    fn render_cursor(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        if r.remaining() != 12 {
            return Err(XError(BAD_LENGTH, 0));
        }
        let id = self.new_id(ctx, r.u32()?)?;
        let src = self.picture(r.u32()?)?;
        let (hx, hy) = (r.u16()? as usize, r.u16()? as usize);
        if src.borrow().drawable.is_none() {
            return Err(XError(BAD_DRAWABLE, 0));
        }
        let (w, h) = self.picture_size(&src.borrow());
        let (w, h) = (w as usize, h as usize);
        if hx > w || hy > h {
            return Err(XError(BAD_MATCH, 0));
        }
        let mut argb = vec![0u32; w * h];
        let direct = {
            let p = src.borrow();
            match (&p.drawable, self.code_of(&p) == Format::A8R8G8B8) {
                (Some(PicDrawable::Pixmap(pm)), true) => {
                    argb.copy_from_slice(&pm.borrow().px[..w * h]);
                    true
                }
                _ => false,
            }
        };
        if !direct && w > 0 && h > 0 {
            let o = self.reduce_op(op::SRC, &src.borrow(), false, &Picture::new(None, None, None), 0, 0, w as i32, h as i32);
            if let Some(s) = self.source_spec(&src) {
                let dest = DestSpec {
                    bits: BitsSpec { store: Store::Scratch, stride: w, width: w as i32, height: h as i32, format: Format::A8R8G8B8, indexed: None },
                    clip: Region::rect(0, 0, w as i32, h as i32),
                    xoff: 0,
                    yoff: 0,
                    origin: (0, 0),
                    alpha: None,
                    saved: Vec::new(),
                    repeat: Repeat::None,
                    convolution: false,
                    ca: false,
                };
                self.run_render(&dest, &[Some(s)], Some(&mut argb), &mut |d, im| {
                    let (s, sx, sy) = im[0].as_ref()?;
                    pixman::composite(o, s, None, d, *sx, *sy, 0, 0, 0, 0, w as i32, h as i32)
                });
            }
        }
        // Two colours (or fewer), all opaque: a core cursor.
        let mut two = Vec::new();
        let mut many = false;
        for &p in &argb {
            let a = p >> 24;
            if a == 0 {
                continue;
            }
            if a == 0xff {
                if !two.contains(&p) {
                    two.push(p);
                    if two.len() > 2 {
                        many = true;
                        break;
                    }
                }
            } else {
                many = true;
                break;
            }
        }
        let color = |p: u32| ((p >> 16 & 0xff) as u16 * 0x101, (p >> 8 & 0xff) as u16 * 0x101, (p & 0xff) as u16 * 0x101);
        let mut source = vec![false; w * h];
        let mut mask = vec![false; w * h];
        for (i, &p) in argb.iter().enumerate() {
            if !many {
                mask[i] = p >> 24 != 0;
                source[i] = p >> 24 != 0 && Some(&p) == two.first();
            } else {
                // Dithered to black and white, as X.org does.
                const DITHER: [[u32; 2]; 2] = [[1, 3], [4, 2]];
                let a = ((p >> 24) * 5 + 127) / 255;
                let i15 = ((pixman::y15(p) >> 7) * 5 + 127) / 255;
                let d = DITHER[(i / w.max(1)) & 1][(i % w.max(1)) & 1];
                mask[i] = a > d;
                source[i] = a > d && i15 <= d;
            }
        }
        let (fore, back) = if many { ((0, 0, 0), (0xffff, 0xffff, 0xffff)) } else { (two.first().map_or((0, 0, 0), |&p| color(p)), two.get(1).map_or((0, 0, 0), |&p| color(p))) };
        let mut c = Cursor::new(w.max(1), h.max(1), hx.min(w.saturating_sub(1)), hy.min(h.saturating_sub(1)), source, mask, fore, back);
        if many {
            c.argb = Some(argb);
        }
        if w == 0 || h == 0 {
            c = Cursor::new(1, 1, 0, 0, vec![false], vec![false], fore, back);
        }
        self.resources.insert(id, Resource::Cursor(Rc::new(c)));
        Ok(None)
    }

    fn create_anim_cursor(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.new_id(ctx, r.u32()?)?;
        if r.remaining() % 8 != 0 {
            return Err(XError(BAD_LENGTH, 0));
        }
        if r.remaining() == 0 {
            return Err(XError(BAD_VALUE, 0));
        }
        let mut frames = Vec::new();
        while r.remaining() >= 8 {
            let c = self.cursor_ref(r.u32()?)?;
            let delay = r.u32()?;
            frames.push((c, delay));
        }
        if frames.iter().any(|f| !f.0.anim.is_empty()) {
            return Err(XError(BAD_MATCH, 0));
        }
        let first = frames[0].0.clone();
        let mut c = Cursor::new(first.width, first.height, first.hot_x, first.hot_y, first.source.clone(), first.mask.clone(), first.fore.get(), first.back.get());
        c.argb = first.argb.clone();
        c.anim = frames;
        self.resources.insert(id, Resource::Cursor(Rc::new(c)));
        Ok(None)
    }

    /// When the animated cursor shown changes frame next.
    pub fn cursor_deadline(&self) -> Option<Instant> {
        self.render.anim.as_ref().map(|a| a.2)
    }

    /// Show the animated cursor's next frame, when it's time.
    pub fn cursor_tick(&mut self) {
        let Some((c, frame, next)) = self.render.anim.clone() else { return };
        if Instant::now() < next {
            return;
        }
        let frame = (frame + 1) % c.anim.len();
        self.host.cursor(&c.anim[frame].0.image());
        let delay = Duration::from_millis(c.anim[frame].1.max(1) as u64);
        self.render.anim = Some((c, frame, next.max(Instant::now() - delay) + delay));
    }

    /// Show a cursor: its first frame, if it's animated.
    pub fn show_cursor(&mut self, c: &Rc<Cursor>) {
        if c.anim.is_empty() {
            self.render.anim = None;
            self.host.cursor(&c.image());
        } else {
            self.host.cursor(&c.anim[0].0.image());
            let delay = Duration::from_millis(c.anim[0].1.max(1) as u64);
            self.render.anim = Some((c.clone(), 0, Instant::now() + delay));
        }
    }
}

fn dest_alpha<'x>(px: Option<&'x mut [u32]>, a: Option<&BitsSpec>, (x, y): (i32, i32)) -> Option<DestAlpha<'x>> {
    let (px, a) = (px?, a?);
    Some(DestAlpha { px, stride: a.stride, width: a.width, height: a.height, format: a.format, x, y })
}

/// The sources as pixman images, borrowing what `holds` holds.
fn build_images<'a>(srcs: &'a [Option<ImageSpec>], holds: &'a [(Hold<'a>, Hold<'a>)]) -> Vec<Option<(Image<'a>, i32, i32)>> {
    let bits = |b: &'a BitsSpec, h: &'a Hold<'a>| -> Option<Bits<'a>> {
        let px = match h {
            Hold::Ref(r) => Px::Slice(&r.px),
            Hold::Own(v) => Px::Slice(v),
            Hold::Dest => Px::Dest,
            Hold::None => return None,
        };
        Some(Bits { px, stride: b.stride, width: b.width, height: b.height, format: b.format, indexed: b.indexed.as_deref() })
    };
    srcs.iter()
        .zip(holds)
        .map(|(s, (hm, ha))| {
            let s = s.as_ref()?;
            let kind = match &s.kind {
                KindSpec::Bits(b) => Kind::Bits(bits(b, hm)?),
                KindSpec::Solid(c) => Kind::Solid(*c),
                KindSpec::Linear(p1, p2, st) => Kind::Linear(pixman::gradient::Linear { p1: *p1, p2: *p2 }, st.clone()),
                KindSpec::Radial(c1, c2, st) => Kind::Radial(pixman::gradient::Radial::new(*c1, *c2), st.clone()),
                KindSpec::Conical(c, a, st) => Kind::Conical(pixman::gradient::Conical::new(*c, *a), st.clone()),
                KindSpec::Glyph(g, f) => Kind::Bits(Bits { px: Px::Slice(&g.px), stride: g.width as usize, width: g.width as i32, height: g.height as i32, format: *f, indexed: None }),
            };
            let mut img = Image::new(kind);
            img.set_transform(s.transform);
            img.filter = s.filter.clone();
            img.repeat = s.repeat;
            img.component_alpha = s.ca;
            if let (Some((b, x, y)), Kind::Bits(_)) = (&s.alpha, &img.kind) {
                if let Some(ab) = bits(b, ha) {
                    img.alpha_map = Some((ab, *x, *y));
                }
            }
            Some((img, s.xoff, s.yoff))
        })
        .collect()
}

fn union(a: Option<Box>, b: Option<Box>) -> Option<Box> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Box { x1: a.x1.min(b.x1), y1: a.y1.min(b.y1), x2: a.x2.max(b.x2), y2: a.y2.max(b.y2) }),
        (a, None) => a,
        (None, b) => b,
    }
}

/// pixman_composite_glyphs_no_mask: each glyph as the mask, where it
/// meets the dest's composite region.
#[allow(clippy::too_many_arguments)]
fn glyphs_no_mask(o: u8, src: &Image, d: &mut Dest, src_x: i32, src_y: i32, dest_x: i32, dest_y: i32, glyphs: &[(&Image, i32, i32, i32, i32)]) -> Option<Box> {
    let region = pixman::composite_region(d, 0, 0, d.width, d.height)?;
    let mut drawn = None;
    for &(g, gx, gy, ox, oy) in glyphs {
        let Kind::Bits(gb) = &g.kind else { continue };
        let gbox = Box::new(dest_x + gx - ox, dest_y + gy - oy, gb.width, gb.height);
        for b in region.boxes() {
            let c = b.intersect(&gbox);
            if c.is_empty() {
                continue;
            }
            pixman::composite_rect(o, src, Some(g), d, src_x + c.x1 - dest_x, src_y + c.y1 - dest_y, c.x1 - gbox.x1, c.y1 - gbox.y1, c.x1, c.y1, c.width(), c.height());
            drawn = union(drawn, Some(c));
        }
    }
    drawn
}

/// pixman_composite_glyphs: the glyphs added up in a mask of their
/// extents, then composited through it.
#[allow(clippy::too_many_arguments)]
fn glyphs_masked(o: u8, src: &Image, d: &mut Dest, mask_code: Format, xs: i32, ys: i32, xdst: i32, ydst: i32, dxo: i32, dyo: i32, glyphs: &[(&Image, i32, i32, i32, i32)]) -> Option<Box> {
    let mut e = Box { x1: i32::MAX, y1: i32::MAX, x2: i32::MIN, y2: i32::MIN };
    for &(g, gx, gy, ox, oy) in glyphs {
        let Kind::Bits(gb) = &g.kind else { continue };
        let (x1, y1) = (gx - ox, gy - oy);
        e = Box { x1: e.x1.min(x1), y1: e.y1.min(y1), x2: e.x2.max(x1 + gb.width), y2: e.y2.max(y1 + gb.height) };
    }
    // Cut to what of the dest the mask can reach: the same pixels result.
    let reach = Box { x1: -dxo, y1: -dyo, x2: d.width - dxo, y2: d.height - dyo };
    let full = e;
    let e = e.intersect(&reach);
    if e.is_empty() || full.is_empty() {
        return None;
    }
    let (w, h) = (e.width(), e.height());
    let mut mpx = vec![0u32; w as usize * h as usize];
    {
        let mut m = Dest { px: &mut mpx, stride: w as usize, width: w, height: h, format: mask_code, indexed: None, clip: None, alpha: None, repeat: Repeat::None, convolution: false, component_alpha: false };
        let white = Image::new(Kind::Solid([0xffff; 4]));
        for &(g, gx, gy, ox, oy) in glyphs {
            let Kind::Bits(gb) = &g.kind else { continue };
            let gbox = Box::new(gx - ox - e.x1, gy - oy - e.y1, gb.width, gb.height);
            let c = gbox.intersect(&Box { x1: 0, y1: 0, x2: w, y2: h });
            if c.is_empty() {
                continue;
            }
            let (sx, sy) = (c.x1 - gbox.x1, c.y1 - gbox.y1);
            if gb.format == mask_code {
                pixman::composite_rect(op::ADD, g, None, &mut m, sx, sy, sx, sy, c.x1, c.y1, c.width(), c.height());
            } else {
                pixman::composite_rect(op::ADD, &white, Some(g), &mut m, sx, sy, sx, sy, c.x1, c.y1, c.width(), c.height());
            }
        }
    }
    let mut mask = Image::bits(Bits { px: Px::Slice(&mpx), stride: w as usize, width: w, height: h, format: mask_code, indexed: None });
    mask.component_alpha = mask_code.a() != 0 && mask_code.rgb() != 0;
    pixman::composite(o, src, Some(&mask), d, xs + e.x1 - xdst, ys + e.y1 - ydst, 0, 0, e.x1 + dxo, e.y1 + dyo, w, h)
}

