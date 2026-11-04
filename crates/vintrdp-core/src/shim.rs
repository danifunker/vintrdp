//! The Rust replacement for rdesktop's X11 front-end (xwin.c, xkeymap.c,
//! xclip.c, ewmhints.c, xgui.c). Every function here is called from C on the
//! rdesktop thread.

#![allow(non_upper_case_globals, clippy::missing_safety_doc, clippy::too_many_arguments)]

use std::cell::RefCell;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::ptr;
use std::slice;
use std::sync::Arc;
use std::time::Instant;

use crate::ffi::*;
use crate::gdi::{Bitmap, Glyph, Pattern, Surface};
use crate::session::{CursorImage, CursorShape, Event, shared};
use crate::state::{g_local_cursor, g_pending_resize, g_server_depth, g_user_quit};

// ---------------------------------------------------------------------------
// Globals rdesktop expects the front-end to define

/// We scale the framebuffer to the window instead of resizing the session.
#[unsafe(no_mangle)]
pub static mut g_dynamic_session_resize: RD_BOOL = 0;

#[unsafe(no_mangle)]
pub static mut g_wait_for_deactivate_ts: libc::time_t = 0;

// ---------------------------------------------------------------------------
// Per-thread state that only the rdesktop thread touches

struct Local {
    palette: [u32; 256],
    /// Drawing happened since the last Event::Updated.
    dirty: bool,
    start: Instant,
}

thread_local! {
    static LOCAL: RefCell<Local> = RefCell::new(Local {
        palette: [0; 256],
        dirty: false,
        start: Instant::now(),
    });
}

fn depth() -> c_int {
    // SAFETY: only written by rdesktop on this same thread.
    unsafe { g_server_depth }
}

/// Convert an RDP colour value (in the session's colour depth) to 0x00RRGGBB.
fn colour(c: u32) -> u32 {
    let (r, g, b) = match depth() {
        8 => return LOCAL.with(|l| l.borrow().palette[(c & 0xff) as usize]),
        15 => (
            ((c >> 7) & 0xf8) | ((c >> 12) & 0x7),
            ((c >> 2) & 0xf8) | ((c >> 8) & 0x7),
            ((c << 3) & 0xf8) | ((c >> 2) & 0x7),
        ),
        16 => (
            ((c >> 8) & 0xf8) | ((c >> 13) & 0x7),
            ((c >> 3) & 0xfc) | ((c >> 9) & 0x3),
            ((c << 3) & 0xf8) | ((c >> 2) & 0x7),
        ),
        _ => (c & 0xff, (c >> 8) & 0xff, (c >> 16) & 0xff),
    };
    (r << 16) | (g << 8) | b
}

/// Convert server-format bitmap data to 0x00RRGGBB pixels.
unsafe fn convert(width: usize, height: usize, data: *const u8) -> Vec<u32> {
    let n = width * height;
    if data.is_null() {
        return vec![0; n];
    }
    match depth() {
        8 => {
            // SAFETY: caller guarantees width*height bytes.
            let src = unsafe { slice::from_raw_parts(data, n) };
            LOCAL.with(|l| {
                let pal = &l.borrow().palette;
                src.iter().map(|&i| pal[i as usize]).collect()
            })
        }
        15 | 16 => {
            // SAFETY: caller guarantees width*height 16-bit pixels.
            let src = unsafe { slice::from_raw_parts(data, n * 2) };
            src.chunks_exact(2).map(|p| colour(u16::from_le_bytes([p[0], p[1]]) as u32)).collect()
        }
        24 => {
            // SAFETY: caller guarantees width*height BGR pixels.
            let src = unsafe { slice::from_raw_parts(data, n * 3) };
            src.chunks_exact(3).map(|p| (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32).collect()
        }
        _ => {
            // SAFETY: caller guarantees width*height BGRX pixels.
            let src = unsafe { slice::from_raw_parts(data, n * 4) };
            src.chunks_exact(4).map(|p| (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32).collect()
        }
    }
}

/// Run `f` against the framebuffer if a window exists.
fn with_surface(f: impl FnOnce(&mut Surface)) {
    let mut screen = shared().screen.lock().unwrap();
    if let Some(s) = screen.surface.as_mut() {
        f(s);
        LOCAL.with(|l| l.borrow_mut().dirty = true);
    }
}

/// Publish drawing done so far to the front-end.
fn flush() {
    if !LOCAL.with(|l| std::mem::take(&mut l.borrow_mut().dirty)) {
        return;
    }
    let sh = shared();
    dump_framebuffer(&sh.screen.lock().unwrap());
    sh.frontend.notify(Event::Updated);
}

/// Debug aid: with VINTRDP_DUMP=/path/file.ppm set, write the framebuffer
/// there after every update so sessions can be checked headlessly.
fn dump_framebuffer(screen: &crate::session::Screen) {
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    let Some(path) = PATH.get_or_init(|| std::env::var("VINTRDP_DUMP").ok()) else { return };
    let Some(s) = &screen.surface else { return };
    let mut out = format!("P6\n{} {}\n255\n", s.width, s.height).into_bytes();
    out.extend(s.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]));
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, out).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

// ---------------------------------------------------------------------------
// Lifecycle and window

#[unsafe(no_mangle)]
pub extern "C" fn ui_init() -> RD_BOOL {
    // SAFETY: rdesktop thread, before any session is set up.
    unsafe {
        if g_server_depth == -1 {
            g_server_depth = 24;
        }
    }
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_deinit() {}

fn screen_size() -> (u32, u32) {
    shared().frontend.screen_size()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_get_screen_size(width: *mut u32, height: *mut u32) {
    let (w, h) = screen_size();
    // SAFETY: rdesktop passes valid out-pointers.
    unsafe {
        *width = w;
        *height = h;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_get_screen_size_from_percentage(pw: u32, ph: u32, width: *mut u32, height: *mut u32) {
    let (w, h) = screen_size();
    // SAFETY: rdesktop passes valid out-pointers.
    unsafe {
        *width = w * pw / 100;
        *height = h * ph / 100;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_get_workarea_size(width: *mut u32, height: *mut u32) {
    let (w, h) = screen_size();
    // Leave room for the menu bar, title bar and Dock.
    // SAFETY: rdesktop passes valid out-pointers.
    unsafe {
        *width = w;
        *height = h.saturating_sub(120);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_get_monitor_layout(
    _monitors: *mut RDP_MONITOR_LAYOUT,
    _max: u32,
    _count: *mut u32,
    _w: *mut u32,
    _h: *mut u32,
) -> RD_BOOL {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_create_window(width: u32, height: u32) -> RD_BOOL {
    let sh = shared();
    sh.screen.lock().unwrap().surface = Some(Surface::new(width as usize, height as usize));
    // SAFETY: rdesktop thread; by now the protocol version is negotiated.
    let rdp4 = unsafe { crate::state::g_rdp_version } == crate::state::RDP_V4;
    sh.frontend.notify(Event::Title(sh.config.session_title(rdp4)));
    sh.frontend.notify(Event::Resized { width, height });
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_resize_window(width: u32, height: u32) {
    let sh = shared();
    if let Some(s) = sh.screen.lock().unwrap().surface.as_mut() {
        s.resize(width as usize, height as usize);
    }
    sh.frontend.notify(Event::Resized { width, height });
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_destroy_window() {
    shared().screen.lock().unwrap().surface = None;
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_have_window() -> RD_BOOL {
    shared().screen.lock().unwrap().surface.is_some() as RD_BOOL
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_update_window_sizehints(_width: u32, _height: u32) {}

#[unsafe(no_mangle)]
pub extern "C" fn ui_release_keyboard() {}

#[unsafe(no_mangle)]
pub extern "C" fn ui_bell() {
    shared().frontend.notify(Event::Bell);
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_move_pointer(_x: c_int, _y: c_int) {}

#[unsafe(no_mangle)]
pub extern "C" fn ui_begin_update() {}

#[unsafe(no_mangle)]
pub extern "C" fn ui_end_update() {
    flush();
}

// ---------------------------------------------------------------------------
// Event loop

/// Called by tcp_recv() whenever it needs data. Pumps UI input to the server
/// and services rdpdr/addin/ctrl descriptors until the RDP socket is readable.
#[unsafe(no_mangle)]
pub extern "C" fn ui_select(rdp_socket: c_int) {
    let sh = shared();
    loop {
        // SAFETY: rdesktop's globals, only touched from this thread.
        unsafe {
            if g_exit_mainloop != 0 {
                return;
            }
            if sh.quit_requested() {
                g_user_quit = 1;
                g_exit_mainloop = 1;
                g_pending_resize = 0;
                return;
            }
        }

        let input = sh.take_input();
        if !input.is_empty() {
            let t = LOCAL.with(|l| l.borrow().start.elapsed().as_millis() as u32);
            for ev in input {
                // SAFETY: rdesktop is connected (we're inside its main loop).
                unsafe { rdp_send_input(t, ev.message_type, ev.flags, ev.param1, ev.param2) };
            }
        }

        // Anything drawn outside a begin/end_update pair still gets shown.
        flush();

        if process_fds(rdp_socket, 60_000) {
            return;
        }
    }
}

/// Port of xwin.c's process_fds(), with our wake pipe instead of the X socket.
fn process_fds(rdp_socket: c_int, timeout_ms: c_int) -> bool {
    let sh = shared();
    let wake = sh.wake_fd();
    // SAFETY: fd_set/timeval manipulation and rdesktop's fd hooks, all on the
    // rdesktop thread with valid pointers to locals.
    unsafe {
        let mut rfds: libc::fd_set = std::mem::zeroed();
        let mut wfds: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&mut rfds);
        libc::FD_ZERO(&mut wfds);
        libc::FD_SET(rdp_socket, &mut rfds);
        libc::FD_SET(wake, &mut rfds);
        let mut n = rdp_socket.max(wake);

        let ms = rdp_heartbeat_select_timeout(timeout_ms);
        let mut tv = libc::timeval { tv_sec: (ms / 1000) as _, tv_usec: ((ms % 1000) * 1000) as _ };
        let mut s_timeout: RD_BOOL = 0;

        rdpdr_add_fds(&mut n, &mut rfds, &mut wfds, &mut tv, &mut s_timeout);
        addin_add_fds(&mut n, &mut rfds, &mut wfds);
        seamless_select_timeout(&mut tv);
        ctrl_add_fds(&mut n, &mut rfds);

        let ret = libc::select(n + 1, &mut rfds, &mut wfds, ptr::null_mut(), &mut tv);
        rdp_heartbeat_check_timeout();
        if ret <= 0 {
            if ret == 0 || s_timeout != 0 {
                rdpdr_check_fds(&mut rfds, &mut wfds, s_timeout);
            }
            return false;
        }

        rdpdr_check_fds(&mut rfds, &mut wfds, 0);
        addin_check_fds(&mut rfds, &mut wfds);
        ctrl_check_fds(&mut rfds, &mut wfds);

        if libc::FD_ISSET(wake, &rfds) {
            sh.drain_wake();
        }
        libc::FD_ISSET(rdp_socket, &rfds)
    }
}

// ---------------------------------------------------------------------------
// Bitmaps, glyphs, palettes

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_create_bitmap(width: c_int, height: c_int, data: *const u8) -> RD_HBITMAP {
    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
    // SAFETY: rdesktop passes width*height pixels in the session depth.
    let px = unsafe { convert(w, h, data) };
    Box::into_raw(Box::new(Bitmap { width: w, height: h, px })).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_update_bitmap(bmp: RD_HBITMAP, width: c_int, height: c_int, data: *const u8) -> RD_BOOL {
    if bmp.is_null() {
        return 0;
    }
    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
    // SAFETY: bmp came from ui_create_bitmap; data as for ui_create_bitmap.
    unsafe { *bmp.cast::<Bitmap>() = Bitmap { width: w, height: h, px: convert(w, h, data) } };
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_destroy_bitmap(bmp: RD_HBITMAP) {
    if !bmp.is_null() {
        // SAFETY: bmp came from ui_create_bitmap and is destroyed once.
        drop(unsafe { Box::from_raw(bmp.cast::<Bitmap>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_paint_bitmap(
    x: c_int,
    y: c_int,
    cx: c_int,
    cy: c_int,
    width: c_int,
    height: c_int,
    data: *const u8,
) {
    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
    // SAFETY: rdesktop passes width*height pixels.
    let px = unsafe { convert(w, h, data) };
    with_surface(|s| s.blit(ROP2_COPY, x, y, cx, cy, &px, w, h, 0, 0));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_create_glyph(width: c_int, height: c_int, data: *const u8) -> RD_HGLYPH {
    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
    let stride = w.div_ceil(8);
    let bits = if data.is_null() {
        vec![0; stride * h]
    } else {
        // SAFETY: rdesktop passes ((width+7)/8)*height bytes.
        unsafe { slice::from_raw_parts(data, stride * h) }.to_vec()
    };
    Box::into_raw(Box::new(Glyph { width: w, height: h, stride, bits })).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_destroy_glyph(glyph: RD_HGLYPH) {
    if !glyph.is_null() {
        // SAFETY: glyph came from ui_create_glyph and is destroyed once.
        drop(unsafe { Box::from_raw(glyph.cast::<Glyph>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_create_colourmap(colours: *const COLOURMAP) -> RD_HCOLOURMAP {
    let mut pal = Box::new([0u32; 256]);
    // SAFETY: rdesktop passes a valid COLOURMAP with ncolours entries.
    unsafe {
        let cm = &*colours;
        let entries = slice::from_raw_parts(cm.colours, (cm.ncolours as usize).min(256));
        for (p, e) in pal.iter_mut().zip(entries) {
            *p = (e.red as u32) << 16 | (e.green as u32) << 8 | e.blue as u32;
        }
    }
    Box::into_raw(pal).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_destroy_colourmap(map: RD_HCOLOURMAP) {
    if !map.is_null() {
        // SAFETY: map came from ui_create_colourmap and is destroyed once.
        drop(unsafe { Box::from_raw(map.cast::<[u32; 256]>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_set_colourmap(map: RD_HCOLOURMAP) {
    if !map.is_null() {
        // SAFETY: map came from ui_create_colourmap.
        let pal = unsafe { *map.cast::<[u32; 256]>() };
        LOCAL.with(|l| l.borrow_mut().palette = pal);
    }
}

// ---------------------------------------------------------------------------
// Cursors

const GET_BIT: fn(&[u8], usize) -> bool = |m, i| m.get(i / 8).is_some_and(|b| b & (0x80 >> (i % 8)) != 0);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_create_cursor(
    xhot: c_uint,
    yhot: c_uint,
    width: u32,
    height: u32,
    andmask: *const u8,
    xormask: *const u8,
    bpp: c_int,
) -> RD_HCURSOR {
    let (w, h) = (width as usize, height as usize);
    if !matches!(bpp, 1 | 16 | 24 | 32) || w == 0 || h == 0 || andmask.is_null() || xormask.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: rdesktop passes masks sized for width*height at the given bpp.
    let (and, xor) = unsafe {
        (
            slice::from_raw_parts(andmask, (w * h).div_ceil(8)),
            slice::from_raw_parts(xormask, if bpp == 1 { (w * h).div_ceil(8) } else { w * h * (bpp as usize / 8) }),
        )
    };

    // Same pixel rules as xwin.c's get_pixel(): AND=1 & XOR!=0 is an
    // "invert" pixel, which we render black with a white outline.
    let mut argb = vec![0u32; w * h];
    let mut inverted = false;
    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let oidx = if bpp == 1 { idx } else { (h - 1 - y) * w + x };
            let transparent = GET_BIT(and, idx);
            let (rgb, alpha) = match bpp {
                1 => (if GET_BIT(xor, idx) { 0xffffff } else { 0 }, 0xff),
                16 => (colour16(u16::from_le_bytes([xor[idx * 2], xor[idx * 2 + 1]])), 0xff),
                24 => ((xor[idx * 3 + 2] as u32) << 16 | (xor[idx * 3 + 1] as u32) << 8 | xor[idx * 3] as u32, 0xff),
                _ => {
                    let p = &xor[idx * 4..idx * 4 + 4];
                    argb[oidx] = (p[3] as u32) << 24 | (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32;
                    continue;
                }
            };
            argb[oidx] = if transparent && rgb != 0 {
                inverted = true;
                0xff00_0000
            } else if transparent {
                0
            } else {
                alpha << 24 | rgb
            };
        }
    }
    if inverted {
        let src = argb.clone();
        for y in 0..h {
            for x in 0..w {
                if src[y * w + x] >> 24 != 0 {
                    continue;
                }
                let near = [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)].iter().any(|(dx, dy)| {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h && src[ny as usize * w + nx as usize] >> 24 != 0
                });
                if near {
                    argb[y * w + x] = 0xffff_ffff;
                }
            }
        }
    }

    let rgba = argb.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]).collect();
    let image = Arc::new(CursorImage { width: w, height: h, hot_x: xhot as usize, hot_y: yhot as usize, rgba });
    Box::into_raw(Box::new(image)).cast()
}

fn colour16(c: u16) -> u32 {
    let c = c as u32;
    let r = ((c >> 8) & 0xf8) | ((c >> 13) & 0x7);
    let g = ((c >> 3) & 0xfc) | ((c >> 9) & 0x3);
    let b = ((c << 3) & 0xf8) | ((c >> 2) & 0x7);
    r << 16 | g << 8 | b
}

fn set_cursor(shape: CursorShape) {
    let sh = shared();
    sh.screen.lock().unwrap().cursor = shape;
    sh.frontend.notify(Event::Cursor);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_set_cursor(cursor: RD_HCURSOR) {
    // SAFETY: rdesktop global, read on its own thread.
    if unsafe { g_local_cursor } != 0 {
        return;
    }
    if cursor.is_null() {
        set_cursor(CursorShape::Hidden);
    } else {
        // SAFETY: cursor came from ui_create_cursor.
        set_cursor(CursorShape::Image(unsafe { (*cursor.cast::<Arc<CursorImage>>()).clone() }));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_destroy_cursor(cursor: RD_HCURSOR) {
    if !cursor.is_null() {
        // SAFETY: cursor came from ui_create_cursor and is destroyed once.
        drop(unsafe { Box::from_raw(cursor.cast::<Arc<CursorImage>>()) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_set_null_cursor() {
    set_cursor(CursorShape::Hidden);
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_set_standard_cursor() {
    set_cursor(CursorShape::Default);
}

// ---------------------------------------------------------------------------
// Drawing

#[unsafe(no_mangle)]
pub extern "C" fn ui_set_clip(x: c_int, y: c_int, cx: c_int, cy: c_int) {
    with_surface(|s| s.set_clip(x, y, cx, cy));
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_reset_clip() {
    with_surface(|s| s.reset_clip());
}

const HATCH_PATTERNS: [[u8; 8]; 6] = [
    [0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0x00], // horizontal
    [0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08], // vertical
    [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01], // forward diagonal
    [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80], // backward diagonal
    [0x08, 0x08, 0x08, 0xff, 0x08, 0x08, 0x08, 0x08], // cross
    [0x81, 0x42, 0x24, 0x18, 0x18, 0x24, 0x42, 0x81], // diagonal cross
];

/// Same brush interpretation as xwin.c's ui_patblt().
unsafe fn brush_pattern(brush: *const BRUSH, bg: u32, fg: u32) -> Option<Pattern> {
    if brush.is_null() {
        return Some(Pattern::Solid(colour(fg)));
    }
    // SAFETY: rdesktop passes a valid BRUSH (and BRUSHDATA when bd != NULL).
    let b = unsafe { &*brush };
    let (ox, oy) = (b.xorigin as i32, b.yorigin as i32);
    match b.style {
        0 => Some(Pattern::Solid(colour(fg))),
        2 => Some(Pattern::Stipple {
            rows: HATCH_PATTERNS[(b.pattern[0] as usize).min(5)],
            set: colour(fg),
            clear: Some(colour(bg)),
            ox,
            oy,
        }),
        3 if b.bd.is_null() => {
            // RDP4 brush: rows are stored bottom-up; colours are swapped.
            let mut rows = [0u8; 8];
            for i in 0..8 {
                rows[7 - i] = b.pattern[i];
            }
            Some(Pattern::Stipple { rows, set: colour(bg), clear: Some(colour(fg)), ox, oy })
        }
        3 => {
            // SAFETY: bd is non-null and owned by rdesktop's brush cache.
            let bd = unsafe { &*b.bd };
            if bd.colour_code > 1 {
                // SAFETY: an 8x8 brush in the session colour depth.
                let px = unsafe { convert(8, 8, bd.data) };
                Some(Pattern::Tile { px: px.into_boxed_slice().try_into().ok()?, ox, oy })
            } else {
                let mut rows = [0u8; 8];
                if !bd.data.is_null() {
                    // SAFETY: a 1bpp 8x8 brush is 8 bytes.
                    rows.copy_from_slice(unsafe { slice::from_raw_parts(bd.data, 8) });
                }
                Some(Pattern::Stipple { rows, set: colour(bg), clear: Some(colour(fg)), ox, oy })
            }
        }
        style => {
            eprintln!("vintrdp: unimplemented brush style {style}");
            None
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_destblt(opcode: u8, x: c_int, y: c_int, cx: c_int, cy: c_int) {
    with_surface(|s| s.fill(opcode, x, y, cx, cy, &Pattern::Solid(0)));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_patblt(
    opcode: u8,
    x: c_int,
    y: c_int,
    cx: c_int,
    cy: c_int,
    brush: *const BRUSH,
    bgcolour: u32,
    fgcolour: u32,
) {
    // SAFETY: forwarded from rdesktop.
    if let Some(p) = unsafe { brush_pattern(brush, bgcolour, fgcolour) } {
        with_surface(|s| s.fill(opcode, x, y, cx, cy, &p));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_screenblt(opcode: u8, x: c_int, y: c_int, cx: c_int, cy: c_int, srcx: c_int, srcy: c_int) {
    with_surface(|s| s.screen_blt(opcode, x, y, cx, cy, srcx, srcy));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_memblt(
    opcode: u8,
    x: c_int,
    y: c_int,
    cx: c_int,
    cy: c_int,
    src: RD_HBITMAP,
    srcx: c_int,
    srcy: c_int,
) {
    if src.is_null() {
        return;
    }
    // SAFETY: src came from ui_create_bitmap and lives in rdesktop's cache.
    let bmp = unsafe { &*src.cast::<Bitmap>() };
    with_surface(|s| s.blit(opcode, x, y, cx, cy, &bmp.px, bmp.width, bmp.height, srcx, srcy));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_triblt(
    opcode: u8,
    x: c_int,
    y: c_int,
    cx: c_int,
    cy: c_int,
    src: RD_HBITMAP,
    srcx: c_int,
    srcy: c_int,
    brush: *const BRUSH,
    bgcolour: u32,
    fgcolour: u32,
) {
    // Decomposed into ROP2 steps exactly as xwin.c does.
    // SAFETY: all arguments forwarded from rdesktop.
    unsafe {
        match opcode {
            0x69 => {
                // PDSxxn
                ui_memblt(ROP2_XOR, x, y, cx, cy, src, srcx, srcy);
                ui_patblt(ROP2_NXOR, x, y, cx, cy, brush, bgcolour, fgcolour);
            }
            0xb8 => {
                // PSDPxax
                ui_patblt(ROP2_XOR, x, y, cx, cy, brush, bgcolour, fgcolour);
                ui_memblt(ROP2_AND, x, y, cx, cy, src, srcx, srcy);
                ui_patblt(ROP2_XOR, x, y, cx, cy, brush, bgcolour, fgcolour);
            }
            0xc0 => {
                // PSa
                ui_memblt(ROP2_COPY, x, y, cx, cy, src, srcx, srcy);
                ui_patblt(ROP2_AND, x, y, cx, cy, brush, bgcolour, fgcolour);
            }
            _ => {
                eprintln!("vintrdp: unimplemented triblt opcode {opcode:#x}");
                ui_memblt(ROP2_COPY, x, y, cx, cy, src, srcx, srcy);
            }
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_line(opcode: u8, startx: c_int, starty: c_int, endx: c_int, endy: c_int, pen: *const PEN) {
    // SAFETY: rdesktop passes a valid PEN.
    let c = colour(unsafe { (*pen).colour });
    with_surface(|s| s.line(opcode, startx, starty, endx, endy, c));
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_rect(x: c_int, y: c_int, cx: c_int, cy: c_int, colour_: u32) {
    let c = colour(colour_);
    with_surface(|s| s.fill(ROP2_COPY, x, y, cx, cy, &Pattern::Solid(c)));
}

/// RD_POINT arrays use X11's CoordModePrevious: each point is a delta.
unsafe fn absolute_points(points: *const RD_POINT, npoints: c_int) -> Vec<(i32, i32)> {
    if points.is_null() || npoints <= 0 {
        return Vec::new();
    }
    // SAFETY: rdesktop passes npoints points.
    let pts = unsafe { slice::from_raw_parts(points, npoints as usize) };
    let (mut x, mut y) = (0i32, 0i32);
    pts.iter()
        .map(|p| {
            x += p.x as i32;
            y += p.y as i32;
            (x, y)
        })
        .collect()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_polygon(
    opcode: u8,
    fillmode: u8,
    point: *const RD_POINT,
    npoints: c_int,
    brush: *const BRUSH,
    bgcolour: u32,
    fgcolour: u32,
) {
    // SAFETY: forwarded from rdesktop.
    let (pts, pat) = unsafe { (absolute_points(point, npoints), brush_pattern(brush, bgcolour, fgcolour)) };
    if let Some(p) = pat {
        with_surface(|s| s.polygon(opcode, &pts, fillmode == ALTERNATE, &p));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_polyline(opcode: u8, points: *const RD_POINT, npoints: c_int, pen: *const PEN) {
    // SAFETY: forwarded from rdesktop.
    let (pts, c) = unsafe { (absolute_points(points, npoints), colour((*pen).colour)) };
    with_surface(|s| {
        for w in pts.windows(2) {
            s.line(opcode, w[0].0, w[0].1, w[1].0, w[1].1, c);
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_ellipse(
    opcode: u8,
    fillmode: u8,
    x: c_int,
    y: c_int,
    cx: c_int,
    cy: c_int,
    brush: *const BRUSH,
    bgcolour: u32,
    fgcolour: u32,
) {
    // SAFETY: forwarded from rdesktop.
    if let Some(p) = unsafe { brush_pattern(brush, bgcolour, fgcolour) } {
        with_surface(|s| s.ellipse(opcode, x, y, cx, cy, fillmode == 1, &p));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_draw_glyph(
    mixmode: c_int,
    x: c_int,
    y: c_int,
    cx: c_int,
    cy: c_int,
    glyph: RD_HGLYPH,
    _srcx: c_int,
    _srcy: c_int,
    bgcolour: u32,
    fgcolour: u32,
) {
    if glyph.is_null() {
        return;
    }
    // SAFETY: glyph came from ui_create_glyph.
    let g = unsafe { &*glyph.cast::<Glyph>() };
    let bg = (mixmode != MIX_TRANSPARENT).then(|| colour(bgcolour));
    let fg = colour(fgcolour);
    with_surface(|s| s.glyph(x, y, cx, cy, g, fg, bg));
}

/// Port of ui_draw_text() and its DO_GLYPH macro from xwin.c.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_draw_text(
    font: u8,
    flags: u8,
    _opcode: u8,
    mixmode: c_int,
    x: c_int,
    y: c_int,
    clipx: c_int,
    clipy: c_int,
    clipcx: c_int,
    clipcy: c_int,
    boxx: c_int,
    boxy: c_int,
    mut boxcx: c_int,
    boxcy: c_int,
    _brush: *const BRUSH,
    bgcolour: u32,
    fgcolour: u32,
    text: *const u8,
    length: u8,
) {
    let (bg, fg) = (colour(bgcolour), colour(fgcolour));
    let sh = shared();
    let mut screen = sh.screen.lock().unwrap();
    let Some(s) = screen.surface.as_mut() else { return };

    let wnd_width = s.width as c_int;
    if boxx + boxcx > wnd_width {
        boxcx = wnd_width - boxx;
    }
    if boxcx > 1 {
        s.fill(ROP2_COPY, boxx, boxy, boxcx, boxcy, &Pattern::Solid(bg));
    } else if mixmode == MIX_OPAQUE {
        s.fill(ROP2_COPY, clipx, clipy, clipcx, clipcy, &Pattern::Solid(bg));
    }

    let mut pos = (x, y);
    let vertical = flags & TEXT2_VERTICAL != 0;
    let implicit_x = flags & TEXT2_IMPLICIT_X != 0;

    let do_glyph = |s: &mut Surface, pos: &mut (c_int, c_int), t: &[u8], idx: &mut usize| {
        let at = |i: usize| t.get(i).copied().unwrap_or(0) as c_int;
        // SAFETY: rdesktop's glyph cache; returns NULL for empty slots.
        let glyph = unsafe { cache_get_font(font, at(*idx) as u16) };
        if !implicit_x {
            *idx += 1;
            let xyoffset = at(*idx);
            let delta = if xyoffset & 0x80 != 0 {
                let d = at(*idx + 1) | (at(*idx + 2) << 8);
                *idx += 2;
                d
            } else {
                xyoffset
            };
            if vertical { pos.1 += delta } else { pos.0 += delta }
        }
        if !glyph.is_null() {
            // SAFETY: non-null FONTGLYPH from the cache; pixmap from ui_create_glyph.
            let g = unsafe { &*glyph };
            if !g.pixmap.is_null() {
                let bits = unsafe { &*g.pixmap.cast::<Glyph>() };
                s.glyph(pos.0 + g.offset as c_int, pos.1 + g.baseline as c_int, g.width as c_int, g.height as c_int, bits, fg, None);
            }
            if implicit_x {
                pos.0 += g.width as c_int;
            }
        }
    };

    if text.is_null() {
        return;
    }
    // SAFETY: rdesktop passes `length` bytes of text.
    let mut t: &[u8] = unsafe { slice::from_raw_parts(text, length as usize) };
    let mut i = 0usize;
    while i < t.len() {
        match t[i] {
            0xff => {
                if i + 3 > t.len() {
                    break;
                }
                // SAFETY: stores a copy of the first t[i+2] bytes in the text cache.
                unsafe { cache_put_text(t[i + 1], t.as_ptr().cast::<c_void>(), t[i + 2] as c_int) };
                t = &t[i + 3..];
                i = 0;
            }
            0xfe => {
                if i + 2 > t.len() {
                    break;
                }
                // SAFETY: returns a pointer into rdesktop's text cache.
                let entry = unsafe { &*cache_get_text(t[i + 1]) };
                if !entry.data.is_null() && entry.size > 0 {
                    // SAFETY: cache entry of `size` bytes.
                    let data = unsafe { slice::from_raw_parts(entry.data, entry.size as usize) };
                    if data.get(1) == Some(&0) && !implicit_x && i + 2 < t.len() {
                        if vertical { pos.1 += t[i + 2] as c_int } else { pos.0 += t[i + 2] as c_int }
                    }
                    let mut j = 0;
                    while j < data.len() {
                        do_glyph(s, &mut pos, data, &mut j);
                        j += 1;
                    }
                }
                let consumed = if i + 2 < t.len() { i + 3 } else { i + 2 };
                t = &t[consumed..];
                i = 0;
            }
            _ => {
                do_glyph(s, &mut pos, t, &mut i);
                i += 1;
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_desktop_save(offset: u32, x: c_int, y: c_int, cx: c_int, cy: c_int) {
    let mut bytes = Vec::new();
    with_surface(|s| bytes = s.read(x, y, cx, cy).iter().flat_map(|p| p.to_le_bytes()).collect());
    if bytes.is_empty() {
        return;
    }
    // SAFETY: bytes holds cx*cy 4-byte pixels; rdesktop copies them.
    unsafe { cache_put_desktop(offset * 4, cx, cy, cx * 4, 4, bytes.as_ptr()) };
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_desktop_restore(offset: u32, x: c_int, y: c_int, cx: c_int, cy: c_int) {
    // SAFETY: returns NULL or cx*cy*4 bytes inside rdesktop's desktop cache.
    let data = unsafe { cache_get_desktop(offset * 4, cx, cy, 4) };
    if data.is_null() || cx <= 0 || cy <= 0 {
        return;
    }
    let n = (cx * cy) as usize;
    // SAFETY: as above.
    let px: Vec<u32> = unsafe { slice::from_raw_parts(data, n * 4) }
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    with_surface(|s| s.blit(ROP2_COPY, x, y, cx, cy, &px, cx as usize, cy as usize, 0, 0));
}

// ---------------------------------------------------------------------------
// Keyboard state, dialogs, clipboard and seamless hooks

/// Lock-key state for the sync event (not tracked yet).
#[unsafe(no_mangle)]
pub extern "C" fn read_keyboard_state() -> c_uint {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_get_numlock_state(_state: c_uint) -> u16 {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_seamless_end() {}

unsafe fn c_string(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: rdesktop passes NUL-terminated strings.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// Certificate and similar prompts (see util_dialog_choice in utils.c).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xgui_choice_dialog(
    title: *const c_char,
    message: *const c_char,
    accept: *const c_char,
    reject: *const c_char,
) -> RD_BOOL {
    // SAFETY: NUL-terminated strings from rdesktop.
    let (t, m, a, r) = unsafe { (c_string(title), c_string(message), c_string(accept), c_string(reject)) };
    let t = t.replace("rdesktop", "vintrdp");
    shared().frontend.ask(&t, m.trim(), &a, &r) as RD_BOOL
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn xgui_message_dialog(title: *const c_char, message: *const c_char, _button: *const c_char) {
    // SAFETY: NUL-terminated strings from rdesktop.
    let text = unsafe { format!("{}: {}", c_string(title), c_string(message)) };
    shared().frontend.notify(Event::Message(text));
}

// ---------------------------------------------------------------------------
// Clipboard and seamless windows: not implemented by this front-end yet.

#[unsafe(no_mangle)]
pub extern "C" fn ui_clip_format_announce(_data: *const u8, _length: u32) {}
#[unsafe(no_mangle)]
pub extern "C" fn ui_clip_handle_data(_data: *const u8, _length: u32) {}
#[unsafe(no_mangle)]
pub extern "C" fn ui_clip_request_failed() {}
#[unsafe(no_mangle)]
pub extern "C" fn ui_clip_request_data(_format: u32) {}
#[unsafe(no_mangle)]
pub extern "C" fn ui_clip_sync() {}

macro_rules! seamless_stubs {
    ($($name:ident($($arg:ident: $ty:ty),*);)*) => {
        $(
            #[unsafe(no_mangle)]
            pub extern "C" fn $name($(_: $ty),*) {}
        )*
    };
}

seamless_stubs! {
    ui_seamless_begin(hidden: RD_BOOL);
    ui_seamless_hide_desktop();
    ui_seamless_unhide_desktop();
    ui_seamless_toggle();
    ui_seamless_create_window(id: libc::c_ulong, group: libc::c_ulong, parent: libc::c_ulong, flags: libc::c_ulong);
    ui_seamless_destroy_window(id: libc::c_ulong, flags: libc::c_ulong);
    ui_seamless_destroy_group(id: libc::c_ulong, flags: libc::c_ulong);
    ui_seamless_seticon(id: libc::c_ulong, format: *const c_char, width: c_int, height: c_int, chunk: c_int, data: *const c_char, size: c_int);
    ui_seamless_delicon(id: libc::c_ulong, format: *const c_char, width: c_int, height: c_int);
    ui_seamless_move_window(id: libc::c_ulong, x: c_int, y: c_int, width: c_int, height: c_int, flags: libc::c_ulong);
    ui_seamless_restack_window(id: libc::c_ulong, behind: libc::c_ulong, flags: libc::c_ulong);
    ui_seamless_settitle(id: libc::c_ulong, title: *const c_char, flags: libc::c_ulong);
    ui_seamless_setstate(id: libc::c_ulong, state: c_uint, flags: libc::c_ulong);
    ui_seamless_syncbegin(flags: libc::c_ulong);
    ui_seamless_ack(serial: c_uint);
}
