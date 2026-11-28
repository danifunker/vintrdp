//! Colormaps as X.org keeps them: a port of its dix/colormap.c
//! (CreateColormap, AllocColor and FindColor, QueryColors, AllocColorCells,
//! AllocColorPlanes and what they use, FreeColors, FreeClientPixels,
//! CopyColormapAndFree, StoreColors) and of mi/micmap.c (miResolveColor,
//! miInitializeColormap, miCreateDefColormap). Pixels are handed out in the
//! same order, with the same errors and error values, as Xvfb's.
//!
//! A pure data structure: no protocol, no windows. The caller looks maps up,
//! checks the request's own fields (`contiguous` being 0 or 1, the
//! request's length) and sends ColormapNotify.
//!
//! X.org's notices for the ported files follow.
//!
//! dix/colormap.c:
//!
//! Copyright 1987, 1998  The Open Group
//!
//! Permission to use, copy, modify, distribute, and sell this software and its
//! documentation for any purpose is hereby granted without fee, provided that
//! the above copyright notice appear in all copies and that both that
//! copyright notice and this permission notice appear in supporting
//! documentation.
//!
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL THE
//! OPEN GROUP BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN
//! AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
//! CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
//!
//! Except as contained in this notice, the name of The Open Group shall not be
//! used in advertising or otherwise to promote the sale, use or other dealings
//! in this Software without prior written authorization from The Open Group.
//!
//! Copyright 1987 by Digital Equipment Corporation, Maynard, Massachusetts.
//!
//! All Rights Reserved
//!
//! Permission to use, copy, modify, and distribute this software and its
//! documentation for any purpose and without fee is hereby granted,
//! provided that the above copyright notice appear in all copies and that
//! both that copyright notice and this permission notice appear in
//! supporting documentation, and that the name of Digital not be
//! used in advertising or publicity pertaining to distribution of the
//! software without specific, written prior permission.
//!
//! DIGITAL DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS SOFTWARE, INCLUDING
//! ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS, IN NO EVENT SHALL
//! DIGITAL BE LIABLE FOR ANY SPECIAL, INDIRECT OR CONSEQUENTIAL DAMAGES OR
//! ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
//! WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
//! ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS
//! SOFTWARE.
//!
//! mi/micmap.c:
//!
//! Copyright (c) 1987, Oracle and/or its affiliates. All rights reserved.
//!
//! Permission is hereby granted, free of charge, to any person obtaining a
//! copy of this software and associated documentation files (the "Software"),
//! to deal in the Software without restriction, including without limitation
//! the rights to use, copy, modify, merge, publish, distribute, sublicense,
//! and/or sell copies of the Software, and to permit persons to whom the
//! Software is furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice (including the next
//! paragraph) shall be included in all copies or substantial portions of the
//! Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL
//! THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//! FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//! DEALINGS IN THE SOFTWARE.

// Nothing in the server uses it yet.


use std::collections::HashMap;

pub const BAD_VALUE: u8 = 2;
pub const BAD_MATCH: u8 = 8;
pub const BAD_ACCESS: u8 = 10;
pub const BAD_ALLOC: u8 = 11;

#[allow(dead_code)]
pub const DO_RED: u8 = 1;
#[allow(dead_code)]
pub const DO_GREEN: u8 = 2;
#[allow(dead_code)]
pub const DO_BLUE: u8 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    StaticGray = 0,
    GrayScale = 1,
    StaticColor = 2,
    PseudoColor = 3,
    TrueColor = 4,
    DirectColor = 5,
}

impl Class {
    /// X.org's `class & DynamicClass`: the cells can be written.
    pub fn dynamic(self) -> bool {
        self as u8 & 1 != 0
    }
    /// `(class | DynamicClass) == DirectColor`: a map per channel.
    fn direct(self) -> bool {
        matches!(self, Class::TrueColor | Class::DirectColor)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Visual {
    pub class: Class,
    pub bits_per_rgb: u8,
    pub entries: u16,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
}

impl Visual {
    /// The visual's nplanes (its depth): the masks' bits for the classes with
    /// subfields, enough bits to number the entries for the rest.
    fn planes(&self) -> u32 {
        if self.class.direct() { (self.red_mask | self.green_mask | self.blue_mask).count_ones() } else { 32 - (self.entries.max(1) as u32 - 1).leading_zeros() }
    }
    fn masks(&self) -> [u32; 3] {
        [self.red_mask, self.green_mask, self.blue_mask]
    }
}

/// An X error: its code and its error value. The value is None where X.org
/// leaves the client's errorValue as an earlier request set it (BadAccess
/// and BadAlloc mostly); send whatever that was, or 0.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CmapError(pub u8, pub Option<u32>);

/// refcnt of a cell one client owns writable; above 0 it's shared read-only,
/// 0 is free.
const PRIVATE: i32 = -1;
/// Marks a freed entry in a client's pixel list, as X.org's ~0 does.
const ZAPPED: u32 = u32::MAX;

/// Which of a map's tables a call works on: X.org's PSEUDOMAP, REDMAP,
/// GREENMAP and BLUEMAP. Pseudo is the one table of the classes without
/// subfields, kept where Red is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chan {
    Pseudo,
    Red,
    Green,
    Blue,
}

impl Chan {
    fn idx(self) -> usize {
        match self {
            Chan::Pseudo | Chan::Red => 0,
            Chan::Green => 1,
            Chan::Blue => 2,
        }
    }
}

const RGB: [Chan; 3] = [Chan::Red, Chan::Green, Chan::Blue];

#[derive(Clone, Default, Debug)]
struct Entry {
    refcnt: i32,
    /// The cell's colour; in the subfield tables only the table's own
    /// channel means anything.
    rgb: [u16; 3],
    /// For cells from AllocColorPlanes on the classes without subfields:
    /// each channel's value lives in a shared slot (X.org's SHAREDCOLOR),
    /// which every cell with that channel's plane bits uses.
    shared: Option<[usize; 3]>,
}

/// A slot shared by cells of an AllocColorPlanes group. `refs` counts the
/// cells that use it; at 0 the slot is free for reuse. X.org keeps its own
/// count, which it gets wrong when a group has no green or blue planes; only
/// its freeing (which then goes wrong) depends on it, never a colour.
#[derive(Clone, Copy, Debug)]
struct Shared {
    color: u16,
    refs: u32,
}

#[derive(Clone, Debug)]
pub struct Cmap {
    visual: Visual,
    planes: u32,
    off: [u32; 3],
    /// The red table (the only one, without subfields), green and blue.
    ents: [Vec<Entry>; 3],
    /// Free cells left in each table (freeRed and the like).
    free: [i64; 3],
    /// Each client's cells in each table (clientPixelsRed and the like):
    /// table indices, once per allocation.
    pixels: [HashMap<u32, Vec<u32>>; 3],
    shared: Vec<Shared>,
    /// Created AllocAll (AllAllocated): FreeColors is refused.
    all_allocated: bool,
    /// The client whose map it is, CLIENT_ID(mid).
    owner: u32,
}

/// miResolveColor: the colour a visual can actually show.
pub fn resolve(visual: &Visual, r: u16, g: u16, b: u16) -> (u16, u16, u16) {
    let bits = visual.bits_per_rgb as u32;
    let shift = 16 - bits;
    let lim = (1u32 << bits) - 1;
    let scale = |c: u32| (((c >> shift) * 65535) / lim) as u16;
    if matches!(visual.class, Class::StaticGray | Class::GrayScale) {
        let gray = ((30 * r as u32 + 59 * g as u32 + 11 * b as u32) / 100) as u16;
        let v = scale(gray as u32);
        (v, v, v)
    } else {
        (scale(r as u32), scale(g as u32), scale(b as u32))
    }
}

/// X.org's GetNextBitsOrBreak: the next of the 2^n values made of `mask`'s
/// bits, counting up from 0; None after `mask` itself. `base` is mask's
/// lowest bit.
fn next_bits(bits: u64, mask: u64, base: u64) -> Option<u64> {
    if bits == mask {
        return None;
    }
    let mut bits = bits + base;
    while bits & !mask != 0 {
        bits += bits & !mask;
    }
    Some(bits)
}

/// Every value made of `mask`'s bits, 0 first.
fn combos(mask: u64) -> Vec<u64> {
    let base = mask & mask.wrapping_neg();
    let mut v = vec![0];
    let mut bits = 0;
    while let Some(b) = next_bits(bits, mask, base) {
        v.push(b);
        bits = b;
    }
    v
}

impl Cmap {
    /// CreateColormap, with miInitializeColormap for the static classes.
    /// `alloc_all` (AllocAll) makes every cell writable and `client`'s.
    /// AllocAll on a static class is BadMatch except for client 0, the
    /// server, which makes its static default maps that way.
    pub fn new(visual: Visual, alloc_all: bool, client: u32) -> Result<Cmap, CmapError> {
        if !visual.class.dynamic() && alloc_all && client != 0 {
            return Err(CmapError(BAD_MATCH, None));
        }
        Ok(Cmap::create(visual, alloc_all, client))
    }

    /// The screen's default map, as miCreateDefColormap makes it: AllocAll
    /// by the server (client 0) for the static classes, then white
    /// allocated and black after it, by client 0, starting at Xvfb's
    /// default pixels (white 1, black 0). Returns the map, black and white.
    pub fn new_default(visual: Visual) -> (Cmap, u32, u32) {
        let depth = visual.planes();
        let mut m = Cmap::create(visual, depth != 1 && !visual.class.dynamic(), 0);
        if depth <= 1 {
            return (m, 0, 1);
        }
        let white = m.alloc(0, 0xffff, 0xffff, 0xffff, 1).map(|c| c.0).unwrap_or(1);
        let black = m.alloc(0, 0, 0, 0, 0).map(|c| c.0).unwrap_or(0);
        (m, black, white)
    }

    /// miBuildRenderColormap for the default map, as X.org's RENDER does at
    /// startup (miInitIndexed, for the root visual's indexed format): on a
    /// PseudoColor or GrayScale default map, client 0 allocates a 4x4x4
    /// colour cube and 13 grays (or just the grays, or black and white,
    /// when the cells aren't there). Xvfb has RENDER, so its default 8-bit
    /// map starts with pixels 0 to 72 taken; call this right after
    /// `new_default` to hand out the same pixels. Returns the pixels used,
    /// in order.
    pub fn render_colors(&mut self) -> Vec<u32> {
        let v = self.visual;
        if !v.class.dynamic() || v.class.direct() || v.entries > 256 {
            return Vec::new();
        }
        const MONO: i32 = 1;
        const GRAY: i32 = 2;
        const COLOR: i32 = 3;
        let mut policy = if v.entries >= 256 && v.class == Class::PseudoColor {
            COLOR
        } else if v.entries >= 64 {
            GRAY
        } else {
            MONO
        };
        loop {
            let needed = match policy {
                COLOR => 71,
                GRAY => 11,
                _ => 0,
            };
            if needed <= self.free[0] {
                break;
            }
            policy -= 1;
        }
        let (cube, gray) = match policy {
            COLOR => (4u32, 13u32),
            GRAY => (0, 13),
            _ => (0, 2),
        };
        let level = |i: u32, n: u32| ((i * 65535 + (n - 1) / 2) / (n - 1)) as u16;
        let mut used = vec![false; v.entries as usize];
        for r in 0..cube {
            for g in 0..cube {
                for b in 0..cube {
                    match self.alloc(0, level(r, cube), level(g, cube), level(b, cube), 0) {
                        Ok((p, ..)) => used[p as usize] = true,
                        Err(_) => return Vec::new(),
                    }
                }
            }
        }
        for g in 0..gray {
            let l = level(g, gray);
            match self.alloc(0, l, l, l, 0) {
                Ok((p, ..)) => used[p as usize] = true,
                Err(_) => return Vec::new(),
            }
        }
        (0..v.entries as u32).filter(|&p| used[p as usize]).collect()
    }

    fn create(visual: Visual, alloc_all: bool, client: u32) -> Cmap {
        let off = visual.masks().map(|m| if m == 0 { 0 } else { m.trailing_zeros() });
        let mut m = Cmap {
            visual,
            planes: visual.planes(),
            off,
            ents: Default::default(),
            free: [0; 3],
            pixels: Default::default(),
            shared: Vec::new(),
            all_allocated: alloc_all && visual.class.dynamic(),
            owner: client,
        };
        let size = visual.entries as usize;
        let tables: &[Chan] = if visual.class.direct() { &RGB } else { &[Chan::Pseudo] };
        for &ch in tables {
            let n = if ch == Chan::Pseudo { size } else { m.num(ch) as usize };
            m.ents[ch.idx()] = vec![Entry::default(); size.max(n)];
            m.free[ch.idx()] = n as i64;
            if alloc_all {
                for e in &mut m.ents[ch.idx()][..n] {
                    e.refcnt = PRIVATE;
                }
                m.free[ch.idx()] = 0;
                m.pixels[ch.idx()].insert(client, (0..n as u32).collect());
            }
        }
        m.initialize();
        m
    }

    /// miInitializeColormap: the fixed contents of the static classes.
    fn initialize(&mut self) {
        let v = self.visual;
        let bits = v.bits_per_rgb as u32;
        let lim = (1u32 << bits) - 1;
        let shift = 16 - bits;
        let maxent = v.entries as u32 - 1;
        let lims = [0, 1, 2].map(|c| v.masks()[c] >> self.off[c]);
        let scale = |i: u32, l: u32| (((((i * 65535) / l) >> shift) * 65535) / lim) as u16;
        match v.class {
            Class::TrueColor => {
                for i in 0..=maxent {
                    for c in 0..3 {
                        self.ents[c][i as usize].rgb[c] = scale(i, lims[c]);
                    }
                }
            }
            Class::StaticColor => {
                for i in 0..=maxent {
                    let e = &mut self.ents[0][i as usize];
                    for c in 0..3 {
                        e.rgb[c] = scale((i & v.masks()[c]) >> self.off[c], lims[c]);
                    }
                }
            }
            Class::StaticGray => {
                for i in 0..=maxent {
                    let g = scale(i, maxent);
                    self.ents[0][i as usize].rgb = [g, g, g];
                }
            }
            _ => {}
        }
    }

    pub fn visual(&self) -> Visual {
        self.visual
    }

    /// Entries in a subfield's table: NUMRED and the like.
    fn num(&self, ch: Chan) -> u32 {
        let c = ch.idx();
        (self.visual.masks()[c] >> self.off[c]) + 1
    }

    fn rgb_mask(&self) -> u32 {
        self.visual.red_mask | self.visual.green_mask | self.visual.blue_mask
    }

    fn share(&mut self, color: u16) -> usize {
        if let Some(i) = self.shared.iter().position(|s| s.refs == 0) {
            self.shared[i] = Shared { color, refs: 0 };
            i
        } else {
            self.shared.push(Shared { color, refs: 0 });
            self.shared.len() - 1
        }
    }

    /// A cell's colour, through its shared slots if it has them.
    fn color(&self, e: &Entry) -> [u16; 3] {
        match e.shared {
            Some(s) => s.map(|i| self.shared[i].color),
            None => e.rgb,
        }
    }

    /// Drops a cell's shared slots, keeping their colours in the cell. (In
    /// X.org the cell's colour is then whatever its union holds.)
    fn unshare(&mut self, ch: usize, i: usize) {
        if let Some(s) = self.ents[ch][i].shared.take() {
            let rgb = s.map(|k| self.shared[k].color);
            for k in s {
                self.shared[k].refs -= 1;
            }
            self.ents[ch][i].rgb = rgb;
        }
    }

    /// Copies an entry from another map, giving it slots of its own here.
    fn take_entry(&mut self, ch: usize, i: usize, src: &Cmap, map: &mut HashMap<usize, usize>) {
        let mut e = src.ents[ch][i].clone();
        if let Some(s) = e.shared {
            e.shared = Some(s.map(|k| {
                let n = *map.entry(k).or_insert_with(|| self.share(src.shared[k].color));
                self.shared[n].refs += 1;
                n
            }));
        }
        self.ents[ch][i] = e;
    }

    /// FreeCell: one reference to cell `i` of a table goes.
    fn free_cell(&mut self, ch: Chan, i: u32) {
        let c = ch.idx();
        let i = i as usize;
        if self.ents[c][i].refcnt > 1 {
            self.ents[c][i].refcnt -= 1;
        } else {
            self.unshare(c, i);
            self.ents[c][i].refcnt = 0;
            self.free[c] += 1;
        }
    }

    fn remember(&mut self, ch: Chan, client: u32, pixel: u32) {
        self.pixels[ch.idx()].entry(client).or_default().push(pixel);
    }

    /// FindColor: a read-only cell of this colour (for the subfield tables,
    /// of this channel's value), sharing one that has it or filling a free
    /// one, looking from `start` round the table. Returns the cell's index.
    fn find_color(&mut self, ch: Chan, size: u32, rgb: [u16; 3], start: u32, client: u32) -> Option<u32> {
        let c = ch.idx();
        let same = |e: &Entry| match ch {
            Chan::Pseudo => e.rgb == rgb,
            _ => e.rgb[c] == rgb[c],
        };
        let mut pixel = if start >= size { 0 } else { start };
        let mut free = None;
        let mut found = None;
        for _ in 0..size {
            let e = &mut self.ents[c][pixel as usize];
            if e.refcnt > 0 {
                if same(e) {
                    e.refcnt += 1;
                    found = Some(pixel);
                    break;
                }
            } else if free.is_none() && e.refcnt == 0 {
                free = Some(pixel);
            }
            pixel += 1;
            if pixel >= size {
                pixel = 0;
            }
        }
        let pixel = match (found, free) {
            (Some(p), _) => p,
            (None, Some(p)) => {
                let e = &mut self.ents[c][p as usize];
                e.shared = None;
                e.refcnt = 1;
                match ch {
                    Chan::Pseudo => e.rgb = rgb,
                    _ => e.rgb[c] = rgb[c],
                }
                self.free[c] -= 1;
                p
            }
            (None, None) => return None,
        };
        self.remember(ch, client, pixel);
        Some(pixel)
    }

    /// FindBestPixel: the cell nearest the colour, the first of equals.
    fn best_pixel(&self, ch: Chan, size: u32, rgb: [u16; 3]) -> u32 {
        let sq = |a: u16, b: u16| {
            let d = a as i64 - b as i64;
            (d * d) as u64
        };
        let mut best = 0;
        let mut min = u64::MAX;
        for (i, e) in self.ents[ch.idx()][..size as usize].iter().enumerate() {
            let d = match ch {
                Chan::Pseudo => sq(e.rgb[0], rgb[0]) + sq(e.rgb[1], rgb[1]) + sq(e.rgb[2], rgb[2]),
                _ => sq(e.rgb[ch.idx()], rgb[ch.idx()]),
            };
            if d < min {
                best = i as u32;
                min = d;
            }
        }
        best
    }

    /// FindColorInRootCmap, called on the screen's default map: where a
    /// colour already is in it, so that another map of the root visual can
    /// give it the same pixel. Pass the result as `hint` to `alloc_color`
    /// on that other map; the hint is 0 for the default map itself and for
    /// maps of other visuals. Like X.org, the last cell that matches wins.
    pub fn root_hint(&self, r: u16, g: u16, b: u16) -> u32 {
        let (r, g, b) = resolve(&self.visual, r, g, b);
        let rgb = [r, g, b];
        let size = self.visual.entries as usize;
        let last = |c: usize, same: &dyn Fn(&Entry) -> bool| self.ents[c][..size].iter().rposition(|e| e.refcnt > 0 && same(e)).map(|p| p as u32);
        match self.visual.class {
            Class::PseudoColor | Class::GrayScale => last(0, &|e| e.rgb == rgb).unwrap_or(0),
            Class::DirectColor => (0..3).map(|c| last(c, &|e| e.rgb[c] == rgb[c]).map_or(0, |p| p << self.off[c])).fold(0, |a, p| a | p),
            _ => 0,
        }
    }

    /// AllocColor: a read-only cell for the colour, as the visual shows it.
    /// Returns the pixel and the colour it actually has. On a PseudoColor or
    /// GrayScale map a free cell gets the colour, so the screen can change.
    /// `hint` is `root_hint` of the default map, or 0 (see there).
    pub fn alloc_color(&mut self, client: u32, r: u16, g: u16, b: u16, hint: u32) -> Result<(u32, u16, u16, u16), CmapError> {
        self.alloc(client, r, g, b, hint)
    }

    fn alloc(&mut self, client: u32, r: u16, g: u16, b: u16, hint: u32) -> Result<(u32, u16, u16, u16), CmapError> {
        let (r, g, b) = resolve(&self.visual, r, g, b);
        let rgb = [r, g, b];
        let entries = self.visual.entries as u32;
        let alloc = Err(CmapError(BAD_ALLOC, None));
        match self.visual.class {
            Class::StaticColor | Class::StaticGray => {
                let p = self.best_pixel(Chan::Pseudo, entries, rgb);
                self.remember(Chan::Pseudo, client, p);
                let [r, g, b] = self.ents[0][p as usize].rgb;
                Ok((p, r, g, b))
            }
            Class::TrueColor => {
                let mut pixel = 0;
                let mut out = [0; 3];
                for ch in RGB {
                    let p = self.best_pixel(ch, self.num(ch), rgb);
                    pixel |= p << self.off[ch.idx()];
                    out[ch.idx()] = self.ents[ch.idx()][p as usize].rgb[ch.idx()];
                    self.remember(ch, client, p);
                }
                Ok((pixel, out[0], out[1], out[2]))
            }
            Class::PseudoColor | Class::GrayScale => match self.find_color(Chan::Pseudo, entries, rgb, hint, client) {
                Some(p) => Ok((p, r, g, b)),
                None => alloc,
            },
            Class::DirectColor => {
                let mut pixel = 0;
                let mut got = Vec::new();
                for ch in RGB {
                    let c = ch.idx();
                    let start = (hint & self.visual.masks()[c]) >> self.off[c];
                    match self.find_color(ch, self.num(ch), rgb, start, client) {
                        Some(p) => {
                            pixel |= p << self.off[c];
                            got.push((ch, p << self.off[c]));
                        }
                        None => {
                            // Give back the channels already found, last first.
                            for &(ch, p) in got.iter().rev() {
                                let mut ev = None;
                                self.free_co(ch, client, &[p], 0, &mut ev);
                            }
                            return alloc;
                        }
                    }
                }
                Ok((pixel, r, g, b))
            }
        }
    }

    /// QueryColors. A bad pixel is BadValue with the last such pixel as its
    /// value, as X.org does.
    pub fn query_colors(&self, pixels: &[u32]) -> Result<Vec<(u16, u16, u16)>, CmapError> {
        let mut out = Vec::with_capacity(pixels.len());
        let mut bad = None;
        let rgbbad = !self.rgb_mask();
        for &p in pixels {
            if self.visual.class.direct() {
                if p & rgbbad != 0 {
                    bad = Some(p);
                    out.push((0, 0, 0));
                    continue;
                }
                let mut c3 = [0; 3];
                for ch in RGB {
                    let c = ch.idx();
                    let i = (p & self.visual.masks()[c]) >> self.off[c];
                    c3[c] = self.ents[c][i as usize].rgb[c];
                }
                out.push((c3[0], c3[1], c3[2]));
            } else if p >= self.visual.entries as u32 {
                bad = Some(p);
                out.push((0, 0, 0));
            } else {
                let [r, g, b] = self.color(&self.ents[0][p as usize]);
                out.push((r, g, b));
            }
        }
        match bad {
            Some(p) => Err(CmapError(BAD_VALUE, Some(p))),
            None => Ok(out),
        }
    }

    /// FreePixels: all of a client's cells go.
    fn free_pixels(&mut self, client: u32) {
        let tables: &[Chan] = if self.visual.class.direct() { &RGB } else { &[Chan::Pseudo] };
        for &ch in tables {
            let list = self.pixels[ch.idx()].remove(&client).unwrap_or_default();
            if self.visual.class.dynamic() {
                for p in list {
                    self.free_cell(ch, p);
                }
            }
        }
    }

    /// A client went (FreeClientPixels). X.org does this only for clients
    /// with cells in someone else's map; a client's own maps go with it,
    /// which comes to the same.
    pub fn free_client(&mut self, client: u32) {
        self.free_pixels(client);
    }

    /// AllocCP: `count` sets of 2^planes free cells, each set a base pixel
    /// with every combination of the plane mask's bits, marked writable.
    /// Returns every cell, the `count` bases first, and the mask.
    fn alloc_cp(&mut self, c: usize, count: usize, planes: u32, contig: bool) -> Option<(Vec<u32>, u32)> {
        let n = self.visual.entries as u64;
        let dplanes = self.planes;
        let ent = &mut self.ents[c];
        let mut out: Vec<u64> = Vec::with_capacity(count << planes);
        if planes == 0 {
            let mut pixel = 0;
            for _ in 0..count {
                while ent[pixel].refcnt != 0 {
                    pixel += 1;
                }
                ent[pixel].refcnt = PRIVATE;
                ent[pixel].shared = None;
                out.push(pixel as u64);
            }
            return Some((out.into_iter().map(|p| p as u32).collect(), 0));
        }
        if planes > dplanes {
            return None;
        }
        let take = |out: &mut Vec<u64>, ent: &mut Vec<Entry>, mask: u64| {
            for k in (0..count).rev() {
                let p = out[k];
                for bits in combos(mask) {
                    let e = &mut ent[(p + bits) as usize];
                    e.refcnt = PRIVATE;
                    e.shared = None;
                    if bits != 0 {
                        out.push(p + bits);
                    }
                }
            }
        };

        // Contiguous planes first, the mask moving up a bit at a time. A
        // mask as big as the table finds nothing (in X.org the unsigned
        // `entries` then wraps and it reads past the table).
        let mut mask: u64 = (1 << planes) - 1;
        let mut base: u64 = 1;
        for _ in 0..dplanes - planes + 1 {
            if mask >= n {
                break;
            }
            let entries = n - mask;
            out.clear();
            let mut pixel = 0;
            while pixel < entries {
                let save = pixel;
                let maxp = pixel + mask + base;
                while pixel != maxp && ent[pixel as usize].refcnt == 0 {
                    pixel += base;
                }
                if pixel == maxp {
                    out.push(save);
                    if out.len() == count {
                        take(&mut out, ent, mask);
                        return Some((out.into_iter().map(|p| p as u32).collect(), mask as u32));
                    }
                }
                pixel = save + 1;
                if pixel & mask != 0 {
                    pixel += mask;
                }
            }
            mask += mask;
            base += base;
        }

        if contig || planes == 1 || dplanes < 3 || dplanes < planes + 1 {
            return None;
        }
        // Then every mask of `planes` bits that aren't all together, smallest
        // first (X.org's last guard above stops a shift by -1 it would do).
        let finalmask: u64 = (((1u64 << (planes - 1)) - 1) << (dplanes - planes + 1)) + (1u64 << (dplanes - planes - 1));
        let mut mask: u64 = (3 << (planes - 1)) - 1;
        while mask <= finalmask {
            if mask >= n {
                break;
            }
            if mask.count_ones() != planes {
                mask += 1;
                continue;
            }
            let entries = n - mask;
            let base = mask & mask.wrapping_neg();
            out.clear();
            for pixel in 0..entries {
                if pixel & mask != 0 {
                    continue;
                }
                let mut maxp = 0;
                while ent[(pixel + maxp) as usize].refcnt == 0 {
                    match next_bits(maxp, mask, base) {
                        Some(b) => maxp = b,
                        None => break,
                    }
                }
                if maxp < mask || ent[(pixel + mask) as usize].refcnt != 0 {
                    continue;
                }
                out.push(pixel);
                if out.len() < count {
                    continue;
                }
                take(&mut out, ent, mask);
                return Some((out.into_iter().map(|p| p as u32).collect(), mask as u32));
            }
            mask += 1;
        }
        None
    }

    /// AllocPseudo: `c` sets of 2^r writable cells on a map without
    /// subfields. Returns the bases, the mask and every cell.
    fn alloc_pseudo(&mut self, client: u32, c: u16, r: u32, contig: bool) -> Result<(Vec<u32>, u32, Vec<u32>), CmapError> {
        let npix = (c as u64) << r.min(63);
        if r >= 32 || npix as i64 > self.free[0] || npix < c as u64 {
            return Err(CmapError(BAD_ALLOC, None));
        }
        let (cells, mask) = self.alloc_cp(0, c as usize, r, contig).ok_or(CmapError(BAD_ALLOC, None))?;
        self.pixels[0].entry(client).or_default().extend(&cells);
        self.free[0] -= npix as i64;
        Ok((cells[..c as usize].to_vec(), mask, cells))
    }

    /// AllocDirect: `c` writable pixels with r, g and b planes of their
    /// channels on a DirectColor map. Returns the pixels and the masks.
    fn alloc_direct(&mut self, client: u32, c: u16, r: u32, g: u32, b: u32, contig: bool) -> Result<(Vec<u32>, [u32; 3]), CmapError> {
        let planes = [r, g, b];
        let alloc = CmapError(BAD_ALLOC, None);
        for k in 0..3 {
            let n = (c as u64) << planes[k].min(63);
            if planes[k] >= 32 || n as i64 > self.free[k] || n < c as u64 {
                return Err(alloc);
            }
        }
        let got: Vec<_> = (0..3).map(|k| self.alloc_cp(k, c as usize, planes[k], contig)).collect();
        if got.iter().any(|g| g.is_none()) {
            for (k, g) in got.iter().enumerate() {
                if let Some((cells, _)) = g {
                    for &p in cells {
                        self.ents[k][p as usize].refcnt = 0;
                    }
                }
            }
            return Err(alloc);
        }
        let mut pixels = vec![0u32; c as usize];
        let mut masks = [0; 3];
        for (k, g) in got.into_iter().enumerate() {
            let (cells, mask) = g.unwrap();
            masks[k] = mask << self.off[k];
            for (i, p) in pixels.iter_mut().enumerate() {
                *p |= cells[i] << self.off[k];
            }
            self.free[k] -= cells.len() as i64;
            self.pixels[k].entry(client).or_default().extend(cells);
        }
        Ok((pixels, masks))
    }

    /// AllocColorCells: `colors` writable pixels and `planes` plane masks,
    /// every combination of the masks ORed into each pixel being the
    /// client's too.
    pub fn alloc_cells(&mut self, client: u32, colors: u16, planes: u16, contiguous: bool) -> Result<(Vec<u32>, Vec<u32>), CmapError> {
        if colors == 0 {
            return Err(CmapError(BAD_VALUE, Some(0)));
        }
        if !self.visual.class.dynamic() {
            return Err(CmapError(BAD_ALLOC, None));
        }
        let planes = planes as u32;
        let mut masks = Vec::with_capacity(planes as usize);
        if self.visual.class == Class::DirectColor {
            let (pixels, [rm, gm, bm]) = self.alloc_direct(client, colors, planes, planes, planes, contiguous)?;
            let (rm, gm, bm) = (rm as u64, gm as u64, bm as u64);
            let (mut r, mut g, mut b) = (1u64, 1u64, 1u64);
            for _ in 0..planes {
                while rm & r == 0 {
                    r += r;
                }
                while gm & g == 0 {
                    g += g;
                }
                while bm & b == 0 {
                    b += b;
                }
                masks.push((r | g | b) as u32);
                r += r;
                g += g;
                b += b;
            }
            Ok((pixels, masks))
        } else {
            let (pixels, mask, _) = self.alloc_pseudo(client, colors, planes, contiguous)?;
            let mask = mask as u64;
            let mut r = 1u64;
            for _ in 0..planes {
                while mask & r == 0 {
                    r += r;
                }
                masks.push(r as u32);
                r += r;
            }
            Ok((pixels, masks))
        }
    }

    /// AllocColorPlanes: `colors` pixels with reds, greens and blues planes;
    /// returns the pixels and the red, green and blue masks. Without
    /// subfields the cells share their channels' values through the masks,
    /// so storing one cell's red sets it in every cell with the same red
    /// bits.
    pub fn alloc_planes(&mut self, client: u32, colors: u16, reds: u16, greens: u16, blues: u16, contiguous: bool) -> Result<(Vec<u32>, u32, u32, u32), CmapError> {
        if colors == 0 {
            return Err(CmapError(BAD_VALUE, Some(0)));
        }
        if !self.visual.class.dynamic() {
            return Err(CmapError(BAD_ALLOC, None));
        }
        let (r, g, b) = (reds as u32, greens as u32, blues as u32);
        if self.visual.class == Class::DirectColor {
            let (pixels, [rm, gm, bm]) = self.alloc_direct(client, colors, r, g, b, contiguous)?;
            return Ok((pixels, rm, gm, bm));
        }
        // As in X.org, `contiguous` makes all r + g + b planes one run.
        let (pixels, mask, cells) = self.alloc_pseudo(client, colors, r + g + b, contiguous)?;
        let mask = mask as u64;
        let mut ms = [0u64; 3];
        let mut shift = 1u64;
        for (k, n) in [r, g, b].into_iter().enumerate() {
            for _ in 0..n {
                while mask & shift == 0 {
                    shift += shift;
                }
                ms[k] |= shift;
                shift += shift;
            }
        }
        self.alloc_shared(&pixels, &cells, ms);
        Ok((pixels, ms[0] as u32, ms[1] as u32, ms[2] as u32))
    }

    /// AllocShared: each channel of each new cell gets a slot shared with
    /// the cells that differ from it only in the other channels' bits.
    /// X.org's new slots are uninitialised memory; these start at 0.
    fn alloc_shared(&mut self, bases: &[u32], cells: &[u32], ms: [u64; 3]) {
        let mut slots: HashMap<u32, [usize; 3]> = cells.iter().map(|&p| (p, [0; 3])).collect();
        for &p in bases {
            for k in 0..3 {
                let basemask = !(ms[(k + 1) % 3] | ms[(k + 2) % 3]);
                let common = p as u64 & basemask;
                for bits in combos(ms[k]) {
                    let s = self.share(0);
                    for &cell in cells {
                        if cell as u64 & basemask == common | bits {
                            slots.get_mut(&cell).unwrap()[k] = s;
                            self.shared[s].refs += 1;
                        }
                    }
                }
            }
        }
        for (cell, s) in slots {
            self.ents[0][cell as usize].shared = Some(s);
        }
    }

    /// FreeColors: each pixel ORed with every combination of `plane_mask`'s
    /// bits loses one of the client's references.
    pub fn free_colors(&mut self, client: u32, plane_mask: u32, pixels: &[u32]) -> Result<(), CmapError> {
        if self.all_allocated {
            return Err(CmapError(BAD_ACCESS, None));
        }
        let mut ev = None;
        let mask = plane_mask as u64;
        let (mut result, rmask);
        if self.visual.class.direct() {
            rmask = mask & self.rgb_mask() as u64;
            result = 0;
            for ch in RGB {
                let m = plane_mask & self.visual.masks()[ch.idx()];
                let r = self.free_co(ch, client, pixels, m, &mut ev);
                if r != 0 {
                    result = r;
                }
            }
        } else {
            rmask = mask & ((1u64 << self.planes) - 1);
            result = self.free_co(Chan::Pseudo, client, pixels, rmask as u32, &mut ev);
        }
        if mask != rmask && !pixels.is_empty() {
            ev = Some(pixels[0] | plane_mask);
            result = BAD_VALUE;
        }
        if result == 0 { Ok(()) } else { Err(CmapError(result, ev)) }
    }

    /// FreeCo: FreeColors for one table. Returns the error code (0 for
    /// none), setting `ev` where X.org sets the error value.
    fn free_co(&mut self, ch: Chan, client: u32, pixels: &[u32], mask: u32, ev: &mut Option<u32>) -> u8 {
        if pixels.is_empty() {
            return 0;
        }
        let c = ch.idx();
        let (cmask, rgbbad, offset, numents) = match ch {
            Chan::Pseudo => (u32::MAX, 0, 0, self.visual.entries as u32),
            _ => (self.visual.masks()[c], !self.rgb_mask(), self.off[c], self.num(ch)),
        };
        let mask = mask as u64;
        let base = mask & mask.wrapping_neg();
        let mut list = self.pixels[c].remove(&client).unwrap_or_default();
        let mut err = 0;
        let mut bits = 0u64;
        loop {
            for &p in pixels {
                let test = ((p | bits as u32) & cmask) >> offset;
                if test >= numents || p & rgbbad != 0 {
                    *ev = Some(p | bits as u32);
                    err = BAD_VALUE;
                    continue;
                }
                if let Some(k) = list.iter().position(|&x| x == test) {
                    if self.visual.class.dynamic() {
                        self.free_cell(ch, test);
                    }
                    list[k] = ZAPPED;
                } else {
                    err = BAD_ACCESS;
                }
            }
            match next_bits(bits, mask, base) {
                Some(b) => bits = b,
                None => break,
            }
        }
        list.retain(|&x| x != ZAPPED);
        if !list.is_empty() {
            self.pixels[c].insert(client, list);
        }
        err
    }

    /// StoreColors: items are (pixel, red, green, blue, flags of DO_RED,
    /// DO_GREEN, DO_BLUE). As in X.org the good items are stored even when
    /// others are bad, so the first value says whether anything was stored
    /// (and the screen may need redrawing) whatever the error.
    pub fn store_colors(&mut self, _client: u32, items: &[(u32, u16, u16, u16, u8)]) -> (bool, Result<(), CmapError>) {
        if !self.visual.class.dynamic() {
            return (false, Err(CmapError(BAD_ACCESS, None)));
        }
        let mut err = 0;
        let mut ev = None;
        let mut changed = false;
        if self.visual.class.direct() {
            let rgbbad = !self.rgb_mask();
            for &(pixel, r, g, b, flags) in items {
                let (r, g, b) = resolve(&self.visual, r, g, b);
                let rgb = [r, g, b];
                if pixel & rgbbad != 0 {
                    err = BAD_VALUE;
                    ev = Some(pixel);
                    continue;
                }
                // Each channel on its own: one can be stored while another
                // is refused.
                let mut ok = true;
                for ch in RGB {
                    let c = ch.idx();
                    let i = (pixel & self.visual.masks()[c]) >> self.off[c];
                    if i >= self.num(ch) {
                        err = BAD_VALUE;
                        ok = false;
                    } else if self.ents[c][i as usize].refcnt != PRIVATE {
                        err = BAD_ACCESS;
                        ok = false;
                    } else if flags & (1 << c) != 0 {
                        self.ents[c][i as usize].rgb[c] = rgb[c];
                        changed = true;
                    }
                }
                if !ok {
                    ev = Some(pixel);
                }
            }
        } else {
            for &(pixel, r, g, b, flags) in items {
                if pixel >= self.visual.entries as u32 {
                    ev = Some(pixel);
                    err = BAD_VALUE;
                    continue;
                }
                if self.ents[0][pixel as usize].refcnt != PRIVATE {
                    err = BAD_ACCESS;
                    continue;
                }
                let (r, g, b) = resolve(&self.visual, r, g, b);
                let rgb = [r, g, b];
                let i = pixel as usize;
                for c in 0..3 {
                    if flags & (1 << c) != 0 {
                        match self.ents[0][i].shared {
                            Some(s) => self.shared[s[c]].color = rgb[c],
                            None => self.ents[0][i].rgb[c] = rgb[c],
                        }
                    }
                }
                changed = true;
            }
        }
        (changed, if err == 0 { Ok(()) } else { Err(CmapError(err, ev)) })
    }

    /// CopyColormapAndFree: a new map, `client`'s, holding the cells (and
    /// colours) the client had in this one, which are freed here. If the
    /// client made this map AllocAll the new map is AllocAll with its
    /// colours, and this one is left with every cell free.
    pub fn copy_and_free(&mut self, client: u32) -> Cmap {
        let alloc_all = self.all_allocated && self.owner == client;
        let mut new = Cmap::create(self.visual, alloc_all, client);
        let tables: &[Chan] = if self.visual.class.direct() { &RGB } else { &[Chan::Pseudo] };
        if alloc_all {
            let mut map = HashMap::new();
            for &ch in tables {
                for i in 0..self.ents[ch.idx()].len() {
                    new.take_entry(ch.idx(), i, self, &mut map);
                }
            }
            self.all_allocated = false;
            self.free_pixels(client);
            return new;
        }
        for &ch in tables {
            self.copy_free(ch, client, &mut new);
        }
        new
    }

    /// CopyFree: moves a client's cells of one table into `dst`.
    fn copy_free(&mut self, ch: Chan, client: u32, dst: &mut Cmap) {
        let c = ch.idx();
        let list = self.pixels[c].remove(&client).unwrap_or_default();
        let mut nalloc = 0;
        if self.visual.class.dynamic() {
            let mut map = HashMap::new();
            for &p in &list {
                let i = p as usize;
                if dst.ents[c][i].refcnt > 0 {
                    dst.ents[c][i].refcnt += 1;
                } else {
                    dst.take_entry(c, i, self, &mut map);
                    nalloc += 1;
                    if self.ents[c][i].refcnt > 0 {
                        dst.ents[c][i].refcnt = 1;
                    } else {
                        // The new map has the shared slots now.
                        self.unshare(c, i);
                    }
                }
                self.free_cell(ch, p);
            }
        }
        dst.free[c] -= nalloc;
        if !list.is_empty() {
            dst.pixels[c].insert(client, list);
        }
    }

    /// How a pixel looks through this map, 0x00RRGGBB: the top 8 bits of
    /// each channel, through the subfields for TrueColor and DirectColor.
    pub fn rgb(&self, pixel: u32) -> u32 {
        let c3 = if self.visual.class.direct() {
            let mut c3 = [0; 3];
            for c in 0..3 {
                let i = ((pixel & self.visual.masks()[c]) >> self.off[c]) as usize;
                c3[c] = self.ents[c].get(i).map_or(0, |e| e.rgb[c]);
            }
            c3
        } else {
            match self.ents[0].get(pixel as usize) {
                Some(e) => self.color(e),
                None => [0; 3],
            }
        };
        ((c3[0] as u32 >> 8) << 16) | ((c3[1] as u32 >> 8) << 8) | (c3[2] as u32 >> 8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vis(class: Class) -> Visual {
        let (entries, r, g, b) = match class {
            Class::TrueColor | Class::DirectColor => (8, 0x07, 0x38, 0xc0),
            Class::StaticColor => (256, 0x07, 0x38, 0xc0),
            _ => (256, 0, 0, 0),
        };
        Visual { class, bits_per_rgb: 8, entries, red_mask: r, green_mask: g, blue_mask: b }
    }

    fn deep(class: Class) -> Visual {
        Visual { class, bits_per_rgb: 8, entries: 256, red_mask: 0xff0000, green_mask: 0xff00, blue_mask: 0xff }
    }

    const C: u32 = 1;

    #[test]
    fn default_maps() {
        let (m, black, white) = Cmap::new_default(vis(Class::PseudoColor));
        assert_eq!((black, white), (0, 1));
        assert_eq!(m.query_colors(&[0, 1]).unwrap(), vec![(0, 0, 0), (0xffff, 0xffff, 0xffff)]);
        let (_, black, white) = Cmap::new_default(vis(Class::TrueColor));
        assert_eq!((black, white), (0, 0xff));
        let (_, black, white) = Cmap::new_default(deep(Class::TrueColor));
        assert_eq!((black, white), (0, 0xffffff));
        let (_, black, white) = Cmap::new_default(vis(Class::StaticGray));
        assert_eq!((black, white), (0, 255));
        // DirectColor: white takes red 1 (from the hint), green 0, blue 0;
        // black then finds green and blue 0 taken.
        let (_, black, white) = Cmap::new_default(vis(Class::DirectColor));
        assert_eq!((black, white), (0x48, 1));
    }

    #[test]
    fn render_colors() {
        // As Xvfb's: black, white, the cube and the grays fill 0 to 72.
        let (mut m, ..) = Cmap::new_default(vis(Class::PseudoColor));
        assert_eq!(m.render_colors(), (0..73).collect::<Vec<_>>());
        assert_eq!(m.query_colors(&[2, 49, 64]).unwrap(), vec![(0, 0, 0x5555), (0xffff, 0, 0), (0x1515, 0x1515, 0x1515)]);
        assert_eq!(m.alloc_color(C, 0x1234, 0x5678, 0x9abc, 0).unwrap().0, 73);
        let (mut g, ..) = Cmap::new_default(vis(Class::GrayScale));
        assert_eq!(g.render_colors().len(), 13);
        let (mut t, ..) = Cmap::new_default(vis(Class::TrueColor));
        assert!(t.render_colors().is_empty());
    }

    #[test]
    fn pseudo_alloc_shares() {
        let (mut m, ..) = Cmap::new_default(vis(Class::PseudoColor));
        let a = m.alloc_color(C, 0x1234, 0x5678, 0x9abc, 0).unwrap();
        assert_eq!(a, (2, 0x1212, 0x5656, 0x9a9a));
        assert_eq!(m.alloc_color(C, 0x12ff, 0x5600, 0x9a00, 0).unwrap().0, 2);
        assert_eq!(m.alloc_color(C, 0xffff, 0xffff, 0xffff, 0).unwrap().0, 1);
        assert_eq!(m.ents[0][2].refcnt, 2);
        m.free_colors(C, 0, &[2]).unwrap();
        assert_eq!(m.ents[0][2].refcnt, 1);
        m.free_colors(C, 0, &[2]).unwrap();
        assert_eq!(m.ents[0][2].refcnt, 0);
        assert_eq!(m.free_colors(C, 0, &[2]), Err(CmapError(BAD_ACCESS, None)));
        // The server's black isn't this client's.
        assert_eq!(m.free_colors(C, 0, &[0]), Err(CmapError(BAD_ACCESS, None)));
        assert_eq!(m.free_colors(C, 0, &[300]), Err(CmapError(BAD_VALUE, Some(300))));
        assert_eq!(m.free_colors(C, 0x100, &[1]), Err(CmapError(BAD_VALUE, Some(0x101))));
        // White is still the client's after the BadValue above: that freed it.
        assert_eq!(m.free_colors(C, 0, &[1]), Err(CmapError(BAD_ACCESS, None)));
    }

    #[test]
    fn root_hint() {
        let (def, ..) = Cmap::new_default(vis(Class::PseudoColor));
        let mut m = Cmap::new(vis(Class::PseudoColor), false, C).unwrap();
        let h = def.root_hint(0xffff, 0xffff, 0xffff);
        assert_eq!(h, 1);
        assert_eq!(m.alloc_color(C, 0xffff, 0xffff, 0xffff, h).unwrap().0, 1);
        assert_eq!(m.alloc_color(C, 0, 0, 0, def.root_hint(0, 0, 0)).unwrap().0, 0);
        assert_eq!(m.alloc_color(C, 0x5000, 0, 0, def.root_hint(0x5000, 0, 0)).unwrap().0, 2);
    }

    #[test]
    fn cells() {
        let (mut m, ..) = Cmap::new_default(vis(Class::PseudoColor));
        assert_eq!(m.alloc_cells(C, 3, 0, false).unwrap(), (vec![2, 3, 4], vec![]));
        // Two planes, contiguous: the first base with four free cells
        // 0b11 apart is 8 (5, 6, 7 are free but 4 isn't).
        let (p, masks) = m.alloc_cells(C, 1, 2, true).unwrap();
        assert_eq!((p, masks), (vec![8], vec![1, 2]));
        let (p, masks) = m.alloc_cells(C, 2, 1, false).unwrap();
        assert_eq!((p, masks), (vec![6, 12], vec![1]));
        assert_eq!(m.alloc_cells(C, 0, 0, false), Err(CmapError(BAD_VALUE, Some(0))));
        assert_eq!(m.alloc_cells(C, 300, 0, false), Err(CmapError(BAD_ALLOC, None)));
        // Freeing a cell with its planes.
        m.free_colors(C, 3, &[8]).unwrap();
        assert!((8..12).all(|i| m.ents[0][i].refcnt == 0));
        let (mut t, ..) = Cmap::new_default(vis(Class::TrueColor));
        assert_eq!(t.alloc_cells(C, 1, 0, false), Err(CmapError(BAD_ALLOC, None)));
    }

    #[test]
    fn noncontiguous_planes() {
        let mut m = Cmap::new(vis(Class::PseudoColor), false, C).unwrap();
        // Everything taken but 0, 1, 4 and 5: no contiguous pair of planes
        // fits, but planes 0 and 2 do.
        m.alloc_cells(C, 256, 0, false).unwrap();
        m.free_colors(C, 0, &[0, 1, 4, 5]).unwrap();
        let (p, masks) = m.alloc_cells(C, 1, 2, false).unwrap();
        assert_eq!((p, masks), (vec![0], vec![1, 4]));
        assert_eq!(m.alloc_cells(C, 1, 2, true), Err(CmapError(BAD_ALLOC, None)));
    }

    #[test]
    fn planes_pseudo_shared() {
        let mut m = Cmap::new(vis(Class::PseudoColor), false, C).unwrap();
        let (p, r, g, b) = m.alloc_planes(C, 1, 1, 1, 0, false).unwrap();
        assert_eq!((p.clone(), r, g, b), (vec![0], 1, 2, 0));
        // Setting red at 0 sets it at 2 (same red bits), not at 1 or 3.
        let (changed, res) = m.store_colors(C, &[(0, 0xffff, 0, 0, DO_RED)]);
        assert!(changed && res.is_ok());
        let q = m.query_colors(&[0, 1, 2, 3]).unwrap();
        assert_eq!(q.iter().map(|c| c.0).collect::<Vec<_>>(), vec![0xffff, 0, 0xffff, 0]);
        m.store_colors(C, &[(1, 0, 0x8000, 0x4000, DO_GREEN | DO_BLUE)]).1.unwrap();
        let q = m.query_colors(&[0, 1, 2, 3]).unwrap();
        assert_eq!(q.iter().map(|c| c.1).collect::<Vec<_>>(), vec![0x8080, 0x8080, 0, 0]);
        // No blue planes: one blue for all four.
        assert_eq!(q.iter().map(|c| c.2).collect::<Vec<_>>(), vec![0x4040; 4]);
        m.free_colors(C, 3, &[0]).unwrap();
        assert!(m.shared.iter().all(|s| s.refs == 0));
    }

    #[test]
    fn planes_direct() {
        let mut m = Cmap::new(deep(Class::DirectColor), false, C).unwrap();
        let (p, r, g, b) = m.alloc_planes(C, 2, 1, 2, 0, true).unwrap();
        assert_eq!(p, vec![0x000000, 0x020401]);
        assert_eq!((r, g, b), (0x010000, 0x0300, 0));
        let (p, masks) = m.alloc_cells(C, 1, 1, false).unwrap();
        assert_eq!((p, masks), (vec![0x040802], vec![0x010101]));
        m.store_colors(C, &[(0x040802, 0x1111, 0x2222, 0x3333, 7)]).1.unwrap();
        assert_eq!(m.query_colors(&[0x040802]).unwrap(), vec![(0x1111, 0x2222, 0x3333)]);
        assert_eq!(m.rgb(0x040802), 0x112233);
        assert_eq!(m.query_colors(&[0x1000000]), Err(CmapError(BAD_VALUE, Some(0x1000000))));
    }

    #[test]
    fn store_errors() {
        let (mut m, ..) = Cmap::new_default(vis(Class::PseudoColor));
        let (p, _) = m.alloc_cells(C, 1, 0, false).unwrap();
        assert_eq!(p, vec![2]);
        // Read-only (black), free, out of range.
        assert_eq!(m.store_colors(C, &[(0, 1, 1, 1, 7)]), (false, Err(CmapError(BAD_ACCESS, None))));
        assert_eq!(m.store_colors(C, &[(9, 1, 1, 1, 7)]), (false, Err(CmapError(BAD_ACCESS, None))));
        assert_eq!(m.store_colors(C, &[(256, 1, 1, 1, 7)]), (false, Err(CmapError(BAD_VALUE, Some(256)))));
        // The good item is stored anyway; the error is the last one.
        let (changed, res) = m.store_colors(C, &[(256, 0, 0, 0, 7), (2, 0xffff, 0, 0, 7), (0, 0, 0, 0, 7)]);
        assert!(changed);
        assert_eq!(res, Err(CmapError(BAD_ACCESS, Some(256))));
        assert_eq!(m.query_colors(&[2]).unwrap(), vec![(0xffff, 0, 0)]);
        let (mut s, ..) = Cmap::new_default(vis(Class::StaticColor));
        assert_eq!(s.store_colors(C, &[(0, 0, 0, 0, 7)]).1, Err(CmapError(BAD_ACCESS, None)));
    }

    #[test]
    fn static_contents() {
        let m = Cmap::new(vis(Class::StaticColor), false, C).unwrap();
        // Pixel 0xff: red 7, green 7, blue 3: all full.
        assert_eq!(m.query_colors(&[0xff, 0x01, 0x40]).unwrap(), vec![(0xffff, 0xffff, 0xffff), (0x2424, 0, 0), (0, 0, 0x5555)]);
        let mut m = m;
        // Nearest: red 0x2424 is 1/7, blue 1/3.
        assert_eq!(m.alloc_color(C, 0x2000, 0, 0x5000, 0).unwrap(), (0x41, 0x2424, 0, 0x5555));
        let mut t = Cmap::new(vis(Class::TrueColor), false, C).unwrap();
        assert_eq!(t.alloc_color(C, 0xffff, 0x8000, 0, 0).unwrap(), (0x07 | 0x20, 0xffff, 0x9292, 0));
        assert_eq!(t.query_colors(&[0xc0]).unwrap(), vec![(0, 0, 0xffff)]);
        assert_eq!(t.rgb(0xff), 0xffffff);
        let mut g = Cmap::new(vis(Class::StaticGray), false, C).unwrap();
        assert_eq!(g.query_colors(&[128]).unwrap(), vec![(0x8080, 0x8080, 0x8080)]);
        // Gray from 30% red, 59% green, 11% blue.
        assert_eq!(g.alloc_color(C, 0xffff, 0, 0, 0).unwrap(), (76, 0x4c4c, 0x4c4c, 0x4c4c));
        assert_eq!(resolve(&vis(Class::GrayScale), 0, 0xffff, 0), (0x9797, 0x9797, 0x9797));
        assert_eq!(Cmap::new(vis(Class::StaticGray), true, C).err(), Some(CmapError(BAD_MATCH, None)));
        // FreeColors on a static map only forgets the client's pixels.
        g.free_colors(C, 0, &[76]).unwrap();
        assert_eq!(g.free_colors(C, 0, &[76]), Err(CmapError(BAD_ACCESS, None)));
    }

    #[test]
    fn copy_and_free() {
        let (mut m, ..) = Cmap::new_default(vis(Class::PseudoColor));
        m.alloc_color(C, 0x1000, 0x2000, 0x3000, 0).unwrap();
        let (cells, _) = m.alloc_cells(C, 2, 0, false).unwrap();
        assert_eq!(cells, vec![3, 4]);
        m.store_colors(C, &[(3, 0xaaaa, 0xbbbb, 0xcccc, 7)]).1.unwrap();
        m.alloc_color(2, 0xffff, 0xffff, 0xffff, 0).unwrap();
        let n = m.copy_and_free(C);
        // The client's cells moved, with their colours; the server's black
        // and white didn't.
        assert_eq!(n.query_colors(&[2, 3]).unwrap(), vec![(0x1010, 0x2020, 0x3030), (0xaaaa, 0xbbbb, 0xcccc)]);
        assert_eq!(n.ents[0][0].refcnt, 0);
        assert_eq!(n.ents[0][2].refcnt, 1);
        assert_eq!(n.ents[0][3].refcnt, PRIVATE);
        assert_eq!(n.free[0], 253);
        assert_eq!(m.ents[0][2].refcnt, 0);
        assert_eq!(m.ents[0][3].refcnt, 0);
        // White has the server's and client 2's references.
        assert_eq!(m.ents[0][1].refcnt, 2);
        assert_eq!(m.free[0], 254);

        // AllocAll: the copy is AllocAll; the old map is emptied.
        let mut a = Cmap::new(vis(Class::PseudoColor), true, C).unwrap();
        assert_eq!(a.free_colors(C, 0, &[0]), Err(CmapError(BAD_ACCESS, None)));
        a.store_colors(C, &[(5, 0x5555, 0x5555, 0x5555, 7)]).1.unwrap();
        let mut b = a.copy_and_free(C);
        assert_eq!(b.query_colors(&[5]).unwrap(), vec![(0x5555, 0x5555, 0x5555)]);
        assert_eq!(b.free_colors(C, 0, &[5]), Err(CmapError(BAD_ACCESS, None)));
        assert_eq!(a.free[0], 256);
        assert_eq!(a.alloc_color(C, 0, 0, 0, 0).unwrap().0, 0);

        // Shared cells keep sharing in the copy.
        let mut s = Cmap::new(vis(Class::PseudoColor), false, 3).unwrap();
        s.alloc_planes(C, 1, 1, 0, 0, false).unwrap();
        s.store_colors(C, &[(0, 0, 0x7777, 0, DO_GREEN)]).1.unwrap();
        let mut t = s.copy_and_free(C);
        assert!(s.shared.iter().all(|x| x.refs == 0));
        t.store_colors(C, &[(1, 0, 0x1111, 0, DO_GREEN)]).1.unwrap();
        assert_eq!(t.query_colors(&[0, 1]).unwrap(), vec![(0, 0x1111, 0), (0, 0x1111, 0)]);
    }

    #[test]
    fn free_client() {
        let (mut m, ..) = Cmap::new_default(vis(Class::PseudoColor));
        m.alloc_cells(C, 4, 1, false).unwrap();
        m.alloc_color(C, 0, 0, 0, 0).unwrap();
        m.free_client(C);
        assert_eq!(m.free[0], 254);
        assert_eq!(m.ents[0][0].refcnt, 1);
        let mut d = Cmap::new(vis(Class::DirectColor), false, C).unwrap();
        assert_eq!(d.alloc_color(C, 0xffff, 0, 0x5555, 0).unwrap(), (0, 0xffff, 0, 0x5555));
        // Green 0 is already 0, so black shares it.
        assert_eq!(d.alloc_color(C, 0, 0, 0, 0).unwrap().0, 1 | 0x40);
        d.free_client(C);
        assert_eq!(d.free, [8, 8, 4]);
    }
}
