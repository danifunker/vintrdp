//! The connection state rdesktop's C core reads, now owned by Rust.
//!
//! These used to be defined in rdesktop.c. The C files still reach them as
//! `extern` globals, so they keep their C names and layouts; `apply()` fills
//! them from a [`Config`] before each connection. Moving each C module from
//! these globals to an explicit session parameter happens module by module.

#![allow(non_upper_case_globals)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint};
use std::ptr;

use crate::config::{Config, RdpVersion};
use crate::ffi::{self, RD_BOOL};

const FALSE: RD_BOOL = 0;
const TRUE: RD_BOOL = 1;

/// RDP_VERSION enum values from types.h.
pub const RDP_V4: c_int = 4;
pub const RDP_V5: c_int = 5;

macro_rules! globals {
    ($($name:ident: $ty:ty = $init:expr;)*) => {
        $(
            #[unsafe(no_mangle)]
            pub static mut $name: $ty = $init;
        )*
    };
}

globals! {
    // User / server identity
    g_username: *mut c_char = ptr::null_mut();
    g_password: [c_char; 64] = [0; 64];
    g_hostname: [c_char; 16] = [0; 16];
    g_codepage: [c_char; 16] = [0; 16];
    g_tls_version: [c_char; 4] = [0; 4];

    // Protocol options
    g_rdp_version: c_int = RDP_V5;
    g_server_depth: c_int = -1;
    g_encryption: RD_BOOL = TRUE;
    g_encryption_initial: RD_BOOL = TRUE;
    g_bitmap_cache: RD_BOOL = TRUE;
    g_bitmap_cache_persist_enable: RD_BOOL = FALSE;
    g_bitmap_cache_precache: RD_BOOL = TRUE;
    g_desktop_save: RD_BOOL = TRUE;
    g_polygon_ellipse_orders: RD_BOOL = TRUE;
    g_console_session: RD_BOOL = FALSE;
    g_restricted_admin: RD_BOOL = FALSE;
    g_shadow_session: RD_BOOL = FALSE;
    g_rdp5_performanceflags: u32 = PERF_DISABLE_FULLWINDOWDRAG | PERF_DISABLE_MENUANIMATIONS | PERF_ENABLE_FONT_SMOOTHING;
    g_extended_client_data_supported: RD_BOOL = FALSE;
    g_dpi: c_int = 0;

    // Session geometry
    g_requested_session_width: u32 = 1024;
    g_requested_session_height: u32 = 768;
    g_fullscreen: RD_BOOL = FALSE;
    g_multimon: RD_BOOL = FALSE;
    g_pending_resize: RD_BOOL = FALSE;
    g_pending_resize_defer: RD_BOOL = TRUE;
    g_pending_resize_defer_timer: libc::timeval = libc::timeval { tv_sec: 0, tv_usec: 0 };

    // Keyboard
    g_keylayout: c_uint = 0x409;
    g_keyboard_type: c_int = 0x4;
    g_keyboard_subtype: c_int = 0x0;
    g_keyboard_functionkeys: c_int = 0xc;
    g_numlock_sync: RD_BOOL = FALSE;
    g_local_cursor: RD_BOOL = FALSE;

    // Connection lifecycle
    g_user_quit: RD_BOOL = FALSE;
    g_rdpsnd: RD_BOOL = FALSE;
    g_network_error: RD_BOOL = FALSE;
    g_use_gui_prompts: RD_BOOL = TRUE;
    g_gui_prompt_cancelled: RD_BOOL = FALSE;
    g_client_random: [u8; 32] = [0; 32];

    // Auto-reconnect cookie
    g_has_reconnect_random: RD_BOOL = FALSE;
    g_reconnect_random: [c_char; 16] = [0; 16];
    g_reconnect_random_ts: ffi::time_t = 0;
    g_reconnect_logonid: u32 = 0;

    // Session directory redirection (filled in by rdp.c)
    g_redirect: RD_BOOL = FALSE;
    g_redirect_server: *mut c_char = ptr::null_mut();
    g_redirect_server_len: u32 = 0;
    g_redirect_domain: *mut c_char = ptr::null_mut();
    g_redirect_domain_len: u32 = 0;
    g_redirect_username: *mut c_char = ptr::null_mut();
    g_redirect_username_len: u32 = 0;
    g_redirect_lb_info: *mut u8 = ptr::null_mut();
    g_redirect_lb_info_len: u32 = 0;
    g_redirect_cookie: *mut u8 = ptr::null_mut();
    g_redirect_cookie_len: u32 = 0;
    g_redirect_flags: u32 = 0;
    g_redirect_session_id: u32 = 0;

    // Features this front-end doesn't offer (yet)
    g_seamless_rdp: RD_BOOL = FALSE;
    g_remote_guard: RD_BOOL = FALSE;
    g_remote_guard_helper: *mut c_char = ptr::null_mut();
    g_remote_guard_server: *mut c_char = ptr::null_mut();
    g_use_password_as_pin: RD_BOOL = FALSE;
    g_sc_csp_name: *mut c_char = ptr::null_mut();
    g_sc_reader_name: *mut c_char = ptr::null_mut();
    g_sc_card_name: *mut c_char = ptr::null_mut();
    g_sc_container_name: *mut c_char = ptr::null_mut();
}

/// Salt for hashing the client hostname into licence file names.
#[unsafe(no_mangle)]
pub static g_static_rdesktop_salt_16: [u8; 16] =
    [0xb8, 0x82, 0x29, 0x31, 0xc5, 0x39, 0xd9, 0x44, 0x54, 0x15, 0x5e, 0x14, 0x71, 0x38, 0xd5, 0x4d];

// constants.h
pub const PERF_DISABLE_WALLPAPER: u32 = 0x01;
pub const PERF_DISABLE_FULLWINDOWDRAG: u32 = 0x02;
pub const PERF_DISABLE_MENUANIMATIONS: u32 = 0x04;
pub const PERF_DISABLE_THEMING: u32 = 0x08;
pub const PERF_DISABLE_CURSOR_SHADOW: u32 = 0x20;
pub const PERF_ENABLE_FONT_SMOOTHING: u32 = 0x80;

/// Copy a Rust string into a fixed C char array, truncating and NUL-terminating.
fn set_chars<const N: usize>(dst: &mut [c_char; N], s: &str) {
    *dst = [0; N];
    for (d, b) in dst.iter_mut().zip(s.bytes().take(N - 1)) {
        *d = b as c_char;
    }
}

/// Replace a malloc'ed C string global (rdesktop frees these with xfree()).
unsafe fn set_cstr(dst: &mut *mut c_char, s: &str) {
    let c = CString::new(s.replace('\0', "")).unwrap();
    // SAFETY: the old value is NULL or came from strdup (ours or rdesktop's).
    unsafe {
        libc::free((*dst).cast());
        *dst = libc::strdup(c.as_ptr());
    }
}

/// Fill rdesktop's globals from `config`. Must run on the rdesktop thread
/// before `rdp_connect()`.
pub(crate) unsafe fn apply(config: &Config) {
    crate::glue::set_imported_licence(config.licence.clone());
    // SAFETY: the C core only touches these from the rdesktop thread, which
    // is the caller; nothing else is running yet.
    unsafe {
        set_cstr(&mut *ptr::addr_of_mut!(g_username), &config.username);
        set_chars(&mut *ptr::addr_of_mut!(g_password), &config.password);
        set_chars(&mut *ptr::addr_of_mut!(g_hostname), &config.client_hostname());
        g_rdpsnd = config.sound as RD_BOOL;
        set_chars(&mut *ptr::addr_of_mut!(g_tls_version), config.tls_version.as_deref().unwrap_or(""));

        g_rdp_version = match config.version {
            RdpVersion::V4 => RDP_V4,
            RdpVersion::V5 | RdpVersion::Auto => RDP_V5,
        };
        g_server_depth = if config.version == RdpVersion::V4 { 8 } else { config.depth as c_int };
        let enc = config.encryption as RD_BOOL;
        g_encryption = enc;
        g_encryption_initial = enc;
        g_bitmap_cache = config.bitmap_cache as RD_BOOL;
        g_bitmap_cache_persist_enable = config.persistent_bitmap_cache as RD_BOOL;
        g_console_session = config.console as RD_BOOL;
        g_rdp5_performanceflags = config.experience.flags();
        g_requested_session_width = config.width;
        g_requested_session_height = config.height;
        g_keylayout = config.keyboard_layout;
        // Always: the front-end reports the real lock state (Session::sync_locks).
        g_numlock_sync = TRUE;
        g_local_cursor = config.local_cursor as RD_BOOL;
        if config.local_cursor {
            g_rdp5_performanceflags |= PERF_DISABLE_CURSOR_SHADOW;
        }

        ffi::g_tcp_port_rdp = config.port as c_int;
    }
}
