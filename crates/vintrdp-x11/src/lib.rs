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
//! XC-MISC; drawing as X.org's does it, its mi code ported. STUBS.md lists
//! what's left.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

mod atoms;
mod color;
mod cursor;
mod deliver;
mod draw;
mod event;
mod font;
mod gc;
mod input;
pub mod keymap;
#[allow(dead_code)]
mod mi;
mod region;
mod render;
mod server;
mod shape;
mod window;
pub mod wire;

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
    pub verbose: bool,
}

/// The running server; stops when dropped.
pub struct Server {
    tx: mpsc::SyncSender<Msg>,
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

    fn allows(&self, stream: &TcpStream, peer: IpAddr) -> bool {
        let canonical = |ip: IpAddr| match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 => v4,
        };
        let peer = canonical(peer);
        // This machine: loopback, or one of its own addresses.
        if peer.is_loopback() || stream.local_addr().is_ok_and(|l| canonical(l.ip()) == peer) {
            return true;
        }
        match self {
            Access::Anyone => true,
            Access::Hosts(hosts) => hosts.iter().any(|&h| canonical(h) == peer),
        }
    }
}

/// Start listening, and serving clients.
// STUB(x11): TCP only; no Unix socket (/tmp/.X11-unix/XN), so DISPLAY=:N without a host doesn't work
pub fn start(settings: Settings, host: Box<dyn Host>) -> io::Result<Server> {
    let access = Access::new(&settings.allow)?;
    let listener = TcpListener::bind(settings.listen)?;
    let addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let verbose = settings.verbose;
    // Bounded, so a client sending faster than the server works waits.
    let (tx, rx) = mpsc::sync_channel(256);
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::Builder::new().name("x11-server".into()).spawn(move || server::run(rx, settings, host))?;
    {
        let (tx, stop) = (tx.clone(), stop.clone());
        std::thread::Builder::new().name("x11-listen".into()).spawn(move || accept(listener, access, tx, stop, verbose))?;
    }
    Ok(Server { tx, stop, addr })
}

fn accept(listener: TcpListener, access: Access, tx: mpsc::SyncSender<Msg>, stop: Arc<AtomicBool>, verbose: bool) {
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
        if !access.allows(&stream, peer.ip()) {
            eprintln!("vintrdp: X11: refused a connection from {}", peer.ip());
            let _ = stream.shutdown(Shutdown::Both);
            continue;
        }
        if verbose {
            eprintln!("vintrdp: X11: connection from {peer}");
        }
        next += 1;
        let (conn, tx) = (next, tx.clone());
        let spawned = std::thread::Builder::new().name("x11-client".into()).spawn(move || {
            if let Err(e) = client(conn, stream, &tx) {
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
fn client(conn: u64, stream: TcpStream, tx: &mpsc::SyncSender<Msg>) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    let _ = stream.set_nodelay(true);
    let mut input = io::BufReader::with_capacity(64 * 1024, stream.try_clone()?);
    let mut head = [0u8; 12];
    input.read_exact(&mut head)?;
    let order = Order::from_byte(head[0]).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an X client"))?;
    let major = order.u16([head[2], head[3]]);
    let (name, data) = (order.u16([head[6], head[7]]) as usize, order.u16([head[8], head[9]]) as usize);
    // STUB(x11): MIT-MAGIC-COOKIE-1 isn't checked: who may connect is decided by address
    let mut auth = vec![0; name + wire::pad(name) + data + wire::pad(data)];
    input.read_exact(&mut auth)?;

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
