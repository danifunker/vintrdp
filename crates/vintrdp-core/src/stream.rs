//! Reading and writing PDUs: a bounds-checked cursor over received bytes, and
//! a growable buffer for building them. Little-endian unless the name says
//! `_be`, as in RDP itself.

// Every integer width in both byte orders, whether or not a caller needs it yet.
#![allow(dead_code)]

use crate::error::{Error, Result};

/// A cursor over a received PDU. Every read checks the length first, so a
/// short or hostile PDU gives [`Error::Truncated`], never a panic.
#[derive(Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

macro_rules! read_int {
    ($($name:ident, $name_be:ident: $ty:ty;)*) => {
        $(
            pub fn $name(&mut self) -> Result<$ty> {
                Ok(<$ty>::from_le_bytes(self.array()?))
            }
            pub fn $name_be(&mut self) -> Result<$ty> {
                Ok(<$ty>::from_be_bytes(self.array()?))
            }
        )*
    };
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Move to an absolute position (at most the end).
    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            return Err(Error::Truncated);
        }
        self.pos = pos;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// The whole underlying buffer, read or not.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// What's left, without consuming it.
    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    /// Consume everything left.
    pub fn take_rest(&mut self) -> &'a [u8] {
        let r = self.rest();
        self.pos = self.data.len();
        r
    }

    pub fn check(&self, n: usize) -> Result<()> {
        if self.remaining() < n { Err(Error::Truncated) } else { Ok(()) }
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.check(n)?;
        self.pos += n;
        Ok(())
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.check(n)?;
        let b = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(b)
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.bytes(N)?);
        Ok(a)
    }

    /// The next `n` bytes as a reader of their own.
    pub fn sub(&mut self, n: usize) -> Result<Reader<'a>> {
        Ok(Reader::new(self.bytes(n)?))
    }

    pub fn peek_u8(&self) -> Result<u8> {
        self.data.get(self.pos).copied().ok_or(Error::Truncated)
    }

    pub fn u8(&mut self) -> Result<u8> {
        let b = self.peek_u8()?;
        self.pos += 1;
        Ok(b)
    }

    pub fn i8(&mut self) -> Result<i8> {
        Ok(self.u8()? as i8)
    }

    read_int! {
        u16, u16_be: u16;
        u32, u32_be: u32;
        u64, u64_be: u64;
        i16, i16_be: i16;
        i32, i32_be: i32;
    }

    /// `n` bytes of UTF-16LE, up to the first NUL.
    pub fn utf16(&mut self, n: usize) -> Result<String> {
        Ok(crate::text::from_utf16le(self.bytes(n)?))
    }
}

/// A PDU being built.
#[derive(Clone, Default, Debug)]
pub struct Writer {
    buf: Vec<u8>,
}

macro_rules! write_int {
    ($($name:ident, $name_be:ident, $set:ident, $set_be:ident: $ty:ty;)*) => {
        $(
            pub fn $name(&mut self, v: $ty) -> &mut Self {
                self.buf.extend_from_slice(&v.to_le_bytes());
                self
            }
            pub fn $name_be(&mut self, v: $ty) -> &mut Self {
                self.buf.extend_from_slice(&v.to_be_bytes());
                self
            }
            /// Overwrite at `pos`, for a length known only at the end.
            pub fn $set(&mut self, pos: usize, v: $ty) {
                let b = v.to_le_bytes();
                self.buf[pos..pos + b.len()].copy_from_slice(&b);
            }
            pub fn $set_be(&mut self, pos: usize, v: $ty) {
                let b = v.to_be_bytes();
                self.buf[pos..pos + b.len()].copy_from_slice(&b);
            }
        )*
    };
}

impl Writer {
    pub fn new() -> Self {
        Writer { buf: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        Writer { buf: Vec::with_capacity(n) }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buf
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    write_int! {
        u16, u16_be, set_u16, set_u16_be: u16;
        u32, u32_be, set_u32, set_u32_be: u32;
        u64, u64_be, set_u64, set_u64_be: u64;
        i16, i16_be, set_i16, set_i16_be: i16;
        i32, i32_be, set_i32, set_i32_be: i32;
    }

    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }

    /// `s` as UTF-16LE, without a terminator.
    pub fn utf16(&mut self, s: &str) -> &mut Self {
        for u in s.encode_utf16() {
            self.u16(u);
        }
        self
    }

    /// `s` as UTF-16LE in exactly `n` bytes: truncated, or padded with NULs.
    /// At least one NUL is kept at the end when `n` is even and nonzero.
    pub fn utf16_fixed(&mut self, s: &str, n: usize) -> &mut Self {
        let start = self.buf.len();
        for u in s.encode_utf16().take(n.saturating_sub(2) / 2) {
            self.u16(u);
        }
        let used = self.buf.len() - start;
        self.zeros(n - used)
    }
}

impl From<Writer> for Vec<u8> {
    fn from(w: Writer) -> Vec<u8> {
        w.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_bounds_checked() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert_eq!(r.u16().unwrap(), 0x0201);
        assert!(matches!(r.u16(), Err(Error::Truncated)));
        assert_eq!(r.u8().unwrap(), 3);
        assert!(r.is_empty());
    }

    #[test]
    fn writes_and_patches() {
        let mut w = Writer::new();
        w.u16_be(0).u32(0x04030201).utf16_fixed("ab", 6);
        w.set_u16_be(0, w.len() as u16);
        assert_eq!(w.as_slice(), [0, 12, 1, 2, 3, 4, b'a', 0, b'b', 0, 0, 0]);
    }
}
