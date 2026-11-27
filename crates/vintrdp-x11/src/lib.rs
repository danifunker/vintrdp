//! An X11 server in pure Rust: X clients on other machines draw into a
//! vintrdp window, as on an X terminal. It knows nothing of the rest of
//! vintrdp; the program shows its screen through a [`Host`].
//!
//! One thread runs every client's requests, in the order they arrive, as the
//! protocol requires; each client has a thread reading its requests and one
//! writing its replies, so a slow client holds up no one else. Only this
//! machine may connect unless [`Settings::allow`] names others: an X server
//! is open to anyone who reaches its port, keyboard and screen included.
//!
//! The core protocol (TrueColor only so far) and SHAPE, BIG-REQUESTS and
//! XC-MISC; drawing as X.org's does it, its mi code ported; X.org's bitmap
//! fonts; XDMCP ([`xdmcp`]), for a display manager's login screen, and
//! MIT-MAGIC-COOKIE-1. STUBS.md lists what's left.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod atoms;
mod backing;
mod color;
mod cursor;
mod deliver;
mod draw;
mod event;
mod font;
mod gc;
mod input;
pub mod keymap;
mod mi;
mod pcf;
mod region;
mod render;
mod server;
mod shape;
mod window;
pub mod wire;
pub mod xdmcp;

use server::Msg;
use wire::Order;

/// What the server needs of the program showing it. Called on the server's
/// thread.
pub trait Host: Send + 'static {
    /// The screen is `width` by `height`; called once, before anything is drawn.
    fn resize(&self, width: u16, height: u16);
    /// Pixels as `0x00RRGGBB`, `w` by `h`, row-major, at (x, y).
    fn draw(&self, x: usize, y: usize, w: usize, h: usize, px: &[u32]);
    /// A batch of drawing is done; show it.
    fn updated(&self);
    fn bell(&self) {}
    /// The cursor to show from now on.
    fn cursor(&self, _cursor: &CursorImage) {}
    // STUB(x11): no clipboard: PRIMARY and CLIPBOARD aren't bridged to the local clipboard (phase 3)
    /// How many clients are connected, when it changes.
    fn clients(&self, _count: usize) {}
    /// How asking a display manager for a session goes ([`Settings::xdmcp`]).
    fn xdmcp(&self, _status: &xdmcp::Status) {}
}

pub struct CursorImage {
    pub width: usize,
    pub height: usize,
    pub hot_x: usize,
    pub hot_y: usize,
    /// Unpremultiplied RGBA, top row first.
    pub rgba: Vec<u8>,
}

/// Keyboard and pointer input for the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// A keycode (see [`keymap::keycode`]).
    Key { code: u8, down: bool },
    /// The pointer, in screen coordinates.
    Motion { x: i32, y: i32 },
    /// Buttons 1 to 5: left, middle, right, wheel up, wheel down.
    Button { button: u8, down: bool },
}

pub struct Settings {
    /// The screen's size, for good: without RANDR clients can't learn of a
    /// change, so resizing vintrdp's window doesn't resize the root.
    pub width: u16,
    pub height: u16,
    /// Where to listen: TCP port 6000 + the display number.
    pub listen: SocketAddr,
    /// Other machines that may connect, by name or address; `*` for any.
    /// This machine always may.
    pub allow: Vec<String>,
    pub keymap: keymap::Keymap,
    /// Ask a display manager to manage this server (XDMCP). Every client
    /// must then give the session's MIT-MAGIC-COOKIE-1, but for the
    /// [`Settings::allow`] hosts; when the session's last client leaves,
    /// the server resets and asks again.
    pub xdmcp: Option<xdmcp::Xdmcp>,
    /// Without XDMCP: a MIT-MAGIC-COOKIE-1 that lets in clients from any
    /// host that reaches the server, besides this machine and `allow`.
    pub cookie: Option<Vec<u8>>,
    /// Font folders on this machine, after the bundled ones in the path.
    pub font_path: Vec<String>,
    pub verbose: bool,
}

/// Who may connect without being one of the hosts allowed.
pub(crate) struct Auth {
    /// The MIT-MAGIC-COOKIE-1 that lets a client in.
    pub cookie: Mutex<Option<Vec<u8>>>,
    /// A display manager that gave no cookie: its host may connect.
    pub manager: Mutex<Option<IpAddr>>,
    /// XDMCP: this machine needs the cookie too, as with X.org's -auth.
    pub strict: bool,
}

/// The running server; stops when dropped.
pub struct Server {
    tx: mpsc::SyncSender<Msg>,
    xdmcp: Option<mpsc::Sender<xdmcp::Event>>,
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
}

impl Server {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn input(&self, ev: InputEvent) {
        let _ = self.tx.send(Msg::Input(ev));
    }

    /// Disconnect every client and stop listening.
    pub fn stop(&self) {
        if !self.stop.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(Msg::Stop);
            if let Some(x) = &self.xdmcp {
                let _ = x.send(xdmcp::Event::Stop);
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Who may connect besides this machine.
enum Access {
    Hosts(Vec<IpAddr>),
    Anyone,
}

impl Access {
    fn new(allow: &[String]) -> io::Result<Access> {
        let mut hosts = Vec::new();
        for name in allow.iter().map(|a| a.trim()).filter(|a| !a.is_empty()) {
            if name == "*" {
                return Ok(Access::Anyone);
            }
            let addrs = (name, 0).to_socket_addrs().map_err(|e| io::Error::new(e.kind(), format!("{name}: {e}")))?;
            hosts.extend(addrs.map(|a| a.ip()));
        }
        Ok(Access::Hosts(hosts))
    }

    /// Whether the peer may connect by its address alone.
    fn allows(&self, stream: &TcpStream, peer: IpAddr, auth: &Auth) -> bool {
        let peer = canonical(peer);
        // This machine: loopback, or one of its own addresses.
        let local = peer.is_loopback() || stream.local_addr().is_ok_and(|l| canonical(l.ip()) == peer);
        if local && !auth.strict || auth.manager.lock().unwrap().is_some_and(|m| canonical(m) == peer) {
            return true;
        }
        match self {
            Access::Anyone => true,
            Access::Hosts(hosts) => hosts.iter().any(|&h| canonical(h) == peer),
        }
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// A new MIT-MAGIC-COOKIE-1: 16 random bytes.
pub fn new_cookie() -> Vec<u8> {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    // std's hasher keys are seeded from the system's random source.
    (0..2)
        .flat_map(|i: u64| {
            let mut h = RandomState::new().build_hasher();
            h.write_u64(i);
            h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
            h.finish().to_le_bytes()
        })
        .collect()
}

/// Start listening, and serving clients.
// STUB(x11): TCP only; no Unix socket (/tmp/.X11-unix/XN), so DISPLAY=:N without a host doesn't work
pub fn start(settings: Settings, host: Box<dyn Host>) -> io::Result<Server> {
    let access = Access::new(&settings.allow)?;
    let listener = TcpListener::bind(settings.listen)?;
    let addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let verbose = settings.verbose;
    let auth = Arc::new(Auth { cookie: Mutex::new(settings.cookie.clone()), manager: Mutex::new(None), strict: settings.xdmcp.is_some() });
    // Bounded, so a client sending faster than the server works waits.
    let (tx, rx) = mpsc::sync_channel(256);
    let stop = Arc::new(AtomicBool::new(false));
    let xdmcp = match settings.xdmcp.clone() {
        Some(cfg) => {
            let (etx, erx) = mpsc::channel();
            let display = addr.port().wrapping_sub(6000);
            xdmcp::start(cfg, display, auth.clone(), tx.clone(), erx, verbose)?;
            Some(etx)
        }
        None => None,
    };
    {
        let x = xdmcp.clone();
        std::thread::Builder::new().name("x11-server".into()).spawn(move || server::run(rx, settings, host, x))?;
    }
    {
        let (tx, stop, auth) = (tx.clone(), stop.clone(), auth.clone());
        std::thread::Builder::new().name("x11-listen".into()).spawn(move || accept(listener, access, auth, tx, stop, verbose))?;
    }
    Ok(Server { tx, xdmcp, stop, addr })
}

fn accept(listener: TcpListener, access: Access, auth: Arc<Auth>, tx: mpsc::SyncSender<Msg>, stop: Arc<AtomicBool>, verbose: bool) {
    let mut next = 0u64;
    while !stop.load(Ordering::SeqCst) {
        let (stream, peer) = match listener.accept() {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(e) => {
                eprintln!("vintrdp: X11: {e}");
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let by_address = access.allows(&stream, peer.ip(), &auth);
        if !by_address && auth.cookie.lock().unwrap().is_none() {
            eprintln!("vintrdp: X11: refused a connection from {}", peer.ip());
            let _ = stream.shutdown(Shutdown::Both);
            continue;
        }
        if verbose {
            eprintln!("vintrdp: X11: connection from {peer}");
        }
        next += 1;
        let (conn, tx) = (next, tx.clone());
        // Others must give the cookie.
        let auth = (!by_address).then(|| auth.clone());
        let spawned = std::thread::Builder::new().name("x11-client".into()).spawn(move || {
            if let Err(e) = client(conn, stream, &tx, auth.as_deref()) {
                if verbose {
                    eprintln!("vintrdp: X11: client {conn}: {e}");
                }
            }
            let _ = tx.send(Msg::Gone { conn });
        });
        if let Err(e) = spawned {
            eprintln!("vintrdp: X11: {e}");
        }
    }
}

/// The largest request with BIG-REQUESTS, in 4-byte units: 16 MB.
pub(crate) const MAX_BIG_REQUEST: u32 = 4 << 20;

/// A client's reader: the connection setup, then its requests, whole, for
/// the server thread.
fn client(conn: u64, stream: TcpStream, tx: &mpsc::SyncSender<Msg>, auth: Option<&Auth>) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    let _ = stream.set_nodelay(true);
    let mut input = io::BufReader::with_capacity(64 * 1024, stream.try_clone()?);
    let mut head = [0u8; 12];
    input.read_exact(&mut head)?;
    let order = Order::from_byte(head[0]).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an X client"))?;
    let major = order.u16([head[2], head[3]]);
    let (name, data) = (order.u16([head[6], head[7]]) as usize, order.u16([head[8], head[9]]) as usize);
    let mut given = vec![0; name + wire::pad(name) + data + wire::pad(data)];
    input.read_exact(&mut given)?;
    if let Some(auth) = auth {
        let (proto, key) = (&given[..name], &given[name + wire::pad(name)..][..data]);
        let cookie = auth.cookie.lock().unwrap().clone();
        let refused = if proto.is_empty() {
            Some("Authorization required, but no authorization protocol specified\n")
        } else if proto != xdmcp::COOKIE || cookie.as_deref() != Some(key) {
            Some("Invalid MIT-MAGIC-COOKIE-1 key")
        } else {
            None
        };
        if let Some(reason) = refused {
            let peer = stream.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
            eprintln!("vintrdp: X11: refused a client from {peer}: {}", reason.trim_end());
            let mut s = &stream;
            let _ = s.write_all(&server::setup_failed(order, reason));
            let _ = stream.shutdown(Shutdown::Both);
            return Ok(());
        }
    }

    // STUB(x11): unbounded: a client that never reads its replies grows this queue
    let (out, replies) = mpsc::channel::<Vec<u8>>();
    let mut writer = stream.try_clone()?;
    std::thread::Builder::new().name("x11-writer".into()).spawn(move || {
        for packet in replies {
            if writer.write_all(&packet).is_err() {
                let _ = writer.shutdown(Shutdown::Both);
                break;
            }
        }
    })?;
    let big = Arc::new(AtomicBool::new(false));
    let shutdown = stream.try_clone()?;
    tx.send(Msg::Connect { conn, order, major, out, stream: shutdown, big: big.clone() }).map_err(|_| closed())?;
    if major != 11 {
        return Ok(());
    }

    let mut header = [0u8; 4];
    loop {
        input.read_exact(&mut header)?;
        let mut words = order.u16([header[2], header[3]]) as u32;
        let mut skip = 4;
        if words == 0 {
            if !big.load(Ordering::Acquire) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "a request of length 0"));
            }
            let mut b = [0u8; 4];
            input.read_exact(&mut b)?;
            words = order.u32(b);
            skip = 8;
            if !(2..=MAX_BIG_REQUEST).contains(&words) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("a request of {words} words")));
            }
        }
        // The request as if it were a small one: header, then the rest.
        let mut req = vec![0u8; words as usize * 4 - skip + 4];
        req[..4].copy_from_slice(&header);
        input.read_exact(&mut req[4..])?;
        tx.send(Msg::Request { conn, req }).map_err(|_| closed())?;
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the server stopped")
}
