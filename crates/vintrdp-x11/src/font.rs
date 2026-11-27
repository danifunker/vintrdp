//! Fonts: X.org's bitmap fonts, bundled as BDF (data/, see data/COPYING),
//! the font requests, and text drawing as fb does it.

use std::rc::Rc;

use crate::draw::{depth_mask, Fill};
use crate::mi::{self, Sink, Span};
use crate::server::*;
use crate::wire::{Reader, Reply};

// STUB(x11): only fixed (6x13, medium and bold, ISO 8859-1) and cursor are bundled; the full misc/75dpi/100dpi set and XLFD matching with scaling are phase 2
const FONTS: [(&str, &str); 3] = [
    ("-misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso8859-1", include_str!("../data/6x13-ISO8859-1.bdf")),
    ("-misc-fixed-bold-r-semicondensed--13-120-75-75-c-60-iso8859-1", include_str!("../data/6x13B-ISO8859-1.bdf")),
    ("cursor", include_str!("../data/cursor.bdf")),
];

/// X.org's misc/fonts.alias, for the fonts here.
const ALIASES: [(&str, &str); 4] = [
    ("fixed", "-misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso8859-1"),
    ("6x13", "-misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso8859-1"),
    ("6x13bold", "-misc-fixed-bold-r-semicondensed--13-120-75-75-c-60-iso8859-1"),
    ("variable", "-misc-fixed-bold-r-semicondensed--13-120-75-75-c-60-iso8859-1"),
];

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub lb: i16,
    pub rb: i16,
    pub width: i16,
    pub ascent: i16,
    pub descent: i16,
    pub attributes: u16,
}

pub struct Glyph {
    pub m: Metrics,
    /// Rows of the bitmap, `rb - lb` wide, leftmost pixel in bit 0.
    pub rows: Vec<u64>,
}

pub enum PropValue {
    Atom(String),
    Int(i32),
}

pub struct Font {
    /// By character, from `min_char`.
    pub glyphs: Vec<Option<Glyph>>,
    pub min_char: u16,
    pub max_char: u16,
    pub default_char: u16,
    pub ascent: i16,
    pub descent: i16,
    pub min_bounds: Metrics,
    pub max_bounds: Metrics,
    pub all_exist: bool,
    pub properties: Vec<(String, PropValue)>,
}

impl Font {
    /// A BDF font, as bdftopcf would read it.
    pub fn parse(bdf: &str) -> Font {
        let mut f = Font {
            glyphs: Vec::new(),
            min_char: 0,
            max_char: 0,
            default_char: 0,
            ascent: 0,
            descent: 0,
            min_bounds: Metrics::default(),
            max_bounds: Metrics::default(),
            all_exist: true,
            properties: Vec::new(),
        };
        let mut chars: Vec<(u16, Glyph)> = Vec::new();
        let mut lines = bdf.lines();
        let mut in_props = false;
        let (mut enc, mut dwidth, mut bbx) = (None::<u16>, 0i16, (0i16, 0i16, 0i16, 0i16));
        let mut default_char = None;
        while let Some(line) = lines.next() {
            let mut w = line.split_whitespace();
            let key = w.next().unwrap_or("");
            let num = |s: Option<&str>| s.and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
            match key {
                "STARTPROPERTIES" => in_props = true,
                "ENDPROPERTIES" => in_props = false,
                _ if in_props => {
                    let rest = line[key.len()..].trim();
                    let value = if let Some(s) = rest.strip_prefix('"') {
                        PropValue::Atom(s.strip_suffix('"').unwrap_or(s).replace("\"\"", "\""))
                    } else {
                        PropValue::Int(rest.parse().unwrap_or(0))
                    };
                    match (key, &value) {
                        ("FONT_ASCENT", PropValue::Int(v)) => f.ascent = *v as i16,
                        ("FONT_DESCENT", PropValue::Int(v)) => f.descent = *v as i16,
                        ("DEFAULT_CHAR", PropValue::Int(v)) => default_char = Some(*v as u16),
                        _ => {}
                    }
                    f.properties.push((key.into(), value));
                }
                "ENCODING" => {
                    let (a, b) = (num(w.next()), w.next().map(|s| s.parse::<i32>().unwrap_or(-1)));
                    enc = match (a, b) {
                        (-1, Some(b)) if b >= 0 => Some(b as u16),
                        (a, _) if a >= 0 => Some(a as u16),
                        _ => None,
                    };
                }
                "DWIDTH" => dwidth = num(w.next()) as i16,
                "BBX" => bbx = (num(w.next()) as i16, num(w.next()) as i16, num(w.next()) as i16, num(w.next()) as i16),
                "BITMAP" => {
                    let (bw, bh, bx, by) = bbx;
                    let mut rows = Vec::with_capacity(bh.max(0) as usize);
                    for _ in 0..bh.max(0) {
                        let hex = lines.next().unwrap_or("").trim();
                        let v = u64::from_str_radix(hex, 16).unwrap_or(0);
                        let bits = hex.len() as u32 * 4;
                        // The leftmost pixel is the hex's top bit; put it in bit 0.
                        let row = (0..bw.max(0) as u32).filter(|&x| x < bits && v >> (bits - 1 - x) & 1 != 0).fold(0u64, |r, x| r | 1 << x);
                        rows.push(row);
                    }
                    let m = Metrics { lb: bx, rb: bx + bw, width: dwidth, ascent: bh + by, descent: -by, attributes: 0 };
                    if let Some(e) = enc {
                        chars.push((e, Glyph { m, rows }));
                    }
                }
                _ => {}
            }
        }
        chars.sort_by_key(|c| c.0);
        f.min_char = chars.first().map_or(0, |c| c.0);
        f.max_char = chars.last().map_or(0, |c| c.0);
        f.glyphs = (f.min_char..=f.max_char).map(|_| None).collect();
        let mut first = true;
        for (e, g) in chars {
            let m = g.m;
            if first {
                (f.min_bounds, f.max_bounds, first) = (m, m, false);
            } else {
                let (lo, hi) = (&mut f.min_bounds, &mut f.max_bounds);
                lo.lb = lo.lb.min(m.lb);
                lo.rb = lo.rb.min(m.rb);
                lo.width = lo.width.min(m.width);
                lo.ascent = lo.ascent.min(m.ascent);
                lo.descent = lo.descent.min(m.descent);
                hi.lb = hi.lb.max(m.lb);
                hi.rb = hi.rb.max(m.rb);
                hi.width = hi.width.max(m.width);
                hi.ascent = hi.ascent.max(m.ascent);
                hi.descent = hi.descent.max(m.descent);
            }
            f.glyphs[(e - f.min_char) as usize] = Some(g);
        }
        f.all_exist = f.glyphs.iter().all(|g| g.is_some());
        f.default_char = default_char.unwrap_or(0);
        f.properties.retain(|p| !p.0.starts_with('_'));
        f.properties.insert(0, ("FONT".into(), PropValue::Atom(bdf_font_name(bdf))));
        f
    }

    pub fn glyph(&self, c: u16) -> Option<&Glyph> {
        let get = |c: u16| c.checked_sub(self.min_char).and_then(|i| self.glyphs.get(i as usize)).and_then(|g| g.as_ref());
        get(c).or_else(|| get(self.default_char))
    }
}

fn bdf_font_name(bdf: &str) -> String {
    bdf.lines().find_map(|l| l.strip_prefix("FONT ")).unwrap_or("").trim().to_string()
}

/// The bundled fonts, parsed when first opened.
#[derive(Default)]
pub struct Library {
    open: Vec<(usize, Rc<Font>)>,
}

impl Library {
    /// The name of every font and alias that matches a pattern.
    pub fn list(&self, pattern: &[u8]) -> Vec<&'static str> {
        let pattern = pattern.to_ascii_lowercase();
        FONTS.iter().map(|f| f.0).chain(ALIASES.iter().map(|a| a.0)).filter(|n| matches(&pattern, n.as_bytes())).collect()
    }

    pub fn open(&mut self, pattern: &[u8]) -> Option<Rc<Font>> {
        let name = *self.list(pattern).first()?;
        let name = ALIASES.iter().find(|a| a.0 == name).map_or(name, |a| a.1);
        let i = FONTS.iter().position(|f| f.0 == name)?;
        if let Some(f) = self.open.iter().find(|o| o.0 == i) {
            return Some(f.1.clone());
        }
        let f = Rc::new(Font::parse(FONTS[i].1));
        self.open.push((i, f.clone()));
        Some(f)
    }
}

/// XLFD pattern matching: `*` any run, `?` any one character.
fn matches(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => matches(&p[1..], s) || (!s.is_empty() && matches(p, &s[1..])),
        (Some(b'?'), Some(_)) => matches(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => matches(&p[1..], &s[1..]),
        _ => false,
    }
}

fn charinfo(r: &mut Reply, m: &Metrics) {
    r.i16(m.lb).i16(m.rb).i16(m.width).i16(m.ascent).i16(m.descent).u16(m.attributes);
}

/// Text: 8-bit characters, or 16-bit ones as byte1, byte2.
fn chars(s: &[u8], wide: bool) -> Vec<u16> {
    if wide { s.chunks_exact(2).map(|c| (c[0] as u16) << 8 | c[1] as u16).collect() } else { s.iter().map(|&c| c as u16).collect() }
}

impl State {
    pub fn font(&self, id: u32) -> Result<Rc<Font>, XError> {
        match self.resources.get(&id) {
            Some(Resource::Font(f)) => Ok(f.clone()),
            _ => Err(XError(BAD_FONT, id)),
        }
    }

    /// A font, or a GC's (its font).
    fn fontable(&mut self, id: u32) -> Result<Rc<Font>, XError> {
        match self.resources.get(&id) {
            Some(Resource::Font(f)) => Ok(f.clone()),
            Some(Resource::Gc(gc)) => match gc.font.clone() {
                Some(f) => Ok(f),
                None => Ok(self.default_font()),
            },
            _ => Err(XError(BAD_FONT, id)),
        }
    }

    pub fn default_font(&mut self) -> Rc<Font> {
        self.fonts.open(b"fixed").expect("fixed is bundled")
    }

    pub fn open_font(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.new_id(ctx, r.u32()?)?;
        let n = r.u16()? as usize;
        r.skip(2)?;
        let f = self.fonts.open(r.bytes(n)?).ok_or(XError(BAD_NAME, 0))?;
        self.resources.insert(id, Resource::Font(f));
        Ok(None)
    }

    /// QueryFont's and ListFontsWithInfo's common part, to the property count.
    fn font_info(&mut self, reply: &mut Reply, f: &Font) {
        charinfo(reply, &f.min_bounds);
        reply.zeros(4);
        charinfo(reply, &f.max_bounds);
        reply.zeros(4);
        reply.u16(f.min_char).u16(f.max_char).u16(f.default_char).u16(f.properties.len() as u16);
        reply.u8(0).u8(0).u8(0).u8(f.all_exist as u8).i16(f.ascent).i16(f.descent);
    }

    fn font_properties(&mut self, reply: &mut Reply, f: &Font) {
        for (name, value) in &f.properties {
            let atom = self.atoms.intern(name.as_bytes());
            let v = match value {
                PropValue::Atom(s) => self.atoms.intern(s.as_bytes()),
                PropValue::Int(i) => *i as u32,
            };
            reply.u32(atom).u32(v);
        }
    }

    pub fn query_font(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let f = self.fontable(r.u32()?)?;
        let mut reply = ctx.reply(0);
        self.font_info(&mut reply, &f);
        reply.u32(f.glyphs.len() as u32);
        self.font_properties(&mut reply, &f);
        for g in &f.glyphs {
            charinfo(&mut reply, &g.as_ref().map(|g| g.m).unwrap_or_default());
        }
        Ok(Some(reply.finish()))
    }

    pub fn query_text_extents(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let odd = ctx.data != 0;
        let f = self.fontable(r.u32()?)?;
        let mut s = r.bytes(r.remaining())?;
        if odd && s.len() >= 2 {
            s = &s[..s.len() - 2];
        }
        let text = chars(s, true);
        let (mut width, mut left, mut right, mut ascent, mut descent) = (0i32, 0i32, 0i32, i16::MIN as i32, i16::MIN as i32);
        let mut first = true;
        for c in text {
            let Some(g) = f.glyph(c) else { continue };
            let m = g.m;
            if first {
                (left, right, ascent, descent, first) = (m.lb as i32, m.rb as i32, m.ascent as i32, m.descent as i32, false);
            } else {
                left = left.min(width + m.lb as i32);
                right = right.max(width + m.rb as i32);
                ascent = ascent.max(m.ascent as i32);
                descent = descent.max(m.descent as i32);
            }
            width += m.width as i32;
        }
        if first {
            (ascent, descent) = (0, 0);
        }
        let mut reply = ctx.reply(0);
        reply.i16(f.ascent).i16(f.descent).i16(ascent as i16).i16(descent as i16).u32(width as u32).u32(left as u32).u32(right as u32);
        Ok(Some(reply.finish()))
    }

    pub fn list_fonts(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let max = r.u16()? as usize;
        let n = r.u16()? as usize;
        let names: Vec<&str> = self.fonts.list(r.bytes(n)?).into_iter().take(max).collect();
        let mut reply = ctx.reply(0);
        reply.u16(names.len() as u16).zeros(22);
        for name in names {
            reply.u8(name.len() as u8).bytes(name.as_bytes());
        }
        Ok(Some(reply.finish()))
    }

    pub fn list_fonts_with_info(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let max = r.u16()? as usize;
        let n = r.u16()? as usize;
        let names: Vec<&str> = self.fonts.list(r.bytes(n)?).into_iter().take(max).collect();
        let count = names.len();
        for (i, name) in names.into_iter().enumerate() {
            let Some(f) = self.fonts.open(name.as_bytes()) else { continue };
            let mut reply = ctx.reply(name.len() as u8);
            self.font_info(&mut reply, &f);
            reply.u32((count - 1 - i) as u32);
            self.font_properties(&mut reply, &f);
            reply.bytes(name.as_bytes());
            self.send_bytes(ctx.slot, reply.finish());
        }
        // The reply that ends the list.
        let mut reply = ctx.reply(0);
        reply.zeros(52);
        Ok(Some(reply.finish()))
    }

    /// Glyph pixels as spans, the origin at (x, y); returns the advance.
    fn glyph_spans(f: &Font, text: &[u16], x: i32, y: i32, spans: &mut Vec<Span>) -> i32 {
        let mut pen = x;
        for &c in text {
            let Some(g) = f.glyph(c) else { continue };
            let top = y - g.m.ascent as i32;
            for (row, &bits) in g.rows.iter().enumerate() {
                let mut b = bits;
                while b != 0 {
                    let start = b.trailing_zeros() as i32;
                    let run = (b >> start).trailing_ones() as i32;
                    spans.push(Span { x: pen + g.m.lb as i32 + start, y: top + row as i32, w: run });
                    b &= !(((1u64 << run) - 1) << start);
                }
            }
            pen += g.m.width as i32;
        }
        pen - x
    }

    pub fn poly_text(&mut self, _ctx: &Ctx, r: &mut Reader, wide: bool) -> Answer {
        let gc_id = {
            let d = r.u32()?;
            let g = r.u32()?;
            self.target(d, g)?;
            (d, g)
        };
        let (mut x, y) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
        let mut spans = Vec::new();
        let mut font = self.fontable(gc_id.1)?;
        // Items: a length and a delta, then the characters; or 255 and a font,
        // most significant byte first whatever the client's order.
        while r.remaining() >= 2 {
            let len = r.u8()?;
            if len == 255 {
                let b = r.bytes(4)?;
                let id = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                let f = self.font(id)?;
                self.gc(gc_id.1)?.font = Some(f.clone());
                // What's drawn so far is in the old font: draw it now.
                self.draw_text_spans(gc_id, &spans)?;
                spans.clear();
                font = f;
                continue;
            }
            let delta = r.u8()? as i8 as i32;
            let s = r.bytes(len as usize * if wide { 2 } else { 1 })?;
            x += delta;
            x += Self::glyph_spans(&font, &chars(s, wide), x, y, &mut spans);
        }
        self.draw_text_spans(gc_id, &spans)?;
        Ok(None)
    }

    /// PolyText's glyphs: spans in the GC's fill.
    fn draw_text_spans(&mut self, (d, g): (u32, u32), spans: &[Span]) -> Result<(), XError> {
        if spans.is_empty() {
            return Ok(());
        }
        let (d, gc) = self.target(d, g)?;
        let Some(place) = self.place(&d, &gc) else { return Ok(()) };
        let fill = gc.fill(place.dx, place.dy);
        let fg = gc.fg & depth_mask(gc.depth);
        self.with_canvas(&d, &place, gc.function, gc.plane_mask, &fill, gc.bg, |c| c.fill_spans(fg, spans));
        Ok(())
    }

    pub fn image_text(&mut self, ctx: &Ctx, r: &mut Reader, wide: bool) -> Answer {
        let n = ctx.data as usize;
        let (d, gc) = self.target(r.u32()?, r.u32()?)?;
        let g = r_gc(r)?;
        let (x, y) = g;
        let s = r.bytes(n * if wide { 2 } else { 1 })?;
        let font = match gc.font.clone() {
            Some(f) => f,
            None => self.default_font(),
        };
        let text = chars(s, wide);
        let Some(place) = self.place(&d, &gc) else { return Ok(None) };
        // The background box, then the glyphs: GXcopy and solid whatever the GC says.
        let mut spans = Vec::new();
        let width = Self::glyph_spans(&font, &text, x, y, &mut spans);
        let (bx, bw) = if width < 0 { (x + width, -width) } else { (x, width) };
        let back = mi::Rect { x: bx, y: y - font.ascent as i32, w: bw, h: font.ascent as i32 + font.descent as i32 };
        let mask = depth_mask(gc.depth);
        self.with_canvas(&d, &place, 3, gc.plane_mask, &Fill::Solid, 0, |c| {
            c.fill_rects(gc.bg & mask, &[back]);
            c.fill_spans(gc.fg & mask, &spans);
        });
        Ok(None)
    }
}

fn r_gc(r: &mut Reader) -> Result<(i32, i32), XError> {
    Ok((r.u16()? as i16 as i32, r.u16()? as i16 as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_fonts() {
        let mut lib = Library::default();
        let f = lib.open(b"fixed").unwrap();
        assert_eq!((f.ascent, f.descent, f.min_char, f.max_char), (11, 2, 0, 255));
        assert_eq!((f.max_bounds.width, f.min_bounds.width), (6, 6));
        // 'A': in the box, with ink.
        let a = f.glyph(b'A' as u16).unwrap();
        assert_eq!(a.m.width, 6);
        assert!(a.rows.iter().any(|&r| r != 0));
        assert_eq!(lib.list(b"*-FIXED-BOLD-*"), ["-misc-fixed-bold-r-semicondensed--13-120-75-75-c-60-iso8859-1"]);
        assert_eq!(lib.list(b"6x1?"), ["6x13"]);
        assert!(lib.open(b"nonesuch").is_none());
        let c = lib.open(b"cursor").unwrap();
        assert_eq!((c.min_char, c.max_char), (0, 153));
        // XC_xterm (152) and its mask.
        assert!(c.glyph(152).is_some() && c.glyph(153).is_some());
    }
}
