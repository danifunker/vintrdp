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
//! The core protocol and SHAPE, BIG-REQUESTS and XC-MISC; drawing as
//! X.org's does it, its mi code ported; TrueColor at 24 bits or the 8-bit
//! visuals (PseudoColor and the rest), with X.org's colormap allocation;
//! backing store and save-unders; X.org's bitmap fonts, and font servers;
//! the selections bridged to the local clipboard; the screen saver; X's
//! host list (xhost); XDMCP ([`xdmcp`]), for a display manager's login
//! screen, and MIT-MAGIC-COOKIE-1; TCP and, on Unix, /tmp/.X11-unix.
//! STUBS.md lists what's left.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod atoms;
mod backing;
mod cmap;
mod color;
mod clipboard;
mod cursor;
mod visual;
mod deliver;
mod draw;
mod event;
mod font;
mod fs;
mod gc;
mod input;
pub mod keymap;
mod mi;
mod pcf;
mod region;
mod render;
mod saver;
mod server;
mod shape;
mod window;
pub mod wire;
pub mod xdmcp;

pub use visual::RootVisual;
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
    /// A client warped the pointer to (x, y) on the screen: move the local
    /// pointer there too.
    fn warp(&self, _x: i32, _y: i32) {}
    /// The cursor to show from now on.
    fn cursor(&self, _cursor: &CursorImage) {}
    /// An X client's selection text, for the local clipboard
    /// (CLIPBOARD, and PRIMARY with [`Settings::primary`]).
    fn clipboard(&self, _text: &str) {}
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
    /// A character typed with no key of its own (an on-screen keyboard,
    /// a paste typed out), as its keysym: pressed and released on the key
    /// that has it, with Shift if need be, or on a spare keycode given it
    /// for the purpose (clients learn of it by MappingNotify).
    Char { keysym: u32 },
    /// The local keyboard's Caps Lock: the server's is toggled to match,
    /// by a press and release of its key. Num Lock isn't followed: it's
    /// Mod2, which the window managers of the time don't leave out of their
    /// button grabs (HP VUE's click to focus stops working with it on), and
    /// Macs report it on; X terminals started with it off, and so does this.
    Locks { caps: bool },
}

pub struct Settings {
    /// The screen's size, for good: without RANDR clients can't learn of a
    /// change, so resizing vintrdp's window doesn't resize the root.
    pub width: u16,
    pub height: u16,
    /// Where to listen: TCP port 6000 + the display number.
    pub listen: SocketAddr,
    /// On Unix systems, listen on /tmp/.X11-unix/XN too (N the display).
    pub unix_socket: bool,
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
    /// Bridge PRIMARY to the local clipboard as well as CLIPBOARD: what's
    /// selected is copied, and the local text pastes with the middle button.
    pub primary: bool,
    /// The root's depth and default visual: TrueColor at 24 bits, or
    /// PseudoColor at 8, as an 8-bit X terminal (old CDE and Motif
    /// applications prefer it).
    pub visual: RootVisual,
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
    /// The Unix socket, removed when the server stops.
    #[cfg(unix)]
    unix_path: Option<std::path::PathBuf>,
}

impl Server {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn input(&self, ev: InputEvent) {
        let _ = self.tx.send(Msg::Input(ev));
    }

    /// The local clipboard's text: X clients may paste it from CLIPBOARD
    /// (and PRIMARY with [`Settings::primary`]).
    pub fn set_clipboard(&self, text: String) {
        let _ = self.tx.send(Msg::Clipboard(text));
    }

    /// Disconnect every client and stop listening.
    pub fn stop(&self) {
        if !self.stop.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(Msg::Stop);
            if let Some(x) = &self.xdmcp {
                let _ = x.send(xdmcp::Event::Stop);
            }
            #[cfg(unix)]
            if let Some(p) = &self.unix_path {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    /// The Unix socket clients on this machine may connect to, if any.
    #[cfg(unix)]
    pub fn unix_socket(&self) -> Option<&std::path::Path> {
        self.unix_path.as_deref()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Who may connect besides this machine, by address: X's access control
/// list, which ListHosts shows and xhost (ChangeHosts, SetAccessControl)
/// changes, from this machine only.
pub(crate) struct Hosts {
    /// Off: anyone may connect (`allow=*`, `xhost +`).
    pub enabled: bool,
    /// Family (0 Internet, 6 Internet6) and address, as ListHosts has them.
    pub list: Vec<(u8, Vec<u8>)>,
}

pub(crate) const FAMILY_INTERNET: u8 = 0;
pub(crate) const FAMILY_INTERNET6: u8 = 6;

impl Hosts {
    fn new(allow: &[String]) -> io::Result<Hosts> {
        let mut hosts = Hosts { enabled: true, list: Vec::new() };
        for name in allow.iter().map(|a| a.trim()).filter(|a| !a.is_empty()) {
            if name == "*" {
                hosts.enabled = false;
                continue;
            }
            let addrs = (name, 0).to_socket_addrs().map_err(|e| io::Error::new(e.kind(), format!("{name}: {e}")))?;
            for a in addrs {
                let entry = family_address(a.ip());
                if !hosts.list.contains(&entry) {
                    hosts.list.push(entry);
                }
            }
        }
        Ok(hosts)
    }

    fn allows(&self, peer: IpAddr) -> bool {
        !self.enabled || self.list.contains(&family_address(peer))
    }
}

/// An address as X's host list has it (IPv4-mapped IPv6 as IPv4).
pub(crate) fn family_address(ip: IpAddr) -> (u8, Vec<u8>) {
    match canonical(ip) {
        IpAddr::V4(v4) => (FAMILY_INTERNET, v4.octets().to_vec()),
        IpAddr::V6(v6) => (FAMILY_INTERNET6, v6.octets().to_vec()),
    }
}

/// Whether a TCP peer is this machine: loopback, or one of its own addresses.
fn is_local(stream: &TcpStream, peer: IpAddr) -> bool {
    let peer = canonical(peer);
    peer.is_loopback() || stream.local_addr().is_ok_and(|l| canonical(l.ip()) == peer)
}

/// Whether the peer may connect by its address alone.
fn allowed(hosts: &Mutex<Hosts>, local: bool, peer: Option<IpAddr>, auth: &Auth) -> bool {
    if local && !auth.strict {
        return true;
    }
    let Some(peer) = peer else { return false };
    if auth.manager.lock().unwrap().is_some_and(|m| canonical(m) == canonical(peer)) {
        return true;
    }
    hosts.lock().unwrap().allows(peer)
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

/// A client's connection: TCP, or the Unix socket.
pub(crate) enum Stream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl Stream {
    pub fn try_clone(&self) -> io::Result<Stream> {
        Ok(match self {
            Stream::Tcp(s) => Stream::Tcp(s.try_clone()?),
            #[cfg(unix)]
            Stream::Unix(s) => Stream::Unix(s.try_clone()?),
        })
    }

    pub fn shutdown(&self) {
        let _ = match self {
            Stream::Tcp(s) => s.shutdown(Shutdown::Both),
            #[cfg(unix)]
            Stream::Unix(s) => s.shutdown(Shutdown::Both),
        };
    }

    fn peer(&self) -> String {
        match self {
            Stream::Tcp(s) => s.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default(),
            #[cfg(unix)]
            Stream::Unix(_) => "this machine (Unix socket)".into(),
        }
    }
}

impl Read for &Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => (&*s).read(buf),
            #[cfg(unix)]
            Stream::Unix(s) => (&*s).read(buf),
        }
    }
}

impl Write for &Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => (&*s).write(buf),
            #[cfg(unix)]
            Stream::Unix(s) => (&*s).write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// What may wait to be written to a client: past this, a client that
/// doesn't read what it's sent (replies, events) is disconnected, as its
/// queue would otherwise grow without end. Four screens' worth of
/// GetImage at 1920x1200.
pub(crate) const MAX_PENDING: usize = 40 << 20;

/// A client's writer: what the server sends it, and how much of that
/// hasn't been written yet.
pub(crate) struct Out {
    tx: mpsc::Sender<Vec<u8>>,
    pending: Arc<std::sync::atomic::AtomicUsize>,
    /// Went over the limit.
    pub overflowed: AtomicBool,
}

impl Out {
    /// Queue a packet; false if the client has too much waiting already
    /// (and is to be disconnected).
    pub fn send(&self, packet: Vec<u8>) -> bool {
        let n = packet.len();
        if self.pending.fetch_add(n, Ordering::AcqRel) + n > MAX_PENDING {
            self.pending.fetch_sub(n, Ordering::AcqRel);
            self.overflowed.store(true, Ordering::Release);
            return false;
        }
        let _ = self.tx.send(packet);
        true
    }

    #[cfg(test)]
    pub fn for_test() -> (Out, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel();
        (Out { tx, pending: Arc::default(), overflowed: AtomicBool::new(false) }, rx)
    }
}

/// The Unix socket for display `n`, as X.org names it.
#[cfg(unix)]
pub fn unix_socket_path(display: u16) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("/tmp/.X11-unix/X{display}"))
}

/// Listen on /tmp/.X11-unix/XN too, for `DISPLAY=:N`. None (with a
/// warning) if another server has it, or it can't be made.
#[cfg(unix)]
fn listen_unix(display: u16) -> Option<(std::os::unix::net::UnixListener, std::path::PathBuf)> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    let path = unix_socket_path(display);
    let dir = path.parent().unwrap();
    if !dir.exists() {
        // As X.org makes it: anyone may put a socket there, only its owner remove it.
        if std::fs::create_dir(dir).is_ok() {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o1777));
        }
    }
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            eprintln!("vintrdp: X11: another X server has {}: only TCP for display :{display}", path.display());
            return None;
        }
        // Left by a server that's gone.
        let _ = std::fs::remove_file(&path);
    }
    match UnixListener::bind(&path) {
        Ok(l) if l.set_nonblocking(true).is_ok() => Some((l, path)),
        Ok(_) => None,
        Err(e) => {
            eprintln!("vintrdp: X11: can't listen on {}: {e}", path.display());
            None
        }
    }
}

/// Start listening, and serving clients: on TCP port 6000 + N, and on
/// Unix systems the socket /tmp/.X11-unix/XN as well, for clients on this
/// machine (`DISPLAY=:N`).
pub fn start(settings: Settings, host: Box<dyn Host>) -> io::Result<Server> {
    let hosts = Arc::new(Mutex::new(Hosts::new(&settings.allow)?));
    let listener = TcpListener::bind(settings.listen)?;
    let addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let verbose = settings.verbose;
    let auth = Arc::new(Auth { cookie: Mutex::new(settings.cookie.clone()), manager: Mutex::new(None), strict: settings.xdmcp.is_some() });
    // Bounded, so a client sending faster than the server works waits.
    let (tx, rx) = mpsc::sync_channel(256);
    let stop = Arc::new(AtomicBool::new(false));
    let display = addr.port().wrapping_sub(6000);
    let xdmcp = match settings.xdmcp.clone() {
        Some(cfg) => {
            let (etx, erx) = mpsc::channel();
            xdmcp::start(cfg, display, auth.clone(), tx.clone(), erx, verbose)?;
            Some(etx)
        }
        None => None,
    };
    #[cfg(unix)]
    let unix = if settings.unix_socket { listen_unix(display) } else { None };
    #[cfg(not(unix))]
    let unix: Option<((), ())> = None;
    let unix_path = unix.as_ref().map(|u| u.1.clone());
    {
        let (x, hosts) = (xdmcp.clone(), hosts.clone());
        std::thread::Builder::new().name("x11-server".into()).spawn(move || server::run(rx, settings, host, x, hosts))?;
    }
    {
        let (tx, stop, auth) = (tx.clone(), stop.clone(), auth.clone());
        let unix = unix.map(|u| u.0);
        std::thread::Builder::new().name("x11-listen".into()).spawn(move || accept(listener, unix, hosts, auth, tx, stop, verbose))?;
    }
    #[cfg(not(unix))]
    let _ = unix_path;
    Ok(Server {
        tx,
        xdmcp,
        stop,
        addr,
        #[cfg(unix)]
        unix_path,
    })
}

#[cfg(unix)]
type UnixListener = std::os::unix::net::UnixListener;
#[cfg(not(unix))]
type UnixListener = ();

fn accept(listener: TcpListener, unix: Option<UnixListener>, hosts: Arc<Mutex<Hosts>>, auth: Arc<Auth>, tx: mpsc::SyncSender<Msg>, stop: Arc<AtomicBool>, verbose: bool) {
    #[cfg(not(unix))]
    let _ = &unix;
    let mut next = 0u64;
    while !stop.load(Ordering::SeqCst) {
        let mut got = None;
        match listener.accept() {
            Ok((stream, peer)) => {
                let local = is_local(&stream, peer.ip());
                got = Some((Stream::Tcp(stream), local, Some(peer.ip())));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => {
                eprintln!("vintrdp: X11: {e}");
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        }
        #[cfg(unix)]
        if got.is_none() {
            if let Some(Ok((stream, _))) = unix.as_ref().map(|l| l.accept()) {
                got = Some((Stream::Unix(stream), true, None));
            }
        }
        let Some((stream, local, peer)) = got else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        let by_address = allowed(&hosts, local, peer, &auth);
        if !by_address && auth.cookie.lock().unwrap().is_none() {
            eprintln!("vintrdp: X11: refused a connection from {}", stream.peer());
            stream.shutdown();
            continue;
        }
        if verbose {
            eprintln!("vintrdp: X11: connection from {}", stream.peer());
        }
        next += 1;
        let (conn, tx) = (next, tx.clone());
        // Others must give the cookie.
        let auth = (!by_address).then(|| auth.clone());
        let spawned = std::thread::Builder::new().name("x11-client".into()).spawn(move || {
            if let Err(e) = client(conn, stream, local, &tx, auth.as_deref()) {
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
fn client(conn: u64, stream: Stream, local: bool, tx: &mpsc::SyncSender<Msg>, auth: Option<&Auth>) -> io::Result<()> {
    match &stream {
        Stream::Tcp(s) => {
            s.set_nonblocking(false)?;
            let _ = s.set_nodelay(true);
        }
        #[cfg(unix)]
        Stream::Unix(s) => s.set_nonblocking(false)?,
    }
    let reader = stream.try_clone()?;
    let mut input = io::BufReader::with_capacity(64 * 1024, &reader);
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
            eprintln!("vintrdp: X11: refused a client from {}: {}", stream.peer(), reason.trim_end());
            let _ = (&stream).write_all(&server::setup_failed(order, reason));
            stream.shutdown();
            return Ok(());
        }
    }

    // The writer: bounded by MAX_PENDING (Out::send).
    let (otx, replies) = mpsc::channel::<Vec<u8>>();
    let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let out = Out { tx: otx, pending: pending.clone(), overflowed: AtomicBool::new(false) };
    let writer = stream.try_clone()?;
    std::thread::Builder::new().name("x11-writer".into()).spawn(move || {
        for packet in replies {
            if (&writer).write_all(&packet).is_err() {
                writer.shutdown();
                break;
            }
            pending.fetch_sub(packet.len(), Ordering::AcqRel);
        }
    })?;
    let big = Arc::new(AtomicBool::new(false));
    let shutdown = stream.try_clone()?;
    tx.send(Msg::Connect { conn, order, major, out, stream: shutdown, local, big: big.clone() }).map_err(|_| closed())?;
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
