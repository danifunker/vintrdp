//! The RDP layer: the connection sequence, capabilities, data PDUs, input,
//! and the session loop. Port of rdesktop's rdp.c, rdp5.c, secure.c's
//! packet handling and rdesktop.c's main().

mod caps;
mod conn;
pub mod input;

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use conn::{Carry, Conn, End, ERRINFO_UNSET, Settings};
use crate::channels::{self, Registered};
use crate::config::RdpVersion;
use crate::error::Error;
use crate::graphics::Graphics;
use crate::session::{Event, LoopMsg, Outcome, Shared};

const ERRINFO_NO_INFO: u32 = 0;
const ERRINFO_LOGOFF_BYUSER: u32 = 0x000c;

const EX_PROTOCOL: i32 = 76;
const EXRD_WINDOW_CLOSED: i32 = 62;
const EXRD_UNKNOWN: i32 = 63;

/// How long to keep trying to get back to a session after a network error.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(3600 + 600);

fn ended(code: i32, message: impl Into<String>) -> Outcome {
    Outcome { code, message: message.into(), user_quit: false }
}

fn closed_by_user(message: &str) -> Outcome {
    Outcome { code: EXRD_WINDOW_CLOSED, message: message.into(), user_quit: true }
}

/// Sleep for `d`, or less if the user quits. True if they did.
fn pause(shared: &Shared, rx: &Receiver<LoopMsg>, d: Duration) -> bool {
    let until = Instant::now() + d;
    while !shared.quit_requested() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let _ = rx.recv_timeout(left);
    }
    true
}

/// The channels this session offers. RDP 4 has none.
fn channels(shared: &Arc<Shared>) -> Vec<Registered> {
    let c = &shared.config;
    let mut out = vec![Registered::new(Box::new(channels::rdpdr::Rdpdr::new(shared.clone())))];
    if c.clipboard {
        out.push(Registered::new(Box::new(channels::cliprdr::Cliprdr::new(shared.clone()))));
    }
    if c.sound && c.version != RdpVersion::V4 && channels::rdpsnd::available() {
        out.push(Registered::new(Box::new(channels::rdpsnd::Rdpsnd::new(shared.clone()))));
    }
    out
}

/// Run a session to the end: connect, reconnect, redirect, and report how it
/// ended. Port of rdesktop.c's main() as the old session.rs had it.
pub(crate) fn run(shared: &Arc<Shared>, rx: Receiver<LoopMsg>) -> Outcome {
    let config = &shared.config;
    let mut graphics = Graphics::new(shared.clone());
    let mut chans = channels(shared);
    let mut carry = Carry::default();

    let mut info_flags = conn::INFO_MOUSE | conn::INFO_DISABLECTRLALTDEL | conn::INFO_UNICODE | conn::INFO_MAXIMIZESHELL | conn::INFO_ENABLEWINDOWSKEY;
    if !config.password.is_empty() {
        info_flags |= conn::INFO_AUTOLOGON;
    }
    if config.compression {
        info_flags |= conn::INFO_COMPRESSION | conn::INFO_COMPRESSION2;
    }
    let (mut server, mut username, mut domain) = (config.server.clone(), config.username.clone(), config.domain.clone());
    let mut rdp_version = if config.version == RdpVersion::V4 { 4 } else { 5 };
    // Downgrade to RDP 4 on refusal when allowed, except when RDP 4 was the
    // explicit request (nothing to fall back from).
    let mut fallback = config.fallback_rdp4 && config.version != RdpVersion::V4;
    let mut reconnecting = false;
    let mut retry_started: Option<Instant> = None;
    let mut conn_id = 0u32;
    // Width must be even, and servers take 200 to 8192 pixels each way.
    let width = (config.width - config.width % 2).clamp(200, 8192) as u16;
    let height = config.height.clamp(200, 8192) as u16;

    let outcome = loop {
        if shared.quit_requested() {
            break closed_by_user("Window closed");
        }
        if let Some(r) = &carry.redirect {
            if let Some(s) = &r.server {
                server = s.clone();
            }
            if let Some(u) = &r.username {
                username = u.clone();
            }
            if let Some(d) = &r.domain {
                domain = d.clone();
            }
            info_flags |= conn::INFO_AUTOLOGON;
        }
        conn_id += 1;
        let settings = Settings {
            server: server.clone(),
            username: username.clone(),
            domain: domain.clone(),
            password: config.password.clone(),
            rdp_version,
            depth: if rdp_version < 5 { 8 } else { config.depth },
            width,
            height,
            info_flags,
            encryption: config.encryption,
        };

        let end = match Conn::connect(shared, &mut graphics, &mut chans, &mut carry, conn_id, settings) {
            Err(end) => end,
            Ok(mut conn) => {
                if config.encrypt_login_only {
                    conn.stop_encrypting();
                }
                let end = match conn.start_reader() {
                    Ok(()) => session_loop(&mut conn, shared, &rx),
                    Err(e) => End::Failed { message: e.to_string(), licence_refused: false, server_rdp4: false },
                };
                conn.disconnect();
                end
            }
        };

        match end {
            End::UserQuit => break closed_by_user("Window closed"),
            End::Redirect(r) => {
                carry.redirect = Some(r);
                continue;
            }
            End::Failed { message, licence_refused, server_rdp4 } => {
                if shared.quit_requested() {
                    break closed_by_user("Connection cancelled");
                }
                // Two servers refuse RDP 5 but accept RDP 4: NT 4.0 TSE answers an
                // RDP 5 request as "RDP 4" then rejects the channels we asked for,
                // and a Terminal Server with no reachable licence server rejects
                // RDP 5 licensing ("Invalid client"). RDP 4 carries no per-session
                // licence, so retry there: the server itself accepts that dialect.
                if fallback && (server_rdp4 || licence_refused) {
                    fallback = false;
                    rdp_version = 4;
                    let why = if licence_refused {
                        "Server needs a licence for RDP 5; connecting with RDP 4"
                    } else {
                        "Server rejected RDP 5; retrying with RDP 4"
                    };
                    shared.frontend.notify(Event::Message(why.into()));
                    // The server resets a connection that arrives while it is
                    // still tearing down the refused one.
                    if pause(shared, &rx, Duration::from_secs(3)) {
                        break closed_by_user("Connection cancelled");
                    }
                    continue;
                }
                if !(reconnecting || retry_started.is_some()) {
                    break ended(EX_PROTOCOL, format!("Unable to connect to {server}: {message}"));
                }
                if retry_started.get_or_insert_with(Instant::now).elapsed() > RECONNECT_TIMEOUT {
                    break ended(EX_PROTOCOL, "Gave up reconnecting");
                }
                if pause(shared, &rx, Duration::from_secs(4)) {
                    break closed_by_user("Connection cancelled");
                }
                continue;
            }
            End::Closed { mut deactivated, mut ext_disc_reason, network } => {
                fallback = false;
                retry_started = None;
                reconnecting = false;
                // Windows 2008 and older report a user disconnect differently.
                if deactivated && ext_disc_reason == ERRINFO_NO_INFO {
                    deactivated = false;
                    ext_disc_reason = ERRINFO_LOGOFF_BYUSER;
                } else if ext_disc_reason == ERRINFO_NO_INFO {
                    ext_disc_reason = ERRINFO_UNSET;
                }
                if deactivated || ext_disc_reason != ERRINFO_UNSET || !network {
                    let (code, text) = disconnect_reason(deactivated, ext_disc_reason);
                    break ended(code, text);
                }
                if carry.reconnect.is_some() {
                    reconnecting = true; // auto-reconnect cookie available
                } else if config.reconnect_on_network_error {
                    retry_started.get_or_insert_with(Instant::now);
                } else {
                    break ended(EX_PROTOCOL, "Disconnected due to a network error");
                }
                continue;
            }
            End::Error(message) => break ended(EX_PROTOCOL, message),
        }
    };
    graphics.destroy();
    graphics.save_state();
    outcome
}

/// Wait for frames and wakes until the connection ends.
fn session_loop(conn: &mut Conn, shared: &Shared, rx: &Receiver<LoopMsg>) -> End {
    loop {
        if shared.quit_requested() {
            return End::UserQuit;
        }
        if conn.heartbeat_lost() {
            return conn.closed(true);
        }
        let timeout = conn.deadline().map(|d| d.saturating_duration_since(Instant::now())).unwrap_or(Duration::from_secs(60));
        match rx.recv_timeout(timeout) {
            Ok(LoopMsg::Frame { conn: id, data }) if id == conn.id => match conn.handle_frame(&data) {
                Ok(Some(end)) => return end,
                Ok(None) => {}
                Err(Error::Io(_)) => return conn.closed(true),
                Err(e) => return End::Error(format!("Protocol error: {e}")),
            },
            Ok(LoopMsg::Closed { conn: id, error }) if id == conn.id => {
                return conn.closed(error.kind() != std::io::ErrorKind::UnexpectedEof);
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return End::UserQuit,
        }
        if let Err(e) = conn.pump() {
            return match e {
                Error::Io(_) => conn.closed(true),
                e => End::Error(format!("Protocol error: {e}")),
            };
        }
    }
}

/// Minutes to add to local time to get UTC, for the time zone in the client
/// info PDU.
pub(crate) fn utc_bias_minutes() -> i32 {
    #[cfg(unix)]
    {
        // SAFETY: localtime_r fills the tm we pass; time(NULL) is always valid.
        unsafe {
            let t = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&t, &mut tm).is_null() {
                return 0;
            }
            -(tm.tm_gmtoff / 60) as i32
        }
    }
    #[cfg(windows)]
    {
        // TIME_ZONE_INFORMATION starts with the bias; it is 172 bytes in all.
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetTimeZoneInformation(info: *mut u8) -> u32;
        }
        let mut info = [0u8; 172];
        // SAFETY: a buffer the size of TIME_ZONE_INFORMATION.
        let id = unsafe { GetTimeZoneInformation(info.as_mut_ptr()) };
        let bias = i32::from_le_bytes([info[0], info[1], info[2], info[3]]);
        // TIME_ZONE_ID_DAYLIGHT: daylight saving's own bias applies on top.
        if id == 2 { bias + i32::from_le_bytes([info[168], info[169], info[170], info[171]]) } else { bias }
    }
    #[cfg(not(any(unix, windows)))]
    0
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
