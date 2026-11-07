//! MPPC bulk decompression (RFC 2118): RDP 4's 8 KB history and RDP 5's
//! 64 KB one. Port of rdesktop's mppc.c.
//!
//! The C walks a 32-bit shift register; this reads the same codes with a
//! plain bit reader. Two deliberate differences: in 8K mode the history is
//! 8 KB (rdesktop let it run on into a 64 KB buffer while masking offsets to
//! 8 KB), and a copy may fill the history to its last byte.

use crate::error::{Error, Result};

/// PACKET_COMPRESSED and friends, from the compression flags of a share data
/// header or a fast-path update.
pub const PACKET_COMPR_TYPE_8K: u8 = 0x00;
pub const PACKET_COMPR_TYPE_64K: u8 = 0x01;
pub const PACKET_COMPRESSED: u8 = 0x20;
pub const PACKET_AT_FRONT: u8 = 0x40;
pub const PACKET_FLUSHED: u8 = 0x80;

const COMPR_TYPE_MASK: u8 = 0x0f;
const HISTORY_8K: usize = 8192;
const HISTORY_64K: usize = 65536;

/// The decompressor's history, kept for the whole connection.
pub struct Mppc {
    hist: Vec<u8>,
    /// Where the next PDU's output starts.
    offset: usize,
}

impl Mppc {
    pub fn new() -> Self {
        Mppc { hist: vec![0; HISTORY_64K], offset: 0 }
    }

    /// Decompress one PDU. `flags` is the full compression-flags byte
    /// (type in the low nibble, PACKET_* above). Returns the decompressed
    /// bytes, which stay valid until the next call. Data that isn't
    /// PACKET_COMPRESSED is returned as is (after honouring FLUSHED).
    pub fn expand<'a>(&'a mut self, data: &'a [u8], flags: u8) -> Result<&'a [u8]> {
        if flags & PACKET_FLUSHED != 0 {
            self.hist.fill(0);
            self.offset = 0;
        }
        if flags & PACKET_COMPRESSED == 0 {
            return Ok(data);
        }
        if flags & PACKET_AT_FRONT != 0 {
            self.offset = 0;
        }
        let size = match flags & COMPR_TYPE_MASK {
            PACKET_COMPR_TYPE_8K => HISTORY_8K,
            PACKET_COMPR_TYPE_64K => HISTORY_64K,
            t => return Err(Error::protocol(format!("MPPC: unsupported compression type {t}"))),
        };
        let start = self.offset;
        let end = decode(data, &mut self.hist[..size], start)?;
        self.offset = end;
        Ok(&self.hist[start..end])
    }
}

impl Default for Mppc {
    fn default() -> Self {
        Self::new()
    }
}

fn corrupt() -> Error {
    Error::protocol("MPPC: corrupt compressed data")
}

/// MSB-first bits of a PDU.
struct Bits<'a> {
    data: &'a [u8],
    byte: usize,
    /// Bits already used from `data[byte]`, 0 to 7.
    bit: u32,
}

impl Bits<'_> {
    fn bit(&mut self) -> Result<u32> {
        let b = *self.data.get(self.byte).ok_or_else(corrupt)?;
        let v = u32::from(b >> (7 - self.bit)) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.byte += 1;
        }
        Ok(v)
    }

    fn bits(&mut self, n: u32) -> Result<u32> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }

    fn at_end(&self) -> bool {
        self.byte >= self.data.len()
    }

    /// Fewer than eight bits left, all zero: the final byte's padding.
    fn only_padding(&self) -> bool {
        self.byte + 1 == self.data.len() && self.bit > 0 && self.data[self.byte] << self.bit == 0
    }
}

/// Decode `data` into `hist` from `start`, returning where the output ends.
/// `hist` is the whole history for the mode, so its length (a power of two)
/// is the ring offsets wrap around.
fn decode(data: &[u8], hist: &mut [u8], start: usize) -> Result<usize> {
    let big = hist.len() == HISTORY_64K;
    let mask = hist.len() - 1;
    let mut out = start;
    let mut bits = Bits { data, byte: 0, bit: 0 };

    loop {
        if bits.at_end() || bits.only_padding() {
            break;
        }
        // Literals: 0xxxxxxx for bytes below 0x80, 10xxxxxxx above.
        let lit = if bits.bit()? == 0 {
            Some(bits.bits(7)?)
        } else if bits.bit()? == 0 {
            Some(bits.bits(7)? | 0x80)
        } else {
            None
        };
        if let Some(lit) = lit {
            let slot = hist.get_mut(out).ok_or_else(corrupt)?;
            *slot = lit as u8;
            out += 1;
            continue;
        }

        // A copy tuple: the offset back into the history...
        let offset = if big {
            if bits.bit()? == 0 {
                bits.bits(16)? + 2368 // 110
            } else if bits.bit()? == 0 {
                bits.bits(11)? + 320 // 1110
            } else if bits.bit()? == 0 {
                bits.bits(8)? + 64 // 11110
            } else {
                bits.bits(6)? // 11111
            }
        } else if bits.bit()? == 0 {
            bits.bits(13)? + 320 // 110
        } else if bits.bit()? == 0 {
            bits.bits(8)? + 64 // 1110
        } else {
            bits.bits(6)? // 1111
        };

        // ...then the length: 0 is 3; n ones and a zero are followed by n+1
        // bits, which with an implied top bit give 4 to 8191 (or 65535).
        let max_ones = if big { 14 } else { 11 };
        let mut ones = 0;
        while bits.bit()? == 1 {
            ones += 1;
            if ones > max_ones {
                return Err(corrupt());
            }
        }
        let len = if ones == 0 { 3 } else { (1usize << (ones + 1)) | bits.bits(ones + 1)? as usize };

        if len > hist.len() - out {
            return Err(Error::protocol("MPPC: copy runs past the end of the history"));
        }
        // The source may overlap the output and wraps round the ring.
        let mut src = out.wrapping_sub(offset as usize) & mask;
        for _ in 0..len {
            hist[out] = hist[src];
            out += 1;
            src = (src + 1) & mask;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an MPPC bit stream, MSB first, zero padded.
    #[derive(Default)]
    struct Enc {
        bits: Vec<bool>,
    }

    impl Enc {
        fn put(&mut self, v: u32, n: u32) -> &mut Self {
            for i in (0..n).rev() {
                self.bits.push(v >> i & 1 != 0);
            }
            self
        }

        fn lit(&mut self, b: u8) -> &mut Self {
            if b < 0x80 { self.put(u32::from(b), 8) } else { self.put(0b10, 2).put(u32::from(b & 0x7f), 7) }
        }

        fn copy(&mut self, big: bool, offset: u32, len: u32) -> &mut Self {
            self.put(0b11, 2);
            match (big, offset) {
                (true, 0..=63) => self.put(0b111, 3).put(offset, 6),
                (true, 64..=319) => self.put(0b110, 3).put(offset - 64, 8),
                (true, 320..=2367) => self.put(0b10, 2).put(offset - 320, 11),
                (true, _) => self.put(0, 1).put(offset - 2368, 16),
                (false, 0..=63) => self.put(0b11, 2).put(offset, 6),
                (false, 64..=319) => self.put(0b10, 2).put(offset - 64, 8),
                (false, _) => self.put(0, 1).put(offset - 320, 13),
            };
            if len == 3 {
                return self.put(0, 1);
            }
            let n = 31 - len.leading_zeros(); // bits after the implied top bit
            self.put((1 << n) - 1, n - 1).put(0, 1).put(len & ((1 << n) - 1), n)
        }

        fn bytes(&self) -> Vec<u8> {
            self.bits
                .chunks(8)
                .map(|c| c.iter().enumerate().fold(0u8, |a, (i, &b)| a | (u8::from(b) << (7 - i))))
                .collect()
        }
    }

    const C8K: u8 = PACKET_COMPRESSED | PACKET_COMPR_TYPE_8K;
    const C64K: u8 = PACKET_COMPRESSED | PACKET_COMPR_TYPE_64K;

    #[test]
    fn literals_and_overlapping_copy() {
        let data = Enc::default().lit(b'a').lit(0xc3).lit(b'c').copy(false, 3, 7).bytes();
        let mut m = Mppc::new();
        assert_eq!(m.expand(&data, C8K).unwrap(), b"a\xc3ca\xc3ca\xc3ca");
        // The history carries over: copy from the previous PDU.
        let data = Enc::default().copy(false, 10, 3).lit(b'z').bytes();
        assert_eq!(m.expand(&data, C8K).unwrap(), b"a\xc3cz");
    }

    #[test]
    fn offset_and_length_forms_8k() {
        let mut e = Enc::default();
        for i in 0..400u32 {
            e.lit((i % 251) as u8);
        }
        e.copy(false, 100, 4).copy(false, 400, 16).copy(false, 8, 300);
        let mut m = Mppc::new();
        let out = m.expand(&e.bytes(), C8K).unwrap().to_vec();
        let mut want: Vec<u8> = (0..400u32).map(|i| (i % 251) as u8).collect();
        for (off, len) in [(100, 4), (400, 16), (8, 300)] {
            for _ in 0..len {
                want.push(want[want.len() - off]);
            }
        }
        assert_eq!(out, want);
    }

    #[test]
    fn offset_and_length_forms_64k() {
        let mut e = Enc::default();
        for i in 0..3000u32 {
            e.lit((i * 7 % 256) as u8);
        }
        e.copy(true, 5, 3).copy(true, 200, 40).copy(true, 1000, 1000).copy(true, 2900, 5000);
        let mut m = Mppc::new();
        let out = m.expand(&e.bytes(), C64K).unwrap().to_vec();
        let mut want: Vec<u8> = (0..3000u32).map(|i| (i * 7 % 256) as u8).collect();
        for (off, len) in [(5, 3), (200, 40), (1000, 1000), (2900, 5000)] {
            for _ in 0..len {
                want.push(want[want.len() - off]);
            }
        }
        assert_eq!(out, want);
    }

    #[test]
    fn history_wraps_after_at_front() {
        let mut m = Mppc::new();
        // Fill 60000 bytes of 'x', then "yzw" at 60001..60004.
        let data = Enc::default().lit(b'x').copy(true, 1, 60000).lit(b'y').lit(b'z').lit(b'w').bytes();
        assert_eq!(m.expand(&data, C64K | PACKET_AT_FRONT).unwrap().len(), 60004);
        // Back at the front, an offset reaching behind 0 wraps to the end.
        let data = Enc::default().copy(true, 65536 - 60001, 3).bytes();
        assert_eq!(m.expand(&data, C64K | PACKET_AT_FRONT).unwrap(), b"yzw");
        // The history can be filled exactly to its end, but no further.
        let data = Enc::default().copy(true, 1, 65533).bytes();
        assert_eq!(m.expand(&data, C64K).unwrap().len(), 65533);
        let data = Enc::default().lit(b'a').bytes();
        assert!(m.expand(&data, C64K).is_err());
    }

    #[test]
    fn flush_clears_history() {
        let mut m = Mppc::new();
        let data = Enc::default().lit(b'a').lit(b'b').lit(b'c').bytes();
        m.expand(&data, C8K).unwrap();
        let data = Enc::default().lit(b'd').copy(false, 3, 3).bytes();
        assert_eq!(m.expand(&data, C8K | PACKET_FLUSHED).unwrap(), b"d\0\0d");
        // FLUSHED without COMPRESSED resets too, and passes the data through.
        assert_eq!(m.expand(b"raw", PACKET_FLUSHED).unwrap(), b"raw");
        let data = Enc::default().copy(false, 1, 3).bytes();
        assert_eq!(m.expand(&data, C8K).unwrap(), b"\0\0\0");
    }

    #[test]
    fn uncompressed_passes_through() {
        let mut m = Mppc::new();
        assert_eq!(m.expand(b"hello", 0).unwrap(), b"hello");
        assert_eq!(m.expand(b"", C8K).unwrap(), b"");
    }

    #[test]
    fn rejects_corrupt_data() {
        let mut m = Mppc::new();
        // A literal cut short.
        assert!(m.expand(&[0b1000_0000], C8K).is_err());
        // Padding that isn't zero.
        let data = Enc::default().lit(0xc3).put(1, 7).bytes();
        assert!(Mppc::new().expand(&data, C8K).is_err());
        let data = Enc::default().lit(0xc3).bytes();
        assert_eq!(Mppc::new().expand(&data, C8K).unwrap(), b"\xc3");
        // A copy tuple without its length.
        let mut e = Enc::default();
        e.lit(b'a').put(0b110, 3).put(5, 13);
        assert!(Mppc::new().expand(&e.bytes(), C8K).is_err());
        // Too many length bits.
        let mut e = Enc::default();
        e.lit(b'a').put(0b1111, 4).put(1, 6).put(0xfff, 12);
        assert!(Mppc::new().expand(&e.bytes(), C8K).is_err());
        // A copy past the end of the 8 KB history.
        let data = Enc::default().lit(b'a').lit(b'b').copy(false, 1, 8191).bytes();
        assert!(Mppc::new().expand(&data, C8K).is_err());
        let data = Enc::default().lit(b'a').copy(false, 1, 8191).bytes();
        assert_eq!(Mppc::new().expand(&data, C8K).unwrap().len(), 8192);
        // RDP 6 bulk compression isn't MPPC.
        assert!(Mppc::new().expand(&[0], PACKET_COMPRESSED | 2).is_err());
    }

    #[test]
    fn garbage_never_panics() {
        let mut seed = 0x8765_4321u32;
        let mut m = Mppc::new();
        for _ in 0..20000 {
            let len = (lcg(&mut seed) % 64) as usize;
            let data: Vec<u8> = (0..len).map(|_| lcg(&mut seed) as u8).collect();
            let flags = (lcg(&mut seed) as u8 & 0xe0) | (lcg(&mut seed) as u8 % 3);
            let _ = m.expand(&data, flags);
        }
    }

    fn lcg(s: &mut u32) -> u32 {
        *s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
        *s >> 8
    }
}
