//! Fonts: X.org's bitmap fonts, bundled as X.org installs them (data/fonts,
//! see its COPYING), found by XLFD name through a font path as X.org's
//! fontfile code finds them; the font requests; and text drawing as fb does
//! it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::draw::{Fill, depth_mask};
use crate::mi::{self, Sink, Span};
use crate::server::*;
use crate::wire::{Reader, Reply};

mod bundled {
    include!(concat!(env!("OUT_DIR"), "/fonts.rs"));
}

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
    /// Where the ink is: QueryFont and QueryTextExtents report these, as
    /// X.org does.
    pub ink: Metrics,
    /// 64-bit words a row.
    pub words: usize,
    /// Rows of the bitmap, `rb - lb` wide, leftmost pixel in bit 0 of each
    /// row's first word.
    pub rows: Vec<u64>,
}

impl Glyph {
    pub fn row(&self, y: usize) -> &[u64] {
        &self.rows[y * self.words..(y + 1) * self.words]
    }

    pub fn height(&self) -> usize {
        self.rows.len() / self.words.max(1)
    }
}

pub enum PropValue {
    Atom(String),
    Int(i32),
}

pub struct Font {
    /// By character: rows `min_byte1..=max_byte1` of `min_char..=max_char`
    /// (the second byte) each.
    pub glyphs: Vec<Option<Glyph>>,
    pub min_char: u16,
    pub max_char: u16,
    pub min_byte1: u8,
    pub max_byte1: u8,
    pub default_char: u16,
    pub ascent: i16,
    pub descent: i16,
    pub min_bounds: Metrics,
    pub max_bounds: Metrics,
    pub all_exist: bool,
    pub properties: Vec<(String, PropValue)>,
}

impl Font {
    /// A BDF font, as bdftopcf would read it (for local font folders).
    pub fn parse_bdf(bdf: &str) -> Font {
        let mut f = Font {
            glyphs: Vec::new(),
            min_char: 0,
            max_char: 0,
            min_byte1: 0,
            max_byte1: 0,
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
                    let (a, b) = (
                        num(w.next()),
                        w.next().map(|s| s.parse::<i32>().unwrap_or(-1)),
                    );
                    enc = match (a, b) {
                        (-1, Some(b)) if b >= 0 => Some(b as u16),
                        (a, _) if (0..=255).contains(&a) => Some(a as u16),
                        _ => None,
                    };
                }
                "DWIDTH" => dwidth = num(w.next()) as i16,
                "BBX" => {
                    bbx = (
                        num(w.next()) as i16,
                        num(w.next()) as i16,
                        num(w.next()) as i16,
                        num(w.next()) as i16,
                    )
                }
                "BITMAP" => {
                    let (bw, bh, bx, by) = bbx;
                    let words = (bw.max(0) as usize).div_ceil(64).max(1);
                    let mut rows = vec![0u64; words * bh.max(0) as usize];
                    for y in 0..bh.max(0) as usize {
                        let hex = lines.next().unwrap_or("").trim().as_bytes();
                        for x in 0..(bw.max(0) as usize).min(hex.len() * 4) {
                            let nibble = (hex[x / 4] as char).to_digit(16).unwrap_or(0);
                            if nibble >> (3 - x % 4) & 1 != 0 {
                                rows[y * words + x / 64] |= 1 << (x % 64);
                            }
                        }
                    }
                    let m = Metrics {
                        lb: bx,
                        rb: bx + bw,
                        width: dwidth,
                        ascent: bh + by,
                        descent: -by,
                        attributes: 0,
                    };
                    let g = Glyph {
                        m,
                        ink: m,
                        words,
                        rows,
                    };
                    let ink = ink_of(&g);
                    if let Some(e) = enc {
                        chars.push((e, Glyph { ink, ..g }));
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
        // The bounds are of the ink, as X.org's pcf fonts have them.
        for (e, g) in chars {
            let m = g.ink;
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
        let name = bdf
            .lines()
            .find_map(|l| l.strip_prefix("FONT "))
            .unwrap_or("")
            .trim()
            .to_string();
        f.properties
            .insert(0, ("FONT".into(), PropValue::Atom(name)));
        f
    }

    /// A character's glyph: `c` is byte1 << 8 | byte2. The default
    /// character's when it has none; None when that has none either.
    pub fn glyph(&self, c: u16) -> Option<&Glyph> {
        self.exact(c).or_else(|| self.exact(self.default_char))
    }

    /// A character's own glyph.
    pub fn exact(&self, c: u16) -> Option<&Glyph> {
        let (b1, b2) = ((c >> 8) as u8, c & 0xff);
        if b1 < self.min_byte1 || b1 > self.max_byte1 || b2 < self.min_char || b2 > self.max_char {
            return None;
        }
        let cols = (self.max_char - self.min_char + 1) as usize;
        self.glyphs
            .get((b1 - self.min_byte1) as usize * cols + (b2 - self.min_char) as usize)?
            .as_ref()
    }
}

/// Where a glyph's ink is: all zero (but the width) for a blank one, as
/// X.org's fonts have it.
fn ink_of(g: &Glyph) -> Metrics {
    let m = g.m;
    let mut ink = Metrics {
        width: m.width,
        ..Metrics::default()
    };
    let h = g.height();
    let lit = |y: usize| g.row(y).iter().any(|&w| w != 0);
    let Some(top) = (0..h).find(|&y| lit(y)) else {
        return ink;
    };
    let bottom = (0..h).rev().find(|&y| lit(y)).unwrap();
    let w = (m.rb - m.lb).max(0) as usize;
    let on = |x: usize| (0..h).any(|y| g.row(y)[x / 64] >> (x % 64) & 1 != 0);
    let left = (0..w).find(|&x| on(x)).unwrap_or(0);
    let right = (0..w).rev().find(|&x| on(x)).unwrap_or(0);
    ink.lb = m.lb + left as i16;
    ink.rb = m.lb + right as i16 + 1;
    ink.ascent = m.ascent - top as i16;
    ink.descent = m.descent - (h - 1 - bottom) as i16;
    ink
}

/// Where a font's data is.
enum Source {
    Bundled(&'static [u8]),
    File(PathBuf),
}

enum Entry {
    Font(usize),
    /// An alias: a name or a pattern, looked up through the whole path.
    Alias(String),
}

/// A font path element: a fonts.dir and fonts.alias, names lowercased and
/// sorted (fonts and aliases together), as X.org's fontfile keeps them.
pub struct Dir {
    /// Its name in the font path, as given.
    name: String,
    entries: Vec<(String, Entry)>,
    files: Vec<Source>,
}

/// The names in a fonts.alias: an alias and its target, each a word or in
/// double quotes.
fn alias_lines(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        if rest.is_empty() || rest.starts_with('!') || rest.starts_with("FILE_NAMES_ALIASES") {
            continue;
        }
        let mut word = || -> Option<String> {
            rest = rest.trim_start();
            let (w, r) = if let Some(q) = rest.strip_prefix('"') {
                let end = q.find('"')?;
                (&q[..end], &q[end + 1..])
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                (&rest[..end], &rest[end..])
            };
            rest = r;
            (!w.is_empty()).then(|| w.to_ascii_lowercase())
        };
        if let (Some(a), Some(t)) = (word(), word()) {
            out.push((a, t));
        }
    }
    out
}

impl Dir {
    fn new(
        name: &str,
        fonts_dir: &str,
        fonts_alias: &str,
        mut file: impl FnMut(&str) -> Option<Source>,
    ) -> Dir {
        let mut entries = Vec::new();
        let mut files = Vec::new();
        for line in fonts_dir.lines().skip(1) {
            let Some((f, n)) = line.trim().split_once(char::is_whitespace) else {
                continue;
            };
            let n = n.trim().trim_matches('"').to_ascii_lowercase();
            if let Some(src) = file(f) {
                files.push(src);
                entries.push((n, Entry::Font(files.len() - 1)));
            }
        }
        for (a, t) in alias_lines(fonts_alias) {
            entries.push((a, Entry::Alias(t)));
        }
        // Sorted, the first of a name kept, as fontfile's table has it.
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        Dir {
            name: name.to_string(),
            entries,
            files,
        }
    }

    fn bundled(which: usize, name: &str) -> Dir {
        let (_, dir, alias, files) = bundled::BUNDLED[which];
        // misc also has CDE's names, as CDE's own fonts.alias would on its host.
        let alias = if which == 0 {
            format!("{alias}{}", include_str!("../data/fonts/cde.alias"))
        } else {
            alias.to_string()
        };
        Dir::new(name, dir, &alias, |f| {
            files
                .iter()
                .find(|x| x.0 == f)
                .map(|x| Source::Bundled(x.1))
        })
    }

    /// A folder on this machine with a fonts.dir.
    fn local(name: &str, path: &Path) -> Option<Dir> {
        let dir = std::fs::read(path.join("fonts.dir")).ok()?;
        let alias = std::fs::read(path.join("fonts.alias")).unwrap_or_default();
        let latin1 = |b: Vec<u8>| b.into_iter().map(|c| c as char).collect::<String>();
        Some(Dir::new(name, &latin1(dir), &latin1(alias), |f| {
            let lower = f.to_ascii_lowercase();
            let ok =
                lower.ends_with(".pcf") || lower.ends_with(".pcf.gz") || lower.ends_with(".bdf");
            ok.then(|| Source::File(path.join(f)))
        }))
    }
}

/// The bundled font directories' names in the font path, in X.org's order.
const DEFAULT_PATH: [&str; 3] = ["built-ins/misc/", "built-ins/75dpi/", "built-ins/100dpi/"];

/// How many aliases deep a name may go (X.org stops at 20).
const MAX_ALIAS_DEPTH: usize = 20;

/// The font path, and every font opened through it.
pub struct Library {
    path: Vec<Rc<Dir>>,
    /// Directories once in the path, by name, so a font opened twice is the same font.
    dirs: HashMap<String, Rc<Dir>>,
    open: HashMap<(String, usize), Rc<Font>>,
    /// Folders on this machine added after the bundled ones.
    extra: Vec<String>,
}

impl Default for Library {
    fn default() -> Library {
        Library::new(&[])
    }
}

impl Library {
    pub fn new(extra: &[String]) -> Library {
        let mut lib = Library {
            path: Vec::new(),
            dirs: HashMap::new(),
            open: HashMap::new(),
            extra: extra.to_vec(),
        };
        lib.set_path(&[]).ok();
        lib
    }

    /// The font path element called `name`: a bundled directory (by the
    /// name of its last part, as `xset fp+ /usr/lib/X11/fonts/75dpi` names
    /// it on the client's machine), or a folder here with a fonts.dir.
    fn element(&mut self, name: &str) -> Option<Rc<Dir>> {
        if let Some(d) = self.dirs.get(name) {
            return Some(d.clone());
        }
        let bare = name.strip_suffix(":unscaled").unwrap_or(name);
        let bare = bare.trim_end_matches('/');
        let path = Path::new(bare);
        let dir = if path.is_absolute() && path.join("fonts.dir").is_file() {
            Dir::local(name, path)?
        } else {
            let last = bare.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
            // STUB(x11): no font servers (tcp/host:7100 elements) and no other charsets than ISO 8859-1 bundled, nor vendors' fonts (HP VUE's hp-roman8 Bitstream ones): they fall back (phase 4)
            let which = match last.as_str() {
                "misc" | "built-ins" => 0,
                "75dpi" => 1,
                "100dpi" => 2,
                _ => return None,
            };
            Dir::bundled(which, name)
        };
        let dir = Rc::new(dir);
        self.dirs.insert(name.to_string(), dir.clone());
        Some(dir)
    }

    /// SetFontPath: no elements is the default path. Unchanged, and Err
    /// with the first bad element's index, if one isn't a font directory.
    pub fn set_path(&mut self, names: &[String]) -> Result<(), usize> {
        let default = names.is_empty();
        let names: Vec<String> = if default {
            DEFAULT_PATH
                .iter()
                .map(|s| s.to_string())
                .chain(self.extra.iter().cloned())
                .collect()
        } else {
            names.to_vec()
        };
        let mut path = Vec::new();
        for (i, n) in names.iter().enumerate() {
            match self.element(n) {
                Some(d) => path.push(d),
                // The default's own folders may be gone: they're left out.
                None if default => {
                    eprintln!("vintrdp: X11: no fonts.dir in {n}; left out of the font path")
                }
                None => return Err(i),
            }
        }
        self.path = path;
        Ok(())
    }

    pub fn path(&self) -> Vec<&str> {
        self.path.iter().map(|d| d.name.as_str()).collect()
    }

    /// The name of every font and alias that matches a pattern, through the
    /// path, each directory's in its sorted order.
    pub fn list(&self, pattern: &[u8]) -> Vec<String> {
        let pattern = pattern.to_ascii_lowercase();
        let mut out = Vec::new();
        for d in &self.path {
            out.extend(
                d.entries
                    .iter()
                    .filter(|e| matches(&pattern, e.0.as_bytes()))
                    .map(|e| e.0.clone()),
            );
        }
        out
    }

    /// The font a name or pattern opens: the first match in the first
    /// directory that has one, an alias looked up again from the start.
    pub fn open(&mut self, pattern: &[u8]) -> Option<Rc<Font>> {
        let mut name = pattern.to_ascii_lowercase();
        for _ in 0..MAX_ALIAS_DEPTH {
            let found = self.path.iter().find_map(|d| {
                d.entries
                    .iter()
                    .find(|e| matches(&name, e.0.as_bytes()))
                    .map(|e| (d.clone(), e))
            });
            let (dir, entry) = found?;
            match &entry.1 {
                Entry::Alias(target) => name = target.as_bytes().to_vec(),
                &Entry::Font(i) => return self.load(&dir, i),
            }
        }
        None
    }

    fn load(&mut self, dir: &Rc<Dir>, i: usize) -> Option<Rc<Font>> {
        let key = (dir.name.clone(), i);
        if let Some(f) = self.open.get(&key) {
            return Some(f.clone());
        }
        let font = match &dir.files[i] {
            Source::Bundled(data) => crate::pcf::load(data),
            Source::File(p) => match std::fs::read(p) {
                Ok(d) if p.to_string_lossy().to_ascii_lowercase().ends_with(".bdf") => Ok(
                    Font::parse_bdf(&d.iter().map(|&c| c as char).collect::<String>()),
                ),
                Ok(d) => crate::pcf::load(&d),
                Err(e) => Err(e.to_string()),
            },
        };
        match font {
            Ok(f) => {
                let f = Rc::new(f);
                self.open.insert(key, f.clone());
                Some(f)
            }
            Err(e) => {
                eprintln!("vintrdp: X11: font {}#{i}: {e}", dir.name);
                None
            }
        }
    }
}

/// XLFD pattern matching, as X.org's: `*` any run, `?` any one character.
fn matches(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star = None;
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

fn charinfo(r: &mut Reply, m: &Metrics) {
    r.i16(m.lb)
        .i16(m.rb)
        .i16(m.width)
        .i16(m.ascent)
        .i16(m.descent)
        .u16(m.attributes);
}

/// Text: 8-bit characters, or 16-bit ones as byte1, byte2.
fn chars(s: &[u8], wide: bool) -> Vec<u16> {
    if wide {
        s.chunks_exact(2)
            .map(|c| (c[0] as u16) << 8 | c[1] as u16)
            .collect()
    } else {
        s.iter().map(|&c| c as u16).collect()
    }
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
        let name = r.bytes(n)?;
        let Some(f) = self.fonts.open(name) else {
            if self.verbose {
                eprintln!("vintrdp: X11: no font {}", String::from_utf8_lossy(name));
            }
            return Err(XError(BAD_NAME, 0));
        };
        self.resources.insert(id, Resource::Font(f));
        Ok(None)
    }

    /// QueryFont's and ListFontsWithInfo's common part, to the property count.
    fn font_info(&mut self, reply: &mut Reply, f: &Font) {
        charinfo(reply, &f.min_bounds);
        reply.zeros(4);
        charinfo(reply, &f.max_bounds);
        reply.zeros(4);
        reply
            .u16(f.min_char)
            .u16(f.max_char)
            .u16(f.default_char)
            .u16(f.properties.len() as u16);
        reply
            .u8(0)
            .u8(f.min_byte1)
            .u8(f.max_byte1)
            .u8(f.all_exist as u8)
            .i16(f.ascent)
            .i16(f.descent);
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
            charinfo(&mut reply, &g.as_ref().map(|g| g.ink).unwrap_or_default());
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
        let (mut width, mut left, mut right, mut ascent, mut descent) =
            (0i32, 0i32, 0i32, i16::MIN as i32, i16::MIN as i32);
        let mut first = true;
        for c in text {
            let Some(g) = f.glyph(c) else { continue };
            let m = g.ink;
            if first {
                (left, right, ascent, descent, first) = (
                    m.lb as i32,
                    m.rb as i32,
                    m.ascent as i32,
                    m.descent as i32,
                    false,
                );
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
        reply
            .i16(f.ascent)
            .i16(f.descent)
            .i16(ascent as i16)
            .i16(descent as i16)
            .u32(width as u32)
            .u32(left as u32)
            .u32(right as u32);
        Ok(Some(reply.finish()))
    }

    pub fn list_fonts(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let max = r.u16()? as usize;
        let n = r.u16()? as usize;
        let names: Vec<String> = self.fonts.list(r.bytes(n)?).into_iter().take(max).collect();
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
        let names: Vec<String> = self.fonts.list(r.bytes(n)?).into_iter().take(max).collect();
        let count = names.len();
        for (i, name) in names.into_iter().enumerate() {
            let Some(f) = self.fonts.open(name.as_bytes()) else {
                continue;
            };
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
            for row in 0..g.height() {
                let bits = g.row(row);
                let on = |x: usize| bits[x / 64] >> (x % 64) & 1 != 0;
                let w = bits.len() * 64;
                let mut x = 0;
                while x < w {
                    if !on(x) {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    while x < w && on(x) {
                        x += 1;
                    }
                    spans.push(Span {
                        x: pen + g.m.lb as i32 + start as i32,
                        y: top + row as i32,
                        w: (x - start) as i32,
                    });
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
        let Some(place) = self.place(&d, &gc) else {
            return Ok(());
        };
        let fill = gc.fill(place.dx, place.dy);
        let fg = gc.fg & depth_mask(gc.depth);
        self.with_canvas(&d, &place, gc.function, gc.plane_mask, &fill, gc.bg, |c| {
            c.fill_spans(fg, spans)
        });
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
        let Some(place) = self.place(&d, &gc) else {
            return Ok(None);
        };
        // The background box, then the glyphs: GXcopy and solid whatever the GC says.
        let mut spans = Vec::new();
        let width = Self::glyph_spans(&font, &text, x, y, &mut spans);
        let (bx, bw) = if width < 0 {
            (x + width, -width)
        } else {
            (x, width)
        };
        let back = mi::Rect {
            x: bx,
            y: y - font.ascent as i32,
            w: bw,
            h: font.ascent as i32 + font.descent as i32,
        };
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
        assert_eq!(
            (f.ascent, f.descent, f.min_char, f.max_char),
            (11, 2, 0, 255)
        );
        assert_eq!((f.max_bounds.width, f.min_bounds.width), (6, 6));
        let a = f.glyph(b'A' as u16).unwrap();
        assert_eq!(a.m.width, 6);
        assert!(a.rows.iter().any(|&r| r != 0));
        // The font, and misc's alias for it at 100 dpi.
        assert_eq!(
            lib.list(b"*-FIXED-BOLD-R-SEMICONDENSED-*"),
            [
                "-misc-fixed-bold-r-semicondensed--13-100-100-100-c-60-iso8859-1",
                "-misc-fixed-bold-r-semicondensed--13-120-75-75-c-60-iso8859-1"
            ]
        );
        assert_eq!(lib.list(b"6x1?"), ["6x10", "6x12", "6x13"]);
        assert!(lib.open(b"nonesuch").is_none());
        let c = lib.open(b"cursor").unwrap();
        assert_eq!((c.min_char, c.max_char), (0, 153));
        // XC_xterm (152) and its mask.
        assert!(c.glyph(152).is_some() && c.glyph(153).is_some());
    }

    #[test]
    fn xlfd() {
        let mut lib = Library::default();
        // variable is an alias for a pattern, found in 75dpi.
        let v = lib.open(b"variable").unwrap();
        let name = |f: &Font| match &f.properties.iter().find(|p| p.0 == "FONT").unwrap().1 {
            PropValue::Atom(s) => s.to_ascii_lowercase(),
            _ => unreachable!(),
        };
        assert_eq!(
            name(&v),
            "-adobe-helvetica-bold-r-normal--12-120-75-75-p-70-iso8859-1"
        );
        // The first match in the first directory with one: 75dpi before 100dpi.
        let t = lib
            .open(b"-*-times-medium-r-normal--*-140-*-*-*-*-iso8859-1")
            .unwrap();
        assert_eq!(
            name(&t),
            "-adobe-times-medium-r-normal--14-140-75-75-p-74-iso8859-1"
        );
        // Sizes that aren't there aren't made up.
        assert!(
            lib.open(b"-adobe-helvetica-medium-r-normal--13-*-*-*-*-*-iso8859-1")
                .is_none()
        );
        assert!(
            lib.open(b"-adobe-helvetica-medium-r-normal--0-0-75-75-p-0-iso8859-1")
                .is_none()
        );
        let all = lib.list(b"*");
        assert!(all.len() > 400 && all.iter().all(|n| !n.contains("--0-0-")));
        assert_eq!(
            lib.list(b"-adobe-helvetica-bold-r-normal--*-*-*-*-*-*-iso8859-1")
                .len(),
            12
        );
        assert!(lib.open(b"lucidasans-12").is_some());
        assert!(lib.open(b"olglyph-12").is_some() && lib.open(b"nil2").is_some());
        // CDE's names.
        assert!(
            lib.open(b"-dt-interface user-medium-r-normal-s*-*-*-*-*-*-*-*-*")
                .is_some()
        );
        for name in lib.list(b"-dt-*") {
            assert!(lib.open(name.as_bytes()).is_some(), "{name}");
        }
        // Every bundled font reads.
        for (d, _, _, files) in bundled::BUNDLED {
            for (f, data) in *files {
                crate::pcf::load(data).unwrap_or_else(|e| panic!("{d}/{f}: {e}"));
            }
        }
    }

    #[test]
    fn font_path() {
        let mut lib = Library::default();
        assert_eq!(lib.path(), DEFAULT_PATH);
        // A client's own folder names, mapped to the bundled ones; others refused.
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            lib.set_path(&p(&[
                "/usr/lib/X11/fonts/misc/",
                "/usr/lib/X11/fonts/100dpi/:unscaled"
            ])),
            Ok(())
        );
        assert_eq!(
            lib.list(b"-adobe-times-medium-r-normal--14-*")[0],
            "-adobe-times-medium-r-normal--14-100-100-100-p-74-iso8859-1"
        );
        assert_eq!(
            lib.set_path(&p(&["/usr/lib/X11/fonts/misc/", "tcp/fonts:7100"])),
            Err(1)
        );
        assert_eq!(lib.path().len(), 2);
        lib.set_path(&[]).unwrap();
        assert_eq!(lib.path().len(), 3);
    }

    #[test]
    fn patterns() {
        assert!(matches(b"*", b""));
        assert!(matches(b"-*-fixed-*", b"-misc-fixed-medium-r-normal--13"));
        assert!(matches(b"a?c", b"a-c"));
        assert!(!matches(b"a*d", b"abc"));
        assert!(matches(b"*x*y", b"axbxcy"));
    }
}
