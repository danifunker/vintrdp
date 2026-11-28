//! Colormaps and colours: the requests, on cmap.rs's maps (X.org's
//! allocation, ported), for every visual (visual.rs). One map is installed
//! at a time, as on an 8-bit terminal, and the screen is shown through it.
//! Colour names are X.org's rgb.txt (data/rgb.txt, MIT licence), then
//! IRIX's own (data/rgb-sgi.txt: sgilightgray and the like, which IRIX
//! programs ask for).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::cmap::{self, Cmap, CmapError};
use crate::event::{self, Builder};
use crate::server::*;
use crate::visual::Lut;
use crate::wire::Reader;
pub struct Colormap {
    pub window: u32,
    pub visual: u32,
    pub map: Cmap,
}

/// rgb.txt, then IRIX's names, by lower-case name.
fn names() -> &'static HashMap<Vec<u8>, (u8, u8, u8)> {
    static NAMES: OnceLock<HashMap<Vec<u8>, (u8, u8, u8)>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut m = HashMap::new();
        for line in include_str!("../data/rgb.txt").lines().chain(include_str!("../data/rgb-sgi.txt").lines()) {
            let mut f = line.split_whitespace();
            let (Some(r), Some(g), Some(b)) = (f.next(), f.next(), f.next()) else { continue };
            let (Ok(r), Ok(g), Ok(b)) = (r.parse(), g.parse(), b.parse()) else { continue };
            let name: Vec<&str> = f.collect();
            m.entry(name.join(" ").to_ascii_lowercase().into_bytes()).or_insert((r, g, b));
        }
        m
    })
}

/// A colour by name, as X.org's OsLookupColor: case doesn't matter.
pub fn lookup(name: &[u8]) -> Option<(u16, u16, u16)> {
    let (r, g, b) = *names().get(&name.to_ascii_lowercase())?;
    Some((r as u16 * 257, g as u16 * 257, b as u16 * 257))
}

/// The TrueColor pixel for a colour at depth 24, and the colour it shows.
#[cfg(test)]
pub fn true_color(r: u16, g: u16, b: u16) -> (u32, (u16, u16, u16)) {
    let (r, g, b) = (r >> 8, g >> 8, b >> 8);
    ((r as u32) << 16 | (g as u32) << 8 | b as u32, (r * 257, g * 257, b * 257))
}

impl State {
    pub fn is_colormap(&self, id: u32) -> bool {
        matches!(self.resources.get(&id), Some(Resource::Colormap(_)))
    }

    fn colormap(&self, id: u32) -> Result<u32, XError> {
        if self.is_colormap(id) { Ok(id) } else { Err(XError(BAD_COLOR, id)) }
    }

    pub fn cmap(&self, id: u32) -> Result<&Colormap, XError> {
        match self.resources.get(&id) {
            Some(Resource::Colormap(c)) => Ok(c),
            _ => Err(XError(BAD_COLOR, id)),
        }
    }

    fn cmap_mut(&mut self, id: u32) -> Result<&mut Colormap, XError> {
        match self.resources.get_mut(&id) {
            Some(Resource::Colormap(c)) => Ok(c),
            _ => Err(XError(BAD_COLOR, id)),
        }
    }

    /// The colormap's visual id.
    pub fn colormap_visual(&self, id: u32) -> Option<u32> {
        self.cmap(id).ok().map(|c| c.visual)
    }

    /// The server's default colormap, as X.org's miCreateDefColormap and
    /// RENDER's colour cube make it.
    pub fn create_default_colormap(&mut self) {
        let v = self.visuals[0];
        let (mut map, black, white) = Cmap::new_default(v.v);
        let pixels = map.render_colors();
        (self.black, self.white) = (black, white);
        self.resources.insert(COLORMAP, Resource::Colormap(Colormap { window: ROOT, visual: v.id, map }));
        self.installed = COLORMAP;
        self.lut = Lut::of(&self.cmap(COLORMAP).unwrap().map, self.depth);
        self.render_init(pixels);
    }

    /// The screen is shown through the installed map: redraw it all when
    /// that changes what it shows.
    pub fn colormap_shown(&mut self) {
        let Ok(c) = self.cmap(self.installed) else { return };
        let lut = Lut::of(&c.map, self.depth);
        if lut != self.lut {
            self.lut = lut;
            self.damage(crate::region::Box::new(0, 0, self.width as i32, self.height as i32));
        }
    }

    pub fn create_colormap(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let alloc = ctx.data;
        let id = r.u32()?;
        let (window, visual) = (r.u32()?, r.u32()?);
        if alloc > 1 {
            return Err(XError(BAD_VALUE, alloc as u32));
        }
        let id = self.new_id(ctx, id)?;
        self.win(window)?;
        let v = self.visual_def(visual).ok_or(XError(BAD_MATCH, visual))?;
        let map = Cmap::new(v.v, alloc != 0, ctx.slot).map_err(|e| XError(e.0, e.1.unwrap_or(id)))?;
        self.resources.insert(id, Resource::Colormap(Colormap { window, visual, map }));
        Ok(None)
    }

    pub fn free_colormap(&mut self, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        if id != COLORMAP {
            self.colormap_freed(id);
            self.resources.remove(&id);
        }
        Ok(None)
    }

    /// Windows using a colormap that goes get None, with ColormapNotify.
    pub fn colormap_freed(&mut self, id: u32) {
        if self.installed == id {
            self.installed = COLORMAP;
            self.colormap_installed(id, false);
            self.colormap_installed(COLORMAP, true);
        }
        let users: Vec<u32> = self.windows.values().filter(|w| w.colormap == id).map(|w| w.id).collect();
        for w in users {
            self.windows.get_mut(&w).unwrap().colormap = 0;
            let ev = Builder::new(event::COLORMAP_NOTIFY, 0).u32(w).u32(0).bool(true).bool(false).done();
            self.deliver(w, event::COLORMAP_CHANGE_MASK, &ev);
        }
        self.colormap_shown();
    }

    /// ColormapNotify (not new) to the windows that use the colormap.
    fn colormap_installed(&self, id: u32, installed: bool) {
        for w in self.windows.values().filter(|w| w.colormap == id) {
            let ev = Builder::new(event::COLORMAP_NOTIFY, 0).u32(w.id).u32(id).bool(false).bool(installed).done();
            self.deliver(w.id, event::COLORMAP_CHANGE_MASK, &ev);
        }
    }

    /// A client went: the cells it had in others' maps are freed.
    pub fn colormaps_client_gone(&mut self, slot: u32) {
        for (id, res) in self.resources.iter_mut() {
            if let Resource::Colormap(c) = res {
                if id >> ID_BITS != slot {
                    c.map.free_client(slot);
                }
            }
        }
        self.colormap_shown();
    }

    pub fn copy_colormap_and_free(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        let src = self.colormap(r.u32()?)?;
        let id = self.new_id(ctx, id)?;
        let c = self.cmap_mut(src)?;
        let (window, visual) = (c.window, c.visual);
        let map = c.map.copy_and_free(ctx.slot);
        self.resources.insert(id, Resource::Colormap(Colormap { window, visual, map }));
        if self.installed == src {
            self.colormap_shown();
        }
        Ok(None)
    }

    pub fn install_colormap(&mut self, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        if self.installed != id {
            let old = std::mem::replace(&mut self.installed, id);
            self.colormap_installed(old, false);
            self.colormap_installed(id, true);
            self.colormap_shown();
        }
        Ok(None)
    }

    pub fn uninstall_colormap(&mut self, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        // The default comes back.
        if self.installed == id && id != COLORMAP {
            self.installed = COLORMAP;
            self.colormap_installed(id, false);
            self.colormap_installed(COLORMAP, true);
            self.colormap_shown();
        }
        Ok(None)
    }

    /// X.org's FindColorInRootCmap hint: a private map of the root's visual
    /// takes the default map's pixel for a colour where it can.
    fn hint(&self, id: u32, r: u16, g: u16, b: u16) -> u32 {
        match (self.cmap(id), self.cmap(COLORMAP)) {
            (Ok(c), Ok(d)) if id != COLORMAP && c.visual == d.visual => d.map.root_hint(r, g, b),
            _ => 0,
        }
    }

    fn alloc(&mut self, slot: u32, id: u32, r: u16, g: u16, b: u16) -> Result<(u32, u16, u16, u16), XError> {
        let hint = self.hint(id, r, g, b);
        // Looking in the default map for the hint leaves its id as X.org's
        // error value.
        let value = if hint != 0 || (id != COLORMAP && self.colormap_visual(id) == self.colormap_visual(COLORMAP)) { COLORMAP } else { id };
        let c = self.cmap_mut(id)?;
        let out = c.map.alloc_color(slot, r, g, b, hint).map_err(|e| err(e, value));
        if out.is_ok() && self.installed == id {
            self.colormap_shown();
        }
        out
    }

    pub fn alloc_color(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        let (red, green, blue) = (r.u16()?, r.u16()?, r.u16()?);
        let (pixel, r, g, b) = self.alloc(ctx.slot, id, red, green, blue)?;
        let mut reply = ctx.reply(0);
        reply.u16(r).u16(g).u16(b).zeros(2).u32(pixel);
        Ok(Some(reply.finish()))
    }

    pub fn alloc_named_color(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let c = self.colormap(r.u32()?)?;
        let n = r.u16()? as usize;
        r.skip(2)?;
        let (er, eg, eb) = self.named(r.bytes(n)?, c)?;
        let (pixel, vr, vg, vb) = self.alloc(ctx.slot, c, er, eg, eb)?;
        let mut reply = ctx.reply(0);
        reply.u32(pixel).u16(er).u16(eg).u16(eb).u16(vr).u16(vg).u16(vb);
        Ok(Some(reply.finish()))
    }

    fn named(&self, name: &[u8], cmap: u32) -> Result<(u16, u16, u16), XError> {
        let c = lookup(name);
        if c.is_none() && self.verbose {
            eprintln!("vintrdp: X11: no colour {:?}", String::from_utf8_lossy(name));
        }
        c.ok_or(XError(BAD_NAME, cmap))
    }

    pub fn lookup_color(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let c = self.colormap(r.u32()?)?;
        let n = r.u16()? as usize;
        r.skip(2)?;
        let (er, eg, eb) = self.named(r.bytes(n)?, c)?;
        let (vr, vg, vb) = cmap::resolve(&self.cmap(c)?.map.visual(), er, eg, eb);
        let mut reply = ctx.reply(0);
        reply.u16(er).u16(eg).u16(eb).u16(vr).u16(vg).u16(vb);
        Ok(Some(reply.finish()))
    }

    pub fn alloc_color_cells(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        let (colors, planes) = (r.u16()?, r.u16()?);
        if ctx.data > 1 {
            return Err(XError(BAD_VALUE, ctx.data as u32));
        }
        let (pixels, masks) = self.cmap_mut(id)?.map.alloc_cells(ctx.slot, colors, planes, ctx.data != 0).map_err(|e| err(e, id))?;
        let mut reply = ctx.reply(0);
        reply.u16(pixels.len() as u16).u16(masks.len() as u16).zeros(20);
        for p in pixels.into_iter().chain(masks) {
            reply.u32(p);
        }
        Ok(Some(reply.finish()))
    }

    pub fn alloc_color_planes(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        let (colors, reds, greens, blues) = (r.u16()?, r.u16()?, r.u16()?, r.u16()?);
        if ctx.data > 1 {
            return Err(XError(BAD_VALUE, ctx.data as u32));
        }
        let (pixels, rm, gm, bm) = self.cmap_mut(id)?.map.alloc_planes(ctx.slot, colors, reds, greens, blues, ctx.data != 0).map_err(|e| err(e, id))?;
        let mut reply = ctx.reply(0);
        reply.u16(pixels.len() as u16).zeros(2).u32(rm).u32(gm).u32(bm).zeros(8);
        for p in pixels {
            reply.u32(p);
        }
        Ok(Some(reply.finish()))
    }

    pub fn free_colors(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        let mask = r.u32()?;
        let mut pixels = Vec::new();
        while r.remaining() >= 4 {
            pixels.push(r.u32()?);
        }
        let out = self.cmap_mut(id)?.map.free_colors(ctx.slot, mask, &pixels).map_err(|e| err(e, id));
        if self.installed == id {
            self.colormap_shown();
        }
        out.map(|_| None)
    }

    fn store(&mut self, slot: u32, id: u32, items: &[(u32, u16, u16, u16, u8)]) -> Answer {
        let (changed, out) = self.cmap_mut(id)?.map.store_colors(slot, items);
        self.render_store_colors(id, items);
        if changed && self.installed == id {
            self.colormap_shown();
        }
        out.map(|_| None).map_err(|e| err(e, id))
    }

    pub fn store_colors(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        let mut items = Vec::new();
        while r.remaining() >= 12 {
            let (p, red, green, blue, flags) = (r.u32()?, r.u16()?, r.u16()?, r.u16()?, r.u8()?);
            r.skip(1)?;
            items.push((p, red, green, blue, flags));
        }
        self.store(ctx.slot, id, &items)
    }

    pub fn store_named_color(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        let pixel = r.u32()?;
        let n = r.u16()? as usize;
        r.skip(2)?;
        let (red, green, blue) = self.named(r.bytes(n)?, id)?;
        self.store(ctx.slot, id, &[(pixel, red, green, blue, ctx.data)])
    }

    pub fn query_colors(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        let mut pixels = Vec::new();
        while r.remaining() >= 4 {
            pixels.push(r.u32()?);
        }
        let colors = self.cmap(id)?.map.query_colors(&pixels).map_err(|e| err(e, id))?;
        let mut reply = ctx.reply(0);
        reply.u16(colors.len() as u16).zeros(22);
        for (r, g, b) in colors {
            reply.u16(r).u16(g).u16(b).zeros(2);
        }
        Ok(Some(reply.finish()))
    }
}

/// A map's error: X.org leaves some values as the request set them, which
/// is the colormap's id.
fn err(e: CmapError, id: u32) -> XError {
    XError(e.0, e.1.unwrap_or(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_pixels() {
        assert_eq!(lookup(b"Light Gray"), Some((211 * 257, 211 * 257, 211 * 257)));
        assert_eq!(lookup(b"lightgray"), lookup(b"LightGray"));
        assert_eq!(lookup(b"red"), Some((0xffff, 0, 0)));
        assert_eq!(lookup(b"nonesuch"), None);
        // IRIX's.
        assert_eq!(lookup(b"SGILightGray"), Some((170 * 257, 170 * 257, 170 * 257)));
        assert_eq!(lookup(b"sgi light grey"), lookup(b"sgilightgray"));
        assert_eq!(lookup(b"Indigo2"), Some((33 * 257, 136 * 257, 104 * 257)));
        assert_eq!(lookup(b"indigo"), Some((75 * 257, 0, 130 * 257)));
        assert_eq!(true_color(0xffff, 0x8000, 0x00ff), (0xff_8000, (0xffff, 0x8080, 0)));
    }
}
