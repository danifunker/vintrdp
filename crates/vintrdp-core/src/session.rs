//! The session: runs rdesktop's protocol core on its own thread and talks to
//! a native front-end through the [`Frontend`] trait.
//!
//! rdesktop's core is single-threaded and keeps its state in C globals, so
//! there is one session per process. Front-ends never call into C: they queue
//! input on a [`Session`] and read the framebuffer, and the core reports back
//! through [`Frontend::notify`].

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::{Config, RdpVersion};
use crate::ffi::{self, *};
use crate::gdi::Surface;
use crate::state::{self, *};

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

#[derive(Clone, Copy, Debug)]
pub(crate) struct Input {
    pub message_type: u16,
    pub flags: u16,
    pub param1: u16,
    pub param2: u16,
}

/// Queued by [`Session::type_char`]; never sent as is. The character is in
/// param1 (low 16 bits) and param2 (the rest).
pub(crate) const TYPE_CHAR: u16 = 0xfffe;

/// The events that type `c`: Unicode key events if the server takes them,
/// else an Alt+0nnn keypad code.
pub(crate) fn expand_char(c: char, unicode: bool) -> Vec<Input> {
    const RDP_INPUT_UNICODE: u16 = 5;
    const RELEASE: u16 = 0x8000;
    let key = |sc: crate::scancode::Scancode, down: bool| Input {
        message_type: RDP_INPUT_SCANCODE,
        flags: if down { RDP_KEYPRESS } else { RDP_KEYRELEASE } | if sc.1 { KBD_FLAG_EXT } else { 0 },
        param1: sc.0 as u16,
        param2: 0,
    };
    if c == '\0' {
        return Vec::new();
    }
    if unicode {
        let mut units = [0u16; 2];
        return c
            .encode_utf16(&mut units)
            .iter()
            .flat_map(|&u| [false, true].map(|up| Input { message_type: RDP_INPUT_UNICODE, flags: if up { RELEASE } else { 0 }, param1: u, param2: 0 }))
            .collect();
    }
    let Some(byte) = crate::scancode::cp1252(c) else { return Vec::new() };
    let alt = crate::scancode::LALT;
    let mut events = vec![key(alt, true)];
    for d in crate::scancode::alt_code(byte) {
        events.extend([key(d, true), key(d, false)]);
    }
    events.push(key(alt, false));
    events
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
    wake: crate::wake::Wake,
    clipboard: Mutex<Clipboard>,
    /// The local lock keys, as TS_SYNC_EVENT flags.
    locks: AtomicU16,
    outcome: Mutex<Option<Outcome>>,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

pub(crate) fn shared() -> &'static Arc<Shared> {
    SHARED.get().expect("no session running")
}

impl Shared {
    fn wake(&self) {
        self.wake.signal();
    }

    pub fn wake_fd(&self) -> std::os::raw::c_int {
        self.wake.socket()
    }

    pub fn drain_wake(&self) {
        self.wake.drain();
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

    /// The local lock keys, for the sync event rdesktop sends at connect.
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
    /// Connect using `config` on a new thread. Only one session can exist per
    /// process, because rdesktop's core state is global.
    pub fn start(config: Config, frontend: Arc<dyn Frontend>) -> Result<Session, String> {
        let wake = crate::wake::Wake::new().map_err(|e| format!("could not create the wake socket: {e}"))?;
        let shared = Arc::new(Shared {
            frontend,
            config,
            screen: Mutex::new(Screen { surface: None, cursor: CursorShape::Default }),
            input: Mutex::new(Vec::new()),
            quit: AtomicBool::new(false),
            wake,
            clipboard: Mutex::new(Clipboard::default()),
            locks: AtomicU16::new(0),
            outcome: Mutex::new(None),
        });
        SHARED
            .set(shared.clone())
            .map_err(|_| "a session is already running in this process".to_string())?;

        std::thread::Builder::new()
            .name("rdesktop".into())
            .spawn(|| {
                let sh = shared_clone();
                let outcome = run(&sh.config);
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

fn shared_clone() -> Arc<Shared> {
    shared().clone()
}

// ---------------------------------------------------------------------------
// The connection loop (ported from rdesktop.c's main())

const RDP_INFO_MOUSE: u32 = 0x1;
const RDP_INFO_DISABLECTRLALTDEL: u32 = 0x2;
const RDP_INFO_AUTOLOGON: u32 = 0x8;
const RDP_INFO_UNICODE: u32 = 0x10;
const RDP_INFO_MAXIMIZESHELL: u32 = 0x20;
const RDP_INFO_COMPRESSION: u32 = 0x80;
const RDP_INFO_ENABLEWINDOWSKEY: u32 = 0x100;
const RDP_INFO_COMPRESSION2: u32 = 0x200;

const ERRINFO_UNSET: u32 = u32::MAX;
const ERRINFO_NO_INFO: u32 = 0;
const ERRINFO_LOGOFF_BYUSER: u32 = 0x000c;

const EX_PROTOCOL: i32 = 76;
#[cfg(unix)]
const EX_OSERR: i32 = 71;
const EXRD_WINDOW_CLOSED: i32 = 62;
const EXRD_UNKNOWN: i32 = 63;

const RECONNECT_TIMEOUT: Duration = Duration::from_secs(3600 + 600);

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap()
}

/// Copy into a writable, NUL-terminated buffer (rdp_connect takes `char *`).
fn cbuf(s: &str) -> Vec<libc::c_char> {
    cstring(s).into_bytes_with_nul().into_iter().map(|b| b as libc::c_char).collect()
}

fn ended(code: i32, message: impl Into<String>) -> Outcome {
    Outcome { code, message: message.into(), user_quit: false }
}

fn run(config: &Config) -> Outcome {
    // SAFETY: everything below runs on the rdesktop thread, which is the only
    // thread that touches rdesktop's globals and C functions.
    unsafe {
        logger_set_verbose(config.verbose as libc::c_int);
        state::apply(config);
        crate::shim::ui_init();

        // Files on the clipboard pass through a drive with a letter for a name;
        // that's how the server turns C:\\x into \\\\tsclient\\c\\x. It only ever
        // holds copies of what's being sent, so start it empty.
        if let Some(dir) = &config.transfer {
            let _ = std::fs::remove_dir_all(dir);
            if std::fs::create_dir_all(dir.join("temp")).is_ok() {
                let mut spec = cbuf(&format!(":C={}", dir.display()));
                disk_enum_devices(ptr_mut(&raw mut g_num_devices), spec.as_mut_ptr());
            }
        }
        for (name, path) in &config.disks {
            // disk_enum_devices() skips the colon of rdesktop's "disk:name=path".
            let mut spec = cbuf(&format!(":{name}={}", path.display()));
            disk_enum_devices(ptr_mut(&raw mut g_num_devices), spec.as_mut_ptr());
        }
        // rdesktop's option syntax, split on ',' and '=' unless escaped.
        let escape = |s: &str| s.replace(',', "\\,").replace('=', "\\=");
        for p in &config.printers {
            let name = p.name.clone().or_else(crate::printing::default_printer);
            let driver = p.driver.clone().unwrap_or_else(|| crate::printing::default_driver(name.as_deref()));
            // "mydeskjet" is printer.c's name for the default printer.
            let mut spec = cbuf(&format!(":{}={}", escape(name.as_deref().unwrap_or("mydeskjet")), driver.replace(',', "\\,")));
            printer_enum_devices(ptr_mut(&raw mut g_num_devices), spec.as_mut_ptr());
        }
        for (ports, enumerate) in [(&config.serial_ports, serial_enum_devices as unsafe extern "C" fn(_, _) -> _), (&config.parallel_ports, parallel_enum_devices)] {
            for (port, device) in ports {
                let mut spec = cbuf(&format!(":{port}={}", escape(device)));
                enumerate(ptr_mut(&raw mut g_num_devices), spec.as_mut_ptr());
            }
        }
        // Add-ins and the Kerberos helper are Unix-only.
        #[cfg(unix)]
        if addin_init() == 0 {
            addin_deinit();
            return ended(EX_OSERR, "Could not initialise add-ins");
        }
        rdpdr_init();
        if config.clipboard {
            cliprdr_init();
        }
        // RDP 4 has no channels to play sound over.
        if config.sound && config.version != RdpVersion::V4 {
            rdpsnd_init(std::ptr::null_mut());
        }
        dvc_init();
        #[cfg(unix)]
        if rdpear_init() == 0 {
            return ended(EX_PROTOCOL, "Could not initialise RDPEAR");
        }
        rdpedisp_init();

        let mut flags = RDP_INFO_MOUSE | RDP_INFO_DISABLECTRLALTDEL | RDP_INFO_UNICODE | RDP_INFO_MAXIMIZESHELL | RDP_INFO_ENABLEWINDOWSKEY;
        if !config.password.is_empty() {
            flags |= RDP_INFO_AUTOLOGON;
        }
        if config.compression {
            flags |= RDP_INFO_COMPRESSION | RDP_INFO_COMPRESSION2;
        }

        let mut server = config.server.clone();
        let mut domain = config.domain.clone();
        // Downgrade to RDP 4 on refusal when allowed, except when RDP 4 was
        // the explicit request (nothing to fall back from).
        let mut version_fallback = config.fallback_rdp4 && config.version != RdpVersion::V4;
        let mut reconnect = false;
        let mut retry_started: Option<Instant> = None;
        let mut deactivated: RD_BOOL;
        let mut ext_disc_reason: u32;

        loop {
            rdp_reset_state();

            if g_redirect != 0 {
                domain = cstr_lossy(g_redirect_domain);
                libc::free(g_username.cast());
                g_username = libc::strdup(g_redirect_username);
                server = cstr_lossy(g_redirect_server);
                flags |= RDP_INFO_AUTOLOGON;
                g_network_error = 0;
            }

            utils_apply_session_size_limitations(&raw mut g_requested_session_width, &raw mut g_requested_session_height);
            let (c_server, c_domain, c_user) = (cstring(&server), cstring(&domain), cstring(&cstr_lossy(g_username)));
            pstcache_set_namespace(c_server.as_ptr(), g_tcp_port_rdp, c_domain.as_ptr(), c_user.as_ptr());

            g_gui_prompt_cancelled = 0;
            let (mut s, mut d) = (cbuf(&server), cbuf(&domain));
            let (mut shell, mut dir) = (cbuf(&config.shell), cbuf(&config.directory));
            let password = (&raw mut g_password).cast::<libc::c_char>();
            let ok = rdp_connect(s.as_mut_ptr(), flags, d.as_mut_ptr(), password, shell.as_mut_ptr(), dir.as_mut_ptr(), reconnect as RD_BOOL);

            if ok == 0 {
                g_network_error = 0;
                if shared().quit_requested() || g_gui_prompt_cancelled != 0 {
                    return Outcome { code: EXRD_WINDOW_CLOSED, message: "Connection cancelled".into(), user_quit: true };
                }
                // Two servers refuse RDP 5 but accept RDP 4: NT 4.0 TSE answers an
                // RDP 5 request as "RDP 4" (secure.c downgrades g_rdp_version)
                // then rejects the channels we asked for, and a Terminal Server
                // with no reachable licence server rejects RDP 5 licensing
                // ("Invalid client"). RDP 4 carries no per-session licence, so
                // retry there — the server itself accepts that older dialect.
                let licence_refused = g_licence_error_result != 0;
                if version_fallback && (g_rdp_version == state::RDP_V4 || licence_refused) {
                    version_fallback = false;
                    g_rdp_version = state::RDP_V4;
                    g_server_depth = 8;
                    g_licence_error_result = 0;
                    // RDP 4 has no virtual channels; -4 never registers any.
                    g_num_channels = 0;
                    let why = if licence_refused {
                        "Server needs a licence for RDP 5; connecting with RDP 4"
                    } else {
                        "Server rejected RDP 5; retrying with RDP 4"
                    };
                    shared().frontend.notify(Event::Message(why.into()));
                    // The server resets a connection that arrives while it is
                    // still tearing down the refused one.
                    std::thread::sleep(Duration::from_secs(3));
                    continue;
                }
                let retrying = reconnect || retry_started.is_some();
                if !retrying {
                    return ended(EX_PROTOCOL, format!("Unable to connect to {server}"));
                }
                if retry_started.get_or_insert_with(Instant::now).elapsed() > RECONNECT_TIMEOUT {
                    return ended(EX_PROTOCOL, "Gave up reconnecting");
                }
                std::thread::sleep(Duration::from_secs(4));
                continue;
            }
            version_fallback = false;

            if g_redirect != 0 {
                rdp_disconnect();
                continue;
            }

            if config.encrypt_login_only {
                g_encryption_initial = 0;
                g_encryption = 0;
            }
            retry_started = None;
            logger_verbose("Connection successful");

            crate::glue::rd_create_ui();
            tcp_run_ui(1);
            deactivated = 0;
            reconnect = false;
            ext_disc_reason = ERRINFO_UNSET;
            rdp_main_loop(&mut deactivated, &mut ext_disc_reason);
            tcp_run_ui(0);
            rdp_disconnect();

            // Windows 2008 and older report a user disconnect differently.
            if deactivated != 0 && ext_disc_reason == ERRINFO_NO_INFO {
                deactivated = 0;
                ext_disc_reason = ERRINFO_LOGOFF_BYUSER;
            } else if ext_disc_reason == 0 {
                ext_disc_reason = ERRINFO_UNSET;
            }

            if g_user_quit != 0 || deactivated != 0 || ext_disc_reason != ERRINFO_UNSET {
                break;
            }
            if g_redirect != 0 {
                continue;
            }
            if g_network_error != 0 {
                g_network_error = 0;
                if g_reconnect_random_ts != 0 {
                    reconnect = true; // auto-reconnect cookie available
                } else if config.reconnect_on_network_error {
                    retry_started.get_or_insert_with(Instant::now);
                } else {
                    return ended(EX_PROTOCOL, "Disconnected due to a network error");
                }
                continue;
            }
            if g_pending_resize != 0 {
                g_pending_resize = 0;
                reconnect = true;
                crate::shim::ui_destroy_window();
                continue;
            }
            break;
        }

        crate::shim::ui_destroy_window();
        cache_save_state();
        #[cfg(unix)]
        addin_deinit();

        if g_user_quit != 0 {
            return Outcome { code: EXRD_WINDOW_CLOSED, message: "Window closed".into(), user_quit: true };
        }
        let (code, text) = disconnect_reason(deactivated != 0, ext_disc_reason);
        ended(code, text)
    }
}

/// `&raw mut` on an extern static, as a plain pointer.
fn ptr_mut<T>(p: *mut T) -> *mut T {
    p
}

unsafe fn cstr_lossy(p: *const libc::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: caller passes a NUL-terminated string.
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn logger_verbose(msg: &str) {
    if shared().config.verbose {
        eprintln!("{msg}");
    }
}

/// rdesktop's handle_disconnect_reason(): exit status and text.
fn disconnect_reason(deactivated: bool, reason: u32) -> (i32, &'static str) {
    match reason {
        ERRINFO_NO_INFO if deactivated => (0, "Disconnected"),
        ERRINFO_NO_INFO => (EXRD_UNKNOWN, "No information available"),
        0x0001 => (1, "Administrator initiated disconnect"),
        0x0002 => (2, "Administrator initiated logout"),
        0x0003 => (3, "Server idle session time limit reached"),
        0x0004 => (4, "Server active session time limit reached"),
        0x0005 => (5, "The session was replaced"),
        0x0006 => (6, "The server is out of memory"),
        0x0007 => (7, "The server denied the connection"),
        0x0008 => (8, "The server denied the connection for security reasons"),
        0x0009 => (9, "The user cannot connect to the server due to insufficient access privileges"),
        0x000a => (10, "The server requires that the user enter their credentials for each connection"),
        0x000b => (11, "Disconnect initiated by user"),
        0x000c => (12, "Logout initiated by user"),
        0x0100 => (16, "Internal licensing error"),
        0x0101 => (17, "No license server available"),
        0x0102 => (18, "No valid license available"),
        0x0103 => (19, "Invalid licensing message from client"),
        0x0104 => (20, "The client license no longer matches the hardware ID"),
        0x0105 => (21, "The client license is in an invalid format"),
        0x0106 => (22, "Network error during licensing protocol"),
        0x0107 => (23, "Licensing protocol was not completed"),
        0x0108 => (24, "Incorrect client license encryption"),
        0x0109 => (25, "Can't upgrade or renew license"),
        0x010a => (26, "The server is not licensed to accept remote connections"),
        0x0400 => (30, "The target endpoint chosen by the broker could not be found"),
        0x0402 => (32, "The target endpoint is disconnecting from the broker"),
        0x0404 => (34, "Error occurred while being redirected by broker"),
        0x0405 => (35, "Error while the endpoint VM was being awakened by the broker"),
        0x0406 => (36, "Error while the endpoint VM was being started by the broker"),
        0x0407 => (37, "The IP address of the endpoint VM could not be determined by the broker"),
        0x0408 => (38, "No available endpoints in the connection broker pool"),
        0x0409 => (39, "Connection processing cancelled by the broker"),
        0x0410 => (40, "The connection settings could not be validated by the broker"),
        0x0411 => (41, "Timeout while the endpoint VM was being started by the broker"),
        0x0412 => (42, "Session monitoring error while the endpoint VM was being started by the broker"),
        0x10f3 => (50, "The server can only host Remote Applications"),
        0x1191 => (51, "Update of session keys failed"),
        0x1192 => (52, "Decryption or session key creation failed"),
        0x1193 => (53, "Encryption failed"),
        ERRINFO_UNSET => (0, "Disconnected"),
        _ => (EXRD_UNKNOWN, "Unknown reason"),
    }
}

pub(crate) use ffi::{RDP_INPUT_MOUSE, RDP_INPUT_SCANCODE};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_type_as_unicode_or_alt_codes() {
        let unicode = expand_char('\u{e9}', true);
        assert_eq!(unicode.iter().map(|e| (e.message_type, e.flags, e.param1)).collect::<Vec<_>>(), [(5, 0, 0xe9), (5, 0x8000, 0xe9)]);
        // Outside the BMP: a surrogate pair, each pressed and released.
        assert_eq!(expand_char('\u{1f600}', true).len(), 4);

        // Alt down, keypad 0 2 3 3 (each down and up), Alt up.
        let alt = expand_char('\u{e9}', false);
        assert_eq!(alt.len(), 10);
        assert_eq!((alt[0].param1, alt[0].flags), (0x38, RDP_KEYPRESS));
        assert_eq!((alt[9].param1, alt[9].flags), (0x38, RDP_KEYRELEASE));
        assert_eq!(alt[1].param1, 0x52);

        assert!(expand_char('\u{4e16}', false).is_empty());
        assert!(expand_char('\0', true).is_empty());
    }
}
