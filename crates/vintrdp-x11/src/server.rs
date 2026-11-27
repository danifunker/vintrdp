//! The server thread: every client's requests, one at a time.

use std::collections::HashMap;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use x11rb_protocol::protocol::xproto;

use crate::atoms::Atoms;
use crate::keymap::{self, Keymap};
use crate::wire::{self, Order, Reader, Reply, Short, Writer};
use crate::{Host, Settings};

pub(crate) enum Msg {
    /// A client sent its connection setup.
    Connect { conn: u64, order: Order, major: u16, out: Sender<Vec<u8>>, stream: TcpStream, big: Arc<AtomicBool> },
    /// A whole request, its length field meaningless (BIG-REQUESTS).
    Request { conn: u64, req: Vec<u8> },
    Gone { conn: u64 },
    Stop,
}

// The server's own resources, in client 0's range.
pub(crate) const ROOT: u32 = 0x100;
pub(crate) const COLORMAP: u32 = 0x20;
pub(crate) const VISUAL: u32 = 0x21;
const WHITE: u32 = 0xff_ffff;
const BLACK: u32 = 0;
/// Each client's resource ids: 21 bits, the client's number above them.
const ID_BITS: u32 = 21;
const ID_MASK: u32 = (1 << ID_BITS) - 1;
const MAX_CLIENTS: usize = 255;
const ROOT_DEPTH: u8 = 24;
/// Pixmap formats (depth, bits per pixel), and the screen's depths, as Xvfb
/// has them at depth 24, so the two can be compared.
const FORMATS: [(u8, u8); 7] = [(1, 1), (4, 8), (8, 8), (15, 16), (16, 16), (24, 32), (32, 32)];
const DEPTHS: [u8; 7] = [24, 1, 4, 8, 15, 16, 32];
const VENDOR: &[u8] = b"vintrdp";

// Extensions, at the major opcodes they're given.
const BIG_REQUESTS: u8 = 128;
const XC_MISC: u8 = 129;
// STUB(x11): SHAPE (phase 3), RENDER (phase 5, optional)
const EXTENSIONS: [(&[u8], u8); 2] = [(b"BIG-REQUESTS", BIG_REQUESTS), (b"XC-MISC", XC_MISC)];

// Error codes.
const BAD_REQUEST: u8 = 1;
const BAD_VALUE: u8 = 2;
const BAD_WINDOW: u8 = 3;
const BAD_ATOM: u8 = 5;
const BAD_MATCH: u8 = 8;
const BAD_DRAWABLE: u8 = 9;
const BAD_GC: u8 = 13;
const BAD_ID_CHOICE: u8 = 14;
const BAD_LENGTH: u8 = 16;
const BAD_IMPLEMENTATION: u8 = 17;

/// An X error: its code and the bad value.
pub(crate) struct XError(u8, u32);

impl From<Short> for XError {
    fn from(_: Short) -> XError {
        XError(BAD_LENGTH, 0)
    }
}

type Answer = Result<Option<Vec<u8>>, XError>;

struct Client {
    /// Picks the resource-id range.
    slot: u32,
    order: Order,
    seq: u16,
    out: Sender<Vec<u8>>,
    stream: TcpStream,
    big: Arc<AtomicBool>,
}

/// What a client's resource id names.
enum Resource {
    Gc(Gc),
}

// STUB(x11): GC values are kept but nothing draws with them; tile, stipple, font and clip mask aren't checked
/// A graphics context: its values, by GC component bit (function, plane
/// mask, foreground, ...), for drawing in a later version.
#[derive(Clone)]
struct Gc {
    values: [u32; GC_VALUES],
}

const GC_VALUES: usize = 23;

impl Default for Gc {
    /// The protocol's defaults; tile, stipple and font come later.
    fn default() -> Self {
        let mut values = [0; GC_VALUES];
        values[0] = 3; // function: Copy
        values[1] = !0; // plane mask
        values[3] = 1; // background
        values[6] = 1; // cap style: Butt
        values[16] = 1; // graphics exposures
        values[21] = 4; // dashes
        values[22] = 1; // arc mode: PieSlice
        Gc { values }
    }
}

struct Property {
    name: u32,
    kind: u32,
    format: u8,
    /// Kept little-endian, converted for each client.
    data: Vec<u8>,
}

pub(crate) struct State {
    host: Box<dyn Host>,
    width: u16,
    height: u16,
    keymap: Keymap,
    atoms: Atoms,
    /// The root window's.
    properties: Vec<Property>,
    clients: HashMap<u64, Client>,
    resources: HashMap<u32, Resource>,
    focus: (u32, u8),
    /// Timeout, interval, prefer blanking, allow exposures.
    screen_saver: (u16, u16, u8, u8),
    pointer: (i16, i16),
    verbose: bool,
}

/// What the current request needs to know of its client.
struct Ctx {
    order: Order,
    seq: u16,
    /// The header's second byte.
    data: u8,
    slot: u32,
}

impl Ctx {
    fn reply(&self, data: u8) -> Reply {
        Reply::new(self.order, self.seq, data)
    }
}

pub(crate) fn run(rx: Receiver<Msg>, mut state: State) {
    state.paint_root();
    for msg in rx {
        match msg {
            Msg::Connect { conn, order, major, out, stream, big } => state.connect(conn, order, major, out, stream, big),
            Msg::Request { conn, req } => state.request(conn, &req),
            Msg::Gone { conn } => state.gone(conn),
            Msg::Stop => break,
        }
    }
    for c in state.clients.values() {
        let _ = c.stream.shutdown(Shutdown::Both);
    }
}

impl State {
    pub(crate) fn new(settings: Settings, host: Box<dyn Host>) -> State {
        State {
            host,
            width: settings.width,
            height: settings.height,
            keymap: settings.keymap,
            atoms: Atoms::default(),
            properties: Vec::new(),
            clients: HashMap::new(),
            resources: HashMap::new(),
            focus: (1, 1), // PointerRoot, RevertToPointerRoot
            screen_saver: (600, 600, 1, 1),
            pointer: (0, 0),
            verbose: settings.verbose,
        }
    }

    // STUB(x11): the screen keeps its starting size; the window being resized doesn't reach the server
    /// X's grey weave, as on an X terminal with nothing running.
    fn paint_root(&self) {
        const WEAVE: [u8; 4] = [0x88, 0x22, 0x44, 0x11];
        let (w, h) = (self.width as usize, self.height as usize);
        let px: Vec<u32> = (0..w * h).map(|i| if WEAVE[i / w % 4] >> (i % w % 8) & 1 != 0 { WHITE } else { BLACK }).collect();
        self.host.resize(self.width, self.height);
        self.host.draw(0, 0, w, h, &px);
        self.host.updated();
    }

    fn connect(&mut self, conn: u64, order: Order, major: u16, out: Sender<Vec<u8>>, stream: TcpStream, big: Arc<AtomicBool>) {
        let free = (1..=MAX_CLIENTS as u32).find(|s| !self.clients.values().any(|c| c.slot == *s));
        let failure = if major != 11 {
            Some("protocol version mismatch")
        } else if free.is_none() {
            Some("maximum number of clients reached")
        } else {
            None
        };
        if let Some(reason) = failure {
            let _ = out.send(setup_failed(order, reason));
            return;
        }
        let slot = free.unwrap();
        let _ = out.send(self.setup(order, slot));
        self.clients.insert(conn, Client { slot, order, seq: 0, out, stream, big });
        self.host.clients(self.clients.len());
    }

    fn gone(&mut self, conn: u64) {
        if let Some(c) = self.clients.remove(&conn) {
            self.resources.retain(|id, _| id >> ID_BITS != c.slot);
            self.host.clients(self.clients.len());
        }
    }

    /// The connection setup's reply.
    fn setup(&self, order: Order, slot: u32) -> Vec<u8> {
        let mut w = Writer::new(order);
        w.u8(1).u8(0).u16(11).u16(0).u16(0); // length, below
        w.u32(release()).u32(slot << ID_BITS).u32(ID_MASK).u32(0);
        w.u16(VENDOR.len() as u16).u16(u16::MAX).u8(1).u8(FORMATS.len() as u8);
        // Images and bitmaps least significant first, scanlines in 32 bits.
        w.u8(0).u8(0).u8(32).u8(32).u8(keymap::MIN_KEYCODE).u8(keymap::MAX_KEYCODE).u32(0);
        w.bytes(VENDOR).align();
        for (depth, bpp) in FORMATS {
            w.u8(depth).u8(bpp).u8(32).zeros(5);
        }
        let mm = |px: u16| ((px as u32 * 254 + 480) / 960) as u16; // 96 dpi
        w.u32(ROOT).u32(COLORMAP).u32(WHITE).u32(BLACK).u32(0);
        w.u16(self.width).u16(self.height).u16(mm(self.width)).u16(mm(self.height));
        // STUB(x11): backing store Never and no save-unders
        w.u16(1).u16(1).u32(VISUAL).u8(0).u8(0).u8(ROOT_DEPTH).u8(DEPTHS.len() as u8);
        for depth in DEPTHS {
            let visuals = u16::from(depth == ROOT_DEPTH);
            w.u8(depth).u8(0).u16(visuals).u32(0);
            if visuals > 0 {
                // TrueColor, 8 bits a channel.
                // STUB(x11): Xvfb also offers DirectColor; old apps want an 8-bit PseudoColor visual
                w.u32(VISUAL).u8(4).u8(8).u16(256).u32(0xff_0000).u32(0xff00).u32(0xff).u32(0);
            }
        }
        let words = ((w.buf.len() - 8) / 4) as u16;
        w.buf[6..8].copy_from_slice(&order.put16(words));
        w.buf
    }

    fn request(&mut self, conn: u64, req: &[u8]) {
        let Some(c) = self.clients.get_mut(&conn) else { return };
        c.seq = c.seq.wrapping_add(1);
        let ctx = Ctx { order: c.order, seq: c.seq, data: req[1], slot: c.slot };
        let major = req[0];
        let minor = if major >= 128 { req[1] as u16 } else { 0 };
        let mut r = Reader::new(&req[4..], ctx.order);
        let answer = self.handle(&ctx, major, &mut r);
        let packet = match answer {
            Ok(Some(reply)) => reply,
            Ok(None) => return,
            Err(XError(code, value)) => {
                if self.verbose {
                    eprintln!("vintrdp: X11: request {major}.{minor}: error {code}");
                }
                wire::error(ctx.order, ctx.seq, code, value, major, minor)
            }
        };
        if let Some(c) = self.clients.get(&conn) {
            let _ = c.out.send(packet);
        }
    }

    fn handle(&mut self, ctx: &Ctx, major: u8, r: &mut Reader) -> Answer {
        match major {
            xproto::GET_WINDOW_ATTRIBUTES_REQUEST => self.get_window_attributes(ctx, r),
            xproto::GET_GEOMETRY_REQUEST => self.get_geometry(ctx, r),
            xproto::QUERY_TREE_REQUEST => {
                window(r.u32()?)?;
                let mut reply = ctx.reply(0);
                reply.u32(ROOT).u32(0).u16(0).zeros(14);
                Ok(Some(reply.finish()))
            }
            xproto::INTERN_ATOM_REQUEST => {
                let n = r.u16()? as usize;
                r.skip(2)?;
                let name = r.bytes(n)?;
                let atom = if ctx.data != 0 { self.atoms.find(name).unwrap_or(0) } else { self.atoms.intern(name) };
                let mut reply = ctx.reply(0);
                reply.u32(atom);
                Ok(Some(reply.finish()))
            }
            xproto::GET_ATOM_NAME_REQUEST => {
                let atom = r.u32()?;
                let name = self.atoms.name(atom).ok_or(XError(BAD_ATOM, atom))?;
                let mut reply = ctx.reply(0);
                reply.u16(name.len() as u16).zeros(22).bytes(name);
                Ok(Some(reply.finish()))
            }
            xproto::CHANGE_PROPERTY_REQUEST => self.change_property(ctx, r),
            xproto::DELETE_PROPERTY_REQUEST => {
                window(r.u32()?)?;
                let name = self.atom(r.u32()?)?;
                self.properties.retain(|p| p.name != name);
                Ok(None)
            }
            xproto::GET_PROPERTY_REQUEST => self.get_property(ctx, r),
            xproto::LIST_PROPERTIES_REQUEST => {
                window(r.u32()?)?;
                let mut reply = ctx.reply(0);
                reply.u16(self.properties.len() as u16).zeros(22);
                for p in &self.properties {
                    reply.u32(p.name);
                }
                Ok(Some(reply.finish()))
            }
            // STUB(x11): no selections: the owner is always None
            xproto::GET_SELECTION_OWNER_REQUEST => {
                self.atom(r.u32()?)?;
                let mut reply = ctx.reply(0);
                reply.u32(0);
                Ok(Some(reply.finish()))
            }
            // STUB(x11): the pointer stays at (0, 0) with no buttons down, until input reaches the server
            xproto::QUERY_POINTER_REQUEST => {
                window(r.u32()?)?;
                let (x, y) = self.pointer;
                let mut reply = ctx.reply(1);
                reply.u32(ROOT).u32(0).i16(x).i16(y).i16(x).i16(y).u16(0);
                Ok(Some(reply.finish()))
            }
            xproto::SET_INPUT_FOCUS_REQUEST => {
                let focus = r.u32()?;
                if focus > 1 {
                    window(focus)?;
                }
                if ctx.data > 2 {
                    return Err(XError(BAD_VALUE, ctx.data as u32));
                }
                self.focus = (focus, ctx.data);
                Ok(None)
            }
            xproto::GET_INPUT_FOCUS_REQUEST => {
                let mut reply = ctx.reply(self.focus.1);
                reply.u32(self.focus.0);
                Ok(Some(reply.finish()))
            }
            // STUB(x11): no keys are ever down
            xproto::QUERY_KEYMAP_REQUEST => {
                let mut reply = ctx.reply(0);
                reply.zeros(32);
                Ok(Some(reply.finish()))
            }
            // STUB(x11): no fonts: ListFonts, ListFontsWithInfo and GetFontPath answer empty
            xproto::LIST_FONTS_REQUEST => {
                let mut reply = ctx.reply(0);
                reply.u16(0).zeros(22);
                Ok(Some(reply.finish()))
            }
            xproto::LIST_FONTS_WITH_INFO_REQUEST => {
                // Only the reply that ends the list.
                let mut reply = ctx.reply(0);
                reply.zeros(52);
                Ok(Some(reply.finish()))
            }
            xproto::GET_FONT_PATH_REQUEST => {
                let mut reply = ctx.reply(0);
                reply.u16(0).zeros(22);
                Ok(Some(reply.finish()))
            }
            xproto::LIST_INSTALLED_COLORMAPS_REQUEST => {
                window(r.u32()?)?;
                let mut reply = ctx.reply(0);
                reply.u16(1).zeros(22).u32(COLORMAP);
                Ok(Some(reply.finish()))
            }
            xproto::QUERY_BEST_SIZE_REQUEST => {
                drawable(r.u32()?)?;
                let (w, h) = (r.u16()?, r.u16()?);
                let (w, h) = match ctx.data {
                    0 => (w.min(64), h.min(64)), // cursors
                    1 | 2 => (w, h),
                    c => return Err(XError(BAD_VALUE, c as u32)),
                };
                let mut reply = ctx.reply(0);
                reply.u16(w).u16(h);
                Ok(Some(reply.finish()))
            }
            xproto::QUERY_EXTENSION_REQUEST => {
                let n = r.u16()? as usize;
                r.skip(2)?;
                let name = r.bytes(n)?;
                let major = EXTENSIONS.iter().find(|e| e.0 == name).map_or(0, |e| e.1);
                let mut reply = ctx.reply(0);
                reply.u8((major != 0) as u8).u8(major).u8(0).u8(0);
                Ok(Some(reply.finish()))
            }
            xproto::LIST_EXTENSIONS_REQUEST => {
                let mut reply = ctx.reply(EXTENSIONS.len() as u8);
                reply.zeros(24);
                for (name, _) in EXTENSIONS {
                    reply.u8(name.len() as u8).bytes(name);
                }
                Ok(Some(reply.finish()))
            }
            xproto::GET_KEYBOARD_MAPPING_REQUEST => {
                let (first, count) = (r.u8()?, r.u8()?);
                if first < keymap::MIN_KEYCODE {
                    return Err(XError(BAD_VALUE, first as u32));
                }
                if first as u16 + count as u16 > keymap::MAX_KEYCODE as u16 + 1 {
                    return Err(XError(BAD_VALUE, count as u32));
                }
                let mut reply = ctx.reply(keymap::KEYSYMS_PER_KEYCODE);
                reply.zeros(24);
                for code in first as u16..first as u16 + count as u16 {
                    for sym in self.keymap.get(code as u8) {
                        reply.u32(sym);
                    }
                }
                Ok(Some(reply.finish()))
            }
            // STUB(x11): keyboard and pointer control are accepted and ignored; the screen saver never runs
            xproto::CHANGE_KEYBOARD_CONTROL_REQUEST | xproto::CHANGE_POINTER_CONTROL_REQUEST | xproto::FORCE_SCREEN_SAVER_REQUEST => Ok(None),
            xproto::GET_KEYBOARD_CONTROL_REQUEST => {
                // Auto-repeat on; bell at 50%, 400 Hz, 100 ms.
                let mut reply = ctx.reply(1);
                reply.u32(0).u8(0).u8(50).u16(400).u16(100).zeros(2).bytes(&[0xff; 32]);
                Ok(Some(reply.finish()))
            }
            xproto::BELL_REQUEST => {
                self.host.bell();
                Ok(None)
            }
            xproto::GET_POINTER_CONTROL_REQUEST => {
                let mut reply = ctx.reply(0);
                reply.u16(2).u16(1).u16(4);
                Ok(Some(reply.finish()))
            }
            xproto::SET_SCREEN_SAVER_REQUEST => {
                let (timeout, interval) = (r.u16()? as i16, r.u16()? as i16);
                let (blanking, exposures) = (r.u8()?, r.u8()?);
                if timeout < -1 || interval < -1 || blanking > 2 || exposures > 2 {
                    return Err(XError(BAD_VALUE, 0));
                }
                let s = &mut self.screen_saver;
                s.0 = if timeout == -1 { 600 } else { timeout as u16 };
                s.1 = if interval == -1 { 600 } else { interval as u16 };
                if blanking < 2 {
                    s.2 = blanking;
                }
                if exposures < 2 {
                    s.3 = exposures;
                }
                Ok(None)
            }
            xproto::GET_SCREEN_SAVER_REQUEST => {
                let (timeout, interval, blanking, exposures) = self.screen_saver;
                let mut reply = ctx.reply(0);
                reply.u16(timeout).u16(interval).u8(blanking).u8(exposures);
                Ok(Some(reply.finish()))
            }
            xproto::LIST_HOSTS_REQUEST => {
                // Access control is on: only the hosts allowed may connect.
                let mut reply = ctx.reply(1);
                reply.u16(0).zeros(22);
                Ok(Some(reply.finish()))
            }
            xproto::GET_POINTER_MAPPING_REQUEST => {
                let mut reply = ctx.reply(5);
                reply.zeros(24).bytes(&[1, 2, 3, 4, 5]);
                Ok(Some(reply.finish()))
            }
            xproto::GET_MODIFIER_MAPPING_REQUEST => {
                let mut reply = ctx.reply(2);
                reply.zeros(24);
                for codes in keymap::MODIFIERS {
                    reply.bytes(&codes);
                }
                Ok(Some(reply.finish()))
            }
            xproto::CREATE_GC_REQUEST => {
                let id = self.new_id(ctx, r.u32()?)?;
                drawable(r.u32()?)?;
                let mut gc = Gc::default();
                gc_values(r, &mut gc)?;
                self.resources.insert(id, Resource::Gc(gc));
                Ok(None)
            }
            xproto::CHANGE_GC_REQUEST => {
                let gc = self.gc(r.u32()?)?;
                gc_values(r, gc)?;
                Ok(None)
            }
            xproto::COPY_GC_REQUEST => {
                let src = self.gc(r.u32()?)?.clone();
                let dst = self.gc(r.u32()?)?;
                let mask = r.u32()?;
                for bit in (0..GC_VALUES).filter(|b| mask >> b & 1 != 0) {
                    dst.values[bit] = src.values[bit];
                }
                Ok(None)
            }
            // STUB(x11): dashes and clip rectangles are checked, not kept
            xproto::SET_DASHES_REQUEST | xproto::SET_CLIP_RECTANGLES_REQUEST => {
                self.gc(r.u32()?)?;
                Ok(None)
            }
            xproto::FREE_GC_REQUEST => {
                let id = r.u32()?;
                self.gc(id)?;
                self.resources.remove(&id);
                Ok(None)
            }
            xproto::NO_OPERATION_REQUEST => Ok(None),
            BIG_REQUESTS => match ctx.data {
                0 => {
                    if let Some(c) = self.clients.values().find(|c| c.slot == ctx.slot) {
                        c.big.store(true, Ordering::Release);
                    }
                    let mut reply = ctx.reply(0);
                    reply.u32(crate::MAX_BIG_REQUEST);
                    Ok(Some(reply.finish()))
                }
                _ => Err(XError(BAD_REQUEST, 0)),
            },
            XC_MISC => self.xc_misc(ctx, r),
            // Opcodes the core protocol leaves free.
            120..=126 | 130.. => Err(XError(BAD_REQUEST, 0)),
            // STUB(x11): core requests not done yet; STUBS.md lists them (and a test checks it)
            _ => Err(XError(BAD_IMPLEMENTATION, 0)),
        }
    }

    /// A new resource's id: in the client's range, and not in use.
    fn new_id(&self, ctx: &Ctx, id: u32) -> Result<u32, XError> {
        if id >> ID_BITS != ctx.slot || self.resources.contains_key(&id) {
            return Err(XError(BAD_ID_CHOICE, id));
        }
        Ok(id)
    }

    fn gc(&mut self, id: u32) -> Result<&mut Gc, XError> {
        match self.resources.get_mut(&id) {
            Some(Resource::Gc(gc)) => Ok(gc),
            _ => Err(XError(BAD_GC, id)),
        }
    }

    fn atom(&self, atom: u32) -> Result<u32, XError> {
        if self.atoms.exists(atom) { Ok(atom) } else { Err(XError(BAD_ATOM, atom)) }
    }

    fn get_window_attributes(&self, ctx: &Ctx, r: &mut Reader) -> Answer {
        window(r.u32()?)?;
        let mut reply = ctx.reply(0); // backing store: NotUseful
        reply.u32(VISUAL).u16(1); // InputOutput
        reply.u8(0).u8(1); // bit gravity Forget, window gravity NorthWest
        reply.u32(!0).u32(0).u8(0); // backing planes and pixel, save-under
        reply.u8(1).u8(2).u8(0); // colormap installed, viewable, not override-redirect
        reply.u32(COLORMAP).u32(0).u32(0).u16(0).zeros(2); // event masks, all and this client's
        Ok(Some(reply.finish()))
    }

    fn get_geometry(&self, ctx: &Ctx, r: &mut Reader) -> Answer {
        drawable(r.u32()?)?;
        let mut reply = ctx.reply(ROOT_DEPTH);
        reply.u32(ROOT).i16(0).i16(0).u16(self.width).u16(self.height).u16(0);
        Ok(Some(reply.finish()))
    }

    // STUB(x11): no PropertyNotify events (here, in DeleteProperty, or GetProperty's delete)
    fn change_property(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let mode = ctx.data;
        window(r.u32()?)?;
        let (name, kind) = (r.u32()?, r.u32()?);
        let format = r.u8()?;
        r.skip(3)?;
        let units = r.u32()?;
        if !matches!(format, 8 | 16 | 32) {
            return Err(XError(BAD_VALUE, format as u32));
        }
        if mode > 2 {
            return Err(XError(BAD_VALUE, mode as u32));
        }
        self.atom(name)?;
        self.atom(kind)?;
        let len = (units as usize).checked_mul(format as usize / 8).ok_or(XError(BAD_LENGTH, 0))?;
        let data = wire::convert(r.bytes(len)?, format, ctx.order);
        match self.properties.iter_mut().find(|p| p.name == name) {
            Some(p) if mode != 0 => {
                if p.kind != kind || p.format != format {
                    return Err(XError(BAD_MATCH, 0));
                }
                if mode == 1 {
                    p.data.splice(0..0, data);
                } else {
                    p.data.extend_from_slice(&data);
                }
            }
            Some(p) => *p = Property { name, kind, format, data },
            None => self.properties.push(Property { name, kind, format, data }),
        }
        Ok(None)
    }

    fn get_property(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        window(r.u32()?)?;
        let name = self.atom(r.u32()?)?;
        let kind = r.u32()?;
        if kind != 0 {
            self.atom(kind)?;
        }
        let (offset, length) = (r.u32()?, r.u32()?);
        let Some(i) = self.properties.iter().position(|p| p.name == name) else {
            let mut reply = ctx.reply(0);
            reply.u32(0).u32(0).u32(0);
            return Ok(Some(reply.finish()));
        };
        let p = &self.properties[i];
        let mut reply = ctx.reply(p.format);
        if kind != 0 && kind != p.kind {
            reply.u32(p.kind).u32(p.data.len() as u32).u32(0);
            return Ok(Some(reply.finish()));
        }
        let n = p.data.len() as u64;
        let start = 4 * offset as u64;
        if start > n {
            return Err(XError(BAD_VALUE, offset));
        }
        let take = (n - start).min(4 * length as u64);
        let after = n - start - take;
        let (start, take) = (start as usize, take as usize);
        let value = wire::convert(&p.data[start..start + take], p.format, ctx.order);
        reply.u32(p.kind).u32(after as u32).u32((take / (p.format as usize / 8)) as u32).zeros(12).bytes(&value);
        if ctx.data != 0 && after == 0 {
            self.properties.remove(i);
        }
        Ok(Some(reply.finish()))
    }

    fn xc_misc(&self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let base = ctx.slot << ID_BITS;
        let mut used: Vec<u32> = self.resources.keys().filter(|id| *id >> ID_BITS == ctx.slot).map(|id| id & ID_MASK).collect();
        used.sort_unstable();
        let mut reply = ctx.reply(0);
        match ctx.data {
            0 => reply.u16(1).u16(1),
            1 => {
                // The longest run of free ids.
                let (mut best, mut from) = ((0, 0), 0);
                for &id in used.iter().chain([ID_MASK + 1].iter()) {
                    if id - from > best.1 {
                        best = (from, id - from);
                    }
                    from = id + 1;
                }
                reply.u32(if best.1 == 0 { 0 } else { base + best.0 }).u32(best.1)
            }
            2 => {
                let count = r.u32()?.min(ID_MASK + 1);
                let free: Vec<u32> = (0..=ID_MASK).filter(|id| used.binary_search(id).is_err()).take(count as usize).collect();
                reply.u32(free.len() as u32).zeros(20);
                for id in free {
                    reply.u32(base + id);
                }
                &mut reply
            }
            _ => return Err(XError(BAD_REQUEST, 0)),
        };
        Ok(Some(reply.finish()))
    }
}

/// A value list: a mask, then a value for each bit set, lowest first.
fn gc_values(r: &mut Reader, gc: &mut Gc) -> Result<(), XError> {
    let mask = r.u32()?;
    if mask >> GC_VALUES != 0 {
        return Err(XError(BAD_VALUE, mask));
    }
    for bit in (0..GC_VALUES).filter(|b| mask >> b & 1 != 0) {
        gc.values[bit] = r.u32()?;
    }
    Ok(())
}

// STUB(x11): the root is the only window and drawable
fn window(id: u32) -> Result<(), XError> {
    if id == ROOT { Ok(()) } else { Err(XError(BAD_WINDOW, id)) }
}

fn drawable(id: u32) -> Result<(), XError> {
    if id == ROOT { Ok(()) } else { Err(XError(BAD_DRAWABLE, id)) }
}

/// vintrdp's version as X.org writes its own: 1.0.16 is 10016000.
fn release() -> u32 {
    let n = |s: &str| s.parse::<u32>().unwrap_or(0);
    n(env!("CARGO_PKG_VERSION_MAJOR")) * 10_000_000 + n(env!("CARGO_PKG_VERSION_MINOR")) * 100_000 + n(env!("CARGO_PKG_VERSION_PATCH")) * 1000
}

fn setup_failed(order: Order, reason: &str) -> Vec<u8> {
    let reason = reason.as_bytes();
    let mut w = Writer::new(order);
    w.u8(0).u8(reason.len() as u8).u16(11).u16(0).u16(((reason.len() + wire::pad(reason.len())) / 4) as u16);
    w.bytes(reason).align();
    w.buf
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nothing;
    impl Host for Nothing {
        fn resize(&self, _: u16, _: u16) {}
        fn draw(&self, _: usize, _: usize, _: usize, _: usize, _: &[u32]) {}
        fn updated(&self) {}
    }

    /// STUBS.md's request list is exactly what answers BadImplementation.
    #[test]
    fn stubs_md_lists_every_unimplemented_request() {
        let settings = Settings {
            width: 640,
            height: 480,
            listen: "127.0.0.1:0".parse().unwrap(),
            allow: Vec::new(),
            keymap: Keymap::new(|_, _| None),
            verbose: false,
        };
        let mut state = State::new(settings, Box::new(Nothing));
        let ctx = Ctx { order: Order::Lsb, seq: 1, data: 0, slot: 1 };
        let body = [0u8; 64];
        let missing: Vec<u8> = (1..=127)
            .filter(|&op| matches!(state.handle(&ctx, op, &mut Reader::new(&body, Order::Lsb)), Err(XError(BAD_IMPLEMENTATION, _))))
            .collect();

        let md = include_str!("../STUBS.md");
        let list = &md[md.find("<!-- requests -->").unwrap()..md.find("<!-- /requests -->").unwrap()];
        let mut listed: Vec<u8> = list.split("` (").skip(1).map(|s| s[..s.find(')').unwrap()].parse().unwrap()).collect();
        listed.sort_unstable();
        assert_eq!(listed, missing, "STUBS.md's request list and the dispatcher differ");
    }
}
