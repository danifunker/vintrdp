//! vintrdp-vnc: a VNC (RFB) client in pure Rust.
//!
//! - **Versions:** RFB 3.3, 3.7 and 3.8 (Apple's 3.889 and RealVNC's 4.x and
//!   5.x spoken as 3.8).
//! - **Logons:** None; VNC Authentication; Tight; VeNCrypt (TLS from the
//!   [`Host`]); Apple Remote Desktop; RealVNC's RSA-AES (RA2, RA2ne, RA2_256,
//!   RA2ne_256).
//! - **Encodings:** Raw, CopyRect, RRE, CoRRE, Hextile, Zlib, Tight (with
//!   JPEG), TRLE and ZRLE, in 8, 16 or 24-bit colour; the cursor shape,
//!   desktop size, the desktop's name, LastRect, and QEMU's scancode keys.
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
use std::sync::{Arc, Mutex};

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
    let shared = Arc::new(State { qemu_keys: AtomicBool::new(false), size: Mutex::new((0, 0)) });
    let reader = Reader::new(writer.link());
    (Client { reader, writer: writer.clone(), state: shared.clone(), decoder: None, verbose: false }, Sender { writer, state: shared })
}

struct State {
    /// The server takes QEMU's scancode key events.
    qemu_keys: AtomicBool,
    /// The framebuffer's size, for update requests.
    size: Mutex<(u16, u16)>,
}

/// The reading side: the handshake, then updates.
pub struct Client {
    reader: Reader,
    writer: Arc<Writer>,
    state: Arc<State>,
    decoder: Option<Decoder>,
    verbose: bool,
}

/// Client-to-server message types.
const SET_PIXEL_FORMAT: u8 = 0;
const SET_ENCODINGS: u8 = 2;
const UPDATE_REQUEST: u8 = 3;
const KEY_EVENT: u8 = 4;
const POINTER_EVENT: u8 = 5;
const CLIENT_CUT_TEXT: u8 = 6;
const QEMU: u8 = 255;

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
    pub fn run(&mut self, host: &dyn Host) -> io::Result<()> {
        let mut decoder = self.decoder.take().ok_or_else(|| bad("run before handshake"))?;
        loop {
            match self.reader.u8()? {
                0 => {
                    self.update(host, &mut decoder)?;
                    host.updated();
                    self.request(true)?;
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
                t => return Err(bad(format!("the server sent message type {t}"))),
            }
        }
    }

    fn update(&mut self, host: &dyn Host, decoder: &mut Decoder) -> io::Result<()> {
        let r = &mut self.reader;
        r.skip(1)?;
        let count = r.u16()?;
        let mut canvas = HostCanvas(host);
        for _ in 0..count {
            let (x, y, w, h) = (r.u16()?, r.u16()?, r.u16()?, r.u16()?);
            let encoding = r.i32()?;
            if self.verbose && decoder.first_use(encoding) {
                eprintln!("vintrdp: VNC encoding {encoding} in use");
            }
            match decoder.rect(r, &mut canvas, x, y, w, h, encoding)? {
                Pseudo::None => {}
                Pseudo::LastRect => break,
                Pseudo::Resize(w, h) => {
                    *self.state.size.lock().unwrap() = (w, h);
                    host.resize(w, h);
                }
                Pseudo::Cursor(c) => host.cursor(c),
                Pseudo::Name(name) => host.desktop_name(&name),
                Pseudo::QemuKeys => self.state.qemu_keys.store(true, Ordering::Release),
            }
        }
        Ok(())
    }

    fn resize(&self, host: &dyn Host, width: u16, height: u16) {
        *self.state.size.lock().unwrap() = (width, height);
        host.resize(width, height);
    }

    /// FramebufferUpdateRequest for the whole screen.
    fn request(&self, incremental: bool) -> io::Result<()> {
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
}

impl Batch {
    /// A key: its X keysym, and its PC scancode (E0-prefixed when `extended`)
    /// for servers with QEMU's extension.
    pub fn key(&mut self, keysym: u32, scancode: Option<(u8, bool)>, down: bool) {
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
        self.bytes.extend([POINTER_EVENT, buttons]);
        self.bytes.extend(x.to_be_bytes());
        self.bytes.extend(y.to_be_bytes());
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
        Batch { bytes: Vec::new(), qemu_keys: self.state.qemu_keys.load(Ordering::Acquire) }
    }

    pub fn send(&self, batch: Batch) -> io::Result<()> {
        if batch.bytes.is_empty() { Ok(()) } else { self.writer.send(&batch.bytes) }
    }

    /// Close the connection; the client's reads end with an error.
    pub fn shutdown(&self) {
        self.writer.shutdown();
    }
}

#[cfg(test)]
mod tests {
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
