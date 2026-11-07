//! The parts of an X.509 certificate the certificate prompt shows and
//! CredSSP binds to: names, validity, alternative names and the public key.
//! GnuTLS checks the certificate; this only reads it. Replaces the
//! gnutls_x509_crt_* calls in rdesktop's utils.c and tcp.c.

use crate::bail;
use crate::der::{self, Der, Tlv};
use crate::error::Result;

/// A distinguished name: RDNs, each a set of (OID, value).
pub type Name = Vec<Vec<(String, String)>>;

pub struct Certificate<'a> {
    pub subject: Name,
    pub issuer: Name,
    /// Unix time.
    pub not_before: i64,
    pub not_after: i64,
    /// The SubjectPublicKeyInfo, whole: what the certificate store keeps.
    pub spki: &'a [u8],
    /// The subjectPublicKey BIT STRING's contents: what CredSSP binds to.
    pub public_key: &'a [u8],
    /// DNS names and IP addresses from subjectAltName.
    pub alt_names: Vec<String>,
}

const OID_SUBJECT_ALT_NAME: &str = "2.5.29.17";

pub fn parse(cert: &[u8]) -> Result<Certificate<'_>> {
    let mut tbs = Der::new(cert).next(der::SEQUENCE)?.children().next(der::SEQUENCE)?.children();
    tbs.optional(der::context(0))?; // version
    tbs.next(der::INTEGER)?; // serialNumber
    tbs.next(der::SEQUENCE)?; // signature
    let issuer = name(tbs.next(der::SEQUENCE)?)?;
    let mut validity = tbs.next(der::SEQUENCE)?.children();
    let not_before = time(validity.next_any()?)?;
    let not_after = time(validity.next_any()?)?;
    let subject = name(tbs.next(der::SEQUENCE)?)?;
    let spki = tbs.next(der::SEQUENCE)?;
    let mut spki_fields = spki.children();
    spki_fields.next(der::SEQUENCE)?; // algorithm
    let public_key = spki_fields.next(der::BIT_STRING)?.bits()?;
    let mut alt_names = Vec::new();
    while !tbs.is_empty() {
        let t = tbs.next_any()?;
        if t.tag == der::context(3) {
            alt_names = subject_alt_names(t)?;
        }
    }
    Ok(Certificate { subject, issuer, not_before, not_after, spki: spki.raw, public_key, alt_names })
}

fn name(seq: Tlv) -> Result<Name> {
    let mut rdns = Vec::new();
    let mut d = seq.children();
    while !d.is_empty() {
        let mut set = d.next(der::SET)?.children();
        let mut avas = Vec::new();
        while !set.is_empty() {
            let mut ava = set.next(der::SEQUENCE)?.children();
            let oid = oid_string(ava.next(der::OID)?.value);
            avas.push((oid, string(ava.next_any()?)));
        }
        rdns.push(avas);
    }
    Ok(rdns)
}

/// A directory string as text, whatever its type.
fn string(t: Tlv) -> String {
    match t.tag {
        // BMPString: UTF-16BE.
        0x1e => {
            let units: Vec<u16> = t.value.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&units)
        }
        // UniversalString: UTF-32BE.
        0x1c => t.value.chunks_exact(4).map(|c| char::from_u32(u32::from_be_bytes([c[0], c[1], c[2], c[3]])).unwrap_or('?')).collect(),
        _ => String::from_utf8_lossy(t.value).into_owned(),
    }
}

/// Dotted form of an OID's contents.
pub fn oid_string(v: &[u8]) -> String {
    let mut arcs: Vec<u64> = Vec::new();
    let mut n: u64 = 0;
    for &b in v {
        n = (n << 7) | (b & 0x7f) as u64;
        if b & 0x80 == 0 {
            if arcs.is_empty() {
                let first = (n / 40).min(2);
                arcs.push(first);
                arcs.push(n - first * 40);
            } else {
                arcs.push(n);
            }
            n = 0;
        }
        if n > u64::MAX >> 7 {
            return "?".into();
        }
    }
    arcs.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

fn short_name(oid: &str) -> &str {
    match oid {
        "2.5.4.3" => "CN",
        "2.5.4.6" => "C",
        "2.5.4.7" => "L",
        "2.5.4.8" => "ST",
        "2.5.4.10" => "O",
        "2.5.4.11" => "OU",
        "0.9.2342.19200300.100.1.25" => "DC",
        "1.2.840.113549.1.9.1" => "EMAIL",
        _ => oid,
    }
}

/// "CN=host, O=Org", in the certificate's order, as rdesktop shows it.
pub fn name_string(name: &Name) -> String {
    name.iter()
        .map(|rdn| rdn.iter().map(|(oid, v)| format!("{}={v}", short_name(oid))).collect::<Vec<_>>().join("+"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The common names in `name`.
pub fn common_names(name: &Name) -> Vec<&str> {
    name.iter().flatten().filter(|(oid, _)| oid == "2.5.4.3").map(|(_, v)| v.as_str()).collect()
}

fn subject_alt_names(extensions: Tlv) -> Result<Vec<String>> {
    let mut exts = extensions.children().next(der::SEQUENCE)?.children();
    while !exts.is_empty() {
        let mut ext = exts.next(der::SEQUENCE)?.children();
        let oid = oid_string(ext.next(der::OID)?.value);
        ext.optional(der::BOOLEAN)?; // critical
        let value = ext.next(der::OCTET_STRING)?;
        if oid != OID_SUBJECT_ALT_NAME {
            continue;
        }
        let mut names = Der::new(value.value).next(der::SEQUENCE)?.children();
        let mut out = Vec::new();
        while !names.is_empty() {
            let n = names.next_any()?;
            match (n.tag, n.value.len()) {
                // dNSName [2] IA5String
                (0x82, _) => out.push(String::from_utf8_lossy(n.value).into_owned()),
                // iPAddress [7]
                (0x87, 4) => out.push(std::net::Ipv4Addr::new(n.value[0], n.value[1], n.value[2], n.value[3]).to_string()),
                (0x87, 16) => {
                    let mut b = [0u8; 16];
                    b.copy_from_slice(n.value);
                    out.push(std::net::Ipv6Addr::from(b).to_string());
                }
                _ => {}
            }
        }
        return Ok(out);
    }
    Ok(Vec::new())
}

fn digits(s: &[u8]) -> Result<i64> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        bail!("X.509: bad time in certificate");
    }
    Ok(s.iter().fold(0i64, |n, &d| n * 10 + (d - b'0') as i64))
}

/// UTCTime or GeneralizedTime (in UTC, seconds present, as RFC 5280 has
/// them) to Unix time.
fn time(t: Tlv) -> Result<i64> {
    let v = t.value;
    let (year, rest) = match t.tag {
        0x17 if v.len() >= 12 => {
            let yy = digits(&v[..2])?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, &v[2..])
        }
        0x18 if v.len() >= 14 => (digits(&v[..4])?, &v[4..]),
        _ => bail!("X.509: bad time in certificate"),
    };
    let field = |i: usize| digits(&rest[i..i + 2]);
    let (month, day, h, m, s) = (field(0)?, field(2)?, field(4)?, field(6)?, field(8)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        bail!("X.509: bad date in certificate");
    }
    Ok(days_from_civil(year, month, day) * 86400 + h * 3600 + m * 60 + s)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Unix time as ctime(3) writes it, in UTC: "Thu Nov  7 12:00:00 2026 UTC".
pub fn ctime(t: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{} {} {:2} {:02}:{:02}:{:02} {y} UTC",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize],
        d,
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A self-signed certificate like the ones Windows makes for RDP:
    /// CN=srv2003, RSA, valid 2026-01-01 to 2049-12-31, with a SAN.
    pub(crate) fn sample() -> Vec<u8> {
        let cn = |s: &str| {
            der::sequence(&[&der::constructed(der::SET, &[&der::sequence(&[&der::oid(&[2, 5, 4, 3]), &der::tlv(0x0c, s.as_bytes())])])])
        };
        let rsa = der::sequence(&[&der::oid(&[1, 2, 840, 113549, 1, 1, 1]), &der::tlv(der::NULL, &[])]);
        let key = der::sequence(&[&der::tlv(der::INTEGER, &[0x00, 0xc3, 0x5a, 0x01]), &der::uint(65537)]);
        let spki = der::sequence(&[&rsa, &der::tlv(der::BIT_STRING, &[&[0u8][..], &key].concat())]);
        let san = der::sequence(&[&der::tlv(0x82, b"srv2003.lab"), &der::tlv(0x87, &[10, 0, 0, 5])]);
        let ext = der::sequence(&[&der::oid(&[2, 5, 29, 17]), &der::octet_string(&san)]);
        let tbs = der::sequence(&[
            &der::explicit(0, &der::uint(2)),
            &der::uint(1),
            &rsa,
            &cn("srv2003"),
            &der::sequence(&[&der::tlv(0x17, b"260101000000Z"), &der::tlv(0x18, b"20491231235959Z")]),
            &cn("srv2003"),
            &spki,
            &der::explicit(3, &der::sequence(&[&ext])),
        ]);
        der::sequence(&[&tbs, &rsa, &der::tlv(der::BIT_STRING, &[0, 1, 2, 3])])
    }

    #[test]
    fn parses() {
        let der = sample();
        let c = parse(&der).unwrap();
        assert_eq!(name_string(&c.subject), "CN=srv2003");
        assert_eq!(common_names(&c.issuer), ["srv2003"]);
        assert_eq!(c.not_before, 1_767_225_600);
        assert_eq!(ctime(c.not_before), "Thu Jan  1 00:00:00 2026 UTC");
        assert_eq!(ctime(c.not_after), "Fri Dec 31 23:59:59 2049 UTC");
        assert_eq!(c.alt_names, ["srv2003.lab", "10.0.0.5"]);
        assert_eq!(c.public_key[..2], [0x30, 0x0b]);
        assert_eq!(c.spki[0], 0x30);
        for n in 0..der.len() {
            assert!(parse(&der[..n]).is_err());
        }
    }

    #[test]
    fn oids_and_dates() {
        assert_eq!(oid_string(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1]), "1.2.840.113549.1.1.1");
        assert_eq!(oid_string(&[0x09, 0x92, 0x26]), "0.9.2342");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2000, 2, 29)), (2000, 2, 29));
        assert_eq!(ctime(-1), "Wed Dec 31 23:59:59 1969 UTC");
    }
}
