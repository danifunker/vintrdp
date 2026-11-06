//! Following Windows' light or dark app mode (Settings > Personalization >
//! Colors). Win32 has no dark controls of its own, so this is the recipe
//! Explorer and Notepad use: a dark title bar from DWM, the "DarkMode_*"
//! visual styles for buttons, fields, lists and scroll bars, our own colours
//! for everything painted through WM_CTLCOLOR*, and checkbox labels drawn here,
//! since themed checkboxes ignore text colours. High Contrast keeps the system
//! colours. Windows before 10 1809 has no dark mode, and stays light.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCSTR, PCWSTR, w};

static DARK: AtomicBool = AtomicBool::new(false);

pub fn dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// Read the setting again. True if it changed.
pub fn refresh() -> bool {
    let dark = build() >= 17763 && apps_use_dark() && !high_contrast();
    if let Some(set_app_mode) = ux().set_app_mode {
        // 1 is AllowDark (or TRUE for 1809's AllowDarkModeForApp): menus and
        // other system-drawn parts follow the setting from here on.
        // SAFETY: uxtheme's export, with its prototype.
        unsafe { set_app_mode(i32::from(dark)) };
    }
    DARK.swap(dark, Ordering::Relaxed) != dark
}

/// Windows' build number, e.g. 19045 or 26100.
fn build() -> u32 {
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| {
        let mut text = [0u16; 16];
        let mut size = (text.len() * 2) as u32;
        // SAFETY: the buffer and its size in bytes.
        let read = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
                w!("CurrentBuildNumber"),
                RRF_RT_REG_SZ,
                None,
                Some(text.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if read.is_err() {
            return 0;
        }
        String::from_utf16_lossy(&text).trim_end_matches('\0').parse().unwrap_or(0)
    })
}

fn apps_use_dark() -> bool {
    let mut light = 1u32;
    let mut size = 4u32;
    // SAFETY: a DWORD and its size.
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut light).cast()),
            Some(&mut size),
        )
    };
    read.is_ok() && light == 0
}

fn high_contrast() -> bool {
    let mut hc = HIGHCONTRASTW { cbSize: size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    // SAFETY: the structure, with its size filled in.
    let ok = unsafe { SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, Some((&raw mut hc).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
    ok.is_ok() && hc.dwFlags.contains(HCF_HIGHCONTRASTON)
}

/// uxtheme's unnamed exports, by ordinal, from Windows 10 1809 on.
struct UxTheme {
    /// 135: SetPreferredAppMode (AllowDarkModeForApp on 1809).
    set_app_mode: Option<unsafe extern "system" fn(i32) -> i32>,
    /// 133: AllowDarkModeForWindow.
    allow_window: Option<unsafe extern "system" fn(HWND, BOOL) -> BOOL>,
}

fn ux() -> &'static UxTheme {
    static UX: OnceLock<UxTheme> = OnceLock::new();
    UX.get_or_init(|| {
        let none = UxTheme { set_app_mode: None, allow_window: None };
        // Before 1809 these ordinals are other functions, or missing.
        if build() < 17763 {
            return none;
        }
        // SAFETY: uxtheme stays loaded (comctl32 uses it); each export is cast
        // to its prototype.
        unsafe {
            let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return none };
            let by_ordinal = |n: usize| GetProcAddress(lib, PCSTR(n as *const u8));
            UxTheme {
                set_app_mode: by_ordinal(135).map(|f| std::mem::transmute::<_, unsafe extern "system" fn(i32) -> i32>(f)),
                allow_window: by_ordinal(133).map(|f| std::mem::transmute::<_, unsafe extern "system" fn(HWND, BOOL) -> BOOL>(f)),
            }
        }
    })
}

/// A top-level window's title bar.
pub fn title_bar(hwnd: HWND) {
    if build() < 17763 {
        return;
    }
    let on = BOOL::from(dark());
    // SAFETY: our window; a BOOL attribute.
    unsafe {
        if let Some(allow) = ux().allow_window {
            let _ = allow(hwnd, on);
        }
        // DWMWA_USE_IMMERSIVE_DARK_MODE: 20 from 20H1, 19 before that.
        let attribute = if build() >= 18985 { 20 } else { 19 };
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(attribute), (&raw const on).cast(), size_of::<BOOL>() as u32);
    }
}

/// Give every control in `parent` the visual style for the current mode, and
/// repaint them.
pub fn controls(parent: HWND) {
    // SAFETY: our own windows.
    unsafe {
        let _ = EnumChildWindows(Some(parent), Some(theme_child), LPARAM(0));
        let _ = RedrawWindow(Some(parent), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME);
    }
}

unsafe extern "system" fn theme_child(hwnd: HWND, _: LPARAM) -> BOOL {
    let mut name = [0u16; 32];
    let len = unsafe { GetClassNameW(hwnd, &mut name) } as usize;
    let class = String::from_utf16_lossy(&name[..len]);
    let dark = dark();
    let style = match class.as_str() {
        "Button" | "VintrdpTiles" | "ComboLBox" | "ListBox" => w!("DarkMode_Explorer"),
        "Edit" | "ComboBox" => w!("DarkMode_CFD"),
        _ => return true.into(),
    };
    // SAFETY: a control of ours, and uxtheme's export with its prototype.
    unsafe {
        if let Some(allow) = ux().allow_window {
            let _ = allow(hwnd, dark.into());
        }
        // No theme name puts the default style back.
        let _ = SetWindowTheme(hwnd, if dark { style } else { PCWSTR::null() }, PCWSTR::null());
        if class == "ComboBox" {
            // A drop-down's list is a window of its own.
            let mut info = COMBOBOXINFO { cbSize: size_of::<COMBOBOXINFO>() as u32, ..Default::default() };
            if GetComboBoxInfo(hwnd, &mut info).is_ok() {
                let _ = theme_child(info.hwndList, LPARAM(0));
            }
        }
    }
    true.into()
}

/// The manager's colours.
#[derive(Clone, Copy)]
pub struct Colors {
    /// Window background, labels' background.
    pub face: COLORREF,
    pub text: COLORREF,
    /// Text fields and drop-down lists.
    pub field: COLORREF,
    pub field_text: COLORREF,
    /// Behind the tiles, and a tile.
    pub tiles: COLORREF,
    pub tile: COLORREF,
    /// The selected tile.
    pub chosen: COLORREF,
    pub chosen_text: COLORREF,
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

pub fn colors() -> Colors {
    if dark() {
        return Colors {
            face: rgb(32, 32, 32),
            text: rgb(240, 240, 240),
            field: rgb(45, 45, 45),
            field_text: rgb(240, 240, 240),
            tiles: rgb(25, 25, 25),
            tile: rgb(52, 52, 52),
            chosen: rgb(0, 95, 184),
            chosen_text: rgb(255, 255, 255),
        };
    }
    // SAFETY: plain lookups.
    let sys = |c: SYS_COLOR_INDEX| COLORREF(unsafe { GetSysColor(c) });
    Colors {
        face: sys(COLOR_BTNFACE),
        text: sys(COLOR_BTNTEXT),
        field: sys(COLOR_WINDOW),
        field_text: sys(COLOR_WINDOWTEXT),
        tiles: sys(COLOR_WINDOW),
        tile: sys(COLOR_BTNFACE),
        chosen: sys(COLOR_HIGHLIGHT),
        chosen_text: sys(COLOR_HIGHLIGHTTEXT),
    }
}

/// A solid brush in `color`, kept for the life of the process.
pub fn brush(color: COLORREF) -> HBRUSH {
    thread_local! {
        static BRUSHES: RefCell<Vec<(u32, HBRUSH)>> = const { RefCell::new(Vec::new()) };
    }
    BRUSHES.with(|b| {
        let mut b = b.borrow_mut();
        if let Some(&(_, brush)) = b.iter().find(|(c, _)| *c == color.0) {
            return brush;
        }
        // SAFETY: a plain GDI object.
        let brush = unsafe { CreateSolidBrush(color) };
        b.push((color.0, brush));
        brush
    })
}

/// WM_CTLCOLOR* for the manager's controls: None means DefWindowProc's.
pub fn control_color(msg: u32, wparam: WPARAM) -> Option<LRESULT> {
    let hdc = HDC(wparam.0 as *mut c_void);
    let c = colors();
    let (text, back) = match msg {
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => (c.text, c.face),
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX if dark() => (c.field_text, c.field),
        _ => return None,
    };
    // SAFETY: the control's DC, for the duration of the message.
    unsafe {
        SetTextColor(hdc, text);
        SetBkColor(hdc, back);
        if msg == WM_CTLCOLORSTATIC {
            let _ = SetBkMode(hdc, TRANSPARENT);
        }
    }
    Some(LRESULT(brush(back).0 as isize))
}

/// The manager window's background, when dark (the class brush is light).
pub fn erase(hwnd: HWND, wparam: WPARAM) -> Option<LRESULT> {
    if !dark() {
        return None;
    }
    let mut r = RECT::default();
    // SAFETY: our window and the DC we were handed.
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
        FillRect(HDC(wparam.0 as *mut c_void), &r, brush(colors().face));
    }
    Some(LRESULT(1))
}

/// NM_CUSTOMDRAW from a checkbox, in dark mode: draw the box from the theme
/// and the label ourselves. None means let the control draw itself.
///
/// # Safety
/// `lparam` is the NMHDR of a WM_NOTIFY.
pub unsafe fn custom_draw(lparam: LPARAM) -> Option<LRESULT> {
    if !dark() {
        return None;
    }
    // SAFETY: as documented; NM_CUSTOMDRAW's NMHDR heads an NMCUSTOMDRAW.
    let header = unsafe { &*(lparam.0 as *const NMHDR) };
    if header.code != NM_CUSTOMDRAW {
        return None;
    }
    let cd = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
    let hwnd = cd.hdr.hwndFrom;
    let kind = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32 & BS_TYPEMASK as u32;
    if cd.dwDrawStage != CDDS_PREPAINT || (kind != BS_AUTOCHECKBOX as u32 && kind != BS_CHECKBOX as u32) {
        return None;
    }
    let state = cd.uItemState;
    let checked = unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None) }.0 == BST_CHECKED.0 as isize;
    let look = match (state.contains(CDIS_DISABLED), state.contains(CDIS_SELECTED), state.contains(CDIS_HOT)) {
        (true, _, _) => 3,
        (_, true, _) => 2,
        (_, _, true) => 1,
        _ => 0,
    };
    let part_state = if checked { CBS_CHECKEDNORMAL.0 } else { CBS_UNCHECKEDNORMAL.0 } + look;
    let c = colors();
    let rc = cd.rc;
    // SAFETY: the DC and rectangle the control gave us, for this paint.
    unsafe {
        let hdc = cd.hdc;
        FillRect(hdc, &rc, brush(c.face));
        let theme = OpenThemeData(Some(hwnd), w!("Button"));
        let size = Some(theme)
            .filter(|t| !t.is_invalid())
            .and_then(|t| GetThemePartSize(t, Some(hdc), BP_CHECKBOX.0, part_state, None, TS_DRAW).ok())
            .unwrap_or(SIZE { cx: 13, cy: 13 });
        let top = rc.top + (rc.bottom - rc.top - size.cy) / 2;
        let check = RECT { left: rc.left, top, right: rc.left + size.cx, bottom: top + size.cy };
        if !theme.is_invalid() {
            let _ = DrawThemeBackground(theme, hdc, BP_CHECKBOX.0, part_state, &check, None);
            let _ = CloseThemeData(theme);
        }

        let mut label = [0u16; 128];
        let len = GetWindowTextW(hwnd, &mut label) as usize;
        let font = HFONT(SendMessageW(hwnd, WM_GETFONT, None, None).0 as *mut c_void);
        let old = SelectObject(hdc, font.into());
        SetTextColor(hdc, if look == 3 { rgb(128, 128, 128) } else { c.text });
        let _ = SetBkMode(hdc, TRANSPARENT);
        let gap = size.cx / 3 + 1;
        let mut text = RECT { left: check.right + gap, ..rc };
        DrawTextW(hdc, &mut label[..len], &mut text, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_NOPREFIX);
        if state.contains(CDIS_FOCUS) && state.contains(CDIS_SHOWKEYBOARDCUES) {
            let mut bounds = text;
            DrawTextW(hdc, &mut label[..len], &mut bounds, DT_SINGLELINE | DT_LEFT | DT_NOPREFIX | DT_CALCRECT);
            let height = bounds.bottom - bounds.top;
            let focus = RECT { left: bounds.left - 1, top: text.top + (text.bottom - text.top - height) / 2 - 1, right: bounds.right + 1, bottom: text.top + (text.bottom - text.top + height) / 2 + 1 };
            let _ = DrawFocusRect(hdc, &focus);
        }
        SelectObject(hdc, old);
    }
    Some(LRESULT(CDRF_SKIPDEFAULT as isize))
}
