//! The session: runs the protocol on a thread of its own and talks to a
//! native front-end through the [`Frontend`] trait.
//!
//! Front-ends queue input on a [`Session`] and read the framebuffer; the core
//! reports back through [`Frontend::notify`]. Every session is independent,
//! so a process can run several.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::graphics::surface::Surface;
use crate::rdp::input::*;

/// What the core tells the front-end. Called on the rdesktop thread: post it
/// to your UI thread and return quickly.
#[derive(Clone, Debug)]
pub enum Event {
    /// The framebuffer was created or changed size.
    Resized { width: u32, height: u32 },
    /// Pixels changed; redraw from [`Session::with_framebuffer`].
    Updated,
    /// The pointer shape changed; see [`Session::cursor`].
    Cursor,
    Title(String),
    /// Something the user should see (e.g. a server message).
    Message(String),
    Bell,
    /// The server's clipboard has new text, for the local clipboard.
    Clipboard(String),
    /// Files were copied on the server, for the local clipboard. They're local
    /// copies the server made on the transfer drive.
    ClipboardFiles(Vec<std::path::PathBuf>),
    /// The session is over.
    Ended(Outcome),
}

#[derive(Clone, Debug)]
pub struct Outcome {
    /// rdesktop-compatible exit status (0 = normal logoff/disconnect).
    pub code: i32,
    pub message: String,
    /// The user closed the window.
    pub user_quit: bool,
}

pub trait Frontend: Send + Sync + 'static {
    fn notify(&self, event: Event);

    /// Size of the display the window will be on, in pixels.
    fn screen_size(&self) -> (u32, u32) {
        (1024, 768)
    }

    /// Ask the user a yes/no question (e.g. "trust this certificate?").
    /// Called on the rdesktop thread, which blocks until it returns.
    fn ask(&self, _title: &str, _message: &str, _accept: &str, _reject: &str) -> bool {
        false
    }
}

pub struct CursorImage {
    pub width: usize,
    pub height: usize,
    pub hot_x: usize,
    pub hot_y: usize,
    /// Unpremultiplied RGBA, top row first.
    pub rgba: Vec<u8>,
}

#[derive(Clone, Default)]
pub enum CursorShape {
    #[default]
    Default,
    Hidden,
    Image(Arc<CursorImage>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

pub(crate) struct Screen {
    pub surface: Option<Surface>,
    pub cursor: CursorShape,
}

#[derive(Default)]
struct Clipboard {
    /// The text both sides last agreed on.
    text: String,
    /// `text` came from here and the server hasn't been told yet.
    changed: bool,
    /// Files on offer instead of text, as `C:\\...` paths on the transfer drive.
    files: Vec<String>,
    /// The format the server is about to send.
    format: u32,
    /// The server opened the channel. RDP 4 servers never do.
    ready: bool,
}

pub(crate) struct Shared {
    pub frontend: Arc<dyn Frontend>,
    pub config: Config,
    pub screen: Mutex<Screen>,
    input: Mutex<Vec<Input>>,
    quit: AtomicBool,
    /// Wakes the session loop: input is waiting, or the network has data.
    pub(crate) tx: mpsc::Sender<LoopMsg>,
    clipboard: Mutex<Clipboard>,
    /// The local lock keys, as TS_SYNC_EVENT flags.
    locks: AtomicU16,
    outcome: Mutex<Option<Outcome>>,
}

/// What wakes the session loop.
pub(crate) enum LoopMsg {
    /// Input or clipboard changes are waiting in [`Shared`].
    Wake,
    /// A whole frame (TPKT or fast-path) from connection number `conn`.
    Frame { conn: u32, data: Vec<u8> },
    /// Connection `conn` closed or failed.
    Closed { conn: u32, error: std::io::Error },
}

impl Shared {
    pub(crate) fn wake(&self) {
        let _ = self.tx.send(LoopMsg::Wake);
    }

    pub fn take_clipboard_change(&self) -> bool {
        let mut c = self.clipboard.lock().unwrap();
        c.ready && std::mem::take(&mut c.changed)
    }

    pub fn clipboard_ready(&self) {
        let mut c = self.clipboard.lock().unwrap();
        c.ready = true;
        c.changed = false;
    }

    pub fn clipboard_text(&self) -> String {
        self.clipboard.lock().unwrap().text.clone()
    }

    pub fn clipboard_files(&self) -> Vec<String> {
        self.clipboard.lock().unwrap().files.clone()
    }

    pub fn clipboard_format(&self) -> u32 {
        self.clipboard.lock().unwrap().format
    }

    pub fn set_clipboard_format(&self, format: u32) {
        self.clipboard.lock().unwrap().format = format;
    }

    /// The server's clipboard holds files now, not the text we knew.
    pub fn note_remote_files(&self) {
        let mut c = self.clipboard.lock().unwrap();
        c.text.clear();
        c.files.clear();
    }

    /// Record text from the server. False if it's what we already have.
    pub fn note_remote_clipboard(&self, text: &str) -> bool {
        let mut c = self.clipboard.lock().unwrap();
        let new = c.text != text;
        c.text = text.to_string();
        new
    }

    /// The local lock keys, for the sync event sent at connect.
    pub fn locks(&self) -> u16 {
        self.locks.load(Ordering::Relaxed)
    }

    pub fn take_input(&self) -> Vec<Input> {
        std::mem::take(&mut *self.input.lock().unwrap())
    }

    pub fn quit_requested(&self) -> bool {
        self.quit.load(Ordering::SeqCst)
    }
}

/// Handle to the running session. Cheap to clone; usable from any thread.
#[derive(Clone)]
pub struct Session {
    shared: Arc<Shared>,
}

impl Session {
    /// Connect using `config` on a new thread.
    pub fn start(config: Config, frontend: Arc<dyn Frontend>) -> Result<Session, String> {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            frontend,
            config,
            screen: Mutex::new(Screen { surface: None, cursor: CursorShape::Default }),
            input: Mutex::new(Vec::new()),
            quit: AtomicBool::new(false),
            tx,
            clipboard: Mutex::new(Clipboard::default()),
            locks: AtomicU16::new(0),
            outcome: Mutex::new(None),
        });
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("rdp".into())
            .spawn(move || {
                let outcome = crate::rdp::run(&sh, rx);
                *sh.outcome.lock().unwrap() = Some(outcome.clone());
                sh.frontend.notify(Event::Ended(outcome));
            })
            .map_err(|e| e.to_string())?;
        Ok(Session { shared })
    }

    pub fn config(&self) -> &Config {
        &self.shared.config
    }

    /// Read the framebuffer: `(width, height, pixels)` with pixels as
    /// `0x00RRGGBB`, row-major. `None` before the session window exists.
    pub fn with_framebuffer<R>(&self, f: impl FnOnce(usize, usize, &[u32]) -> R) -> Option<R> {
        let screen = self.shared.screen.lock().unwrap();
        screen.surface.as_ref().map(|s| f(s.width, s.height, &s.px))
    }

    pub fn cursor(&self) -> CursorShape {
        self.shared.screen.lock().unwrap().cursor.clone()
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.shared.outcome.lock().unwrap().clone()
    }

    fn push(&self, events: impl IntoIterator<Item = Input>) {
        let events: Vec<Input> = events.into_iter().collect();
        self.shared.input.lock().unwrap().extend(events);
        self.shared.wake();
    }

    /// A PC/AT set-1 scancode; `extended` for E0-prefixed keys.
    pub fn key(&self, scancode: u8, extended: bool, down: bool) {
        let flags = if down { RDP_KEYPRESS } else { RDP_KEYRELEASE } | if extended { KBD_FLAG_EXT } else { 0 };
        self.push([Input { message_type: RDP_INPUT_SCANCODE, flags, param1: scancode as u16, param2: 0 }]);
    }

    /// Type a character that no key on a US keyboard makes (an accented
    /// letter from an on-screen or remote-control keyboard, say). Goes as a
    /// Unicode key event where the server takes them (Windows XP/2003 and
    /// later), else as an Alt+0nnn keypad code in Windows-1252; characters
    /// outside that code page can't reach older servers.
    pub fn type_char(&self, c: char) {
        let code = c as u32;
        self.push([Input { message_type: TYPE_CHAR, flags: 0, param1: code as u16, param2: (code >> 16) as u16 }]);
    }

    /// Match the server's Caps, Num and Scroll Lock to the local keyboard.
    /// Front-ends call this when the session window gets focus (the locks may
    /// have changed elsewhere); the state is also sent when the session starts.
    pub fn sync_locks(&self, caps: bool, num: bool, scroll: bool) {
        const SCROLL: u16 = 1;
        const NUM: u16 = 2;
        const CAPS: u16 = 4;
        let flags = if caps { CAPS } else { 0 } | if num { NUM } else { 0 } | if scroll { SCROLL } else { 0 };
        self.shared.locks.store(flags, Ordering::Relaxed);
        // A sync event, or key taps on RDP 4: see locks.rs.
        self.push([Input { message_type: crate::locks::TYPE_LOCKS, flags: 0, param1: flags, param2: 0 }]);
    }

    /// True once the server has opened the clipboard and there's a transfer
    /// drive, which is what it takes to send files.
    pub fn can_send_files(&self) -> bool {
        self.shared.config.transfer.is_some() && self.shared.clipboard.lock().unwrap().ready
    }

    /// Put files on the server's clipboard, ready to paste. `paths` are on the
    /// transfer drive, as `C:\\...`.
    pub fn set_clipboard_files(&self, paths: Vec<String>) {
        let mut c = self.shared.clipboard.lock().unwrap();
        c.files = paths;
        c.changed = true;
        drop(c);
        self.shared.wake();
    }

    /// Paste into whatever is at (x, y) on the server: click it, then Ctrl+V.
    pub fn paste_at(&self, x: u16, y: u16) {
        const CTRL: u8 = 0x1d;
        const V: u8 = 0x2f;
        self.mouse_button(MouseButton::Left, true, x, y);
        self.mouse_button(MouseButton::Left, false, x, y);
        self.key(CTRL, false, true);
        self.key(V, false, true);
        self.key(V, false, false);
        self.key(CTRL, false, false);
    }

    /// Offer local clipboard text to the server. Front-ends call this when the
    /// session window gets focus; unchanged text costs nothing.
    pub fn set_clipboard(&self, text: &str) {
        let text = text.replace("\r\n", "\n");
        let mut c = self.shared.clipboard.lock().unwrap();
        if c.text != text {
            c.text = text;
            c.files.clear();
            c.changed = true;
            drop(c);
            self.shared.wake();
        }
    }

    pub fn mouse_move(&self, x: u16, y: u16) {
        self.push([Input { message_type: RDP_INPUT_MOUSE, flags: MOUSE_FLAG_MOVE, param1: x, param2: y }]);
    }

    pub fn mouse_button(&self, button: MouseButton, down: bool, x: u16, y: u16) {
        let flag = match button {
            MouseButton::Left => MOUSE_FLAG_BUTTON1,
            MouseButton::Right => MOUSE_FLAG_BUTTON2,
            MouseButton::Middle => MOUSE_FLAG_BUTTON3,
        };
        let flags = flag | if down { MOUSE_FLAG_DOWN } else { 0 };
        self.push([Input { message_type: RDP_INPUT_MOUSE, flags, param1: x, param2: y }]);
    }

    /// One wheel notch.
    pub fn wheel(&self, up: bool, x: u16, y: u16) {
        let flags = if up { MOUSE_FLAG_BUTTON4 } else { MOUSE_FLAG_BUTTON5 };
        self.push([Input { message_type: RDP_INPUT_MOUSE, flags, param1: x, param2: y }]);
    }

    /// Disconnect. [`Event::Ended`] follows.
    pub fn quit(&self) {
        self.shared.quit.store(true, Ordering::SeqCst);
        self.shared.wake();
    }

    /// Wait up to `timeout` for the session thread to finish.
    pub fn wait(&self, timeout: Duration) -> Option<Outcome> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(o) = self.outcome() {
                return Some(o);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// VINTRDP_TRACE_INPUT=1 logs every input event sent to the server.
pub(crate) fn trace_input() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VINTRDP_TRACE_INPUT").is_some())
}
