//! Just enough ASN.1 DER for X.509 certificates, CredSSP (TSRequest) and
//! SPNEGO: walk tag-length-value elements, and build them. Lengths are
//! definite (DER), up to four length bytes.

// The common tags and encoders, whether or not a caller needs them yet.
#![allow(dead_code)]

use crate::error::{Error, Result};

pub const BOOLEAN: u8 = 0x01;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OID: u8 = 0x06;
pub const ENUMERATED: u8 = 0x0a;
pub const UTF8_STRING: u8 = 0x0c;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

/// Context-specific constructed tag `[n]`.
pub const fn context(n: u8) -> u8 {
    0xa0 | n
}

/// Application constructed tag `[APPLICATION n]` (n < 31).
pub const fn application(n: u8) -> u8 {
    0x60 | n
}

/// One element.
#[derive(Clone, Copy, Debug)]
pub struct Tlv<'a> {
    pub tag: u8,
    /// The contents, without tag and length.
    pub value: &'a [u8],
    /// The whole encoding, tag and length included.
    pub raw: &'a [u8],
}

impl<'a> Tlv<'a> {
    /// The elements inside a constructed value.
    pub fn children(&self) -> Der<'a> {
        Der::new(self.value)
    }

    /// An INTEGER's bytes without the sign-padding leading zero.
    pub fn unsigned(&self) -> &'a [u8] {
        let v = self.value;
        if v.len() > 1 && v[0] == 0 { &v[1..] } else { v }
    }

    /// A small non-negative INTEGER or ENUMERATED.
    pub fn uint(&self) -> Result<u32> {
        let v = self.unsigned();
        if v.len() > 4 {
            return Err(Error::protocol("DER integer too large"));
        }
        Ok(v.iter().fold(0u32, |n, &b| (n << 8) | b as u32))
    }

    /// A BIT STRING's bits, without the unused-bits count.
    pub fn bits(&self) -> Result<&'a [u8]> {
        self.value.split_first().map(|(_, rest)| rest).ok_or(Error::Truncated)
    }
}

/// A sequence of elements being read.
#[derive(Clone)]
pub struct Der<'a> {
    data: &'a [u8],
}

impl<'a> Der<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Der { data }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.data.first().copied()
    }

    /// The next element, whatever its tag.
    pub fn next_any(&mut self) -> Result<Tlv<'a>> {
        let d = self.data;
        let (&tag, rest) = d.split_first().ok_or(Error::Truncated)?;
        if tag & 0x1f == 0x1f {
            return Err(Error::protocol("DER: multi-byte tags are not supported"));
        }
        let (&first, rest) = rest.split_first().ok_or(Error::Truncated)?;
        let (len, rest) = if first < 0x80 {
            (first as usize, rest)
        } else {
            let n = (first & 0x7f) as usize;
            if n == 0 || n > 4 || rest.len() < n {
                return Err(Error::protocol("DER: bad length"));
            }
            (rest[..n].iter().fold(0usize, |l, &b| (l << 8) | b as usize), &rest[n..])
        };
        if rest.len() < len {
            return Err(Error::Truncated);
        }
        let header = d.len() - rest.len();
        let raw = &d[..header + len];
        self.data = &rest[len..];
        Ok(Tlv { tag, value: &rest[..len], raw })
    }

    /// The next element, which must have tag `tag`.
    pub fn next(&mut self, tag: u8) -> Result<Tlv<'a>> {
        let t = self.next_any()?;
        if t.tag != tag {
            return Err(Error::protocol(format!("DER: expected tag {tag:#04x}, found {:#04x}", t.tag)));
        }
        Ok(t)
    }

    /// The next element if it has tag `tag` (an OPTIONAL field).
    pub fn optional(&mut self, tag: u8) -> Result<Option<Tlv<'a>>> {
        if self.peek_tag() == Some(tag) { self.next(tag).map(Some) } else { Ok(None) }
    }
}

/// Encode a length.
fn put_len(out: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = (len as u32).to_be_bytes();
        let skip = bytes.iter().take_while(|&&b| b == 0).count();
        out.push(0x80 | (4 - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
}

/// One element: `tag`, then `value`'s length and bytes.
pub fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() + 6);
    out.push(tag);
    put_len(&mut out, value.len());
    out.extend_from_slice(value);
    out
}

/// A constructed element from already-encoded parts.
pub fn constructed(tag: u8, parts: &[&[u8]]) -> Vec<u8> {
    tlv(tag, &parts.concat())
}

pub fn sequence(parts: &[&[u8]]) -> Vec<u8> {
    constructed(SEQUENCE, parts)
}

/// `[n] EXPLICIT` around an encoded element.
pub fn explicit(n: u8, inner: &[u8]) -> Vec<u8> {
    tlv(context(n), inner)
}

pub fn octet_string(v: &[u8]) -> Vec<u8> {
    tlv(OCTET_STRING, v)
}

/// A non-negative INTEGER, minimally encoded.
pub fn uint(v: u32) -> Vec<u8> {
    let bytes = v.to_be_bytes();
    let skip = bytes.iter().take(3).take_while(|&&b| b == 0).count();
    let mut body = bytes[skip..].to_vec();
    if body[0] & 0x80 != 0 {
        body.insert(0, 0);
    }
    tlv(INTEGER, &body)
}

/// An OID from its dotted components.
pub fn oid(arcs: &[u32]) -> Vec<u8> {
    let mut body = vec![(arcs[0] * 40 + arcs[1]) as u8];
    for &a in &arcs[2..] {
        let mut chunk = Vec::new();
        let mut v = a;
        chunk.push((v & 0x7f) as u8);
        v >>= 7;
        while v > 0 {
            chunk.push(0x80 | (v & 0x7f) as u8);
            v >>= 7;
        }
        chunk.reverse();
        body.extend(chunk);
    }
    tlv(OID, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let long = vec![7u8; 300];
        let enc = sequence(&[&uint(0x80), &explicit(1, &octet_string(&long))]);
        let mut d = Der::new(&enc);
        let seq = d.next(SEQUENCE).unwrap();
        assert!(d.is_empty());
        let mut c = seq.children();
        assert_eq!(c.next(INTEGER).unwrap().uint().unwrap(), 0x80);
        let ctx = c.next(context(1)).unwrap();
        assert_eq!(ctx.children().next(OCTET_STRING).unwrap().value, &long[..]);
        assert!(c.optional(context(2)).unwrap().is_none());
    }

    #[test]
    fn oids() {
        // 1.2.840.113549.1.1.1, rsaEncryption
        assert_eq!(oid(&[1, 2, 840, 113549, 1, 1, 1]), [6, 9, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1]);
    }
}
