//! The connection to the server: TCP, optionally upgraded to TLS, read one
//! whole frame at a time. A frame is a TPKT (X.224) packet or a fast-path
//! PDU; which one is clear from its first byte, so framing needs no protocol
//! state, and a thread of its own can do the reading while the session
//! thread writes. Port of rdesktop's tcp.c (without its select loop).

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::session::LoopMsg;

/// TPKT version byte; anything else starts a fast-path PDU.
pub const TPKT_VERSION: u8 = 3;

/// Long enough for any server we know, short enough not to hang forever
/// on a dead address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
enum Link {
    Tcp(Arc<TcpStream>),
    #[cfg(feature = "tls")]
    Tls(Arc<dyn crate::tls::TlsStream>),
}

impl Link {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Link::Tcp(t) => (&**t).read(buf),
            #[cfg(feature = "tls")]
            Link::Tls(t) => t.read(buf),
        }
    }

    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        match self {
            Link::Tcp(t) => (&**t).write_all(buf),
            #[cfg(feature = "tls")]
            Link::Tls(t) => t.write_all(buf),
        }
    }
}

/// Reads whole frames.
pub struct FrameReader {
    link: Link,
    buf: Vec<u8>,
}

impl FrameReader {
    fn fill(&mut self, n: usize) -> io::Result<()> {
        while self.buf.len() < n {
            let mut chunk = [0u8; 8192];
            let want = (n - self.buf.len()).min(chunk.len());
            let got = self.link.read(&mut chunk[..want])?;
            if got == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the server closed the connection"));
            }
            self.buf.extend_from_slice(&chunk[..got]);
        }
        Ok(())
    }

    /// The next frame, header included.
    pub fn read_frame(&mut self) -> io::Result<Vec<u8>> {
        self.buf.clear();
        self.fill(2)?;
        let len = if self.buf[0] == TPKT_VERSION {
            self.fill(4)?;
            u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize
        } else if self.buf[1] & 0x80 != 0 {
            self.fill(3)?;
            (((self.buf[1] & 0x7f) as usize) << 8) | self.buf[2] as usize
        } else {
            self.buf[1] as usize
        };
        if len < self.buf.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad frame length {len}")));
        }
        self.fill(len)?;
        Ok(std::mem::take(&mut self.buf))
    }

    /// Read frames on a thread of its own, posting each to `tx` tagged with
    /// connection number `conn`, until the connection closes.
    pub fn spawn(mut self, conn: u32, tx: Sender<LoopMsg>) -> io::Result<()> {
        std::thread::Builder::new().name("rdp-reader".into()).spawn(move || {
            loop {
                match self.read_frame() {
                    Ok(data) => {
                        if tx.send(LoopMsg::Frame { conn, data }).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(LoopMsg::Closed { conn, error });
                        return;
                    }
                }
            }
        })?;
        Ok(())
    }
}

/// The connection's sending side, and its socket for shutting it down.
pub struct Transport {
    tcp: Arc<TcpStream>,
    link: Link,
    /// Until `take_reader`, frames are read here (the connection sequence).
    reader: Option<FrameReader>,
    pub peer: SocketAddr,
    pub local: SocketAddr,
}

impl Transport {
    /// Connect to `server` on `port`, trying each address it resolves to.
    pub fn connect(server: &str, port: u16) -> io::Result<Transport> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, format!("could not resolve {server}"));
        for addr in (server, port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                Ok(tcp) => return Transport::from_tcp(tcp),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn from_tcp(tcp: TcpStream) -> io::Result<Transport> {
        // Input events are tiny and latency matters more than packet count.
        tcp.set_nodelay(true)?;
        let peer = tcp.peer_addr()?;
        let local = tcp.local_addr()?;
        let tcp = Arc::new(tcp);
        let link = Link::Tcp(tcp.clone());
        Ok(Transport { tcp, reader: Some(FrameReader { link: link.clone(), buf: Vec::new() }), link, peer, local })
    }

    pub fn send(&self, data: &[u8]) -> io::Result<()> {
        self.link.write_all(data)
    }

    /// Read a frame on this thread (only before `take_reader`).
    pub fn read_frame(&mut self) -> io::Result<Vec<u8>> {
        match &mut self.reader {
            Some(r) => r.read_frame(),
            None => Err(io::Error::other("frames are read on the reader thread")),
        }
    }

    /// Hand reading over to someone else (the reader thread).
    pub fn take_reader(&mut self) -> Option<FrameReader> {
        self.reader.take()
    }

    /// Switch to TLS (PROTOCOL_SSL or PROTOCOL_HYBRID was negotiated).
    #[cfg(feature = "tls")]
    pub fn start_tls(&mut self, server: &str, shared: &crate::session::Shared) -> crate::error::Result<Arc<dyn crate::tls::TlsStream>> {
        let tcp = self.tcp.try_clone()?;
        let tls = crate::tls::connect(tcp, server, shared)?;
        self.link = Link::Tls(tls.clone());
        self.reader = Some(FrameReader { link: self.link.clone(), buf: Vec::new() });
        Ok(tls)
    }

    /// Close the connection; a reader blocked on it wakes with an error.
    pub fn shutdown(&self) {
        #[cfg(feature = "tls")]
        if let Link::Tls(t) = &self.link {
            t.shutdown();
        }
        let _ = self.tcp.shutdown(Shutdown::Both);
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn reads_tpkt_and_fastpath_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            // A TPKT, a short fast-path PDU and a long one, in awkward pieces.
            let mut long = vec![0x00, 0x81, 0x04];
            long.extend(std::iter::repeat_n(7u8, 0x104 - 3));
            let mut all = vec![3, 0, 0, 6, 0xaa, 0xbb, 0x00, 3, 9];
            all.extend(long);
            for piece in all.chunks(5) {
                s.write_all(piece).unwrap();
                s.flush().unwrap();
            }
        });
        let mut t = Transport::connect("127.0.0.1", port).unwrap();
        assert_eq!(t.read_frame().unwrap(), [3, 0, 0, 6, 0xaa, 0xbb]);
        assert_eq!(t.read_frame().unwrap(), [0, 3, 9]);
        assert_eq!(t.read_frame().unwrap().len(), 0x104);
        assert!(t.read_frame().is_err(), "closed");
        server.join().unwrap();
    }
}
