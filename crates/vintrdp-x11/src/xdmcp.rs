//! XDMCP 1.0, the display's side: ask a display manager (xdm, dtlogin,
//! vuelogin) to manage this server, as X.org's `-query`, `-broadcast` and
//! `-indirect` do. Query, Willing, Request, Accept (a session and its
//! MIT-MAGIC-COOKIE-1), Manage; then KeepAlive and Alive while the session
//! runs. When the session's last client leaves the server resets and this
//! starts again, so logging out brings the login screen back.
//!
//! The manager connects back to the connection address sent in Request: by
//! default the address this machine reaches it from, which a host behind
//! NAT can usually reach too (its replies come from the NAT gateway, but
//! the address in the packet is ours). [`Xdmcp::address`] overrides it.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use crate::server::Msg;

/// The XDMCP port.
pub const PORT: u16 = 177;

const VERSION: u16 = 1;
const BROADCAST_QUERY: u16 = 1;
const QUERY: u16 = 2;
const INDIRECT_QUERY: u16 = 3;
const WILLING: u16 = 5;
const UNWILLING: u16 = 6;
const REQUEST: u16 = 7;
const ACCEPT: u16 = 8;
const DECLINE: u16 = 9;
const MANAGE: u16 = 10;
const REFUSE: u16 = 11;
const FAILED: u16 = 12;
const KEEPALIVE: u16 = 13;
const ALIVE: u16 = 14;

/// X.org's connection types: FamilyInternet, FamilyInternet6.
const INTERNET: u16 = 0;
const INTERNET6: u16 = 6;

pub const COOKIE: &[u8] = b"MIT-MAGIC-COOKIE-1";

/// How long to wait for an answer, doubling each time, as X.org does:
/// 2, 4, 8, 16 and 32 seconds.
const RETRIES: [u64; 5] = [2, 4, 8, 16, 32];
/// How often to ask whether the session is still there (X.org's default).
const KEEPALIVE_EVERY: Duration = Duration::from_secs(180);
/// How soon to ask after a client leaves (not X.org's: it waits for the
/// next KeepAlive, so a logout that leaves clients behind takes minutes).
const SOON: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Ask one host.
    Query,
    /// Ask every host on the network; the first that's willing manages.
    Broadcast,
    /// Ask a host to show a chooser of hosts.
    Indirect,
}

#[derive(Clone, Debug)]
pub struct Xdmcp {
    /// The display manager's host (Query, Indirect), or where to broadcast.
    pub host: String,
    pub port: u16,
    pub mode: Mode,
    /// The address the manager connects back to; None for the one this
    /// machine reaches it from.
    pub address: Option<IpAddr>,
}

/// What the server tells the XDMCP side.
pub(crate) enum Event {
    /// A client connected (the manager, as it starts managing).
    Opened,
    /// A client left: the session may be over, though some of its clients
    /// stay (as with CDE), so ask soon.
    Left,
    /// The server reset: ask for a new session.
    Reset,
    Stop,
}

/// What the XDMCP side tells the server (and through it the program).
pub enum Status {
    /// Progress, for the window's title.
    Progress(String),
    /// The session runs.
    Managed { host: String },
    /// No session to be had: why, for the user.
    Failed(String),
    /// The manager says the session is gone: reset the server.
    Dead,
}

/// Start asking for a session, on a thread of its own.
pub(crate) fn start(
    cfg: Xdmcp,
    display: u16,
    auth: Arc<crate::Auth>,
    server: SyncSender<Msg>,
    events: Receiver<Event>,
    verbose: bool,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("x11-xdmcp".into())
        .spawn(move || {
            let mut x = Client {
                cfg,
                display,
                auth,
                server,
                events,
                verbose,
                socket: None,
            };
            if let Err(Stop) = x.run() {
                // Stopped, or failed and said so.
            }
        })?;
    Ok(())
}

/// The thread ends: the server stopped, or there's no session to be had.
struct Stop;

struct Client {
    cfg: Xdmcp,
    display: u16,
    auth: Arc<crate::Auth>,
    server: SyncSender<Msg>,
    events: Receiver<Event>,
    verbose: bool,
    socket: Option<UdpSocket>,
}

/// A packet's fields, read in order (all big-endian).
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (a, b) = (self.0.get(..n)?, self.0.get(n..)?);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn array8(&mut self) -> Option<&'a [u8]> {
        let n = self.u16()? as usize;
        self.take(n)
    }
}

/// A packet being written.
#[derive(Default)]
struct Packet(Vec<u8>);

impl Packet {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn array8(&mut self, b: &[u8]) -> &mut Self {
        self.u16(b.len() as u16);
        self.0.extend_from_slice(b);
        self
    }
    /// With its header: version, opcode, length.
    fn finish(&self, opcode: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(6 + self.0.len());
        out.extend_from_slice(&VERSION.to_be_bytes());
        out.extend_from_slice(&opcode.to_be_bytes());
        out.extend_from_slice(&(self.0.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.0);
        out
    }
}

/// A packet received: its opcode and body, if it's XDMCP 1.
fn parse(buf: &[u8]) -> Option<(u16, &[u8])> {
    let mut f = Fields(buf);
    let (version, opcode, len) = (f.u16()?, f.u16()?, f.u16()? as usize);
    (version == VERSION).then_some(())?;
    Some((opcode, f.take(len)?))
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim().to_string()
}

/// The address this machine reaches `to` from.
fn local_address(to: SocketAddr) -> Option<IpAddr> {
    let bind: SocketAddr = if to.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let s = UdpSocket::bind(bind).ok()?;
    s.connect(to).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// An answer to wait for.
enum Got<T> {
    Answer(T),
    /// Nothing within the time.
    Timeout,
}

impl Client {
    fn say(&self, status: Status) {
        let _ = self.server.send(Msg::Xdmcp(status));
    }

    fn progress(&self, s: String) {
        if self.verbose {
            eprintln!("vintrdp: XDMCP: {s}");
        }
        self.say(Status::Progress(s));
    }

    /// Tell the user why, and end.
    fn fail(&self, why: String) -> Stop {
        eprintln!("vintrdp: XDMCP: {why}");
        self.say(Status::Failed(why));
        Stop
    }

    fn socket(&mut self, v6: bool) -> Result<&UdpSocket, Stop> {
        if self
            .socket
            .as_ref()
            .is_some_and(|s| s.local_addr().is_ok_and(|a| a.is_ipv6() == v6))
        {
            return Ok(self.socket.as_ref().unwrap());
        }
        let bind: SocketAddr = if v6 {
            (Ipv6Addr::UNSPECIFIED, 0).into()
        } else {
            (Ipv4Addr::UNSPECIFIED, 0).into()
        };
        let s = UdpSocket::bind(bind).map_err(|e| self.fail(format!("Unable to use UDP: {e}")))?;
        let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
        if self.cfg.mode == Mode::Broadcast {
            let _ = s.set_broadcast(true);
        }
        self.socket = Some(s);
        Ok(self.socket.as_ref().unwrap())
    }

    fn send(&mut self, to: SocketAddr, packet: &[u8]) -> Result<(), Stop> {
        let s = self.socket(to.is_ipv6())?;
        if let Err(e) = s.send_to(packet, to) {
            return Err(self.fail(format!("Unable to reach {to}: {e}")));
        }
        Ok(())
    }

    /// Server events while waiting: Stop ends, Reset starts again.
    fn poll_events(&self) -> Result<Option<Event>, Stop> {
        match self.events.try_recv() {
            Ok(Event::Stop) | Err(TryRecvError::Disconnected) => Err(Stop),
            Ok(e) => Ok(Some(e)),
            Err(TryRecvError::Empty) => Ok(None),
        }
    }

    /// Send `packet` to `to`, again and again with longer waits, until `want`
    /// takes something that arrives (from anyone).
    fn exchange<T>(
        &mut self,
        to: SocketAddr,
        packet: &[u8],
        mut want: impl FnMut(&mut Self, SocketAddr, u16, &[u8]) -> Option<Result<T, Stop>>,
    ) -> Result<Got<T>, Stop> {
        let mut buf = [0u8; 2048];
        for wait in RETRIES {
            self.send(to, packet)?;
            let until = Instant::now() + Duration::from_secs(wait);
            while Instant::now() < until {
                if let Some(Event::Reset) = self.poll_events()? {
                    // A reset while asking: nothing to do but go on asking.
                }
                let s = self.socket.as_ref().unwrap();
                let (n, from) = match s.recv_from(&mut buf) {
                    Ok(r) => r,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    // ICMP port unreachable comes back as an error on some systems.
                    Err(_) => continue,
                };
                let Some((opcode, body)) = parse(&buf[..n]) else {
                    continue;
                };
                if let Some(r) = want(self, from, opcode, body) {
                    return r.map(Got::Answer);
                }
            }
        }
        Ok(Got::Timeout)
    }

    fn manager_address(&self) -> Result<SocketAddr, Stop> {
        let host = self.cfg.host.trim_start_matches('[').trim_end_matches(']');
        if self.cfg.mode == Mode::Broadcast && (host.is_empty() || host == "*") {
            return Ok((Ipv4Addr::BROADCAST, self.cfg.port).into());
        }
        match (host, self.cfg.port)
            .to_socket_addrs()
            .map(|mut a| a.next())
        {
            Ok(Some(a)) => Ok(a),
            Ok(None) => Err(self.fail(format!("{host}: no such host"))),
            Err(e) => Err(self.fail(format!("{host}: {e}"))),
        }
    }

    fn run(&mut self) -> Result<(), Stop> {
        let target = self.manager_address()?;
        loop {
            match self.session(target)? {
                // Back to the start: a new Query.
                Next::Again => continue,
            }
        }
    }

    /// One session: Query to the end of the session (a reset).
    fn session(&mut self, target: SocketAddr) -> Result<Next, Stop> {
        let host = self.cfg.host.clone();
        let (opcode, what) = match self.cfg.mode {
            Mode::Query => (QUERY, format!("Asking {host} for a login screen")),
            // STUB(x11): Broadcast and the chooser's side of Indirect (ForwardQuery from the host picked) are untested: the farm's managers sit behind NAT and answer IndirectQuery directly (phase 4)
            Mode::Broadcast => (
                BROADCAST_QUERY,
                "Looking for a host to log in to".to_string(),
            ),
            Mode::Indirect => (INDIRECT_QUERY, format!("Asking {host} for a list of hosts")),
        };
        self.progress(what);
        // STUB(x11): XDMCP authentication (XDM-AUTHENTICATION-1) and XDM-AUTHORIZATION-1 aren't offered: MIT-MAGIC-COOKIE-1 only (phase 4)
        let query = Packet::default().u8(0).finish(opcode);
        let willing = self.exchange(target, &query, |me, from, op, body| {
            let mut f = Fields(body);
            match op {
                WILLING => {
                    let (_auth, name, status) = (f.array8()?, f.array8()?, f.array8()?);
                    if me.verbose {
                        eprintln!(
                            "vintrdp: XDMCP: {from} ({}) is willing: {}",
                            text(name),
                            text(status)
                        );
                    }
                    Some(Ok((from, text(name))))
                }
                UNWILLING => {
                    let (name, status) = (f.array8()?, f.array8()?);
                    Some(Err(me.fail(format!(
                        "{} won't manage this display: {}",
                        or(text(name), &host),
                        text(status)
                    ))))
                }
                _ => None,
            }
        })?;
        let (manager, name) = match willing {
            Got::Answer(w) => w,
            Got::Timeout => {
                return Err(self.fail(match self.cfg.mode {
                    Mode::Broadcast => "No host answered the XDMCP broadcast".to_string(),
                    _ => format!("{host} didn't answer: is XDMCP enabled there (xdm, dtlogin, vuelogin), and UDP port {} open?", target.port()),
                }));
            }
        };
        let label = if name.is_empty() {
            host.clone()
        } else {
            name.clone()
        };

        // Request: where to connect, and the authorization we take.
        let address = match self.cfg.address.or_else(|| local_address(manager)) {
            Some(a) => a,
            None => {
                return Err(self.fail(format!(
                    "No address of this machine reaches {manager}; give one with ?address="
                )));
            }
        };
        let (family, bytes) = match address {
            IpAddr::V4(v4) => (INTERNET, v4.octets().to_vec()),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => (INTERNET, v4.octets().to_vec()),
                None => (INTERNET6, v6.octets().to_vec()),
            },
        };
        if self.verbose {
            eprintln!(
                "vintrdp: XDMCP: requesting display :{} at {address}",
                self.display
            );
        }
        let mut p = Packet::default();
        p.u16(self.display).u8(1).u16(family).u8(1).array8(&bytes);
        p.array8(b"").array8(b""); // no authentication
        p.u8(1).array8(COOKIE);
        p.array8(b""); // manufacturer display id
        let request = p.finish(REQUEST);
        let accept = self.exchange(manager, &request, |me, _, op, body| {
            let mut f = Fields(body);
            match op {
                ACCEPT => {
                    let session = f.u32()?;
                    let (_an, _ad, authz, data) =
                        (f.array8()?, f.array8()?, f.array8()?, f.array8()?);
                    Some(Ok((session, authz.to_vec(), data.to_vec())))
                }
                DECLINE => {
                    let status = f.array8()?;
                    Some(Err(me.fail(format!(
                        "{label} declined to manage this display: {}",
                        text(status)
                    ))))
                }
                _ => None,
            }
        })?;
        let Got::Answer((session, authz, data)) = accept else {
            return Err(self.fail(format!("{label} stopped answering (no Accept)")));
        };
        // The cookie every client must now give; with none, the server goes by address.
        let cookie = (authz == COOKIE && !data.is_empty()).then_some(data);
        if self.verbose {
            eprintln!(
                "vintrdp: XDMCP: session {session:#x}, {}",
                if cookie.is_some() {
                    "MIT-MAGIC-COOKIE-1"
                } else {
                    "no authorization"
                }
            );
        }
        // Without a cookie, the manager's host may connect (as X.org lets it).
        *self.auth.manager.lock().unwrap() = if cookie.is_none() {
            Some(manager.ip())
        } else {
            None
        };
        *self.auth.cookie.lock().unwrap() = cookie;

        // Manage, until the manager connects (or says why it can't).
        let mut p = Packet::default();
        p.u32(session).u16(self.display).array8(b"MIT-unspecified");
        let manage = p.finish(MANAGE);
        let mut opened = false;
        let mut buf = [0u8; 2048];
        'manage: for wait in RETRIES {
            self.send(manager, &manage)?;
            let until = Instant::now() + Duration::from_secs(wait);
            while Instant::now() < until {
                match self.poll_events()? {
                    Some(Event::Opened) => {
                        opened = true;
                        break 'manage;
                    }
                    Some(Event::Reset) => return Ok(Next::Again),
                    _ => {}
                }
                let Ok((n, _)) = self.socket.as_ref().unwrap().recv_from(&mut buf) else {
                    continue;
                };
                let Some((op, body)) = parse(&buf[..n]) else {
                    continue;
                };
                let mut f = Fields(body);
                match op {
                    REFUSE if f.u32() == Some(session) => {
                        // The session's gone (a manager restart): start again.
                        return Ok(Next::Again);
                    }
                    FAILED if f.u32() == Some(session) => {
                        let status = text(f.array8().unwrap_or_default());
                        return Err(self.fail(format!("{label} couldn't open this display: {status} (it connects to {address}:{}; if that's wrong, give ?address=)", 6000 + self.display as u32)));
                    }
                    _ => {}
                }
            }
        }
        if !opened {
            return Err(self.fail(format!("{label} never connected to this display (at {address}:{}; behind NAT, give ?address= an address it can reach)", 6000 + self.display as u32)));
        }
        self.say(Status::Managed {
            host: label.clone(),
        });

        // The session: KeepAlive now and then, until the server resets.
        let mut next = Instant::now() + KEEPALIVE_EVERY;
        loop {
            match self.poll_events()? {
                Some(Event::Reset) => return Ok(Next::Again),
                Some(Event::Left) => next = next.min(Instant::now() + SOON),
                _ => {}
            }
            if Instant::now() >= next {
                let mut p = Packet::default();
                p.u16(self.display).u32(session);
                let alive = self.exchange(manager, &p.finish(KEEPALIVE), |_, _, op, body| {
                    let mut f = Fields(body);
                    (op == ALIVE).then(|| Ok((f.u8().unwrap_or(0), f.u32().unwrap_or(0))))
                })?;
                let running = matches!(alive, Got::Answer((r, s)) if r != 0 && s == session);
                if !running {
                    if self.verbose {
                        eprintln!("vintrdp: XDMCP: the session is gone; resetting");
                    }
                    self.say(Status::Dead);
                    // The server resets, and says so.
                    loop {
                        if let Some(Event::Reset) = self.poll_events()? {
                            return Ok(Next::Again);
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                next = Instant::now() + KEEPALIVE_EVERY;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

enum Next {
    Again,
}

fn or(a: String, b: &str) -> String {
    if a.is_empty() { b.to_string() } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets() {
        let q = Packet::default().u8(0).finish(QUERY);
        assert_eq!(q, [0, 1, 0, 2, 0, 1, 0]);
        let mut p = Packet::default();
        p.array8(b"").array8(b"aix43").array8(b"AIX 4.3");
        let w = p.finish(WILLING);
        let (op, body) = parse(&w).unwrap();
        assert_eq!(op, WILLING);
        let mut f = Fields(body);
        assert_eq!(
            (f.array8(), f.array8(), f.array8(), f.u8()),
            (
                Some(&b""[..]),
                Some(&b"aix43"[..]),
                Some(&b"AIX 4.3"[..]),
                None
            )
        );
        assert!(parse(&[0, 2, 0, 5, 0, 0]).is_none());
        assert!(parse(&[0, 1, 0, 5, 0, 9, 1]).is_none());
    }
}
