//! XDMCP against a display manager played here: the exchange, the cookie it
//! hands out, the reset when the session's last client leaves, and what the
//! user is told when the manager says no.

use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vintrdp_x11::keymap::Keymap;
use vintrdp_x11::xdmauth;
use vintrdp_x11::xdmcp::{Mode, Status, Xdmcp};
use vintrdp_x11::{Host, Server, Settings};
use x11rb::connection::Connection;
use x11rb::rust_connection::{DefaultStream, RustConnection};

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Host for Log {
    fn resize(&self, _: u16, _: u16) {}
    fn draw(&self, _: usize, _: usize, _: usize, _: usize, _: &[u32]) {}
    fn updated(&self) {}
    fn xdmcp(&self, status: &Status) {
        let s = match status {
            Status::Progress(s) => format!("progress {s}"),
            Status::Managed { host } => format!("managed {host}"),
            Status::Failed(s) => format!("failed {s}"),
            Status::Dead => "dead".into(),
        };
        self.0.lock().unwrap().push(s);
    }
}

fn server(manager: SocketAddr, log: &Log) -> Server {
    server_with_key(manager, log, None)
}

fn server_with_key(manager: SocketAddr, log: &Log, key: Option<xdmauth::Key>) -> Server {
    let settings = Settings {
        width: 640,
        height: 480,
        listen: "127.0.0.1:0".parse().unwrap(),
        unix_socket: false,
        allow: Vec::new(),
        keymap: Keymap::new(|_, _| None),
        xdmcp: Some(Xdmcp { host: manager.ip().to_string(), port: manager.port(), mode: Mode::Query, address: None, key }),
        cookie: None,
        font_path: Vec::new(),
        primary: false,
        visual: Default::default(),
        verbose: false,
    };
    vintrdp_x11::start(settings, Box::new(log.clone())).unwrap()
}

/// A packet: opcode and body.
fn recv(s: &UdpSocket) -> (u16, Vec<u8>, SocketAddr) {
    let mut buf = [0u8; 1024];
    let (n, from) = s.recv_from(&mut buf).expect("nothing from the display");
    assert_eq!(&buf[..2], [0, 1], "XDMCP version 1");
    let op = u16::from_be_bytes([buf[2], buf[3]]);
    let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    assert_eq!(n, 6 + len);
    (op, buf[6..n].to_vec(), from)
}

fn send(s: &UdpSocket, to: SocketAddr, op: u16, body: &[u8]) {
    let mut p = vec![0, 1];
    p.extend_from_slice(&op.to_be_bytes());
    p.extend_from_slice(&(body.len() as u16).to_be_bytes());
    p.extend_from_slice(body);
    s.send_to(&p, to).unwrap();
}

fn array8(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u16).to_be_bytes());
    out.extend_from_slice(b);
}

fn manager() -> (UdpSocket, SocketAddr) {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let a = s.local_addr().unwrap();
    (s, a)
}

fn x_client(server: &Server, cookie: Option<&[u8]>) -> Result<RustConnection, String> {
    let (name, data) = match cookie {
        Some(c) => (b"MIT-MAGIC-COOKIE-1".to_vec(), c.to_vec()),
        None => (Vec::new(), Vec::new()),
    };
    x_client_auth(server, name, data)
}

fn x_client_auth(server: &Server, name: Vec<u8>, data: Vec<u8>) -> Result<RustConnection, String> {
    let stream = TcpStream::connect(server.local_addr()).unwrap();
    let stream = DefaultStream::from_tcp_stream(stream).unwrap().0;
    RustConnection::connect_to_stream_with_auth_info(stream, 0, name, data).map_err(|e| match e {
        x11rb::errors::ConnectError::SetupFailed(f) => String::from_utf8_lossy(&f.reason).into_owned(),
        e => format!("{e:?}"),
    })
}

/// One session's exchange, from the manager's side: the display and session.
fn manage(m: &UdpSocket, session: u32, cookie: &[u8]) -> (u16, SocketAddr) {
    let (op, body, display_at) = recv(m);
    assert_eq!((op, body), (2, vec![0]), "Query, with no authentication names");
    let mut w = Vec::new();
    array8(&mut w, b"");
    array8(&mut w, b"testhost");
    array8(&mut w, b"Willing to manage");
    send(m, display_at, 5, &w);

    let (op, body, _) = recv(m);
    assert_eq!(op, 7, "Request");
    let display = u16::from_be_bytes([body[0], body[1]]);
    // One connection type, Internet, and its address: this machine's.
    assert_eq!(&body[2..5], [1, 0, 0]);
    assert_eq!(&body[5..10], [1, 0, 4, 127, 0]);
    assert!(body.windows(18).any(|w| w == b"MIT-MAGIC-COOKIE-1"));
    let mut a = session.to_be_bytes().to_vec();
    array8(&mut a, b"");
    array8(&mut a, b"");
    array8(&mut a, b"MIT-MAGIC-COOKIE-1");
    array8(&mut a, cookie);
    send(m, display_at, 8, &a);

    let (op, body, _) = recv(m);
    assert_eq!(op, 10, "Manage");
    assert_eq!(&body[..6], [&session.to_be_bytes()[..], &display.to_be_bytes()].concat());
    (display, display_at)
}

#[test]
fn a_session_its_cookie_and_the_reset() {
    let (m, at) = manager();
    let log = Log::default();
    let server = server(at, &log);
    let cookie = [7u8; 16];
    let (display, _) = manage(&m, 1, &cookie);
    assert_eq!(display, server.local_addr().port().wrapping_sub(6000));

    // Without the cookie (or with another) no one connects, this machine included.
    let refused = x_client(&server, None).unwrap_err();
    assert!(refused.contains("no authorization protocol"), "{refused}");
    assert!(x_client(&server, Some(&[1; 16])).unwrap_err().contains("Invalid MIT-MAGIC-COOKIE-1"));
    let c = x_client(&server, Some(&cookie)).unwrap();
    assert_eq!(c.setup().vendor, b"vintrdp");
    std::thread::sleep(Duration::from_millis(300));
    assert!(log.0.lock().unwrap().iter().any(|l| l == "managed testhost"));

    // The session's last client goes: the server resets and asks again,
    // and the new session's cookie is the one that counts.
    drop(c);
    let cookie2 = [9u8; 16];
    manage(&m, 2, &cookie2);
    assert!(x_client(&server, Some(&cookie)).is_err());
    let c = x_client(&server, Some(&cookie2)).unwrap();
    drop(c);
}

#[test]
fn unwilling_is_said() {
    let (m, at) = manager();
    let log = Log::default();
    let _server = server(at, &log);
    let (op, _, display_at) = recv(&m);
    assert_eq!(op, 2);
    let mut u = Vec::new();
    array8(&mut u, b"testhost");
    array8(&mut u, b"Host unwilling (no Xaccess entry)");
    send(&m, display_at, 6, &u);
    for _ in 0..50 {
        if log.0.lock().unwrap().iter().any(|l| l.starts_with("failed")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let failed = log.0.lock().unwrap().iter().find(|l| l.starts_with("failed")).cloned();
    assert_eq!(failed.as_deref(), Some("failed testhost won't manage this display: Host unwilling (no Xaccess entry)"));
}

#[test]
fn listening_takes_a_cookie_from_anywhere() {
    // Listening alone: this machine needs no cookie; a client giving the
    // right one gets in whatever its address (here it's this machine too).
    let settings = Settings {
        width: 640,
        height: 480,
        listen: "127.0.0.1:0".parse().unwrap(),
        unix_socket: false,
        allow: Vec::new(),
        keymap: Keymap::new(|_, _| None),
        xdmcp: None,
        cookie: Some(vec![3; 16]),
        font_path: Vec::new(),
        primary: false,
        visual: Default::default(),
        verbose: false,
    };
    let server = vintrdp_x11::start(settings, Box::new(Log::default())).unwrap();
    assert!(x_client(&server, None).is_ok());
    assert!(x_client(&server, Some(&[3; 16])).is_ok());
}

/// xdm with an xdm-keys entry for the display, played here as X.org's xdm
/// does it (xdm/xdmauth.c): XDM-AUTHENTICATION-1, then XDM-AUTHORIZATION-1
/// for the session's clients, as libxcb makes them.
#[test]
fn xdm_authentication_and_authorization() {
    let (m, at) = manager();
    let log = Log::default();
    let key = xdmauth::parse_key("vintrdp").unwrap();
    let server = server_with_key(at, &log, Some(key));

    let (op, body, display_at) = recv(&m);
    let mut q = vec![1];
    array8(&mut q, b"XDM-AUTHENTICATION-1");
    assert_eq!((op, body), (2, q), "Query, naming XDM-AUTHENTICATION-1");
    let mut w = Vec::new();
    array8(&mut w, b"XDM-AUTHENTICATION-1");
    array8(&mut w, b"testhost");
    array8(&mut w, b"Willing to manage");
    send(&m, display_at, 5, &w);

    let (op, body, _) = recv(&m);
    assert_eq!(op, 7, "Request");
    // Display, one connection (Internet, 127.0.0.1), then authentication.
    let mut at_auth = 2 + 1 + 2 + 1 + 2 + 4;
    let field = |b: &[u8], at: &mut usize| {
        let n = u16::from_be_bytes([b[*at], b[*at + 1]]) as usize;
        let f = b[*at + 2..*at + 2 + n].to_vec();
        *at += 2 + n;
        f
    };
    assert_eq!(field(&body, &mut at_auth), b"XDM-AUTHENTICATION-1");
    let wrapped = field(&body, &mut at_auth);
    let rho: xdmauth::Key = xdmauth::unwrap(&wrapped, &key).unwrap().try_into().unwrap();
    // Authorizations asked for, XDM-AUTHORIZATION-1 first.
    assert_eq!(body[at_auth], 2);
    at_auth += 1;
    assert_eq!(field(&body, &mut at_auth), b"XDM-AUTHORIZATION-1");
    assert_eq!(field(&body, &mut at_auth), b"MIT-MAGIC-COOKIE-1");

    let mut next = rho;
    xdmauth::increment(&mut next);
    let session_key: xdmauth::Key = [0, 0x5e, 0x55, 0x10, 0x4e, 0x4b, 0x45, 0x59];
    let mut a = 7u32.to_be_bytes().to_vec();
    array8(&mut a, b"XDM-AUTHENTICATION-1");
    array8(&mut a, &xdmauth::wrap(&next, &key));
    array8(&mut a, b"XDM-AUTHORIZATION-1");
    array8(&mut a, &xdmauth::wrap(&session_key, &key));
    send(&m, display_at, 8, &a);
    let (op, _, _) = recv(&m);
    assert_eq!(op, 10, "Manage");

    // A client with the session key gets in; once only.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as u32;
    let data = xdmauth::client_data(&rho, &session_key, [127, 0, 0, 1], 5000, now);
    let name = b"XDM-AUTHORIZATION-1".to_vec();
    let c = x_client_auth(&server, name.clone(), data.clone()).unwrap();
    assert_eq!(c.setup().vendor, b"vintrdp");
    let again = x_client_auth(&server, name.clone(), data).unwrap_err();
    assert!(again.contains("matches an existing client"), "{again}");
    let wrong = xdmauth::client_data(&rho, &[0, 1, 2, 3, 4, 5, 6, 7], [127, 0, 0, 1], 5001, now);
    assert!(x_client_auth(&server, name.clone(), wrong).unwrap_err().contains("XDM-AUTHORIZATION-1"));
    // The cookie isn't the session's: no cookie works.
    assert!(x_client(&server, Some(&[0; 16])).is_err());
    assert!(x_client(&server, None).is_err());
    drop(c);
}

#[test]
fn a_manager_without_the_key_is_refused() {
    let (m, at) = manager();
    let log = Log::default();
    let _server = server_with_key(at, &log, Some(xdmauth::parse_key("vintrdp").unwrap()));
    let (_, _, display_at) = recv(&m);
    let mut w = Vec::new();
    array8(&mut w, b"XDM-AUTHENTICATION-1");
    array8(&mut w, b"testhost");
    array8(&mut w, b"Willing to manage");
    send(&m, display_at, 5, &w);
    let (op, _, _) = recv(&m);
    assert_eq!(op, 7);
    // It answers with another key.
    let other = xdmauth::parse_key("another").unwrap();
    let mut a = 7u32.to_be_bytes().to_vec();
    array8(&mut a, b"XDM-AUTHENTICATION-1");
    array8(&mut a, &xdmauth::wrap(&[1; 8], &other));
    array8(&mut a, b"XDM-AUTHORIZATION-1");
    array8(&mut a, &xdmauth::wrap(&[2; 8], &other));
    send(&m, display_at, 8, &a);
    for _ in 0..50 {
        if log.0.lock().unwrap().iter().any(|l| l.starts_with("failed")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let failed = log.0.lock().unwrap().iter().find(|l| l.starts_with("failed")).cloned().unwrap_or_default();
    assert!(failed.contains("key"), "{failed}");
}

#[test]
fn a_manager_without_des_gets_the_cookie_asked_for() {
    // As the farm's xdms answer: a Willing naming no authentication.
    let (m, at) = manager();
    let log = Log::default();
    let server = server_with_key(at, &log, Some(xdmauth::parse_key("vintrdp").unwrap()));
    let (_, _, display_at) = recv(&m);
    let mut w = Vec::new();
    array8(&mut w, b"");
    array8(&mut w, b"testhost");
    array8(&mut w, b"Willing to manage");
    send(&m, display_at, 5, &w);
    let (op, body, _) = recv(&m);
    assert_eq!(op, 7);
    assert!(!body.windows(19).any(|w| w == b"XDM-AUTHORIZATION-1"));
    let mut a = 3u32.to_be_bytes().to_vec();
    array8(&mut a, b"");
    array8(&mut a, b"");
    array8(&mut a, b"MIT-MAGIC-COOKIE-1");
    array8(&mut a, &[4; 16]);
    send(&m, display_at, 8, &a);
    assert_eq!(recv(&m).0, 10);
    assert!(x_client(&server, Some(&[4; 16])).is_ok());
}
