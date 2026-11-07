//! The user's certificate exceptions: servers whose certificates the system
//! doesn't trust but the user accepted once. Kept where rdesktop keeps them,
//! `~/.local/share/rdesktop/certs/known_certs` (under `%USERPROFILE%` on
//! Windows), in GnuTLS's trust-on-first-use format, so stores written by
//! rdesktop and earlier vintrdp builds still work, and the reverse. Port of
//! utils.c's utils_cert_handle_exception and its helpers, which used
//! gnutls_verify_stored_pubkey and gnutls_store_pubkey.

use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use super::sha256::sha256;
use super::x509::{self, Certificate};
use crate::error::{Error, Result};
use crate::util::hex;

/// The service name rdesktop files its entries under.
const SERVICE: &str = "rdesktop";

/// gnutls_certificate_status_t bits, for the report.
pub const CERT_REVOKED: u32 = 1 << 5;
pub const CERT_SIGNER_NOT_FOUND: u32 = 1 << 6;
pub const CERT_SIGNER_NOT_CA: u32 = 1 << 7;
pub const CERT_INSECURE_ALGORITHM: u32 = 1 << 8;
pub const CERT_NOT_ACTIVATED: u32 = 1 << 9;
pub const CERT_EXPIRED: u32 = 1 << 10;
pub const CERT_SIGNATURE_FAILURE: u32 = 1 << 11;
pub const CERT_REVOCATION_DATA_SUPERSEDED: u32 = 1 << 12;
pub const CERT_UNEXPECTED_OWNER: u32 = 1 << 14;
pub const CERT_REVOCATION_DATA_ISSUED_IN_FUTURE: u32 = 1 << 15;
pub const CERT_SIGNER_CONSTRAINTS_FAILURE: u32 = 1 << 16;
pub const CERT_MISMATCH: u32 = 1 << 17;
pub const CERT_PURPOSE_MISMATCH: u32 = 1 << 18;
pub const CERT_MISSING_OCSP_STATUS: u32 = 1 << 19;
pub const CERT_INVALID_OCSP_STATUS: u32 = 1 << 20;
pub const CERT_UNKNOWN_CRIT_EXTENSIONS: u32 = 1 << 21;

/// The store file, its directory created if need be.
fn store_path() -> Result<PathBuf> {
    let dir = crate::util::data_dir()
        .ok_or_else(|| Error::protocol("No home directory (HOME or USERPROFILE) to keep certificate exceptions in"))?
        .join("certs");
    if !dir.is_dir() && (dir.exists() || !crate::util::mkdirs(&dir)) {
        return Err(Error::protocol(format!("Could not create the certificate store directory {}", dir.display())));
    }
    Ok(dir.join("known_certs"))
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, on one line, as GnuTLS stores keys.
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// An unexpired entry for this host holds this key.
    Found,
    /// Entries for this host hold other keys.
    Mismatch,
    NotFound,
}

/// Look `spki` (the certificate's SubjectPublicKeyInfo) up for `host` in a
/// store's contents, as gnutls_verify_stored_pubkey does: `|g0|` lines hold
/// the key in base64, `|c0|` lines a hash of it; `*` matches any host or
/// service; expired lines don't count.
pub fn lookup(store: &str, host: &str, now: i64, spki: &[u8]) -> Lookup {
    let key = base64(spki);
    let mut mismatch = false;
    for line in store.lines() {
        let mut f = line.split('|').filter(|s| !s.is_empty());
        let (Some(kind), Some(h), Some(service), Some(expiry)) = (f.next(), f.next(), f.next(), f.next()) else {
            continue;
        };
        if (h != "*" && h != host) || (service != "*" && service != SERVICE) {
            continue;
        }
        let expiry: i64 = expiry.trim().parse().unwrap_or(0);
        if expiry > 0 && now > expiry {
            continue;
        }
        let matches = match kind {
            "g0" => f.next().map(|k| k.split([' ', '\t', '\r', '\n']).next() == Some(&key)),
            // A commitment: the gnutls_digest_algorithm_t, then the hash in hex.
            "c0" => match (f.next(), f.next().map(str::trim)) {
                (Some("3"), Some(h)) => Some(h.eq_ignore_ascii_case(&hex(&Sha1::digest(spki)))),
                (Some("6"), Some(h)) => Some(h.eq_ignore_ascii_case(&hex(&sha256(&[spki])))),
                _ => None,
            },
            _ => None,
        };
        match matches {
            Some(true) => return Lookup::Found,
            Some(false) => mismatch = true,
            None => {}
        }
    }
    if mismatch { Lookup::Mismatch } else { Lookup::NotFound }
}

/// A store line for `spki`, as gnutls_store_pubkey writes it.
pub fn entry(host: &str, expiry: i64, spki: &[u8]) -> String {
    format!("|g0|{host}|{SERVICE}|{}|{}\n", expiry.max(0), base64(spki))
}

fn append(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::OpenOptions::new().append(true).create(true).open(path)?.write_all(line.as_bytes())
}

/// The certificate's details: names, validity and fingerprints.
pub fn info(cert: &Certificate, der: &[u8]) -> String {
    format!(
        "    Subject: {}\n     Issuer: {}\n Valid From: {}\n         To: {}\n\n  Certificate fingerprints:\n\n       sha1: {}\n     sha256: {}\n",
        x509::name_string(&cert.subject),
        x509::name_string(&cert.issuer),
        x509::ctime(cert.not_before),
        x509::ctime(cert.not_after),
        hex(&Sha1::digest(der)),
        hex(&sha256(&[der])),
    )
}

/// Why the certificate isn't trusted, numbered, from GnuTLS's verification
/// `status` and whether the name didn't match.
pub fn status_report(cert: &Certificate, status: u32, hostname_mismatch: bool, hostname: &str) -> String {
    let mut out = String::new();
    let mut i = 1;
    let mut item = |out: &mut String, text: &str| {
        out.push_str(&format!(" {i}. {text}\n\n"));
        i += 1;
    };
    if hostname_mismatch {
        item(
            &mut out,
            &format!(
                "The hostname used for this connection does not match any of the names\n    given in the certificate.\n\n             Hostname: {hostname}"
            ),
        );
        out.pop();
        let cn = x509::common_names(&cert.subject);
        if !cn.is_empty() {
            out.push_str(&format!("          Common Name: {}\n", cn.join(", ")));
        }
        if !cert.alt_names.is_empty() {
            out.push_str(&format!("      Alternate names: {}\n", cert.alt_names.join(", ")));
        }
        out.push('\n');
    }
    let simple: [(u32, &str); 13] = [
        (CERT_REVOKED, "Certificate is revoked by its authority"),
        (CERT_SIGNER_NOT_FOUND, "Certificate issuer is not trusted by this system."),
        (CERT_SIGNER_NOT_CA, "Certificate signer is not a CA."),
        (CERT_INSECURE_ALGORITHM, "Certificate was signed using an insecure algorithm."),
        (CERT_NOT_ACTIVATED, "Certificate is not yet activated."),
        (CERT_EXPIRED, "Certificate has expired."),
        (CERT_SIGNATURE_FAILURE, "Failed to verify the signature of the certificate."),
        (CERT_REVOCATION_DATA_SUPERSEDED, "Revocation data are old and have been superseded."),
        (CERT_UNEXPECTED_OWNER, "The owner is not the expected one."),
        (CERT_REVOCATION_DATA_ISSUED_IN_FUTURE, "The revocation data have a future issue date."),
        (CERT_SIGNER_CONSTRAINTS_FAILURE, "The certificate's signer constraints were violated."),
        (CERT_MISMATCH, "The certificate presented isn't the expected one (TOFU)"),
        (CERT_PURPOSE_MISMATCH, "The certificate or an intermediate does not match the\n     intended purpose (extended key usage)."),
    ];
    for (bit, text) in simple {
        if status & bit == 0 {
            continue;
        }
        item(&mut out, text);
        match bit {
            CERT_SIGNER_NOT_FOUND => out.push_str(&format!("     Issuer: {}\n\n", x509::name_string(&cert.issuer))),
            CERT_NOT_ACTIVATED => out.push_str(&format!("     Valid From: {}\n\n", x509::ctime(cert.not_before))),
            CERT_EXPIRED => out.push_str(&format!("     Valid to: {}\n\n", x509::ctime(cert.not_after))),
            _ => {}
        }
    }
    let more: [(u32, &str); 3] = [
        (CERT_MISSING_OCSP_STATUS, "The certificate requires the server to send the certifiate\n     status, but no status was received."),
        (CERT_INVALID_OCSP_STATUS, "The received OCSP status response is invalid."),
        (CERT_UNKNOWN_CRIT_EXTENSIONS, "The certificate has extensions marked as critical which are\n     not supported."),
    ];
    for (bit, text) in more {
        if status & bit != 0 {
            item(&mut out, text);
        }
    }
    if out.is_empty() {
        item(&mut out, "The certificate could not be verified.");
    }
    out
}

const PROMPT: &str = "Do you trust this certificate (yes/no/y/n)?";
const REVIEW: &str = "Review the following certificate info before you trust it to be added as an exception.\n\
                      If you do not trust the certificate the connection atempt will be aborted:";

/// The system doesn't trust the server's certificate (`der`): accept it if
/// the user did before, else ask through `ask(message)` and remember a yes.
/// `status` and `hostname_mismatch` are GnuTLS's verdict, for the message.
pub fn handle_exception(der: &[u8], status: u32, hostname_mismatch: bool, hostname: &str, ask: impl FnOnce(&str) -> bool) -> Result<()> {
    handle_exception_in(&store_path()?, der, status, hostname_mismatch, hostname, ask)
}

fn handle_exception_in(
    path: &Path,
    der: &[u8],
    status: u32,
    hostname_mismatch: bool,
    hostname: &str,
    ask: impl FnOnce(&str) -> bool,
) -> Result<()> {
    let cert = x509::parse(der)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let store = match std::fs::read(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error::protocol(format!("Could not read the certificate store {}: {e}", path.display()))),
    };
    let message = match lookup(&store, hostname, now, cert.spki) {
        Lookup::Found => {
            eprintln!(
                "vintrdp: the server's certificate is NOT trusted by this system; it was accepted because the user added an exception for it"
            );
            return Ok(());
        }
        Lookup::Mismatch => format!(
            "ATTENTION! Found a certificate stored for host '{hostname}', but it does not match the certificate\n\
             received from server.\n{REVIEW}\n\n{}\n\n{PROMPT}",
            info(&cert, der)
        ),
        Lookup::NotFound => format!(
            "ATTENTION! The server uses an invalid security certificate which can not be trusted for\n\
             the following identified reason(s);\n\n{}\n{REVIEW}\n\n{}\n\n{PROMPT}",
            status_report(&cert, status, hostname_mismatch, hostname),
            info(&cert, der)
        ),
    };
    if !ask(&message) {
        return Err(Error::Cancelled);
    }
    append(&path, &entry(hostname, cert.not_after, cert.spki))
        .map_err(|e| Error::protocol(format!("Could not save the certificate in {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn store_lines() {
        let key = b"\x30\x03\x02\x01\x05";
        let other = b"\x30\x03\x02\x01\x07";
        let line = entry("srv", 2_000_000_000, key);
        assert_eq!(line, "|g0|srv|rdesktop|2000000000|MAMCAQU=\n");
        let now = 1_800_000_000;
        assert_eq!(lookup(&line, "srv", now, key), Lookup::Found);
        assert_eq!(lookup(&line, "srv", now, other), Lookup::Mismatch);
        assert_eq!(lookup(&line, "other", now, key), Lookup::NotFound);
        // Expired entries don't count; 0 never expires.
        assert_eq!(lookup(&line, "srv", 2_000_000_001, key), Lookup::NotFound);
        assert_eq!(lookup(&entry("srv", 0, key), "srv", i64::MAX, key), Lookup::Found);
        // A changed certificate added after the old one is found.
        let both = format!("{}{}", entry("srv", 0, other), line);
        assert_eq!(lookup(&both, "srv", now, key), Lookup::Found);
        // Wildcards, another service, CRLF, junk.
        assert_eq!(lookup("|g0|*|*|0|MAMCAQU=\r\n", "x", now, key), Lookup::Found);
        assert_eq!(lookup("|g0|srv|ssh|0|MAMCAQU=\n", "srv", now, key), Lookup::NotFound);
        assert_eq!(lookup("garbage\n||||\n|g0|srv\n", "srv", now, key), Lookup::NotFound);
    }

    #[test]
    fn commitments() {
        let key = b"\x30\x03\x02\x01\x05";
        let sha1 = hex(&Sha1::digest(key));
        let sha256 = hex(&sha256(&[key]));
        assert_eq!(lookup(&format!("|c0|srv|rdesktop|0|3|{sha1}\n"), "srv", 0, key), Lookup::Found);
        assert_eq!(lookup(&format!("|c0|srv|rdesktop|0|6|{}\n", sha256.to_uppercase()), "srv", 0, key), Lookup::Found);
        assert_eq!(lookup(&format!("|c0|srv|rdesktop|0|6|{sha1}\n"), "srv", 0, key), Lookup::Mismatch);
        assert_eq!(lookup("|c0|srv|rdesktop|0|99|00\n", "srv", 0, key), Lookup::NotFound);
    }

    #[test]
    fn exceptions() {
        let dir = std::env::temp_dir().join(format!("vintrdp-certstore-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("known_certs");
        let _ = std::fs::remove_file(&path);
        let der = x509::tests::sample();
        let spki = x509::parse(&der).unwrap().spki.to_vec();

        // Refused: nothing stored.
        let r = handle_exception_in(&path, &der, CERT_SIGNER_NOT_FOUND, false, "srv", |m| {
            assert!(m.contains("can not be trusted") && m.contains("not trusted by this system"));
            false
        });
        assert!(matches!(r, Err(Error::Cancelled)));
        assert!(!path.exists());
        // Accepted, stored, then accepted without asking.
        handle_exception_in(&path, &der, CERT_SIGNER_NOT_FOUND, false, "srv", |_| true).unwrap();
        handle_exception_in(&path, &der, CERT_SIGNER_NOT_FOUND, false, "srv", |_| panic!("asked again")).unwrap();
        let stored = std::fs::read_to_string(&path).unwrap();
        assert_eq!(stored, entry("srv", 2524607999, &spki));
        // Another key for the host: a warning about the change.
        std::fs::write(&path, entry("srv", 0, b"other key")).unwrap();
        let r = handle_exception_in(&path, &der, 0, true, "srv", |m| m.contains("does not match the certificate"));
        assert!(r.is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprints_and_report() {
        let der = x509::tests::sample();
        let cert = x509::parse(&der).unwrap();
        let text = info(&cert, &der);
        assert!(text.contains(&format!("sha1: {}", hex(&Sha1::digest(&der)))));
        assert!(text.contains(&format!("sha256: {}", hex(&sha256(&[&der])))));
        assert!(text.contains("Subject: CN=srv2003"));
        let report = status_report(&cert, CERT_SIGNER_NOT_FOUND | CERT_EXPIRED, true, "10.0.0.9");
        assert!(report.starts_with(" 1. The hostname used"));
        assert!(report.contains("Hostname: 10.0.0.9\n          Common Name: srv2003\n      Alternate names: srv2003.lab, 10.0.0.5\n\n"));
        assert!(report.contains(" 2. Certificate issuer is not trusted by this system.\n\n     Issuer: CN=srv2003\n\n"));
        assert!(report.contains(" 3. Certificate has expired.\n\n     Valid to: Fri Dec 31 23:59:59 2049 UTC\n\n"));
    }
}
