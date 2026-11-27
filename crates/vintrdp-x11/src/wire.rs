//! The X protocol's encoding, in either byte order. Each client picks its
//! own order at connection time and the server must answer in it: the big-endian
//! workstations (SPARC, MIPS, PA-RISC, POWER) send big-endian requests to a
//! little-endian server and the other way around. x11rb-protocol only
//! speaks the host's order, so the server reads and writes through these.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// `l`: least significant byte first.
    Lsb,
    /// `B`: most significant byte first.
    Msb,
}

impl Order {
    /// The order from the first byte of the connection setup.
    pub fn from_byte(b: u8) -> Option<Order> {
        match b {
            b'l' => Some(Order::Lsb),
            b'B' => Some(Order::Msb),
            _ => None,
        }
    }

    pub fn u16(self, b: [u8; 2]) -> u16 {
        match self {
            Order::Lsb => u16::from_le_bytes(b),
            Order::Msb => u16::from_be_bytes(b),
        }
    }

    pub fn u32(self, b: [u8; 4]) -> u32 {
        match self {
            Order::Lsb => u32::from_le_bytes(b),
            Order::Msb => u32::from_be_bytes(b),
        }
    }

    pub fn put16(self, v: u16) -> [u8; 2] {
        match self {
            Order::Lsb => v.to_le_bytes(),
            Order::Msb => v.to_be_bytes(),
        }
    }

    pub fn put32(self, v: u32) -> [u8; 4] {
        match self {
            Order::Lsb => v.to_le_bytes(),
            Order::Msb => v.to_be_bytes(),
        }
    }
}

/// Padding that takes `n` bytes to a multiple of 4.
pub fn pad(n: usize) -> usize {
    (4 - n % 4) % 4
}

/// The request ended early.
#[derive(Debug)]
pub struct Short;

/// Reads a request (or the connection setup) in the client's order.
#[derive(Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    order: Order,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8], order: Order) -> Self {
        Reader { buf, pos: 0, order }
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Short> {
        let b = self.buf.get(self.pos..self.pos.checked_add(n).ok_or(Short)?).ok_or(Short)?;
        self.pos += n;
        Ok(b)
    }

    pub fn skip(&mut self, n: usize) -> Result<(), Short> {
        self.bytes(n).map(drop)
    }

    pub fn u8(&mut self) -> Result<u8, Short> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Short> {
        let b = self.bytes(2)?;
        Ok(self.order.u16([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, Short> {
        let b = self.bytes(4)?;
        Ok(self.order.u32([b[0], b[1], b[2], b[3]]))
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
}

/// Builds a reply, event or error in the client's order.
pub struct Writer {
    pub buf: Vec<u8>,
    order: Order,
}

impl Writer {
    pub fn new(order: Order) -> Self {
        Writer { buf: Vec::with_capacity(32), order }
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&self.order.put16(v));
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&self.order.put32(v));
        self
    }

    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.u16(v as u16)
    }

    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }

    /// Pad to a multiple of 4.
    pub fn align(&mut self) -> &mut Self {
        self.zeros(pad(self.buf.len()))
    }

    pub fn order(&self) -> Order {
        self.order
    }
}

/// A reply: its 8-byte header, then what the request returns.
pub struct Reply(Writer);

impl Reply {
    pub fn new(order: Order, seq: u16, data: u8) -> Self {
        let mut w = Writer::new(order);
        w.u8(1).u8(data).u16(seq).u32(0);
        Reply(w)
    }

    /// The whole reply: at least 32 bytes, its length filled in.
    pub fn finish(mut self) -> Vec<u8> {
        let w = &mut self.0;
        w.align();
        if w.buf.len() < 32 {
            w.zeros(32 - w.buf.len());
        }
        let words = ((w.buf.len() - 32) / 4) as u32;
        let len = w.order.put32(words);
        w.buf[4..8].copy_from_slice(&len);
        self.0.buf
    }
}

impl std::ops::Deref for Reply {
    type Target = Writer;
    fn deref(&self) -> &Writer {
        &self.0
    }
}

impl std::ops::DerefMut for Reply {
    fn deref_mut(&mut self) -> &mut Writer {
        &mut self.0
    }
}

/// An error packet.
pub fn error(order: Order, seq: u16, code: u8, value: u32, major: u8, minor: u16) -> Vec<u8> {
    let mut w = Writer::new(order);
    w.u8(0).u8(code).u16(seq).u32(value).u16(minor).u8(major).zeros(21);
    w.buf
}

/// Swap property data between the canonical (little-endian) form it's kept
/// in and a client's order.
pub fn convert(data: &[u8], format: u8, order: Order) -> Vec<u8> {
    let mut out = data.to_vec();
    if order == Order::Msb {
        match format {
            16 => out.chunks_exact_mut(2).for_each(|c| c.reverse()),
            32 => out.chunks_exact_mut(4).for_each(|c| c.reverse()),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders() {
        let mut r = Reader::new(&[1, 2, 3, 4, 5, 6], Order::Msb);
        assert_eq!(r.u16().unwrap(), 0x0102);
        assert_eq!(r.u32().unwrap(), 0x03040506);
        assert!(r.u8().is_err());
        let mut r = Reader::new(&[1, 2, 3, 4], Order::Lsb);
        assert_eq!(r.u32().unwrap(), 0x04030201);
        let mut w = Writer::new(Order::Msb);
        w.u16(0x0102).u32(0x03040506).align();
        assert_eq!(w.buf, [1, 2, 3, 4, 5, 6, 0, 0]);
    }

    #[test]
    fn replies() {
        let mut r = Reply::new(Order::Lsb, 7, 9);
        r.zeros(30);
        let b = r.finish();
        assert_eq!(b.len(), 40);
        assert_eq!(&b[..8], &[1, 9, 7, 0, 2, 0, 0, 0]);
        assert_eq!(Reply::new(Order::Msb, 1, 0).finish().len(), 32);
        assert_eq!(error(Order::Msb, 0x102, 17, 5, 60, 0)[..12], [0, 17, 1, 2, 0, 0, 0, 5, 0, 0, 60, 0]);
        assert_eq!(convert(&[1, 2, 3, 4], 16, Order::Msb), [2, 1, 4, 3]);
        assert_eq!(convert(&[1, 2, 3, 4], 32, Order::Lsb), [1, 2, 3, 4]);
    }
}
