//! Colormaps and colours. The one visual is TrueColor, 8 bits a channel, so
//! a colour's pixel is worked out, never allocated, and cells can't be
//! written: AllocColorCells and the like fail as X.org's do on TrueColor.
//! Colour names are X.org's rgb.txt (data/rgb.txt, MIT licence).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::event::{self, Builder};
use crate::server::*;
use crate::wire::Reader;

// STUB(x11): TrueColor only; a PseudoColor visual with writable cells (AllocColorCells, StoreColors) is phase 3
pub struct Colormap {
    pub window: u32,
}

/// rgb.txt by lower-case name.
fn names() -> &'static HashMap<Vec<u8>, (u8, u8, u8)> {
    static NAMES: OnceLock<HashMap<Vec<u8>, (u8, u8, u8)>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut m = HashMap::new();
        for line in include_str!("../data/rgb.txt").lines() {
            let mut f = line.split_whitespace();
            let (Some(r), Some(g), Some(b)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            let (Ok(r), Ok(g), Ok(b)) = (r.parse(), g.parse(), b.parse()) else {
                continue;
            };
            let name: Vec<&str> = f.collect();
            m.insert(name.join(" ").to_ascii_lowercase().into_bytes(), (r, g, b));
        }
        m
    })
}

/// A colour by name, as X.org's OsLookupColor: case doesn't matter.
pub fn lookup(name: &[u8]) -> Option<(u16, u16, u16)> {
    let (r, g, b) = *names().get(&name.to_ascii_lowercase())?;
    Some((r as u16 * 257, g as u16 * 257, b as u16 * 257))
}

/// The TrueColor pixel for a colour, and the colour it shows.
pub fn true_color(r: u16, g: u16, b: u16) -> (u32, (u16, u16, u16)) {
    let (r, g, b) = (r >> 8, g >> 8, b >> 8);
    (
        (r as u32) << 16 | (g as u32) << 8 | b as u32,
        (r * 257, g * 257, b * 257),
    )
}

impl State {
    pub fn is_colormap(&self, id: u32) -> bool {
        id == COLORMAP || matches!(self.resources.get(&id), Some(Resource::Colormap(_)))
    }

    fn colormap(&self, id: u32) -> Result<u32, XError> {
        if self.is_colormap(id) {
            Ok(id)
        } else {
            Err(XError(BAD_COLOR, id))
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
        if visual != VISUAL {
            return Err(XError(BAD_MATCH, visual));
        }
        // AllocAll needs writable cells.
        if alloc != 0 {
            return Err(XError(BAD_MATCH, 0));
        }
        self.resources
            .insert(id, Resource::Colormap(Colormap { window }));
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
        let users: Vec<u32> = self
            .windows
            .values()
            .filter(|w| w.colormap == id)
            .map(|w| w.id)
            .collect();
        for w in users {
            self.windows.get_mut(&w).unwrap().colormap = 0;
            let ev = Builder::new(event::COLORMAP_NOTIFY, 0)
                .u32(w)
                .u32(0)
                .bool(true)
                .bool(false)
                .done();
            self.deliver(w, event::COLORMAP_CHANGE_MASK, &ev);
        }
    }

    /// ColormapNotify (not new) to the windows that use the colormap.
    fn colormap_installed(&self, id: u32, installed: bool) {
        for w in self.windows.values().filter(|w| w.colormap == id) {
            let ev = Builder::new(event::COLORMAP_NOTIFY, 0)
                .u32(w.id)
                .u32(id)
                .bool(false)
                .bool(installed)
                .done();
            self.deliver(w.id, event::COLORMAP_CHANGE_MASK, &ev);
        }
    }

    pub fn copy_colormap_and_free(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        let src = self.colormap(r.u32()?)?;
        let id = self.new_id(ctx, id)?;
        let window = match self.resources.get(&src) {
            Some(Resource::Colormap(c)) => c.window,
            _ => ROOT,
        };
        // Nothing to move: TrueColor allocates nothing.
        self.resources
            .insert(id, Resource::Colormap(Colormap { window }));
        Ok(None)
    }

    pub fn install_colormap(&mut self, r: &mut Reader) -> Answer {
        let id = self.colormap(r.u32()?)?;
        if self.installed != id {
            let old = std::mem::replace(&mut self.installed, id);
            self.colormap_installed(old, false);
            self.colormap_installed(id, true);
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
        }
        Ok(None)
    }

    pub fn alloc_color(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        self.colormap(r.u32()?)?;
        let (red, green, blue) = (r.u16()?, r.u16()?, r.u16()?);
        let (pixel, (r, g, b)) = true_color(red, green, blue);
        let mut reply = ctx.reply(0);
        reply.u16(r).u16(g).u16(b).zeros(2).u32(pixel);
        Ok(Some(reply.finish()))
    }

    pub fn alloc_named_color(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let c = self.colormap(r.u32()?)?;
        let n = r.u16()? as usize;
        r.skip(2)?;
        let (er, eg, eb) = self.named(r.bytes(n)?, c)?;
        let (pixel, (vr, vg, vb)) = true_color(er, eg, eb);
        let mut reply = ctx.reply(0);
        reply
            .u32(pixel)
            .u16(er)
            .u16(eg)
            .u16(eb)
            .u16(vr)
            .u16(vg)
            .u16(vb);
        Ok(Some(reply.finish()))
    }

    fn named(&self, name: &[u8], cmap: u32) -> Result<(u16, u16, u16), XError> {
        let c = lookup(name);
        if c.is_none() && self.verbose {
            eprintln!(
                "vintrdp: X11: no colour {:?}",
                String::from_utf8_lossy(name)
            );
        }
        c.ok_or(XError(BAD_NAME, cmap))
    }

    pub fn lookup_color(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let c = self.colormap(r.u32()?)?;
        let n = r.u16()? as usize;
        r.skip(2)?;
        let (er, eg, eb) = self.named(r.bytes(n)?, c)?;
        let (_, (vr, vg, vb)) = true_color(er, eg, eb);
        let mut reply = ctx.reply(0);
        reply.u16(er).u16(eg).u16(eb).u16(vr).u16(vg).u16(vb);
        Ok(Some(reply.finish()))
    }

    /// AllocColorCells and AllocColorPlanes: a TrueColor map has no cells to give.
    pub fn alloc_cells(&mut self, r: &mut Reader) -> Answer {
        let c = self.colormap(r.u32()?)?;
        Err(XError(BAD_ALLOC, c))
    }

    /// StoreColors and StoreNamedColor: TrueColor cells are read-only.
    pub fn store_colors(&mut self, r: &mut Reader) -> Answer {
        let c = self.colormap(r.u32()?)?;
        Err(XError(BAD_ACCESS, c))
    }

    pub fn free_colors(&mut self, r: &mut Reader) -> Answer {
        self.colormap(r.u32()?)?;
        r.u32()?;
        while r.remaining() >= 4 {
            let p = r.u32()?;
            if p > 0xff_ffff {
                return Err(XError(BAD_VALUE, p));
            }
        }
        Ok(None)
    }

    pub fn query_colors(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        self.colormap(r.u32()?)?;
        let mut pixels = Vec::new();
        while r.remaining() >= 4 {
            let p = r.u32()?;
            if p > 0xff_ffff {
                return Err(XError(BAD_VALUE, p));
            }
            pixels.push(p);
        }
        let mut reply = ctx.reply(0);
        reply.u16(pixels.len() as u16).zeros(22);
        for p in pixels {
            let c = |s: u32| ((p >> s) & 0xff) as u16 * 257;
            reply.u16(c(16)).u16(c(8)).u16(c(0)).zeros(2);
        }
        Ok(Some(reply.finish()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_pixels() {
        assert_eq!(
            lookup(b"Light Gray"),
            Some((211 * 257, 211 * 257, 211 * 257))
        );
        assert_eq!(lookup(b"lightgray"), lookup(b"LightGray"));
        assert_eq!(lookup(b"red"), Some((0xffff, 0, 0)));
        assert_eq!(lookup(b"nonesuch"), None);
        assert_eq!(
            true_color(0xffff, 0x8000, 0x00ff),
            (0xff_8000, (0xffff, 0x8080, 0))
        );
    }
}
