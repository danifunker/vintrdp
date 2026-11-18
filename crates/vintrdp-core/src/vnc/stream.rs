//! The connection to a VNC server: TCP, upgraded to TLS by VeNCrypt. RFB has no
//! framing, so the reader is a buffered stream the decoders read field by field;
//! a thread of its own reads while the session thread writes input.

use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// No field is bigger than this; more is a broken or hostile server.
const MAX_FIELD: usize = 64 << 20;

#[derive(Clone)]
pub(super) enum Link {
    Tcp(Arc<TcpStream>),
    #[cfg(feature = "tls")]
    Tls(Arc<dyn crate::tls::TlsStream>),
}

impl Link {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        use std::io::Read;
        match self {
            Link::Tcp(t) => (&**t).read(buf),
            #[cfg(feature = "tls")]
            Link::Tls(t) => t.read(buf),
        }
    }

    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        use std::io::Write;
        match self {
            Link::Tcp(t) => (&**t).write_all(buf),
            #[cfg(feature = "tls")]
            Link::Tls(t) => t.write_all(buf),
        }
    }
}

pub(super) fn connect(server: &str, port: u16) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("could not resolve {server}"));
    for addr in (server, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(tcp) => {
                tcp.set_nodelay(true)?;
                return Ok(tcp);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Bytes from the server, read as RFB's big-endian fields.
pub(super) struct Reader {
    link: Link,
    buf: Vec<u8>,
    pos: usize,
    end: usize,
}

impl Reader {
    pub fn new(link: Link) -> Reader {
        Reader { link, buf: vec![0; 64 * 1024], pos: 0, end: 0 }
    }

    fn refill(&mut self) -> io::Result<()> {
        self.pos = 0;
        self.end = 0;
        let n = self.link.read(&mut self.buf)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the server closed the connection"));
        }
        self.end = n;
        Ok(())
    }

    /// Nothing read and not yet used: true before switching to TLS.
    #[cfg_attr(not(feature = "tls"), allow(dead_code))]
    pub fn is_drained(&self) -> bool {
        self.pos == self.end
    }

    pub fn read_into(&mut self, out: &mut [u8]) -> io::Result<()> {
        let mut done = 0;
        while done < out.len() {
            if self.pos == self.end {
                self.refill()?;
            }
            let n = (self.end - self.pos).min(out.len() - done);
            out[done..done + n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            done += n;
        }
        Ok(())
    }
}

/// What the decoders read from: the network, or a buffer inflated from it.
pub(super) trait Src {
    fn read_into(&mut self, out: &mut [u8]) -> io::Result<()>;

    fn bytes(&mut self, n: usize) -> io::Result<Vec<u8>> {
        if n > MAX_FIELD {
            return Err(bad(format!("a field of {n} bytes")));
        }
        let mut v = vec![0; n];
        self.read_into(&mut v)?;
        Ok(v)
    }
    fn u8(&mut self) -> io::Result<u8> {
        let mut b = [0; 1];
        self.read_into(&mut b)?;
        Ok(b[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        let mut b = [0; 2];
        self.read_into(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let mut b = [0; 4];
        self.read_into(&mut b)?;
        Ok(u32::from_be_bytes(b))
    }
    fn i32(&mut self) -> io::Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn skip(&mut self, n: usize) -> io::Result<()> {
        self.bytes(n).map(drop)
    }
    /// A u32 length and that many bytes, as text (Latin-1 when not UTF-8).
    fn string(&mut self) -> io::Result<String> {
        let n = self.u32()? as usize;
        let b = self.bytes(n)?;
        Ok(match String::from_utf8(b) {
            Ok(s) => s,
            Err(e) => e.into_bytes().iter().map(|&c| c as char).collect(),
        })
    }
}

impl Src for Reader {
    fn read_into(&mut self, out: &mut [u8]) -> io::Result<()> {
        Reader::read_into(self, out)
    }
}

/// An in-memory source: what came out of a zlib stream.
pub(super) struct Cursor<'a> {
    pub data: &'a [u8],
}

impl Src for Cursor<'_> {
    fn read_into(&mut self, out: &mut [u8]) -> io::Result<()> {
        if out.len() > self.data.len() {
            return Err(bad("compressed data ended early"));
        }
        let (head, tail) = self.data.split_at(out.len());
        out.copy_from_slice(head);
        self.data = tail;
        Ok(())
    }
}

/// The sending side, shared by the reader thread (update requests) and the
/// session thread (input); a message goes out whole.
pub(super) struct Writer {
    tcp: Arc<TcpStream>,
    link: Mutex<Link>,
}

impl Writer {
    pub fn new(tcp: Arc<TcpStream>) -> Writer {
        Writer { link: Mutex::new(Link::Tcp(tcp.clone())), tcp }
    }

    pub fn link(&self) -> Link {
        self.link.lock().unwrap().clone()
    }

    pub fn send(&self, data: &[u8]) -> io::Result<()> {
        self.link.lock().unwrap().write_all(data)
    }

    /// Switch to TLS on the same socket (VeNCrypt), returning the new link:
    /// with the server's certificate checked, or `anonymous`.
    #[cfg(feature = "tls")]
    pub fn start_tls(&self, server: &str, shared: &crate::session::Shared, anonymous: bool) -> crate::error::Result<Link> {
        let tcp = self.tcp.try_clone()?;
        let tls = if anonymous { crate::tls::connect_anonymous(tcp, server, shared)? } else { crate::tls::connect(tcp, server, shared)? };
        let link = Link::Tls(tls);
        *self.link.lock().unwrap() = link.clone();
        Ok(link)
    }

    /// Close the connection; a read in progress ends with an error.
    pub fn shutdown(&self) {
        #[cfg(feature = "tls")]
        if let Link::Tls(t) = &*self.link.lock().unwrap() {
            t.shutdown();
        }
        let _ = self.tcp.shutdown(Shutdown::Both);
    }
}

pub(super) fn bad(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
