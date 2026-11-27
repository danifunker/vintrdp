//! X11: vintrdp as an X server, for an address `x11://[bind][:display]`, or
//! `xdmcp://host` to show host's login screen. The [`vintrdp_x11`] server
//! draws into the session's framebuffer (the [`Host`] here); X clients
//! connect to it, as to an X terminal.

use std::net::{SocketAddr, TcpListener, ToSocketAddrs};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use vintrdp_x11::keymap::{self, Keymap};
use vintrdp_x11::xdmcp::{self, Status};
use vintrdp_x11::{CursorImage as XCursor, Host, InputEvent, Settings};

use crate::config::XdmcpMode;

use crate::graphics::surface::Surface;
use crate::rdp::input::*;
use crate::session::{CursorImage, CursorShape, Event, LoopMsg, Outcome, Shared};

/// rdesktop's exit codes, as the RDP engine uses them.
const EX_PROTOCOL: i32 = 76;
const EXRD_WINDOW_CLOSED: i32 = 62;

pub(crate) fn run(shared: &Arc<Shared>, rx: Receiver<LoopMsg>) -> Outcome {
    let config = &shared.config;
    // The display manager connects from elsewhere: listen everywhere.
    let others = !config.x11_allow.is_empty() || config.xdmcp.is_some();
    let listen = match listen_address(&config.server, config.port, others) {
        Ok(a) => a,
        Err(e) => return ended(EX_PROTOCOL, e),
    };
    let xdmcp = config.xdmcp.as_ref().map(|x| xdmcp::Xdmcp {
        host: x.host.clone(),
        port: x.port,
        mode: match x.mode {
            XdmcpMode::Query => xdmcp::Mode::Query,
            XdmcpMode::Broadcast => xdmcp::Mode::Broadcast,
            XdmcpMode::Indirect => xdmcp::Mode::Indirect,
        },
        address: x.address,
    });
    // Listening for clients: a cookie lets in hosts not on the list.
    let cookie = xdmcp.is_none().then(vintrdp_x11::new_cookie);
    let settings = Settings {
        width: config.width.min(u16::MAX as u32) as u16,
        height: config.height.min(u16::MAX as u32) as u16,
        listen,
        allow: config.x11_allow.clone(),
        keymap: Keymap::new(vintrdp_vnc::keys::key_keysyms),
        xdmcp,
        cookie: cookie.clone(),
        font_path: config.x11_font_path.clone(),
        verbose: config.verbose,
    };
    let failure = Arc::new(Mutex::new(None));
    let adapter = Adapter { shared: shared.clone(), title: Mutex::new(config.x11_title()), failure: failure.clone() };
    let server = match vintrdp_x11::start(settings, Box::new(adapter)) {
        Ok(s) => s,
        Err(e) => return ended(EX_PROTOCOL, format!("Unable to listen on {listen}: {e}")),
    };
    let at = server.local_addr();
    eprintln!("vintrdp: X server listening on {at} (display :{})", at.port().wrapping_sub(crate::X11_PORT));
    if let Some(c) = cookie.filter(|_| !at.ip().is_loopback()) {
        let hex: String = c.iter().map(|b| format!("{b:02x}")).collect();
        eprintln!("vintrdp: clients on hosts not allowed by address may connect with this cookie:");
        eprintln!("vintrdp:   xauth add {}:{} MIT-MAGIC-COOKIE-1 {hex}", config.client_hostname(), at.port().wrapping_sub(crate::X11_PORT));
    }
    shared.frontend.notify(Event::Title(config.x11_title()));

    let outcome = loop {
        let Ok(msg) = rx.recv() else { break ended(EX_PROTOCOL, "The session ended") };
        if shared.quit_requested() {
            break Outcome { code: EXRD_WINDOW_CLOSED, message: "Window closed".into(), user_quit: true };
        }
        if let Some(why) = failure.lock().unwrap().take() {
            break ended(EX_PROTOCOL, why);
        }
        if let LoopMsg::Wake = msg {
            for ev in shared.take_input().into_iter().flat_map(translate) {
                server.input(ev);
            }
        }
    };
    server.stop();
    outcome
}

/// Front-end input as the X server takes it: PC scancodes become keycodes
/// (keymap.rs), the wheel buttons 4 and 5.
fn translate(i: Input) -> Vec<InputEvent> {
    match i.message_type {
        RDP_INPUT_SCANCODE => {
            let down = i.flags & KBD_FLAG_UP == 0;
            match keymap::keycode(i.param1 as u8, i.flags & KBD_FLAG_EXT != 0) {
                Some(code) => vec![InputEvent::Key { code, down }],
                None => Vec::new(),
            }
        }
        RDP_INPUT_MOUSE => {
            let at = InputEvent::Motion { x: i.param1 as i32, y: i.param2 as i32 };
            let button = match i.flags & !MOUSE_FLAG_DOWN {
                MOUSE_FLAG_BUTTON4 => return vec![at, InputEvent::Button { button: 4, down: true }, InputEvent::Button { button: 4, down: false }],
                MOUSE_FLAG_BUTTON5 => return vec![at, InputEvent::Button { button: 5, down: true }, InputEvent::Button { button: 5, down: false }],
                MOUSE_FLAG_BUTTON1 => 1,
                MOUSE_FLAG_BUTTON3 => 2,
                MOUSE_FLAG_BUTTON2 => 3,
                _ => return vec![at],
            };
            vec![at, InputEvent::Button { button, down: i.flags & MOUSE_FLAG_DOWN != 0 }]
        }
        // STUB(x11): typed characters with no key (Session::type_char) and lock-key syncing are dropped (phase 4)
        _ => Vec::new(),
    }
}

/// Where to listen: the address given, or this machine only when no other
/// may connect (everywhere when some may). Port 0: the first free display
/// from :1.
fn listen_address(bind: &str, port: u16, others: bool) -> Result<SocketAddr, String> {
    let bind = match bind {
        "" if others => "0.0.0.0",
        "" => "127.0.0.1",
        b => b,
    };
    let addr = |port: u16| (bind, port).to_socket_addrs().ok().and_then(|mut a| a.next()).ok_or_else(|| format!("Unable to listen on {bind}: no such address"));
    if port != 0 {
        return addr(port);
    }
    for display in 1..64 {
        let a = addr(crate::X11_PORT + display)?;
        if TcpListener::bind(a).is_ok() {
            return Ok(a);
        }
    }
    Err("No free X display from :1 to :63".into())
}

fn ended(code: i32, message: impl Into<String>) -> Outcome {
    Outcome { code, message: message.into(), user_quit: false }
}

/// The session as the X server's host.
struct Adapter {
    shared: Arc<Shared>,
    /// The window's title, but the client count.
    title: Mutex<String>,
    /// Why XDMCP gave up, for the session loop.
    failure: Arc<Mutex<Option<String>>>,
}

impl Adapter {
    fn show_title(&self, count: usize) {
        let title = self.title.lock().unwrap().clone();
        let title = match count {
            0 => title,
            1 => title.replacen(" - vintrdp", " (1 client) - vintrdp", 1),
            n => title.replacen(" - vintrdp", &format!(" ({n} clients) - vintrdp"), 1),
        };
        self.shared.frontend.notify(Event::Title(title));
    }
}

impl Host for Adapter {
    fn resize(&self, width: u16, height: u16) {
        {
            let mut screen = self.shared.screen.lock().unwrap();
            match &mut screen.surface {
                Some(s) => s.resize(width as usize, height as usize),
                None => screen.surface = Some(Surface::new(width as usize, height as usize)),
            }
        }
        self.shared.frontend.notify(Event::Resized { width: width as u32, height: height as u32 });
    }

    fn draw(&self, x: usize, y: usize, w: usize, h: usize, px: &[u32]) {
        let mut screen = self.shared.screen.lock().unwrap();
        let Some(s) = screen.surface.as_mut() else { return };
        if x >= s.width || y >= s.height {
            return;
        }
        let cw = w.min(s.width - x);
        for row in 0..h.min(s.height - y) {
            s.px[(y + row) * s.width + x..][..cw].copy_from_slice(&px[row * w..][..cw]);
        }
    }

    fn updated(&self) {
        self.shared.frontend.notify(Event::Updated);
    }

    fn bell(&self) {
        self.shared.frontend.notify(Event::Bell);
    }

    fn cursor(&self, c: &XCursor) {
        let image = CursorImage { width: c.width, height: c.height, hot_x: c.hot_x, hot_y: c.hot_y, rgba: c.rgba.clone() };
        self.shared.screen.lock().unwrap().cursor = CursorShape::Image(Arc::new(image));
        self.shared.frontend.notify(Event::Cursor);
    }

    fn clients(&self, count: usize) {
        self.show_title(count);
    }

    fn xdmcp(&self, status: &Status) {
        let base = self.shared.config.x11_title();
        match status {
            Status::Progress(s) => {
                eprintln!("vintrdp: {s}");
                *self.title.lock().unwrap() = format!("{s}... - vintrdp");
                self.show_title(0);
            }
            Status::Managed { host } => {
                let name = match self.shared.config.title.as_deref().map(str::trim) {
                    Some(n) if !n.is_empty() => format!("{n} / "),
                    _ => String::new(),
                };
                *self.title.lock().unwrap() = if host.is_empty() { base } else { format!("{name}{host} (XDMCP) - vintrdp") };
                self.show_title(0);
            }
            Status::Failed(why) => {
                *self.failure.lock().unwrap() = Some(why.clone());
                self.shared.wake();
            }
            Status::Dead => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input() {
        let key = |sc: u16, flags: u16| translate(Input { message_type: RDP_INPUT_SCANCODE, flags, param1: sc, param2: 0 });
        assert_eq!(key(0x1e, RDP_KEYPRESS), [InputEvent::Key { code: 38, down: true }]);
        assert_eq!(key(0x48, RDP_KEYRELEASE | KBD_FLAG_EXT), [InputEvent::Key { code: 98, down: false }]);
        let mouse = |flags: u16| translate(Input { message_type: RDP_INPUT_MOUSE, flags, param1: 5, param2: 6 });
        let at = InputEvent::Motion { x: 5, y: 6 };
        assert_eq!(mouse(MOUSE_FLAG_MOVE), [at]);
        assert_eq!(mouse(MOUSE_FLAG_BUTTON2 | MOUSE_FLAG_DOWN), [at, InputEvent::Button { button: 3, down: true }]);
        assert_eq!(mouse(MOUSE_FLAG_BUTTON5)[1..], [InputEvent::Button { button: 5, down: true }, InputEvent::Button { button: 5, down: false }]);
    }

    #[test]
    fn listen_addresses() {
        assert_eq!(listen_address("", 6001, false).unwrap(), "127.0.0.1:6001".parse().unwrap());
        assert_eq!(listen_address("", 6001, true).unwrap(), "0.0.0.0:6001".parse().unwrap());
        assert_eq!(listen_address("::", 6002, false).unwrap(), "[::]:6002".parse().unwrap());
    }
}
