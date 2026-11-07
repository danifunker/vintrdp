//! RDP standard security (MS-RDPBCGR 5.3): the server's security data and
//! certificate, RSA for the client random, session keys, RC4 with key
//! updates, and MAC signatures. Port of rdesktop's secure.c (the crypto and
//! certificate parts; the GCC blocks are in `net::gcc`).

pub mod cert;
pub mod licence;

use md5::{Digest, Md5};
use num_bigint::BigUint;
use rc4::consts::{U8, U16};
use rc4::{KeyInit, Rc4, StreamCipher};
use sha1::Sha1;

use crate::bail;
use crate::error::Result;
use crate::stream::Reader;

/// SEC_* flags in the basic security header.
pub const SEC_EXCHANGE_PKT: u16 = 0x0001;
#[allow(dead_code)] // the full set of flags
pub const SEC_TRANSPORT_REQ: u16 = 0x0002;
pub const SEC_ENCRYPT: u16 = 0x0008;
#[allow(dead_code)] // the full set of flags
pub const SEC_RESET_SEQNO: u16 = 0x0010;
#[allow(dead_code)] // the full set of flags
pub const SEC_IGNORE_SEQNO: u16 = 0x0020;
pub const SEC_INFO_PKT: u16 = 0x0040;
pub const SEC_LICENSE_PKT: u16 = 0x0080;
#[allow(dead_code)] // the full set of flags
pub const SEC_LICENSE_ENCRYPT_CS: u16 = 0x0200;
pub const SEC_REDIRECTION_PKT: u16 = 0x0400;
#[allow(dead_code)] // the full set of flags
pub const SEC_SECURE_CHECKSUM: u16 = 0x0800;
#[allow(dead_code)] // the full set of flags
pub const SEC_AUTODETECT_REQ: u16 = 0x1000;
pub const SEC_HEARTBEAT: u16 = 0x2000;
#[allow(dead_code)] // the full set of flags
pub const SEC_FLAGSHI_VALID: u16 = 0x8000;

/// ENCRYPTION_METHOD_* (SC_SECURITY / CS_SECURITY).
pub const METHOD_NONE: u32 = 0;
pub const METHOD_40BIT: u32 = 0x01;
#[allow(dead_code)] // the full set of flags
pub const METHOD_128BIT: u32 = 0x02;
pub const METHOD_56BIT: u32 = 0x08;
pub const METHOD_FIPS: u32 = 0x10;

pub const RANDOM_SIZE: usize = 32;
/// The smallest modulus RDP uses (512 bits), and the largest rdesktop takes.
pub const MODULUS_SIZE: usize = 64;
pub const MAX_MODULUS_SIZE: usize = 256;
/// The zeros after the encrypted client random and in key blobs.
pub const PADDING_SIZE: usize = 8;
const EXPONENT_SIZE: usize = 4;

/// An RSA public key from a server certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    /// Big-endian, without leading zeros.
    pub modulus: Vec<u8>,
    /// Big-endian.
    pub exponent: Vec<u8>,
}

/// The server security data block (SC_SECURITY), parsed.
#[derive(Clone, Debug)]
pub struct ServerSecurity {
    /// ENCRYPTION_METHOD_* the server chose; METHOD_NONE for no encryption
    /// (then there's no random or key).
    pub method: u32,
    /// ENCRYPTION_LEVEL_* (0 none, 1 low, 2 client compatible, 3 high, 4 FIPS).
    #[allow(dead_code)] // parsed for completeness and debugging
    pub level: u32,
    pub server_random: Option<[u8; 32]>,
    pub public_key: Option<PublicKey>,
    /// The certificate was X.509 (Windows 2000 and later with a licence
    /// server), as opposed to the proprietary kind.
    #[allow(dead_code)]
    pub x509: bool,
}

/// Parse the body of an SC_SECURITY block (after its type and length).
/// `rdp_version` is the version the server announced in SC_CORE (4 or 5).
///
/// NT 4.0 (RDP 4) always sends the proprietary certificate, rdesktop's
/// "RDP4-style encryption"; later servers send either kind, so the
/// certificate's own flags decide, whatever the version.
pub fn parse_server_security(data: &[u8], rdp_version: u8) -> Result<ServerSecurity> {
    let _ = rdp_version;
    let mut r = Reader::new(data);
    let method = r.u32()?;
    let level = r.u32()?;
    if level == 0 || method == METHOD_NONE {
        // Nothing follows: no random, no certificate.
        return Ok(ServerSecurity { method: METHOD_NONE, level, server_random: None, public_key: None, x509: false });
    }
    if method & METHOD_FIPS != 0 {
        bail!("the server requires FIPS encryption, which isn't supported");
    }
    let random_len = r.u32()?;
    let cert_len = r.u32()?;
    if random_len as usize != RANDOM_SIZE {
        bail!("server random is {random_len} bytes, expected {RANDOM_SIZE}");
    }
    let server_random: [u8; 32] = r.array()?;
    let mut cert = r.sub(cert_len as usize)?;
    let (public_key, x509) = cert::parse(&mut cert)?;
    Ok(ServerSecurity { method, level, server_random: Some(server_random), public_key: Some(public_key), x509 })
}

/// The security exchange PDU's payload: `client_random` encrypted with the
/// server's key, as RDP lays it out (little-endian, followed by 8 zero bytes).
pub fn encrypt_client_random(client_random: &[u8; 32], key: &PublicKey) -> Vec<u8> {
    let n = BigUint::from_bytes_be(&key.modulus);
    let size = key.modulus.len();
    let mut out = if n.bits() == 0 {
        Vec::new()
    } else {
        let x = BigUint::from_bytes_le(client_random);
        let e = BigUint::from_bytes_be(&key.exponent);
        x.modpow(&e, &n).to_bytes_le()
    };
    out.resize(size + PADDING_SIZE, 0);
    out
}

/// RC4 with a 64-bit (40- and 56-bit methods) or 128-bit key: the `rc4`
/// crate fixes the key size in the type.
enum Rc4State {
    Short(Rc4<U8>),
    Long(Rc4<U16>),
}

impl Rc4State {
    fn new(key: &[u8]) -> Rc4State {
        match key.len() {
            8 => Rc4State::Short(Rc4::new(key.into())),
            _ => Rc4State::Long(Rc4::new(key[..16].into())),
        }
    }

    fn apply(&mut self, data: &mut [u8]) {
        match self {
            Rc4State::Short(c) => c.apply_keystream(data),
            Rc4State::Long(c) => c.apply_keystream(data),
        }
    }
}

/// RC4 with a fresh 16-byte key over `data` (licensing).
fn rc4_16(key: &[u8; 16], data: &mut [u8]) {
    Rc4State::new(key).apply(data);
}

/// One direction's key, its RC4 state and the count towards the next update.
struct Direction {
    key: [u8; 16],
    /// The first key of the session, which every update mixes in.
    update_key: [u8; 16],
    rc4: Rc4State,
    uses: u32,
}

/// The session's keys and RC4 state in both directions.
pub struct Crypto {
    method: u32,
    /// 8 for the 40- and 56-bit methods, else 16.
    key_len: usize,
    sign_key: [u8; 16],
    encrypt: Direction,
    decrypt: Direction,
}

/// Keys are updated after this many packets in a direction.
const UPDATE_EVERY: u32 = 4096;

impl Crypto {
    /// Derive the session keys (MS-RDPBCGR 5.3.5) for encryption `method`.
    pub fn new(client_random: &[u8; 32], server_random: &[u8; 32], method: u32) -> Crypto {
        let mut pre_master = [0u8; 48];
        pre_master[..24].copy_from_slice(&client_random[..24]);
        pre_master[24..].copy_from_slice(&server_random[..24]);
        let master = hash_48(&pre_master, client_random, server_random, b'A');
        let block = hash_48(&master, client_random, server_random, b'X');

        let mut sign_key = [0u8; 16];
        sign_key.copy_from_slice(&block[..16]);
        let mut decrypt_key = hash_16(&block[16..32], client_random, server_random);
        let mut encrypt_key = hash_16(&block[32..48], client_random, server_random);
        // rdesktop takes anything but 40-bit as 128-bit; 56-bit is per the spec.
        let key_len = if method == METHOD_40BIT || method == METHOD_56BIT { 8 } else { 16 };
        for key in [&mut sign_key, &mut decrypt_key, &mut encrypt_key] {
            reduce_key(key, method);
        }
        let direction = |key: [u8; 16]| Direction { key, update_key: key, rc4: Rc4State::new(&key[..key_len]), uses: 0 };
        Crypto { method, key_len, sign_key, encrypt: direction(encrypt_key), decrypt: direction(decrypt_key) }
    }

    /// Sign and encrypt `data` in place for sending; returns the 8-byte MAC
    /// that goes before it. Updates the key every 4096 packets.
    pub fn encrypt(&mut self, data: &mut [u8]) -> [u8; 8] {
        let mut mac = [0u8; 8];
        mac.copy_from_slice(&sign(&self.sign_key[..self.key_len], data)[..8]);
        self.crypt(true, data);
        mac
    }

    /// Decrypt `data` in place (the MAC isn't checked, as rdesktop doesn't).
    pub fn decrypt(&mut self, data: &mut [u8]) {
        if !data.is_empty() {
            self.crypt(false, data);
        }
    }

    fn crypt(&mut self, encrypt: bool, data: &mut [u8]) {
        let (method, key_len) = (self.method, self.key_len);
        let d = if encrypt { &mut self.encrypt } else { &mut self.decrypt };
        if d.uses == UPDATE_EVERY {
            update_key(&mut d.key, &d.update_key, key_len, method);
            d.rc4 = Rc4State::new(&d.key[..key_len]);
            d.uses = 0;
        }
        d.rc4.apply(data);
        d.uses += 1;
    }
}

/// rdesktop's sec_hash_sha1_16: SHA-1 over `input` (16 bytes) and `salt`
/// (16 bytes), for naming licence files.
pub fn hash_sha1_16(input: &[u8; 16], salt: &[u8; 16]) -> [u8; 20] {
    Sha1::new().chain_update(input).chain_update(salt).finalize().into()
}

/// The 48-byte transformation behind the master secret and key block
/// (SSL 3.0's, with SHA-1 and MD5 both): sec_hash_48.
fn hash_48(input: &[u8; 48], salt1: &[u8; 32], salt2: &[u8; 32], salt: u8) -> [u8; 48] {
    let mut out = [0u8; 48];
    for i in 0..3 {
        let pad = [salt + i as u8; 3];
        let sha: [u8; 20] = Sha1::new().chain_update(&pad[..=i]).chain_update(input).chain_update(salt1).chain_update(salt2).finalize().into();
        let md5: [u8; 16] = Md5::new().chain_update(input).chain_update(sha).finalize().into();
        out[i * 16..(i + 1) * 16].copy_from_slice(&md5);
    }
    out
}

/// sec_hash_16: MD5 over 16 bytes of key material and both randoms.
fn hash_16(input: &[u8], salt1: &[u8; 32], salt2: &[u8; 32]) -> [u8; 16] {
    Md5::new().chain_update(input).chain_update(salt1).chain_update(salt2).finalize().into()
}

const PAD_54: [u8; 40] = [54; 40];
const PAD_92: [u8; 48] = [92; 48];

/// The MAC (MS-RDPBCGR 5.3.6.1, sec_sign): MD5 over the key, pad and a
/// SHA-1 over the key, pad, length and data. Callers keep 8 or 16 bytes.
fn sign(key: &[u8], data: &[u8]) -> [u8; 16] {
    let len = (data.len() as u32).to_le_bytes();
    let sha: [u8; 20] = Sha1::new().chain_update(key).chain_update(PAD_54).chain_update(len).chain_update(data).finalize().into();
    Md5::new().chain_update(key).chain_update(PAD_92).chain_update(sha).finalize().into()
}

/// Lower a key's entropy to what the method allows (sec_make_40bit, and the
/// 56-bit salt rdesktop never needed since it doesn't offer 56-bit).
fn reduce_key(key: &mut [u8; 16], method: u32) {
    match method {
        METHOD_40BIT => key[..3].copy_from_slice(&[0xd1, 0x26, 0x9e]),
        METHOD_56BIT => key[0] = 0xd1,
        _ => {}
    }
}

/// The key update every 4096 packets (sec_update, MS-RDPBCGR 5.3.7).
fn update_key(key: &mut [u8; 16], update_key: &[u8; 16], key_len: usize, method: u32) {
    let sha: [u8; 20] = Sha1::new()
        .chain_update(&update_key[..key_len])
        .chain_update(PAD_54)
        .chain_update(&key[..key_len])
        .finalize()
        .into();
    *key = Md5::new().chain_update(&update_key[..key_len]).chain_update(PAD_92).chain_update(sha).finalize().into();
    let mut rc4 = Rc4State::new(&key[..key_len]);
    rc4.apply(&mut key[..key_len]);
    reduce_key(key, method);
}

#[cfg(test)]
mod tests;
