//! Windows front-end: plain Win32.
//!
//! Everything runs on the main thread's message loop. The rdesktop thread
//! hands work over by posting boxed closures to a hidden message window. The
//! connection manager mirrors the macOS one: tiles on the left, details on the
//! right.

#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, S_OK, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HRESULT, PCWSTR, w};

use vintrdp_core::scancode::{self, Scancode};
use vintrdp_core::{Config, CursorShape, Event, Frontend, MouseButton, Session};

use crate::launch::{self, DEPTHS, Form, MONITORS, Picture, Protocol, Rgba, SIZES, Store, TileImage, decode_png, remember, size_index, size_label};

/// Let `vintrdp --version` and friends print to the console they came from.
pub fn attach_console() {
    // SAFETY: no preconditions; failing just means there's no console.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

pub fn run(config: Option<Config>) -> i32 {
    // SAFETY: the rest of this file runs on this thread, as Win32 requires.
    unsafe {
        let controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES,
        };
        let _ = InitCommonControlsEx(&controls);
        register_classes();
        make_message_window();
        match config {
            Some(c) => start_session(c, None),
            None => show_manager(),
        }
        message_loop();
    }
    0
}

unsafe fn message_loop() {
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        // Tab, Enter and friends in the manager's form.
        let dialog = MGR_WINDOW.load(Ordering::Relaxed);
        if dialog != 0 && IsDialogMessageW(HWND(dialog as *mut c_void), &msg).as_bool() {
            continue;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

// ---------------------------------------------------------------------------
// Small helpers

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn instance() -> HINSTANCE {
    // SAFETY: asks for this module's own handle.
    unsafe { GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default() }
}

unsafe fn send(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    SendMessageW(hwnd, msg, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0
}

unsafe fn text_of(hwnd: HWND) -> String {
    let mut buf = vec![0u16; GetWindowTextLengthW(hwnd) as usize + 1];
    let n = GetWindowTextW(hwnd, &mut buf);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    let _ = SetWindowTextW(hwnd, PCWSTR(text.as_ptr()));
}

fn alert(title: &str, message: &str) {
    let (title, message) = (wide(title), wide(message));
    // SAFETY: valid NUL-terminated strings.
    unsafe { MessageBoxW(None, PCWSTR(message.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONWARNING) };
}

/// A two-button question with our own button labels.
fn ask_dialog(title: &str, message: &str, accept: &str, reject: &str) -> bool {
    let (title, message, accept, reject) = (wide(title), wide(message), wide(accept), wide(reject));
    let buttons = [
        TASKDIALOG_BUTTON { nButtonID: 100, pszButtonText: PCWSTR(accept.as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: 101, pszButtonText: PCWSTR(reject.as_ptr()) },
    ];
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszContent: PCWSTR(message.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: 101,
        ..Default::default()
    };
    let mut pressed = 0;
    // SAFETY: config and the strings it points to outlive the call.
    unsafe { TaskDialogIndirect(&config, Some(&mut pressed), None, None).is_ok() && pressed == 100 }
}

unsafe extern "system" fn about_callback(_: HWND, msg: TASKDIALOG_NOTIFICATIONS, _: WPARAM, lparam: LPARAM, _: isize) -> HRESULT {
    if msg == TDN_HYPERLINK_CLICKED {
        ShellExecuteW(None, w!("open"), PCWSTR(lparam.0 as *const u16), None, None, SW_SHOWNORMAL);
    }
    S_OK
}

fn show_about(owner: HWND) {
    let heading = wide(&format!("vintrdp {}", launch::VERSION));
    let body = wide(&format!(
        "{}\n<a href=\"{repo}\">github.com/danifunker/vintrdp</a>\n\nA fork of <a href=\"{rdesktop}\">rdesktop</a>, \u{a9} 1999\u{2013}2016 Matthew Chapman et al.\n\n\
         Free software under the <a href=\"{gpl}\">GNU GPL v3</a> or later.\nIt comes with ABSOLUTELY NO WARRANTY.",
        launch::DESCRIPTION,
        repo = launch::REPO_URL,
        rdesktop = launch::RDESKTOP_URL,
        gpl = launch::GPL_URL,
    ));
    let mut config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        hInstance: instance(),
        dwFlags: TDF_ENABLE_HYPERLINKS | TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_OK_BUTTON,
        pszWindowTitle: w!("About vintrdp"),
        pszMainInstruction: PCWSTR(heading.as_ptr()),
        pszContent: PCWSTR(body.as_ptr()),
        pfCallback: Some(about_callback),
        ..Default::default()
    };
    config.Anonymous1.pszMainIcon = PCWSTR(1 as *const u16);
    // SAFETY: config and its strings outlive the call.
    let _ = unsafe { TaskDialogIndirect(&config, None, None, None) };
}

// ---------------------------------------------------------------------------
// Getting work from the rdesktop thread onto the UI thread

const WM_RUN: u32 = WM_APP + 1;
type Job = Box<dyn FnOnce() + Send>;

/// The hidden window jobs are posted to.
static MESSAGE_WINDOW: AtomicIsize = AtomicIsize::new(0);

unsafe fn make_message_window() {
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("VintrdpMessages"),
        None,
        WINDOW_STYLE(0),
        0,
        0,
        0,
        0,
        Some(HWND_MESSAGE),
        None,
        Some(instance()),
        None,
    )
    .expect("message window");
    MESSAGE_WINDOW.store(hwnd.0 as isize, Ordering::Release);
}

fn run_on_ui(job: impl FnOnce() + Send + 'static) {
    let job: Box<Job> = Box::new(Box::new(job));
    let hwnd = HWND(MESSAGE_WINDOW.load(Ordering::Acquire) as *mut c_void);
    // SAFETY: the window procedure takes the pointer back with Box::from_raw.
    unsafe {
        let raw = Box::into_raw(job);
        if PostMessageW(Some(hwnd), WM_RUN, WPARAM(0), LPARAM(raw as isize)).is_err() {
            drop(Box::from_raw(raw));
        }
    }
}

unsafe extern "system" fn message_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_RUN {
        let job = Box::from_raw(lparam.0 as *mut Job);
        job();
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe fn register_classes() {
    let icon = LoadIconW(Some(instance()), PCWSTR(1 as *const u16)).unwrap_or_default();
    let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
    let class = |name: PCWSTR, proc_: WNDPROC, style: WNDCLASS_STYLES, background: HBRUSH| {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style,
            lpfnWndProc: proc_,
            hInstance: instance(),
            hIcon: icon,
            hCursor: arrow,
            hbrBackground: background,
            lpszClassName: name,
            ..Default::default()
        };
        RegisterClassExW(&class);
    };
    let face = HBRUSH((COLOR_BTNFACE.0 + 1) as usize as *mut c_void);
    class(w!("VintrdpMessages"), Some(message_proc), WNDCLASS_STYLES(0), HBRUSH::default());
    class(w!("VintrdpManager"), Some(manager_proc), WNDCLASS_STYLES(0), face);
    class(w!("VintrdpTiles"), Some(tiles_proc), CS_DBLCLKS | CS_HREDRAW | CS_VREDRAW, HBRUSH::default());
    // No hCursor would be nicer, but WM_SETCURSOR below always decides.
    class(w!("VintrdpSession"), Some(session_proc), WNDCLASS_STYLES(0), HBRUSH::default());
}

// ---------------------------------------------------------------------------
// The session window

/// Set while a redraw is queued, so a burst of updates costs one message.
static REDRAW_QUEUED: AtomicBool = AtomicBool::new(false);
/// Sessions started from the manager go back to it when they end.
static BACK_TO_MANAGER: AtomicBool = AtomicBool::new(false);
/// The saved connection a session belongs to, for its tile thumbnail.
static THUMBNAIL_ID: OnceLock<String> = OnceLock::new();

struct SessionUi {
    hwnd: HWND,
    session: Option<Session>,
    cursor: HCURSOR,
    /// Keys we've sent as pressed, released again when focus is lost.
    held: Vec<Scancode>,
    /// Wheel movement not yet sent as a notch.
    wheel: i32,
    /// The next WM_CHAR is text with no key behind it (see WM_KEYDOWN).
    text_key: bool,
    last_thumbnail: Option<Instant>,
}

thread_local! {
    static SESSION: RefCell<Option<SessionUi>> = const { RefCell::new(None) };
}

fn with_session<R>(f: impl FnOnce(&mut SessionUi) -> R) -> Option<R> {
    SESSION.with(|s| s.borrow_mut().as_mut().map(f))
}

fn current_session() -> Option<Session> {
    with_session(|ui| ui.session.clone()).flatten()
}

fn finish(code: i32) -> ! {
    if BACK_TO_MANAGER.load(Ordering::Relaxed) {
        launch::relaunch();
    }
    std::process::exit(code);
}

fn save_thumbnail(background: bool) {
    let (Some(id), Some(session)) = (THUMBNAIL_ID.get(), current_session()) else { return };
    let Some((w, h, px)) = session.with_framebuffer(|w, h, px| (w, h, px.to_vec())) else { return };
    let work = move || {
        if let Err(e) = launch::save_thumbnail(id, w, h, &px) {
            eprintln!("vintrdp: could not save thumbnail: {e}");
        }
    };
    if background {
        std::thread::spawn(work);
    } else {
        work();
    }
}

struct WinFrontend {
    screen: (u32, u32),
}

impl Frontend for WinFrontend {
    fn notify(&self, event: Event) {
        if matches!(event, Event::Updated) && REDRAW_QUEUED.swap(true, Ordering::AcqRel) {
            return;
        }
        run_on_ui(move || handle_event(event));
    }

    fn screen_size(&self) -> (u32, u32) {
        self.screen
    }

    fn ask(&self, title: &str, message: &str, accept: &str, reject: &str) -> bool {
        let (tx, rx) = mpsc::channel();
        let (title, message, accept, reject) = (title.to_owned(), message.to_owned(), accept.to_owned(), reject.to_owned());
        run_on_ui(move || {
            let _ = tx.send(ask_dialog(&title, &message, &accept, &reject));
        });
        rx.recv().unwrap_or(false)
    }
}

fn handle_event(event: Event) {
    let Some(hwnd) = with_session(|ui| ui.hwnd) else { return };
    // SAFETY: on the UI thread, with our own window.
    unsafe {
        match event {
            Event::Resized { width, height } => {
                size_session_window(hwnd, width as i32, height as i32);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            Event::Updated => {
                REDRAW_QUEUED.store(false, Ordering::Release);
                let _ = InvalidateRect(Some(hwnd), None, false);
                let due = with_session(|ui| {
                    let due = ui.last_thumbnail.is_none_or(|t| t.elapsed() >= Duration::from_secs(15));
                    if due {
                        ui.last_thumbnail = Some(Instant::now());
                    }
                    due
                });
                if due == Some(true) {
                    save_thumbnail(true);
                }
            }
            Event::Cursor => {
                if let Some(session) = current_session() {
                    let cursor = make_cursor(&session.cursor());
                    with_session(|ui| ui.cursor = cursor);
                    let mut point = POINT::default();
                    let mut rect = RECT::default();
                    if GetCursorPos(&mut point).is_ok() && GetWindowRect(hwnd, &mut rect).is_ok() {
                        SetCursor(Some(cursor));
                    }
                }
            }
            Event::Title(t) => set_text(hwnd, &t),
            Event::Message(m) => eprintln!("vintrdp: {m}"),
            Event::Bell => {
                let _ = MessageBeep(MB_OK);
            }
            Event::Ended(outcome) => {
                if !outcome.user_quit && outcome.code != 0 {
                    alert("The remote session ended", &outcome.message);
                }
                finish(outcome.code);
            }
        }
    }
}

unsafe fn size_session_window(hwnd: HWND, width: i32, height: i32) {
    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
    let mut rect = RECT { left: 0, top: 0, right: width * dpi / 96, bottom: height * dpi / 96 };
    let _ = AdjustWindowRectExForDpi(&mut rect, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0), dpi as u32);
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    let x = (GetSystemMetrics(SM_CXSCREEN) - w).max(0) / 2;
    let y = (GetSystemMetrics(SM_CYSCREEN) - h).max(0) / 2;
    let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
}

unsafe fn start_session(config: Config, saved_id: Option<String>) {
    BACK_TO_MANAGER.store(saved_id.is_some(), Ordering::Relaxed);
    if let Some(id) = saved_id {
        let _ = THUMBNAIL_ID.set(id);
    }
    let title = wide(&config.window_title());
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("VintrdpSession"),
        PCWSTR(title.as_ptr()),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        800,
        600,
        None,
        None,
        Some(instance()),
        None,
    )
    .expect("session window");
    SESSION.with(|s| {
        *s.borrow_mut() = Some(SessionUi {
            hwnd,
            session: None,
            cursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            held: Vec::new(),
            wheel: 0,
            text_key: false,
            last_thumbnail: None,
        });
    });
    size_session_window(hwnd, config.width as i32, config.height as i32);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetFocus(Some(hwnd));

    let screen = (GetSystemMetrics(SM_CXSCREEN) as u32, GetSystemMetrics(SM_CYSCREEN) as u32);
    match Session::start(config, Arc::new(WinFrontend { screen })) {
        Ok(session) => {
            with_session(|ui| ui.session = Some(session));
        }
        Err(e) => {
            alert("Could not start the session", &e);
            finish(1);
        }
    }
}

/// Where the framebuffer goes in the client area, keeping its aspect ratio.
fn fit(client: &RECT, fb_w: usize, fb_h: usize) -> RECT {
    let (cw, ch) = (client.right as f64, client.bottom as f64);
    let scale = (cw / fb_w as f64).min(ch / fb_h as f64);
    let (w, h) = ((fb_w as f64 * scale) as i32, (fb_h as f64 * scale) as i32);
    let (x, y) = ((client.right - w) / 2, (client.bottom - h) / 2);
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

unsafe fn paint_session(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let black = HBRUSH(GetStockObject(BLACK_BRUSH).0);
    let drawn = current_session().and_then(|session| {
        session.with_framebuffer(|w, h, px| {
            let dest = fit(&client, w, h);
            // Letterbox bars
            for bar in [
                RECT { left: 0, top: 0, right: client.right, bottom: dest.top },
                RECT { left: 0, top: dest.bottom, right: client.right, bottom: client.bottom },
                RECT { left: 0, top: dest.top, right: dest.left, bottom: dest.bottom },
                RECT { left: dest.right, top: dest.top, right: client.right, bottom: dest.bottom },
            ] {
                FillRect(hdc, &bar, black);
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let (dw, dh) = (dest.right - dest.left, dest.bottom - dest.top);
            SetStretchBltMode(hdc, if dw < w as i32 { HALFTONE } else { COLORONCOLOR });
            // 0x00RRGGBB in a little-endian u32 is exactly a 32-bit BI_RGB DIB.
            StretchDIBits(hdc, dest.left, dest.top, dw, dh, 0, 0, w as i32, h as i32, Some(px.as_ptr().cast()), &info, DIB_RGB_COLORS, SRCCOPY);
        })
    });
    if drawn.is_none() {
        FillRect(hdc, &client, black);
    }
    let _ = EndPaint(hwnd, &ps);
}

/// A client-area position in session pixels.
unsafe fn to_session(hwnd: HWND, x: i32, y: i32) -> Option<(u16, u16)> {
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    current_session()?.with_framebuffer(|w, h, _| {
        let dest = fit(&client, w, h);
        let (dw, dh) = ((dest.right - dest.left).max(1) as f64, (dest.bottom - dest.top).max(1) as f64);
        let sx = ((x - dest.left) as f64 * w as f64 / dw).clamp(0.0, w as f64 - 1.0);
        let sy = ((y - dest.top) as f64 * h as f64 / dh).clamp(0.0, h as f64 - 1.0);
        (sx as u16, sy as u16)
    })
}

fn low_word(v: isize) -> i32 {
    v as i16 as i32
}

fn high_word(v: isize) -> i32 {
    (v >> 16) as i16 as i32
}

fn send_key(sc: Scancode, down: bool) {
    let Some(session) = current_session() else { return };
    with_session(|ui| {
        ui.held.retain(|&k| k != sc);
        if down {
            ui.held.push(sc);
        }
    });
    session.key(sc.0, sc.1, down);
}

/// Type one character-derived key, with Shift held exactly if needed.
fn type_char(sc: Scancode, shift: bool) {
    let held = with_session(|ui| ui.held.iter().any(|&k| k == scancode::LSHIFT || k == scancode::RSHIFT)).unwrap_or(false);
    if shift != held {
        send_key(scancode::LSHIFT, shift);
        if !shift {
            send_key(scancode::RSHIFT, false);
        }
    }
    send_key(sc, true);
    send_key(sc, false);
    if shift != held {
        send_key(scancode::LSHIFT, held);
    }
}

unsafe fn mouse_button(hwnd: HWND, button: MouseButton, down: bool, lparam: LPARAM) {
    if down {
        SetCapture(hwnd);
    } else {
        let _ = ReleaseCapture();
    }
    if let (Some(s), Some((x, y))) = (current_session(), to_session(hwnd, low_word(lparam.0), high_word(lparam.0))) {
        s.mouse_button(button, down, x, y);
    }
}

unsafe extern "system" fn session_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    const VK_PACKET: usize = 0xe7;
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            paint_session(hwnd);
            return LRESULT(0);
        }
        WM_SETCURSOR => {
            if (lparam.0 & 0xffff) as u32 == HTCLIENT {
                if let Some(cursor) = with_session(|ui| ui.cursor) {
                    SetCursor(Some(cursor));
                    return LRESULT(1);
                }
            }
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            if wparam.0 == VK_PACKET {
                // Text injected with no key behind it (remote-control apps,
                // on-screen keyboards): the character follows in WM_CHAR.
                if down {
                    with_session(|ui| ui.text_key = true);
                }
                return LRESULT(0);
            }
            if let Some(sc) = scancode::from_windows_lparam(lparam.0) {
                send_key(sc, down);
            }
            return LRESULT(0);
        }
        WM_CHAR => {
            if with_session(|ui| std::mem::take(&mut ui.text_key)) == Some(true) {
                if let Some((sc, shift)) = char::from_u32(wparam.0 as u32).and_then(scancode::from_us_char) {
                    type_char(sc, shift);
                }
            }
            return LRESULT(0);
        }
        WM_SYSCHAR => return LRESULT(0), // no menu-bar beep for Alt combinations
        WM_KILLFOCUS => {
            // Don't leave keys stuck down on the server.
            let held = with_session(|ui| std::mem::take(&mut ui.held)).unwrap_or_default();
            if let Some(s) = current_session() {
                for (code, ext) in held {
                    s.key(code, ext, false);
                }
            }
        }
        WM_MOUSEMOVE => {
            if let (Some(s), Some((x, y))) = (current_session(), to_session(hwnd, low_word(lparam.0), high_word(lparam.0))) {
                s.mouse_move(x, y);
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => mouse_button(hwnd, MouseButton::Left, true, lparam),
        WM_LBUTTONUP => mouse_button(hwnd, MouseButton::Left, false, lparam),
        WM_RBUTTONDOWN => mouse_button(hwnd, MouseButton::Right, true, lparam),
        WM_RBUTTONUP => mouse_button(hwnd, MouseButton::Right, false, lparam),
        WM_MBUTTONDOWN => mouse_button(hwnd, MouseButton::Middle, true, lparam),
        WM_MBUTTONUP => mouse_button(hwnd, MouseButton::Middle, false, lparam),
        WM_MOUSEWHEEL => {
            // Wheel messages carry screen coordinates.
            let mut p = POINT { x: low_word(lparam.0), y: high_word(lparam.0) };
            let _ = ScreenToClient(hwnd, &mut p);
            if let (Some(s), Some((x, y))) = (current_session(), to_session(hwnd, p.x, p.y)) {
                let delta = high_word(wparam.0 as isize);
                let mut total = with_session(|ui| ui.wheel).unwrap_or(0) + delta;
                while total.abs() >= WHEEL_DELTA as i32 {
                    s.wheel(total > 0, x, y);
                    total -= total.signum() * WHEEL_DELTA as i32;
                }
                with_session(|ui| ui.wheel = total);
            }
            return LRESULT(0);
        }
        WM_CLOSE => {
            save_thumbnail(false);
            let code = current_session().map_or(0, |s| {
                s.quit();
                s.wait(Duration::from_secs(3)).map_or(0, |o| if o.user_quit { 0 } else { o.code })
            });
            finish(code);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// An HBITMAP holding premultiplied BGRA, for AlphaBlend and cursors.
unsafe fn bitmap_from_rgba(width: usize, height: usize, rgba: &[u8]) -> HBITMAP {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let Ok(bitmap) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
        return HBITMAP::default();
    };
    let out = std::slice::from_raw_parts_mut(bits.cast::<u8>(), width * height * 4);
    for (dst, src) in out.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
        let a = src[3] as u32;
        dst[0] = (src[2] as u32 * a / 255) as u8;
        dst[1] = (src[1] as u32 * a / 255) as u8;
        dst[2] = (src[0] as u32 * a / 255) as u8;
        dst[3] = src[3];
    }
    bitmap
}

unsafe fn make_cursor(shape: &CursorShape) -> HCURSOR {
    let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
    let (w, h, hx, hy, pixels);
    let hidden = [0u8; 4];
    match shape {
        CursorShape::Default => return arrow,
        CursorShape::Hidden => (w, h, hx, hy, pixels) = (1, 1, 0, 0, &hidden[..]),
        CursorShape::Image(c) => (w, h, hx, hy, pixels) = (c.width, c.height, c.hot_x as u32, c.hot_y as u32, &c.rgba[..]),
    }
    let color = bitmap_from_rgba(w, h, pixels);
    let mask = CreateBitmap(w as i32, h as i32, 1, 1, None);
    let info = ICONINFO { fIcon: BOOL(0), xHotspot: hx, yHotspot: hy, hbmMask: mask, hbmColor: color };
    let cursor = CreateIconIndirect(&info).map(|i| HCURSOR(i.0)).unwrap_or(arrow);
    let _ = DeleteObject(color.into());
    let _ = DeleteObject(mask.into());
    cursor
}

// ---------------------------------------------------------------------------
// Connection manager

/// The manager window, for IsDialogMessage; 0 once it's closed.
static MGR_WINDOW: AtomicIsize = AtomicIsize::new(0);

const LOGICAL_W: i32 = 820;
const LOGICAL_H: i32 = 620;
const LEFT_W: i32 = 440;
const TILE_W: i32 = 132;
const TILE_H: i32 = 112;
const TILE_GAP: i32 = 10;

const ID_CONNECT: u16 = 1; // IDOK, so Enter presses it
const ID_SAVE: u16 = 10;
const ID_NEW: u16 = 11;
const ID_DELETE: u16 = 12;
const ID_ABOUT: u16 = 13;
const ID_WIDTH: u16 = 14;
const ID_HEIGHT: u16 = 15;

const SS_RIGHT: u32 = 2;
const TBM_GETPOS: u32 = WM_USER;
const TBM_SETPOS: u32 = WM_USER + 5;
const TBM_SETRANGE: u32 = WM_USER + 6;
const TBM_SETTICFREQ: u32 = WM_USER + 20;

#[derive(Clone, Copy, Default)]
struct Controls {
    tiles: HWND,
    name: HWND,
    computer: HWND,
    user: HWND,
    password: HWND,
    save_password: HWND,
    domain: HWND,
    slider: HWND,
    width: HWND,
    height: HWND,
    size_label: HWND,
    depth: HWND,
    protocol: HWND,
    picture: HWND,
}

/// One tile's picture, already scaled to fit.
struct TilePicture {
    bitmap: HBITMAP,
    width: i32,
    height: i32,
}

struct Manager {
    hwnd: HWND,
    store: Store,
    selected: Option<String>,
    c: Controls,
    /// Labels, buttons etc.: (window, x, y, w, h) in logical pixels, for DPI changes.
    layout: Vec<(HWND, i32, i32, i32, i32)>,
    font: HFONT,
    pictures: Vec<Option<TilePicture>>,
    scroll: i32,
    /// The colour of the connection in the form, kept while "Last screen" is chosen.
    color: usize,
    /// Set while we fill in the form, so EN_CHANGE doesn't react to ourselves.
    loading: bool,
    connecting: bool,
}

thread_local! {
    static MANAGER: RefCell<Option<Manager>> = const { RefCell::new(None) };
    static IN_SIZE_CHANGE: Cell<bool> = const { Cell::new(false) };
}

/// Careful: no message boxes or SendMessage to our own windows inside `f`,
/// or their window procedure would find the manager already borrowed.
fn with_mgr<R>(f: impl FnOnce(&mut Manager) -> R) -> Option<R> {
    MANAGER.with(|m| m.borrow_mut().as_mut().map(f))
}

fn controls() -> Controls {
    with_mgr(|m| m.c).unwrap_or_default()
}

unsafe fn dpi_of(hwnd: HWND) -> i32 {
    GetDpiForWindow(hwnd).max(96) as i32
}

unsafe fn make_font(dpi: i32) -> HFONT {
    let mut metrics = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
    let _ = SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS.0, metrics.cbSize, Some((&mut metrics as *mut NONCLIENTMETRICSW).cast()), 0, dpi as u32);
    CreateFontIndirectW(&metrics.lfMessageFont)
}

unsafe fn show_manager() {
    let dpi_guess = 96;
    let mut rect = RECT { left: 0, top: 0, right: LOGICAL_W, bottom: LOGICAL_H };
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
    let _ = AdjustWindowRectExForDpi(&mut rect, style, false, WINDOW_EX_STYLE(0), dpi_guess);
    let hwnd = CreateWindowExW(
        WS_EX_CONTROLPARENT,
        w!("VintrdpManager"),
        w!("vintrdp"),
        style,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        rect.right - rect.left,
        rect.bottom - rect.top,
        None,
        None,
        Some(instance()),
        None,
    )
    .expect("manager window");

    let mut layout = Vec::new();
    let mut child = |class: PCWSTR, text: &str, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, id: u16, x, y, w, h| -> HWND {
        let text = wide(text);
        let c = CreateWindowExW(
            ex,
            class,
            PCWSTR(text.as_ptr()),
            WS_CHILD | WS_VISIBLE | style,
            x,
            y,
            w,
            h,
            Some(hwnd),
            Some(HMENU(id as usize as *mut c_void)),
            Some(instance()),
            None,
        )
        .expect("control");
        layout.push((c, x, y, w, h));
        c
    };
    let edge = WS_EX_CLIENTEDGE;
    let plain = WINDOW_EX_STYLE(0);
    let edit_style = WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
    let (lx, fx, fw) = (LEFT_W + 30, LEFT_W + 110, LOGICAL_W - LEFT_W - 130);
    let row = |n: i32| 20 + n * 34;

    let tiles = child(w!("VintrdpTiles"), "", WS_VSCROLL | WS_TABSTOP, edge, 0, 20, 20, LEFT_W, LOGICAL_H - 80);

    // Labels first would put them in the tab order's way; statics don't take
    // focus, so creation order only matters for the fields.
    let mut label = |text: &str, r: i32| {
        child(w!("STATIC"), text, WINDOW_STYLE(SS_RIGHT), plain, 0, lx, row(r) + 4, 70, 20);
    };
    for (text, r) in [("Name", 0), ("Computer", 1), ("User name", 2), ("Password", 3), ("Domain", 5), ("Size", 6), ("Colours", 8), ("Protocol", 9), ("Picture", 10)] {
        label(text, r);
    }
    let name = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(0), fw, 24);
    let computer = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(1), fw, 24);
    let user = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(2), fw, 24);
    let password = child(w!("EDIT"), "", edit_style | WINDOW_STYLE(ES_PASSWORD as u32), edge, 0, fx, row(3), fw, 24);
    let save_password = child(w!("BUTTON"), "Save password", WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32), plain, 0, fx, row(4), fw, 22);
    let domain = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(5), fw, 24);
    let slider = child(w!("msctls_trackbar32"), "", WS_TABSTOP | WINDOW_STYLE(TBS_AUTOTICKS), plain, 0, fx, row(6), fw, 30);
    child(w!("STATIC"), "W", WINDOW_STYLE(SS_RIGHT), plain, 0, fx - 6, row(7) + 4, 18, 20);
    let number = edit_style | WINDOW_STYLE(ES_NUMBER as u32);
    let width = child(w!("EDIT"), "", number, edge, ID_WIDTH, fx + 16, row(7), 56, 24);
    child(w!("STATIC"), "H", WINDOW_STYLE(SS_RIGHT), plain, 0, fx + 74, row(7) + 4, 18, 20);
    let height = child(w!("EDIT"), "", number, edge, ID_HEIGHT, fx + 96, row(7), 56, 24);
    let size_text = child(w!("STATIC"), "", WINDOW_STYLE(0), plain, 0, fx + 160, row(7) + 4, fw - 160, 20);
    let combo_style = WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32);
    let depth = child(w!("COMBOBOX"), "", combo_style, plain, 0, fx, row(8), 120, 200);
    let protocol = child(w!("COMBOBOX"), "", combo_style, plain, 0, fx, row(9), fw, 200);
    let picture = child(w!("COMBOBOX"), "", combo_style, plain, 0, fx, row(10), 170, 300);

    let button_y = LOGICAL_H - 46;
    let push = WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32);
    child(w!("BUTTON"), "New", push, plain, ID_NEW, 20, button_y, 80, 30);
    child(w!("BUTTON"), "Delete", push, plain, ID_DELETE, 106, button_y, 80, 30);
    child(w!("BUTTON"), "About", push, plain, ID_ABOUT, 192, button_y, 80, 30);
    child(w!("BUTTON"), "Save", push, plain, ID_SAVE, LOGICAL_W - 208, button_y, 90, 30);
    child(w!("BUTTON"), "Connect", WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32), plain, ID_CONNECT, LOGICAL_W - 110, button_y, 90, 30);

    for (s, limit) in [(width, 5), (height, 5)] {
        send(s, EM_SETLIMITTEXT, limit, 0);
    }
    let last = (SIZES.len() - 1) as isize;
    send(slider, TBM_SETRANGE, 1, last << 16);
    send(slider, TBM_SETTICFREQ, 1, 0);
    let add = |combo: HWND, text: &str| {
        let text = wide(text);
        send(combo, CB_ADDSTRING, 0, text.as_ptr() as isize);
    };
    for d in DEPTHS {
        add(depth, &format!("{d} bit"));
    }
    for p in Protocol::ALL {
        add(protocol, p.label());
    }
    add(picture, "Last screen");
    for (name, _) in MONITORS {
        add(picture, &format!("{name} monitor"));
    }

    let c = Controls { tiles, name, computer, user, password, save_password, domain, slider, width, height, size_label: size_text, depth, protocol, picture };
    MANAGER.with(|m| {
        *m.borrow_mut() = Some(Manager {
            hwnd,
            store: Store::load(),
            selected: None,
            c,
            layout,
            font: HFONT::default(),
            pictures: Vec::new(),
            scroll: 0,
            color: 0,
            loading: false,
            connecting: false,
        });
    });
    apply_dpi(hwnd);

    MGR_WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
    refresh();
    let first = with_mgr(|m| m.store.connections.first().map(|c| c.id.clone())).flatten();
    match first {
        Some(id) => select(Some(id)),
        None => add_new(),
    }
    center(hwnd);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
}

unsafe fn center(hwnd: HWND) {
    let mut r = RECT::default();
    let _ = GetWindowRect(hwnd, &mut r);
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let x = (GetSystemMetrics(SM_CXSCREEN) - w).max(0) / 2;
    let y = (GetSystemMetrics(SM_CYSCREEN) - h).max(0) / 2;
    let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
}

/// Size everything for the window's current DPI.
unsafe fn apply_dpi(hwnd: HWND) {
    let dpi = dpi_of(hwnd);
    let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
    let mut rect = RECT { left: 0, top: 0, right: LOGICAL_W * dpi / 96, bottom: LOGICAL_H * dpi / 96 };
    let _ = AdjustWindowRectExForDpi(&mut rect, style, false, WINDOW_EX_STYLE(0), dpi as u32);
    let _ = SetWindowPos(hwnd, None, 0, 0, rect.right - rect.left, rect.bottom - rect.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);

    let font = make_font(dpi);
    let (old, children) = with_mgr(|m| (std::mem::replace(&mut m.font, font), m.layout.clone())).unwrap_or_default();
    for (child, x, y, w, h) in children {
        let _ = SetWindowPos(child, None, x * dpi / 96, y * dpi / 96, w * dpi / 96, h * dpi / 96, SWP_NOZORDER | SWP_NOACTIVATE);
        send(child, WM_SETFONT, font.0 as usize, 1);
    }
    if !old.is_invalid() {
        let _ = DeleteObject(old.into());
    }
    refresh();
}

/// Reduce an image to fit `max_w` x `max_h` with a box filter, keeping its
/// aspect ratio. Never enlarges.
fn scale_rgba(src: &Rgba, max_w: usize, max_h: usize) -> Rgba {
    let k = (max_w as f64 / src.width as f64).min(max_h as f64 / src.height as f64).min(1.0);
    let (tw, th) = (((src.width as f64 * k) as usize).max(1), ((src.height as f64 * k) as usize).max(1));
    let mut bytes = Vec::with_capacity(tw * th * 4);
    for ty in 0..th {
        let (y0, y1) = (ty * src.height / th, ((ty + 1) * src.height / th).max(ty * src.height / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * src.width / tw, ((tx + 1) * src.width / tw).max(tx * src.width / tw + 1));
            let (mut sum, mut n) = ([0u32; 4], 0u32);
            for y in y0..y1.min(src.height) {
                for x in x0..x1.min(src.width) {
                    let p = &src.bytes[(y * src.width + x) * 4..][..4];
                    let a = p[3] as u32;
                    // Weight colour by alpha, so transparent edges don't darken.
                    sum[0] += p[0] as u32 * a;
                    sum[1] += p[1] as u32 * a;
                    sum[2] += p[2] as u32 * a;
                    sum[3] += a;
                    n += 1;
                }
            }
            let a = sum[3].max(1);
            bytes.extend([(sum[0] / a) as u8, (sum[1] / a) as u8, (sum[2] / a) as u8, (sum[3] / n.max(1)) as u8]);
        }
    }
    Rgba { width: tw, height: th, bytes }
}

unsafe fn tile_picture(image: TileImage, color: usize, max_w: i32, max_h: i32) -> Option<TilePicture> {
    let decoded = match image {
        TileImage::Screenshot(path) => std::fs::read(path).ok().and_then(|b| decode_png(&b)),
        TileImage::Monitor(_) => None,
    };
    let source = decoded.or_else(|| decode_png(MONITORS[color % MONITORS.len()].1))?;
    let scaled = scale_rgba(&source, max_w as usize, max_h as usize);
    Some(TilePicture { bitmap: bitmap_from_rgba(scaled.width, scaled.height, &scaled.bytes), width: scaled.width as i32, height: scaled.height as i32 })
}

/// Rebuild the tiles from the store.
fn refresh() {
    // SAFETY: on the UI thread.
    unsafe {
        let Some(tiles) = with_mgr(|m| m.c.tiles) else { return };
        let dpi = dpi_of(tiles);
        let list: Vec<_> = with_mgr(|m| m.store.connections.iter().map(|c| (c.tile_image(), c.color)).collect()).unwrap_or_default();
        let pictures: Vec<_> = list
            .into_iter()
            .map(|(image, color)| tile_picture(image, color, (TILE_W - 16) * dpi / 96, (TILE_H - 40) * dpi / 96))
            .collect();
        let old = with_mgr(|m| std::mem::replace(&mut m.pictures, pictures)).unwrap_or_default();
        for p in old.into_iter().flatten() {
            let _ = DeleteObject(p.bitmap.into());
        }
        update_scrollbar();
        let _ = InvalidateRect(Some(tiles), None, false);
    }
}

/// (columns, tile w, tile h, gap) for the tile panel, in physical pixels.
unsafe fn tile_metrics(tiles: HWND) -> (i32, i32, i32, i32) {
    let dpi = dpi_of(tiles);
    let (w, h, gap) = (TILE_W * dpi / 96, TILE_H * dpi / 96, TILE_GAP * dpi / 96);
    let mut client = RECT::default();
    let _ = GetClientRect(tiles, &mut client);
    (((client.right - gap) / (w + gap)).max(1), w, h, gap)
}

unsafe fn update_scrollbar() {
    let Some((tiles, count, scroll)) = with_mgr(|m| (m.c.tiles, m.store.connections.len() as i32, m.scroll)) else { return };
    let (columns, _, h, gap) = tile_metrics(tiles);
    let mut client = RECT::default();
    let _ = GetClientRect(tiles, &mut client);
    let content = gap + (count + columns - 1) / columns * (h + gap);
    let info = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        nMin: 0,
        nMax: content,
        nPage: client.bottom as u32,
        nPos: scroll,
        nTrackPos: 0,
    };
    SetScrollInfo(tiles, SB_VERT, &info, true);
    let limit = (content - client.bottom).max(0);
    with_mgr(|m| m.scroll = m.scroll.min(limit));
}

unsafe fn set_scroll(tiles: HWND, pos: i32) {
    with_mgr(|m| m.scroll = pos.max(0));
    update_scrollbar();
    let _ = InvalidateRect(Some(tiles), None, false);
}

unsafe fn paint_tiles(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    // Draw off-screen so scrolling doesn't flicker.
    let mem = CreateCompatibleDC(Some(hdc));
    let canvas = CreateCompatibleBitmap(hdc, client.right, client.bottom);
    let old_canvas = SelectObject(mem, canvas.into());
    FillRect(mem, &client, GetSysColorBrush(COLOR_WINDOW));

    let (columns, tw, th, gap) = tile_metrics(hwnd);
    let radius = 16 * dpi_of(hwnd) / 96;
    let picture_dc = CreateCompatibleDC(Some(hdc));
    let pen = SelectObject(mem, GetStockObject(NULL_PEN));
    let _ = SetBkMode(mem, TRANSPARENT);
    with_mgr(|m| {
        let old_font = SelectObject(mem, m.font.into());
        let selected = m.selected.clone();
        for (i, c) in m.store.connections.iter().enumerate() {
            let (col, row) = (i as i32 % columns, i as i32 / columns);
            let (x, y) = (gap + col * (tw + gap), gap + row * (th + gap) - m.scroll);
            if y + th < 0 || y > client.bottom {
                continue;
            }
            let chosen = selected.as_deref() == Some(c.id.as_str());
            let fill = if chosen { GetSysColorBrush(COLOR_HIGHLIGHT) } else { GetSysColorBrush(COLOR_BTNFACE) };
            let old_brush = SelectObject(mem, fill.into());
            let _ = RoundRect(mem, x, y, x + tw, y + th, radius, radius);
            SelectObject(mem, old_brush);

            if let Some(Some(p)) = m.pictures.get(i) {
                let old = SelectObject(picture_dc, p.bitmap.into());
                let area_h = th - 40 * tw / TILE_W;
                let (px, py) = (x + (tw - p.width) / 2, y + 8 * tw / TILE_W + (area_h - p.height) / 2);
                let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
                let _ = AlphaBlend(mem, px, py, p.width, p.height, picture_dc, 0, 0, p.width, p.height, blend);
                SelectObject(picture_dc, old);
            }

            let colour = if chosen { COLOR_HIGHLIGHTTEXT } else { COLOR_BTNTEXT };
            SetTextColor(mem, COLORREF(GetSysColor(colour)));
            let mut name: Vec<u16> = c.display_name().encode_utf16().collect();
            let mut text_rect = RECT { left: x + 6, top: y + th - 28 * tw / TILE_W, right: x + tw - 6, bottom: y + th - 6 };
            DrawTextW(mem, &mut name, &mut text_rect, DT_CENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER | DT_NOPREFIX);
        }
        SelectObject(mem, old_font);
    });
    SelectObject(mem, pen);
    let _ = DeleteDC(picture_dc);

    let _ = BitBlt(hdc, 0, 0, client.right, client.bottom, Some(mem), 0, 0, SRCCOPY);
    SelectObject(mem, old_canvas);
    let _ = DeleteObject(canvas.into());
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

/// The saved connection under a point in the tile panel.
unsafe fn tile_at(hwnd: HWND, x: i32, y: i32) -> Option<String> {
    let (columns, tw, th, gap) = tile_metrics(hwnd);
    let scroll = with_mgr(|m| m.scroll)?;
    let (col, row) = ((x - gap) / (tw + gap), (y + scroll - gap) / (th + gap));
    let inside = x >= gap && (x - gap) % (tw + gap) < tw && (y + scroll - gap) % (th + gap) < th && col < columns;
    if !inside {
        return None;
    }
    let index = (row * columns + col) as usize;
    with_mgr(|m| m.store.connections.get(index).map(|c| c.id.clone())).flatten()
}

unsafe extern "system" fn tiles_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            paint_tiles(hwnd);
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            let _ = SetFocus(Some(hwnd));
            if let Some(id) = tile_at(hwnd, low_word(lparam.0), high_word(lparam.0)) {
                select(Some(id));
                if msg == WM_LBUTTONDBLCLK {
                    connect();
                }
            }
            return LRESULT(0);
        }
        WM_VSCROLL => {
            let info_mask = SCROLLINFO { cbSize: size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL, ..Default::default() };
            let mut info = info_mask;
            let _ = GetScrollInfo(hwnd, SB_VERT, &mut info);
            let step = TILE_H * dpi_of(hwnd) / 96;
            let pos = match SCROLLBAR_COMMAND((wparam.0 & 0xffff) as i32) {
                SB_LINEUP => info.nPos - step / 2,
                SB_LINEDOWN => info.nPos + step / 2,
                SB_PAGEUP => info.nPos - info.nPage as i32,
                SB_PAGEDOWN => info.nPos + info.nPage as i32,
                SB_THUMBTRACK | SB_THUMBPOSITION => info.nTrackPos,
                _ => info.nPos,
            };
            set_scroll(hwnd, pos);
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = high_word(wparam.0 as isize);
            let scroll = with_mgr(|m| m.scroll).unwrap_or(0);
            set_scroll(hwnd, scroll - delta * (TILE_H * dpi_of(hwnd) / 96) / (2 * WHEEL_DELTA as i32));
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Show `id` in the form and highlight its tile.
fn select(id: Option<String>) {
    let saved = with_mgr(|m| {
        m.selected = id.clone();
        id.as_deref().and_then(|id| m.store.get(id).cloned())
    })
    .flatten();
    load_form(&saved.map(|s| s.to_form()).unwrap_or_default());
    let tiles = controls().tiles;
    // SAFETY: our own window.
    unsafe {
        let _ = InvalidateRect(Some(tiles), None, false);
    }
}

fn load_form(form: &Form) {
    let c = controls();
    with_mgr(|m| {
        m.loading = true;
        m.color = form.color;
    });
    // SAFETY: on the UI thread, with our own controls.
    unsafe {
        set_text(c.name, &form.name);
        set_text(c.computer, &form.computer);
        set_text(c.user, &form.username);
        set_text(c.password, &form.password);
        set_text(c.domain, &form.domain);
        set_text(c.width, &form.width);
        set_text(c.height, &form.height);
        send(c.depth, CB_SETCURSEL, form.depth_index, 0);
        let protocol = Protocol::ALL.iter().position(|&p| p == form.protocol).unwrap_or(0);
        send(c.protocol, CB_SETCURSEL, protocol, 0);
        let picture = match form.picture {
            Picture::Screen => 0,
            Picture::Monitor => form.color % MONITORS.len() + 1,
        };
        send(c.picture, CB_SETCURSEL, picture, 0);
        send(c.save_password, BM_SETCHECK, form.save_password as usize, 0);
    }
    with_mgr(|m| m.loading = false);
    size_changed();
}

fn read_form() -> Form {
    let c = controls();
    let color = with_mgr(|m| m.color).unwrap_or(0);
    // SAFETY: on the UI thread, with our own controls.
    unsafe {
        let picture = send(c.picture, CB_GETCURSEL, 0, 0);
        Form {
            name: text_of(c.name),
            computer: text_of(c.computer),
            username: text_of(c.user),
            password: text_of(c.password),
            domain: text_of(c.domain),
            width: text_of(c.width),
            height: text_of(c.height),
            depth_index: send(c.depth, CB_GETCURSEL, 0, 0).max(0) as usize,
            protocol: Protocol::ALL[send(c.protocol, CB_GETCURSEL, 0, 0).clamp(0, 2) as usize],
            save_password: send(c.save_password, BM_GETCHECK, 0, 0) != 0,
            picture: if picture <= 0 { Picture::Screen } else { Picture::Monitor },
            color: if picture > 0 { picture as usize - 1 } else { color },
        }
    }
}

/// Width/height were edited: move the slider to match, or say "Custom".
fn size_changed() {
    if IN_SIZE_CHANGE.replace(true) {
        return;
    }
    let c = controls();
    // SAFETY: on the UI thread, with our own controls.
    unsafe {
        let (w, h) = (text_of(c.width), text_of(c.height));
        if let Some(i) = size_index(&w, &h) {
            send(c.slider, TBM_SETPOS, 1, i as isize);
        }
        set_text(c.size_label, &size_label(&w, &h));
    }
    IN_SIZE_CHANGE.set(false);
}

/// The slider moved: fill in width and height.
fn slider_changed() {
    let c = controls();
    // SAFETY: on the UI thread, with our own controls.
    unsafe {
        let i = (send(c.slider, TBM_GETPOS, 0, 0).max(0) as usize).min(SIZES.len() - 1);
        let (w, h, _) = SIZES[i];
        set_text(c.width, &w.to_string());
        set_text(c.height, &h.to_string());
    }
    size_changed();
}

fn report(result: std::io::Result<()>) {
    if let Err(e) = result {
        alert("Could not update saved connections", &e.to_string());
    }
}

/// "New": add a tile straight away and start editing it.
fn add_new() {
    match with_mgr(|m| m.store.new_blank()) {
        Some(Ok(blank)) => {
            refresh();
            select(Some(blank.id));
        }
        Some(Err(e)) => report(Err(e)),
        None => {}
    }
    let name = controls().name;
    // SAFETY: our own control.
    unsafe {
        let _ = SetFocus(Some(name));
        send(name, EM_SETSEL, 0, -1);
    }
}

/// Validate and save the form. Returns the config to connect with.
fn save() -> Option<(Config, String)> {
    let form = read_form();
    let id = with_mgr(|m| m.selected.clone()).flatten();
    let checked = with_mgr(|m| m.store.validate(&form, id.as_deref()))?;
    let config = match checked {
        Ok(c) => c,
        Err(e) => {
            alert("Check the connection settings", &e);
            return None;
        }
    };
    let saved = with_mgr(|m| {
        let saved = remember(&mut m.store, &form, &config, id);
        m.selected = Some(saved.id.clone());
        saved
    })?;
    refresh();
    Some((config, saved.id))
}

fn connect() {
    let Some((config, id)) = save() else { return };
    let hwnd = with_mgr(|m| {
        m.connecting = true;
        m.hwnd
    });
    MGR_WINDOW.store(0, Ordering::Relaxed);
    // SAFETY: on the UI thread.
    unsafe {
        if let Some(hwnd) = hwnd {
            let _ = DestroyWindow(hwnd);
        }
        start_session(config, Some(id));
    }
}

fn delete() {
    let Some(id) = with_mgr(|m| m.selected.clone()).flatten() else { return };
    let shown = with_mgr(|m| m.store.get(&id).map(|c| c.display_name().to_string())).flatten().unwrap_or_default();
    let question = format!("Delete \u{201c}{shown}\u{201d}?\n\nIts settings, picture and saved password will be removed.");
    if !ask_dialog("vintrdp", &question, "Delete", "Cancel") {
        return;
    }
    if let Some(result) = with_mgr(|m| m.store.remove(&id)) {
        report(result);
    }
    let first = with_mgr(|m| m.store.connections.first().map(|c| c.id.clone())).flatten();
    refresh();
    match first {
        Some(first) => select(Some(first)),
        None => add_new(),
    }
}

unsafe extern "system" fn manager_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let (id, code) = ((wparam.0 & 0xffff) as u16, ((wparam.0 >> 16) & 0xffff) as u32);
            match (id, code) {
                (ID_CONNECT, _) => connect(),
                (ID_SAVE, _) => {
                    save();
                }
                (ID_NEW, _) => add_new(),
                (ID_DELETE, _) => delete(),
                (ID_ABOUT, _) => show_about(hwnd),
                (ID_WIDTH | ID_HEIGHT, EN_CHANGE) => {
                    if with_mgr(|m| m.loading) == Some(false) {
                        size_changed();
                    }
                }
                _ => {}
            }
            return LRESULT(0);
        }
        WM_HSCROLL => {
            slider_changed();
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            apply_dpi(hwnd);
            return LRESULT(0);
        }
        WM_CTLCOLORSTATIC => {
            // Plain dialog colours for labels.
            let hdc = HDC(wparam.0 as *mut c_void);
            let _ = SetBkMode(hdc, TRANSPARENT);
            return LRESULT(GetSysColorBrush(COLOR_BTNFACE).0 as isize);
        }
        WM_DESTROY => {
            if with_mgr(|m| m.connecting) != Some(true) {
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
