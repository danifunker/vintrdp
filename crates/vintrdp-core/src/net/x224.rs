//! X.224 (ISO 8073 class 0) in TPKTs, with RDP's protocol negotiation in the
//! connection request and confirm. Port of rdesktop's iso.c.

use super::transport::TPKT_VERSION;
use crate::error::{Error, Result};
use crate::stream::{Reader, Writer};

const PDU_CR: u8 = 0xe0;
const PDU_CC: u8 = 0xd0;
const PDU_DR: u8 = 0x80;
const PDU_DT: u8 = 0xf0;

const NEG_REQ: u8 = 1;
const NEG_RSP: u8 = 2;
const NEG_FAILURE: u8 = 3;

/// requestedProtocols / selectedProtocol.
pub const PROTOCOL_RDP: u32 = 0;
#[cfg_attr(not(feature = "tls"), allow(dead_code))]
pub const PROTOCOL_SSL: u32 = 1;
#[cfg_attr(not(feature = "tls"), allow(dead_code))]
pub const PROTOCOL_HYBRID: u32 = 2;

/// Wrap `payload` (already X.224) in a TPKT header.
fn tpkt(payload: &[u8]) -> Vec<u8> {
    let mut w = Writer::with_capacity(payload.len() + 4);
    w.u8(TPKT_VERSION).u8(0).u16_be((payload.len() + 4) as u16).bytes(payload);
    w.into_vec()
}

/// The connection request. `negotiate` adds RDP_NEG_REQ (RDP 5 clients;
/// NT 4.0 TSE ignores it, and RDP 4 clients never sent it).
pub fn connection_request(username: &str, negotiate: Option<u32>) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(0) // length indicator, patched below
        .u8(PDU_CR)
        .u16(0) // dst_ref
        .u16(0) // src_ref
        .u8(0); // class
    w.bytes(b"Cookie: mstshash=").bytes(username.as_bytes()).bytes(b"\r\n");
    if let Some(protocols) = negotiate {
        w.u8(NEG_REQ).u8(0).u16(8).u32(protocols);
    }
    let li = (w.len() - 1) as u8;
    w.as_mut_slice()[0] = li;
    tpkt(w.as_slice())
}

/// How the server answered the connection request.
#[derive(Debug, PartialEq, Eq)]
pub enum Confirm {
    /// No negotiation in the answer: plain RDP security (servers before
    /// Server 2003 SP1).
    Legacy,
    /// The server picked `protocol`.
    Selected { protocol: u32, flags: u8 },
    /// The server refused what we asked for, with RDP_NEG_FAILURE's code.
    Failed(u32),
}

/// RDP_NEG_FAILURE codes.
pub const SSL_REQUIRED_BY_SERVER: u32 = 1;
pub const SSL_NOT_ALLOWED_BY_SERVER: u32 = 2;
pub const SSL_CERT_NOT_ON_SERVER: u32 = 3;
pub const INCONSISTENT_FLAGS: u32 = 4;
pub const HYBRID_REQUIRED_BY_SERVER: u32 = 5;
pub const SSL_WITH_USER_AUTH_REQUIRED_BY_SERVER: u32 = 6;

/// Parse the connection confirm frame.
pub fn parse_confirm(frame: &[u8]) -> Result<Confirm> {
    if frame.first() != Some(&TPKT_VERSION) {
        return Err(Error::protocol(
            "the server's answer isn't RDP: check that the host and port are an RDP listener, not a gateway, proxy or closed service",
        ));
    }
    let mut r = Reader::new(frame);
    r.skip(4)?;
    let _li = r.u8()?;
    if r.u8()? != PDU_CC {
        return Err(Error::protocol("expected an X.224 connection confirm"));
    }
    r.skip(5)?; // dst_ref, src_ref, class
    if r.remaining() < 8 {
        return Ok(Confirm::Legacy);
    }
    let kind = r.u8()?;
    let flags = r.u8()?;
    let _len = r.u16()?;
    let data = r.u32()?;
    match kind {
        NEG_RSP => Ok(Confirm::Selected { protocol: data, flags }),
        NEG_FAILURE => Ok(Confirm::Failed(data)),
        k => Err(Error::protocol(format!("unexpected negotiation response type {k}"))),
    }
}

/// A message for RDP_NEG_FAILURE's `code`.
pub fn failure_reason(code: u32) -> &'static str {
    match code {
        SSL_REQUIRED_BY_SERVER => "the server requires TLS",
        SSL_NOT_ALLOWED_BY_SERVER => "the server doesn't allow TLS",
        SSL_CERT_NOT_ON_SERVER => "the server has no certificate for TLS",
        INCONSISTENT_FLAGS => "inconsistent negotiation flags",
        HYBRID_REQUIRED_BY_SERVER => "the server requires Network Level Authentication (CredSSP)",
        SSL_WITH_USER_AUTH_REQUIRED_BY_SERVER => "the server requires TLS with user authentication",
        _ => "unknown reason",
    }
}

/// `payload` in an X.224 data TPDU, in a TPKT.
pub fn data(payload: &[u8]) -> Vec<u8> {
    let mut w = Writer::with_capacity(payload.len() + 7);
    w.u8(TPKT_VERSION).u8(0).u16_be((payload.len() + 7) as u16).u8(2).u8(PDU_DT).u8(0x80).bytes(payload);
    w.into_vec()
}

/// The disconnect request.
pub fn disconnect_request() -> Vec<u8> {
    tpkt(&[6, PDU_DR, 0, 0, 0, 0, 0])
}

/// The payload of an X.224 data TPDU frame.
pub fn parse_data(frame: &[u8]) -> Result<&[u8]> {
    let mut r = Reader::new(frame);
    if r.u8()? != TPKT_VERSION {
        return Err(Error::protocol("expected a TPKT"));
    }
    r.skip(3)?;
    let li = r.u8()?;
    let code = r.u8()?;
    if code != PDU_DT {
        return Err(Error::protocol(format!("expected X.224 data, got code {code:#x}")));
    }
    // The header is `li` bytes after the length indicator itself.
    r.seek(4 + 1 + li as usize)?;
    Ok(r.rest())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_request_with_negotiation() {
        let cr = connection_request("bob", Some(PROTOCOL_SSL | PROTOCOL_HYBRID));
        assert_eq!(&cr[..4], [3, 0, 0, cr.len() as u8]);
        assert_eq!(cr[4] as usize, cr.len() - 5);
        assert_eq!(cr[5], PDU_CR);
        assert!(cr.ends_with(&[b'\r', b'\n', 1, 0, 8, 0, 3, 0, 0, 0]));
        let plain = connection_request("bob", None);
        assert!(plain.ends_with(b"bob\r\n"));
    }

    #[test]
    fn confirms() {
        let legacy = [3, 0, 0, 11, 6, PDU_CC, 0, 0, 0x12, 0x34, 0];
        assert_eq!(parse_confirm(&legacy).unwrap(), Confirm::Legacy);
        let ssl = [3, 0, 0, 19, 14, PDU_CC, 0, 0, 0x12, 0x34, 0, NEG_RSP, 1, 8, 0, 1, 0, 0, 0];
        assert_eq!(parse_confirm(&ssl).unwrap(), Confirm::Selected { protocol: PROTOCOL_SSL, flags: 1 });
        let fail = [3, 0, 0, 19, 14, PDU_CC, 0, 0, 0, 0, 0, NEG_FAILURE, 0, 8, 0, 5, 0, 0, 0];
        assert_eq!(parse_confirm(&fail).unwrap(), Confirm::Failed(HYBRID_REQUIRED_BY_SERVER));
        assert!(parse_confirm(&[0x30, 5, 1, 2, 3]).is_err());
    }

    #[test]
    fn data_round_trip() {
        let f = data(&[1, 2, 3]);
        assert_eq!(f, [3, 0, 0, 10, 2, PDU_DT, 0x80, 1, 2, 3]);
        assert_eq!(parse_data(&f).unwrap(), [1, 2, 3]);
    }
}
