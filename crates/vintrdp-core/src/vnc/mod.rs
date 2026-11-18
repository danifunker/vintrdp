//! VNC (RFB, the Remote Framebuffer protocol): the second engine behind
//! [`Session`](crate::Session), for an address `vnc://host`.
//!
//! RFB 3.3, 3.7 and 3.8 (and Apple's 3.889, spoken as 3.8). A reader thread
//! does the handshake and then decodes updates into the framebuffer the
//! front-ends draw; the session thread turns the front-end's input (PC
//! scancodes, mouse events, clipboard text) into RFB messages.
//!
//! Pure Rust apart from VeNCrypt's TLS (the `tls` feature), so it runs on
//! Windows NT 4.0 too.

mod auth;
mod decode;
mod keys;
mod stream;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use decode::{Canvas, Decoder, Pf, Pseudo};
use keys::Keyboard;
use stream::{Reader, Src, Writer, bad};

use crate::graphics::surface::Surface;
use crate::rdp::input::*;
use crate::session::{Event, LoopMsg, Outcome, Shared};

/// rdesktop's exit codes, as the RDP engine uses them.
const EX_PROTOCOL: i32 = 76;
const EXRD_WINDOW_CLOSED: i32 = 62;

/// Client-to-server message types.
const SET_PIXEL_FORMAT: u8 = 0;
const SET_ENCODINGS: u8 = 2;
const UPDATE_REQUEST: u8 = 3;
const KEY_EVENT: u8 = 4;
const POINTER_EVENT: u8 = 5;
const CLIENT_CUT_TEXT: u8 = 6;
const QEMU: u8 = 255;

/// What the reader thread and the session thread share.
struct Engine {
    writer: Writer,
    /// Logged on; input may go.
    ready: AtomicBool,
    /// The server takes QEMU's scancode key events.
    qemu_keys: AtomicBool,
    /// The framebuffer's size, for update requests.
    size: Mutex<(u16, u16)>,
}

pub(crate) fn run(shared: &Arc<Shared>, rx: Receiver<LoopMsg>) -> Outcome {
    let config = &shared.config;
    let tcp = match stream::connect(&config.server, config.port) {
        Ok(tcp) => Arc::new(tcp),
        Err(e) => return ended(EX_PROTOCOL, format!("Unable to connect to {}: {e}", config.server)),
    };
    let engine = Arc::new(Engine { writer: Writer::new(tcp), ready: AtomicBool::new(false), qemu_keys: AtomicBool::new(false), size: Mutex::new((0, 0)) });
    let spawned = {
        let (shared, engine) = (shared.clone(), engine.clone());
        std::thread::Builder::new().name("vnc-reader".into()).spawn(move || {
            let reader = Reader::new(engine.writer.link());
            let error = match receive(&shared, &engine, reader) {
                Ok(()) => io::Error::new(io::ErrorKind::UnexpectedEof, "the server closed the connection"),
                Err(e) => e,
            };
            let _ = shared.tx.send(LoopMsg::Closed { conn: 0, error });
        })
    };
    if let Err(e) = spawned {
        return ended(EX_PROTOCOL, e.to_string());
    }

    let mut input = InputState::default();
    let outcome = loop {
        let Ok(msg) = rx.recv() else { break ended(EX_PROTOCOL, "The session ended") };
        if shared.quit_requested() {
            break Outcome { code: EXRD_WINDOW_CLOSED, message: "Window closed".into(), user_quit: true };
        }
        match msg {
            LoopMsg::Wake => {
                let events = shared.take_input();
                if engine.ready.load(Ordering::Acquire) {
                    if let Err(e) = send_input(shared, &engine, &mut input, events) {
                        break ended(EX_PROTOCOL, format!("Disconnected: {e}"));
                    }
                }
            }
            LoopMsg::Closed { error, .. } => {
                break if engine.ready.load(Ordering::Acquire) {
                    ended(EX_PROTOCOL, format!("The VNC session ended: {error}"))
                } else {
                    ended(EX_PROTOCOL, format!("Unable to connect to {}: {error}", config.server))
                };
            }
            LoopMsg::Frame { .. } => {}
        }
    };
    engine.writer.shutdown();
    outcome
}

fn ended(code: i32, message: impl Into<String>) -> Outcome {
    Outcome { code, message: message.into(), user_quit: false }
}

// ---------------------------------------------------------------------------
// The reader thread

/// Connect, then decode updates until the connection ends.
fn receive(shared: &Arc<Shared>, engine: &Engine, mut r: Reader) -> io::Result<()> {
    let config = &shared.config;
    let w = &engine.writer;

    let mut version = [0u8; 12];
    r.read_into(&mut version)?;
    let minor = protocol_minor(&version)?;
    w.send(format!("RFB 003.{minor:03}\n").as_bytes())?;
    let logon = auth::logon(shared, w, &mut r, minor)?;

    w.send(&[1])?; // ClientInit: share the desktop with other viewers
    let (width, height) = (r.u16()?, r.u16()?);
    r.skip(16)?; // the server's pixel format; we set ours
    let name = r.string()?;
    if logon.tight {
        // Tight's capability lists: server and client messages, encodings.
        let (s, c, e) = (r.u16()?, r.u16()?, r.u16()?);
        r.skip(2 + (s as usize + c as usize + e as usize) * 16)?;
    }
    if config.verbose {
        eprintln!("vintrdp: VNC desktop {name:?}, {width}x{height}, RFB 3.{minor}");
    }

    let pf = Pf::ours(config.depth);
    let mut m = vec![SET_PIXEL_FORMAT, 0, 0, 0];
    m.extend(pf.to_bytes());
    let encodings = decode::encodings(&pf);
    m.extend([SET_ENCODINGS, 0]);
    m.extend((encodings.len() as u16).to_be_bytes());
    for e in encodings {
        m.extend(e.to_be_bytes());
    }
    w.send(&m)?;

    resize(shared, engine, width, height);
    shared.frontend.notify(Event::Title(config.vnc_title()));
    shared.clipboard_ready();
    engine.ready.store(true, Ordering::Release);
    request(engine, false)?;

    let mut decoder = Decoder::new(pf);
    let mut canvas = Screen(shared);
    loop {
        match r.u8()? {
            0 => {
                update(shared, engine, &mut decoder, &mut canvas, &mut r)?;
                shared.frontend.notify(Event::Updated);
                request(engine, true)?;
            }
            1 => {
                // SetColourMapEntries: for colour-mapped formats, which we don't use.
                r.skip(1)?;
                let (_first, count) = (r.u16()?, r.u16()?);
                r.skip(count as usize * 6)?;
            }
            2 => shared.frontend.notify(Event::Bell),
            3 => {
                r.skip(3)?;
                let len = r.i32()?;
                if len < 0 {
                    // The extended clipboard, which we don't ask for.
                    r.skip(len.unsigned_abs() as usize)?;
                } else {
                    let bytes = r.bytes(len as usize)?;
                    let text: String = bytes.iter().map(|&b| b as char).collect();
                    let text = text.replace("\r\n", "\n");
                    if shared.note_remote_clipboard(&text) {
                        shared.frontend.notify(Event::Clipboard(text));
                    }
                }
            }
            t => return Err(bad(format!("the server sent message type {t}"))),
        }
    }
}

/// The version to speak: 3.8 for 3.8 and later (Apple's 3.889 included), 3.7,
/// else 3.3 (UltraVNC's 3.4 and 3.6 are 3.3 with extensions we don't use).
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

fn update(shared: &Shared, engine: &Engine, decoder: &mut Decoder, canvas: &mut Screen, r: &mut Reader) -> io::Result<()> {
    r.skip(1)?;
    let count = r.u16()?;
    for _ in 0..count {
        let (x, y, w, h) = (r.u16()?, r.u16()?, r.u16()?, r.u16()?);
        let encoding = r.i32()?;
        if shared.config.verbose && decoder.first_use(encoding) {
            eprintln!("vintrdp: VNC encoding {encoding} in use");
        }
        match decoder.rect(r, canvas, x, y, w, h, encoding)? {
            Pseudo::None => {}
            Pseudo::LastRect => break,
            Pseudo::Resize(w, h) => resize(shared, engine, w, h),
            Pseudo::Cursor(shape) => {
                shared.screen.lock().unwrap().cursor = shape;
                shared.frontend.notify(Event::Cursor);
            }
            Pseudo::Name(name) => {
                if shared.config.verbose {
                    eprintln!("vintrdp: VNC desktop now {name:?}");
                }
            }
            Pseudo::QemuKeys => engine.qemu_keys.store(true, Ordering::Release),
        }
    }
    Ok(())
}

fn resize(shared: &Shared, engine: &Engine, width: u16, height: u16) {
    {
        let mut screen = shared.screen.lock().unwrap();
        match &mut screen.surface {
            Some(s) => s.resize(width as usize, height as usize),
            None => screen.surface = Some(Surface::new(width as usize, height as usize)),
        }
    }
    *engine.size.lock().unwrap() = (width, height);
    shared.frontend.notify(Event::Resized { width: width as u32, height: height as u32 });
}

/// FramebufferUpdateRequest for the whole screen.
fn request(engine: &Engine, incremental: bool) -> io::Result<()> {
    let (w, h) = *engine.size.lock().unwrap();
    let mut m = vec![UPDATE_REQUEST, incremental as u8, 0, 0, 0, 0];
    m.extend(w.to_be_bytes());
    m.extend(h.to_be_bytes());
    engine.writer.send(&m)
}

/// The session's framebuffer, locked for each rectangle.
struct Screen<'a>(&'a Shared);

impl Canvas for Screen<'_> {
    fn put(&mut self, x: usize, y: usize, w: usize, h: usize, px: &[u32]) {
        let mut screen = self.0.screen.lock().unwrap();
        let Some(s) = screen.surface.as_mut() else { return };
        if x >= s.width || y >= s.height {
            return;
        }
        let cw = w.min(s.width - x);
        for row in 0..h.min(s.height - y) {
            s.px[(y + row) * s.width + x..][..cw].copy_from_slice(&px[row * w..][..cw]);
        }
    }

    fn copy(&mut self, sx: usize, sy: usize, x: usize, y: usize, w: usize, h: usize) {
        let mut screen = self.0.screen.lock().unwrap();
        let Some(s) = screen.surface.as_mut() else { return };
        if sx >= s.width || sy >= s.height || x >= s.width || y >= s.height {
            return;
        }
        let (w, h) = (w.min(s.width - sx).min(s.width - x), h.min(s.height - sy).min(s.height - y));
        let rows: Vec<Vec<u32>> = (0..h).map(|row| s.px[(sy + row) * s.width + sx..][..w].to_vec()).collect();
        for (row, line) in rows.iter().enumerate() {
            s.px[(y + row) * s.width + x..][..w].copy_from_slice(line);
        }
    }
}

// ---------------------------------------------------------------------------
// Input, on the session thread

#[derive(Default)]
struct InputState {
    keyboard: Keyboard,
    /// RFB's button mask: 1 left, 2 middle, 4 right.
    buttons: u8,
}

fn send_input(shared: &Shared, engine: &Engine, state: &mut InputState, events: Vec<Input>) -> io::Result<()> {
    let mut m = Vec::new();
    for e in events {
        match e.message_type {
            RDP_INPUT_SCANCODE => {
                let (sc, ext, down) = (e.param1 as u8, e.flags & KBD_FLAG_EXT != 0, e.flags & KBD_FLAG_UP == 0);
                if let Some(sym) = state.keyboard.key(sc, ext, down) {
                    key_event(&mut m, engine, sym, Some((sc, ext)), down);
                }
                if !down {
                    state.keyboard.released(sc, ext);
                }
            }
            TYPE_CHAR => {
                let c = char::from_u32(e.param1 as u32 | (e.param2 as u32) << 16).unwrap_or('?');
                let sym = keys::char_keysym(c);
                key_event(&mut m, engine, sym, None, true);
                key_event(&mut m, engine, sym, None, false);
            }
            crate::locks::TYPE_LOCKS => state.keyboard.set_locks(e.param1),
            RDP_INPUT_MOUSE => {
                let (x, y) = (e.param1, e.param2);
                let wheel = match e.flags {
                    MOUSE_FLAG_BUTTON4 => Some(8),
                    MOUSE_FLAG_BUTTON5 => Some(16),
                    _ => None,
                };
                if let Some(bit) = wheel {
                    pointer(&mut m, state.buttons | bit, x, y);
                } else {
                    let bit = match e.flags & (MOUSE_FLAG_BUTTON1 | MOUSE_FLAG_BUTTON2 | MOUSE_FLAG_BUTTON3) {
                        MOUSE_FLAG_BUTTON1 => 1,
                        MOUSE_FLAG_BUTTON3 => 2,
                        MOUSE_FLAG_BUTTON2 => 4,
                        _ => 0,
                    };
                    if e.flags & MOUSE_FLAG_DOWN != 0 {
                        state.buttons |= bit;
                    } else {
                        state.buttons &= !bit;
                    }
                }
                pointer(&mut m, state.buttons, x, y);
            }
            _ => {}
        }
    }
    if shared.take_clipboard_change() {
        // RFB's clipboard is Latin-1.
        let text: Vec<u8> = shared.clipboard_text().chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
        m.extend([CLIENT_CUT_TEXT, 0, 0, 0]);
        m.extend((text.len() as u32).to_be_bytes());
        m.extend(text);
    }
    if m.is_empty() {
        return Ok(());
    }
    if crate::session::trace_input() {
        eprintln!("vintrdp: VNC input {} bytes", m.len());
    }
    engine.writer.send(&m)
}

fn key_event(m: &mut Vec<u8>, engine: &Engine, sym: u32, scancode: Option<(u8, bool)>, down: bool) {
    match scancode {
        Some((sc, ext)) if engine.qemu_keys.load(Ordering::Acquire) => {
            m.extend([QEMU, 0]);
            m.extend((down as u16).to_be_bytes());
            m.extend(sym.to_be_bytes());
            m.extend(keys::qemu_keycode(sc, ext).to_be_bytes());
        }
        _ => {
            m.extend([KEY_EVENT, down as u8, 0, 0]);
            m.extend(sym.to_be_bytes());
        }
    }
}

fn pointer(m: &mut Vec<u8>, buttons: u8, x: u16, y: u16) {
    m.extend([POINTER_EVENT, buttons]);
    m.extend(x.to_be_bytes());
    m.extend(y.to_be_bytes());
}

#[cfg(test)]
mod tests;
