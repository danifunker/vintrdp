//! Just enough SPNEGO (RFC 4178) for NTLM inside CredSSP: wrap an NTLM token
//! in NegTokenInit or NegTokenResp, and find the NTLM token in whatever the
//! server sends back, SPNEGO or raw. rdesktop sends raw NTLM, which Windows
//! accepts, and servers may answer either way. Port of rdesktop's spnego.c.

use crate::der;

const NTLMSSP_SIGNATURE: &[u8] = b"NTLMSSP\0";
/// 1.3.6.1.5.5.2
#[cfg(test)]
const SPNEGO_OID: [u32; 7] = [1, 3, 6, 1, 5, 5, 2];
/// 1.3.6.1.4.1.311.2.2.10
#[cfg(test)]
const NTLMSSP_OID: [u32; 10] = [1, 3, 6, 1, 4, 1, 311, 2, 2, 10];

/// `[APPLICATION 0] { spnego OID, [0] NegTokenInit { [0] mechTypes { NTLM },
/// [2] mechToken } }`.
#[cfg(test)]
pub fn wrap_init(mech_token: &[u8]) -> Vec<u8> {
    let mech_types = der::explicit(0, &der::sequence(&[&der::oid(&NTLMSSP_OID)]));
    let token = der::explicit(2, &der::octet_string(mech_token));
    let init = der::explicit(0, &der::sequence(&[&mech_types, &token]));
    der::constructed(der::application(0), &[&der::oid(&SPNEGO_OID), &init])
}

/// `[1] NegTokenResp { [2] responseToken }`.
#[cfg(test)]
pub fn wrap_resp(mech_token: &[u8]) -> Vec<u8> {
    der::explicit(1, &der::sequence(&[&der::explicit(2, &der::octet_string(mech_token))]))
}

/// The first OCTET STRING holding an NTLM message, searched depth first
/// through constructed elements.
fn find(data: &[u8], depth: u32) -> Option<&[u8]> {
    if depth > 16 {
        return None;
    }
    let mut d = der::Der::new(data);
    while !d.is_empty() {
        let t = d.next_any().ok()?;
        if t.tag == der::OCTET_STRING && t.value.starts_with(NTLMSSP_SIGNATURE) {
            return Some(t.value);
        }
        if (t.tag & 0x20 != 0 || t.tag & 0x80 != 0)
            && let Some(found) = find(t.value, depth + 1)
        {
            return Some(found);
        }
    }
    None
}

/// The NTLM message in `token`: inside SPNEGO, or the token itself (or
/// whatever follows the NTLMSSP signature) when it's raw.
pub fn extract_ntlm(token: &[u8]) -> Option<&[u8]> {
    find(token, 0).or_else(|| token.windows(NTLMSSP_SIGNATURE.len()).position(|w| w == NTLMSSP_SIGNATURE).map(|i| &token[i..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTLM: &[u8] = b"NTLMSSP\0\x01\x00\x00\x00rest";

    #[test]
    fn init_encoding() {
        let t = wrap_init(NTLM);
        // As rdesktop's spnego_wrap_neg_token_init lays it out.
        let mut expect = vec![0x60, 0x30, 0x06, 0x06, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];
        expect.extend([0xa0, 0x26, 0x30, 0x24, 0xa0, 0x0e, 0x30, 0x0c, 0x06, 0x0a]);
        expect.extend([0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x0a]);
        expect.extend([0xa2, 0x12, 0x04, 0x10]);
        expect.extend(NTLM);
        assert_eq!(t, expect);
        assert_eq!(extract_ntlm(&t), Some(NTLM));
    }

    #[test]
    fn resp_round_trip() {
        let t = wrap_resp(NTLM);
        assert_eq!(&t[..6], [0xa1, 0x16, 0x30, 0x14, 0xa2, 0x12]);
        assert_eq!(extract_ntlm(&t), Some(NTLM));
        // A server's NegTokenResp: negState, supportedMech, then the token.
        let server = der::explicit(
            1,
            &der::sequence(&[
                &der::explicit(0, &der::tlv(der::ENUMERATED, &[1])),
                &der::explicit(1, &der::oid(&NTLMSSP_OID)),
                &der::explicit(2, &der::octet_string(NTLM)),
            ]),
        );
        assert_eq!(extract_ntlm(&server), Some(NTLM));
    }

    #[test]
    fn raw_and_garbage() {
        assert_eq!(extract_ntlm(NTLM), Some(NTLM));
        assert_eq!(extract_ntlm(b"junkNTLMSSP\0x"), Some(&b"NTLMSSP\0x"[..]));
        assert_eq!(extract_ntlm(&[0xa1, 0x84, 0xff, 0xff, 0xff, 0xff]), None);
        assert_eq!(extract_ntlm(&[]), None);
        // Deep nesting stops, without overflowing the stack.
        let mut deep = NTLM.to_vec();
        for _ in 0..40 {
            deep = der::explicit(0, &deep);
        }
        assert!(extract_ntlm(&deep).is_some());
        let t = wrap_init(NTLM);
        for n in 0..t.len() {
            let _ = extract_ntlm(&t[..n]);
        }
    }
}
