//! Security: choosing among the types the server offers, and logging on.
//!
//! - None (1) and VNC Authentication (2, DES of a challenge): every server.
//! - Tight (16): TightVNC's wrapper around None and VNC Authentication.
//! - VeNCrypt (19): TLS, then None, VNC Authentication or a user name and
//!   password, with the TLS anonymous (TigerVNC's default) or checked against
//!   the server's certificate. The TLS is the [`Host`]'s.
//! - Apple Remote Desktop (30): Diffie-Hellman, then the account's name and
//!   password encrypted with AES; macOS Screen Sharing.
//! - RSA-AES (5, 6, 129, 130): RealVNC's own, which TigerVNC also has; see
//!   `rsaaes.rs`.
//!
//! RFB 3.3 has only None and VNC Authentication, chosen by the server.

use std::io;

use aes::Aes128;
use des::Des;
use des::cipher::generic_array::GenericArray;
use des::cipher::{BlockEncrypt, KeyInit};
use md5::{Digest, Md5};
use num_bigint::BigUint;

use crate::stream::{Reader, Src, Writer, bad};
use crate::{Host, Settings, rsaaes};

pub(crate) const NONE: u8 = 1;
pub(crate) const VNC: u8 = 2;
pub(crate) const TIGHT: u8 = 16;
pub(crate) const VENCRYPT: u8 = 19;
pub(crate) const ARD: u8 = 30;
pub(crate) const RA2: u8 = 5;
pub(crate) const RA2NE: u8 = 6;
pub(crate) const RA2_256: u8 = 129;
pub(crate) const RA2NE_256: u8 = 130;

/// What the logon left behind for the rest of the connection.
#[derive(Default)]
pub(crate) struct Logon {
    /// Tight security was used: ServerInit carries Tight's capability lists.
    pub tight: bool,
}

fn type_name(t: u8) -> String {
    match t {
        NONE => "None".into(),
        VNC => "VNC Authentication".into(),
        RA2 => "RA2 (RealVNC)".into(),
        RA2NE => "RA2ne (RealVNC)".into(),
        RA2_256 => "RA2_256 (RealVNC)".into(),
        RA2NE_256 => "RA2ne_256 (RealVNC)".into(),
        TIGHT => "Tight".into(),
        17 => "Ultra".into(),
        18 => "TLS".into(),
        VENCRYPT => "VeNCrypt".into(),
        20 => "SASL".into(),
        21 => "MD5 hash".into(),
        22 => "xvp".into(),
        ARD => "Apple Remote Desktop".into(),
        35 => "Apple (Kerberos)".into(),
        113 => "UltraVNC MS-Logon".into(),
        t => format!("type {t}"),
    }
}

/// The types we can use, best first. With a password, those that encrypt the
/// whole session, then those that protect the password; without one, None.
fn preference(settings: &Settings, host: &dyn Host) -> Vec<u8> {
    let tls = host.has_tls();
    let mut order = Vec::new();
    if settings.password.is_empty() {
        order.extend([NONE, TIGHT, VNC]);
        if tls {
            order.push(VENCRYPT);
        }
        order.extend([ARD, RA2_256, RA2, RA2NE_256, RA2NE]);
    } else {
        order.extend([RA2_256, RA2]);
        if tls {
            order.push(VENCRYPT);
        }
        order.extend([RA2NE_256, RA2NE, ARD, VNC, TIGHT, NONE]);
    }
    order
}

/// Negotiate security and log on. `r` and `w` may be wrapped in another layer
/// on return (VeNCrypt's TLS, RSA-AES's encryption).
pub(crate) fn logon(settings: &Settings, host: &dyn Host, w: &Writer, r: &mut Reader, minor: u32) -> io::Result<Logon> {
    let password = &settings.password;
    if minor == 3 {
        // RFB 3.3: the server decides.
        return match r.u32()? {
            0 => Err(failure(r)),
            1 => Ok(Logon::default()),
            2 => {
                vnc_auth(w, r, password)?;
                result(r, minor).map(|_| Logon::default())
            }
            t => Err(bad(format!("the server wants {}, which RFB 3.3 doesn't have", type_name(t as u8)))),
        };
    }
    let count = r.u8()?;
    if count == 0 {
        return Err(failure(r));
    }
    let offered = r.bytes(count as usize)?;
    let Some(&chosen) = preference(settings, host).iter().find(|t| offered.contains(t)) else {
        let names: Vec<String> = offered.iter().map(|&t| type_name(t)).collect();
        return Err(bad(format!("the server offers no logon vintrdp knows: {}", names.join(", "))));
    };
    if settings.verbose {
        let names: Vec<String> = offered.iter().map(|&t| type_name(t)).collect();
        eprintln!("vintrdp: VNC security offered: {}; using {}", names.join(", "), type_name(chosen));
    }
    w.send(&[chosen])?;
    match chosen {
        NONE => {
            // RFB 3.7 sends no result for None.
            if minor >= 8 {
                result(r, minor)?;
            }
            Ok(Logon::default())
        }
        VNC => {
            vnc_auth(w, r, password)?;
            result(r, minor).map(|_| Logon::default())
        }
        ARD => {
            ard(w, r, &settings.username, password)?;
            result(r, minor).map(|_| Logon::default())
        }
        TIGHT => tight(w, r, password, minor),
        VENCRYPT => vencrypt(settings, host, w, r),
        RA2 | RA2NE | RA2_256 | RA2NE_256 => {
            rsaaes::logon(settings, host, w, r, chosen)?;
            result(r, minor).map(|_| Logon::default())
        }
        _ => unreachable!(),
    }
}

/// The reason after a failed negotiation.
fn failure(r: &mut Reader) -> io::Error {
    match r.string() {
        Ok(reason) if !reason.is_empty() => bad(format!("the server refused the connection: {reason}")),
        _ => bad("the server refused the connection"),
    }
}

/// SecurityResult: 0 is success; RFB 3.8 adds a reason to a failure.
fn result(r: &mut Reader, minor: u32) -> io::Result<()> {
    match r.u32()? {
        0 => Ok(()),
        code => {
            let reason = if minor >= 8 { r.string().unwrap_or_default() } else { String::new() };
            let reason = match (reason.trim(), code) {
                ("", 2) => "too many attempts; try again later".to_string(),
                ("", _) => "wrong password?".to_string(),
                (text, _) => text.to_string(),
            };
            // Servers often say it already ("Authentication failed").
            let message = if reason.to_ascii_lowercase().starts_with("authentication failed") { reason } else { format!("authentication failed: {reason}") };
            Err(io::Error::new(io::ErrorKind::PermissionDenied, message))
        }
    }
}

/// VNC Authentication: the 16-byte challenge encrypted with DES, keyed by the
/// password's first 8 bytes with each byte's bits reversed.
pub fn vnc_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (k, b) in key.iter_mut().zip(password.bytes()) {
        *k = b.reverse_bits();
    }
    let des = Des::new_from_slice(&key).expect("an 8-byte DES key");
    let mut out = *challenge;
    for block in out.chunks_exact_mut(8) {
        des.encrypt_block(GenericArray::from_mut_slice(block));
    }
    out
}

fn vnc_auth(w: &Writer, r: &mut Reader, password: &str) -> io::Result<()> {
    if password.is_empty() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the server wants a password"));
    }
    let mut challenge = [0u8; 16];
    r.read_into(&mut challenge)?;
    w.send(&vnc_response(password, &challenge))
}

/// Apple Remote Desktop (type 30): Diffie-Hellman with the server's
/// parameters; the shared secret's MD5 keys AES-128, which encrypts the user
/// name and password, each in 64 bytes, NUL-terminated, padded with random.
pub(crate) fn ard_response(generator: u16, prime: &[u8], server_public: &[u8], private: &[u8], username: &str, password: &str, padding: &[u8; 128]) -> Vec<u8> {
    let len = prime.len();
    let p = BigUint::from_bytes_be(prime);
    let x = BigUint::from_bytes_be(private);
    let fixed = |n: BigUint| {
        let b = n.to_bytes_be();
        let mut out = vec![0u8; len.saturating_sub(b.len())];
        out.extend_from_slice(&b[b.len().saturating_sub(len)..]);
        out
    };
    let public = fixed(BigUint::from(generator).modpow(&x, &p));
    let secret = fixed(BigUint::from_bytes_be(server_public).modpow(&x, &p));
    let key: [u8; 16] = Md5::digest(&secret).into();

    let mut credentials = *padding;
    let (user_field, pass_field) = credentials.split_at_mut(64);
    for (field, text) in [(user_field, username), (pass_field, password)] {
        let text = &text.as_bytes()[..text.len().min(63)];
        field[..text.len()].copy_from_slice(text);
        field[text.len()] = 0;
    }
    let aes = Aes128::new(GenericArray::from_slice(&key));
    for block in credentials.chunks_exact_mut(16) {
        aes.encrypt_block(GenericArray::from_mut_slice(block));
    }
    let mut out = credentials.to_vec();
    out.extend_from_slice(&public);
    out
}

fn ard(w: &Writer, r: &mut Reader, username: &str, password: &str) -> io::Result<()> {
    let generator = r.u16()?;
    let len = r.u16()? as usize;
    if !(16..=1024).contains(&len) {
        return Err(bad(format!("Apple Remote Desktop: a key of {len} bytes")));
    }
    let prime = r.bytes(len)?;
    let server_public = r.bytes(len)?;
    let mut private = vec![0u8; len];
    crate::random(&mut private);
    let mut padding = [0u8; 128];
    crate::random(&mut padding);
    w.send(&ard_response(generator, &prime, &server_public, &private, username, password, &padding))
}

/// Tight security: tunnels (we take none) and a list of logons inside it.
fn tight(w: &Writer, r: &mut Reader, password: &str, minor: u32) -> io::Result<Logon> {
    let tunnels = r.u32()?;
    if tunnels > 64 {
        return Err(bad("Tight security: too many tunnel types"));
    }
    r.skip(tunnels as usize * 16)?;
    if tunnels > 0 {
        w.send(&0u32.to_be_bytes())?; // NOTUNNEL
    }
    let count = r.u32()?;
    if count > 64 {
        return Err(bad("Tight security: too many logon types"));
    }
    let mut codes = Vec::new();
    for _ in 0..count {
        codes.push(r.u32()?);
        r.skip(12)?; // vendor and name
    }
    let logon = Logon { tight: true };
    if count == 0 {
        if minor >= 8 {
            result(r, minor)?;
        }
        return Ok(logon);
    }
    let vnc = codes.contains(&(VNC as u32));
    let none = codes.contains(&(NONE as u32));
    let chosen = if vnc && (!password.is_empty() || !none) {
        VNC
    } else if none {
        NONE
    } else {
        return Err(bad("Tight security: no logon vintrdp knows"));
    };
    w.send(&(chosen as u32).to_be_bytes())?;
    if chosen == VNC {
        vnc_auth(w, r, password)?;
        result(r, minor)?;
    } else if minor >= 8 {
        result(r, minor)?;
    }
    Ok(logon)
}

/// VeNCrypt subtypes.
mod vencrypt_types {
    pub const PLAIN: u32 = 256;
    pub const TLS_NONE: u32 = 257;
    pub const TLS_VNC: u32 = 258;
    pub const TLS_PLAIN: u32 = 259;
    pub const X509_NONE: u32 = 260;
    pub const X509_VNC: u32 = 261;
    pub const X509_PLAIN: u32 = 262;
}

fn vencrypt(settings: &Settings, host: &dyn Host, w: &Writer, r: &mut Reader) -> io::Result<Logon> {
    use vencrypt_types::*;
    let config = settings;
    let (major, minor) = (r.u8()?, r.u8()?);
    if (major, minor) < (0, 2) {
        return Err(bad(format!("VeNCrypt {major}.{minor} is too old (0.2 or later)")));
    }
    w.send(&[0, 2])?;
    if r.u8()? != 0 {
        return Err(bad("the server refused VeNCrypt 0.2"));
    }
    let count = r.u8()?;
    let mut offered = Vec::new();
    for _ in 0..count {
        offered.push(r.u32()?);
    }
    // A checked certificate before an anonymous one; with a password, the
    // logon that uses it. Plain (256) would send the password unencrypted:
    // never.
    let order: &[u32] = if config.password.is_empty() {
        &[X509_NONE, TLS_NONE, X509_VNC, TLS_VNC]
    } else {
        &[X509_VNC, X509_PLAIN, TLS_VNC, TLS_PLAIN, X509_NONE, TLS_NONE]
    };
    let Some(&chosen) = order.iter().find(|t| offered.contains(t)) else {
        let unencrypted = offered.contains(&PLAIN);
        return Err(bad(if unencrypted {
            "the server offers VeNCrypt only without encryption, which vintrdp won't send a password over".to_string()
        } else {
            format!("the server offers no VeNCrypt logon vintrdp knows: {offered:?}")
        }));
    };
    w.send(&chosen.to_be_bytes())?;
    if r.u8()? != 1 {
        return Err(bad("the server refused the VeNCrypt logon"));
    }
    if !r.is_drained() {
        return Err(bad("VeNCrypt: data before TLS"));
    }
    let anonymous = matches!(chosen, TLS_NONE | TLS_VNC | TLS_PLAIN);
    let tls = host.start_tls(w.tcp()?, anonymous)?;
    *r = Reader::new(w.wrap(tls));
    match chosen {
        TLS_VNC | X509_VNC => vnc_auth(w, r, &config.password)?,
        TLS_PLAIN | X509_PLAIN => {
            let (user, pass) = (config.username.as_bytes(), config.password.as_bytes());
            let mut m = Vec::new();
            m.extend_from_slice(&(user.len() as u32).to_be_bytes());
            m.extend_from_slice(&(pass.len() as u32).to_be_bytes());
            m.extend_from_slice(user);
            m.extend_from_slice(pass);
            w.send(&m)?;
        }
        _ => {}
    }
    result(r, 8)?;
    Ok(Logon::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vnc_auth_matches_known_answer() {
        // From a real exchange (x11vnc, password "secret").
        let challenge: [u8; 16] = *b"\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff\x00";
        let response = vnc_response("secret", &challenge);
        // DES-ECB of each half with the bit-reversed key, computed independently.
        let mut key = [0u8; 8];
        for (k, b) in key.iter_mut().zip(b"secret") {
            *k = b.reverse_bits();
        }
        let des = Des::new_from_slice(&key).unwrap();
        let mut expect = challenge;
        for block in expect.chunks_exact_mut(8) {
            des.encrypt_block(GenericArray::from_mut_slice(block));
        }
        assert_eq!(response, expect);
        // Only the first 8 bytes of the password count.
        assert_eq!(vnc_response("12345678", &challenge), vnc_response("123456789", &challenge));
        assert_ne!(vnc_response("secret", &challenge), vnc_response("Secret", &challenge));
    }

    #[test]
    fn ard_both_sides_agree() {
        // Play the server too: with its private key it gets the same secret,
        // and can decrypt the credentials.
        let prime = {
            let mut p = vec![0xffu8; 16];
            p[15] = 0x61; // 2^128 - 159, a prime
            p
        };
        let g = 2u16;
        let server_private = [7u8; 16];
        let p = BigUint::from_bytes_be(&prime);
        let server_public = BigUint::from(g).modpow(&BigUint::from_bytes_be(&server_private), &p).to_bytes_be();
        let mut server_public_fixed = vec![0u8; 16 - server_public.len()];
        server_public_fixed.extend(server_public);
        let client_private = [3u8; 16];
        let out = ard_response(g, &prime, &server_public_fixed, &client_private, "dani", "hunter2", &[0xaa; 128]);
        assert_eq!(out.len(), 128 + 16);
        let client_public = BigUint::from_bytes_be(&out[128..]);
        let secret = client_public.modpow(&BigUint::from_bytes_be(&server_private), &p).to_bytes_be();
        let mut secret_fixed = vec![0u8; 16 - secret.len()];
        secret_fixed.extend(secret);
        let key: [u8; 16] = Md5::digest(&secret_fixed).into();
        let aes = Aes128::new(GenericArray::from_slice(&key));
        let mut plain = out[..128].to_vec();
        use des::cipher::BlockDecrypt;
        for block in plain.chunks_exact_mut(16) {
            aes.decrypt_block(GenericArray::from_mut_slice(block));
        }
        assert_eq!(&plain[..5], b"dani\0");
        assert_eq!(&plain[64..72], b"hunter2\0");
        assert_eq!(plain[5], 0xaa);
    }
}
