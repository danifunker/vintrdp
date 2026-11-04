//! The session: runs rdesktop's protocol core on its own thread and talks to
//! a native front-end through the [`Frontend`] trait.
//!
//! rdesktop's core is single-threaded and keeps its state in C globals, so
//! there is one session per process. Front-ends never call into C: they queue
//! input on a [`Session`] and read the framebuffer, and the core reports back
//! through [`Frontend::notify`].

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
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

pub(crate) struct Screen {
    pub surface: Option<Surface>,
    pub cursor: CursorShape,
}

pub(crate) struct Shared {
    pub frontend: Arc<dyn Frontend>,
    pub config: Config,
    pub screen: Mutex<Screen>,
    input: Mutex<Vec<Input>>,
    quit: AtomicBool,
    wake_read: OwnedFd,
    wake_write: OwnedFd,
    outcome: Mutex<Option<Outcome>>,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

pub(crate) fn shared() -> &'static Arc<Shared> {
    SHARED.get().expect("no session running")
}

impl Shared {
    fn wake(&self) {
        // A full pipe already guarantees a wakeup, so errors don't matter.
        // SAFETY: one byte from a valid buffer to our own pipe.
        unsafe { libc::write(self.wake_write.as_raw_fd(), [0u8].as_ptr().cast(), 1) };
    }

    pub fn wake_fd(&self) -> RawFd {
        self.wake_read.as_raw_fd()
    }

    pub fn drain_wake(&self) {
        let mut buf = [0u8; 64];
        // SAFETY: reading into a valid buffer from our own non-blocking pipe.
        while unsafe { libc::read(self.wake_read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
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
        let mut fds = [0; 2];
        // SAFETY: valid two-element array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err("pipe() failed".into());
        }
        for fd in fds {
            // SAFETY: fd was just returned by pipe().
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        let shared = Arc::new(Shared {
            frontend,
            config,
            screen: Mutex::new(Screen { surface: None, cursor: CursorShape::Default }),
            input: Mutex::new(Vec::new()),
            quit: AtomicBool::new(false),
            // SAFETY: freshly created pipe fds, owned exclusively here.
            wake_read: unsafe { OwnedFd::from_raw_fd(fds[0]) },
            wake_write: unsafe { OwnedFd::from_raw_fd(fds[1]) },
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
        if trace_input() {
            for e in &events {
                eprintln!("input: type={:#06x} flags={:#06x} {:#06x} {:#06x}", e.message_type, e.flags, e.param1, e.param2);
            }
        }
        self.shared.input.lock().unwrap().extend(events);
        self.shared.wake();
    }

    /// A PC/AT set-1 scancode; `extended` for E0-prefixed keys.
    pub fn key(&self, scancode: u8, extended: bool, down: bool) {
        let flags = if down { RDP_KEYPRESS } else { RDP_KEYRELEASE } | if extended { KBD_FLAG_EXT } else { 0 };
        self.push([Input { message_type: RDP_INPUT_SCANCODE, flags, param1: scancode as u16, param2: 0 }]);
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
fn trace_input() -> bool {
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

        for (name, path) in &config.disks {
            let mut spec = cbuf(&format!("{name}={}", path.display()));
            disk_enum_devices(ptr_mut(&raw mut g_num_devices), spec.as_mut_ptr());
        }

        if addin_init() == 0 {
            addin_deinit();
            return ended(EX_OSERR, "Could not initialise add-ins");
        }
        rdpdr_init();
        dvc_init();
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
        let mut version_fallback = config.version == RdpVersion::Auto;
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
                // NT 4.0 Terminal Server answers an RDP 5 request with "RDP 4"
                // (secure.c then downgrades g_rdp_version) but refuses the
                // virtual channels we already asked for. Reconnect as RDP 4.
                if version_fallback && g_rdp_version == state::RDP_V4 {
                    version_fallback = false;
                    g_rdp_version = state::RDP_V4;
                    g_server_depth = 8;
                    // RDP 4 has no virtual channels; -4 never registers any.
                    g_num_channels = 0;
                    shared().frontend.notify(Event::Message("Server rejected RDP 5; retrying with RDP 4".into()));
                    // NT4 TSE resets a connection that arrives while it is
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
