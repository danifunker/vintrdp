//! The server thread: every client's requests, one at a time, and the input
//! from the front-end between them.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};

use crate::xdmcp;
use std::sync::Arc;
use std::time::{Duration, Instant};

use x11rb_protocol::protocol::xproto;

use crate::atoms::Atoms;
use crate::color::Colormap;
use crate::cursor::Cursor;
use crate::draw::PixmapRef;
use crate::event::{self, Builder, Event};
use crate::font::{Font, Library};
use crate::gc::Gc;
use crate::input::{Input, PassiveGrab};
use crate::keymap::{self, Keymap};
use crate::region::Box as Area;
use crate::window::Window;
use crate::wire::{self, Order, Reader, Reply, Short, Writer};
use crate::{Host, InputEvent, Settings};

pub(crate) enum Msg {
    /// A client sent its connection setup.
    Connect { conn: u64, order: Order, major: u16, out: crate::Out, stream: crate::Stream, local: bool, big: Arc<AtomicBool> },
    /// A whole request, its length field meaningless (BIG-REQUESTS).
    Request { conn: u64, req: Vec<u8> },
    Gone { conn: u64 },
    Input(InputEvent),
    /// The local clipboard's text.
    Clipboard(String),
    /// How asking the display manager goes.
    Xdmcp(crate::xdmcp::Status),
    Stop,
}

// The server's own resources, in client 0's range.
pub const ROOT: u32 = 0x100;
pub const COLORMAP: u32 = 0x20;
pub const VISUAL: u32 = 0x21;
pub const WHITE: u32 = 0xff_ffff;
pub const BLACK: u32 = 0;
/// Each client's resource ids: 21 bits, the client's number above them.
pub const ID_BITS: u32 = 21;
pub const ID_MASK: u32 = (1 << ID_BITS) - 1;
const MAX_CLIENTS: u32 = 255;
pub const ROOT_DEPTH: u8 = 24;
/// Pixmap formats (depth, bits per pixel), and the screen's depths, as Xvfb
/// has them at depth 24, so the two can be compared.
pub const FORMATS: [(u8, u8); 7] = [(1, 1), (4, 8), (8, 8), (15, 16), (16, 16), (24, 32), (32, 32)];
pub const DEPTHS: [u8; 7] = [24, 1, 4, 8, 15, 16, 32];

const VENDOR: &[u8] = b"vintrdp";

// Extensions, at the major opcodes they're given.
const BIG_REQUESTS: u8 = 128;
const XC_MISC: u8 = 129;
pub const SHAPE: u8 = 130;
pub const RENDER: u8 = 131;
// STUB(x11): no Display PostScript (Adobe-DPS-Extension): Solaris's AnswerBook and pageview want it (phase 4)
/// Name, major opcode, first event, first error.
const EXTENSIONS: [(&[u8], u8, u8, u8); 4] = [(b"BIG-REQUESTS", BIG_REQUESTS, 0, 0), (b"RENDER", RENDER, 0, crate::xrender::ERROR_BASE), (b"SHAPE", SHAPE, event::SHAPE_NOTIFY, 0), (b"XC-MISC", XC_MISC, 0, 0)];

// Error codes.
pub const BAD_REQUEST: u8 = 1;
pub const BAD_VALUE: u8 = 2;
pub const BAD_WINDOW: u8 = 3;
pub const BAD_PIXMAP: u8 = 4;
pub const BAD_ATOM: u8 = 5;
pub const BAD_CURSOR: u8 = 6;
pub const BAD_FONT: u8 = 7;
pub const BAD_MATCH: u8 = 8;
pub const BAD_DRAWABLE: u8 = 9;
pub const BAD_ACCESS: u8 = 10;
pub const BAD_ALLOC: u8 = 11;
pub const BAD_COLOR: u8 = 12;
pub const BAD_GC: u8 = 13;
pub const BAD_ID_CHOICE: u8 = 14;
pub const BAD_NAME: u8 = 15;
pub const BAD_LENGTH: u8 = 16;
pub const BAD_IMPLEMENTATION: u8 = 17;

/// An X error: its code and the bad value.
#[derive(Debug)]
pub struct XError(pub u8, pub u32);

impl From<Short> for XError {
    fn from(_: Short) -> XError {
        XError(BAD_LENGTH, 0)
    }
}

pub type Answer = Result<Option<Vec<u8>>, XError>;

pub struct Client {
    pub conn: u64,
    pub order: Order,
    pub seq: u16,
    out: crate::Out,
    stream: crate::Stream,
    /// Connected from this machine: may change the host list (xhost).
    local: bool,
    big: Arc<AtomicBool>,
    /// SetCloseDownMode: 0 Destroy, 1 RetainPermanent, 2 RetainTemporary.
    pub close_down: u8,
    pub save_set: Vec<u32>,
}

impl Client {
    /// Queue a packet for the client; one that has more than
    /// crate::MAX_PENDING bytes unread is disconnected (it goes as any
    /// other does, when its reader sees the connection closed).
    fn queue(&self, slot: u32, packet: Vec<u8>) {
        let first = !self.out.overflowed.load(Ordering::Acquire);
        if !self.out.send(packet) && first {
            eprintln!("vintrdp: X11: client {slot} doesn't read what it's sent: disconnected");
            self.stream.shutdown();
        }
    }
}

/// What a client's resource id names (windows are kept apart).
pub enum Resource {
    Gc(Gc),
    Pixmap(PixmapRef),
    Font(Rc<Font>),
    Cursor(Rc<Cursor>),
    Colormap(Colormap),
    Picture(crate::xrender::PicRef),
    GlyphSet(crate::xrender::GlyphSetRef),
}

pub struct Selection {
    pub atom: u32,
    pub window: u32,
    pub slot: u32,
    pub time: u32,
}

pub struct KeyboardControl {
    pub click: u8,
    pub bell_percent: u8,
    pub bell_pitch: u16,
    pub bell_duration: u16,
    pub leds: u32,
    pub global_repeat: bool,
    pub repeats: [u8; 32],
}

pub struct State {
    pub host: Box<dyn Host>,
    pub width: u16,
    pub height: u16,
    pub keysyms_per: usize,
    /// Keysyms from MIN_KEYCODE, `keysyms_per` each.
    pub keysyms: Vec<u32>,
    /// Eight modifiers, `mod_per` keycodes each.
    pub modmap: Vec<u8>,
    pub mod_per: usize,
    pub pointer_map: [u8; 5],
    pub atoms: Atoms,
    pub clients: HashMap<u32, Client>,
    conns: HashMap<u64, u32>,
    /// Gone clients whose resources stay (SetCloseDownMode), by slot.
    retained: HashMap<u32, u8>,
    pub windows: HashMap<u32, Window>,
    pub resources: HashMap<u32, Resource>,
    /// The screen.
    pub fb: Vec<u32>,
    damaged: Vec<Area>,
    pub selections: Vec<Selection>,
    pub passive: Vec<PassiveGrab>,
    pub input: Input,
    /// The client that grabbed the server, and the requests waiting for it.
    server_grab: Option<u32>,
    deferred: VecDeque<(u64, Vec<u8>)>,
    pub installed: u32,
    pub kbd: KeyboardControl,
    /// Acceleration numerator, denominator, threshold.
    pub pointer_control: (u16, u16, u16),
    /// Timeout, interval, prefer blanking, allow exposures.
    pub screen_saver: (u16, u16, u8, u8),
    pub saver: crate::saver::Saver,
    pub fonts: Library,
    start: Instant,
    pub verbose: bool,
    /// What a reset starts from: the keyboard map, the font folders.
    initial_keysyms: Vec<u32>,
    font_path: Vec<String>,
    /// XDMCP: told when the manager connects and when the server resets,
    /// which it does when the last client leaves.
    xdmcp: Option<Sender<xdmcp::Event>>,
    /// A client connected since the last reset.
    opened: bool,
    /// Who may connect by address (lib.rs), shared with the listener.
    pub hosts: Arc<std::sync::Mutex<crate::Hosts>>,
    /// The visuals, the root's first (visual.rs); how the screen is shown
    /// through the installed colormap.
    pub visuals: Vec<crate::visual::VisualDef>,
    pub root_visual: crate::visual::RootVisual,
    pub lut: crate::visual::Lut,
    /// The root's depth, and its default colormap's black and white.
    pub depth: u8,
    pub white: u32,
    pub black: u32,
    /// Backing store (backing.rs): whether it's offered, the windows'
    /// pixmaps, and those just replaced, for the exposures to copy from.
    pub backing_store: bool,
    pub backings: HashMap<u32, crate::backing::Backing>,
    pub retired: HashMap<u64, crate::backing::Backing>,
    pub next_serial: u64,
    /// Save-unders: whether they're offered, and what each window covers.
    pub save_unders: bool,
    pub saves: HashMap<u32, crate::backing::Save>,
    /// The local clipboard and the selections (clipboard.rs).
    pub clip: crate::clipboard::Clip,
    /// RENDER's formats and state (xrender.rs).
    pub render: crate::xrender::Render,
}

/// What the current request needs to know of its client.
pub struct Ctx {
    pub order: Order,
    pub seq: u16,
    /// The header's second byte.
    pub data: u8,
    pub slot: u32,
    /// The major and minor opcode, for errors and GraphicsExpose.
    pub major: u8,
    pub minor: u16,
}

impl Ctx {
    pub fn reply(&self, data: u8) -> Reply {
        Reply::new(self.order, self.seq, data)
    }
}

/// Show the screen this often at most while requests keep coming.
const FRAME: Duration = Duration::from_millis(15);

pub(crate) fn run(rx: Receiver<Msg>, settings: Settings, host: Box<dyn Host>, xdmcp: Option<Sender<xdmcp::Event>>, hosts: Arc<std::sync::Mutex<crate::Hosts>>) {
    let mut state = State::new(settings, host);
    state.xdmcp = xdmcp;
    state.hosts = hosts;
    state.create_root();
    state.host.resize(state.width, state.height);
    state.update_cursor();
    state.flush();
    let mut last = Instant::now();
    'run: loop {
        state.clip_tick();
        state.saver_tick();
        state.cursor_tick();
        let clip = [state.clip_deadline(), state.saver_deadline(), state.cursor_deadline()].into_iter().flatten().min();
        let idle = state.damaged.is_empty() || state.saver.on;
        let msg = if idle && clip.is_none() {
            match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            }
        } else {
            let frame = if idle { Duration::MAX } else { FRAME.saturating_sub(last.elapsed()) };
            let clip = clip.map_or(Duration::MAX, |at| at.saturating_duration_since(Instant::now()));
            match rx.recv_timeout(frame.min(clip)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    state.flush();
                    last = Instant::now();
                    continue;
                }
                Err(_) => break,
            }
        };
        let mut next = Some(msg);
        while let Some(msg) = next.take() {
            match msg {
                Msg::Connect { conn, order, major, out, stream, local, big } => state.connect(conn, order, major, out, stream, local, big),
                Msg::Request { conn, req } => state.request(conn, req),
                Msg::Gone { conn } => state.gone(conn),
                Msg::Input(ev) => {
                    state.saver_reset();
                    state.device(ev)
                }
                Msg::Clipboard(text) => state.local_clipboard(text),
                Msg::Xdmcp(xdmcp::Status::Dead) => state.reset(),
                Msg::Xdmcp(status) => state.host.xdmcp(&status),
                Msg::Stop => break 'run,
            }
            if last.elapsed() < FRAME {
                next = rx.try_recv().ok();
            }
        }
        state.flush();
        last = Instant::now();
    }
    for c in state.clients.values() {
        c.stream.shutdown();
    }
}

impl State {
    pub fn new(settings: Settings, host: Box<dyn Host>) -> State {
        let n = (keymap::MAX_KEYCODE - keymap::MIN_KEYCODE) as usize + 1;
        let keysyms: Vec<u32> = (0..n).flat_map(|i| settings.keymap.get(keymap::MIN_KEYCODE + i as u8)).collect();
        let _ = &settings.keymap as &Keymap;
        let mut s = State::fresh(host, settings.width, settings.height, keysyms, settings.font_path, settings.verbose);
        s.clip.primary = settings.primary;
        s.set_root_visual(settings.visual);
        s
    }

    fn fresh(host: Box<dyn Host>, width: u16, height: u16, keysyms: Vec<u32>, font_path: Vec<String>, verbose: bool) -> State {
        let settings = (width, height, verbose);
        State {
            host,
            width: settings.0,
            height: settings.1,
            keysyms_per: keymap::KEYSYMS_PER_KEYCODE as usize,
            keysyms: keysyms.clone(),
            modmap: keymap::MODIFIERS.iter().flatten().copied().collect(),
            mod_per: 2,
            pointer_map: [1, 2, 3, 4, 5],
            atoms: Atoms::default(),
            clients: HashMap::new(),
            conns: HashMap::new(),
            retained: HashMap::new(),
            windows: HashMap::new(),
            resources: HashMap::new(),
            fb: vec![0; settings.0 as usize * settings.1 as usize],
            damaged: Vec::new(),
            selections: Vec::new(),
            passive: Vec::new(),
            input: Input::default(),
            server_grab: None,
            deferred: VecDeque::new(),
            installed: COLORMAP,
            kbd: KeyboardControl { click: 0, bell_percent: 50, bell_pitch: 400, bell_duration: 100, leds: 0, global_repeat: true, repeats: [0xff; 32] },
            pointer_control: (2, 1, 4),
            screen_saver: (600, 600, 1, 1),
            saver: Default::default(),
            fonts: Library::new(&font_path),
            start: Instant::now(),
            verbose: settings.2,
            initial_keysyms: keysyms,
            font_path,
            xdmcp: None,
            opened: false,
            hosts: Arc::new(std::sync::Mutex::new(crate::Hosts { enabled: true, list: Vec::new() })),
            visuals: crate::visual::visuals(Default::default()),
            root_visual: Default::default(),
            lut: crate::visual::Lut::Identity,
            depth: ROOT_DEPTH,
            white: WHITE,
            black: BLACK,
            backing_store: true,
            backings: HashMap::new(),
            retired: HashMap::new(),
            next_serial: 0,
            save_unders: true,
            saves: HashMap::new(),
            clip: Default::default(),
            render: Default::default(),
        }
    }

    /// What X.org does when the last client leaves: every client
    /// disconnected, every resource freed, everything set back as it
    /// started (but the time); then XDMCP asks for a new session.
    pub fn reset(&mut self) {
        if self.verbose {
            eprintln!("vintrdp: X11: resetting the server");
        }
        for c in self.clients.values() {
            c.stream.shutdown();
        }
        struct Nothing;
        impl Host for Nothing {
            fn resize(&self, _: u16, _: u16) {}
            fn draw(&self, _: usize, _: usize, _: usize, _: usize, _: &[u32]) {}
            fn updated(&self) {}
        }
        let host = std::mem::replace(&mut self.host, Box::new(Nothing));
        let mut fresh = State::fresh(host, self.width, self.height, self.initial_keysyms.clone(), self.font_path.clone(), self.verbose);
        fresh.start = self.start;
        fresh.xdmcp = self.xdmcp.take();
        fresh.hosts = self.hosts.clone();
        fresh.clip.primary = self.clip.primary;
        fresh.set_root_visual(self.root_visual);
        let text = self.clip.text.take();
        *self = fresh;
        self.create_root();
        if let Some(t) = text {
            self.local_clipboard(t);
        }
        self.update_cursor();
        self.host.clients(0);
        if let Some(x) = &self.xdmcp {
            let _ = x.send(xdmcp::Event::Reset);
        }
    }

    pub fn set_root_visual(&mut self, root: crate::visual::RootVisual) {
        self.root_visual = root;
        self.visuals = crate::visual::visuals(root);
        self.depth = self.visuals[0].depth;
        // An 8-bit X terminal: no Composite, so no backing store in Xvfb;
        // ours works at any depth, and is kept.
    }

    pub fn visual_def(&self, id: u32) -> Option<crate::visual::VisualDef> {
        self.visuals.iter().find(|v| v.id == id).copied()
    }

    /// The server's time, in milliseconds.
    pub fn now(&self) -> u32 {
        (self.start.elapsed().as_millis() as u32).max(1)
    }

    pub fn damage(&mut self, b: Area) {
        if !b.is_empty() {
            self.damaged.push(b);
        }
    }

    /// Show what changed.
    pub fn flush(&mut self) {
        self.composite();
        if self.damaged.is_empty() || self.saver.on {
            // The saver's on: shown when it goes, all of it.
            return;
        }
        let screen = Area::new(0, 0, self.width as i32, self.height as i32);
        let damaged = std::mem::take(&mut self.damaged);
        let region = crate::region::Region::from_boxes(&damaged.iter().map(|b| b.intersect(&screen)).collect::<Vec<_>>());
        let stride = self.width as usize;
        for b in region.boxes() {
            let (w, h) = (b.width() as usize, b.height() as usize);
            let mut px = Vec::with_capacity(w * h);
            for y in b.y1..b.y2 {
                let s = y as usize * stride + b.x1 as usize;
                match &self.lut {
                    crate::visual::Lut::Identity => px.extend_from_slice(&self.fb[s..s + w]),
                    lut => px.extend(self.fb[s..s + w].iter().map(|&p| lut.apply(p))),
                }
            }
            self.host.draw(b.x1 as usize, b.y1 as usize, w, h, &px);
        }
        self.host.updated();
    }

    fn connect(&mut self, conn: u64, order: Order, major: u16, out: crate::Out, stream: crate::Stream, local: bool, big: Arc<AtomicBool>) {
        let free = (1..=MAX_CLIENTS).find(|s| !self.clients.contains_key(s) && !self.retained.contains_key(s));
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
        self.clients.insert(slot, Client { conn, order, seq: 0, out, stream, local, big, close_down: 0, save_set: Vec::new() });
        self.conns.insert(conn, slot);
        self.host.clients(self.clients.len());
        if !self.opened {
            self.opened = true;
            if let Some(x) = &self.xdmcp {
                let _ = x.send(xdmcp::Event::Opened);
            }
        }
    }

    fn gone(&mut self, conn: u64) {
        let Some(slot) = self.conns.remove(&conn) else { return };
        let Some(c) = self.clients.remove(&slot) else { return };
        self.deferred.retain(|d| d.0 != conn);
        if self.server_grab == Some(slot) {
            self.server_grab = None;
        }
        self.client_gone_input(slot);
        self.render.versions.remove(&slot);
        self.colormaps_client_gone(slot);
        self.selections.retain(|s| s.slot != slot);
        // Its save set: windows of others it had reparented go back to the
        // nearest ancestor that isn't its own, and are mapped.
        for w in c.save_set {
            self.rescue(slot, w);
        }
        for w in self.windows.values_mut() {
            w.events.retain(|e| e.0 != slot);
            w.shape_events.retain(|&s| s != slot);
        }
        if c.close_down == 0 {
            self.free_client_resources(slot);
        } else {
            self.retained.insert(slot, c.close_down);
        }
        self.host.clients(self.clients.len());
        // XDMCP: the session's over, or may be.
        if self.xdmcp.is_some() && self.opened {
            if self.clients.is_empty() {
                self.reset();
                return;
            }
            if let Some(x) = &self.xdmcp {
                let _ = x.send(xdmcp::Event::Left);
            }
        }
        self.run_deferred();
    }

    fn rescue(&mut self, slot: u32, w: u32) {
        let Some(win) = self.windows.get(&w) else { return };
        let mut parent = win.parent;
        while parent != ROOT && parent >> ID_BITS == slot {
            parent = self.windows[&parent].parent;
        }
        let ctx = Ctx { order: Order::Lsb, seq: 0, data: 0, slot: 0, major: 0, minor: 0 };
        if parent != win.parent {
            let (x, y) = {
                let p = &self.windows[&parent];
                (win.ax - win.border_width as i32 - p.ax, win.ay - win.border_width as i32 - p.ay)
            };
            let mut req = Vec::new();
            for v in [w, parent] {
                req.extend_from_slice(&v.to_le_bytes());
            }
            req.extend_from_slice(&(x as i16).to_le_bytes());
            req.extend_from_slice(&(y as i16).to_le_bytes());
            let _ = self.reparent_window(&ctx, &mut Reader::new(&req, Order::Lsb));
        }
        let _ = self.map_window(&ctx, w);
    }

    /// Free everything a client made: windows (with their events), then the rest.
    fn free_client_resources(&mut self, slot: u32) {
        let mut tops: Vec<u32> = self.windows.keys().copied().filter(|&w| w >> ID_BITS == slot && w != ROOT).collect();
        tops.sort_unstable();
        for w in tops {
            if self.windows.contains_key(&w) {
                let _ = self.destroy_window(w);
            }
        }
        let ids: Vec<u32> = self.resources.keys().copied().filter(|id| id >> ID_BITS == slot).collect();
        for id in ids {
            self.free_resource(id);
        }
    }

    fn free_resource(&mut self, id: u32) {
        if let Some(Resource::Colormap(_)) = self.resources.get(&id) {
            self.colormap_freed(id);
        }
        self.resources.remove(&id);
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
        w.u32(ROOT).u32(COLORMAP).u32(self.white).u32(self.black).u32(self.windows[&ROOT].all_masks());
        w.u16(self.width).u16(self.height).u16(mm(self.width)).u16(mm(self.height));
        // One colormap installed at a time; backing store WhenMapped (as
        // Xvfb's at depth 24), and save-unders.
        let depths: Vec<u8> = std::iter::once(self.depth).chain(DEPTHS.iter().copied().filter(|&d| d != self.depth)).collect();
        w.u16(1).u16(1).u32(VISUAL).u8(self.backing_store as u8).u8(self.save_unders as u8).u8(self.depth).u8(depths.len() as u8);
        for depth in depths {
            let visuals: Vec<_> = self.visuals.iter().filter(|v| v.depth == depth).collect();
            w.u8(depth).u8(0).u16(visuals.len() as u16).u32(0);
            for v in visuals {
                let v2 = v.v;
                w.u32(v.id).u8(v2.class as u8).u8(v2.bits_per_rgb).u16(v2.entries).u32(v2.red_mask).u32(v2.green_mask).u32(v2.blue_mask).u32(0);
            }
        }
        let words = ((w.buf.len() - 8) / 4) as u16;
        w.buf[6..8].copy_from_slice(&order.put16(words));
        w.buf
    }

    fn request(&mut self, conn: u64, req: Vec<u8>) {
        let Some(&slot) = self.conns.get(&conn) else { return };
        // Another client has the server: wait, in order.
        if self.server_grab.is_some_and(|s| s != slot) || self.deferred.iter().any(|d| d.0 == conn) {
            self.deferred.push_back((conn, req));
            return;
        }
        self.execute(slot, &req);
        self.composite();
        if self.server_grab.is_none() {
            self.run_deferred();
        }
    }

    /// Requests held back by a server grab, now it's over.
    fn run_deferred(&mut self) {
        while self.server_grab.is_none() {
            let Some((conn, req)) = self.deferred.pop_front() else { break };
            if let Some(&slot) = self.conns.get(&conn) {
                self.execute(slot, &req);
                self.composite();
            }
        }
    }

    fn execute(&mut self, slot: u32, req: &[u8]) {
        let Some(c) = self.clients.get_mut(&slot) else { return };
        c.seq = c.seq.wrapping_add(1);
        let major = req[0];
        let minor = if major >= 128 { req[1] as u16 } else { 0 };
        let ctx = Ctx { order: c.order, seq: c.seq, data: req[1], slot, major, minor };
        let mut r = Reader::new(&req[4..], ctx.order);
        let answer = self.handle(&ctx, major, &mut r);
        let packet = match answer {
            Ok(Some(reply)) => reply,
            Ok(None) => return,
            Err(XError(code, value)) => {
                if self.verbose {
                    eprintln!("vintrdp: X11: client {slot}: request {major}.{minor}: error {code}, value {value:#x}");
                }
                wire::error(ctx.order, ctx.seq, code, value, major, minor)
            }
        };
        self.send_bytes(slot, packet);
    }

    pub fn send_bytes(&self, slot: u32, packet: Vec<u8>) {
        if let Some(c) = self.clients.get(&slot) {
            c.queue(slot, packet);
        }
    }

    /// An event to one client.
    pub fn send(&self, slot: u32, ev: &Event) {
        if let Some(c) = self.clients.get(&slot) {
            c.queue(slot, ev.encode(c.order, c.seq).to_vec());
        }
    }

    /// An event to every client that selected one of `mask` on the window.
    /// How many got it.
    pub fn deliver(&self, win: u32, mask: u32, ev: &Event) -> usize {
        let Some(w) = self.windows.get(&win) else { return 0 };
        let mut n = 0;
        for &(slot, m) in &w.events {
            if m & mask != 0 {
                self.send(slot, ev);
                n += 1;
            }
        }
        n
    }

    pub fn handle(&mut self, ctx: &Ctx, major: u8, r: &mut Reader) -> Answer {
        match major {
            xproto::CREATE_WINDOW_REQUEST => self.create_window(ctx, r),
            xproto::CHANGE_WINDOW_ATTRIBUTES_REQUEST => self.change_window_attributes(ctx, r),
            xproto::GET_WINDOW_ATTRIBUTES_REQUEST => self.get_window_attributes(ctx, r),
            xproto::DESTROY_WINDOW_REQUEST => self.destroy_window(r.u32()?).map(|_| None),
            xproto::DESTROY_SUBWINDOWS_REQUEST => self.destroy_subwindows(r.u32()?).map(|_| None),
            xproto::CHANGE_SAVE_SET_REQUEST => self.change_save_set(ctx, r),
            xproto::REPARENT_WINDOW_REQUEST => self.reparent_window(ctx, r),
            xproto::MAP_WINDOW_REQUEST => self.map_window(ctx, r.u32()?).map(|_| None),
            xproto::MAP_SUBWINDOWS_REQUEST => self.map_subwindows(ctx, r.u32()?).map(|_| None),
            xproto::UNMAP_WINDOW_REQUEST => self.unmap_window(r.u32()?).map(|_| None),
            xproto::UNMAP_SUBWINDOWS_REQUEST => self.unmap_subwindows(r.u32()?).map(|_| None),
            xproto::CONFIGURE_WINDOW_REQUEST => self.configure_window(ctx, r),
            xproto::CIRCULATE_WINDOW_REQUEST => self.circulate_window(ctx, r),
            xproto::GET_GEOMETRY_REQUEST => self.get_geometry(ctx, r),
            xproto::QUERY_TREE_REQUEST => self.query_tree(ctx, r),
            xproto::INTERN_ATOM_REQUEST => {
                let n = r.u16()? as usize;
                r.skip(2)?;
                let name = r.bytes(n)?;
                let atom = if ctx.data != 0 { self.atoms.find(name).unwrap_or(0) } else { self.atoms.try_intern(name).ok_or(XError(BAD_ALLOC, 0))? };
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
            xproto::DELETE_PROPERTY_REQUEST => self.delete_property(r),
            xproto::GET_PROPERTY_REQUEST => self.get_property(ctx, r),
            xproto::LIST_PROPERTIES_REQUEST => self.list_properties(ctx, r),
            xproto::SET_SELECTION_OWNER_REQUEST => self.set_selection_owner(ctx, r),
            xproto::GET_SELECTION_OWNER_REQUEST => {
                let atom = self.atom(r.u32()?)?;
                let owner = self.selections.iter().find(|s| s.atom == atom).map_or(0, |s| s.window);
                let mut reply = ctx.reply(0);
                reply.u32(owner);
                Ok(Some(reply.finish()))
            }
            xproto::CONVERT_SELECTION_REQUEST => self.convert_selection(ctx, r),
            xproto::SEND_EVENT_REQUEST => self.send_event(ctx, r),
            xproto::GRAB_POINTER_REQUEST => self.grab_pointer(ctx, r),
            xproto::UNGRAB_POINTER_REQUEST => self.ungrab_pointer(ctx, r),
            xproto::GRAB_BUTTON_REQUEST => self.grab_button(ctx, r),
            xproto::UNGRAB_BUTTON_REQUEST => self.ungrab_button(ctx, r),
            xproto::CHANGE_ACTIVE_POINTER_GRAB_REQUEST => self.change_active_pointer_grab(ctx, r),
            xproto::GRAB_KEYBOARD_REQUEST => self.grab_keyboard(ctx, r),
            xproto::UNGRAB_KEYBOARD_REQUEST => self.ungrab_keyboard(ctx, r),
            xproto::GRAB_KEY_REQUEST => self.grab_key(ctx, r),
            xproto::UNGRAB_KEY_REQUEST => self.ungrab_key(ctx, r),
            xproto::ALLOW_EVENTS_REQUEST => self.allow_events(ctx, r),
            xproto::GRAB_SERVER_REQUEST => {
                self.server_grab = Some(ctx.slot);
                Ok(None)
            }
            xproto::UNGRAB_SERVER_REQUEST => {
                if self.server_grab == Some(ctx.slot) {
                    self.server_grab = None;
                }
                Ok(None)
            }
            xproto::QUERY_POINTER_REQUEST => self.query_pointer(ctx, r),
            xproto::GET_MOTION_EVENTS_REQUEST => {
                // No motion history (the setup says its buffer holds none).
                self.win(r.u32()?)?;
                let mut reply = ctx.reply(0);
                reply.u32(0);
                Ok(Some(reply.finish()))
            }
            xproto::TRANSLATE_COORDINATES_REQUEST => self.translate_coordinates(ctx, r),
            xproto::WARP_POINTER_REQUEST => self.warp_pointer(ctx, r),
            xproto::SET_INPUT_FOCUS_REQUEST => self.set_input_focus(ctx, r),
            xproto::GET_INPUT_FOCUS_REQUEST => {
                let mut reply = ctx.reply(self.input.focus_revert);
                reply.u32(self.input.focus);
                Ok(Some(reply.finish()))
            }
            xproto::QUERY_KEYMAP_REQUEST => {
                let mut reply = ctx.reply(0);
                reply.bytes(&self.input.keys);
                Ok(Some(reply.finish()))
            }
            xproto::OPEN_FONT_REQUEST => self.open_font(ctx, r),
            xproto::CLOSE_FONT_REQUEST => {
                let id = r.u32()?;
                self.font(id)?;
                self.resources.remove(&id);
                Ok(None)
            }
            xproto::QUERY_FONT_REQUEST => self.query_font(ctx, r),
            xproto::QUERY_TEXT_EXTENTS_REQUEST => self.query_text_extents(ctx, r),
            xproto::LIST_FONTS_REQUEST => self.list_fonts(ctx, r),
            xproto::LIST_FONTS_WITH_INFO_REQUEST => self.list_fonts_with_info(ctx, r),
            xproto::SET_FONT_PATH_REQUEST => {
                let n = r.u16()? as usize;
                r.skip(2)?;
                let mut names = Vec::with_capacity(n);
                for _ in 0..n {
                    let len = r.u8()? as usize;
                    names.push(String::from_utf8_lossy(r.bytes(len)?).into_owned());
                }
                if self.verbose {
                    eprintln!("vintrdp: X11: client {}: font path {names:?}", ctx.slot);
                }
                self.fonts.set_path(&names).map_err(|i| XError(BAD_VALUE, i as u32))?;
                Ok(None)
            }
            xproto::GET_FONT_PATH_REQUEST => {
                let path = self.fonts.path();
                let mut reply = ctx.reply(0);
                reply.u16(path.len() as u16).zeros(22);
                for p in path {
                    reply.u8(p.len() as u8).bytes(p.as_bytes());
                }
                Ok(Some(reply.finish()))
            }
            xproto::CREATE_PIXMAP_REQUEST => self.create_pixmap(ctx, r),
            xproto::FREE_PIXMAP_REQUEST => {
                let id = r.u32()?;
                self.pixmap(id)?;
                self.resources.remove(&id);
                Ok(None)
            }
            xproto::CREATE_GC_REQUEST => self.create_gc(ctx, r),
            xproto::CHANGE_GC_REQUEST => self.change_gc(r),
            xproto::COPY_GC_REQUEST => self.copy_gc(r),
            xproto::SET_DASHES_REQUEST => self.set_dashes(r),
            xproto::SET_CLIP_RECTANGLES_REQUEST => self.set_clip_rectangles(ctx, r),
            xproto::FREE_GC_REQUEST => {
                let id = r.u32()?;
                self.gc(id)?;
                self.resources.remove(&id);
                Ok(None)
            }
            xproto::CLEAR_AREA_REQUEST => self.clear_area(ctx, r),
            xproto::COPY_AREA_REQUEST => self.copy_area(ctx, r, false),
            xproto::COPY_PLANE_REQUEST => self.copy_area(ctx, r, true),
            xproto::POLY_POINT_REQUEST
            | xproto::POLY_LINE_REQUEST
            | xproto::POLY_SEGMENT_REQUEST
            | xproto::POLY_RECTANGLE_REQUEST
            | xproto::POLY_ARC_REQUEST
            | xproto::FILL_POLY_REQUEST
            | xproto::POLY_FILL_RECTANGLE_REQUEST
            | xproto::POLY_FILL_ARC_REQUEST => self.poly(ctx, major, r),
            xproto::PUT_IMAGE_REQUEST => self.put_image(ctx, r),
            xproto::GET_IMAGE_REQUEST => self.get_image(ctx, r),
            xproto::POLY_TEXT8_REQUEST | xproto::POLY_TEXT16_REQUEST => self.poly_text(ctx, r, major == xproto::POLY_TEXT16_REQUEST),
            xproto::IMAGE_TEXT8_REQUEST | xproto::IMAGE_TEXT16_REQUEST => self.image_text(ctx, r, major == xproto::IMAGE_TEXT16_REQUEST),
            xproto::CREATE_COLORMAP_REQUEST => self.create_colormap(ctx, r),
            xproto::FREE_COLORMAP_REQUEST => self.free_colormap(r),
            xproto::COPY_COLORMAP_AND_FREE_REQUEST => self.copy_colormap_and_free(ctx, r),
            xproto::INSTALL_COLORMAP_REQUEST => self.install_colormap(r),
            xproto::UNINSTALL_COLORMAP_REQUEST => self.uninstall_colormap(r),
            xproto::LIST_INSTALLED_COLORMAPS_REQUEST => {
                self.win(r.u32()?)?;
                let mut reply = ctx.reply(0);
                reply.u16(1).zeros(22).u32(self.installed);
                Ok(Some(reply.finish()))
            }
            xproto::ALLOC_COLOR_REQUEST => self.alloc_color(ctx, r),
            xproto::ALLOC_NAMED_COLOR_REQUEST => self.alloc_named_color(ctx, r),
            xproto::ALLOC_COLOR_CELLS_REQUEST => self.alloc_color_cells(ctx, r),
            xproto::ALLOC_COLOR_PLANES_REQUEST => self.alloc_color_planes(ctx, r),
            xproto::FREE_COLORS_REQUEST => self.free_colors(ctx, r),
            xproto::STORE_COLORS_REQUEST => self.store_colors(ctx, r),
            xproto::STORE_NAMED_COLOR_REQUEST => self.store_named_color(ctx, r),
            xproto::QUERY_COLORS_REQUEST => self.query_colors(ctx, r),
            xproto::LOOKUP_COLOR_REQUEST => self.lookup_color(ctx, r),
            xproto::CREATE_CURSOR_REQUEST => self.create_cursor(ctx, r),
            xproto::CREATE_GLYPH_CURSOR_REQUEST => self.create_glyph_cursor(ctx, r),
            xproto::FREE_CURSOR_REQUEST => {
                let id = r.u32()?;
                self.cursor_ref(id)?;
                self.resources.remove(&id);
                Ok(None)
            }
            xproto::RECOLOR_CURSOR_REQUEST => self.recolor_cursor(r),
            xproto::QUERY_BEST_SIZE_REQUEST => {
                let d = r.u32()?;
                self.drawable(d)?;
                let (w, h) = (r.u16()?, r.u16()?);
                let (w, h) = match ctx.data {
                    0 => (w.min(64), h.min(64)), // cursors
                    1 | 2 => (w, h),
                    c => return Err(XError(BAD_VALUE, c as u32)),
                };
                if ctx.data != 0 && self.windows.get(&d).is_some_and(|w| w.input_only()) {
                    return Err(XError(BAD_MATCH, d));
                }
                let mut reply = ctx.reply(0);
                reply.u16(w).u16(h);
                Ok(Some(reply.finish()))
            }
            xproto::QUERY_EXTENSION_REQUEST => {
                let n = r.u16()? as usize;
                r.skip(2)?;
                let name = r.bytes(n)?;
                let ext = EXTENSIONS.iter().find(|e| e.0 == name);
                let mut reply = ctx.reply(0);
                reply.u8(ext.is_some() as u8).u8(ext.map_or(0, |e| e.1)).u8(ext.map_or(0, |e| e.2)).u8(ext.map_or(0, |e| e.3));
                Ok(Some(reply.finish()))
            }
            xproto::LIST_EXTENSIONS_REQUEST => {
                let mut reply = ctx.reply(EXTENSIONS.len() as u8);
                reply.zeros(24);
                for (name, ..) in EXTENSIONS {
                    reply.u8(name.len() as u8).bytes(name);
                }
                Ok(Some(reply.finish()))
            }
            xproto::CHANGE_KEYBOARD_MAPPING_REQUEST => self.change_keyboard_mapping(ctx, r),
            xproto::GET_KEYBOARD_MAPPING_REQUEST => {
                let (first, count) = (r.u8()?, r.u8()?);
                if first < keymap::MIN_KEYCODE {
                    return Err(XError(BAD_VALUE, first as u32));
                }
                if first as u16 + count as u16 > keymap::MAX_KEYCODE as u16 + 1 {
                    return Err(XError(BAD_VALUE, count as u32));
                }
                let mut reply = ctx.reply(self.keysyms_per as u8);
                reply.zeros(24);
                let from = (first - keymap::MIN_KEYCODE) as usize * self.keysyms_per;
                for &sym in &self.keysyms[from..from + count as usize * self.keysyms_per] {
                    reply.u32(sym);
                }
                Ok(Some(reply.finish()))
            }
            xproto::CHANGE_KEYBOARD_CONTROL_REQUEST => self.change_keyboard_control(r),
            xproto::GET_KEYBOARD_CONTROL_REQUEST => {
                let k = &self.kbd;
                let mut reply = ctx.reply(k.global_repeat as u8);
                reply.u32(k.leds).u8(k.click).u8(k.bell_percent).u16(k.bell_pitch).u16(k.bell_duration).zeros(2).bytes(&k.repeats);
                Ok(Some(reply.finish()))
            }
            xproto::BELL_REQUEST => {
                let percent = ctx.data as i8;
                if !(-100..=100).contains(&percent) {
                    return Err(XError(BAD_VALUE, percent as u32));
                }
                // The bell's volume, pitch and length are kept (xset q shows them),
                // and knowingly not applied: the front-end beeps its own way.
                self.host.bell();
                Ok(None)
            }
            xproto::CHANGE_POINTER_CONTROL_REQUEST => {
                let (num, den, threshold) = (r.u16()? as i16, r.u16()? as i16, r.u16()? as i16);
                let (do_accel, do_threshold) = (r.u8()?, r.u8()?);
                let p = &mut self.pointer_control;
                if do_accel != 0 {
                    if num < -1 || den < -1 || den == 0 {
                        return Err(XError(BAD_VALUE, den as u32));
                    }
                    p.0 = if num == -1 { 2 } else { num as u16 };
                    p.1 = if den == -1 { 1 } else { den as u16 };
                }
                if do_threshold != 0 {
                    if threshold < -1 {
                        return Err(XError(BAD_VALUE, threshold as u32));
                    }
                    p.2 = if threshold == -1 { 4 } else { threshold as u16 };
                }
                // Acceleration is kept, and knowingly not applied: the front-end's
                // pointer is absolute, already accelerated by the local system.
                Ok(None)
            }
            xproto::GET_POINTER_CONTROL_REQUEST => {
                let mut reply = ctx.reply(0);
                let (n, d, t) = self.pointer_control;
                reply.u16(n).u16(d).u16(t);
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
                // As X.org's SetScreenSaverTimer: counted from now.
                self.saver.since = Instant::now();
                Ok(None)
            }
            xproto::GET_SCREEN_SAVER_REQUEST => {
                let (timeout, interval, blanking, exposures) = self.screen_saver;
                let mut reply = ctx.reply(0);
                reply.u16(timeout).u16(interval).u8(blanking).u8(exposures);
                Ok(Some(reply.finish()))
            }
            xproto::FORCE_SCREEN_SAVER_REQUEST => {
                match ctx.data {
                    0 => self.saver_reset(),
                    1 => self.saver_activate(),
                    d => return Err(XError(BAD_VALUE, d as u32)),
                }
                Ok(None)
            }
            xproto::LIST_HOSTS_REQUEST => {
                let hosts = self.hosts.lock().unwrap();
                let mut reply = ctx.reply(hosts.enabled as u8);
                reply.u16(hosts.list.len() as u16).zeros(22);
                for (family, address) in &hosts.list {
                    reply.u8(*family).u8(0).u16(address.len() as u16).bytes(address);
                    reply.zeros(wire::pad(address.len()));
                }
                Ok(Some(reply.finish()))
            }
            xproto::CHANGE_HOSTS_REQUEST => self.change_hosts(ctx, r),
            xproto::SET_ACCESS_CONTROL_REQUEST => {
                // X.org's order: the mode, then whether the client may.
                if ctx.data > 1 {
                    return Err(XError(BAD_VALUE, ctx.data as u32));
                }
                self.may_change_hosts(ctx)?;
                self.hosts.lock().unwrap().enabled = ctx.data == 1;
                Ok(None)
            }
            xproto::SET_CLOSE_DOWN_MODE_REQUEST => {
                if ctx.data > 2 {
                    return Err(XError(BAD_VALUE, ctx.data as u32));
                }
                self.clients.get_mut(&ctx.slot).unwrap().close_down = ctx.data;
                Ok(None)
            }
            xproto::KILL_CLIENT_REQUEST => self.kill_client(ctx, r.u32()?),
            xproto::ROTATE_PROPERTIES_REQUEST => self.rotate_properties(r),
            xproto::SET_POINTER_MAPPING_REQUEST => self.set_pointer_mapping(ctx, r),
            xproto::GET_POINTER_MAPPING_REQUEST => {
                let mut reply = ctx.reply(5);
                reply.zeros(24).bytes(&self.pointer_map);
                Ok(Some(reply.finish()))
            }
            xproto::SET_MODIFIER_MAPPING_REQUEST => self.set_modifier_mapping(ctx, r),
            xproto::GET_MODIFIER_MAPPING_REQUEST => {
                let mut reply = ctx.reply(self.mod_per as u8);
                reply.zeros(24).bytes(&self.modmap);
                Ok(Some(reply.finish()))
            }
            xproto::NO_OPERATION_REQUEST => Ok(None),
            BIG_REQUESTS => match ctx.data {
                0 => {
                    if let Some(c) = self.clients.get(&ctx.slot) {
                        c.big.store(true, Ordering::Release);
                    }
                    let mut reply = ctx.reply(0);
                    reply.u32(crate::MAX_BIG_REQUEST);
                    Ok(Some(reply.finish()))
                }
                _ => Err(XError(BAD_REQUEST, 0)),
            },
            XC_MISC => self.xc_misc(ctx, r),
            SHAPE => self.shape(ctx, r),
            RENDER => self.render(ctx, r),
            // Opcodes the core protocol leaves free.
            120..=126 | 132.. => Err(XError(BAD_REQUEST, 0)),
            // Every core request is done: a test keeps STUBS.md's (empty) list in step.
            _ => Err(XError(BAD_IMPLEMENTATION, 0)),
        }
    }

    /// A new resource's id: in the client's range, and not in use.
    pub fn new_id(&self, ctx: &Ctx, id: u32) -> Result<u32, XError> {
        if id >> ID_BITS != ctx.slot || self.resources.contains_key(&id) || self.windows.contains_key(&id) {
            return Err(XError(BAD_ID_CHOICE, id));
        }
        Ok(id)
    }

    pub fn atom(&self, atom: u32) -> Result<u32, XError> {
        if self.atoms.exists(atom) { Ok(atom) } else { Err(XError(BAD_ATOM, atom)) }
    }

    /// X.org's AuthorizedClient: only clients on this machine may change
    /// who may connect.
    fn may_change_hosts(&self, ctx: &Ctx) -> Result<(), XError> {
        if self.clients.get(&ctx.slot).is_some_and(|c| c.local) { Ok(()) } else { Err(XError(BAD_ACCESS, 0)) }
    }

    /// ChangeHosts, as X.org's AddHost and RemoveHost: Internet and
    /// Internet6 addresses (xhost resolves names itself).
    fn change_hosts(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let family = r.u8()?;
        r.skip(1)?;
        let n = r.u16()? as usize;
        let address = r.bytes(n)?.to_vec();
        if ctx.data > 1 {
            return Err(XError(BAD_VALUE, ctx.data as u32));
        }
        self.may_change_hosts(ctx)?;
        let len = match family {
            crate::FAMILY_INTERNET => 4,
            crate::FAMILY_INTERNET6 => 16,
            _ => return Err(XError(BAD_VALUE, family as u32)),
        };
        if address.len() != len {
            return Err(XError(BAD_VALUE, address.len() as u32));
        }
        let mut hosts = self.hosts.lock().unwrap();
        let entry = (family, address);
        if ctx.data == 0 {
            if !hosts.list.contains(&entry) {
                hosts.list.push(entry);
            }
        } else {
            hosts.list.retain(|h| *h != entry);
        }
        Ok(None)
    }

    fn kill_client(&mut self, ctx: &Ctx, id: u32) -> Answer {
        if id == 0 {
            // AllTemporary: what RetainTemporary clients left.
            let temp: Vec<u32> = self.retained.iter().filter(|r| *r.1 == 2).map(|r| *r.0).collect();
            for slot in temp {
                self.retained.remove(&slot);
                self.free_client_resources(slot);
            }
            return Ok(None);
        }
        let slot = id >> ID_BITS;
        let known = self.windows.contains_key(&id) || self.resources.contains_key(&id);
        if slot == 0 || !known {
            return Err(XError(BAD_VALUE, id));
        }
        if let Some(c) = self.clients.get(&slot) {
            c.stream.shutdown();
            if slot == ctx.slot {
                // Itself: gone now, its request's errors and all.
                let conn = c.conn;
                self.gone(conn);
            }
        } else if self.retained.remove(&slot).is_some() {
            self.free_client_resources(slot);
        }
        Ok(None)
    }

    fn get_geometry(&self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        let (depth, x, y, w, h, bw) = match self.drawable(id)? {
            crate::gc::Drawable::Window(w) => {
                let win = &self.windows[&w];
                (win.depth, win.x, win.y, win.width, win.height, win.border_width)
            }
            crate::gc::Drawable::Pixmap(p) => {
                let p = p.borrow();
                (p.depth, 0, 0, p.width, p.height, 0)
            }
        };
        let mut reply = ctx.reply(depth);
        reply.u32(ROOT).i16(x).i16(y).u16(w).u16(h).u16(bw);
        Ok(Some(reply.finish()))
    }

    fn change_keyboard_mapping(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let count = ctx.data as usize;
        let (first, per) = (r.u8()?, r.u8()? as usize);
        r.skip(2)?;
        if first < keymap::MIN_KEYCODE {
            return Err(XError(BAD_VALUE, first as u32));
        }
        if first as usize + count > keymap::MAX_KEYCODE as usize + 1 {
            return Err(XError(BAD_VALUE, count as u32));
        }
        if per == 0 {
            return Err(XError(BAD_VALUE, 0));
        }
        let mut syms = Vec::with_capacity(count * per);
        for _ in 0..count * per {
            syms.push(r.u32()?);
        }
        // Widen the map if this wants more keysyms a key.
        if per > self.keysyms_per {
            let old = self.keysyms_per;
            self.keysyms = self.keysyms.chunks(old).flat_map(|c| c.iter().copied().chain(std::iter::repeat_n(0, per - old))).collect();
            self.keysyms_per = per;
        }
        let w = self.keysyms_per;
        for (i, key) in syms.chunks(per).enumerate() {
            let at = (first - keymap::MIN_KEYCODE) as usize * w + i * w;
            self.keysyms[at..at + w].fill(0);
            self.keysyms[at..at + per].copy_from_slice(key);
        }
        self.mapping_notify(1, first, count as u8);
        Ok(None)
    }

    pub(crate) fn mapping_notify(&self, request: u8, first: u8, count: u8) {
        let ev = Builder::new(event::MAPPING_NOTIFY, 0).u8(request).u8(first).u8(count).done();
        for &slot in self.clients.keys() {
            self.send(slot, &ev);
        }
    }

    fn set_pointer_mapping(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let map = r.bytes(ctx.data as usize)?;
        if map.len() != self.pointer_map.len() {
            return Err(XError(BAD_VALUE, map.len() as u32));
        }
        for (i, &b) in map.iter().enumerate() {
            if b != 0 && map[..i].contains(&b) {
                return Err(XError(BAD_VALUE, b as u32));
            }
        }
        let mut reply = ctx.reply(0);
        // Busy if a button that changes is down.
        let busy = (0..5).any(|i| map[i] != self.pointer_map[i] && self.input.buttons & (1 << i) != 0);
        if busy {
            let mut reply = ctx.reply(1);
            reply.zeros(0);
            return Ok(Some(reply.finish()));
        }
        self.pointer_map.copy_from_slice(map);
        self.mapping_notify(2, 0, 0);
        reply.zeros(0);
        Ok(Some(reply.finish()))
    }

    fn set_modifier_mapping(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let per = ctx.data as usize;
        let codes = r.bytes(8 * per)?.to_vec();
        if let Some(&bad) = codes.iter().find(|&&c| c != 0 && c < keymap::MIN_KEYCODE) {
            return Err(XError(BAD_VALUE, bad as u32));
        }
        let down = |c: u8| c != 0 && self.input.keys[c as usize / 8] >> (c % 8) & 1 != 0;
        let changed = |m: usize| {
            let old: Vec<u8> = self.modmap[m * self.mod_per..(m + 1) * self.mod_per].iter().copied().filter(|&c| c != 0).collect();
            let new: Vec<u8> = codes[m * per..(m + 1) * per].iter().copied().filter(|&c| c != 0).collect();
            (old != new).then_some((old, new))
        };
        let busy = (0..8).filter_map(changed).any(|(o, n)| o.iter().chain(&n).any(|&c| down(c)));
        if busy {
            return Ok(Some(ctx.reply(1).finish()));
        }
        self.modmap = codes;
        self.mod_per = per;
        self.mapping_notify(0, 0, 0);
        Ok(Some(ctx.reply(0).finish()))
    }

    fn change_keyboard_control(&mut self, r: &mut Reader) -> Answer {
        let mask = r.u32()?;
        if mask >> 8 != 0 {
            return Err(XError(BAD_VALUE, mask));
        }
        let mut v = [None::<u32>; 8];
        for (bit, slot) in v.iter_mut().enumerate() {
            if mask >> bit & 1 != 0 {
                *slot = Some(r.u32()?);
            }
        }
        let pct = |x: u32| {
            let x = x as i32;
            if x == -1 || (0..=100).contains(&x) { Ok(x) } else { Err(XError(BAD_VALUE, x as u32)) }
        };
        let mut k = std::mem::replace(&mut self.kbd, KeyboardControl { click: 0, bell_percent: 0, bell_pitch: 0, bell_duration: 0, leds: 0, global_repeat: true, repeats: [0; 32] });
        let result = (|| {
            if let Some(x) = v[0] {
                let x = pct(x)?;
                k.click = if x == -1 { 0 } else { x as u8 };
            }
            if let Some(x) = v[1] {
                let x = pct(x)?;
                k.bell_percent = if x == -1 { 50 } else { x as u8 };
            }
            if let Some(x) = v[2] {
                let x = x as i32;
                if x < -1 {
                    return Err(XError(BAD_VALUE, x as u32));
                }
                k.bell_pitch = if x == -1 { 400 } else { x as u16 };
            }
            if let Some(x) = v[3] {
                let x = x as i32;
                if x < -1 {
                    return Err(XError(BAD_VALUE, x as u32));
                }
                k.bell_duration = if x == -1 { 100 } else { x as u16 };
            }
            // LED and LED mode; key and auto-repeat mode.
            if v[4].is_some() && v[5].is_none() || v[6].is_some() && v[7].is_none() {
                return Err(XError(BAD_MATCH, 0));
            }
            if let Some(mode) = v[5] {
                if mode > 1 {
                    return Err(XError(BAD_VALUE, mode));
                }
                let bits = match v[4] {
                    Some(led) if (1..=32).contains(&led) => 1u32 << (led - 1),
                    Some(led) => return Err(XError(BAD_VALUE, led)),
                    None => !0,
                };
                if mode == 1 { k.leds |= bits } else { k.leds &= !bits }
            }
            if let Some(mode) = v[7] {
                if mode > 2 {
                    return Err(XError(BAD_VALUE, mode));
                }
                match v[6] {
                    Some(key) => {
                        if !(keymap::MIN_KEYCODE as u32..=keymap::MAX_KEYCODE as u32).contains(&key) {
                            return Err(XError(BAD_VALUE, key));
                        }
                        let (i, b) = (key as usize / 8, key % 8);
                        match mode {
                            0 => k.repeats[i] &= !(1 << b),
                            1 => k.repeats[i] |= 1 << b,
                            _ => k.repeats[i] = k.repeats[i] & !(1 << b) | 1 << b,
                        }
                    }
                    None => k.global_repeat = mode != 0,
                }
            }
            Ok(())
        })();
        self.kbd = k;
        result.map(|_| None)
    }

    fn xc_misc(&self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let base = ctx.slot << ID_BITS;
        let mut used: Vec<u32> = self.resources.keys().chain(self.windows.keys()).filter(|id| *id >> ID_BITS == ctx.slot).map(|id| id & ID_MASK).collect();
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

/// vintrdp's version as X.org writes its own: 1.0.16 is 10016000.
fn release() -> u32 {
    let n = |s: &str| s.parse::<u32>().unwrap_or(0);
    n(env!("CARGO_PKG_VERSION_MAJOR")) * 10_000_000 + n(env!("CARGO_PKG_VERSION_MINOR")) * 100_000 + n(env!("CARGO_PKG_VERSION_PATCH")) * 1000
}

pub(crate) fn setup_failed(order: Order, reason: &str) -> Vec<u8> {
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

    fn test_state() -> State {
        let settings = Settings {
            width: 640,
            height: 480,
            listen: "127.0.0.1:0".parse().unwrap(),
            unix_socket: false,
            allow: Vec::new(),
            keymap: Keymap::new(|_, _| None),
            xdmcp: None,
            cookie: None,
            font_path: Vec::new(),
            primary: false,
            visual: Default::default(),
            verbose: false,
        };
        let mut state = State::new(settings, Box::new(Nothing));
        state.create_root();
        state
    }

    /// A client in slot `slot`, from this machine or not.
    fn add_client(state: &mut State, slot: u32, local: bool) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (out, rx) = crate::Out::for_test();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = crate::Stream::Tcp(std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        state.clients.insert(slot, Client { conn: slot as u64, order: Order::Lsb, seq: 1, out, stream, local, big: Arc::default(), close_down: 0, save_set: Vec::new() });
        rx
    }

    fn call(state: &mut State, slot: u32, op: u8, data: u8, body: &[u8]) -> Answer {
        let ctx = Ctx { order: Order::Lsb, seq: 1, data, slot, major: op, minor: 0 };
        state.handle(&ctx, op, &mut Reader::new(body, Order::Lsb))
    }

    fn code(a: Answer) -> Option<(u8, u32)> {
        a.err().map(|XError(c, v)| (c, v))
    }

    /// ListHosts, ChangeHosts and SetAccessControl, with X.org's errors.
    #[test]
    fn access_control() {
        let mut state = test_state();
        *state.hosts.lock().unwrap() = crate::Hosts::new(&["192.168.99.153".to_string()]).unwrap();
        let _a = add_client(&mut state, 1, true);
        let _b = add_client(&mut state, 2, false);
        let ls = |state: &mut State| {
            let r = call(state, 1, xproto::LIST_HOSTS_REQUEST, 0, &[]).unwrap().unwrap();
            let n = u16::from_le_bytes([r[8], r[9]]) as usize;
            let (mut hosts, mut at) = (Vec::new(), 32);
            for _ in 0..n {
                let len = u16::from_le_bytes([r[at + 2], r[at + 3]]) as usize;
                hosts.push((r[at], r[at + 4..at + 4 + len].to_vec()));
                at += 4 + len + wire::pad(len);
            }
            assert_eq!(r.len(), at);
            (r[1], hosts)
        };
        assert_eq!(ls(&mut state), (1, vec![(0, vec![192, 168, 99, 153])]));
        let host = |family: u8, addr: &[u8]| {
            let mut b = vec![family, 0];
            b.extend_from_slice(&(addr.len() as u16).to_le_bytes());
            b.extend_from_slice(addr);
            b.resize(b.len() + wire::pad(addr.len()), 0);
            b
        };
        // Insert, from this machine; twice is once.
        for _ in 0..2 {
            assert!(call(&mut state, 1, xproto::CHANGE_HOSTS_REQUEST, 0, &host(0, &[10, 0, 0, 7])).is_ok());
        }
        let v6 = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        assert!(call(&mut state, 1, xproto::CHANGE_HOSTS_REQUEST, 0, &host(6, &v6)).is_ok());
        assert_eq!(ls(&mut state).1, vec![(0, vec![192, 168, 99, 153]), (0, vec![10, 0, 0, 7]), (6, v6.to_vec())]);
        assert!(state.hosts.lock().unwrap().allows("10.0.0.7".parse().unwrap()));
        assert!(!state.hosts.lock().unwrap().allows("10.0.0.8".parse().unwrap()));
        // Delete.
        assert!(call(&mut state, 1, xproto::CHANGE_HOSTS_REQUEST, 1, &host(0, &[192, 168, 99, 153])).is_ok());
        assert_eq!(ls(&mut state).1.len(), 2);
        // X.org's errors: the mode first, then who asks, then the family and length.
        assert_eq!(code(call(&mut state, 2, xproto::CHANGE_HOSTS_REQUEST, 2, &host(0, &[1, 2, 3, 4]))), Some((BAD_VALUE, 2)));
        assert_eq!(code(call(&mut state, 2, xproto::CHANGE_HOSTS_REQUEST, 0, &host(0, &[1, 2, 3, 4]))), Some((BAD_ACCESS, 0)));
        assert_eq!(code(call(&mut state, 1, xproto::CHANGE_HOSTS_REQUEST, 0, &host(1, &[1, 2]))), Some((BAD_VALUE, 1)));
        assert_eq!(code(call(&mut state, 1, xproto::CHANGE_HOSTS_REQUEST, 0, &host(0, &[1, 2, 3]))), Some((BAD_VALUE, 3)));
        assert_eq!(code(call(&mut state, 1, xproto::CHANGE_HOSTS_REQUEST, 0, &[0, 0, 8, 0, 1, 2])), Some((BAD_LENGTH, 0)));
        // SetAccessControl: off lets anyone in, and ListHosts says so.
        assert_eq!(code(call(&mut state, 1, xproto::SET_ACCESS_CONTROL_REQUEST, 2, &[])), Some((BAD_VALUE, 2)));
        assert_eq!(code(call(&mut state, 2, xproto::SET_ACCESS_CONTROL_REQUEST, 0, &[])), Some((BAD_ACCESS, 0)));
        assert!(call(&mut state, 1, xproto::SET_ACCESS_CONTROL_REQUEST, 0, &[]).is_ok());
        assert_eq!(ls(&mut state).0, 0);
        assert!(state.hosts.lock().unwrap().allows("10.0.0.8".parse().unwrap()));
        assert!(call(&mut state, 1, xproto::SET_ACCESS_CONTROL_REQUEST, 1, &[]).is_ok());
        assert_eq!(ls(&mut state).0, 1);
        // allow=* starts with it off.
        assert!(!crate::Hosts::new(&["*".to_string()]).unwrap().enabled);
    }

    /// A client that reads nothing is cut off once MAX_PENDING bytes wait.
    #[test]
    fn unread_replies_are_bounded() {
        let mut state = test_state();
        let _rx = add_client(&mut state, 1, true);
        let big = vec![0u8; 1 << 20];
        for _ in 0..crate::MAX_PENDING >> 20 {
            state.send_bytes(1, big.clone());
        }
        assert!(!state.clients[&1].out.overflowed.load(Ordering::Acquire));
        state.send_bytes(1, big.clone());
        assert!(state.clients[&1].out.overflowed.load(Ordering::Acquire));
    }

    /// InternAtom past the limits is BadAlloc.
    #[test]
    fn atoms_run_out() {
        let mut state = test_state();
        let _rx = add_client(&mut state, 1, true);
        let intern = |state: &mut State, name: &[u8]| {
            let mut b = (name.len() as u16).to_le_bytes().to_vec();
            b.extend_from_slice(&[0, 0]);
            b.extend_from_slice(name);
            b.resize(b.len() + wire::pad(name.len()), 0);
            call(state, 1, xproto::INTERN_ATOM_REQUEST, 0, &b)
        };
        let big = vec![b'x'; 65535];
        let mut n = 0;
        let err = loop {
            let mut name = big.clone();
            name[..8].copy_from_slice(&(n as u64).to_le_bytes());
            match intern(&mut state, &name) {
                Ok(_) => n += 1,
                Err(e) => break e,
            }
        };
        // The predefined names take a few hundred bytes of the limit.
        assert_eq!((err.0, n), (BAD_ALLOC, (crate::atoms::MAX_NAME_BYTES - 1000) / 65535));
        // Known names still answer.
        assert!(intern(&mut state, b"PRIMARY").is_ok());
    }

    /// STUBS.md's request list is exactly what answers BadImplementation.
    #[test]
    fn stubs_md_lists_every_unimplemented_request() {
        let settings = Settings {
            width: 640,
            height: 480,
            listen: "127.0.0.1:0".parse().unwrap(),
            unix_socket: false,
            allow: Vec::new(),
            keymap: Keymap::new(|_, _| None),
            xdmcp: None,
            cookie: None,
            font_path: Vec::new(),
            primary: false,
            visual: Default::default(),
            verbose: false,
        };
        let mut state = State::new(settings, Box::new(Nothing));
        state.create_root();
        let (out, _rx) = crate::Out::for_test();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = crate::Stream::Tcp(std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        state.clients.insert(1, Client { conn: 1, order: Order::Lsb, seq: 1, out, stream, local: true, big: Arc::default(), close_down: 0, save_set: Vec::new() });
        let body = [0u8; 64];
        let missing: Vec<u8> = (1..=127)
            .filter(|&op| {
                let ctx = Ctx { order: Order::Lsb, seq: 1, data: 0, slot: 1, major: op, minor: 0 };
                matches!(state.handle(&ctx, op, &mut Reader::new(&body, Order::Lsb)), Err(XError(BAD_IMPLEMENTATION, _)))
            })
            .collect();

        let md = include_str!("../STUBS.md");
        let list = &md[md.find("<!-- requests -->").unwrap()..md.find("<!-- /requests -->").unwrap()];
        let mut listed: Vec<u8> = list.split("` (").skip(1).map(|s| s[..s.find(')').unwrap()].parse().unwrap()).collect();
        listed.sort_unstable();
        assert_eq!(listed, missing, "STUBS.md's request list and the dispatcher differ");
    }
}
