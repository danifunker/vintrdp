//! XDM-AUTHENTICATION-1 and XDM-AUTHORIZATION-1: XDMCP's DES-based
//! authentication of the display and the manager to each other, and the
//! session key X clients then prove they hold, as X.org's `os/xdmauth.c`
//! and libXdmcp's `Wrap.c` have them.
//!
//! The display and the manager share a 56-bit key (X.org's `-cookie`, a
//! line in xdm's `xdm-keys`). The display sends a random 64-bit number,
//! rho, wrapped with it in its Request; the manager answers rho + 1 wrapped
//! the same way in Accept, along with a new session key, wrapped too. A
//! client then sends, wrapped with the session key, rho, its address, and
//! the time; the server checks each, and refuses one it has seen before.

use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use des::cipher::generic_array::GenericArray;
use des::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use des::Des;

pub const AUTHENTICATION: &[u8] = b"XDM-AUTHENTICATION-1";
pub const AUTHORIZATION: &[u8] = b"XDM-AUTHORIZATION-1";

/// An XDMCP key: 8 bytes, of which the last 7 count (56 bits).
pub type Key = [u8; 8];

/// A key as X.org's `-cookie` and xdm's `xdm-keys` take it: `0x` and up to
/// 16 hex digits, or up to 7 characters (after a zero byte).
pub fn parse_key(s: &str) -> Result<Key, String> {
    let mut key = [0u8; 8];
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        let hex = &hex[..hex.len().min(16)];
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("bad key {s}: 0x and hex digits, or up to 7 characters"));
        }
        // As X.org's HexToBinary: digits fill from the first byte.
        for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
            let digits = std::str::from_utf8(pair).unwrap();
            let v = u8::from_str_radix(digits, 16).unwrap();
            key[i] = if pair.len() == 1 { v << 4 } else { v };
        }
    } else {
        if s.is_empty() {
            return Err("an empty key".into());
        }
        let b = s.as_bytes();
        let n = b.len().min(7);
        key[1..1 + n].copy_from_slice(&b[..n]);
    }
    Ok(key)
}

/// The DES key for an XDMCP key: its last 56 bits, 7 to a byte.
fn des_key(k: &Key) -> Des {
    let mut out = [0u8; 8];
    for i in 0..7 {
        // _XdmcpWrapperToOddParity: bit i of the output's 7-bit groups
        // runs on from the input's; the parity bit DES ignores.
        let c = ((((k[i] as u16) << (7 - i)) | ((k[i + 1] as u16) >> (i + 1))) & 0x7f) as u8;
        out[i] = c << 1;
    }
    out[7] = (k[7] & 0x7f) << 1;
    Des::new_from_slice(&out).expect("an 8-byte key")
}

/// XdmcpWrap: DES in CBC mode with a zero start, the last block padded
/// with zeros.
pub fn wrap(input: &[u8], key: &Key) -> Vec<u8> {
    let des = des_key(key);
    let mut out = Vec::with_capacity(input.len().div_ceil(8) * 8);
    let mut prev = [0u8; 8];
    for chunk in input.chunks(8) {
        let mut block = [0u8; 8];
        block[..chunk.len()].copy_from_slice(chunk);
        for i in 0..8 {
            block[i] ^= prev[i];
        }
        let mut b = GenericArray::from(block);
        des.encrypt_block(&mut b);
        prev.copy_from_slice(&b);
        out.extend_from_slice(&b);
    }
    out
}

/// XdmcpUnwrap; None if the input isn't whole blocks.
pub fn unwrap(input: &[u8], key: &Key) -> Option<Vec<u8>> {
    if input.len() % 8 != 0 {
        return None;
    }
    let des = des_key(key);
    let mut out = Vec::with_capacity(input.len());
    let mut prev = [0u8; 8];
    for chunk in input.chunks(8) {
        let mut b = GenericArray::clone_from_slice(chunk);
        des.decrypt_block(&mut b);
        for i in 0..8 {
            out.push(b[i] ^ prev[i]);
        }
        prev.copy_from_slice(chunk);
    }
    Some(out)
}

/// XdmcpIncrementKey: the key as a 64-bit big-endian number, plus one.
pub fn increment(k: &mut Key) {
    *k = u64::from_be_bytes(*k).wrapping_add(1).to_be_bytes();
}

/// A new random rho.
pub fn random_key() -> Key {
    let mut k = [0u8; 8];
    k.copy_from_slice(&crate::new_cookie()[..8]);
    k
}

/// The display's half of XDM-AUTHENTICATION-1.
#[derive(Clone, Debug)]
pub struct Authentication {
    pub key: Key,
    pub rho: Key,
}

impl Authentication {
    pub fn new(key: Key) -> Self {
        Authentication { key, rho: random_key() }
    }

    /// The Request's authentication data: rho, wrapped.
    pub fn request(&self) -> Vec<u8> {
        wrap(&self.rho, &self.key)
    }

    /// Whether the Accept's authentication data is rho + 1, wrapped.
    pub fn check_accept(&self, data: &[u8]) -> bool {
        let Some(plain) = unwrap(data, &self.key) else { return false };
        let mut want = self.rho;
        increment(&mut want);
        plain.len() == 8 && plain == want
    }

    /// The session's XDM-AUTHORIZATION-1 from the Accept's authorization
    /// data (the session key, wrapped); 16 bytes as R5's xdm sent works too.
    pub fn authorization(&self, data: &[u8]) -> Option<Authorization> {
        let plain = unwrap(data, &self.key)?;
        if plain.len() != 8 && plain.len() != 16 {
            return None;
        }
        let mut key = [0u8; 8];
        key.copy_from_slice(&plain[..8]);
        key[0] = 0;
        Some(Authorization::new(key, self.rho))
    }
}

/// How far a client's clock may be from the server's (X.org's).
const WINDOW: u32 = 20 * 60;

/// The session's XDM-AUTHORIZATION-1: what clients must prove.
#[derive(Debug)]
pub struct Authorization {
    key: Key,
    rho: Key,
    /// The first client's clock less this one's, as X.org learns it.
    offset: Option<i64>,
    /// Clients seen: address and time, to refuse a replay.
    seen: Vec<([u8; 6], u32)>,
}

impl Authorization {
    pub fn new(key: Key, rho: Key) -> Self {
        Authorization { key, rho, offset: None, seen: Vec::new() }
    }

    /// Check a client's authorization data. `peer` is its address (None on
    /// the Unix socket, where X.org doesn't check it). `manager` is the
    /// display manager's address: a client seen from there may give another
    /// (its own behind NAT, where the manager's host is the gateway), which
    /// X.org would refuse.
    pub fn check(&mut self, data: &[u8], peer: Option<IpAddr>, manager: Option<IpAddr>) -> Result<(), &'static str> {
        self.check_at(data, peer, manager, now())
    }

    fn check_at(&mut self, data: &[u8], peer: Option<IpAddr>, also: Option<IpAddr>, now: u32) -> Result<(), &'static str> {
        if data.len() != 24 {
            return Err("Bad XDM authorization key length");
        }
        let plain = unwrap(data, &self.key).ok_or("Bad XDM authorization key length")?;
        if plain[..8] != self.rho {
            return Err("Invalid XDM-AUTHORIZATION-1 key (failed key comparison)");
        }
        if plain[18..].iter().any(|&b| b != 0) {
            return Err("Invalid XDM-AUTHORIZATION-1 key (failed NULL check)");
        }
        let v4 = |ip: IpAddr| match ip {
            IpAddr::V4(v4) => Some(v4.octets()),
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(|v| v.octets()),
        };
        if let Some(peer) = peer {
            let given = &plain[8..12];
            let from_manager = also.is_some_and(|m| v4(m).is_some() && v4(m) == v4(peer));
            if v4(peer).is_none_or(|a| a != given) && !from_manager {
                return Err("Invalid XDM-AUTHORIZATION-1 key (failed address comparison)");
            }
        }
        let utc = u32::from_be_bytes([plain[14], plain[15], plain[16], plain[17]]);
        let offset = *self.offset.get_or_insert(utc as i64 - now as i64);
        let now = (now as i64 + offset) as u32;
        if now.wrapping_sub(utc) as i32 > WINDOW as i32 || utc.wrapping_sub(now) as i32 > WINDOW as i32 {
            return Err("Excessive XDM-AUTHORIZATION-1 time offset");
        }
        // Forget clients too old to replay; refuse one seen already.
        self.seen.retain(|&(_, t)| (now.wrapping_sub(t) as i32) <= (WINDOW as i32) + 5 * 60);
        let mut client = [0u8; 6];
        client.copy_from_slice(&plain[8..14]);
        if self.seen.iter().any(|&(c, t)| c == client && t == utc) {
            return Err("XDM authorization key matches an existing client!");
        }
        self.seen.push((client, utc));
        Ok(())
    }
}

fn now() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs() as u32
}

/// A client's XDM-AUTHORIZATION-1 data, as libxcb makes it from an
/// Xauthority entry (rho, then the key): for tests.
pub fn client_data(rho: &Key, key: &Key, addr: [u8; 4], port: u16, time: u32) -> Vec<u8> {
    let mut plain = Vec::with_capacity(24);
    plain.extend_from_slice(rho);
    plain.extend_from_slice(&addr);
    plain.extend_from_slice(&port.to_be_bytes());
    plain.extend_from_slice(&time.to_be_bytes());
    plain.resize(24, 0);
    wrap(&plain, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(parse_key("vintrdp"), Ok(*b"\0vintrdp"));
        assert_eq!(parse_key("toolongkey"), Ok(*b"\0toolong"));
        assert_eq!(parse_key("0x0102030405060708"), Ok([1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(parse_key("0x01020304050607080910"), Ok([1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(parse_key("0x123"), Ok([0x12, 0x30, 0, 0, 0, 0, 0, 0]));
        assert!(parse_key("0xzz").is_err());
        assert!(parse_key("").is_err());
        let mut k = [0, 0, 0, 0, 0, 0, 0xff, 0xff];
        increment(&mut k);
        assert_eq!(k, [0, 0, 0, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn wrap_matches_libxdmcp() {
        // From a C program calling libXdmcp 1.1.5's XdmcpWrap (m900):
        // key "\0vintrdp", input 0..24.
        let key = parse_key("vintrdp").unwrap();
        let input: Vec<u8> = (0..24).collect();
        let w = wrap(&input, &key);
        assert_eq!(hex(&w), LIBXDMCP_WRAP);
        assert_eq!(unwrap(&w, &key).unwrap(), input);
        // A short last block is padded.
        assert_eq!(wrap(&input[..5], &key).len(), 8);
        assert!(unwrap(&w[..7], &key).is_none());
        // And a hex key, one block: "0x00112233445566".
        let k2 = parse_key("0x00112233445566").unwrap();
        assert_eq!(hex(&wrap(&input[..8], &k2)), "46d17160d1fc0029");
    }

    /// See wrap_matches_libxdmcp.
    const LIBXDMCP_WRAP: &str = "5021172b1377b00c2ac790f4447b5e9d6c3679d5a1bb3b83";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn the_exchange_and_a_client() {
        let shared = parse_key("0x00112233445566").unwrap();
        let display = Authentication::new(shared);
        // The manager unwraps rho, answers rho + 1 and a session key.
        let mut rho: Key = unwrap(&display.request(), &shared).unwrap().try_into().unwrap();
        assert_eq!(rho, display.rho);
        increment(&mut rho);
        assert!(display.check_accept(&wrap(&rho, &shared)));
        assert!(!display.check_accept(&wrap(&display.rho, &shared)));
        let session: Key = [0, 9, 8, 7, 6, 5, 4, 3];
        let mut authz = display.authorization(&wrap(&session, &shared)).unwrap();

        let ip: IpAddr = "192.168.99.153".parse().unwrap();
        let t = 1_800_000_000;
        let c = client_data(&display.rho, &session, [192, 168, 99, 153], 40000, t);
        assert_eq!(authz.check_at(&c, Some(ip), None, t + 5), Ok(()));
        // The same again is a replay.
        assert_eq!(authz.check_at(&c, Some(ip), None, t + 6), Err("XDM authorization key matches an existing client!"));
        // From elsewhere.
        let c2 = client_data(&display.rho, &session, [192, 168, 99, 153], 40001, t);
        assert!(authz.check_at(&c2, Some("10.0.0.1".parse().unwrap()), None, t).unwrap_err().contains("address"));
        // Behind NAT: its own address isn't the one it's seen from, but it
        // comes from the manager's host.
        let nat = client_data(&display.rho, &session, [10, 0, 2, 15], 40002, t);
        assert_eq!(authz.check_at(&nat, Some(ip), Some(ip), t), Ok(()));
        // The Unix socket: no address to check.
        let local = client_data(&display.rho, &session, [0xff, 0xff, 0xff, 0xfe], 1, t);
        assert_eq!(authz.check_at(&local, None, None, t), Ok(()));
        // Too old, by the first client's clock.
        let old = client_data(&display.rho, &session, [192, 168, 99, 153], 40003, t - 21 * 60);
        assert!(authz.check_at(&old, Some(ip), None, t).unwrap_err().contains("time offset"));
        // The wrong key, the wrong rho.
        let wrong = client_data(&display.rho, &[0, 1, 1, 1, 1, 1, 1, 1], [192, 168, 99, 153], 40004, t);
        assert!(authz.check_at(&wrong, Some(ip), None, t).is_err());
        assert_eq!(authz.check_at(&c[..16], Some(ip), None, t), Err("Bad XDM authorization key length"));
    }
}
