//! Declarations mirroring rdesktop's C types (types.h) and the parts of its
//! core that the front-end calls back into.

#![allow(non_camel_case_types, non_upper_case_globals, dead_code, clippy::upper_case_acronyms)]

use std::os::raw::{c_char, c_int, c_void};

pub type RD_BOOL = c_int;
pub type RD_HBITMAP = *mut c_void;
pub type RD_HGLYPH = *mut c_void;
pub type RD_HCOLOURMAP = *mut c_void;
pub type RD_HCURSOR = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RD_POINT {
    pub x: i16,
    pub y: i16,
}

#[repr(C)]
pub struct COLOURENTRY {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[repr(C)]
pub struct COLOURMAP {
    pub ncolours: u16,
    pub colours: *const COLOURENTRY,
}

#[repr(C)]
pub struct PEN {
    pub style: u8,
    pub width: u8,
    pub colour: u32,
}

#[repr(C)]
pub struct BRUSHDATA {
    pub colour_code: u32,
    pub data_size: u32,
    pub data: *const u8,
}

#[repr(C)]
pub struct BRUSH {
    pub xorigin: u8,
    pub yorigin: u8,
    pub style: u8,
    pub pattern: [u8; 8],
    pub bd: *const BRUSHDATA,
}

#[repr(C)]
pub struct FONTGLYPH {
    pub offset: i16,
    pub baseline: i16,
    pub width: u16,
    pub height: u16,
    pub pixmap: RD_HGLYPH,
}

#[repr(C)]
pub struct DATABLOB {
    pub data: *const u8,
    pub size: c_int,
}

#[repr(C)]
pub struct RDP_MONITOR_LAYOUT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub flags: u32,
}

// constants.h
pub const RDP_INPUT_SYNCHRONIZE: u16 = 0;
pub const RDP_INPUT_SCANCODE: u16 = 4;
pub const RDP_INPUT_MOUSE: u16 = 0x8001;

pub const KBD_FLAG_EXT: u16 = 0x0100;
pub const KBD_FLAG_DOWN: u16 = 0x4000;
pub const KBD_FLAG_UP: u16 = 0x8000;
pub const RDP_KEYPRESS: u16 = 0;
pub const RDP_KEYRELEASE: u16 = KBD_FLAG_DOWN | KBD_FLAG_UP;

pub const MOUSE_FLAG_MOVE: u16 = 0x0800;
pub const MOUSE_FLAG_BUTTON1: u16 = 0x1000;
pub const MOUSE_FLAG_BUTTON2: u16 = 0x2000;
pub const MOUSE_FLAG_BUTTON3: u16 = 0x4000;
pub const MOUSE_FLAG_BUTTON4: u16 = 0x0278;
pub const MOUSE_FLAG_BUTTON5: u16 = 0x0388;
pub const MOUSE_FLAG_DOWN: u16 = 0x8000;

pub const ROP2_COPY: u8 = 0xc;
pub const ROP2_XOR: u8 = 0x6;
pub const ROP2_AND: u8 = 0x8;
pub const ROP2_NXOR: u8 = 0x9;

pub const MIX_TRANSPARENT: c_int = 0;
pub const MIX_OPAQUE: c_int = 1;

pub const TEXT2_VERTICAL: u8 = 0x04;
pub const TEXT2_IMPLICIT_X: u8 = 0x20;

pub const ALTERNATE: u8 = 1;

/// C's time_t. 64-bit in every Windows CRT we target, whatever the libc crate says.
#[cfg(windows)]
#[allow(non_camel_case_types)]
pub type time_t = i64;
#[cfg(unix)]
#[allow(non_camel_case_types)]
pub type time_t = libc::time_t;

unsafe extern "C" {
    // Session lifecycle (rdp.c, tcp.c, addin.c, rdpdr.c, ...)
    pub fn rdp_connect(
        server: *mut c_char,
        flags: u32,
        domain: *mut c_char,
        password: *mut c_char,
        command: *mut c_char,
        directory: *mut c_char,
        reconnect: RD_BOOL,
    ) -> RD_BOOL;
    pub fn rdp_main_loop(deactivated: *mut RD_BOOL, ext_disc_reason: *mut u32);
    pub fn rdp_disconnect();
    pub fn rdp_reset_state();
    pub fn tcp_run_ui(run: RD_BOOL);
    #[cfg(unix)]
    pub fn addin_init() -> RD_BOOL;
    #[cfg(unix)]
    pub fn addin_deinit();
    #[cfg(unix)]
    pub fn rdpdr_init() -> RD_BOOL;
    pub fn dvc_init() -> RD_BOOL;
    #[cfg(unix)]
    pub fn rdpear_init() -> RD_BOOL;
    pub fn rdpedisp_init();
    #[cfg(unix)]
    pub fn disk_enum_devices(id: *mut u32, optarg: *mut c_char) -> c_int;
    pub fn cache_save_state();
    pub fn pstcache_set_namespace(server: *const c_char, port: c_int, domain: *const c_char, username: *const c_char);
    pub fn utils_apply_session_size_limitations(width: *mut u32, height: *mut u32);
    pub fn logger_set_verbose(verbose: c_int);
    pub fn sec_hash_sha1_16(out: *mut u8, input: *mut u8, salt: *mut u8);
    pub fn sec_hash_to_string(out: *mut c_char, out_size: c_int, input: *mut u8, in_size: c_int);

    pub fn rdp_send_input(time: u32, message_type: u16, device_flags: u16, param1: u16, param2: u16);

    pub fn cache_get_font(font: u8, character: u16) -> *const FONTGLYPH;
    pub fn cache_get_text(cache_id: u8) -> *const DATABLOB;
    pub fn cache_put_text(cache_id: u8, data: *const c_void, length: c_int);
    pub fn cache_get_desktop(offset: u32, cx: c_int, cy: c_int, bytes_per_pixel: c_int) -> *const u8;
    pub fn cache_put_desktop(
        offset: u32,
        cx: c_int,
        cy: c_int,
        scanline: c_int,
        bytes_per_pixel: c_int,
        data: *const u8,
    );

    pub fn rdp_heartbeat_select_timeout(timeout_ms: c_int) -> c_int;
    pub fn rdp_heartbeat_check_timeout();
    #[cfg(unix)]
    pub fn rdpdr_add_fds(
        n: *mut c_int,
        rfds: *mut libc::fd_set,
        wfds: *mut libc::fd_set,
        tv: *mut libc::timeval,
        timeout: *mut RD_BOOL,
    );
    #[cfg(unix)]
    pub fn rdpdr_check_fds(rfds: *mut libc::fd_set, wfds: *mut libc::fd_set, timed_out: RD_BOOL);
    #[cfg(unix)]
    pub fn addin_add_fds(n: *mut c_int, rfds: *mut libc::fd_set, wfds: *mut libc::fd_set);
    #[cfg(unix)]
    pub fn addin_check_fds(rfds: *mut libc::fd_set, wfds: *mut libc::fd_set);
    pub fn seamless_select_timeout(tv: *mut libc::timeval);
    #[cfg(unix)]
    pub fn ctrl_add_fds(n: *mut c_int, rfds: *mut libc::fd_set);
    #[cfg(unix)]
    pub fn ctrl_check_fds(rfds: *mut libc::fd_set, wfds: *mut libc::fd_set);

    // Globals still owned by C files
    pub static mut g_exit_mainloop: RD_BOOL;
    pub static mut g_tcp_port_rdp: c_int;
    #[cfg(unix)]
    pub static mut g_num_devices: u32;
    pub static mut g_num_channels: libc::c_uint;
}
