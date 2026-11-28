//! Compositing as pixman does it (pixman 0.46, the library X.org's fb uses
//! for RENDER): images of any RENDER format, solid fills and gradients,
//! with transforms, filters, repeat, alpha maps and component alpha,
//! composited through pixman's general path, row by row. Ported where the
//! numbers matter, so Xvfb's results come out the same; see COPYING here.
//!
//! Pixels are kept as everywhere in this crate: one u32 each, the
//! format's bits at the bottom.

pub mod combine;
pub mod combine_float;
pub mod gradient;
pub mod matrix;
pub mod trap;

use crate::region::{Box, Region};

pub type Transform = [[i32; 3]; 3];
pub const FIXED_1: i32 = 0x10000;
pub const FIXED_E: i32 = 1;

/// RENDER's (and pixman's) operators.
pub mod op {
    pub const CLEAR: u8 = 0;
    pub const SRC: u8 = 1;
    pub const DST: u8 = 2;
    pub const OVER: u8 = 3;
    pub const OVER_REVERSE: u8 = 4;
    pub const IN: u8 = 5;
    pub const IN_REVERSE: u8 = 6;
    pub const OUT: u8 = 7;
    pub const OUT_REVERSE: u8 = 8;
    pub const ATOP: u8 = 9;
    pub const ATOP_REVERSE: u8 = 10;
    pub const XOR: u8 = 11;
    pub const ADD: u8 = 12;
    pub const SATURATE: u8 = 13;
    pub const DISJOINT_CLEAR: u8 = 0x10;
    pub const DISJOINT_SRC: u8 = 0x11;
    pub const DISJOINT_DST: u8 = 0x12;
    pub const DISJOINT_OVER: u8 = 0x13;
    pub const DISJOINT_OVER_REVERSE: u8 = 0x14;
    pub const DISJOINT_IN: u8 = 0x15;
    pub const DISJOINT_IN_REVERSE: u8 = 0x16;
    pub const DISJOINT_OUT: u8 = 0x17;
    pub const DISJOINT_OUT_REVERSE: u8 = 0x18;
    pub const DISJOINT_ATOP: u8 = 0x19;
    pub const DISJOINT_ATOP_REVERSE: u8 = 0x1a;
    pub const DISJOINT_XOR: u8 = 0x1b;
    pub const CONJOINT_CLEAR: u8 = 0x20;
    pub const CONJOINT_SRC: u8 = 0x21;
    pub const CONJOINT_DST: u8 = 0x22;
    pub const CONJOINT_OVER: u8 = 0x23;
    pub const CONJOINT_OVER_REVERSE: u8 = 0x24;
    pub const CONJOINT_IN: u8 = 0x25;
    pub const CONJOINT_IN_REVERSE: u8 = 0x26;
    pub const CONJOINT_OUT: u8 = 0x27;
    pub const CONJOINT_OUT_REVERSE: u8 = 0x28;
    pub const CONJOINT_ATOP: u8 = 0x29;
    pub const CONJOINT_ATOP_REVERSE: u8 = 0x2a;
    pub const CONJOINT_XOR: u8 = 0x2b;
    pub const MULTIPLY: u8 = 0x30;
    pub const SCREEN: u8 = 0x31;
    pub const OVERLAY: u8 = 0x32;
    pub const DARKEN: u8 = 0x33;
    pub const LIGHTEN: u8 = 0x34;
    pub const COLOR_DODGE: u8 = 0x35;
    pub const COLOR_BURN: u8 = 0x36;
    pub const HARD_LIGHT: u8 = 0x37;
    pub const SOFT_LIGHT: u8 = 0x38;
    pub const DIFFERENCE: u8 = 0x39;
    pub const EXCLUSION: u8 = 0x3a;
    pub const HSL_HUE: u8 = 0x3b;
    pub const HSL_SATURATION: u8 = 0x3c;
    pub const HSL_COLOR: u8 = 0x3d;
    pub const HSL_LUMINOSITY: u8 = 0x3e;

    /// RENDER's PictOpValid.
    pub fn valid(op: u8) -> bool {
        op <= SATURATE || (DISJOINT_CLEAR..=DISJOINT_XOR).contains(&op) || (CONJOINT_CLEAR..=CONJOINT_XOR).contains(&op) || (MULTIPLY..=HSL_LUMINOSITY).contains(&op)
    }
}

/// A pixman format code: bits per pixel, type, and each channel's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Format(pub u32);

pub const TYPE_A: u32 = 1;
pub const TYPE_ARGB: u32 = 2;
pub const TYPE_ABGR: u32 = 3;
pub const TYPE_COLOR: u32 = 4;
pub const TYPE_GRAY: u32 = 5;
pub const TYPE_BGRA: u32 = 8;

impl Format {
    pub const fn new(bpp: u32, ty: u32, a: u32, r: u32, g: u32, b: u32) -> Format {
        Format(bpp << 24 | ty << 16 | a << 12 | r << 8 | g << 4 | b)
    }
    pub const A8R8G8B8: Format = Format::new(32, TYPE_ARGB, 8, 8, 8, 8);
    pub const X8R8G8B8: Format = Format::new(32, TYPE_ARGB, 0, 8, 8, 8);
    pub const A8: Format = Format::new(8, TYPE_A, 8, 0, 0, 0);
    pub const A1: Format = Format::new(1, TYPE_A, 1, 0, 0, 0);
    pub const X1R5G5B5: Format = Format::new(16, TYPE_ARGB, 0, 5, 5, 5);

    pub fn bpp(self) -> u32 {
        self.0 >> 24
    }
    pub fn ty(self) -> u32 {
        self.0 >> 16 & 0x3f
    }
    pub fn a(self) -> u32 {
        self.0 >> 12 & 0xf
    }
    pub fn r(self) -> u32 {
        self.0 >> 8 & 0xf
    }
    pub fn g(self) -> u32 {
        self.0 >> 4 & 0xf
    }
    pub fn b(self) -> u32 {
        self.0 & 0xf
    }
    pub fn rgb(self) -> u32 {
        self.0 & 0xfff
    }
    pub fn with_bpp(self, bpp: u32) -> Format {
        Format(self.0 & 0xff_ffff | bpp << 24)
    }
    pub fn is_wide(self) -> bool {
        self.a() > 8 || self.r() > 8 || self.g() > 8 || self.b() > 8
    }
    pub fn indexed(self) -> bool {
        matches!(self.ty(), TYPE_COLOR | TYPE_GRAY)
    }
    /// PIXMAN_FORMAT_COLOR.
    pub fn is_color(self) -> bool {
        matches!(self.ty(), TYPE_ARGB | TYPE_ABGR | TYPE_BGRA)
    }

    /// get_shifts: where alpha, red, green and blue are.
    fn shifts(self) -> (u32, u32, u32, u32) {
        let (a, r, g, b) = (self.a(), self.r(), self.g(), self.b());
        match self.ty() {
            TYPE_ARGB => (r + g + b, g + b, b, 0),
            TYPE_ABGR => (r + g + b, 0, r, r + g),
            TYPE_BGRA => {
                let bs = self.bpp() - b;
                let gs = bs - g;
                let rs = gs - r;
                (rs.wrapping_sub(a), rs, gs, bs)
            }
            _ => (0, 0, 0, 0),
        }
    }

    fn mask(self) -> u32 {
        if self.bpp() >= 32 { !0 } else { (1u32 << self.bpp()) - 1 }
    }
}

/// unorm_to_unorm: bits replicated into a wider channel, or cut.
fn unorm(val: u32, from: u32, to: u32) -> u32 {
    if from == 0 {
        return 0;
    }
    let val = val & ((1 << from) - 1);
    if from >= to {
        return val >> (from - to);
    }
    let mut r = val << (to - from);
    let mut f = from;
    while f < to {
        r |= r >> f;
        f *= 2;
    }
    r
}

fn convert_channel(pixel: u32, def: u32, from: u32, from_shift: u32, to: u32, to_shift: u32) -> u32 {
    let v = if from != 0 && to != 0 {
        unorm(pixel >> from_shift, from, to)
    } else if to != 0 {
        def
    } else {
        0
    };
    (v & ((1u64 << to) - 1) as u32) << to_shift
}

/// convert_pixel between two direct formats.
fn convert(from: Format, to: Format, p: u32) -> u32 {
    let (fa, fr, fg, fb) = from.shifts();
    let (ta, tr, tg, tb) = to.shifts();
    convert_channel(p, !0, from.a(), fa, to.a(), ta) | convert_channel(p, 0, from.r(), fr, to.r(), tr) | convert_channel(p, 0, from.g(), fg, to.g(), tg) | convert_channel(p, 0, from.b(), fb, to.b(), tb)
}

/// What an indexed (PseudoColor, GrayScale...) format's pixels are, and
/// which pixel each 15-bit colour (or gray) goes to.
#[derive(Clone)]
pub struct Indexed {
    pub color: bool,
    pub rgba: [u32; 256],
    pub ent: Vec<u8>,
}

pub fn y15(s: u32) -> u32 {
    ((s >> 16 & 0xff) * 153 + (s >> 8 & 0xff) * 301 + (s & 0xff) * 58) >> 2
}

/// A pixel of the format, as a8r8g8b8.
pub fn to_argb(f: Format, ix: Option<&Indexed>, p: u32) -> u32 {
    if f.indexed() {
        return ix.map_or(0, |ix| ix.rgba[(p & 0xff) as usize]);
    }
    convert(f, Format::A8R8G8B8, p)
}

/// An a8r8g8b8 pixel in the format.
pub fn from_argb(f: Format, ix: Option<&Indexed>, p: u32) -> u32 {
    let v = match f.ty() {
        TYPE_GRAY => ix.map_or(0, |ix| ix.ent[(y15(p) & 0x7fff) as usize] as u32),
        TYPE_COLOR => ix.map_or(0, |ix| ix.ent[(convert(Format::A8R8G8B8, Format::X1R5G5B5, p) & 0x7fff) as usize] as u32),
        _ => convert(Format::A8R8G8B8, f, p),
    };
    v & f.mask()
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Argb {
    pub a: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

const MULTIPLIERS: [f32; 16] = {
    let mut m = [0.0f32; 16];
    let mut i = 1;
    while i < 16 {
        m[i] = 1.0f32 / ((1u32 << i) - 1) as f32;
        i += 1;
    }
    m
};

/// pixman_expand_to_float: an a8r8g8b8 pixel that came from `f`.
fn expand(p: u32, f: Format) -> Argb {
    let f = if f.0 & 0xffff == 0 { Format::A8R8G8B8 } else { f };
    let (a, r, g, b) = (f.a(), f.r(), f.g(), f.b());
    let ch = |shift: u32, size: u32| ((p >> shift) & ((1u32 << size) - 1)) as f32 * MULTIPLIERS[size as usize];
    Argb { a: if a != 0 { ch(32 - a, a) } else { 1.0 }, r: ch(24 - r, r), g: ch(16 - g, g), b: ch(8 - b, b) }
}

/// unorm_to_float.
fn unorm_to_float(u: u32, n: u32) -> f32 {
    let m = (1u32 << n) - 1;
    (u & m) as f32 * (1.0f32 / m as f32)
}

/// float_to_unorm.
pub fn float_to_unorm(f: f32, n: u32) -> u32 {
    let f = if f > 1.0 {
        1.0
    } else if f < 0.0 {
        0.0
    } else {
        f
    };
    let u = (f * (1u32 << n) as f32) as u32;
    u - (u >> n)
}

fn contract(c: Argb) -> u32 {
    float_to_unorm(c.a, 8) << 24 | float_to_unorm(c.r, 8) << 16 | float_to_unorm(c.g, 8) << 8 | float_to_unorm(c.b, 8)
}

/// A wide format's pixel, straight to float.
fn wide_to_float(f: Format, p: u32) -> Argb {
    let (sa, sr, sg, sb) = f.shifts();
    let ch = |s: u32, n: u32| unorm_to_float(p >> s, n);
    Argb { a: if f.a() != 0 { ch(sa, f.a()) } else { 1.0 }, r: ch(sr, f.r()), g: ch(sg, f.g()), b: ch(sb, f.b()) }
}

fn float_to_wide(f: Format, c: Argb) -> u32 {
    let (sa, sr, sg, sb) = f.shifts();
    let mut v = float_to_unorm(c.r, f.r()) << sr | float_to_unorm(c.g, f.g()) << sg | float_to_unorm(c.b, f.b()) << sb;
    if f.a() != 0 {
        v |= float_to_unorm(c.a, f.a()) << sa;
    }
    v
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    None,
    Normal,
    Pad,
    Reflect,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    Nearest,
    Bilinear,
    /// Width and height (16.16) and the kernel, as RENDER sends them.
    Convolution(Vec<i32>),
}

/// A gradient stop: its place in 16.16, and red, green, blue, alpha.
#[derive(Clone, Copy, Debug)]
pub struct Stop {
    pub x: i32,
    pub color: [u16; 4],
}

/// Where a bits image's pixels are.
#[derive(Clone, Copy)]
pub enum Px<'a> {
    Slice(&'a [u32]),
    /// The destination's own (a picture composited onto itself).
    Dest,
}

#[derive(Clone, Copy)]
pub struct Bits<'a> {
    pub px: Px<'a>,
    pub stride: usize,
    pub width: i32,
    pub height: i32,
    pub format: Format,
    pub indexed: Option<&'a Indexed>,
}

impl Bits<'_> {
    #[inline]
    fn raw(&self, d: &[u32], x: i32, y: i32) -> u32 {
        let s = match self.px {
            Px::Slice(s) => s,
            Px::Dest => d,
        };
        s[y as usize * self.stride + x as usize]
    }

    #[inline]
    fn pixel_32(&self, d: &[u32], x: i32, y: i32) -> u32 {
        to_argb(self.format, self.indexed, self.raw(d, x, y))
    }

    #[inline]
    fn pixel_float(&self, d: &[u32], x: i32, y: i32) -> Argb {
        if self.format.is_wide() {
            wide_to_float(self.format, self.raw(d, x, y))
        } else {
            expand(self.pixel_32(d, x, y), self.format)
        }
    }
}

pub enum Kind<'a> {
    Bits(Bits<'a>),
    /// Red, green, blue, alpha.
    Solid([u16; 4]),
    Linear(gradient::Linear, Vec<Stop>),
    Radial(gradient::Radial, Vec<Stop>),
    Conical(gradient::Conical, Vec<Stop>),
}

pub struct Image<'a> {
    pub kind: Kind<'a>,
    pub transform: Option<Transform>,
    pub filter: Filter,
    pub repeat: Repeat,
    /// Another image's alpha in place of this one's, from (x, y) of it.
    pub alpha_map: Option<(Bits<'a>, i32, i32)>,
    pub component_alpha: bool,
}

impl<'a> Image<'a> {
    pub fn new(kind: Kind<'a>) -> Image<'a> {
        Image { kind, transform: None, filter: Filter::Nearest, repeat: Repeat::None, alpha_map: None, component_alpha: false }
    }

    pub fn bits(b: Bits<'a>) -> Image<'a> {
        Image::new(Kind::Bits(b))
    }

    /// pixman_image_set_transform: the identity is none.
    pub fn set_transform(&mut self, t: Option<Transform>) {
        let id = [[FIXED_1, 0, 0], [0, FIXED_1, 0], [0, 0, FIXED_1]];
        self.transform = t.filter(|t| *t != id);
    }

    fn info(&self) -> Info {
        let mut i = Info { narrow: true, ..Default::default() };
        let affine = match &self.transform {
            None => true,
            Some(t) => t[2][0] == 0 && t[2][1] == 0 && t[2][2] == FIXED_1,
        };
        i.id = self.transform.is_none();
        match self.filter {
            Filter::Nearest => i.nearest = true,
            Filter::Bilinear => {
                i.bilinear = true;
                if i.id {
                    i.nearest = true;
                } else if affine {
                    let t = self.transform.unwrap();
                    let frac = (t[0][0] | t[0][1] | t[0][2] | t[1][0] | t[1][1] | t[1][2]) & 0xffff;
                    if frac == 0 && (t[0][0].wrapping_add(t[0][1]) & t[1][0].wrapping_add(t[1][1])) >> 16 & 1 == 1 {
                        let lim = 30000 << 16;
                        if (-lim..=lim).contains(&t[0][2]) && (-lim..=lim).contains(&t[1][2]) {
                            i.nearest = true;
                        }
                    }
                }
            }
            Filter::Convolution(_) => {}
        }
        match &self.kind {
            Kind::Solid(c) => i.opaque = c[3] == 0xffff,
            Kind::Bits(b) => {
                if b.format.a() == 0 && !b.format.indexed() {
                    i.samples_opaque = true;
                    i.opaque = self.repeat != Repeat::None;
                }
                if b.format.is_wide() {
                    i.narrow = false;
                }
            }
            Kind::Linear(_, stops) | Kind::Conical(_, stops) => i.opaque = self.repeat != Repeat::None && stops.iter().all(|s| s.color[3] == 0xffff),
            Kind::Radial(r, stops) => i.opaque = r.covers() && self.repeat != Repeat::None && stops.iter().all(|s| s.color[3] == 0xffff),
        }
        if let (Some((a, ..)), Kind::Bits(_)) = (&self.alpha_map, &self.kind) {
            if a.format.is_wide() {
                i.narrow = false;
            }
        }
        if (self.alpha_map.is_some() && matches!(self.kind, Kind::Bits(_))) || matches!(self.filter, Filter::Convolution(_)) || self.component_alpha {
            i.opaque = false;
            i.samples_opaque = false;
        }
        i
    }
}

#[derive(Clone, Copy, Default)]
struct Info {
    opaque: bool,
    samples_opaque: bool,
    nearest: bool,
    bilinear: bool,
    narrow: bool,
    id: bool,
}

/// What's composited onto: a bits image, written.
pub struct Dest<'a> {
    pub px: &'a mut [u32],
    pub stride: usize,
    pub width: i32,
    pub height: i32,
    pub format: Format,
    pub indexed: Option<&'a Indexed>,
    /// The composite clip, in the image's coordinates.
    pub clip: Option<Region>,
    pub alpha: Option<DestAlpha<'a>>,
    /// The picture's own repeat, filter and component alpha: they decide
    /// whether pixman takes it as opaque.
    pub repeat: Repeat,
    pub convolution: bool,
    pub component_alpha: bool,
}

pub struct DestAlpha<'a> {
    pub px: &'a mut [u32],
    pub stride: usize,
    pub width: i32,
    pub height: i32,
    pub format: Format,
    pub x: i32,
    pub y: i32,
}

impl Dest<'_> {
    fn opaque(&self) -> bool {
        self.format.a() == 0 && !self.format.indexed() && self.repeat != Repeat::None && self.alpha.is_none() && !self.convolution && !self.component_alpha
    }

    fn narrow(&self) -> bool {
        !self.format.is_wide() && self.alpha.as_ref().is_none_or(|a| !a.format.is_wide())
    }

    pub fn as_bits(&self) -> Bits<'_> {
        Bits { px: Px::Dest, stride: self.stride, width: self.width, height: self.height, format: self.format, indexed: self.indexed }
    }
}

// ---------------------------------------------------------------------
// Fetching.

#[inline]
fn repeat(r: Repeat, c: i32, size: i32) -> i32 {
    match r {
        Repeat::Normal => c.rem_euclid(size),
        Repeat::Pad => c.clamp(0, size - 1),
        Repeat::Reflect => {
            let s2 = size.wrapping_mul(2);
            let c = if c < 0 { (s2 - ((-(c as i64) - 1) % s2 as i64) as i32) - 1 } else { c % s2 };
            if c >= size { s2 - c - 1 } else { c }
        }
        Repeat::None => c,
    }
}

/// A pixel type: a8r8g8b8 (pixman's narrow path) or float (wide).
trait Pix: Copy + Default {
    fn get(b: &Bits, d: &[u32], x: i32, y: i32) -> Self;
    fn with_alpha(self, a: Self) -> Self;
    fn bilinear(tl: Self, tr: Self, bl: Self, br: Self, x1: i32, y1: i32) -> Self;
    fn accum(acc: &mut [u32; 4], p: Self, f: i32);
    fn reduce(acc: [u32; 4]) -> Self;
    fn solid(c: [u16; 4]) -> Self;
}

impl Pix for u32 {
    #[inline]
    fn get(b: &Bits, d: &[u32], x: i32, y: i32) -> u32 {
        b.pixel_32(d, x, y)
    }
    #[inline]
    fn with_alpha(self, a: u32) -> u32 {
        self & 0xff_ffff | (a >> 24) << 24
    }
    fn bilinear(tl: u32, tr: u32, bl: u32, br: u32, x1: i32, y1: i32) -> u32 {
        let distx = ((x1 >> 9) & 0x7f) as u64 * 2;
        let disty = ((y1 >> 9) & 0x7f) as u64 * 2;
        let distxy = distx * disty;
        let distxiy = distx * (256 - disty);
        let distixy = (256 - distx) * disty;
        let distixiy = (256 - distx) * (256 - disty);
        let ab = |p: u32| (p & 0xff00_00ff) as u64;
        let mut f = ab(tl) * distixiy + ab(tr) * distxiy + ab(bl) * distixy + ab(br) * distxy;
        let mut r = f & 0x0000_ff00_00ff_0000;
        let rg = |p: u32| {
            let p = p as u64;
            ((p << 16) & 0x0000_00ff_0000_0000) | (p & 0x0000_ff00)
        };
        f = rg(tl) * distixiy + rg(tr) * distxiy + rg(bl) * distixy + rg(br) * distxy;
        r |= ((f >> 16) & 0x0000_00ff_0000_0000) | (f & 0xff00_0000);
        (r >> 16) as u32
    }
    fn accum(acc: &mut [u32; 4], p: u32, f: i32) {
        acc[0] = acc[0].wrapping_add(((p >> 24) as i32).wrapping_mul(f) as u32);
        acc[1] = acc[1].wrapping_add(((p >> 16 & 0xff) as i32).wrapping_mul(f) as u32);
        acc[2] = acc[2].wrapping_add(((p >> 8 & 0xff) as i32).wrapping_mul(f) as u32);
        acc[3] = acc[3].wrapping_add(((p & 0xff) as i32).wrapping_mul(f) as u32);
    }
    fn reduce(acc: [u32; 4]) -> u32 {
        let c = |t: u32| ((t.wrapping_add(0x8000) as i32) / 65536).clamp(0, 0xff) as u32;
        c(acc[0]) << 24 | c(acc[1]) << 16 | c(acc[2]) << 8 | c(acc[3])
    }
    fn solid(c: [u16; 4]) -> u32 {
        (c[3] as u32 >> 8) << 24 | (c[0] as u32 >> 8) << 16 | (c[1] as u32 & 0xff00) | (c[2] as u32 >> 8)
    }
}

impl Pix for Argb {
    #[inline]
    fn get(b: &Bits, d: &[u32], x: i32, y: i32) -> Argb {
        b.pixel_float(d, x, y)
    }
    #[inline]
    fn with_alpha(self, a: Argb) -> Argb {
        Argb { a: a.a, ..self }
    }
    fn bilinear(tl: Argb, tr: Argb, bl: Argb, br: Argb, x1: i32, y1: i32) -> Argb {
        let distx = (x1 & 0xffff) as f32 / 65536.0;
        let disty = (y1 & 0xffff) as f32 / 65536.0;
        let distxy = distx * disty;
        let distxiy = distx * (1.0 - disty);
        let distixy = (1.0 - distx) * disty;
        let distixiy = (1.0 - distx) * (1.0 - disty);
        let ch = |t: f32, r: f32, b: f32, bb: f32| t * distixiy + r * distxiy + b * distixy + bb * distxy;
        Argb { a: ch(tl.a, tr.a, bl.a, br.a), r: ch(tl.r, tr.r, bl.r, br.r), g: ch(tl.g, tr.g, bl.g, br.g), b: ch(tl.b, tr.b, bl.b, br.b) }
    }
    fn accum(acc: &mut [u32; 4], p: Argb, f: i32) {
        // `unsigned += float * int`, as C does it.
        let add = |t: u32, v: f32| (t as f32 + v * f as f32) as i64 as u32;
        acc[0] = add(acc[0], p.a);
        acc[1] = add(acc[1], p.r);
        acc[2] = add(acc[2], p.g);
        acc[3] = add(acc[3], p.b);
    }
    fn reduce(acc: [u32; 4]) -> Argb {
        let c = |t: u32| (t as i32 as f32 / 65536.0).clamp(0.0, 1.0);
        Argb { a: c(acc[0]), r: c(acc[1]), g: c(acc[2]), b: c(acc[3]) }
    }
    fn solid(c: [u16; 4]) -> Argb {
        Argb { a: unorm_to_float(c[3] as u32, 16), r: unorm_to_float(c[0] as u32, 16), g: unorm_to_float(c[1] as u32, 16), b: unorm_to_float(c[2] as u32, 16) }
    }
}

impl Image<'_> {
    /// fetch_pixel_no_alpha / fetch_pixel_general: a pixel, or clear
    /// outside the image when it's checked; with the alpha map's alpha.
    #[inline]
    fn get<P: Pix>(&self, b: &Bits, d: &[u32], x: i32, y: i32, check: bool) -> P {
        if check && (x < 0 || x >= b.width || y < 0 || y >= b.height) {
            return P::default();
        }
        let p = P::get(b, d, x, y);
        match &self.alpha_map {
            Some((a, ax, ay)) => {
                let (x, y) = (x.wrapping_sub(*ax), y.wrapping_sub(*ay));
                let pa = if x < 0 || x >= a.width || y < 0 || y >= a.height { P::default() } else { P::get(a, d, x, y) };
                p.with_alpha(pa)
            }
            None => p,
        }
    }

    fn filtered<P: Pix>(&self, b: &Bits, d: &[u32], x: i32, y: i32) -> P {
        let r = self.repeat;
        let (w, h) = (b.width, b.height);
        match &self.filter {
            Filter::Nearest => {
                let (x0, y0) = (x.wrapping_sub(FIXED_E) >> 16, y.wrapping_sub(FIXED_E) >> 16);
                if r != Repeat::None { self.get(b, d, repeat(r, x0, w), repeat(r, y0, h), false) } else { self.get(b, d, x0, y0, true) }
            }
            Filter::Bilinear => {
                let (x1f, y1f) = (x.wrapping_sub(FIXED_1 / 2), y.wrapping_sub(FIXED_1 / 2));
                let (x1, y1) = (x1f >> 16, y1f >> 16);
                let (x2, y2) = (x1.wrapping_add(1), y1.wrapping_add(1));
                let (tl, tr, bl, br) = if r != Repeat::None {
                    let (x1, y1, x2, y2) = (repeat(r, x1, w), repeat(r, y1, h), repeat(r, x2, w), repeat(r, y2, h));
                    (self.get(b, d, x1, y1, false), self.get(b, d, x2, y1, false), self.get(b, d, x1, y2, false), self.get(b, d, x2, y2, false))
                } else {
                    (self.get(b, d, x1, y1, true), self.get(b, d, x2, y1, true), self.get(b, d, x1, y2, true), self.get(b, d, x2, y2, true))
                };
                P::bilinear(tl, tr, bl, br, x1f, y1f)
            }
            Filter::Convolution(params) => {
                let x_off = params[0].wrapping_sub(FIXED_1) >> 1;
                let y_off = params[1].wrapping_sub(FIXED_1) >> 1;
                let (cw, ch) = (params[0] >> 16, params[1] >> 16);
                let x1 = x.wrapping_sub(FIXED_E).wrapping_sub(x_off) >> 16;
                let y1 = y.wrapping_sub(FIXED_E).wrapping_sub(y_off) >> 16;
                let mut acc = [0u32; 4];
                let mut k = 2;
                for i in y1..y1.saturating_add(ch) {
                    for j in x1..x1.saturating_add(cw) {
                        let f = params.get(k).copied().unwrap_or(0);
                        k += 1;
                        if f != 0 {
                            let p: P = if r != Repeat::None { self.get(b, d, repeat(r, j, w), repeat(r, i, h), false) } else { self.get(b, d, j, i, true) };
                            P::accum(&mut acc, p, f);
                        }
                    }
                }
                P::reduce(acc)
            }
        }
    }

    /// One scanline of a bits image, from (x, y), as pixman's fetchers.
    fn fetch_bits<P: Pix>(&self, b: &Bits, d: &[u32], x: i32, y: i32, out: &mut [P]) {
        let width = out.len() as i32;
        let conv = matches!(self.filter, Filter::Convolution(_));
        if self.alpha_map.is_none() && self.transform.is_none() && !conv && matches!(self.repeat, Repeat::None | Repeat::Normal) {
            // bits_image_fetch_untransformed.
            if self.repeat == Repeat::None {
                for (i, o) in out.iter_mut().enumerate() {
                    let xx = x.wrapping_add(i as i32);
                    *o = if y < 0 || y >= b.height || xx < 0 || xx >= b.width { P::default() } else { P::get(b, d, xx, y) };
                }
            } else {
                let yy = y.rem_euclid(b.height);
                for (i, o) in out.iter_mut().enumerate() {
                    *o = P::get(b, d, x.wrapping_add(i as i32).rem_euclid(b.width), yy);
                }
            }
            return;
        }
        let v = [(x << 16).wrapping_add(FIXED_1 / 2), (y << 16).wrapping_add(FIXED_1 / 2), FIXED_1];
        let (v, ux, uy, uw) = match &self.transform {
            Some(t) => match matrix::point_3d(t, v) {
                Some(v) => (v, t[0][0], t[1][0], t[2][0]),
                None => return,
            },
            None => (v, FIXED_1, 0, 0),
        };
        let affine = self.transform.as_ref().is_some_and(|t| t[2][0] == 0 && t[2][1] == 0 && t[2][2] == FIXED_1);
        let (mut px, mut py, mut pw) = (v[0], v[1], v[2]);
        if self.alpha_map.is_none() && affine {
            for o in out.iter_mut() {
                *o = self.filtered(b, d, px, py);
                px = px.wrapping_add(ux);
                py = py.wrapping_add(uy);
            }
            return;
        }
        for o in out.iter_mut().take(width as usize) {
            let (x0, y0) = if pw != 0 {
                let w = pw as i64 as u64;
                ((((px as i64 as u64) << 16) / w) as i32, (((py as i64 as u64) << 16) / w) as i32)
            } else {
                (0, 0)
            };
            *o = self.filtered(b, d, x0, y0);
            px = px.wrapping_add(ux);
            py = py.wrapping_add(uy);
            pw = pw.wrapping_add(uw);
        }
    }
}

/// The source and mask iterators of general_composite_rect.
struct Iter<'i, 'a> {
    img: &'i Image<'a>,
    x: i32,
    y: i32,
    /// Fetched once at the start: solid colours, 1x1 repeating images,
    /// horizontal linear gradients.
    fixed: bool,
}

impl<'i, 'a> Iter<'i, 'a> {
    fn new<P: Pix>(img: &'i Image<'a>, d: &[u32], x: i32, y: i32, height: i32, buf: &mut [P]) -> Iter<'i, 'a>
    where
        [P]: gradient::Out,
    {
        let mut it = Iter { img, x, y, fixed: false };
        match &img.kind {
            Kind::Solid(c) => {
                buf.fill(P::solid(*c));
                it.fixed = true;
            }
            Kind::Bits(b) if b.width == 1 && b.height == 1 && img.repeat != Repeat::None && img.alpha_map.is_none() => {
                buf.fill(P::get(b, d, 0, 0));
                it.fixed = true;
            }
            Kind::Linear(l, _) if l.is_horizontal(img.transform.as_ref(), height) => {
                it.fetch(d, buf);
                it.fixed = true;
            }
            _ => {}
        }
        it
    }

    fn fetch<P: Pix>(&mut self, d: &[u32], buf: &mut [P])
    where
        [P]: gradient::Out,
    {
        if self.fixed {
            return;
        }
        let img = self.img;
        let t = img.transform.as_ref();
        match &img.kind {
            Kind::Bits(b) => img.fetch_bits(b, d, self.x, self.y, buf),
            Kind::Solid(_) => {}
            Kind::Linear(l, stops) => {
                let stops = gradient::with_sentinels(stops, img.repeat);
                l.scanline(buf, &mut gradient::Walker::new(&stops, img.repeat), t, self.x, self.y)
            }
            Kind::Radial(r, stops) => {
                let stops = gradient::with_sentinels(stops, img.repeat);
                r.scanline(buf, &mut gradient::Walker::new(&stops, img.repeat), t, img.repeat, self.x, self.y)
            }
            Kind::Conical(c, stops) => {
                let stops = gradient::with_sentinels(stops, img.repeat);
                c.scanline(buf, &mut gradient::Walker::new(&stops, img.repeat), t, self.x, self.y)
            }
        }
        self.y = self.y.wrapping_add(1);
    }
}

/// operator_needs_division: pixman's float path, whatever the formats.
fn needs_division(o: u8) -> bool {
    matches!(o, op::SATURATE | op::DISJOINT_CLEAR..=op::DISJOINT_XOR | op::CONJOINT_CLEAR..=op::CONJOINT_XOR | op::COLOR_DODGE | op::COLOR_BURN | op::SOFT_LIGHT | op::HSL_HUE..=op::HSL_LUMINOSITY)
}

/// The dest is fetched (the operator reads it) or not.
fn reads_dest(o: u8) -> bool {
    !matches!(o, op::CLEAR | op::SRC)
}

/// general_composite_rect: (x, y, w, h) of the dest, within it.
#[allow(clippy::too_many_arguments)]
pub fn composite_rect(o: u8, src: &Image, mask: Option<&Image>, dst: &mut Dest, sx: i32, sy: i32, mx: i32, my: i32, dx: i32, dy: i32, w: i32, h: i32) {
    if w <= 0 || h <= 0 {
        return;
    }
    let narrow = src.info().narrow && mask.is_none_or(|m| m.info().narrow) && dst.narrow() && !needs_division(o);
    // The source doesn't matter, so nor does the mask.
    let mask = if matches!(o, op::CLEAR | op::DST) { None } else { mask };
    let ca = mask.is_some_and(|m| m.component_alpha);
    if narrow {
        run::<u32>(o, src, mask, dst, (sx, sy, mx, my, dx, dy, w, h), |o, d, s, m| match m {
            Some(m) if ca => combine::combine_ca(o, d, s, m),
            m => combine::combine_u(o, d, s, m),
        });
    } else {
        run::<Argb>(o, src, mask, dst, (sx, sy, mx, my, dx, dy, w, h), |o, d, s, m| combine_float::combine(o, ca, d, s, m));
    }
}

/// Writing a dest scanline back: narrow or wide.
trait Store: Pix {
    fn fetch_dest(dst: &Dest, x: i32, y: i32, out: &mut [Self]);
    fn store(dst: &mut Dest, x: i32, y: i32, row: &[Self]);
}

impl Store for u32 {
    fn fetch_dest(dst: &Dest, x: i32, y: i32, out: &mut [u32]) {
        let b = dst.as_bits();
        for (i, o) in out.iter_mut().enumerate() {
            *o = b.pixel_32(dst.px, x + i as i32, y);
        }
        if let Some(a) = &dst.alpha {
            let ab = Bits { px: Px::Slice(a.px), stride: a.stride, width: a.width, height: a.height, format: a.format, indexed: None };
            for (i, o) in out.iter_mut().enumerate() {
                let p = ab.pixel_32(&[], x + i as i32 - a.x, y - a.y);
                *o = *o & 0xff_ffff | p & 0xff00_0000;
            }
        }
    }
    fn store(dst: &mut Dest, x: i32, y: i32, row: &[u32]) {
        let (f, ix) = (dst.format, dst.indexed);
        let base = y as usize * dst.stride + x as usize;
        for (i, &p) in row.iter().enumerate() {
            dst.px[base + i] = from_argb(f, ix, p);
        }
        if let Some(a) = &mut dst.alpha {
            let base = (y - a.y) as usize * a.stride + (x - a.x) as usize;
            for (i, &p) in row.iter().enumerate() {
                a.px[base + i] = from_argb(a.format, None, p);
            }
        }
    }
}

impl Store for Argb {
    fn fetch_dest(dst: &Dest, x: i32, y: i32, out: &mut [Argb]) {
        let b = dst.as_bits();
        for (i, o) in out.iter_mut().enumerate() {
            *o = b.pixel_float(dst.px, x + i as i32, y);
        }
        if let Some(a) = &dst.alpha {
            let ab = Bits { px: Px::Slice(a.px), stride: a.stride, width: a.width, height: a.height, format: a.format, indexed: None };
            for (i, o) in out.iter_mut().enumerate() {
                o.a = ab.pixel_float(&[], x + i as i32 - a.x, y - a.y).a;
            }
        }
    }
    fn store(dst: &mut Dest, x: i32, y: i32, row: &[Argb]) {
        let store = |f: Format, ix: Option<&Indexed>, c: Argb| if f.is_wide() { float_to_wide(f, c) } else { from_argb(f, ix, contract(c)) };
        let (f, ix) = (dst.format, dst.indexed);
        let base = y as usize * dst.stride + x as usize;
        for (i, &c) in row.iter().enumerate() {
            dst.px[base + i] = store(f, ix, c);
        }
        if let Some(a) = &mut dst.alpha {
            let base = (y - a.y) as usize * a.stride + (x - a.x) as usize;
            for (i, &c) in row.iter().enumerate() {
                a.px[base + i] = store(a.format, None, c);
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn run<P: Store>(o: u8, src: &Image, mask: Option<&Image>, dst: &mut Dest, (sx, sy, mx, my, dx, dy, w, h): (i32, i32, i32, i32, i32, i32, i32, i32), combine: impl Fn(u8, &mut [P], &[P], Option<&[P]>))
where
    [P]: gradient::Out,
{
    let n = w as usize;
    let (mut sbuf, mut mbuf, mut dbuf) = (vec![P::default(); n], vec![P::default(); n], vec![P::default(); n]);
    let mut si = Iter::new(src, dst.px, sx, sy, h, &mut sbuf);
    let mut mi = mask.map(|m| Iter::new(m, dst.px, mx, my, h, &mut mbuf));
    let wide = std::mem::size_of::<P>() != 4;
    for row in 0..h {
        if let Some(mi) = &mut mi {
            mi.fetch(dst.px, &mut mbuf);
        }
        si.fetch(dst.px, &mut sbuf);
        if wide || reads_dest(o) {
            P::fetch_dest(dst, dx, dy + row, &mut dbuf);
        }
        combine(o, &mut dbuf, &sbuf, mi.as_ref().map(|_| &mbuf[..]));
        P::store(dst, dx, dy + row, &dbuf);
    }
}

// ---------------------------------------------------------------------
// pixman_image_composite32.

/// The operator, simplified by what's opaque (pixman's operator_table:
/// neither, source, dest, both).
fn optimize(o: u8, src_opaque: bool, dst_opaque: bool) -> u8 {
    use op::*;
    let t: [u8; 4] = match o {
        CLEAR => [CLEAR; 4],
        SRC => [SRC; 4],
        DST => [DST; 4],
        OVER => [OVER, SRC, OVER, SRC],
        OVER_REVERSE => [OVER_REVERSE, OVER_REVERSE, DST, DST],
        IN => [IN, IN, SRC, SRC],
        IN_REVERSE => [IN_REVERSE, DST, IN_REVERSE, DST],
        OUT => [OUT, OUT, CLEAR, CLEAR],
        OUT_REVERSE => [OUT_REVERSE, CLEAR, OUT_REVERSE, CLEAR],
        ATOP => [ATOP, IN, OVER, SRC],
        ATOP_REVERSE => [ATOP_REVERSE, OVER_REVERSE, IN_REVERSE, DST],
        XOR => [XOR, OUT, OUT_REVERSE, CLEAR],
        SATURATE => [SATURATE, OVER_REVERSE, DST, DST],
        DISJOINT_CLEAR | CONJOINT_CLEAR => [CLEAR; 4],
        DISJOINT_SRC | CONJOINT_SRC => [SRC; 4],
        DISJOINT_DST | CONJOINT_DST => [DST; 4],
        o => [o; 4],
    };
    t[(dst_opaque as usize) << 1 | src_opaque as usize]
}

/// _pixman_compute_composite_region32.
pub fn composite_region(dst: &Dest, dx: i32, dy: i32, w: i32, h: i32) -> Option<Region> {
    let b = Box { x1: dx.max(0), y1: dy.max(0), x2: dx.wrapping_add(w).min(dst.width), y2: dy.wrapping_add(h).min(dst.height) };
    if b.is_empty() {
        return None;
    }
    let mut r = Region::from_box(b);
    if let Some(c) = &dst.clip {
        r = r.intersect(c);
    }
    if let Some(a) = &dst.alpha {
        r = r.intersect_box(Box::new(a.x, a.y, a.width, a.height));
    }
    (!r.is_empty()).then_some(r)
}

/// compute_transformed_extents: a box's pixel centres through the
/// transform, in 48.16.
fn transformed_extents(t: Option<&Transform>, e: Box) -> Option<(i64, i64, i64, i64)> {
    let (x1, y1) = ((e.x1 << 16) as i64 + 0x8000, (e.y1 << 16) as i64 + 0x8000);
    let (x2, y2) = ((e.x2 << 16) as i64 - 0x8000, (e.y2 << 16) as i64 - 0x8000);
    let Some(t) = t else { return Some((x1, y1, x2, y2)) };
    let (mut tx1, mut ty1, mut tx2, mut ty2) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for i in 0..4 {
        let v = [if i & 1 != 0 { x1 } else { x2 } as i32, if i & 2 != 0 { y1 } else { y2 } as i32, FIXED_1];
        let v = matrix::point(t, v)?;
        let (tx, ty) = (v[0] as i64, v[1] as i64);
        tx1 = tx1.min(tx);
        ty1 = ty1.min(ty);
        tx2 = tx2.max(tx);
        ty2 = ty2.max(ty);
    }
    Some((tx1, ty1, tx2, ty2))
}

/// analyze_extent: None if the coordinates could overflow (nothing is
/// drawn); else whether the samples cover the clip, nearest and bilinear.
fn analyze_extent(img: &Image, e: Box) -> Option<(bool, bool)> {
    let s16 = |v: i32| (i16::MIN as i32..=i16::MAX as i32).contains(&v);
    if !s16(e.x1 - 1) || !s16(e.y1 - 1) || !s16(e.x2 + 1) || !s16(e.y2 + 1) {
        return None;
    }
    let t = img.transform.as_ref();
    let (x_off, y_off, width, height) = match &img.kind {
        Kind::Bits(b) => {
            if b.width >= 0x7fff || b.height >= 0x7fff {
                return None;
            }
            if t.is_none() && e.x1 >= 0 && e.y1 >= 0 && e.x2 <= b.width && e.y2 <= b.height {
                return Some((true, false));
            }
            match &img.filter {
                Filter::Convolution(p) => (-FIXED_E - ((p[0] - FIXED_1) >> 1), -FIXED_E - ((p[1] - FIXED_1) >> 1), p[0], p[1]),
                Filter::Bilinear => (-FIXED_1 / 2, -FIXED_1 / 2, FIXED_1, FIXED_1),
                Filter::Nearest => (-FIXED_E, -FIXED_E, 0, 0),
            }
        }
        _ => (0, 0, 0, 0),
    };
    let (mut near, mut bil) = (false, false);
    let te = transformed_extents(t, e)?;
    if let Kind::Bits(b) = &img.kind {
        let int = |v: i64| (v >> 16) as i32;
        if int(te.0 - 1) >= 0 && int(te.1 - 1) >= 0 && int(te.2 - 1) < b.width && int(te.3 - 1) < b.height {
            near = true;
        }
        if int(te.0 - 0x8000) >= 0 && int(te.1 - 0x8000) >= 0 && int(te.2 + 0x8000) < b.width && int(te.3 + 0x8000) < b.height {
            bil = true;
        }
    }
    let ex = Box { x1: e.x1 - 1, y1: e.y1 - 1, x2: e.x2 + 1, y2: e.y2 + 1 };
    let te = transformed_extents(t, ex)?;
    let fits = |v: i64| (-0x8000_0000..=0x7fff_ffff).contains(&v);
    if !fits(te.0 + x_off as i64 - 8) || !fits(te.1 + y_off as i64 - 8) || !fits(te.2 + x_off as i64 + 8 + width as i64) || !fits(te.3 + y_off as i64 + 8 + height as i64) {
        return None;
    }
    Some((near, bil))
}

/// pixman_image_composite32. Returns the dest box drawn in, if any.
#[allow(clippy::too_many_arguments)]
pub fn composite(o: u8, src: &Image, mask: Option<&Image>, dst: &mut Dest, sx: i32, sy: i32, mx: i32, my: i32, dx: i32, dy: i32, w: i32, h: i32) -> Option<Box> {
    let si = src.info();
    let mi = mask.map(|m| m.info());
    let region = composite_region(dst, dx, dy, w, h)?;
    let mut e = region.extents();
    e = Box { x1: e.x1 - (dx - sx), y1: e.y1 - (dy - sy), x2: e.x2 - (dx - sx), y2: e.y2 - (dy - sy) };
    let (sn, sb) = analyze_extent(src, e)?;
    let e = Box { x1: e.x1 - (sx - mx), y1: e.y1 - (sy - my), x2: e.x2 - (sx - mx), y2: e.y2 - (sy - my) };
    let promote = |i: &Info, (n, b): (bool, bool)| i.opaque || (i.samples_opaque && ((i.nearest && n) || (i.bilinear && b)));
    let src_opaque = promote(&si, (sn, sb));
    let mask_opaque = match (mask, mi) {
        (Some(m), Some(mi)) => {
            let cover = analyze_extent(m, e)?;
            mi.opaque || promote(&mi, cover)
        }
        _ => true,
    };
    let o = optimize(o, src_opaque && mask_opaque, dst.opaque());
    for b in region.boxes() {
        composite_rect(o, src, mask, dst, b.x1 + sx - dx, b.y1 + sy - dy, b.x1 + mx - dx, b.y1 + my - dy, b.x1, b.y1, b.width(), b.height());
    }
    Some(region.extents())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_convert_as_pixman() {
        let r5g6b5 = Format::new(16, TYPE_ARGB, 0, 5, 6, 5);
        assert_eq!(to_argb(r5g6b5, None, 0xf800), 0xffff_0000);
        assert_eq!(to_argb(r5g6b5, None, 0x07e0), 0xff00_ff00);
        assert_eq!(from_argb(r5g6b5, None, 0x80ff_8040), 0xfc08);
        assert_eq!(to_argb(Format::A1, None, 1), 0xff00_0000);
        assert_eq!(from_argb(Format::A1, None, 0x8000_0000), 1);
        assert_eq!(from_argb(Format::A1, None, 0x7f00_0000), 0);
        let bgra = Format::new(32, TYPE_BGRA, 8, 8, 8, 8);
        assert_eq!(to_argb(bgra, None, 0x1122_3344), 0x4433_2211);
        assert_eq!(float_to_unorm(0.5, 8), 128);
        assert_eq!(float_to_unorm(1.0, 8), 255);
    }

    #[test]
    fn over_and_saturate() {
        let mut px = vec![0x8000_0000u32; 4];
        let src = Image::new(Kind::Solid([0xffff, 0, 0, 0x8000]));
        let mut d = Dest { px: &mut px, stride: 2, width: 2, height: 2, format: Format::A8R8G8B8, indexed: None, clip: None, alpha: None, repeat: Repeat::None, convolution: false, component_alpha: false };
        assert_eq!(composite(op::OVER, &src, None, &mut d, 0, 0, 0, 0, 1, 0, 5, 5), Some(Box { x1: 1, y1: 0, x2: 2, y2: 2 }));
        assert_eq!(px, [0x8000_0000, 0xc0ff_0000, 0x8000_0000, 0xc0ff_0000]);
    }
}
