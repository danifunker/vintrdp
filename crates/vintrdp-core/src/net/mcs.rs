//! T.125 MCS: the connect-initial/response exchange (BER), then domain,
//! user and channel setup and data (PER). Port of rdesktop's mcs.c.

use crate::error::{Error, Result};
use crate::stream::{Reader, Writer};

pub const GLOBAL_CHANNEL: u16 = 1003;
pub const USERCHANNEL_BASE: u16 = 1001;

const CONNECT_INITIAL: u16 = 0x7f65;
const CONNECT_RESPONSE: u16 = 0x7f66;

const BER_BOOLEAN: u8 = 1;
const BER_INTEGER: u8 = 2;
const BER_OCTET_STRING: u8 = 4;
const BER_RESULT: u8 = 10;
const BER_DOMAIN_PARAMS: u8 = 0x30;

const EDRQ: u8 = 1;
const DPUM: u8 = 8;
const AURQ: u8 = 10;
const AUCF: u8 = 11;
const CJRQ: u8 = 14;
const CJCF: u8 = 15;
const SDRQ: u8 = 25;
const SDIN: u8 = 26;

/// rn-user-requested, for the disconnect provider ultimatum.
const RN_USER_REQUESTED: u8 = 3;

fn ber_header(w: &mut Writer, tag: u16, len: usize) {
    if tag > 0xff {
        w.u16_be(tag);
    } else {
        w.u8(tag as u8);
    }
    if len >= 0x80 {
        w.u8(0x82).u16_be(len as u16);
    } else {
        w.u8(len as u8);
    }
}

/// Always two bytes, as rdesktop has always sent them (and NT 4.0 TSE has
/// always accepted).
fn ber_integer(w: &mut Writer, v: u32) {
    ber_header(w, BER_INTEGER as u16, 2);
    w.u16_be(v as u16);
}

fn domain_params(channels: u32, users: u32, tokens: u32, pdu_size: u32) -> Vec<u8> {
    let mut body = Writer::new();
    for v in [channels, users, tokens, 1, 0, 1, pdu_size, 2] {
        ber_integer(&mut body, v);
    }
    let mut w = Writer::new();
    ber_header(&mut w, BER_DOMAIN_PARAMS as u16, body.len());
    w.bytes(body.as_slice());
    w.into_vec()
}

/// MCS connect-initial carrying the GCC conference create request.
pub fn connect_initial(gcc: &[u8]) -> Vec<u8> {
    let mut body = Writer::new();
    ber_header(&mut body, BER_OCTET_STRING as u16, 1);
    body.u8(1); // calling domain
    ber_header(&mut body, BER_OCTET_STRING as u16, 1);
    body.u8(1); // called domain
    ber_header(&mut body, BER_BOOLEAN as u16, 1);
    body.u8(0xff); // upward
    body.bytes(&domain_params(34, 2, 0, 0xffff)); // target
    body.bytes(&domain_params(1, 1, 1, 0x420)); // minimum
    body.bytes(&domain_params(0xffff, 0xfc17, 0xffff, 0xffff)); // maximum
    ber_header(&mut body, BER_OCTET_STRING as u16, gcc.len());
    body.bytes(gcc);
    let mut w = Writer::new();
    ber_header(&mut w, CONNECT_INITIAL, body.len());
    w.bytes(body.as_slice());
    w.into_vec()
}

fn ber_parse(r: &mut Reader, tag: u16) -> Result<usize> {
    let got = if tag > 0xff { r.u16_be()? } else { r.u8()? as u16 };
    if got != tag {
        return Err(Error::protocol(format!("MCS: expected BER tag {tag:#x}, got {got:#x}")));
    }
    let len = r.u8()?;
    if len & 0x80 == 0 {
        return Ok(len as usize);
    }
    let n = (len & 0x7f) as usize;
    if n > 4 {
        return Err(Error::protocol("MCS: bad BER length"));
    }
    let mut l = 0usize;
    for _ in 0..n {
        l = (l << 8) | r.u8()? as usize;
    }
    Ok(l)
}

/// The GCC conference create response inside MCS connect-response. Fails if
/// the server refused the connection.
pub fn parse_connect_response(payload: &[u8]) -> Result<&[u8]> {
    let mut r = Reader::new(payload);
    ber_parse(&mut r, CONNECT_RESPONSE)?;
    let len = ber_parse(&mut r, BER_RESULT as u16)?;
    let result = r.bytes(len)?.first().copied().unwrap_or(0);
    if result != 0 {
        return Err(Error::protocol(format!("the server refused the MCS connection (result {result})")));
    }
    let len = ber_parse(&mut r, BER_INTEGER as u16)?;
    r.skip(len)?; // connect id
    let len = ber_parse(&mut r, BER_DOMAIN_PARAMS as u16)?;
    r.skip(len)?;
    let len = ber_parse(&mut r, BER_OCTET_STRING as u16)?;
    r.bytes(len)
}

/// PER unconstrained integer, as rdesktop writes it.
fn per_integer(w: &mut Writer, v: u32) {
    if v <= 0xff {
        w.u8(1).u8(v as u8);
    } else if v <= 0xffff {
        w.u8(2).u16_be(v as u16);
    } else {
        w.u8(4).u32_be(v);
    }
}

pub fn erect_domain_request() -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(EDRQ << 2);
    per_integer(&mut w, 1); // subHeight
    per_integer(&mut w, 1); // subInterval
    w.into_vec()
}

pub fn attach_user_request() -> Vec<u8> {
    vec![AURQ << 2]
}

/// The user ID the server assigned.
pub fn parse_attach_user_confirm(payload: &[u8]) -> Result<u16> {
    let mut r = Reader::new(payload);
    let opcode = r.u8()?;
    if opcode >> 2 != AUCF {
        return Err(Error::protocol("expected MCS attach user confirm"));
    }
    if r.u8()? != 0 {
        return Err(Error::protocol("the server refused the MCS user"));
    }
    if opcode & 2 == 0 {
        return Err(Error::protocol("MCS attach user confirm without a user ID"));
    }
    r.u16_be()
}

pub fn channel_join_request(user_id: u16, channel: u16) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(CJRQ << 2).u16_be(user_id).u16_be(channel);
    w.into_vec()
}

pub fn parse_channel_join_confirm(payload: &[u8]) -> Result<()> {
    let mut r = Reader::new(payload);
    if r.u8()? >> 2 != CJCF {
        return Err(Error::protocol("expected MCS channel join confirm"));
    }
    match r.u8()? {
        0 => Ok(()),
        res => Err(Error::protocol(format!("the server refused an MCS channel (result {res})"))),
    }
}

pub fn disconnect_provider_ultimatum() -> Vec<u8> {
    vec![(DPUM << 2) | (RN_USER_REQUESTED >> 1), (RN_USER_REQUESTED & 1) << 7]
}

/// Send data request header, followed by `data`.
pub fn send_data_request(user_id: u16, channel: u16, data: &[u8]) -> Vec<u8> {
    let mut w = Writer::with_capacity(data.len() + 8);
    w.u8(SDRQ << 2).u16_be(user_id).u16_be(channel).u8(0x70);
    w.u16_be(data.len() as u16 | 0x8000);
    w.bytes(data);
    w.into_vec()
}

/// What arrives as MCS data.
pub enum Incoming<'a> {
    Data { channel: u16, data: &'a [u8] },
    /// The server ended the connection.
    Disconnect,
}

pub fn parse_data(payload: &[u8]) -> Result<Incoming<'_>> {
    let mut r = Reader::new(payload);
    let opcode = r.u8()?;
    match opcode >> 2 {
        SDIN => {
            r.skip(2)?; // user ID
            let channel = r.u16_be()?;
            r.skip(1)?; // priority and segmentation
            let len = r.u8()?;
            if len & 0x80 != 0 {
                r.skip(1)?;
            }
            Ok(Incoming::Data { channel, data: r.rest() })
        }
        DPUM => Ok(Incoming::Disconnect),
        other => Err(Error::protocol(format!("unexpected MCS PDU {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_initial_wraps_gcc() {
        let gcc = vec![0xaa; 300];
        let ci = connect_initial(&gcc);
        let mut r = Reader::new(&ci);
        let len = ber_parse(&mut r, CONNECT_INITIAL).unwrap();
        assert_eq!(len, r.remaining());
        assert!(ci.ends_with(&gcc));
    }

    #[test]
    fn connect_response() {
        let mut w = Writer::new();
        let mut body = Writer::new();
        ber_header(&mut body, BER_RESULT as u16, 1);
        body.u8(0);
        ber_integer(&mut body, 0);
        body.bytes(&domain_params(34, 3, 0, 0xfff8));
        ber_header(&mut body, BER_OCTET_STRING as u16, 3);
        body.bytes(&[7, 8, 9]);
        ber_header(&mut w, CONNECT_RESPONSE, body.len());
        w.bytes(body.as_slice());
        assert_eq!(parse_connect_response(w.as_slice()).unwrap(), [7, 8, 9]);
    }

    #[test]
    fn data_pdus() {
        let sd = send_data_request(1007, GLOBAL_CHANNEL, &[1, 2]);
        assert_eq!(sd, [0x64, 0x03, 0xef, 0x03, 0xeb, 0x70, 0x80, 0x02, 1, 2]);
        let sdin = [0x68, 0, 1, 0x03, 0xeb, 0x70, 0x81, 0x00, 5, 6];
        match parse_data(&sdin).unwrap() {
            Incoming::Data { channel, data } => assert_eq!((channel, data), (GLOBAL_CHANNEL, &[5u8, 6][..])),
            Incoming::Disconnect => panic!(),
        }
        assert!(matches!(parse_data(&disconnect_provider_ultimatum()).unwrap(), Incoming::Disconnect));
        assert_eq!(parse_attach_user_confirm(&[(AUCF << 2) | 2, 0, 0, 6]).unwrap(), 6);
    }
}
