//! TLS through GnuTLS, linked (build.rs) but not compiled: the declarations
//! below are written by hand from gnutls.h and x509.h. GnuTLS reads and
//! writes through push/pull callbacks on the Rust `TcpStream`, never a
//! socket handle of its own, so the same code runs over Winsock on NT 4.0.
//! Port of rdesktop's tcp.c (tcp_tls_connect, cert_verify_callback,
//! tcp_tls_get_server_pubkey and the TLS parts of tcp_send and tcp_recv).
//!
//! Threads. GnuTLS lets one thread receive while another sends on the same
//! session, with three conditions ("Thread safety" in its manual): nothing
//! else may run during a TLS 1.2 renegotiation, nothing may send or receive
//! besides the two, and the session ends with the sender calling
//! `gnutls_bye(GNUTLS_SHUT_WR)`. So: the handshake runs in [`connect`],
//! before the stream is shared; renegotiation requests are ignored (RDP
//! servers don't make them); sends and `shutdown` hold one lock, so two
//! writers or a writer and the close_notify never overlap; reads take no
//! lock, so a blocked read never holds up a write. A TLS 1.3 key update the
//! server asks for is answered by GnuTLS on the next send, in the writer.
//! `gnutls_transport_set_errno` is unreliable with two threads, so the
//! callbacks report errors through a thread-local and
//! `gnutls_transport_set_errno_function`: each thread sees its own.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use super::{TlsStream, certstore, x509};
use crate::error::{Error, Result};
use crate::session::Shared;

type Session = *mut c_void;
type CertCredentials = *mut c_void;
type X509Crt = *mut c_void;

#[repr(C)]
struct Datum {
    data: *mut u8,
    size: c_uint,
}

type PushFn = extern "C" fn(*mut c_void, *const c_void, usize) -> isize;
type PullFn = extern "C" fn(*mut c_void, *mut c_void, usize) -> isize;
type PullTimeoutFn = extern "C" fn(*mut c_void, c_uint) -> c_int;
type ErrnoFn = extern "C" fn(*mut c_void) -> c_int;

const GNUTLS_CLIENT: c_uint = 1 << 1;
const GNUTLS_NONBLOCK: c_uint = 1 << 3;
const GNUTLS_CRD_CERTIFICATE: c_int = 1;
const GNUTLS_CRT_X509: c_int = 1;
const GNUTLS_X509_FMT_DER: c_int = 0;
const GNUTLS_SHUT_WR: c_int = 1;
const GNUTLS_CERT_INVALID: c_uint = 1 << 1;

const GNUTLS_E_UNSUPPORTED_VERSION_PACKET: c_int = -8;
const GNUTLS_E_UNEXPECTED_PACKET_LENGTH: c_int = -9;
const GNUTLS_E_FATAL_ALERT_RECEIVED: c_int = -12;
const GNUTLS_E_WARNING_ALERT_RECEIVED: c_int = -16;
const GNUTLS_E_AGAIN: c_int = -28;
const GNUTLS_E_INSUFFICIENT_CREDENTIALS: c_int = -32;
const GNUTLS_E_REHANDSHAKE: c_int = -37;
const GNUTLS_E_INTERRUPTED: c_int = -52;
const GNUTLS_E_PUSH_ERROR: c_int = -53;
const GNUTLS_E_PULL_ERROR: c_int = -54;
const GNUTLS_E_NO_CIPHER_SUITES: c_int = -87;
const GNUTLS_E_PREMATURE_TERMINATION: c_int = -110;

/// TLS alert descriptions that mean "no version or cipher in common".
const GNUTLS_A_HANDSHAKE_FAILURE: c_int = 40;
const GNUTLS_A_PROTOCOL_VERSION: c_int = 70;
const GNUTLS_A_INSUFFICIENT_SECURITY: c_int = 71;

unsafe extern "C" {
    fn gnutls_global_init() -> c_int;
    fn gnutls_init(session: *mut Session, flags: c_uint) -> c_int;
    fn gnutls_deinit(session: Session);
    fn gnutls_priority_set_direct(session: Session, priorities: *const c_char, err_pos: *mut *const c_char) -> c_int;
    fn gnutls_certificate_allocate_credentials(res: *mut CertCredentials) -> c_int;
    fn gnutls_certificate_free_credentials(sc: CertCredentials);
    fn gnutls_credentials_set(session: Session, kind: c_int, cred: *mut c_void) -> c_int;
    fn gnutls_certificate_set_x509_system_trust(cred: CertCredentials) -> c_int;
    fn gnutls_transport_set_ptr(session: Session, ptr: *mut c_void);
    fn gnutls_transport_set_push_function(session: Session, push: PushFn);
    fn gnutls_transport_set_pull_function(session: Session, pull: PullFn);
    fn gnutls_transport_set_pull_timeout_function(session: Session, pull: PullTimeoutFn);
    fn gnutls_transport_set_errno_function(session: Session, errno: ErrnoFn);
    fn gnutls_handshake(session: Session) -> c_int;
    fn gnutls_error_is_fatal(error: c_int) -> c_int;
    fn gnutls_strerror(error: c_int) -> *const c_char;
    fn gnutls_alert_get(session: Session) -> c_int;
    fn gnutls_alert_get_name(alert: c_int) -> *const c_char;
    fn gnutls_record_send(session: Session, data: *const c_void, len: usize) -> isize;
    fn gnutls_record_recv(session: Session, data: *mut c_void, len: usize) -> isize;
    fn gnutls_bye(session: Session, how: c_int) -> c_int;
    fn gnutls_protocol_get_version(session: Session) -> c_int;
    fn gnutls_protocol_get_name(version: c_int) -> *const c_char;
    fn gnutls_cipher_get(session: Session) -> c_int;
    fn gnutls_cipher_get_name(cipher: c_int) -> *const c_char;
    fn gnutls_certificate_verify_peers2(session: Session, status: *mut c_uint) -> c_int;
    fn gnutls_certificate_type_get(session: Session) -> c_int;
    fn gnutls_certificate_get_peers(session: Session, list_size: *mut c_uint) -> *const Datum;
    fn gnutls_x509_crt_init(cert: *mut X509Crt) -> c_int;
    fn gnutls_x509_crt_import(cert: X509Crt, data: *const Datum, format: c_int) -> c_int;
    fn gnutls_x509_crt_check_hostname(cert: X509Crt, hostname: *const c_char) -> c_uint;
    fn gnutls_x509_crt_deinit(cert: X509Crt);
}

/// Windows' self-signed certificates lack the Digital Signature key usage,
/// which only %COMPAT makes GnuTLS overlook. TLS 1.0 and 1.1 for Server 2003.
const PRIORITY: &str = "NORMAL:%COMPAT:+VERS-TLS1.1:+VERS-TLS1.0";
/// All unpatched Server 2003 offers. Last, so anything newer still gets AES;
/// a GnuTLS built without them refuses the string and we go without.
const PRIORITY_LEGACY: &str = ":+3DES-CBC:+ARCFOUR-128:+SHA1:+SIGN-RSA-SHA1";
/// How long the handshake may wait for the server, as GnuTLS's default.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(40);
const CERT_TITLE: &str = "vintrdp certificate warning";

/// The priority string for `config.tls_version` (-V).
fn priority(version: Option<&str>) -> Result<String> {
    let only = match version {
        None | Some("") => "",
        Some("1.0") => ":-VERS-ALL:+VERS-TLS1.0",
        Some("1.1") => ":-VERS-ALL:+VERS-TLS1.1",
        Some("1.2") => ":-VERS-ALL:+VERS-TLS1.2",
        Some(v) => return Err(Error::protocol(format!("Unknown TLS version {v:?}: it should be 1.0, 1.1 or 1.2"))),
    };
    Ok(format!("{PRIORITY}{only}"))
}

// ---------------------------------------------------------------------------
// Transport callbacks

#[cfg(unix)]
const EAGAIN: c_int = libc::EAGAIN;
#[cfg(unix)]
const EIO: c_int = libc::EIO;
/// What mingw's errno.h, which GnuTLS is built against, says.
#[cfg(not(unix))]
const EAGAIN: c_int = 11;
#[cfg(not(unix))]
const EIO: c_int = 5;

thread_local! {
    /// The last transport error on this thread: the errno GnuTLS gets, and
    /// the error itself for our caller.
    static LAST_ERROR: RefCell<Option<(c_int, io::Error)>> = const { RefCell::new(None) };
}

fn clear_error() {
    LAST_ERROR.with(|e| e.borrow_mut().take());
}

fn take_error() -> Option<io::Error> {
    LAST_ERROR.with(|e| e.borrow_mut().take()).map(|(_, e)| e)
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// Record `e` and fail the callback. A read timeout is EAGAIN, which GnuTLS
/// survives (the record resumes on the next read); anything else is fatal.
fn fail(e: io::Error, pull: bool) -> isize {
    let errno = if pull && is_timeout(&e) { EAGAIN } else { EIO };
    LAST_ERROR.with(|l| *l.borrow_mut() = Some((errno, e)));
    -1
}

/// # Safety (all four)
/// `ptr` is the `TcpStream` the session was given in [`connect`], boxed in
/// the [`Gnutls`] that owns the session, so it outlives every call.
extern "C" fn push(ptr: *mut c_void, data: *const c_void, len: usize) -> isize {
    if len == 0 {
        return 0;
    }
    // SAFETY: see above; GnuTLS passes `len` readable bytes at `data`.
    let (mut tcp, buf) = unsafe { (&*(ptr as *const TcpStream), std::slice::from_raw_parts(data as *const u8, len)) };
    match tcp.write_all(buf) {
        Ok(()) => len as isize,
        Err(e) => fail(e, false),
    }
}

extern "C" fn pull(ptr: *mut c_void, data: *mut c_void, len: usize) -> isize {
    if len == 0 {
        return 0;
    }
    // SAFETY: see above; GnuTLS passes `len` writable bytes at `data`.
    let (mut tcp, buf) = unsafe { (&*(ptr as *const TcpStream), std::slice::from_raw_parts_mut(data as *mut u8, len)) };
    loop {
        match tcp.read(buf) {
            Ok(n) => return n as isize,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return fail(e, true),
        }
    }
}

/// Never called with GNUTLS_NONBLOCK, but GnuTLS wants one with a custom
/// pull function. "Data may be ready": the pull that follows blocks.
extern "C" fn pull_timeout(_ptr: *mut c_void, _ms: c_uint) -> c_int {
    1
}

extern "C" fn errno(_ptr: *mut c_void) -> c_int {
    LAST_ERROR.with(|e| e.borrow().as_ref().map_or(EIO, |(n, _)| *n))
}

// ---------------------------------------------------------------------------

fn c_str(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: GnuTLS returns static NUL-terminated strings.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn strerror(code: c_int) -> String {
    // SAFETY: plain call.
    c_str(unsafe { gnutls_strerror(code) })
}

pub struct Gnutls {
    session: Session,
    credentials: CertCredentials,
    /// Boxed: the session holds its address.
    tcp: Box<TcpStream>,
    /// Held while sending and for the close_notify.
    send: Mutex<bool>,
    /// The server's certificate, DER.
    cert: Vec<u8>,
}

// SAFETY: GnuTLS sessions may move between threads, and one thread may
// receive while another sends (see the module docs); everything else that
// touches the session holds `send` or runs before the stream is shared.
unsafe impl Send for Gnutls {}
unsafe impl Sync for Gnutls {}

impl Drop for Gnutls {
    fn drop(&mut self) {
        // SAFETY: the last reference is going, so no thread is in a call.
        unsafe {
            if !self.session.is_null() {
                gnutls_deinit(self.session);
            }
            if !self.credentials.is_null() {
                gnutls_certificate_free_credentials(self.credentials);
            }
        }
    }
}

/// An error from a GnuTLS record call as an io::Error: the transport's own
/// error when it was the transport that failed.
fn io_error(code: c_int) -> io::Error {
    if let Some(e) = take_error() {
        return e;
    }
    if code == GNUTLS_E_AGAIN {
        return io::ErrorKind::WouldBlock.into();
    }
    io::Error::other(format!("TLS: {}", strerror(code)))
}

impl TlsStream for Gnutls {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            clear_error();
            // SAFETY: a live session; `buf` is writable for its length.
            let n = unsafe { gnutls_record_recv(self.session, buf.as_mut_ptr() as *mut c_void, buf.len()) };
            if n >= 0 {
                return Ok(n as usize);
            }
            match n as c_int {
                // Renegotiating would need the writer stopped; RDP servers
                // don't ask, so we decline by not answering.
                GNUTLS_E_INTERRUPTED | GNUTLS_E_REHANDSHAKE | GNUTLS_E_WARNING_ALERT_RECEIVED => continue,
                // GNUTLS_NONBLOCK: a record with no data for us (a TLS 1.3
                // session ticket, say). Only a read timeout is our caller's.
                GNUTLS_E_AGAIN if LAST_ERROR.with(|e| e.borrow().is_none()) => continue,
                // Windows often closes without close_notify. RDP frames its
                // own PDUs, so a truncation can't pass unnoticed: call it EOF.
                GNUTLS_E_PREMATURE_TERMINATION => return Ok(0),
                code => return Err(io_error(code)),
            }
        }
    }

    fn write_all(&self, mut buf: &[u8]) -> io::Result<()> {
        let closed = self.send.lock().unwrap_or_else(|e| e.into_inner());
        if *closed {
            return Err(io::ErrorKind::NotConnected.into());
        }
        while !buf.is_empty() {
            clear_error();
            // SAFETY: a live session; `buf` is readable for its length.
            let n = unsafe { gnutls_record_send(self.session, buf.as_ptr() as *const c_void, buf.len()) };
            match n {
                n if n > 0 => buf = &buf[(n as usize).min(buf.len())..],
                0 => return Err(io::ErrorKind::WriteZero.into()),
                // With GNUTLS_NONBLOCK, the same call again; push never
                // asks for it, so this is only for an interrupted send.
                n if n as c_int == GNUTLS_E_AGAIN || n as c_int == GNUTLS_E_INTERRUPTED => {}
                n => return Err(io_error(n as c_int)),
            }
        }
        Ok(())
    }

    fn server_public_key(&self) -> Result<Vec<u8>> {
        Ok(x509::parse(&self.cert)?.public_key.to_vec())
    }

    fn shutdown(&self) {
        let mut closed = self.send.lock().unwrap_or_else(|e| e.into_inner());
        if !*closed {
            *closed = true;
            clear_error();
            // SAFETY: a live session; the send lock keeps writers out, and
            // GnuTLS allows SHUT_WR beside a reading thread.
            unsafe { gnutls_bye(self.session, GNUTLS_SHUT_WR) };
        }
        // Some servers never answer close_notify: end the reads ourselves.
        let _ = self.tcp.shutdown(Shutdown::Both);
    }
}

/// What a failed handshake means to the user.
fn handshake_error(session: Session, code: c_int) -> Error {
    let io = take_error();
    if io.as_ref().is_some_and(is_timeout) {
        return Error::protocol("TLS handshake failed: the server did not answer in time");
    }
    let no_common = "TLS handshake failed: the server and this client have no TLS version or cipher in common \
                     (an older server may need -V 1.0)";
    match code {
        GNUTLS_E_FATAL_ALERT_RECEIVED => {
            // SAFETY: a live session.
            let alert = unsafe { gnutls_alert_get(session) };
            match alert {
                GNUTLS_A_HANDSHAKE_FAILURE | GNUTLS_A_PROTOCOL_VERSION | GNUTLS_A_INSUFFICIENT_SECURITY => Error::protocol(no_common),
                _ => {
                    // SAFETY: plain call.
                    let name = c_str(unsafe { gnutls_alert_get_name(alert) });
                    Error::protocol(format!("TLS handshake failed: the server refused it ({name})"))
                }
            }
        }
        GNUTLS_E_NO_CIPHER_SUITES | GNUTLS_E_INSUFFICIENT_CREDENTIALS | GNUTLS_E_UNSUPPORTED_VERSION_PACKET => Error::protocol(no_common),
        GNUTLS_E_PREMATURE_TERMINATION | GNUTLS_E_UNEXPECTED_PACKET_LENGTH => {
            Error::protocol("TLS handshake failed: the server closed the connection")
        }
        GNUTLS_E_PUSH_ERROR | GNUTLS_E_PULL_ERROR if io.is_some() => {
            Error::protocol(format!("TLS handshake failed: {}", io.unwrap_or_else(|| io::Error::other(""))))
        }
        _ => Error::protocol(format!("TLS handshake failed: {}", strerror(code))),
    }
}

fn check(code: c_int, what: &str) -> Result<()> {
    if code < 0 { Err(Error::protocol(format!("{what}: {}", strerror(code)))) } else { Ok(()) }
}

/// The peer's certificate, DER, and GnuTLS's verdict on it against the
/// system's trust store and `server`: (certificate, status, name mismatch).
/// Trusted is a zero status and no mismatch.
fn verify_peer(session: Session, server: &str) -> Result<(Vec<u8>, c_uint, bool)> {
    // SAFETY: a live session after its handshake; GnuTLS owns the list,
    // valid until the session goes, and we copy what we keep.
    unsafe {
        if gnutls_certificate_type_get(session) != GNUTLS_CRT_X509 {
            return Err(Error::protocol("The server's certificate is not an X.509 certificate"));
        }
        let mut count: c_uint = 0;
        let list = gnutls_certificate_get_peers(session, &mut count);
        if list.is_null() || count == 0 || (*list).data.is_null() {
            return Err(Error::protocol("The server sent no certificate"));
        }
        let first = &*list;
        let der = std::slice::from_raw_parts(first.data, first.size as usize).to_vec();

        let mut status: c_uint = 0;
        let mut mismatch = false;
        if gnutls_certificate_verify_peers2(session, &mut status) < 0 {
            // Verification itself failed: untrusted, for no reason we know.
            status = GNUTLS_CERT_INVALID;
        } else if status == 0 {
            let mut crt: X509Crt = std::ptr::null_mut();
            let mut matches = false;
            if gnutls_x509_crt_init(&mut crt) == 0 {
                if let Ok(name) = CString::new(server) {
                    matches = gnutls_x509_crt_import(crt, first, GNUTLS_X509_FMT_DER) == 0
                        && gnutls_x509_crt_check_hostname(crt, name.as_ptr()) != 0;
                }
                gnutls_x509_crt_deinit(crt);
            }
            if matches {
                return Ok((der, 0, false));
            }
            mismatch = true;
        }
        Ok((der, status, mismatch))
    }
}

/// See [`super::connect`].
pub fn connect(tcp: TcpStream, server: &str, shared: &Shared) -> Result<Arc<dyn TlsStream>> {
    let config = &shared.config;
    let tls = handshake(tcp, server, config.tls_version.as_deref(), config.verbose, &mut |cert, status, mismatch| {
        // Not trusted by the system: the user's exceptions decide.
        certstore::handle_exception(cert, status, mismatch, server, |message| shared.frontend.ask(CERT_TITLE, message, "Trust", "Cancel"))
    })?;
    Ok(Arc::new(tls))
}

/// Decides on a certificate the system doesn't trust: (DER, GnuTLS status
/// bits, name mismatch).
type Untrusted<'a> = dyn FnMut(&[u8], u32, bool) -> Result<()> + 'a;

/// TLS on `tcp` to `server`, offering `version` (or the defaults). A
/// certificate the system doesn't trust goes to `untrusted`, with GnuTLS's
/// status bits and whether the name failed to match, to accept or refuse.
fn handshake(tcp: TcpStream, server: &str, version: Option<&str>, verbose: bool, untrusted: &mut Untrusted) -> Result<Gnutls> {
    let base = priority(version)?;
    static INIT: Once = Once::new();
    // SAFETY: once per process, before any session.
    INIT.call_once(|| unsafe {
        gnutls_global_init();
    });

    let mut tls = Gnutls {
        session: std::ptr::null_mut(),
        credentials: std::ptr::null_mut(),
        tcp: Box::new(tcp),
        send: Mutex::new(false),
        cert: Vec::new(),
    };
    // SAFETY: GnuTLS calls on the session we create; `tls` frees it on any
    // early return. The transport pointer is the boxed TcpStream, which
    // lives as long as `tls` does.
    unsafe {
        check(gnutls_init(&mut tls.session, GNUTLS_CLIENT | GNUTLS_NONBLOCK), "Could not initialize GnuTLS")?;
        let legacy = CString::new(format!("{base}{PRIORITY_LEGACY}")).map_err(|_| Error::protocol("bad TLS priority"))?;
        let plain = CString::new(base).map_err(|_| Error::protocol("bad TLS priority"))?;
        if gnutls_priority_set_direct(tls.session, legacy.as_ptr(), std::ptr::null_mut()) < 0 {
            check(gnutls_priority_set_direct(tls.session, plain.as_ptr(), std::ptr::null_mut()), "Could not set the TLS priorities")?;
        }
        check(gnutls_certificate_allocate_credentials(&mut tls.credentials), "Could not allocate TLS credentials")?;
        check(gnutls_credentials_set(tls.session, GNUTLS_CRD_CERTIFICATE, tls.credentials), "Could not set TLS credentials")?;
        let trusted = gnutls_certificate_set_x509_system_trust(tls.credentials);
        if trusted < 0 {
            eprintln!("vintrdp: could not load the system's trusted certificates: {}", strerror(trusted));
        }
        let ptr = &*tls.tcp as *const TcpStream as *mut c_void;
        gnutls_transport_set_ptr(tls.session, ptr);
        gnutls_transport_set_push_function(tls.session, push);
        gnutls_transport_set_pull_function(tls.session, pull);
        gnutls_transport_set_pull_timeout_function(tls.session, pull_timeout);
        gnutls_transport_set_errno_function(tls.session, errno);
    }

    // The handshake's timeout is the socket's, put back afterwards.
    let old_timeouts = (tls.tcp.read_timeout().ok().flatten(), tls.tcp.write_timeout().ok().flatten());
    let _ = tls.tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let _ = tls.tcp.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
    let result = loop {
        clear_error();
        // SAFETY: a live, configured session.
        let r = unsafe { gnutls_handshake(tls.session) };
        // SAFETY: plain call.
        if r == 0 || unsafe { gnutls_error_is_fatal(r) } != 0 {
            break r;
        }
        // Not fatal: a warning alert, or EAGAIN, which only a timeout makes.
        if r == GNUTLS_E_AGAIN && LAST_ERROR.with(|e| e.borrow().is_some()) {
            break r;
        }
    };
    let _ = tls.tcp.set_read_timeout(old_timeouts.0);
    let _ = tls.tcp.set_write_timeout(old_timeouts.1);
    if result < 0 {
        return Err(handshake_error(tls.session, result));
    }

    if verbose {
        // SAFETY: plain calls on a live session; the names are static.
        let (version, cipher) = unsafe {
            (
                c_str(gnutls_protocol_get_name(gnutls_protocol_get_version(tls.session))),
                c_str(gnutls_cipher_get_name(gnutls_cipher_get(tls.session))),
            )
        };
        eprintln!("vintrdp: TLS session: {version}, {cipher}");
    }

    let (cert, status, mismatch) = verify_peer(tls.session, server)?;
    if status != 0 || mismatch {
        untrusted(&cert, status, mismatch)?;
    }
    tls.cert = cert;
    Ok(tls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priorities() {
        assert_eq!(priority(None).unwrap(), PRIORITY);
        assert_eq!(priority(Some("1.0")).unwrap(), format!("{PRIORITY}:-VERS-ALL:+VERS-TLS1.0"));
        assert!(priority(Some("1.3")).is_err());
    }

    /// Against a local `openssl s_server -rev` on VINTRDP_TLS_TEST_PORT with
    /// a self-signed certificate, offering VINTRDP_TLS_TEST_VERSION (1.0,
    /// 1.2, or unset). Reads on this thread while another writes.
    #[test]
    #[ignore]
    fn openssl_smoke() {
        let Ok(port) = std::env::var("VINTRDP_TLS_TEST_PORT") else { return };
        let version = std::env::var("VINTRDP_TLS_TEST_VERSION").ok();
        let tcp = TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
        let mut seen = None;
        let tls = handshake(tcp, "localhost", version.as_deref(), true, &mut |cert, status, mismatch| {
            seen = Some((cert.to_vec(), status, mismatch));
            Ok(())
        })
        .unwrap();
        let (cert, status, _) = seen.expect("a self-signed certificate is not trusted");
        assert_ne!(status & certstore::CERT_SIGNER_NOT_FOUND, 0);
        assert_eq!(tls.server_public_key().unwrap(), x509::parse(&cert).unwrap().public_key);
        // SAFETY: plain call on a live session.
        let proto = c_str(unsafe { gnutls_protocol_get_name(gnutls_protocol_get_version(tls.session)) });
        if let Some(v) = &version {
            assert_eq!(proto, format!("TLS{v}"));
        }

        let tls = Arc::new(tls);
        let w = tls.clone();
        const LINES: usize = 500;
        let writer = std::thread::spawn(move || {
            for i in 0..LINES {
                w.write_all(format!("line {i} {}\n", "x".repeat(i)).as_bytes()).unwrap();
            }
        });
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while got.iter().filter(|&&b| b == b'\n').count() < LINES {
            let n = tls.read(&mut buf).unwrap();
            assert!(n > 0, "EOF after {} bytes", got.len());
            got.extend_from_slice(&buf[..n]);
        }
        writer.join().unwrap();
        for (i, line) in String::from_utf8(got).unwrap().lines().enumerate() {
            let back: String = line.chars().rev().collect();
            assert_eq!(back, format!("line {i} {}", "x".repeat(i)));
        }
        tls.shutdown();
        assert_eq!(tls.read(&mut buf).unwrap_or(0), 0);
        assert!(tls.write_all(b"late").is_err());
    }
}
