//! Everything the server draws with: the update and pointer PDUs, drawing
//! orders, the caches (bitmaps, glyphs, brushes, colour tables, cursors,
//! the desktop save area, the persistent bitmap cache), and the software
//! framebuffer in `session::Shared::screen` that front-ends read. Port of
//! rdesktop's orders.c, cache.c, pstcache.c, the update and pointer parts of
//! rdp.c and rdp5.c, and the old shim.rs (xwin.c's drawing).
//!
//! Runs on the session thread. All state lives in [`Graphics`], so sessions
//! in one process don't share any.

mod cache;
mod colour;
mod draw;
mod orders;
mod pointer;
mod pstcache;
pub mod surface;

use std::sync::{Arc, Mutex, MutexGuard};

use crate::config::Config;
use crate::error::Result;
use crate::session::{CursorImage, CursorShape, Event, Frontend, Screen, Shared};
use crate::stream::Reader;

use cache::{BitmapCache, BrushCache, CursorCache, DeskCache, GlyphCache, TextCache};
use colour::{Colours, bytes_per_pixel};
use orders::{NINEGRID_CACHE_ENTRIES, NineGridEntry, OrderState, StreamBitmap};
use pstcache::PstCache;
use surface::{Bitmap, Surface};

pub use pstcache::namespace as cache_namespace;

/// What graphics needs to know about the connection, from the demand active
/// PDU and the user's settings.
#[derive(Clone, Debug)]
pub struct Params {
    /// The session's colour depth: 8, 15, 16, 24 or 32.
    pub depth: u32,
    pub width: u32,
    pub height: u32,
    /// 4 or 5.
    pub rdp_version: u8,
    /// The user's bitmap-cache setting, and whether to keep it on disk.
    pub bitmap_cache: bool,
    pub persistent_cache: bool,
    /// Draw the cursor locally (the user's "local cursor" option).
    pub local_cursor: bool,
    /// Names the persistent cache files: [`cache_namespace`] of the server,
    /// port, domain and user, as rdesktop names them. Empty is "default".
    pub cache_namespace: String,
}

impl Params {
    /// The user's settings, before the server has said anything.
    pub fn from_config(c: &Config) -> Self {
        Params {
            depth: c.depth,
            width: c.width,
            height: c.height,
            rdp_version: 5,
            bitmap_cache: c.bitmap_cache,
            persistent_cache: c.persistent_bitmap_cache,
            local_cursor: c.local_cursor,
            cache_namespace: cache_namespace(&c.server, c.port, &c.domain, &c.username),
        }
    }
}

/// The largest framebuffer side we make: RDP 5 servers allow 4096.
const MAX_SIDE: u32 = 4096;
/// The largest bitmap we'll unpack from one update.
const MAX_BITMAP_PIXELS: usize = (MAX_SIDE * MAX_SIDE) as usize;

/// Precache bitmaps from disk when the key list goes out (rdesktop's
/// default), except at 8 bpp, where the palette isn't known yet.
const PRECACHE: bool = true;

// Update types.
const UPDATE_ORDERS: u16 = 0;
const UPDATE_BITMAP: u16 = 1;
const UPDATE_PALETTE: u16 = 2;
const UPDATE_SYNCHRONIZE: u16 = 3;

// Slow-path pointer messages.
const POINTER_SYSTEM: u16 = 1;
const POINTER_MOVE: u16 = 3;
const POINTER_COLOR: u16 = 6;
const POINTER_CACHED: u16 = 7;
const POINTER_NEW: u16 = 8;
const SYSPTR_NULL: u32 = 0;
const SYSPTR_DEFAULT: u32 = 0x7f00;

// Fast-path update codes.
const FP_ORDERS: u8 = 0;
const FP_BITMAP: u8 = 1;
const FP_PALETTE: u8 = 2;
const FP_SYNCHRONIZE: u8 = 3;
const FP_PTR_NULL: u8 = 5;
const FP_PTR_DEFAULT: u8 = 6;
const FP_PTR_POSITION: u8 = 8;
const FP_COLOR: u8 = 9;
const FP_CACHED: u8 = 10;
const FP_POINTER: u8 = 11;
const FP_LARGE_POINTER: u8 = 12;

const BITMAP_COMPRESSION: u16 = 0x0001;
const NO_BITMAP_COMPRESSION_HDR: u16 = 0x0400;

/// Something the server did we can't follow. Printed, as rdesktop logs it.
pub(crate) fn warn(args: std::fmt::Arguments) {
    eprintln!("vintrdp: {args}");
}

pub struct Graphics {
    shared: Arc<Shared>,
    core: Core,
}

impl Graphics {
    pub fn new(shared: Arc<Shared>) -> Self {
        let core = Core::new(Params::from_config(&shared.config));
        Graphics { shared, core }
    }

    /// A demand active PDU set the session up (or changed its size or
    /// depth): create or resize the framebuffer, reset order state, open the
    /// persistent cache. Sends `Event::Resized`.
    pub fn activate(&mut self, params: &Params) {
        self.core.activate(params, &sink(&self.shared));
    }

    /// For the bitmap cache rev. 2 capability: open the persistent cache
    /// (for `params.persistent_cache`) and say whether it's usable.
    pub fn persistent_cache_ready(&mut self, params: &Params) -> bool {
        self.core.persistent_cache_ready(params)
    }

    /// The persistent cache's keys for bitmap cache `cache_id` (0-2), for
    /// the persistent key list PDU. Empty when there's no persistent cache.
    pub fn persistent_keys(&mut self, cache_id: u8) -> Vec<[u8; 8]> {
        self.core.persistent_keys(cache_id)
    }

    /// A slow-path update PDU (TS_UPDATE_*), from `updateType` on: 0 orders,
    /// 1 bitmap, 2 palette, 3 synchronize. `r` starts just after
    /// `updateType`.
    pub fn update(&mut self, update_type: u16, r: &mut Reader) -> Result<()> {
        self.core.update(update_type, r, &sink(&self.shared))
    }

    /// A slow-path pointer PDU (TS_POINTER_PDU), from `messageType` on. `r`
    /// starts just after `messageType`, at the padding.
    pub fn pointer(&mut self, message_type: u16, r: &mut Reader) -> Result<()> {
        self.core.pointer(message_type, r, &sink(&self.shared))
    }

    /// A fast-path update, already reassembled and decompressed: `code` is
    /// FASTPATH_UPDATETYPE_* (0 orders, 1 bitmap, 2 palette, 3 sync, 5 null
    /// pointer, 6 default pointer, 8 pointer position, 9 colour pointer,
    /// 10 cached pointer, 11 new pointer, 12 large pointer).
    pub fn fastpath_update(&mut self, code: u8, r: &mut Reader) -> Result<()> {
        self.core.fastpath_update(code, r, &sink(&self.shared))
    }

    /// Tell the front-end about what was drawn since the last call
    /// (`Event::Updated`), if anything. Called after each PDU.
    pub fn flush(&mut self) {
        self.core.flush(&sink(&self.shared));
    }

    /// The session is over: save the persistent cache's state.
    pub fn save_state(&mut self) {
        self.core.save_state();
    }

    /// Drop the framebuffer (the session ended, or is reconnecting at a new
    /// size).
    pub fn destroy(&mut self) {
        lock(&self.shared.screen).surface = None;
    }
}

fn sink(shared: &Shared) -> Sink<'_> {
    Sink { screen: &shared.screen, frontend: &*shared.frontend, config: &shared.config }
}

/// Where drawing goes and events are sent: the session's, or a test's.
pub(crate) struct Sink<'a> {
    pub screen: &'a Mutex<Screen>,
    pub frontend: &'a dyn Frontend,
    pub config: &'a Config,
}

/// A front-end that panicked mid-read mustn't stop the session drawing.
fn lock(m: &Mutex<Screen>) -> MutexGuard<'_, Screen> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The framebuffer for the length of a PDU, if there is one, noting
/// whether anything was drawn.
pub(crate) struct Canvas<'a> {
    surface: Option<&'a mut Surface>,
    dirty: bool,
}

impl<'a> Canvas<'a> {
    pub fn new(surface: Option<&'a mut Surface>) -> Self {
        Canvas { surface, dirty: false }
    }

    pub fn draw(&mut self, f: impl FnOnce(&mut Surface)) {
        if let Some(s) = self.surface.as_deref_mut() {
            f(s);
            self.dirty = true;
        }
    }

    pub fn surface(&self) -> Option<&Surface> {
        self.surface.as_deref()
    }
}

/// The graphics state proper: what rdesktop kept in globals.
pub(crate) struct Core {
    params: Params,
    colours: Colours,
    orders: OrderState,
    bitmaps: BitmapCache,
    glyphs: GlyphCache,
    text: TextCache,
    desk: DeskCache,
    cursors: CursorCache,
    brushes: BrushCache,
    ninegrids: Vec<Option<NineGridEntry>>,
    stream: StreamBitmap,
    pst: PstCache,
    /// Drawn on since the last `Event::Updated`.
    dirty: bool,
}

impl Core {
    pub fn new(params: Params) -> Self {
        Core {
            colours: Colours::new(params.depth),
            params,
            orders: OrderState::default(),
            bitmaps: BitmapCache::new(),
            glyphs: GlyphCache::new(),
            text: TextCache::new(),
            desk: DeskCache::new(),
            cursors: CursorCache::new(),
            brushes: BrushCache::new(),
            ninegrids: (0..NINEGRID_CACHE_ENTRIES).map(|_| None).collect(),
            stream: StreamBitmap::default(),
            pst: PstCache::new(),
            dirty: false,
        }
    }

    fn apply(&mut self, params: &Params) {
        self.params = params.clone();
        self.colours.depth = params.depth;
    }

    /// rd_create_ui, ui_create_window and ui_resize_window, then
    /// reset_order_state.
    pub fn activate(&mut self, params: &Params, sink: &Sink) {
        self.apply(params);
        let (w, h) = (params.width.min(MAX_SIDE), params.height.min(MAX_SIDE));
        let mut screen = lock(sink.screen);
        let event = match screen.surface.as_mut() {
            None => {
                screen.surface = Some(Surface::new(w as usize, h as usize));
                Some(true)
            }
            Some(s) => {
                s.reset_clip();
                ((s.width, s.height) != (w as usize, h as usize)).then(|| {
                    s.resize(w as usize, h as usize);
                    false
                })
            }
        };
        drop(screen);
        if let Some(created) = event {
            if created {
                sink.frontend.notify(Event::Title(sink.config.session_title(params.rdp_version < 5)));
            }
            sink.frontend.notify(Event::Resized { width: w, height: h });
        }
        self.reset_order_state();
        if !self.pst.is_open(2) {
            self.persistent_cache_ready(params);
        }
    }

    fn reset_order_state(&mut self) {
        self.orders = OrderState::default();
        self.ninegrids.iter_mut().for_each(|n| *n = None);
        self.stream = StreamBitmap::default();
    }

    /// pstcache_init(2).
    pub fn persistent_cache_ready(&mut self, params: &Params) -> bool {
        self.apply(params);
        let ok = params.bitmap_cache && params.persistent_cache && self.pst.init(2, params.depth, &params.cache_namespace);
        self.bitmaps.set_persistent(2, self.pst.is_open(2));
        ok
    }

    /// pstcache_enumerate: the keys, in cell order. Bitmaps used recently
    /// are loaded now, and the LRU list is rebuilt from the stamps.
    pub fn persistent_keys(&mut self, id: u8) -> Vec<[u8; 8]> {
        if !(self.params.bitmap_cache && self.params.persistent_cache && self.pst.is_open(id)) || self.pst.enumerated {
            return Vec::new();
        }
        let cells = self.pst.cells(id);
        for (idx, c) in cells.iter().enumerate() {
            if PRECACHE && c.stamp != 0 && self.params.depth > 8 {
                self.load_persistent(id, idx as u16);
            }
        }
        let mut order: Vec<u16> = (0..cells.len() as u16).collect();
        order.sort_by_key(|&i| cells[i as usize].stamp);
        self.bitmaps.rebuild(id, &order);
        self.pst.enumerated = true;
        cells.iter().map(|c| c.key).collect()
    }

    /// cache_save_state: stamp the persistent cells in LRU order.
    pub fn save_state(&mut self) {
        let mut t = 0u32;
        for id in 0..3 {
            if self.bitmaps.is_persistent(id) {
                for idx in self.bitmaps.lru_order(id) {
                    t += 1;
                    self.pst.touch(id, idx, t);
                }
            }
        }
    }

    /// Server-format pixels (`bytes` per pixel, rows top first) to ours.
    fn make_bitmap(&self, bytes: usize, width: usize, height: usize, data: &[u8]) -> Bitmap {
        Bitmap { width, height, px: self.colours.convert(self.colours.depth_for(bytes), width, height, data) }
    }

    fn decompress(&self, data: &[u8], width: usize, height: usize, bytes: usize) -> Option<Vec<u8>> {
        let need = width.checked_mul(height)?.checked_mul(bytes)?;
        let out = crate::codec::bitmap::decompress(data, width, height, self.colours.depth_for(bytes))?;
        (out.len() >= need).then_some(out)
    }

    fn put_bitmap(&mut self, id: u8, idx: u16, bitmap: Bitmap) {
        if let Some(evicted) = self.bitmaps.put(id, idx, bitmap) {
            self.pst.touch(id, evicted, 0);
        }
    }

    /// pstcache_load_bitmap.
    fn load_persistent(&mut self, id: u8, idx: u16) -> bool {
        if !self.params.persistent_cache {
            return false;
        }
        let Some((w, h, data)) = self.pst.load(id, idx) else { return false };
        let bitmap = self.make_bitmap(bytes_per_pixel(self.params.depth), w, h, &data);
        self.put_bitmap(id, idx, bitmap);
        true
    }

    /// cache_get_bitmap: from memory, or else the persistent cache.
    fn cached_bitmap(&mut self, id: u8, idx: u16) -> Option<&Bitmap> {
        if !self.bitmaps.contains(id, idx) && !(self.bitmaps.is_persistent(id) && self.load_persistent(id, idx)) {
            return None;
        }
        self.bitmaps.get(id, idx)
    }

    /// Run `f` with the framebuffer locked, then note what it drew.
    fn with_canvas<R>(&mut self, sink: &Sink, f: impl FnOnce(&mut Self, &mut Canvas) -> R) -> R {
        let mut screen = lock(sink.screen);
        let mut cv = Canvas::new(screen.surface.as_mut());
        let out = f(self, &mut cv);
        // An order that failed half way mustn't leave its clip behind.
        if let Some(s) = cv.surface.as_deref_mut() {
            s.reset_clip();
        }
        self.dirty |= cv.dirty;
        out
    }

    /// process_update_pdu.
    pub fn update(&mut self, update_type: u16, r: &mut Reader, sink: &Sink) -> Result<()> {
        match update_type {
            UPDATE_ORDERS => {
                r.skip(2)?;
                let count = r.u16()?;
                r.skip(2)?;
                self.with_canvas(sink, |core, cv| core.process_orders(r, count, cv))
            }
            UPDATE_BITMAP => self.with_canvas(sink, |core, cv| core.bitmap_updates(r, cv)),
            UPDATE_PALETTE => self.palette(r),
            UPDATE_SYNCHRONIZE => Ok(()),
            _ => {
                warn(format_args!("unhandled update type {update_type}"));
                Ok(())
            }
        }
    }

    /// process_ts_fp_update_by_code.
    pub fn fastpath_update(&mut self, code: u8, r: &mut Reader, sink: &Sink) -> Result<()> {
        match code {
            FP_ORDERS => {
                let count = r.u16()?;
                self.with_canvas(sink, |core, cv| core.process_orders(r, count, cv))
            }
            FP_BITMAP => {
                r.skip(2)?; // updateType
                self.with_canvas(sink, |core, cv| core.bitmap_updates(r, cv))
            }
            FP_PALETTE => {
                r.skip(2)?; // updateType
                self.palette(r)
            }
            FP_SYNCHRONIZE => Ok(()),
            FP_PTR_NULL => {
                set_cursor(sink, CursorShape::Hidden);
                Ok(())
            }
            FP_PTR_DEFAULT => {
                self.system_pointer(SYSPTR_DEFAULT, sink);
                Ok(())
            }
            FP_PTR_POSITION => {
                // We don't move the local pointer.
                r.skip(4)
            }
            FP_COLOR => self.colour_pointer(r, 24, sink),
            FP_CACHED => self.cached_pointer(r, sink),
            FP_POINTER => {
                let bpp = r.u16()?;
                self.colour_pointer(r, bpp as u32, sink)
            }
            FP_LARGE_POINTER => self.large_pointer(r, sink),
            _ => {
                warn(format_args!("unhandled fast-path update {code}"));
                Ok(())
            }
        }
    }

    /// process_bitmap_updates.
    fn bitmap_updates(&mut self, r: &mut Reader, cv: &mut Canvas) -> Result<()> {
        let n = r.u16()?;
        for _ in 0..n {
            self.bitmap_data(r, cv)?;
        }
        Ok(())
    }

    /// process_bitmap_data: one rectangle of a bitmap update.
    fn bitmap_data(&mut self, r: &mut Reader, cv: &mut Canvas) -> Result<()> {
        let (left, top, right, bottom) = (r.u16()?, r.u16()?, r.u16()?, r.u16()?);
        let (width, height) = (r.u16()? as usize, r.u16()? as usize);
        let bpp = r.u16()? as u32;
        let flags = r.u16()?;
        let bufsize = r.u16()?;
        let bytes = (bpp as usize).div_ceil(8);
        if bytes == 0 || bytes > 4 || width == 0 || height == 0 {
            crate::bail!("bitmap update of {width}x{height} at {bpp} bpp");
        }
        let depth = if matches!(bpp, 8 | 15 | 16 | 24 | 32) { bpp } else { self.colours.depth_for(bytes) };
        // Both u16, so this can't overflow even 32 bits; the byte count can.
        let pixels = width * height;
        let (cx, cy) = (right as i32 - left as i32 + 1, bottom as i32 - top as i32 + 1);

        let data = if flags & BITMAP_COMPRESSION == 0 {
            let Some(size) = pixels.checked_mul(bytes) else { crate::bail!("bitmap update of {width}x{height} is too big") };
            let data = r.bytes(size)?;
            orders::flip_rows(data, width * bytes, height)
        } else {
            let size = if flags & NO_BITMAP_COMPRESSION_HDR != 0 {
                bufsize
            } else {
                // TS_CD_HEADER: first row size, main body size, scan width,
                // uncompressed size.
                r.skip(2)?;
                let size = r.u16()?;
                r.skip(4)?;
                size
            };
            let data = r.bytes(size as usize)?;
            if pixels > MAX_BITMAP_PIXELS {
                warn(format_args!("bitmap update of {width}x{height} is too big"));
                return Ok(());
            }
            match self.decompress(data, width, height, bytes) {
                Some(d) => d,
                None => {
                    warn(format_args!("failed to decompress a bitmap update"));
                    return Ok(());
                }
            }
        };
        let px = self.colours.convert(depth, width, height, &data);
        cv.draw(|s| s.blit(draw::ROP2_COPY, left as i32, top as i32, cx, cy, &px, width, height, 0, 0));
        Ok(())
    }

    /// process_palette: the 8 bpp colour table.
    fn palette(&mut self, r: &mut Reader) -> Result<()> {
        r.skip(2)?;
        let n = r.u16()? as usize;
        r.skip(2)?;
        let entries = r.bytes(n * 3)?;
        let mut palette = [0u32; 256];
        for (p, e) in palette.iter_mut().zip(entries.chunks_exact(3)) {
            *p = (e[0] as u32) << 16 | (e[1] as u32) << 8 | e[2] as u32;
        }
        self.colours.palette = palette;
        Ok(())
    }

    /// process_pointer_pdu.
    pub fn pointer(&mut self, message_type: u16, r: &mut Reader, sink: &Sink) -> Result<()> {
        r.skip(2)?; // pad
        match message_type {
            POINTER_MOVE => r.skip(4),
            POINTER_COLOR => self.colour_pointer(r, 24, sink),
            POINTER_CACHED => self.cached_pointer(r, sink),
            POINTER_SYSTEM => {
                let kind = r.u32()?;
                self.system_pointer(kind, sink);
                Ok(())
            }
            POINTER_NEW => {
                let bpp = r.u16()?;
                self.colour_pointer(r, bpp as u32, sink)
            }
            _ => {
                warn(format_args!("unhandled pointer message {message_type}"));
                Ok(())
            }
        }
    }

    fn system_pointer(&mut self, kind: u32, sink: &Sink) {
        match kind {
            SYSPTR_NULL => set_cursor(sink, CursorShape::Hidden),
            SYSPTR_DEFAULT => set_cursor(sink, CursorShape::Default),
            _ => warn(format_args!("unhandled system pointer {kind:#x}")),
        }
    }

    /// process_colour_pointer_common: a new pointer, shown and cached.
    fn colour_pointer(&mut self, r: &mut Reader, bpp: u32, sink: &Sink) -> Result<()> {
        let cache_idx = r.u16()?;
        let (x, y) = (r.u16()?, r.u16()?);
        let (width, height) = (r.u16()?, r.u16()?);
        let masklen = r.u16()? as usize;
        let datalen = r.u16()? as usize;
        let xor = r.bytes(datalen)?;
        let and = r.bytes(masklen)?;
        self.new_pointer(cache_idx, x, y, width, height, and, xor, bpp, sink);
        Ok(())
    }

    /// TS_FP_LARGEPOINTERATTRIBUTE: the same, up to 384x384.
    fn large_pointer(&mut self, r: &mut Reader, sink: &Sink) -> Result<()> {
        let bpp = r.u16()? as u32;
        let cache_idx = r.u16()?;
        let (x, y) = (r.u16()?, r.u16()?);
        let (width, height) = (r.u16()?, r.u16()?);
        let andlen = r.u32()? as usize;
        let xorlen = r.u32()? as usize;
        let xor = r.bytes(xorlen)?;
        let and = r.bytes(andlen)?;
        self.new_pointer(cache_idx, x, y, width, height, and, xor, bpp, sink);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn new_pointer(&mut self, cache_idx: u16, x: u16, y: u16, width: u16, height: u16, and: &[u8], xor: &[u8], bpp: u32, sink: &Sink) {
        // Not worth making a cursor we won't use.
        if self.params.local_cursor {
            return;
        }
        let cursor = pointer::decode(&self.colours, x, y, width, height, and, xor, bpp).map(Arc::new);
        if cursor.is_none() {
            warn(format_args!("can't use a {width}x{height} pointer at {bpp} bpp"));
        }
        set_cursor(sink, shape(cursor.clone()));
        self.cursors.put(cache_idx, cursor);
    }

    fn cached_pointer(&mut self, r: &mut Reader, sink: &Sink) -> Result<()> {
        let idx = r.u16()?;
        if !self.params.local_cursor {
            set_cursor(sink, shape(self.cursors.get(idx)));
        }
        Ok(())
    }

    /// Publish drawing done so far to the front-end.
    pub fn flush(&mut self, sink: &Sink) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        dump_framebuffer(&lock(sink.screen));
        sink.frontend.notify(Event::Updated);
    }
}

/// A missing cursor is rdesktop's NULL one: hidden.
fn shape(cursor: Option<Arc<CursorImage>>) -> CursorShape {
    cursor.map_or(CursorShape::Hidden, CursorShape::Image)
}

fn set_cursor(sink: &Sink, shape: CursorShape) {
    lock(sink.screen).cursor = shape;
    sink.frontend.notify(Event::Cursor);
}

/// Debug aid: with VINTRDP_DUMP=/path/file.ppm set, write the framebuffer
/// there after every update so sessions can be checked headlessly.
fn dump_framebuffer(screen: &Screen) {
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    let Some(path) = PATH.get_or_init(|| std::env::var("VINTRDP_DUMP").ok()) else { return };
    let Some(s) = &screen.surface else { return };
    let mut out = format!("P6\n{} {}\n255\n", s.width, s.height).into_bytes();
    out.extend(s.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]));
    let _ = crate::util::write_atomic(std::path::Path::new(path), &out);
}

#[cfg(test)]
mod tests;
