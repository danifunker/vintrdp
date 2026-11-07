//! RDP standard security (MS-RDPBCGR 5.3): the server's security data and
//! certificate, RSA for the client random, session keys, RC4 with key
//! updates, and MAC signatures. Port of rdesktop's secure.c (the crypto and
//! certificate parts; the GCC blocks are in `net::gcc`).

pub mod cert;
pub mod licence;

use crate::error::Result;

/// SEC_* flags in the basic security header.
pub const SEC_EXCHANGE_PKT: u16 = 0x0001;
pub const SEC_TRANSPORT_REQ: u16 = 0x0002;
pub const SEC_ENCRYPT: u16 = 0x0008;
pub const SEC_RESET_SEQNO: u16 = 0x0010;
pub const SEC_IGNORE_SEQNO: u16 = 0x0020;
pub const SEC_INFO_PKT: u16 = 0x0040;
pub const SEC_LICENSE_PKT: u16 = 0x0080;
pub const SEC_LICENSE_ENCRYPT_CS: u16 = 0x0200;
pub const SEC_REDIRECTION_PKT: u16 = 0x0400;
pub const SEC_SECURE_CHECKSUM: u16 = 0x0800;
pub const SEC_AUTODETECT_REQ: u16 = 0x1000;
pub const SEC_HEARTBEAT: u16 = 0x2000;
pub const SEC_FLAGSHI_VALID: u16 = 0x8000;

/// ENCRYPTION_METHOD_* (SC_SECURITY / CS_SECURITY).
pub const METHOD_NONE: u32 = 0;
pub const METHOD_40BIT: u32 = 0x01;
pub const METHOD_128BIT: u32 = 0x02;
pub const METHOD_56BIT: u32 = 0x08;
pub const METHOD_FIPS: u32 = 0x10;

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
    pub level: u32,
    pub server_random: Option<[u8; 32]>,
    pub public_key: Option<PublicKey>,
    /// The certificate was X.509 (Windows 2000 and later with a licence
    /// server), as opposed to the proprietary kind.
    pub x509: bool,
}

/// Parse the body of an SC_SECURITY block (after its type and length).
/// `rdp_version` is the version the server announced in SC_CORE (4 or 5).
pub fn parse_server_security(data: &[u8], rdp_version: u8) -> Result<ServerSecurity> {
    let _ = (data, rdp_version);
    todo!("sec::parse_server_security")
}

/// The security exchange PDU's payload: `client_random` encrypted with the
/// server's key, as RDP lays it out (little-endian, followed by 8 zero bytes).
pub fn encrypt_client_random(client_random: &[u8; 32], key: &PublicKey) -> Vec<u8> {
    let _ = (client_random, key);
    todo!("sec::encrypt_client_random")
}

/// The session's keys and RC4 state in both directions.
pub struct Crypto {
    _private: (),
}

impl Crypto {
    /// Derive the session keys (MS-RDPBCGR 5.3.5) for encryption `method`.
    pub fn new(client_random: &[u8; 32], server_random: &[u8; 32], method: u32) -> Crypto {
        let _ = (client_random, server_random, method);
        todo!("sec::Crypto::new")
    }

    /// Sign and encrypt `data` in place for sending; returns the 8-byte MAC
    /// that goes before it. Updates the key every 4096 packets.
    pub fn encrypt(&mut self, data: &mut [u8]) -> [u8; 8] {
        let _ = data;
        todo!("sec::Crypto::encrypt")
    }

    /// Decrypt `data` in place (the MAC isn't checked, as rdesktop doesn't).
    pub fn decrypt(&mut self, data: &mut [u8]) {
        let _ = data;
        todo!("sec::Crypto::decrypt")
    }
}

/// rdesktop's sec_hash_sha1_16: SHA-1 over `input` (16 bytes) and `salt`
/// (16 bytes), for naming licence files.
pub fn hash_sha1_16(input: &[u8; 16], salt: &[u8; 16]) -> [u8; 20] {
    let _ = (input, salt);
    todo!("sec::hash_sha1_16")
}
