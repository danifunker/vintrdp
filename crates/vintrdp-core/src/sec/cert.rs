//! Server certificates: the proprietary kind (NT 4.0 TSE, Windows 2000
//! without a licence server) and X.509 chains (licensing, Windows 2000 and
//! later), parsed in Rust with `crate::der`. Port of rdesktop's secure.c
//! (sec_parse_crypt_info and its helpers) and of what asn.c and ssl.c did
//! with libtasn1 and GnuTLS.

use super::{EXPONENT_SIZE, MAX_MODULUS_SIZE, MODULUS_SIZE, PADDING_SIZE, PublicKey};
use crate::bail;
use crate::der::{self, Der};
use crate::error::Result;
use crate::stream::Reader;

/// dwVersion's low bit: a proprietary certificate. Anything else is taken
/// as an X.509 chain (2, with the top bit set when the certificate is
/// temporary), as rdesktop does.
const CERT_PROPRIETARY: u32 = 1;

const TAG_PUBKEY: u16 = 0x0006;
/// "RSA1", the magic of a proprietary public key blob.
const RSA_MAGIC: u32 = 0x3141_5352;

/// The server certificate: its public key, and whether it was X.509.
pub fn parse(r: &mut Reader) -> Result<(PublicKey, bool)> {
    let version = r.u32()?;
    if version & CERT_PROPRIETARY != 0 {
        Ok((parse_proprietary(r)?, false))
    } else {
        Ok((parse_x509_chain(r)?, true))
    }
}

/// A proprietary certificate after dwVersion: the signature and key
/// algorithms, then tagged blobs, of which the public key is the one needed.
/// The signature with Terminal Services' well-known key isn't checked, as
/// rdesktop doesn't.
fn parse_proprietary(r: &mut Reader) -> Result<PublicKey> {
    r.skip(8)?; // dwSigAlgId, dwKeyAlgId
    let mut key = None;
    while !r.is_empty() {
        let tag = r.u16()?;
        let len = r.u16()? as usize;
        let mut blob = r.sub(len)?;
        // The signature (0x0008), and anything unknown, is passed over.
        if tag == TAG_PUBKEY {
            key = Some(parse_public_key(&mut blob)?);
        }
    }
    match key {
        Some(k) => Ok(k),
        None => bail!("the server did not send an RSA public key"),
    }
}

/// A proprietary RSA public key blob (sec_parse_public_key): little-endian
/// exponent and modulus, the modulus followed by 8 bytes of padding.
fn parse_public_key(r: &mut Reader) -> Result<PublicKey> {
    let magic = r.u32()?;
    if magic != RSA_MAGIC {
        bail!("server public key magic {magic:#x} isn't RSA1");
    }
    let key_len = r.u32()? as usize;
    let modulus_len = key_len.wrapping_sub(PADDING_SIZE);
    if !(MODULUS_SIZE..=MAX_MODULUS_SIZE).contains(&modulus_len) {
        bail!("invalid server public key size ({key_len} bytes)");
    }
    r.skip(8)?; // bitlen, datalen
    let exponent = r.bytes(EXPONENT_SIZE)?;
    let modulus = r.bytes(modulus_len)?;
    r.skip(PADDING_SIZE)?;
    let mut exponent = exponent.to_vec();
    exponent.reverse();
    let mut modulus = modulus.to_vec();
    modulus.reverse();
    key_from_be(&modulus, &exponent)
}

/// An X.509 chain after dwVersion (MS-RDPBCGR 2.2.1.4.3.1.1.1 as rdesktop
/// reads it): a count, then length-prefixed certificates. Those between the
/// root and the last two are only read past, and a bad one among them is no
/// error; the second-to-last (the signing CA) must parse; the key is the last
/// one's. The 16 bytes of padding after it, and any garbage, are ignored.
fn parse_x509_chain(r: &mut Reader) -> Result<PublicKey> {
    let count = r.u32()?;
    if count < 2 {
        bail!("the server didn't send enough X.509 certificates ({count})");
    }
    for _ in 2..count {
        let len = r.u32()? as usize;
        r.skip(len)?;
    }
    let len = r.u32()? as usize;
    let ca = r.bytes(len)?;
    if let Err(e) = subject_public_key_info(ca) {
        bail!("couldn't read the server's CA certificate: {e}");
    }
    let len = r.u32()? as usize;
    let server = r.bytes(len)?;
    match parse_certificate(server) {
        Ok(key) => Ok(key),
        Err(e) => bail!("couldn't read the server's certificate: {e}"),
    }
}

/// rsaEncryption, and the signature OIDs Microsoft's certificates put in
/// SubjectPublicKeyInfo instead (rdesktop's workaround in ssl.c).
const RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const MD5_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x04];
const SHA1_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x05];

/// The RSA key in a DER certificate's SubjectPublicKeyInfo.
pub fn parse_certificate(data: &[u8]) -> Result<PublicKey> {
    let (oid, bits) = subject_public_key_info(data)?;
    if ![RSA_ENCRYPTION, MD5_WITH_RSA, SHA1_WITH_RSA].contains(&oid) {
        bail!("the certificate's key isn't RSA");
    }
    let mut k = Der::new(bits).next(der::SEQUENCE)?.children();
    let modulus = k.next(der::INTEGER)?.unsigned();
    let exponent = k.next(der::INTEGER)?.unsigned();
    key_from_be(modulus, exponent)
}

/// A DER certificate's SubjectPublicKeyInfo: the algorithm OID's contents
/// and the key's bits. Walking down to it is all the checking a certificate
/// gets, as with rdesktop, which verifies no chain.
fn subject_public_key_info(data: &[u8]) -> Result<(&[u8], &[u8])> {
    let cert = Der::new(data).next(der::SEQUENCE)?;
    let mut c = cert.children();
    let tbs = c.next(der::SEQUENCE)?;
    c.next(der::SEQUENCE)?; // signatureAlgorithm
    c.next(der::BIT_STRING)?; // signature
    let mut t = tbs.children();
    t.optional(der::context(0))?; // version
    t.next(der::INTEGER)?; // serialNumber
    t.next(der::SEQUENCE)?; // signature
    t.next(der::SEQUENCE)?; // issuer
    t.next(der::SEQUENCE)?; // validity
    t.next(der::SEQUENCE)?; // subject
    let spki = t.next(der::SEQUENCE)?;
    let mut s = spki.children();
    let alg = s.next(der::SEQUENCE)?;
    let oid = alg.children().next(der::OID)?;
    let bits = s.next(der::BIT_STRING)?.bits()?;
    Ok((oid.value, bits))
}

/// A key from big-endian parts, checked for the sizes RDP can use.
fn key_from_be(modulus: &[u8], exponent: &[u8]) -> Result<PublicKey> {
    let modulus = strip_zeros(modulus);
    let exponent = strip_zeros(exponent);
    if !(MODULUS_SIZE..=MAX_MODULUS_SIZE).contains(&modulus.len()) {
        bail!("bad server public key size ({} bits)", modulus.len() * 8);
    }
    if exponent.is_empty() || exponent.len() > EXPONENT_SIZE {
        bail!("bad server public key exponent");
    }
    Ok(PublicKey { modulus: modulus.to_vec(), exponent: exponent.to_vec() })
}

fn strip_zeros(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|&&x| x == 0).count();
    &b[n..]
}
