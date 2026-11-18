//! RSA-AES, RealVNC's logon (security types 5 RA2, 6 RA2ne, 129 RA2_256 and
//! 130 RA2ne_256), as TigerVNC implements it compatibly:
//!
//! 1. The server sends its RSA key: its size in bits (u32), then the modulus
//!    and the exponent, each as many bytes as the size. The client checks the
//!    key (the [`Host`] decides; its fingerprint is the first 8 bytes of the
//!    SHA-1 of those fields, as RealVNC shows it).
//! 2. The client sends a key of its own of the same size, made for the
//!    session, and 16 random bytes (32 for the _256 types) encrypted with the
//!    server's key (PKCS #1 v1.5); the server sends its random bytes encrypted
//!    with the client's.
//! 3. Each direction gets an AES key: SHA-1 (SHA-256) of the two random
//!    values, the sender's last. From here messages are AES-EAX: a 2-byte
//!    length, which is also the associated data, the ciphertext and a 16-byte
//!    tag, the nonce a 128-bit little-endian counter.
//! 4. Each side sends a hash of both keys (its own first), the server a
//!    subtype (1: user name and password, 2: password), and the client the
//!    credentials, each a length byte and the text.
//!
//! RA2 and RA2_256 go on encrypting the whole session; the "ne" types encrypt
//! only the logon.

use std::io;
use std::sync::{Arc, Mutex};

use aes::{Aes128, Aes256};
use eax::Eax;
use eax::aead::generic_array::GenericArray;
use eax::aead::{AeadInPlace, KeyInit};
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::traits::PublicKeyParts;
use rsa::{BigUint, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha1::{Digest, Sha1};
use sha2::Sha256;

use crate::auth::{RA2, RA2_256};
use crate::stream::{Link, Reader, Src, Transport, Writer, bad};
use crate::{Host, Settings};

const MIN_KEY_BITS: u32 = 1024;
const MAX_KEY_BITS: u32 = 8192;
const USER_AND_PASSWORD: u8 = 1;
const PASSWORD: u8 = 2;
/// The most a message carries, as TigerVNC sends them.
const MAX_MESSAGE: usize = 8192;

pub(crate) fn logon(settings: &Settings, host: &dyn Host, w: &Writer, r: &mut Reader, kind: u8) -> io::Result<()> {
    let wide = matches!(kind, crate::auth::RA2_256 | crate::auth::RA2NE_256);
    let whole_session = matches!(kind, RA2 | RA2_256);
    let random_len = if wide { 32 } else { 16 };

    // The server's key.
    let bits = r.u32()?;
    if !(MIN_KEY_BITS..=MAX_KEY_BITS).contains(&bits) {
        return Err(bad(format!("RSA-AES: a server key of {bits} bits")));
    }
    let size = bits.div_ceil(8) as usize;
    let (n, e) = (r.bytes(size)?, r.bytes(size)?);
    let server_fields = key_fields(bits, &n, &e);
    let digest = Sha1::digest(&server_fields);
    let fingerprint: Vec<String> = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    if !host.trust_key(&fingerprint.join("-"), &server_fields) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the server's key was not trusted"));
    }
    let server_key = RsaPublicKey::new_with_max_size(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e), MAX_KEY_BITS as usize)
        .map_err(|e| bad(format!("RSA-AES: the server's key: {e}")))?;

    // Ours, and our random bytes for it.
    let mut rng = OsRandom;
    let saved = host.saved_client_key(bits).and_then(|der| RsaPrivateKey::from_pkcs1_der(&der).ok()).filter(|k| k.size() == size);
    let client_key = match saved {
        Some(key) => key,
        None => {
            host.status("Making an encryption key");
            let started = std::time::Instant::now();
            let key = RsaPrivateKey::new(&mut rng, bits as usize).map_err(|e| bad(format!("RSA-AES: making a key: {e}")))?;
            let took = started.elapsed();
            if settings.verbose {
                eprintln!("vintrdp: RSA-AES: made a {bits}-bit key in {:.1} s", took.as_secs_f64());
            }
            if let Ok(der) = key.to_pkcs1_der() {
                host.client_key_made(bits, der.as_bytes(), took);
            }
            key
        }
    };
    let client_fields = key_fields(bits, &fixed(&client_key.n().to_bytes_be(), size), &fixed(&client_key.e().to_bytes_be(), size));
    let mut client_random = vec![0u8; random_len];
    crate::random(&mut client_random);
    let sealed = server_key.encrypt(&mut rng, Pkcs1v15Encrypt, &client_random).map_err(|e| bad(format!("RSA-AES: {e}")))?;
    let mut m = client_fields.clone();
    m.extend((sealed.len() as u16).to_be_bytes());
    m.extend(&sealed);
    w.send(&m)?;

    // The server's random bytes.
    let len = r.u16()? as usize;
    if len != size {
        return Err(bad("RSA-AES: the server's random data doesn't fit our key"));
    }
    let server_random = client_key.decrypt(Pkcs1v15Encrypt, &r.bytes(len)?).map_err(|_| bad("RSA-AES: could not decrypt the server's random data"))?;
    if server_random.len() != random_len {
        return Err(bad("RSA-AES: the server's random data has the wrong size"));
    }

    let digest = |a: &[u8], b: &[u8]| -> Vec<u8> {
        if wide { Sha256::new().chain_update(a).chain_update(b).finalize().to_vec() } else { Sha1::new().chain_update(a).chain_update(b).finalize().to_vec() }
    };
    let receiving = Channel::new(&digest(&client_random, &server_random), wide);
    let sending = Channel::new(&digest(&server_random, &client_random), wide);
    let ours = digest(&client_fields, &server_fields);
    let theirs = digest(&server_fields, &client_fields);

    let credentials = |subtype: u8| -> io::Result<Vec<u8>> {
        let field = |text: &str| -> io::Result<Vec<u8>> {
            let b = text.as_bytes();
            if b.len() > 255 {
                return Err(bad("RSA-AES: a user name or password over 255 bytes"));
            }
            Ok([&[b.len() as u8][..], b].concat())
        };
        let user = match subtype {
            USER_AND_PASSWORD => field(&settings.username)?,
            PASSWORD => vec![0],
            other => return Err(bad(format!("RSA-AES: unknown subtype {other}"))),
        };
        Ok([user, field(&settings.password)?].concat())
    };

    if whole_session {
        // Everything from here on is encrypted, starting with what the reader
        // may already hold.
        let leftover = r.take_buffered();
        let link = w.link();
        let layer = Arc::new(Stream { inner: link, receiving: Mutex::new(Receiving { channel: receiving, raw: leftover, plain: Vec::new(), pos: 0 }), sending: Mutex::new(sending) });
        *r = Reader::new(w.wrap(layer));
        w.send(&ours)?;
        if r.bytes(ours.len())? != theirs {
            return Err(bad("RSA-AES: the server's hash doesn't match"));
        }
        let subtype = r.u8()?;
        w.send(&credentials(subtype)?)
    } else {
        // Only the logon is encrypted: messages read straight off the reader.
        let (mut receiving, mut sending) = (receiving, sending);
        let mut pending = Vec::new();
        let mut take = |r: &mut Reader, n: usize| -> io::Result<Vec<u8>> {
            while pending.len() < n {
                pending.extend(receiving.open(r)?);
            }
            Ok(pending.drain(..n).collect())
        };
        w.send(&sending.seal(&ours))?;
        if take(r, ours.len())? != theirs {
            return Err(bad("RSA-AES: the server's hash doesn't match"));
        }
        let subtype = take(r, 1)?[0];
        w.send(&sending.seal(&credentials(subtype)?))
    }
}

/// A key as RSA-AES sends it: its size in bits, then n and e, each `size` bytes.
fn key_fields(bits: u32, n: &[u8], e: &[u8]) -> Vec<u8> {
    [&bits.to_be_bytes()[..], n, e].concat()
}

/// A big-endian number in exactly `size` bytes.
fn fixed(b: &[u8], size: usize) -> Vec<u8> {
    let mut out = vec![0u8; size.saturating_sub(b.len())];
    out.extend_from_slice(&b[b.len().saturating_sub(size)..]);
    out
}

enum Cipher {
    Aes128(Box<Eax<Aes128>>),
    Aes256(Box<Eax<Aes256>>),
}

/// One direction of the encrypted channel.
pub(crate) struct Channel {
    cipher: Cipher,
    counter: [u8; 16],
}

impl Channel {
    /// AES-128 with the first 16 bytes of `key`, or AES-256 with 32.
    fn new(key: &[u8], wide: bool) -> Channel {
        let cipher = if wide {
            Cipher::Aes256(Box::new(Eax::new(GenericArray::from_slice(&key[..32]))))
        } else {
            Cipher::Aes128(Box::new(Eax::new(GenericArray::from_slice(&key[..16]))))
        };
        Channel { cipher, counter: [0; 16] }
    }

    fn next_nonce(&mut self) -> [u8; 16] {
        let nonce = self.counter;
        for b in &mut self.counter {
            *b = b.wrapping_add(1);
            if *b != 0 {
                break;
            }
        }
        nonce
    }

    /// `data` as messages.
    fn seal(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 18);
        for chunk in data.chunks(MAX_MESSAGE) {
            let header = (chunk.len() as u16).to_be_bytes();
            let mut body = chunk.to_vec();
            let nonce = self.next_nonce();
            let nonce = GenericArray::from_slice(&nonce);
            let tag = match &self.cipher {
                Cipher::Aes128(c) => c.encrypt_in_place_detached(nonce, &header, &mut body),
                Cipher::Aes256(c) => c.encrypt_in_place_detached(nonce, &header, &mut body),
            }
            .expect("EAX encryption");
            out.extend(header);
            out.extend(body);
            out.extend(tag);
        }
        out
    }

    /// The next message from `src`, decrypted and checked.
    fn open(&mut self, src: &mut impl Src) -> io::Result<Vec<u8>> {
        let len = src.u16()?;
        let mut body = src.bytes(len as usize)?;
        let tag = src.bytes(16)?;
        let nonce = self.next_nonce();
        let (nonce, tag) = (GenericArray::from_slice(&nonce), GenericArray::from_slice(&tag));
        let header = len.to_be_bytes();
        match &self.cipher {
            Cipher::Aes128(c) => c.decrypt_in_place_detached(nonce, &header, &mut body, tag),
            Cipher::Aes256(c) => c.decrypt_in_place_detached(nonce, &header, &mut body, tag),
        }
        .map_err(|_| bad("RSA-AES: a message failed its check"))?;
        Ok(body)
    }
}

struct Receiving {
    channel: Channel,
    /// Bytes from the network not yet decrypted.
    raw: Vec<u8>,
    plain: Vec<u8>,
    pos: usize,
}

/// The whole session encrypted (RA2, RA2_256).
struct Stream {
    inner: Link,
    receiving: Mutex<Receiving>,
    sending: Mutex<Channel>,
}

impl Transport for Stream {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut rx = self.receiving.lock().unwrap();
        while rx.pos == rx.plain.len() {
            // A whole message: its length, the body and the tag.
            let need = if rx.raw.len() >= 2 { 2 + u16::from_be_bytes([rx.raw[0], rx.raw[1]]) as usize + 16 } else { 2 };
            if rx.raw.len() < need {
                let mut chunk = vec![0u8; 16 * 1024];
                let n = self.inner.read(&mut chunk)?;
                if n == 0 {
                    return Ok(0);
                }
                rx.raw.extend_from_slice(&chunk[..n]);
                continue;
            }
            if need == 2 {
                continue;
            }
            let frame: Vec<u8> = rx.raw.drain(..need).collect();
            let plain = rx.channel.open(&mut crate::stream::Cursor { data: &frame })?;
            rx.plain = plain;
            rx.pos = 0;
        }
        let n = buf.len().min(rx.plain.len() - rx.pos);
        buf[..n].copy_from_slice(&rx.plain[rx.pos..rx.pos + n]);
        rx.pos += n;
        Ok(n)
    }

    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        let mut tx = self.sending.lock().unwrap();
        let sealed = tx.seal(buf);
        self.inner.write_all(&sealed)
    }
}

/// The system's random source, for RSA.
struct OsRandom;

impl rand_core::RngCore for OsRandom {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        crate::random(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        crate::random(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        crate::random(dest);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        crate::random(dest);
        Ok(())
    }
}

impl rand_core::CryptoRng for OsRandom {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_round_trip_and_counter() {
        for wide in [false, true] {
            let key = [7u8; 32];
            let (mut a, mut b) = (Channel::new(&key, wide), Channel::new(&key, wide));
            let big = vec![0x5a; MAX_MESSAGE + 10];
            let sealed = [a.seal(b"hello"), a.seal(&big)].concat();
            // Two messages for the big one: 8192 bytes, then 10.
            assert_eq!(sealed.len(), (2 + 5 + 16) + (2 + MAX_MESSAGE + 16) + (2 + 10 + 16));
            let mut src = crate::stream::Cursor { data: &sealed };
            assert_eq!(b.open(&mut src).unwrap(), b"hello");
            assert_eq!(b.open(&mut src).unwrap().len(), MAX_MESSAGE);
            assert_eq!(b.open(&mut src).unwrap().len(), 10);
            // Out of order (a nonce skipped) fails its check.
            let one = a.seal(b"x");
            let _ = a.seal(b"y");
            let two = a.seal(b"z");
            let mut c = Channel::new(&key, wide);
            for _ in 0..3 {
                c.next_nonce();
            }
            assert!(c.open(&mut crate::stream::Cursor { data: &two }).is_err());
            let _ = one;
        }
        let mut c = Channel::new(&[0; 16], false);
        c.counter = [0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(c.next_nonce()[0], 0xff);
        assert_eq!(&c.counter[..3], &[0, 0, 1]);
    }

    /// How long a 2048-bit key takes here: `cargo test -p vintrdp-vnc --release -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn key_time() {
        let started = std::time::Instant::now();
        for _ in 0..3 {
            RsaPrivateKey::new(&mut OsRandom, 2048).unwrap();
        }
        eprintln!("2048-bit key: {:.2} s each", started.elapsed().as_secs_f64() / 3.0);
    }

    #[test]
    fn fixed_width() {
        assert_eq!(fixed(&[1, 2], 4), [0, 0, 1, 2]);
        assert_eq!(fixed(&[9, 1, 2], 2), [1, 2]);
    }
}
