//! X11: vintrdp as an X server, for an address `x11://[bind][:display]`. The
//! [`vintrdp_x11`] server draws into the session's framebuffer (the [`Host`]
//! here); X clients connect to it, as to an X terminal.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use vintrdp_x11::keymap::{self, Keymap};
use vintrdp_x11::{CursorImage as XCursor, Host, InputEvent, Settings};

use crate::graphics::surface::Surface;
use crate::rdp::input::*;
use crate::session::{CursorImage, CursorShape, Event, LoopMsg, Outcome, Shared};

/// rdesktop's exit codes, as the RDP engine uses them.
const EX_PROTOCOL: i32 = 76;
const EXRD_WINDOW_CLOSED: i32 = 62;

pub(crate) fn run(shared: &Arc<Shared>, rx: Receiver<LoopMsg>) -> Outcome {
    let config = &shared.config;
    let listen = match listen_address(&config.server, config.port, !config.x11_allow.is_empty()) {
        Ok(a) => a,
        Err(e) => return ended(EX_PROTOCOL, e),
    };
    let settings = Settings {
        width: config.width.min(u16::MAX as u32) as u16,
        height: config.height.min(u16::MAX as u32) as u16,
        listen,
        allow: config.x11_allow.clone(),
        keymap: Keymap::new(vintrdp_vnc::keys::key_keysyms),
        verbose: config.verbose,
    };
    let server = match vintrdp_x11::start(settings, Box::new(Adapter(shared.clone()))) {
        Ok(s) => s,
        Err(e) => return ended(EX_PROTOCOL, format!("Unable to listen on {listen}: {e}")),
    };
    eprintln!("vintrdp: X server listening on {}", server.local_addr());
    shared.frontend.notify(Event::Title(config.x11_title()));

    let outcome = loop {
        let Ok(msg) = rx.recv() else { break ended(EX_PROTOCOL, "The session ended") };
        if shared.quit_requested() {
            break Outcome { code: EXRD_WINDOW_CLOSED, message: "Window closed".into(), user_quit: true };
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
/// may connect (everywhere when some may).
fn listen_address(bind: &str, port: u16, others: bool) -> Result<SocketAddr, String> {
    let bind = match bind {
        "" if others => "0.0.0.0",
        "" => "127.0.0.1",
        b => b,
    };
    (bind, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .ok_or_else(|| format!("Unable to listen on {bind}: no such address"))
}

fn ended(code: i32, message: impl Into<String>) -> Outcome {
    Outcome { code, message: message.into(), user_quit: false }
}

/// The session as the X server's host.
struct Adapter(Arc<Shared>);

impl Host for Adapter {
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

    fn updated(&self) {
        self.0.frontend.notify(Event::Updated);
    }

    fn bell(&self) {
        self.0.frontend.notify(Event::Bell);
    }

    fn cursor(&self, c: &XCursor) {
        let image = CursorImage { width: c.width, height: c.height, hot_x: c.hot_x, hot_y: c.hot_y, rgba: c.rgba.clone() };
        self.0.screen.lock().unwrap().cursor = CursorShape::Image(Arc::new(image));
        self.0.frontend.notify(Event::Cursor);
    }

    fn clients(&self, count: usize) {
        let title = self.0.config.x11_title();
        let title = match count {
            0 => title,
            1 => title.replacen(" - vintrdp", " (1 client) - vintrdp", 1),
            n => title.replacen(" - vintrdp", &format!(" ({n} clients) - vintrdp"), 1),
        };
        self.0.frontend.notify(Event::Title(title));
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
