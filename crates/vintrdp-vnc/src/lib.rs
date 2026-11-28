//! vintrdp-vnc: a VNC (RFB) client in pure Rust.
//!
//! - **Versions:** RFB 3.3, 3.7 and 3.8 (Apple's 3.889 and RealVNC's 4.x and
//!   5.x spoken as 3.8).
//! - **Logons:** None; VNC Authentication; Tight; VeNCrypt (TLS from the
//!   [`Host`]); Apple Remote Desktop; RealVNC's RSA-AES (RA2, RA2ne, RA2_256,
//!   RA2ne_256).
//! - **Encodings:** Raw, CopyRect, RRE, CoRRE, Hextile, Zlib, Tight (with
//!   JPEG), TRLE and ZRLE, in 8, 16 or 24-bit colour; the cursor shape,
//!   desktop size, the desktop's name, LastRect, and QEMU's scancode keys;
//!   continuous updates with fences (TigerVNC, TurboVNC).
//! - **Keys:** [`keys`] turns PC scancodes into X keysyms on a US layout.
//!
//! [`client`] splits a connection into a [`Client`], which does the handshake
//! and then reads updates into a [`Host`] on its thread, and a [`Sender`] for
//! input from any other. It builds with Rust 1.90, assumes nothing wider than
//! 32-bit pointers, and needs no C: it runs on Windows NT 4.0.

mod auth;
mod decode;
pub mod keys;
mod rsaaes;
mod stream;

use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use decode::{Canvas, Decoder, Pf, Pseudo};
use stream::{Reader, Src, Writer, bad};

pub use auth::vnc_response;
pub use stream::{Transport, connect};

/// Random bytes from the system (RtlGenRandom on Windows NT 4.0).
pub(crate) fn random(buf: &mut [u8]) {
    getrandom::fill(buf).expect("no source of randomness");
}

/// What the client logs on with, and the colour it asks for.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// For Apple Remote Desktop, VeNCrypt's Plain and RSA-AES user logons.
    pub username: String,
    pub password: String,
    /// Bits per pixel to ask for: 8, 15, 16, 24 or 32.
    pub depth: u32,
    /// Leave other viewers connected (ClientInit's shared flag).
    pub shared: bool,
    /// Log the handshake and the encodings in use to stderr.
    pub verbose: bool,
}

/// The cursor shape the server set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub width: usize,
    pub height: usize,
    pub hot_x: usize,
    pub hot_y: usize,
    /// Unpremultiplied RGBA, top row first.
    pub rgba: Vec<u8>,
}

/// Where the session goes: the framebuffer and the rest. Called on the
/// client's thread.
pub trait Host: Sync {
    /// The framebuffer is now this size (also at the start).
    fn resize(&self, width: u16, height: u16);
    /// Pixels as `0x00RRGGBB`, `w` per row, at (x, y).
    fn draw(&self, x: usize, y: usize, w: usize, h: usize, px: &[u32]);
    /// Copy a rectangle of the framebuffer from (sx, sy) to (x, y).
    fn copy(&self, sx: usize, sy: usize, x: usize, y: usize, w: usize, h: usize);
    /// A whole update has been drawn.
    fn updated(&self) {}
    /// The cursor's shape; None hides it.
    fn cursor(&self, _cursor: Option<Cursor>) {}
    fn bell(&self) {}
    /// The server's clipboard text.
    fn clipboard(&self, _text: String) {}
    fn desktop_name(&self, _name: &str) {}
    /// TLS on the connection for VeNCrypt, checking the server's certificate,
    /// or `anonymous` (no certificate). Without it, VeNCrypt isn't offered.
    fn start_tls(&self, _tcp: TcpStream, _anonymous: bool) -> io::Result<Arc<dyn Transport>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "TLS is not available"))
    }
    fn has_tls(&self) -> bool {
        false
    }
    /// Trust the server's RSA key (RealVNC's logon)? `fingerprint` is as
    /// RealVNC shows it (`xx-xx-xx-xx-xx-xx-xx-xx`); `key` is the whole key.
    fn trust_key(&self, _fingerprint: &str, _key: &[u8]) -> bool {
        false
    }
    /// RealVNC's logon needs a key of the client's as big as the server's,
    /// which takes a while on a slow computer (a minute or more at 2048 bits
    /// on a 1990s processor). A key kept from before (PKCS #1 DER) skips that.
    fn saved_client_key(&self, _bits: u32) -> Option<Vec<u8>> {
        None
    }
    /// A client key was just made, in `took`: keep it for next time if that
    /// was slow. Reusing a key gives up a little of what a fresh one protects
    /// (a stolen key and recorded logons together would reveal the session).
    fn client_key_made(&self, _bits: u32, _der: &[u8], _took: std::time::Duration) {}
    /// What the connection is doing, when it takes a while.
    fn status(&self, _text: &str) {}
}

/// What the server said about its desktop.
#[derive(Clone, Debug)]
pub struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub name: String,
    /// The RFB version spoken: 3, 7 or 8 (for 3.x).
    pub minor: u32,
}

/// Split a connection into its reading and sending sides.
pub fn client(tcp: TcpStream) -> (Client, Sender) {
    let writer = Arc::new(Writer::new(Arc::new(tcp)));
    let shared = Arc::new(State {
        qemu_keys: AtomicBool::new(false),
        size: Mutex::new((0, 0)),
        continuous: AtomicBool::new(false),
        pointer: Arc::new(Mutex::new(None)),
        nudger: Mutex::new(None),
    });
    let reader = Reader::new(writer.link());
    (Client { reader, writer: writer.clone(), state: shared.clone(), decoder: None, verbose: false, stats: Stats::new() }, Sender { writer, state: shared })
}

struct State {
    /// The server takes QEMU's scancode key events.
    qemu_keys: AtomicBool,
    /// The framebuffer's size, for update requests.
    size: Mutex<(u16, u16)>,
    /// The server sends updates continuously, unasked.
    continuous: AtomicBool,
    /// The last PointerEvent sent; held while one is sent.
    pointer: Arc<Mutex<Option<[u8; 6]>>>,
    /// Key presses for the thread that nudges QEMU (see `Sender::send`).
    nudger: Mutex<Option<mpsc::Sender<Instant>>>,
}

/// The reading side: the handshake, then updates.
pub struct Client {
    reader: Reader,
    writer: Arc<Writer>,
    state: Arc<State>,
    decoder: Option<Decoder>,
    verbose: bool,
    stats: Stats,
}

/// What `-v` reports every few seconds: how fast updates come and go.
struct Stats {
    since: Instant,
    received: u64,
    updates: u32,
    pixels: u64,
    /// Reading and drawing updates, from their header to their end.
    busy: Duration,
    /// From an update request to the update's header, summed over requests.
    waited: Duration,
    waits: u32,
    /// The request not yet answered.
    asked: Option<Instant>,
}

impl Stats {
    fn new() -> Stats {
        Stats { since: Instant::now(), received: 0, updates: 0, pixels: 0, busy: Duration::ZERO, waited: Duration::ZERO, waits: 0, asked: None }
    }

    /// Every five seconds, a line on stderr.
    fn report(&mut self, received: u64) {
        let span = self.since.elapsed();
        if span < Duration::from_secs(5) {
            return;
        }
        let secs = span.as_secs_f64();
        let wait = if self.waits > 0 { format!(", {:.0} ms from request to update", self.waited.as_secs_f64() * 1000.0 / self.waits as f64) } else { String::new() };
        eprintln!(
            "vintrdp: VNC {:.1} updates/s, {:.2} Mpixels/s, {:.0} KB/s, busy {:.0}% of the time{wait}",
            self.updates as f64 / secs,
            self.pixels as f64 / secs / 1e6,
            (received - self.received) as f64 / secs / 1024.0,
            self.busy.as_secs_f64() / secs * 100.0,
        );
        *self = Stats { received, asked: self.asked, ..Stats::new() };
    }
}

/// Client-to-server message types.
const SET_PIXEL_FORMAT: u8 = 0;
const SET_ENCODINGS: u8 = 2;
const UPDATE_REQUEST: u8 = 3;
const KEY_EVENT: u8 = 4;
const POINTER_EVENT: u8 = 5;
const CLIENT_CUT_TEXT: u8 = 6;
const QEMU: u8 = 255;
/// Both ways: EnableContinuousUpdates from us, EndOfContinuousUpdates from
/// the server.
const ENABLE_CONTINUOUS_UPDATES: u8 = 150;
const END_OF_CONTINUOUS_UPDATES: u8 = 150;
const FENCE: u8 = 248;
const FENCE_REQUEST: u32 = 1 << 31;

impl Client {
    /// Version, logon, ServerInit; then set our pixel format and encodings
    /// and ask for the whole screen. Calls `host.resize`.
    pub fn handshake(&mut self, settings: &Settings, host: &dyn Host) -> io::Result<ServerInit> {
        self.verbose = settings.verbose;
        let r = &mut self.reader;
        let w = &*self.writer;
        let mut version = [0u8; 12];
        r.read_into(&mut version)?;
        let minor = protocol_minor(&version)?;
        w.send(format!("RFB 003.{minor:03}\n").as_bytes())?;
        let logon = auth::logon(settings, host, w, r, minor)?;

        w.send(&[settings.shared as u8])?;
        let (width, height) = (r.u16()?, r.u16()?);
        r.skip(16)?; // the server's pixel format; we set ours
        let name = r.string()?;
        if logon.tight {
            // Tight's capability lists: server and client messages, encodings.
            let (s, c, e) = (r.u16()?, r.u16()?, r.u16()?);
            r.skip(2 + (s as usize + c as usize + e as usize) * 16)?;
        }
        if settings.verbose {
            eprintln!("vintrdp: VNC desktop {name:?}, {width}x{height}, RFB 3.{minor}");
        }

        let pf = Pf::ours(settings.depth);
        let mut m = vec![SET_PIXEL_FORMAT, 0, 0, 0];
        m.extend(pf.to_bytes());
        let encodings = decode::encodings(&pf);
        m.extend([SET_ENCODINGS, 0]);
        m.extend((encodings.len() as u16).to_be_bytes());
        for e in encodings {
            m.extend(e.to_be_bytes());
        }
        w.send(&m)?;
        self.decoder = Some(Decoder::new(pf));
        self.resize(host, width, height);
        host.desktop_name(&name);
        self.request(false)?;
        Ok(ServerInit { width, height, name, minor })
    }

    /// Read and draw updates until the connection ends (which is an error).
    ///
    /// The next update is asked for as soon as one starts to arrive, so the
    /// server prepares it while this one is read and drawn (QEMU, which
    /// looks for changes on a timer, sends only while a request is waiting).
    /// A server with continuous updates (TigerVNC, TurboVNC) sends them
    /// without being asked, pacing itself with fences, which we answer.
    pub fn run(&mut self, host: &dyn Host) -> io::Result<()> {
        let mut decoder = self.decoder.take().ok_or_else(|| bad("run before handshake"))?;
        loop {
            match self.reader.u8()? {
                0 => {
                    let start = Instant::now();
                    if let Some(asked) = self.stats.asked.take() {
                        self.stats.waited += start - asked;
                        self.stats.waits += 1;
                    }
                    self.reader.skip(1)?;
                    let count = self.reader.u16()?;
                    if !self.state.continuous.load(Ordering::Acquire) {
                        self.request(true)?;
                    }
                    if self.update(host, &mut decoder, count)? {
                        // A new size: the whole screen, at that size.
                        if self.state.continuous.load(Ordering::Acquire) {
                            self.enable_continuous()?;
                        } else {
                            self.request(false)?;
                        }
                    }
                    host.updated();
                    self.stats.updates += 1;
                    self.stats.busy += start.elapsed();
                    if self.verbose {
                        self.stats.report(self.reader.received);
                    }
                }
                1 => {
                    // SetColourMapEntries: for colour-mapped formats, which we don't use.
                    self.reader.skip(1)?;
                    let (_first, count) = (self.reader.u16()?, self.reader.u16()?);
                    self.reader.skip(count as usize * 6)?;
                }
                2 => host.bell(),
                3 => {
                    self.reader.skip(3)?;
                    let len = self.reader.i32()?;
                    if len < 0 {
                        // The extended clipboard, which we don't ask for.
                        self.reader.skip(len.unsigned_abs() as usize)?;
                    } else {
                        let bytes = self.reader.bytes(len as usize)?;
                        let text: String = bytes.iter().map(|&b| b as char).collect();
                        host.clipboard(text.replace("\r\n", "\n"));
                    }
                }
                END_OF_CONTINUOUS_UPDATES => {
                    // The server can send updates continuously (it says so
                    // once it sees we can take them), or has stopped.
                    if self.state.continuous.load(Ordering::Acquire) {
                        self.state.continuous.store(false, Ordering::Release);
                        self.request(true)?;
                    } else {
                        if self.verbose {
                            eprintln!("vintrdp: VNC continuous updates");
                        }
                        self.state.continuous.store(true, Ordering::Release);
                        self.enable_continuous()?;
                    }
                }
                FENCE => {
                    self.reader.skip(3)?;
                    let flags = self.reader.u32()?;
                    let len = self.reader.u8()?;
                    let data = self.reader.bytes(len as usize)?;
                    if flags & FENCE_REQUEST != 0 {
                        // Answered at once, as everything before it is done
                        // (we act on messages in order): no flags, as
                        // TigerVNC's viewer answers.
                        let mut m = vec![FENCE, 0, 0, 0, 0, 0, 0, 0, len];
                        m.extend(data);
                        self.writer.send(&m)?;
                    }
                }
                t => return Err(bad(format!("the server sent message type {t}"))),
            }
        }
    }

    /// The rectangles of an update; true if the desktop's size changed.
    fn update(&mut self, host: &dyn Host, decoder: &mut Decoder, count: u16) -> io::Result<bool> {
        let r = &mut self.reader;
        let mut canvas = HostCanvas(host);
        let mut resized = false;
        for _ in 0..count {
            let (x, y, w, h) = (r.u16()?, r.u16()?, r.u16()?, r.u16()?);
            let encoding = r.i32()?;
            if self.verbose && decoder.first_use(encoding) {
                eprintln!("vintrdp: VNC encoding {encoding} in use");
            }
            match decoder.rect(r, &mut canvas, x, y, w, h, encoding)? {
                Pseudo::None => self.stats.pixels += w as u64 * h as u64,
                Pseudo::LastRect => break,
                Pseudo::Resize(w, h) => {
                    // QEMU repeats the size with every full update: only a
                    // change counts.
                    let old = std::mem::replace(&mut *self.state.size.lock().unwrap(), (w, h));
                    resized |= old != (w, h);
                    host.resize(w, h);
                }
                Pseudo::Cursor(c) => host.cursor(c),
                Pseudo::Name(name) => host.desktop_name(&name),
                Pseudo::QemuKeys => self.state.qemu_keys.store(true, Ordering::Release),
            }
        }
        Ok(resized)
    }

    /// EnableContinuousUpdates for the whole screen.
    fn enable_continuous(&self) -> io::Result<()> {
        let (w, h) = *self.state.size.lock().unwrap();
        let mut m = vec![ENABLE_CONTINUOUS_UPDATES, 1, 0, 0, 0, 0];
        m.extend(w.to_be_bytes());
        m.extend(h.to_be_bytes());
        self.writer.send(&m)
    }

    fn resize(&self, host: &dyn Host, width: u16, height: u16) {
        *self.state.size.lock().unwrap() = (width, height);
        host.resize(width, height);
    }

    /// FramebufferUpdateRequest for the whole screen.
    fn request(&mut self, incremental: bool) -> io::Result<()> {
        self.stats.asked.get_or_insert_with(Instant::now);
        let (w, h) = *self.state.size.lock().unwrap();
        let mut m = vec![UPDATE_REQUEST, incremental as u8, 0, 0, 0, 0];
        m.extend(w.to_be_bytes());
        m.extend(h.to_be_bytes());
        self.writer.send(&m)
    }
}

struct HostCanvas<'a>(&'a dyn Host);

impl Canvas for HostCanvas<'_> {
    fn put(&mut self, x: usize, y: usize, w: usize, h: usize, px: &[u32]) {
        self.0.draw(x, y, w, h, px);
    }
    fn copy(&mut self, sx: usize, sy: usize, x: usize, y: usize, w: usize, h: usize) {
        self.0.copy(sx, sy, x, y, w, h);
    }
}

/// The version to speak: 3.8 for 3.8 and later (Apple's 3.889, RealVNC's 4.x
/// and 5.x), 3.7, else 3.3 (UltraVNC's 3.4 and 3.6 are 3.3 with extensions).
fn protocol_minor(version: &[u8; 12]) -> io::Result<u32> {
    let text = std::str::from_utf8(version).map_err(|_| bad("not a VNC server"))?;
    let rest = text.strip_prefix("RFB ").ok_or_else(|| bad(format!("not a VNC server (it said {text:?})")))?;
    let (major, minor) = rest.trim_end().split_once('.').ok_or_else(|| bad("not a VNC server"))?;
    let (major, minor): (u32, u32) = (major.parse().map_err(|_| bad("not a VNC server"))?, minor.parse().map_err(|_| bad("not a VNC server"))?);
    Ok(match (major, minor) {
        (3, 8..) | (4.., _) => 8,
        (3, 7) => 7,
        (3, _) => 3,
        _ => return Err(bad(format!("RFB {major}.{minor} is older than 3.3"))),
    })
}

/// The sending side: input, from any thread.
#[derive(Clone)]
pub struct Sender {
    writer: Arc<Writer>,
    state: Arc<State>,
}

/// Messages to send together.
#[derive(Default)]
pub struct Batch {
    bytes: Vec<u8>,
    qemu_keys: bool,
    /// It has a key event.
    keys: bool,
    /// Its last PointerEvent.
    pointer: Option<[u8; 6]>,
}

impl Batch {
    /// A key: its X keysym, and its PC scancode (E0-prefixed when `extended`)
    /// for servers with QEMU's extension.
    pub fn key(&mut self, keysym: u32, scancode: Option<(u8, bool)>, down: bool) {
        self.keys = true;
        match scancode {
            Some((sc, ext)) if self.qemu_keys => {
                self.bytes.extend([QEMU, 0]);
                self.bytes.extend((down as u16).to_be_bytes());
                self.bytes.extend(keysym.to_be_bytes());
                self.bytes.extend(keys::qemu_keycode(sc, ext).to_be_bytes());
            }
            _ => {
                self.bytes.extend([KEY_EVENT, down as u8, 0, 0]);
                self.bytes.extend(keysym.to_be_bytes());
            }
        }
    }

    /// The pointer at (x, y) with these buttons down: 1 left, 2 middle, 4
    /// right, 8 and 16 the wheel up and down.
    pub fn pointer(&mut self, buttons: u8, x: u16, y: u16) {
        let [x0, x1] = x.to_be_bytes();
        let [y0, y1] = y.to_be_bytes();
        let m = [POINTER_EVENT, buttons, x0, x1, y0, y1];
        self.bytes.extend(m);
        self.pointer = Some(m);
    }

    /// Clipboard text for the server, in Latin-1 as RFB has it.
    pub fn clipboard(&mut self, text: &str) {
        let text: Vec<u8> = text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
        self.bytes.extend([CLIENT_CUT_TEXT, 0, 0, 0]);
        self.bytes.extend((text.len() as u32).to_be_bytes());
        self.bytes.extend(text);
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl Sender {
    /// An empty batch, knowing what the server takes.
    pub fn batch(&self) -> Batch {
        Batch { qemu_keys: self.state.qemu_keys.load(Ordering::Acquire), ..Batch::default() }
    }

    /// Send a batch. After a key, QEMU's server is nudged: it looks for
    /// changes on a timer, which a key event fires at once (before the guest
    /// has drawn anything) and then backs off to 80 ms, so the echo of a key
    /// shows up 80 ms or more later. Repeating the last pointer event (which
    /// moves nothing) brings the timer back to 30 ms; three of them over the
    /// next 70 ms show a quick guest's response about 50 ms sooner.
    pub fn send(&self, batch: Batch) -> io::Result<()> {
        if batch.bytes.is_empty() {
            return Ok(());
        }
        {
            let mut pointer = self.state.pointer.lock().unwrap();
            if batch.pointer.is_some() {
                *pointer = batch.pointer;
            }
            self.writer.send(&batch.bytes)?;
        }
        // QEMU's key extension and no continuous updates: QEMU, or much like
        // it. VINTRDP_VNC_NUDGE=0 turns this off, for testing.
        static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let off = *OFF.get_or_init(|| std::env::var("VINTRDP_VNC_NUDGE").is_ok_and(|v| v == "0"));
        if batch.keys && batch.qemu_keys && !off && !self.state.continuous.load(Ordering::Acquire) {
            self.nudge();
        }
        Ok(())
    }

    fn nudge(&self) {
        let mut nudger = self.state.nudger.lock().unwrap();
        if nudger.is_none() {
            let (tx, rx) = mpsc::channel();
            let (writer, pointer) = (self.writer.clone(), self.state.pointer.clone());
            if std::thread::Builder::new().name("vnc-nudge".into()).spawn(move || nudge_qemu(rx, &writer, &pointer)).is_ok() {
                *nudger = Some(tx);
            }
        }
        if let Some(tx) = nudger.as_ref() {
            let _ = tx.send(Instant::now());
        }
    }

    /// Close the connection; the client's reads end with an error.
    pub fn shutdown(&self) {
        self.writer.shutdown();
    }
}

/// When to repeat the pointer after a key, for QEMU (see `Sender::send`).
const NUDGES: [Duration; 3] = [Duration::from_millis(10), Duration::from_millis(40), Duration::from_millis(70)];

/// The nudging thread: for each key press (its time from `keys`), the last
/// pointer event again at each of `NUDGES`, starting over at a newer key.
/// It ends with the connection.
fn nudge_qemu(keys: mpsc::Receiver<Instant>, writer: &Writer, pointer: &Mutex<Option<[u8; 6]>>) {
    while let Ok(mut key) = keys.recv() {
        let mut next = 0;
        while next < NUDGES.len() {
            match keys.recv_timeout((key + NUDGES[next]).saturating_duration_since(Instant::now())) {
                Ok(newer) => {
                    (key, next) = (newer, 0);
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let last = pointer.lock().unwrap();
            if let Some(m) = *last
                && writer.send(&m).is_err()
            {
                return;
            }
            next += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct Counter(AtomicUsize);

    impl super::Host for Counter {
        fn resize(&self, _width: u16, _height: u16) {}
        fn draw(&self, _x: usize, _y: usize, _w: usize, _h: usize, _px: &[u32]) {}
        fn copy(&self, _sx: usize, _sy: usize, _x: usize, _y: usize, _w: usize, _h: usize) {}
        fn updated(&self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn read_n(s: &mut TcpStream, n: usize) -> Vec<u8> {
        let mut b = vec![0; n];
        s.read_exact(&mut b).unwrap();
        b
    }

    /// An update's rectangle header.
    fn rect(x: u16, y: u16, w: u16, h: u16, encoding: i32) -> Vec<u8> {
        [x, y, w, h].iter().flat_map(|v| v.to_be_bytes()).chain(encoding.to_be_bytes()).collect()
    }

    /// Updates asked for ahead, sizes, QEMU's nudges, fences and continuous
    /// updates, against a fake server.
    #[test]
    fn update_flow() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            s.write_all(b"RFB 003.008\n").unwrap();
            read_n(&mut s, 12);
            s.write_all(&[1, 1]).unwrap(); // None
            assert_eq!(read_n(&mut s, 1), [1]);
            s.write_all(&0u32.to_be_bytes()).unwrap();
            read_n(&mut s, 1);
            let mut init = vec![0, 4, 0, 2, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0];
            init.extend(1u32.to_be_bytes());
            init.push(b'q');
            s.write_all(&init).unwrap();
            read_n(&mut s, 20);
            let se = read_n(&mut s, 4);
            let encodings: Vec<i32> = read_n(&mut s, 4 * u16::from_be_bytes([se[2], se[3]]) as usize)
                .chunks(4)
                .map(|e| i32::from_be_bytes([e[0], e[1], e[2], e[3]]))
                .collect();
            assert!(encodings.contains(&-312) && encodings.contains(&-313), "{encodings:?}");
            assert_eq!(read_n(&mut s, 10), [3, 0, 0, 0, 0, 0, 0, 4, 0, 2]);

            // The next request comes as soon as an update's header has.
            s.write_all(&[0, 0, 0, 2]).unwrap();
            assert_eq!(read_n(&mut s, 10), [3, 1, 0, 0, 0, 0, 0, 4, 0, 2]);
            let mut m = rect(0, 0, 4, 2, 0);
            m.extend([0; 32]);
            m.extend(rect(0, 0, 0, 0, -258)); // QEMU's keys
            s.write_all(&m).unwrap();

            // A key, after a pointer event: the pointer again three times.
            assert_eq!(read_n(&mut s, 6), [5, 0, 0, 1, 0, 1]);
            assert_eq!(read_n(&mut s, 12)[..4], [255, 0, 0, 1]);
            for _ in 0..3 {
                assert_eq!(read_n(&mut s, 6), [5, 0, 0, 1, 0, 1]);
            }

            // The same size again (QEMU sends it with full updates) is no
            // change; a new one is asked for whole.
            let mut m = vec![0, 0, 0, 1];
            m.extend(rect(0, 0, 4, 2, -223));
            s.write_all(&m).unwrap();
            assert_eq!(read_n(&mut s, 10), [3, 1, 0, 0, 0, 0, 0, 4, 0, 2]);
            let mut m = vec![0, 0, 0, 1];
            m.extend(rect(0, 0, 8, 2, -223));
            s.write_all(&m).unwrap();
            assert_eq!(read_n(&mut s, 10), [3, 1, 0, 0, 0, 0, 0, 4, 0, 2]);
            assert_eq!(read_n(&mut s, 10), [3, 0, 0, 0, 0, 0, 0, 8, 0, 2]);

            // A fence is answered, without flags.
            s.write_all(&[248, 0, 0, 0, 0x80, 0, 0, 3, 2, b'h', b'i']).unwrap();
            assert_eq!(read_n(&mut s, 11), [248, 0, 0, 0, 0, 0, 0, 0, 2, b'h', b'i']);

            // Continuous updates: turned on for the whole screen, then no
            // more requests; a new size turns them on again at that size.
            s.write_all(&[150]).unwrap();
            assert_eq!(read_n(&mut s, 10), [150, 1, 0, 0, 0, 0, 0, 8, 0, 2]);
            let mut m = vec![0, 0, 0, 1];
            m.extend(rect(0, 0, 4, 2, -223));
            s.write_all(&m).unwrap();
            assert_eq!(read_n(&mut s, 10), [150, 1, 0, 0, 0, 0, 0, 4, 0, 2]);
        });

        let (mut client, sender) = super::client(super::connect("127.0.0.1", port).unwrap());
        let host = std::sync::Arc::new(Counter(AtomicUsize::new(0)));
        let settings = super::Settings { depth: 24, ..Default::default() };
        client.handshake(&settings, &*host).unwrap();
        let reader = {
            let host = host.clone();
            std::thread::spawn(move || client.run(&*host))
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while host.0.load(Ordering::Acquire) == 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut b = sender.batch();
        b.pointer(0, 1, 1);
        sender.send(b).unwrap();
        let mut b = sender.batch();
        b.key('a' as u32, Some((0x1e, false)), true);
        sender.send(b).unwrap();
        server.join().unwrap();
        // The server has gone: the reader ends.
        assert!(reader.join().unwrap().is_err());
    }

    #[test]
    fn versions() {
        let v = |s: &[u8; 12]| super::protocol_minor(s).ok();
        assert_eq!(v(b"RFB 003.003\n"), Some(3));
        assert_eq!(v(b"RFB 003.006\n"), Some(3));
        assert_eq!(v(b"RFB 003.007\n"), Some(7));
        assert_eq!(v(b"RFB 003.008\n"), Some(8));
        assert_eq!(v(b"RFB 003.889\n"), Some(8));
        assert_eq!(v(b"RFB 004.001\n"), Some(8));
        assert_eq!(v(b"RFB 005.000\n"), Some(8));
        assert_eq!(v(b"SSH-2.0-xyz\n"), None);
    }
}
