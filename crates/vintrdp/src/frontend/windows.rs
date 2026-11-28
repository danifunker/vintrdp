//! Windows front-end: plain Win32.
//!
//! Everything runs on the main thread's message loop. The rdesktop thread
//! hands work over by posting boxed closures to a hidden message window. The
//! connection manager mirrors the macOS one: tiles on the left, details on the
//! right.
//!
//! The same binary runs on Windows NT 4.0 SP0: everything newer is looked up
//! at run time (`win32::proc`) and has a fallback, so nothing here may import
//! a function NT 4.0 lacks.

#![allow(unsafe_op_in_unsafe_fn)]

mod theme;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, S_OK, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VIRTUAL_KEY, VK_CANCEL, VK_CAPITAL, VK_CONTROL, VK_MENU, VK_NUMLOCK, VK_PAUSE, VK_SCROLL};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, GPFIDL_DEFAULT, GPFIDL_FLAGS, HDROP, SHBrowseForFolderW, SHGetPathFromIDListW, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW};
use windows::core::{BOOL, HRESULT, PCWSTR, PWSTR, w};

use vintrdp_core::scancode::{self, Scancode};
use vintrdp_core::{Config, CursorShape, Event, Frontend, MouseButton, Session};

use crate::launch::{self, DEPTHS, Form, MONITORS, Picture, ServerKind, Rgba, SIZES, Store, TileImage, decode_png, remember, size_index, size_label};
use crate::win32;

pub fn run(config: Option<Config>) -> i32 {
    // SAFETY: the rest of this file runs on this thread, as Win32 requires.
    unsafe {
        init_common_controls();
        theme::refresh();
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

/// Register the trackbar class. comctl32 before 4.70 has no
/// InitCommonControlsEx, and ICC_STANDARD_CLASSES is a 6.0 flag that older
/// versions may refuse; InitCommonControls does the job on any version.
unsafe fn init_common_controls() {
    type InitEx = unsafe extern "system" fn(*const INITCOMMONCONTROLSEX) -> BOOL;
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES | ICC_BAR_CLASSES,
    };
    let done = win32::proc::<InitEx>("comctl32.dll", "InitCommonControlsEx").is_some_and(|init| init(&controls).as_bool());
    if !done {
        InitCommonControls();
    }
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

type TaskDialogIndirect = unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut BOOL) -> HRESULT;

/// comctl32 6.0's task dialog (Windows Vista on), or None.
fn task_dialog() -> Option<TaskDialogIndirect> {
    // SAFETY: comctl32's export with its prototype.
    unsafe { win32::proc("comctl32.dll", "TaskDialogIndirect") }
}

/// A two-button question with our own button labels.
fn ask_dialog(title: &str, message: &str, accept: &str, reject: &str) -> bool {
    let Some(task_dialog) = task_dialog() else {
        // Before Vista: Yes and No, with what they mean spelled out.
        let (title, message) = (wide(title), wide(&format!("{message}\n\nYes: {accept}\nNo: {reject}")));
        // SAFETY: valid NUL-terminated strings.
        return unsafe { MessageBoxW(None, PCWSTR(message.as_ptr()), PCWSTR(title.as_ptr()), MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) } == IDYES;
    };
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
    unsafe { task_dialog(&config, &mut pressed, std::ptr::null_mut(), std::ptr::null_mut()).is_ok() && pressed == 100 }
}

unsafe extern "system" fn about_callback(_: HWND, msg: TASKDIALOG_NOTIFICATIONS, _: WPARAM, lparam: LPARAM, _: isize) -> HRESULT {
    if msg == TDN_HYPERLINK_CLICKED {
        ShellExecuteW(None, w!("open"), PCWSTR(lparam.0 as *const u16), None, None, SW_SHOWNORMAL);
    }
    S_OK
}

fn show_about(owner: HWND) {
    let Some(task_dialog) = task_dialog() else {
        // Before Vista: the same text, with the links written out.
        let body = wide(&format!(
            "vintrdp {}\n\n{}\n{repo}\n\n{}\n{rdesktop}\n\nFree software under the GNU GPL v3 or later:\n{gpl}\nIt comes with ABSOLUTELY NO WARRANTY.",
            launch::VERSION,
            launch::DESCRIPTION,
            launch::FORK_CREDIT,
            repo = launch::REPO_URL,
            rdesktop = launch::RDESKTOP_URL,
            gpl = launch::GPL_URL,
        ));
        // SAFETY: a valid NUL-terminated string.
        unsafe { MessageBoxW(Some(owner), PCWSTR(body.as_ptr()), w!("About vintrdp"), MB_OK | MB_ICONINFORMATION) };
        return;
    };
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
    let _ = unsafe { task_dialog(&config, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
}

// ---------------------------------------------------------------------------
// Getting work from the rdesktop thread onto the UI thread

const WM_RUN: u32 = WM_APP + 1;
type Job = Box<dyn FnOnce() + Send>;

/// The hidden window jobs are posted to.
static MESSAGE_WINDOW: AtomicIsize = AtomicIsize::new(0);

unsafe fn make_message_window() {
    let make = |parent: Option<HWND>| CreateWindowExW(WINDOW_EX_STYLE(0), w!("VintrdpMessages"), None, WINDOW_STYLE(0), 0, 0, 0, 0, parent, None, Some(instance()), None);
    // Message-only windows arrived in Windows 2000; before that, a top-level
    // window that is never shown does the same job.
    let hwnd = make(Some(HWND_MESSAGE)).or_else(|_| make(None)).expect("message window");
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
    class(w!("VintrdpBar"), Some(bar_proc), WNDCLASS_STYLES(0), face);
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
    /// The first half of a surrogate pair from WM_CHAR.
    high_surrogate: Option<u16>,
    last_thumbnail: Option<Instant>,
    /// The session pixel at the client area's top left while the session is drawn
    /// at 1:1 and doesn't fit (see `place`), set by the scroll bars.
    scroll: (i32, i32),
    /// In full screen, where the window was before (see `set_fullscreen`).
    restore: Option<RECT>,
    /// The bar that drops down from the top of the screen in full screen.
    bar: Option<HWND>,
}

thread_local! {
    static SESSION: RefCell<Option<SessionUi>> = const { RefCell::new(None) };
}

fn with_session<R>(f: impl FnOnce(&mut SessionUi) -> R) -> Option<R> {
    SESSION.with(|s| s.borrow_mut().as_mut().map(f))
}

/// Give the server our Caps, Num and Scroll Lock; they may have changed while
/// another window had the keyboard.
fn sync_locks(s: &Session) {
    // SAFETY: plain state queries.
    let on = |key: VIRTUAL_KEY| unsafe { GetKeyState(key.0 as i32) } & 1 != 0;
    s.sync_locks(on(VK_CAPITAL), on(VK_NUMLOCK), on(VK_SCROLL));
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
            Event::Clipboard(text) => set_clipboard(hwnd, &text),
            Event::ClipboardFiles(paths) => set_clipboard_files(hwnd, &paths),
            Event::WarpPointer { x, y } => warp_pointer(hwnd, x, y),
            Event::Ended(outcome) => {
                if !outcome.user_quit && outcome.code != 0 {
                    alert("The remote session ended", &outcome.message);
                }
                finish(outcome.code);
            }
        }
    }
}

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;

/// Replace the clipboard with `items`, each a format and its bytes.
unsafe fn put_clipboard(hwnd: HWND, items: &[(u32, Vec<u8>)]) {
    if OpenClipboard(Some(hwnd)).is_err() {
        return;
    }
    let _ = EmptyClipboard();
    for (format, bytes) in items {
        if let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) {
            let target = GlobalLock(memory).cast::<u8>();
            if !target.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
                let _ = GlobalUnlock(memory);
                // The clipboard owns the memory from here.
                let _ = SetClipboardData(*format, Some(HANDLE(memory.0)));
            }
        }
    }
    let _ = CloseClipboard();
}

unsafe fn set_clipboard(hwnd: HWND, text: &str) {
    let bytes = wide(&text.replace('\n', "\r\n")).into_iter().flat_map(u16::to_le_bytes).collect();
    put_clipboard(hwnd, &[(CF_UNICODETEXT, bytes)]);
}

/// Put files on the clipboard as Explorer copies them: a DROPFILES list
/// (CF_HDROP) and a "copy, don't move" drop effect.
unsafe fn set_clipboard_files(hwnd: HWND, paths: &[PathBuf]) {
    let mut drop = Vec::new();
    drop.extend(20u32.to_le_bytes()); // where the names start
    drop.extend([0u8; 12]); // drop point and fNC
    drop.extend(1u32.to_le_bytes()); // fWide
    for path in paths {
        drop.extend(wide(&path.to_string_lossy()).into_iter().flat_map(u16::to_le_bytes));
    }
    drop.extend([0, 0]);
    let effect = RegisterClipboardFormatW(w!("Preferred DropEffect"));
    const DROPEFFECT_COPY: u32 = 1;
    put_clipboard(hwnd, &[(CF_HDROP, drop), (effect, DROPEFFECT_COPY.to_le_bytes().to_vec())]);
}

unsafe fn get_clipboard(hwnd: HWND) -> Option<String> {
    OpenClipboard(Some(hwnd)).ok()?;
    let text = GetClipboardData(CF_UNICODETEXT).ok().and_then(|handle| {
        let memory = HGLOBAL(handle.0);
        let source = GlobalLock(memory).cast::<u16>();
        if source.is_null() {
            return None;
        }
        let len = (0..).take_while(|&i| *source.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(source, len));
        let _ = GlobalUnlock(memory);
        Some(text)
    });
    let _ = CloseClipboard();
    text
}

/// The window that shows the whole session, kept within the work area (the screen less
/// the taskbar). A session as big as the screen doesn't fit with the window's frame; it
/// is then shown at 1:1 with scroll bars (see `place`).
unsafe fn size_session_window(hwnd: HWND, width: i32, height: i32) {
    if is_fullscreen() {
        update_session_scroll(hwnd);
        return;
    }
    let dpi = dpi_of(hwnd);
    let mut rect = RECT { left: 0, top: 0, right: width * dpi / 96, bottom: height * dpi / 96 };
    adjust_window_rect(&mut rect, WS_OVERLAPPEDWINDOW, dpi);
    let mut work = RECT::default();
    let found = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some((&mut work as *mut RECT).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_ok();
    if !found || work.right <= work.left || work.bottom <= work.top {
        work = RECT { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) };
    }
    let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);
    let (w, h) = ((rect.right - rect.left).min(work_w), (rect.bottom - rect.top).min(work_h));
    let (x, y) = (work.left + (work_w - w) / 2, work.top + (work_h - h) / 2);
    let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    update_session_scroll(hwnd);
}

unsafe fn start_session(mut config: Config, saved_id: Option<String>) {
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
    theme::title_bar(hwnd);
    SESSION.with(|s| {
        *s.borrow_mut() = Some(SessionUi {
            hwnd,
            session: None,
            cursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            held: Vec::new(),
            wheel: 0,
            text_key: false,
            high_surrogate: None,
            last_thumbnail: None,
            scroll: (0, 0),
            restore: None,
            bar: None,
        });
    });
    let menu = GetSystemMenu(hwnd, false);
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(menu, MF_STRING, FULLSCREEN_COMMAND, w!("&Full screen\tCtrl+Alt+Break"));
    size_session_window(hwnd, config.width as i32, config.height as i32);
    if config.fullscreen {
        // The session the size of the screen, shown at 1:1.
        let screen = monitor_rect(hwnd);
        (config.width, config.height) = ((screen.right - screen.left) as u32, (screen.bottom - screen.top) as u32);
        set_fullscreen(hwnd, true);
    }
    DragAcceptFiles(hwnd, true);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetFocus(Some(hwnd));

    let screen = (GetSystemMetrics(SM_CXSCREEN) as u32, GetSystemMetrics(SM_CYSCREEN) as u32);
    match Session::start(config, Arc::new(WinFrontend { screen })) {
        Ok(session) => {
            sync_locks(&session);
            with_session(|ui| ui.session = Some(session));
        }
        Err(e) => {
            alert("Could not start the session", &e);
            finish(1);
        }
    }
}

/// Where the framebuffer goes in the client area. When the client area holds all of it,
/// it is scaled up to fill it, keeping its aspect ratio, unless `scale_up` is false (full
/// screen), which centres it at 1:1. When it doesn't, it is drawn at
/// 1:1, from the session pixel `scroll` along each direction that doesn't fit (centred
/// along one that does), and the window clips it.
///
/// It is never shrunk. A session as big as the screen leaves a client area a little
/// smaller than itself, and Windows' stretch, at least NT 4.0's on ARM, shrinks by
/// dropping whole pixel columns, which takes thin strokes out of text.
fn place(client: &RECT, fb_w: usize, fb_h: usize, scroll: (i32, i32), scale_up: bool) -> RECT {
    let (cw, ch, w, h) = (client.right, client.bottom, fb_w as i32, fb_h as i32);
    if scale_up && cw >= w && ch >= h {
        let scale = (cw as f64 / w as f64).min(ch as f64 / h as f64);
        let (dw, dh) = ((w as f64 * scale) as i32, (h as f64 * scale) as i32);
        let (x, y) = ((cw - dw) / 2, (ch - dh) / 2);
        return RECT { left: x, top: y, right: x + dw, bottom: y + dh };
    }
    let start = |room: i32, size: i32, scroll: i32| if room >= size { (room - size) / 2 } else { -scroll.clamp(0, size - room) };
    let (x, y) = (start(cw, w, scroll.0), start(ch, h, scroll.1));
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

fn session_scroll() -> (i32, i32) {
    with_session(|ui| ui.scroll).unwrap_or_default()
}

/// Scroll bars for the directions the session doesn't fit in (see `place`); hidden when
/// it fits. Showing one narrows the client area, which can call for the other, so it
/// goes round until the client area stays put.
unsafe fn update_session_scroll(hwnd: HWND) {
    thread_local!(static BUSY: Cell<bool> = const { Cell::new(false) });
    // SetScrollInfo sends WM_SIZE, which comes back here.
    if BUSY.with(|b| b.replace(true)) {
        return;
    }
    let size = current_session().and_then(|s| s.with_framebuffer(|w, h, _| (w as i32, h as i32)));
    let mut last = None;
    for _ in 0..4 {
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        if last == Some((client.right, client.bottom)) {
            break;
        }
        last = Some((client.right, client.bottom));
        let (w, h) = size.unwrap_or((client.right, client.bottom));
        let fits = client.right >= w && client.bottom >= h;
        let (mut sx, mut sy) = session_scroll();
        for (bar, room, size, pos) in [(SB_HORZ, client.right, w, &mut sx), (SB_VERT, client.bottom, h, &mut sy)] {
            let room = if fits { size } else { room };
            *pos = (*pos).clamp(0, (size - room).max(0));
            let info = SCROLLINFO {
                cbSize: size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                nMin: 0,
                nMax: size - 1,
                nPage: room.max(0) as u32,
                nPos: *pos,
                nTrackPos: 0,
            };
            SetScrollInfo(hwnd, bar, &info, true);
        }
        with_session(|ui| ui.scroll = (sx, sy));
    }
    BUSY.with(|b| b.set(false));
}

// ---------------------------------------------------------------------------
// Full screen

/// The session window's system-menu item for full screen.
const FULLSCREEN_COMMAND: usize = 0x10;
/// The bar's buttons.
const BAR_MINIMISE: usize = 1;
const BAR_WINDOW: usize = 2;
const BAR_CLOSE: usize = 3;
/// The bar's size at 96 DPI, and its buttons': (id, label, width).
const BAR_W: i32 = 520;
const BAR_H: i32 = 30;
const BAR_BUTTONS: [(usize, &str, i32); 3] = [(BAR_MINIMISE, "Minimise", 80), (BAR_WINDOW, "Leave full screen", 120), (BAR_CLOSE, "Close", 70)];
const BAR_TIMER: usize = 1;

fn is_fullscreen() -> bool {
    with_session(|ui| ui.restore.is_some()).unwrap_or(false)
}

/// The monitor the window is on. NT 4.0 has one screen and no MonitorFromWindow.
unsafe fn monitor_rect(hwnd: HWND) -> RECT {
    type FromWindow = unsafe extern "system" fn(HWND, u32) -> HMONITOR;
    type Info = unsafe extern "system" fn(HMONITOR, *mut MONITORINFO) -> BOOL;
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    let from = win32::proc::<FromWindow>("user32.dll", "MonitorFromWindow");
    let info = win32::proc::<Info>("user32.dll", "GetMonitorInfoW");
    if let (Some(from), Some(info)) = (from, info) {
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if info(from(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            return mi.rcMonitor;
        }
    }
    RECT { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) }
}

/// Full screen: the window without its frame, over the whole monitor and the taskbar.
/// The session keeps its size; one smaller than the screen is centred at 1:1 (see
/// `place`). Leaving it puts the window back where it was.
unsafe fn set_fullscreen(hwnd: HWND, on: bool) {
    if on == is_fullscreen() {
        return;
    }
    // Scroll bars come and go with the size (update_session_scroll).
    let bars = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32) & (WS_HSCROLL | WS_VSCROLL);
    let (style, after, rect) = if on {
        if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        with_session(|ui| ui.restore = Some(rect));
        (WS_POPUP, HWND_TOPMOST, monitor_rect(hwnd))
    } else {
        hide_bar();
        let rect = with_session(|ui| ui.restore.take()).flatten().unwrap_or_default();
        (WS_OVERLAPPEDWINDOW, HWND_NOTOPMOST, rect)
    };
    SetWindowLongPtrW(hwnd, GWL_STYLE, (style | WS_VISIBLE | bars).0 as _);
    let flags = SWP_FRAMECHANGED | SWP_SHOWWINDOW;
    let _ = SetWindowPos(hwnd, Some(after), rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top, flags);
    update_session_scroll(hwnd);
    let _ = InvalidateRect(Some(hwnd), None, false);
    let _ = SetForegroundWindow(hwnd);
    let _ = SetFocus(Some(hwnd));
}

/// In full screen the window stays over the taskbar only while it's the active one,
/// so switching to another program (Alt+Tab) shows that program. The bar counts as ours.
unsafe fn fullscreen_activated(hwnd: HWND, active: bool, other: HWND) {
    let bar = with_session(|ui| ui.bar).flatten();
    if !is_fullscreen() || (!active && bar == Some(other)) {
        return;
    }
    let after = if active { HWND_TOPMOST } else { HWND_NOTOPMOST };
    let _ = SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
}

/// The bar, shown when the pointer reaches the top of the screen in full screen: the
/// connection's name, Minimise, Leave full screen and Close. It goes once the pointer
/// leaves it.
unsafe fn show_bar(session: HWND) {
    let bar = match with_session(|ui| ui.bar).flatten() {
        Some(bar) => bar,
        None => {
            let Ok(bar) = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                w!("VintrdpBar"),
                w!(""),
                WS_POPUP | WS_BORDER,
                0,
                0,
                BAR_W,
                BAR_H,
                Some(session),
                None,
                Some(instance()),
                None,
            ) else {
                return;
            };
            let font = make_font(dpi_of(session));
            SetWindowLongPtrW(bar, GWLP_USERDATA, font.0 as _);
            for (id, label, _) in BAR_BUTTONS {
                let label = wide(label);
                if let Ok(button) = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("BUTTON"),
                    PCWSTR(label.as_ptr()),
                    WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                    0,
                    0,
                    10,
                    10,
                    Some(bar),
                    Some(HMENU(id as *mut c_void)),
                    Some(instance()),
                    None,
                ) {
                    send(button, WM_SETFONT, font.0 as usize, 0);
                }
            }
            with_session(|ui| ui.bar = Some(bar));
            bar
        }
    };
    if IsWindowVisible(bar).as_bool() {
        return;
    }
    let dpi = dpi_of(session);
    let (w, h) = (BAR_W * dpi / 96, BAR_H * dpi / 96);
    let (gap, button_h) = (6 * dpi / 96, 22 * dpi / 96);
    let mut x = w - 2;
    for (id, _, width) in BAR_BUTTONS.iter().rev() {
        x -= width * dpi / 96 + gap;
        if let Ok(button) = GetDlgItem(Some(bar), *id as i32) {
            let _ = SetWindowPos(button, None, x, (h - 2 - button_h) / 2, width * dpi / 96, button_h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
    let screen = monitor_rect(session);
    let left = screen.left + (screen.right - screen.left - w) / 2;
    let _ = SetWindowPos(bar, Some(HWND_TOPMOST), left, screen.top, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    let _ = InvalidateRect(Some(bar), None, true);
    SetTimer(Some(bar), BAR_TIMER, 250, None);
}

unsafe fn hide_bar() {
    if let Some(bar) = with_session(|ui| ui.bar).flatten() {
        let _ = KillTimer(Some(bar), BAR_TIMER);
        let _ = ShowWindow(bar, SW_HIDE);
    }
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wparam.0 == BAR_TIMER => {
            let (mut point, mut rect) = (POINT::default(), RECT::default());
            if GetCursorPos(&mut point).is_ok() && GetWindowRect(hwnd, &mut rect).is_ok() && !PtInRect(&rect, point).as_bool() {
                hide_bar();
            }
            return LRESULT(0);
        }
        WM_COMMAND => {
            let Some(session) = with_session(|ui| ui.hwnd) else { return LRESULT(0) };
            hide_bar();
            match wparam.0 & 0xffff {
                BAR_MINIMISE => {
                    let _ = ShowWindow(session, SW_MINIMIZE);
                }
                BAR_WINDOW => set_fullscreen(session, false),
                BAR_CLOSE => {
                    let _ = PostMessageW(Some(session), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                _ => {}
            }
            return LRESULT(0);
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let title = with_session(|ui| ui.hwnd).map(|s| text_of(s)).unwrap_or_default();
            let mut title = wide(&title);
            let dpi = with_session(|ui| dpi_of(ui.hwnd)).unwrap_or(96);
            let buttons: i32 = BAR_BUTTONS.iter().map(|(_, _, w)| (w + 6) * dpi / 96).sum();
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let mut text = RECT { left: 8 * dpi / 96, right: client.right - buttons - 8 * dpi / 96, ..client };
            let font = HFONT(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut c_void);
            let old = SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(GetSysColor(COLOR_BTNTEXT)));
            let len = title.len() - 1;
            DrawTextW(hdc, &mut title[..len], &mut text, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
            SelectObject(hdc, old);
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_DESTROY => {
            let font = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if font != 0 {
                let _ = DeleteObject(HGDIOBJ(font as *mut c_void));
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// WM_HSCROLL and WM_VSCROLL of the session window.
unsafe fn session_scrolled(hwnd: HWND, bar: SCROLLBAR_CONSTANTS, wparam: WPARAM) {
    let mut info = SCROLLINFO { cbSize: size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL, ..Default::default() };
    if GetScrollInfo(hwnd, bar, &mut info).is_err() {
        return;
    }
    let line = 32 * dpi_of(hwnd) / 96;
    let pos = match SCROLLBAR_COMMAND((wparam.0 & 0xffff) as i32) {
        SB_LINEUP => info.nPos - line,
        SB_LINEDOWN => info.nPos + line,
        SB_PAGEUP => info.nPos - info.nPage as i32,
        SB_PAGEDOWN => info.nPos + info.nPage as i32,
        SB_THUMBTRACK | SB_THUMBPOSITION => info.nTrackPos,
        SB_TOP => 0,
        SB_BOTTOM => info.nMax,
        _ => return,
    };
    let pos = pos.clamp(0, (info.nMax + 1 - info.nPage as i32).max(0));
    with_session(|ui| if bar == SB_HORZ { ui.scroll.0 = pos } else { ui.scroll.1 = pos });
    let info = SCROLLINFO { cbSize: size_of::<SCROLLINFO>() as u32, fMask: SIF_POS, nPos: pos, ..Default::default() };
    SetScrollInfo(hwnd, bar, &info, true);
    let _ = InvalidateRect(Some(hwnd), None, false);
}

unsafe fn paint_session(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let black = HBRUSH(GetStockObject(BLACK_BRUSH).0);
    let (scroll, scale_up) = (session_scroll(), !is_fullscreen());
    let drawn = current_session().and_then(|session| {
        session.with_framebuffer(|w, h, px| {
            let dest = place(&client, w, h, scroll, scale_up);
            // Letterbox bars, around the part of `dest` in the client area.
            let shown = RECT {
                left: dest.left.max(0),
                top: dest.top.max(0),
                right: dest.right.min(client.right),
                bottom: dest.bottom.min(client.bottom),
            };
            for bar in [
                RECT { left: 0, top: 0, right: client.right, bottom: shown.top },
                RECT { left: 0, top: shown.bottom, right: client.right, bottom: client.bottom },
                RECT { left: 0, top: shown.top, right: shown.left, bottom: shown.bottom },
                RECT { left: shown.right, top: shown.top, right: client.right, bottom: shown.bottom },
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
            // 1:1 or larger (see `place`).
            SetStretchBltMode(hdc, COLORONCOLOR);
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
    let (scroll, scale_up) = (session_scroll(), !is_fullscreen());
    current_session()?.with_framebuffer(|w, h, _| {
        let dest = place(&client, w, h, scroll, scale_up);
        let (dw, dh) = ((dest.right - dest.left).max(1) as f64, (dest.bottom - dest.top).max(1) as f64);
        let sx = ((x - dest.left) as f64 * w as f64 / dw).clamp(0.0, w as f64 - 1.0);
        let sy = ((y - dest.top) as f64 * h as f64 / dh).clamp(0.0, h as f64 - 1.0);
        (sx as u16, sy as u16)
    })
}

/// An X client moved the pointer to (x, y) in session pixels: move ours
/// there (`to_session` inverted), if the window has focus and the pointer
/// is over its client area.
unsafe fn warp_pointer(hwnd: HWND, x: i32, y: i32) {
    if GetForegroundWindow() != hwnd {
        return;
    }
    let mut client = RECT::default();
    let mut now = POINT::default();
    if GetClientRect(hwnd, &mut client).is_err() || GetCursorPos(&mut now).is_err() || !ScreenToClient(hwnd, &mut now).as_bool() {
        return;
    }
    let inside = |p: &POINT| p.x >= client.left && p.y >= client.top && p.x < client.right && p.y < client.bottom;
    if !inside(&now) {
        return;
    }
    let (scroll, scale_up) = (session_scroll(), !is_fullscreen());
    let Some(mut to) = current_session().and_then(|s| {
        s.with_framebuffer(|w, h, _| {
            let dest = place(&client, w, h, scroll, scale_up);
            let (dw, dh) = ((dest.right - dest.left) as f64, (dest.bottom - dest.top) as f64);
            // The middle of the session pixel.
            let (x, y) = (x.clamp(0, w as i32 - 1) as f64 + 0.5, y.clamp(0, h as i32 - 1) as f64 + 0.5);
            POINT { x: dest.left + (x * dw / w as f64) as i32, y: dest.top + (y * dh / h as f64) as i32 }
        })
    }) else {
        return;
    };
    // Scrolled out of sight: leave the pointer be.
    if inside(&to) && ClientToScreen(hwnd, &mut to).as_bool() {
        let _ = SetCursorPos(to.x, to.y);
    }
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

/// Type a character no US key makes, with Shift up: Shift would turn the
/// keypad keys of an Alt+0nnn code into arrows.
fn type_text(c: char) {
    let Some(session) = current_session() else { return };
    let shift: Vec<Scancode> = with_session(|ui| ui.held.iter().copied().filter(|&k| k == scancode::LSHIFT || k == scancode::RSHIFT).collect()).unwrap_or_default();
    for &k in &shift {
        send_key(k, false);
    }
    session.type_char(c);
    for &k in &shift {
        send_key(k, true);
    }
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

unsafe fn drop_paths(drop: HDROP) -> Vec<PathBuf> {
    (0..DragQueryFileW(drop, u32::MAX, None))
        .map(|i| {
            let mut name = vec![0u16; DragQueryFileW(drop, i, None) as usize + 1];
            let len = DragQueryFileW(drop, i, Some(&mut name)) as usize;
            PathBuf::from(String::from_utf16_lossy(&name[..len]))
        })
        .collect()
}

unsafe fn files_dropped(hwnd: HWND, drop: HDROP) {
    let paths = drop_paths(drop);
    let mut point = POINT::default();
    let _ = DragQueryPoint(drop, &mut point);
    DragFinish(drop);
    let Some(session) = current_session() else { return };
    let (x, y) = to_session(hwnd, point.x, point.y).unwrap_or((0, 0));
    let title = text_of(hwnd);
    launch::drop_files(session, paths, x, y, move |line| {
        let text = line.unwrap_or_else(|| title.clone());
        run_on_ui(move || {
            if let Some(hwnd) = with_session(|ui| ui.hwnd) {
                // SAFETY: on the UI thread, with our own window.
                unsafe { set_text(hwnd, &text) };
            }
        });
    });
}

/// The files on the clipboard, if that's what's there.
unsafe fn clipboard_files(hwnd: HWND) -> Vec<PathBuf> {
    if OpenClipboard(Some(hwnd)).is_err() {
        return Vec::new();
    }
    let paths = GetClipboardData(CF_HDROP).map(|h| drop_paths(HDROP(h.0))).unwrap_or_default();
    let _ = CloseClipboard();
    paths
}

/// WM_SETTINGCHANGE for the light/dark app mode (or High Contrast).
unsafe fn is_colour_change(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    let name = PCWSTR(lparam.0 as *const u16);
    matches!(name.to_string().as_deref(), Ok("ImmersiveColorSet" | "WindowsThemeElement"))
}

unsafe extern "system" fn session_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    const VK_PACKET: usize = 0xe7;
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_SETTINGCHANGE if is_colour_change(lparam) => {
            theme::refresh();
            theme::title_bar(hwnd);
        }
        WM_PAINT => {
            paint_session(hwnd);
            return LRESULT(0);
        }
        WM_SIZE => {
            update_session_scroll(hwnd);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        WM_HSCROLL | WM_VSCROLL => {
            session_scrolled(hwnd, if msg == WM_HSCROLL { SB_HORZ } else { SB_VERT }, wparam);
            return LRESULT(0);
        }
        WM_SYSCOMMAND if wparam.0 & 0xfff0 == FULLSCREEN_COMMAND => {
            set_fullscreen(hwnd, !is_fullscreen());
            return LRESULT(0);
        }
        WM_ACTIVATE => fullscreen_activated(hwnd, wparam.0 & 0xffff != 0, HWND(lparam.0 as *mut c_void)),
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
            // Ctrl+Alt+Break switches full screen, as in Microsoft's client. With Ctrl
            // held, Pause/Break usually arrives as VK_CANCEL; some keyboards send VK_PAUSE.
            let held = |vk: VIRTUAL_KEY| GetKeyState(vk.0 as i32) < 0;
            let brk = wparam.0 == VK_CANCEL.0 as usize || wparam.0 == VK_PAUSE.0 as usize;
            if brk && held(VK_CONTROL) && held(VK_MENU) {
                if down {
                    set_fullscreen(hwnd, !is_fullscreen());
                }
                return LRESULT(0);
            }
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
                let unit = wparam.0 as u16;
                let c = match unit {
                    0xd800..=0xdbff => {
                        with_session(|ui| ui.high_surrogate = Some(unit));
                        None
                    }
                    0xdc00..=0xdfff => with_session(|ui| ui.high_surrogate.take())
                        .flatten()
                        .and_then(|high| char::decode_utf16([high, unit]).next()?.ok()),
                    _ => char::from_u32(unit as u32),
                };
                match c.filter(|c| !c.is_control()) {
                    Some(c) => match scancode::from_us_char(c) {
                        Some((sc, shift)) => type_char(sc, shift),
                        None => type_text(c),
                    },
                    // Enter, Tab, Backspace and the like.
                    None => {
                        if let Some((sc, shift)) = c.and_then(scancode::from_us_char) {
                            type_char(sc, shift);
                        }
                    }
                }
            }
            return LRESULT(0);
        }
        WM_SYSCHAR => return LRESULT(0), // no menu-bar beep for Alt combinations
        WM_DROPFILES => {
            files_dropped(hwnd, HDROP(wparam.0 as *mut c_void));
            return LRESULT(0);
        }
        WM_SETFOCUS => {
            // Text copied elsewhere reaches the server when you come back here.
            if let Some(s) = current_session() {
                sync_locks(&s);
                let files = clipboard_files(hwnd);
                if !files.is_empty() {
                    launch::offer_clipboard_files(s, files);
                } else if let Some(text) = get_clipboard(hwnd) {
                    s.set_clipboard(&text);
                }
            }
        }
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
            if high_word(lparam.0) <= 0 && is_fullscreen() {
                show_bar(hwnd);
            }
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
    dib_from_rgba(width, height, rgba, |src| {
        let a = src[3] as u32;
        [(src[2] as u32 * a / 255) as u8, (src[1] as u32 * a / 255) as u8, (src[0] as u32 * a / 255) as u8, src[3]]
    })
}

/// For a Windows that can't blend (NT 4.0 has no AlphaBlend, and no alpha
/// cursors before XP): the picture with its transparent pixels black, and a
/// monochrome mask set where they are, cut at half opacity. The classic
/// AND-mask/OR-picture pair, which is also what a cursor's ICONINFO wants.
unsafe fn masked_from_rgba(width: usize, height: usize, rgba: &[u8]) -> (HBITMAP, HBITMAP) {
    let clear = |p: &[u8]| p[3] < 128;
    let color = dib_from_rgba(width, height, rgba, |p| if clear(p) { [0; 4] } else { [p[2], p[1], p[0], 255] });
    // Monochrome rows are padded to 16 bits, leftmost pixel in the top bit.
    let stride = width.div_ceil(16) * 2;
    let mut bits = vec![0u8; stride * height];
    for (i, p) in rgba.chunks_exact(4).enumerate().take(width * height) {
        if clear(p) {
            let (x, y) = (i % width, i / width);
            bits[y * stride + x / 8] |= 0x80 >> (x % 8);
        }
    }
    let mask = CreateBitmap(width as i32, height as i32, 1, 1, Some(bits.as_ptr().cast()));
    (color, mask)
}

/// A top-down 32-bit DIB section, each pixel `pixel` of the RGBA source.
unsafe fn dib_from_rgba(width: usize, height: usize, rgba: &[u8], pixel: impl Fn(&[u8]) -> [u8; 4]) -> HBITMAP {
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
        dst.copy_from_slice(&pixel(src));
    }
    bitmap
}

type AlphaBlend = unsafe extern "system" fn(HDC, i32, i32, i32, i32, HDC, i32, i32, i32, i32, BLENDFUNCTION) -> BOOL;

/// msimg32's AlphaBlend (Windows 98 and 2000 on); NT 4.0 has no msimg32.
fn alpha_blend() -> Option<AlphaBlend> {
    // SAFETY: msimg32's export with its prototype.
    unsafe { win32::proc("msimg32.dll", "AlphaBlend") }
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
    let (color, mask) = if win32::version() < (5, 1) {
        masked_from_rgba(w, h, pixels)
    } else {
        (bitmap_from_rgba(w, h, pixels), CreateBitmap(w as i32, h as i32, 1, 1, None))
    };
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
// One row taller than the form needed before Encryption had a line of its own.
const LOGICAL_H: i32 = 654;
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
const ID_FOLDER: u16 = 16;
const ID_SERVER: u16 = 17;
const ID_IMPORT: u16 = 18;
const ID_LICENCE_CHOOSE: u16 = 19;
const ID_LICENCE_GLOBAL: u16 = 20;
const ID_LICENCES: u16 = 21;

const SS_RIGHT: u32 = 2;
const TBM_GETPOS: u32 = WM_USER;
const TBM_SETPOS: u32 = WM_USER + 5;
const TBM_SETRANGE: u32 = WM_USER + 6;
const TBM_SETTICFREQ: u32 = WM_USER + 20;

#[derive(Clone, Copy, Default)]
struct Controls {
    tiles: HWND,
    name: HWND,
    /// "Computer", or "Address" for X11.
    computer_label: HWND,
    computer: HWND,
    /// User name, Password, Domain and Colours: hidden for X11.
    logon_labels: [HWND; 4],
    user: HWND,
    password: HWND,
    save_password: HWND,
    domain: HWND,
    slider: HWND,
    width: HWND,
    height: HWND,
    size_label: HWND,
    depth: HWND,
    server: HWND,
    picture: HWND,
    share: HWND,
    choose: HWND,
    clipboard: HWND,
    sound: HWND,
    fallback: HWND,
    encryption: HWND,
    folder_label: HWND,
    printer: HWND,
    printer_label: HWND,
    ports: HWND,
    ports_label: HWND,
    licence_status: HWND,
    /// Display, Visual, Allow hosts, Font path: shown for X11, where the logon rows were.
    x11_labels: [HWND; 4],
    x11_display: HWND,
    /// launch::X11_VISUALS
    x11_visual: HWND,
    x11_primary: HWND,
    x11_allow: HWND,
    x11_font_path: HWND,
}

/// One tile's picture, already scaled to fit.
struct TilePicture {
    bitmap: HBITMAP,
    /// Without AlphaBlend: `bitmap` is masked_from_rgba's picture, this its mask.
    mask: Option<HBITMAP>,
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
    /// The Printer menu's items (launch::printer_choices).
    printer_choices: Vec<String>,
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

/// A window's DPI. Before Windows 10 1607 there's no per-window DPI, so the
/// system's: 96 on NT 4.0 with small fonts, 120 with large ones.
unsafe fn dpi_of(hwnd: HWND) -> i32 {
    type GetDpiForWindow = unsafe extern "system" fn(HWND) -> u32;
    let dpi = match win32::proc::<GetDpiForWindow>("user32.dll", "GetDpiForWindow") {
        Some(get) => get(hwnd) as i32,
        None => {
            let screen = GetDC(None);
            let dpi = GetDeviceCaps(Some(screen), LOGPIXELSY);
            ReleaseDC(None, screen);
            dpi
        }
    };
    dpi.max(96)
}

/// AdjustWindowRectExForDpi (Windows 10 1607 on), or AdjustWindowRectEx,
/// which measures at the system DPI [`dpi_of`] falls back to.
unsafe fn adjust_window_rect(rect: &mut RECT, style: WINDOW_STYLE, dpi: i32) {
    type ForDpi = unsafe extern "system" fn(*mut RECT, WINDOW_STYLE, BOOL, WINDOW_EX_STYLE, u32) -> BOOL;
    match win32::proc::<ForDpi>("user32.dll", "AdjustWindowRectExForDpi") {
        Some(adjust) => {
            let _ = adjust(rect, style, false.into(), WINDOW_EX_STYLE(0), dpi as u32);
        }
        None => {
            let _ = AdjustWindowRectEx(rect, style, false, WINDOW_EX_STYLE(0));
        }
    }
}

unsafe fn make_font(dpi: i32) -> HFONT {
    type ForDpi = unsafe extern "system" fn(u32, u32, *mut c_void, u32, u32) -> BOOL;
    let mut metrics = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
    let out: *mut c_void = (&raw mut metrics).cast();
    match win32::proc::<ForDpi>("user32.dll", "SystemParametersInfoForDpi") {
        Some(get) => {
            let _ = get(SPI_GETNONCLIENTMETRICS.0, metrics.cbSize, out, 0, dpi as u32);
        }
        None => {
            let get = |size: u32| SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, size, Some(out), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_ok();
            // Before Vista the structure ends before iPaddedBorderWidth, and
            // a size that counts it is refused.
            if !get(metrics.cbSize) {
                metrics.cbSize -= size_of::<i32>() as u32;
                get(metrics.cbSize);
            }
        }
    }
    CreateFontIndirectW(&metrics.lfMessageFont)
}

unsafe fn show_manager() {
    let dpi_guess = 96;
    let mut rect = RECT { left: 0, top: 0, right: LOGICAL_W, bottom: LOGICAL_H };
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
    adjust_window_rect(&mut rect, style, dpi_guess);
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
    let mut label = |text: &str, r: i32| child(w!("STATIC"), text, WINDOW_STYLE(SS_RIGHT), plain, 0, lx, row(r) + 4, 70, 20);
    for (text, r) in [("Name", 0), ("Size", 6), ("Server", 8), ("Picture", 10), ("License", 16)] {
        label(text, r);
    }
    let computer_label = label("Computer", 1);
    let logon_labels = [label("User name", 2), label("Password", 3), label("Domain", 5), label("Colours", 9)];
    // X11 has no logon: its options take those rows.
    let x11_labels = [label("Display", 2), label("Visual", 3), label("Allow hosts", 4), label("Font path", 5)];
    label("Options", 11);
    let folder_label = label("Folder", 13);
    let printer_label = label("Printer", 14);
    let ports_label = label("Ports", 15);
    let name = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(0), fw, 24);
    let computer = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(1), fw, 24);
    let user = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(2), fw, 24);
    let password = child(w!("EDIT"), "", edit_style | WINDOW_STYLE(ES_PASSWORD as u32), edge, 0, fx, row(3), fw, 24);
    let save_password = child(w!("BUTTON"), "Save password", WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32), plain, 0, fx, row(4), fw, 22);
    if !launch::can_save_passwords() {
        set_text(save_password, "Save password (needs Windows XP)");
        let _ = EnableWindow(save_password, false);
    }
    let domain = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(5), fw, 24);
    let number = edit_style | WINDOW_STYLE(ES_NUMBER as u32);
    let combo_style = WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32);
    // X11's options, in the rows above (and the tab order place) of the logon it doesn't have.
    let x11_display = child(w!("EDIT"), "", number, edge, 0, fx, row(2), 120, 24);
    let x11_visual = child(w!("COMBOBOX"), "", combo_style, plain, 0, fx, row(3), 170, 200);
    let x11_allow = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(4), fw, 24);
    let x11_font_path = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(5), fw, 24);
    let slider = child(w!("msctls_trackbar32"), "", WS_TABSTOP | WINDOW_STYLE(TBS_AUTOTICKS), plain, 0, fx, row(6), fw, 30);
    child(w!("STATIC"), "W", WINDOW_STYLE(SS_RIGHT), plain, 0, fx - 6, row(7) + 4, 18, 20);
    let width = child(w!("EDIT"), "", number, edge, ID_WIDTH, fx + 16, row(7), 56, 24);
    child(w!("STATIC"), "H", WINDOW_STYLE(SS_RIGHT), plain, 0, fx + 74, row(7) + 4, 18, 20);
    let height = child(w!("EDIT"), "", number, edge, ID_HEIGHT, fx + 96, row(7), 56, 24);
    let size_text = child(w!("STATIC"), "", WINDOW_STYLE(0), plain, 0, fx + 160, row(7) + 4, fw - 160, 20);
    let server = child(w!("COMBOBOX"), "", combo_style, plain, ID_SERVER, fx, row(8), fw, 200);
    let depth = child(w!("COMBOBOX"), "", combo_style, plain, 0, fx, row(9), 120, 200);
    let picture = child(w!("COMBOBOX"), "", combo_style, plain, 0, fx, row(10), 170, 300);
    let check = WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32);
    let clipboard = child(w!("BUTTON"), "Clipboard", check, plain, 0, fx, row(11), 76, 22);
    let sound = child(w!("BUTTON"), "Sound", check, plain, 0, fx + 80, row(11), 54, 22);
    let fallback = child(w!("BUTTON"), "RDP 4 fallback", check, plain, 0, fx + 138, row(11), 112, 22);
    // Options' second line: Encryption for every server but X11, PRIMARY for X11.
    let encryption = child(w!("BUTTON"), "Encryption", check, plain, 0, fx, row(12), 100, 22);
    // X11 has no Encryption, so its PRIMARY option takes that place.
    let x11_primary = child(w!("BUTTON"), "Share PRIMARY too", check, plain, 0, fx, row(12), 146, 22);
    let share = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(13), fw - 86, 24);
    let choose = child(w!("BUTTON"), "Choose\u{2026}", WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), plain, ID_FOLDER, fx + fw - 80, row(13), 80, 26);
    let printer = child(w!("COMBOBOX"), "", WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32), plain, 0, fx, row(14), fw, 200);
    let ports = child(w!("EDIT"), "", edit_style, edge, 0, fx, row(15), fw, 24);
    let licence_status = child(w!("STATIC"), "None", WINDOW_STYLE(0), plain, 0, fx, row(16) + 4, 54, 20);
    child(w!("BUTTON"), "Choose\u{2026}", WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), plain, ID_LICENCE_CHOOSE, fx + 58, row(16), 82, 26);
    child(w!("BUTTON"), "Use Global", WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), plain, ID_LICENCE_GLOBAL, fx + 144, row(16), 106, 26);

    let button_y = LOGICAL_H - 46;
    let push = WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32);
    child(w!("BUTTON"), "New", push, plain, ID_NEW, 20, button_y, 80, 30);
    child(w!("BUTTON"), "Delete", push, plain, ID_DELETE, 106, button_y, 80, 30);
    child(w!("BUTTON"), "About", push, plain, ID_ABOUT, 192, button_y, 80, 30);
    child(w!("BUTTON"), "Import TS License\u{2026}", push, plain, ID_IMPORT, 278, button_y, 150, 30);
    child(w!("BUTTON"), "Licenses\u{2026}", push, plain, ID_LICENCES, 434, button_y, 110, 30);
    child(w!("BUTTON"), "Save", push, plain, ID_SAVE, LOGICAL_W - 208, button_y, 90, 30);
    child(w!("BUTTON"), "Connect", WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32), plain, ID_CONNECT, LOGICAL_W - 110, button_y, 90, 30);

    // EM_SETCUEBANNER: grey text while the box is empty.
    let no_folder = wide("No folder selected");
    send(share, 0x1501, 1, no_folder.as_ptr() as isize);
    let ports_hint = wide(launch::PORTS_HINT);
    send(ports, 0x1501, 1, ports_hint.as_ptr() as isize);
    let hints = [(x11_display, launch::X11_DISPLAY_HINT), (x11_allow, launch::X11_ALLOW_HINT), (x11_font_path, launch::X11_FONT_PATH_HINT)].map(|(h, t)| (h, wide(t)));
    for (h, text) in &hints {
        send(*h, 0x1501, 1, text.as_ptr() as isize);
    }
    send(x11_display, EM_SETLIMITTEXT, 3, 0);
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
    for p in ServerKind::ALL {
        add(server, p.label());
    }
    for (v, _) in launch::X11_VISUALS {
        add(x11_visual, v);
    }
    add(picture, "Last screen");
    for (name, _) in MONITORS {
        add(picture, &format!("{name} monitor"));
    }

    let c = Controls {
        tiles, name, computer_label, computer, logon_labels, user, password, save_password, domain, slider, width, height, size_label: size_text, depth, server, picture, share, choose, clipboard, sound,
        fallback, encryption, folder_label, printer, printer_label, ports, ports_label, licence_status, x11_labels, x11_display, x11_visual, x11_primary, x11_allow, x11_font_path,
    };
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
            printer_choices: Vec::new(),
        });
    });
    apply_dpi(hwnd);
    theme::title_bar(hwnd);
    theme::controls(hwnd);

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
    adjust_window_rect(&mut rect, style, dpi);
    let _ = SetWindowPos(hwnd, None, 0, 0, rect.right - rect.left, rect.bottom - rect.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);

    let font = make_font(dpi);
    let (old, children) = with_mgr(|m| (std::mem::replace(&mut m.font, font), m.layout.clone())).unwrap_or_default();
    for (child, ..) in children {
        send(child, WM_SETFONT, font.0 as usize, 1);
    }
    place_children();
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
    let (bitmap, mask) = match alpha_blend() {
        Some(_) => (bitmap_from_rgba(scaled.width, scaled.height, &scaled.bytes), None),
        None => {
            let (bitmap, mask) = masked_from_rgba(scaled.width, scaled.height, &scaled.bytes);
            (bitmap, Some(mask))
        }
    };
    Some(TilePicture { bitmap, mask, width: scaled.width as i32, height: scaled.height as i32 })
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
            if let Some(mask) = p.mask {
                let _ = DeleteObject(mask.into());
            }
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
    let colors = theme::colors();
    FillRect(mem, &client, theme::brush(colors.tiles));

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
            let fill = theme::brush(if chosen { colors.chosen } else { colors.tile });
            let old_brush = SelectObject(mem, fill.into());
            let _ = RoundRect(mem, x, y, x + tw, y + th, radius, radius);
            SelectObject(mem, old_brush);

            if let Some(Some(p)) = m.pictures.get(i) {
                let area_h = th - 40 * tw / TILE_W;
                let (px, py) = (x + (tw - p.width) / 2, y + 8 * tw / TILE_W + (area_h - p.height) / 2);
                let old = SelectObject(picture_dc, p.mask.unwrap_or(p.bitmap).into());
                match (p.mask, alpha_blend()) {
                    (None, Some(alpha_blend)) => {
                        let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
                        let _ = alpha_blend(mem, px, py, p.width, p.height, picture_dc, 0, 0, p.width, p.height, blend);
                    }
                    _ => {
                        // Clear the opaque part (mask bits become white where
                        // set, black where not), then OR the picture in.
                        SetBkColor(mem, COLORREF(0xffffff));
                        SetTextColor(mem, COLORREF(0));
                        let _ = BitBlt(mem, px, py, p.width, p.height, Some(picture_dc), 0, 0, SRCAND);
                        SelectObject(picture_dc, p.bitmap.into());
                        let _ = BitBlt(mem, px, py, p.width, p.height, Some(picture_dc), 0, 0, SRCPAINT);
                    }
                }
                SelectObject(picture_dc, old);
            }

            SetTextColor(mem, if chosen { colors.chosen_text } else { colors.text });
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
    update_licence_label();
    let tiles = controls().tiles;
    // SAFETY: our own window.
    unsafe {
        let _ = InvalidateRect(Some(tiles), None, false);
    }
}

/// Update the License row to show this connection's licence source.
fn update_licence_label() {
    let id = with_mgr(|m| m.selected.clone()).flatten();
    let text = match id.as_deref() {
        Some(id) if launch::connection_licence_path(id).exists() => "Custom",
        _ if launch::imported_licence().exists() => "Global",
        _ => "None",
    };
    // SAFETY: on the UI thread, with our own control.
    unsafe {
        set_text(controls().licence_status, text);
    }
}

/// List the imported licences, with the option to remove the global one.
unsafe fn show_licences(owner: HWND) {
    let (global, per) = with_mgr(|m| m.store.licences()).unwrap_or((None, Vec::new()));
    let report = launch::licence_report(global, &per);
    if global.is_none() {
        let title = wide("Imported licenses");
        let body = wide(&report);
        MessageBoxW(Some(owner), PCWSTR(body.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONINFORMATION);
        return;
    }
    let title = wide("Imported licenses");
    let body = wide(&format!("{report}\n\nRemove the global licence?"));
    let answer = MessageBoxW(Some(owner), PCWSTR(body.as_ptr()), PCWSTR(title.as_ptr()), MB_YESNO | MB_ICONQUESTION);
    if answer == IDYES {
        if let Err(e) = launch::remove_global_licence() {
            alert("Could not remove the licence", &e.to_string());
        }
        update_licence_label();
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
        set_text(c.share, &form.share);
        set_text(c.ports, &form.ports);
        set_text(c.x11_display, &form.x11_display);
        set_text(c.x11_allow, &form.x11_allow);
        set_text(c.x11_font_path, &form.x11_font_path);
        send(c.x11_visual, CB_SETCURSEL, form.x11_visual.min(launch::X11_VISUALS.len() - 1), 0);
        send(c.x11_primary, BM_SETCHECK, form.x11_primary as usize, 0);
        let choices = launch::printer_choices(&form.printer);
        send(c.printer, CB_RESETCONTENT, 0, 0);
        for choice in &choices {
            let text = wide(choice);
            send(c.printer, CB_ADDSTRING, 0, text.as_ptr() as isize);
        }
        send(c.printer, CB_SETCURSEL, choices.iter().position(|ch| *ch == form.printer).unwrap_or(0), 0);
        with_mgr(|m| m.printer_choices = choices);
        let server = ServerKind::ALL.iter().position(|&p| p == form.server).unwrap_or(0);
        send(c.server, CB_SETCURSEL, server, 0);
        apply_server();
        send(c.depth, CB_SETCURSEL, form.depth_index.min(form.server.depths() - 1), 0);
        send(c.clipboard, BM_SETCHECK, form.clipboard as usize, 0);
        send(c.sound, BM_SETCHECK, form.sound as usize, 0);
        send(c.fallback, BM_SETCHECK, form.fallback_rdp4 as usize, 0);
        send(c.encryption, BM_SETCHECK, form.encryption as usize, 0);
        let picture = match form.picture {
            Picture::Screen => 0,
            Picture::Monitor => form.color % MONITORS.len() + 1,
        };
        send(c.picture, CB_SETCURSEL, picture, 0);
        send(c.save_password, BM_SETCHECK, (form.save_password && launch::can_save_passwords()) as usize, 0);
    }
    with_mgr(|m| m.loading = false);
    size_changed();
}

fn server_kind() -> ServerKind {
    // SAFETY: on the UI thread, with our own control.
    let index = unsafe { send(controls().server, CB_GETCURSEL, 0, 0) };
    ServerKind::at(index as isize)
}

/// Offer only what the chosen kind of server can do.
unsafe fn apply_server() {
    let c = controls();
    let kind = server_kind();
    let depth = send(c.depth, CB_GETCURSEL, 0, 0).max(0) as usize;
    send(c.depth, CB_RESETCONTENT, 0, 0);
    for d in &DEPTHS[..kind.depths()] {
        let text = wide(&format!("{d} bit"));
        send(c.depth, CB_ADDSTRING, 0, text.as_ptr() as isize);
    }
    send(c.depth, CB_SETCURSEL, depth.min(kind.depths() - 1), 0);
    let show = |hwnd: HWND, visible: bool| {
        let _ = ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
    };
    show(c.encryption, kind.has_encryption());
    show(c.clipboard, kind.has_clipboard());
    show(c.sound, kind.has_sound());
    show(c.fallback, kind.has_rdp4_fallback());
    show(c.folder_label, kind.has_folder());
    show(c.share, kind.has_folder());
    show(c.choose, kind.has_folder());
    for h in [c.printer_label, c.printer, c.ports_label, c.ports] {
        show(h, kind.has_devices());
    }
    for h in c.logon_labels.into_iter().chain([c.user, c.password, c.save_password, c.domain, c.depth]) {
        show(h, kind.has_logon());
    }
    let x11 = kind == ServerKind::X11;
    for h in c.x11_labels.into_iter().chain([c.x11_display, c.x11_visual, c.x11_primary, c.x11_allow, c.x11_font_path]) {
        show(h, x11);
    }
    let (label, hint) = kind.address_label();
    set_text(c.computer_label, label);
    let hint = wide(hint);
    send(c.computer, 0x1501, 1, hint.as_ptr() as isize);
    place_children();
}

/// Put every control at its place for the window's DPI. The form's rows
/// close up over rows whose controls are all hidden, so there are no gaps.
unsafe fn place_children() {
    let Some((hwnd, children)) = with_mgr(|m| (m.hwnd, m.layout.clone())) else { return };
    let dpi = dpi_of(hwnd);
    const ROWS: usize = 17;
    let row_of = |y: i32| ((y - 20).max(0) / 34) as usize;
    let in_form = |x: i32, y: i32| x >= LEFT_W + 30 && y < LOGICAL_H - 46 && row_of(y) < ROWS;
    let visible = |c: HWND| GetWindowLongPtrW(c, GWL_STYLE) as u32 & WS_VISIBLE.0 != 0;
    let mut used = [false; ROWS];
    for &(c, x, y, _, _) in &children {
        if in_form(x, y) && visible(c) {
            used[row_of(y)] = true;
        }
    }
    for (c, x, y, w, h) in children {
        let y = if in_form(x, y) { y - 34 * used[..row_of(y)].iter().filter(|u| !**u).count() as i32 } else { y };
        let _ = SetWindowPos(c, None, x * dpi / 96, y * dpi / 96, w * dpi / 96, h * dpi / 96, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

fn read_form() -> Form {
    let c = controls();
    let color = with_mgr(|m| m.color).unwrap_or(0);
    // SAFETY: on the UI thread, with our own controls.
    unsafe {
        let picture = send(c.picture, CB_GETCURSEL, 0, 0);
        let printer = send(c.printer, CB_GETCURSEL, 0, 0);
        Form {
            name: text_of(c.name),
            computer: text_of(c.computer),
            username: text_of(c.user),
            password: text_of(c.password),
            domain: text_of(c.domain),
            width: text_of(c.width),
            height: text_of(c.height),
            share: text_of(c.share),
            printer: with_mgr(|m| m.printer_choices.get(printer.max(0) as usize).map(|ch| launch::printer_from_choice(ch))).flatten().unwrap_or_default(),
            ports: text_of(c.ports),
            x11_display: text_of(c.x11_display),
            x11_visual: send(c.x11_visual, CB_GETCURSEL, 0, 0).max(0) as usize,
            x11_primary: send(c.x11_primary, BM_GETCHECK, 0, 0) != 0,
            x11_allow: text_of(c.x11_allow),
            x11_font_path: text_of(c.x11_font_path),
            depth_index: send(c.depth, CB_GETCURSEL, 0, 0).max(0) as usize,
            server: server_kind(),
            clipboard: send(c.clipboard, BM_GETCHECK, 0, 0) != 0,
            sound: send(c.sound, BM_GETCHECK, 0, 0) != 0,
            fallback_rdp4: send(c.fallback, BM_GETCHECK, 0, 0) != 0,
            encryption: send(c.encryption, BM_GETCHECK, 0, 0) != 0,
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
    let mut config = match checked {
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
    config.licence = launch::effective_licence(&saved.id);
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

/// An Open dialog for a licence file.
unsafe fn choose_licence_file(owner: HWND, title: &str) -> Option<PathBuf> {
    let title = wide(title);
    // A double-NUL-terminated filter list, each entry a label then a pattern.
    let filter: Vec<u16> = "Licence files (*.cal)\0*.cal\0All files (*.*)\0*.*\0\0".encode_utf16().collect();
    let mut file = [0u16; 1024];
    // NT 4.0's comdlg32 refuses the Windows 2000 structure, which adds
    // pvReserved, dwReserved and FlagsEx at the end.
    let size = if win32::is_nt4() { std::mem::offset_of!(OPENFILENAMEW, pvReserved) } else { size_of::<OPENFILENAMEW>() };
    let mut ofn = OPENFILENAMEW {
        lStructSize: size as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    if !GetOpenFileNameW(&mut ofn).as_bool() {
        return None;
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    Some(PathBuf::from(String::from_utf16_lossy(&file[..len])))
}

unsafe fn import_licence_dialog(owner: HWND) {
    let Some(path) = choose_licence_file(owner, "Choose a Terminal Services licence file (.cal)") else { return };
    match launch::import_licence(&path) {
        Ok(()) => alert("Licence imported", "vintrdp will present it to every server you connect to."),
        Err(e) => alert("Could not import the licence", &e.to_string()),
    }
}

/// Pick a licence file to tie to the selected connection.
unsafe fn choose_connection_licence(owner: HWND) {
    let Some(id) = with_mgr(|m| m.selected.clone()).flatten() else { return };
    let Some(path) = choose_licence_file(owner, "Choose a licence file (.cal) for this connection") else { return };
    match with_mgr(|m| m.store.set_connection_licence(&id, &path)) {
        Some(Err(e)) => alert("Could not set the licence", &e.to_string()),
        _ => update_licence_label(),
    }
}

unsafe fn choose_folder(owner: HWND) {
    let title = wide("Choose a folder to share with the server");
    let info = BROWSEINFOW {
        hwndOwner: owner,
        lpszTitle: PCWSTR(title.as_ptr()),
        // The resizable dialog needs shell32 5.0 (Windows 2000).
        ulFlags: if win32::is_nt4() { BIF_RETURNONLYFSDIRS } else { BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE },
        ..Default::default()
    };
    let list = SHBrowseForFolderW(&info);
    if list.is_null() {
        return;
    }
    // SHGetPathFromIDListEx is Vista's; the plain one fills MAX_PATH.
    type PathEx = unsafe extern "system" fn(*const ITEMIDLIST, PWSTR, u32, GPFIDL_FLAGS) -> BOOL;
    let mut path = [0u16; 1024];
    let found = match win32::proc::<PathEx>("shell32.dll", "SHGetPathFromIDListEx") {
        Some(get) => get(list, PWSTR(path.as_mut_ptr()), path.len() as u32, GPFIDL_DEFAULT).as_bool(),
        None => SHGetPathFromIDListW(list, (&mut path[..260]).try_into().expect("MAX_PATH")).as_bool(),
    };
    if found {
        let len = path.iter().position(|&c| c == 0).unwrap_or(path.len());
        set_text(controls().share, &String::from_utf16_lossy(&path[..len]));
    }
    // ole32's, which every Windows has: the windows crate binds it to
    // combase.dll, which arrived in Windows 8.
    type CoTaskMemFree = unsafe extern "system" fn(*const c_void);
    if let Some(free) = win32::proc::<CoTaskMemFree>("ole32.dll", "CoTaskMemFree") {
        free(list.cast());
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
                (ID_IMPORT, _) => import_licence_dialog(hwnd),
                (ID_LICENCE_CHOOSE, _) => choose_connection_licence(hwnd),
                (ID_LICENCE_GLOBAL, _) => {
                    if let Some(id) = with_mgr(|m| m.selected.clone()).flatten() {
                        with_mgr(|m| m.store.clear_connection_licence(&id));
                        update_licence_label();
                    }
                }
                (ID_LICENCES, _) => show_licences(hwnd),
                (ID_FOLDER, _) => choose_folder(hwnd),
                (ID_SERVER, CBN_SELCHANGE) => apply_server(),
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
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            if let Some(brush) = theme::control_color(msg, wparam) {
                return brush;
            }
        }
        WM_ERASEBKGND => {
            if let Some(done) = theme::erase(hwnd, wparam) {
                return done;
            }
        }
        WM_NOTIFY => {
            if let Some(result) = theme::custom_draw(lparam) {
                return result;
            }
        }
        WM_SETTINGCHANGE if is_colour_change(lparam) => {
            if theme::refresh() {
                theme::title_bar(hwnd);
                theme::controls(hwnd);
                if let Some(h) = current_session().and(with_session(|ui| ui.hwnd)) {
                    theme::title_bar(h);
                }
            }
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
