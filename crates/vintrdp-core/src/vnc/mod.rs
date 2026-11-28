//! VNC sessions: the [`vintrdp_vnc`] client behind [`Session`](crate::Session),
//! for an address `vnc://host`.
//!
//! A reader thread does the handshake and draws updates into the session's
//! framebuffer (the [`Host`] here); the session thread turns the front-end's
//! input (PC scancodes, mouse events, clipboard text) into RFB messages.
//! VeNCrypt's TLS is ours (the `tls` feature), and RealVNC's server keys are
//! remembered as SSH's are: asked about once, warned about if they change.

use std::io;
#[cfg(feature = "tls")]
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use vintrdp_vnc::keys::{self, Keyboard};
#[cfg(feature = "tls")]
use vintrdp_vnc::Transport;
use vintrdp_vnc::{Batch, Host, Settings};

use crate::graphics::surface::Surface;
use crate::rdp::input::*;
use crate::session::{CursorImage, CursorShape, Event, LoopMsg, Outcome, Shared};

/// rdesktop's exit codes, as the RDP engine uses them.
const EX_PROTOCOL: i32 = 76;
const EXRD_WINDOW_CLOSED: i32 = 62;

pub(crate) fn run(shared: &Arc<Shared>, rx: Receiver<LoopMsg>) -> Outcome {
    let config = &shared.config;
    let tcp = match vintrdp_vnc::connect(&config.server, config.port) {
        Ok(tcp) => tcp,
        Err(e) => return ended(EX_PROTOCOL, format!("Unable to connect to {}: {e}", config.server)),
    };
    let settings = Settings {
        username: config.username.clone(),
        password: config.password.clone(),
        depth: config.depth,
        shared: true,
        verbose: config.verbose,
    };
    let (mut client, sender) = vintrdp_vnc::client(tcp);
    let ready = Arc::new(AtomicBool::new(false));
    let spawned = {
        let (shared, ready) = (shared.clone(), ready.clone());
        std::thread::Builder::new().name("vnc-reader".into()).spawn(move || {
            let host = Adapter(&shared);
            let result = client.handshake(&settings, &host).and_then(|_| {
                shared.frontend.notify(Event::Title(shared.config.vnc_title()));
                shared.clipboard_ready();
                ready.store(true, Ordering::Release);
                client.run(&host)
            });
            let error = result.err().unwrap_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "the server closed the connection"));
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
                if ready.load(Ordering::Acquire) {
                    let mut batch = sender.batch();
                    input.translate(shared, &mut batch, events);
                    if let Err(e) = sender.send(batch) {
                        break ended(EX_PROTOCOL, format!("Disconnected: {e}"));
                    }
                }
            }
            LoopMsg::Closed { error, .. } => {
                break if ready.load(Ordering::Acquire) {
                    ended(EX_PROTOCOL, format!("The VNC session ended: {error}"))
                } else {
                    ended(EX_PROTOCOL, format!("Unable to connect to {}: {error}", config.server))
                };
            }
            LoopMsg::Frame { .. } => {}
        }
    };
    sender.shutdown();
    outcome
}

fn ended(code: i32, message: impl Into<String>) -> Outcome {
    Outcome { code, message: message.into(), user_quit: false }
}

/// The session as the VNC client's host.
struct Adapter<'a>(&'a Shared);

impl Host for Adapter<'_> {
    fn resize(&self, width: u16, height: u16) {
        {
            let mut screen = self.0.screen.lock().unwrap();
            match &mut screen.surface {
                Some(s) => s.resize(width as usize, height as usize),
                None => screen.surface = Some(Surface::new(width as usize, height as usize)),
            }
        }
        self.0.frontend.notify(Event::Resized { width: width as u32, height: height as u32 });
    }

    fn draw(&self, x: usize, y: usize, w: usize, h: usize, px: &[u32]) {
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

    fn copy(&self, sx: usize, sy: usize, x: usize, y: usize, w: usize, h: usize) {
        let mut screen = self.0.screen.lock().unwrap();
        let Some(s) = screen.surface.as_mut() else { return };
        if sx >= s.width || sy >= s.height || x >= s.width || y >= s.height {
            return;
        }
        let (w, h) = (w.min(s.width - sx).min(s.width - x), h.min(s.height - sy).min(s.height - y));
        // Row by row in place, bottom up when moving down so no row is
        // overwritten before it's copied.
        let width = s.width;
        let mut copy_row = |row: usize| {
            let from = (sy + row) * width + sx;
            s.px.copy_within(from..from + w, (y + row) * width + x);
        };
        if y > sy { (0..h).rev().for_each(&mut copy_row) } else { (0..h).for_each(&mut copy_row) }
    }

    fn updated(&self) {
        self.0.frontend.notify(Event::Updated);
    }

    fn cursor(&self, cursor: Option<vintrdp_vnc::Cursor>) {
        let shape = match cursor {
            Some(c) => CursorShape::Image(Arc::new(CursorImage { width: c.width, height: c.height, hot_x: c.hot_x, hot_y: c.hot_y, rgba: c.rgba })),
            None => CursorShape::Hidden,
        };
        self.0.screen.lock().unwrap().cursor = shape;
        self.0.frontend.notify(Event::Cursor);
    }

    fn bell(&self) {
        self.0.frontend.notify(Event::Bell);
    }

    fn clipboard(&self, text: String) {
        if self.0.note_remote_clipboard(&text) {
            self.0.frontend.notify(Event::Clipboard(text));
        }
    }

    fn has_tls(&self) -> bool {
        cfg!(feature = "tls")
    }

    #[cfg(feature = "tls")]
    fn start_tls(&self, tcp: TcpStream, anonymous: bool) -> io::Result<Arc<dyn Transport>> {
        let server = &self.0.config.server;
        let tls = if anonymous { crate::tls::connect_anonymous(tcp, server, self.0) } else { crate::tls::connect(tcp, server, self.0) };
        tls.map(|t| Arc::new(Tls(t)) as Arc<dyn Transport>).map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))
    }

    fn trust_key(&self, fingerprint: &str, key: &[u8]) -> bool {
        let config = &self.0.config;
        let host = format!("{}:{}", config.server, config.port);
        let id = key_id(key);
        match known_keys().iter().find(|(h, _)| *h == host) {
            Some((_, k)) if *k == id => return true,
            Some(_) => {
                let message = format!(
                    "The VNC server {host} has a different key from the one it had before (its fingerprint is now {fingerprint}).\n\n\
                     That happens when the server was reinstalled, or when someone is pretending to be it. Connect anyway?"
                );
                if !self.0.frontend.ask("vintrdp: the server's key changed", &message, "Connect", "Cancel") {
                    return false;
                }
            }
            None => {
                let message = format!(
                    "This is the first connection to the VNC server {host}. Its key's fingerprint is\n\n{fingerprint}\n\n\
                     RealVNC Server shows it in its window, for checking. Connect, and remember this key?"
                );
                if !self.0.frontend.ask("vintrdp: a new VNC server", &message, "Connect", "Cancel") {
                    return false;
                }
            }
        }
        remember_key(&host, &id);
        true
    }

    fn saved_client_key(&self, bits: u32) -> Option<Vec<u8>> {
        std::fs::read(client_key_path(bits)?).ok()
    }

    fn client_key_made(&self, bits: u32, der: &[u8], took: std::time::Duration) {
        // A fresh key every time where that's quick; kept where it isn't.
        if took < std::time::Duration::from_secs(2) {
            return;
        }
        let Some(path) = client_key_path(bits) else { return };
        if let Some(dir) = path.parent() {
            crate::util::mkdirs(dir);
        }
        let saved = crate::util::write_atomic(&path, der);
        #[cfg(unix)]
        if saved.is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        if let Err(e) = saved {
            eprintln!("vintrdp: could not keep the VNC client key in {}: {e}", path.display());
        }
    }

    fn status(&self, text: &str) {
        let title = self.0.config.vnc_title();
        self.0.frontend.notify(Event::Title(format!("{text}… {title}")));
    }
}

/// The key RealVNC's logon reuses on a slow computer (see `client_key_made`).
fn client_key_path(bits: u32) -> Option<PathBuf> {
    Some(crate::util::data_dir()?.join("vnc").join(format!("client_key_{bits}.der")))
}

#[cfg(feature = "tls")]
struct Tls(Arc<dyn crate::tls::TlsStream>);

#[cfg(feature = "tls")]
impl Transport for Tls {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        self.0.write_all(buf)
    }
    fn shutdown(&self) {
        self.0.shutdown();
    }
}

// ---------------------------------------------------------------------------
// Known server keys: `vnc/known_keys` in the data directory, a line per
// server: `host:port` and the SHA-1 of its key, in hex.

fn keys_path() -> Option<PathBuf> {
    Some(crate::util::data_dir()?.join("vnc").join("known_keys"))
}

fn key_id(key: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(key).iter().map(|b| format!("{b:02x}")).collect()
}

fn known_keys() -> Vec<(String, String)> {
    let text = keys_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    text.lines().filter_map(|l| l.split_once(' ')).map(|(h, k)| (h.to_string(), k.trim().to_string())).collect()
}

fn remember_key(host: &str, id: &str) {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock();
    let Some(path) = keys_path() else { return };
    if let Some(dir) = path.parent() {
        crate::util::mkdirs(dir);
    }
    let mut keys: Vec<(String, String)> = known_keys().into_iter().filter(|(h, _)| h != host).collect();
    keys.push((host.to_string(), id.to_string()));
    let text: String = keys.iter().map(|(h, k)| format!("{h} {k}\n")).collect();
    if let Err(e) = crate::util::write_atomic(&path, text.as_bytes()) {
        eprintln!("vintrdp: could not save the VNC server's key in {}: {e}", path.display());
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

impl InputState {
    fn translate(&mut self, shared: &Shared, m: &mut Batch, events: Vec<Input>) {
        for e in events {
            match e.message_type {
                RDP_INPUT_SCANCODE => {
                    let (sc, ext, down) = (e.param1 as u8, e.flags & KBD_FLAG_EXT != 0, e.flags & KBD_FLAG_UP == 0);
                    if let Some(sym) = self.keyboard.key(sc, ext, down) {
                        m.key(sym, Some((sc, ext)), down);
                    }
                    if !down {
                        self.keyboard.released(sc, ext);
                    }
                }
                TYPE_CHAR => {
                    let c = char::from_u32(e.param1 as u32 | (e.param2 as u32) << 16).unwrap_or('?');
                    let sym = keys::char_keysym(c);
                    m.key(sym, None, true);
                    m.key(sym, None, false);
                }
                crate::locks::TYPE_LOCKS => self.keyboard.set_locks(e.param1),
                RDP_INPUT_MOUSE => {
                    let (x, y) = (e.param1, e.param2);
                    let wheel = match e.flags {
                        MOUSE_FLAG_BUTTON4 => Some(8),
                        MOUSE_FLAG_BUTTON5 => Some(16),
                        _ => None,
                    };
                    if let Some(bit) = wheel {
                        m.pointer(self.buttons | bit, x, y);
                    } else {
                        let bit = match e.flags & (MOUSE_FLAG_BUTTON1 | MOUSE_FLAG_BUTTON2 | MOUSE_FLAG_BUTTON3) {
                            MOUSE_FLAG_BUTTON1 => 1,
                            MOUSE_FLAG_BUTTON3 => 2,
                            MOUSE_FLAG_BUTTON2 => 4,
                            _ => 0,
                        };
                        if e.flags & MOUSE_FLAG_DOWN != 0 {
                            self.buttons |= bit;
                        } else {
                            self.buttons &= !bit;
                        }
                    }
                    m.pointer(self.buttons, x, y);
                }
                _ => {}
            }
        }
        if shared.take_clipboard_change() {
            m.clipboard(&shared.clipboard_text());
        }
        if crate::session::trace_input() && !m.is_empty() {
            eprintln!("vintrdp: VNC input sent");
        }
    }
}

#[cfg(test)]
mod tests;
