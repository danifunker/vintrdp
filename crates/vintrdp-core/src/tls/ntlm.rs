//! The NTLMv2 client CredSSP authenticates with (MS-NLMP): NEGOTIATE,
//! CHALLENGE and AUTHENTICATE messages with a MIC, then sealing and signing
//! with the negotiated keys (extended session security, key exchange,
//! 128-bit RC4). Port of rdesktop's ntlmssp.c.

use hmac::{Hmac, Mac};
use md4::Md4;
use md5::{Digest, Md5};
use rc4::consts::U16;
use rc4::{KeyInit, Rc4, StreamCipher};

use crate::bail;
use crate::error::{Error, Result};
use crate::stream::{Reader, Writer};

const UNICODE: u32 = 0x0000_0001;
const OEM: u32 = 0x0000_0002;
const REQUEST_TARGET: u32 = 0x0000_0004;
const SIGN: u32 = 0x0000_0010;
const SEAL: u32 = 0x0000_0020;
const LM_KEY: u32 = 0x0000_0080;
const NTLM: u32 = 0x0000_0200;
const WORKSTATION_SUPPLIED: u32 = 0x0000_2000;
const ALWAYS_SIGN: u32 = 0x0000_8000;
const EXTENDED_SESSIONSECURITY: u32 = 0x0008_0000;
const TARGET_INFO: u32 = 0x0080_0000;
const VERSION: u32 = 0x0200_0000;
const NEG_128: u32 = 0x2000_0000;
const KEY_EXCH: u32 = 0x4000_0000;
const NEG_56: u32 = 0x8000_0000;

/// What NEGOTIATE asks for. Not TARGET_INFO: newer Windows servers drop the
/// first CredSSP token, without a CHALLENGE, when it carries that flag.
const NEGOTIATE_FLAGS: u32 = UNICODE
    | OEM
    | REQUEST_TARGET
    | SIGN
    | SEAL
    | LM_KEY
    | NTLM
    | ALWAYS_SIGN
    | EXTENDED_SESSIONSECURITY
    | VERSION
    | NEG_128
    | KEY_EXCH
    | NEG_56;
/// The CHALLENGE flags AUTHENTICATE may echo.
const AUTH_FLAGS: u32 = NEGOTIATE_FLAGS | TARGET_INFO;

const SIGNATURE: &[u8; 8] = b"NTLMSSP\0";
/// Header, VERSION and MIC of an AUTHENTICATE message: where its payload starts.
const AUTH_PAYLOAD: usize = 64 + 8 + 16;
const MIC_OFFSET: usize = 72;

const AV_EOL: u16 = 0x0000;
const AV_FLAGS: u16 = 0x0006;
const AV_TIMESTAMP: u16 = 0x0007;
const AV_TARGET_NAME: u16 = 0x0009;
const AV_FLAG_MIC_PRESENT: u32 = 0x0000_0002;
/// The service name rdesktop puts in MsvAvTargetName.
const TARGET_NAME: &str = "TERMSRV";

const CLIENT_SIGN_MAGIC: &[u8] = b"session key to client-to-server signing key magic constant\0";
const SERVER_SIGN_MAGIC: &[u8] = b"session key to server-to-client signing key magic constant\0";
const CLIENT_SEAL_MAGIC: &[u8] = b"session key to client-to-server sealing key magic constant\0";
const SERVER_SEAL_MAGIC: &[u8] = b"session key to server-to-client sealing key magic constant\0";

fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut m = <Hmac<Md5> as Mac>::new_from_slice(key).expect("HMAC takes keys of any length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

fn md5(parts: &[&[u8]]) -> [u8; 16] {
    let mut m = Md5::new();
    for p in parts {
        m.update(p);
    }
    m.finalize().into()
}

pub(super) fn rc4(key: &[u8; 16]) -> Rc4<U16> {
    Rc4::<U16>::new(key.into())
}

fn utf16(s: &str) -> Vec<u8> {
    crate::text::to_utf16le(s, false)
}

/// Upper case as Windows does it: one character for one, so 'ß' stays.
fn upper(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut u = c.to_uppercase();
            match (u.next(), u.next()) {
                (Some(one), None) => one,
                _ => c,
            }
        })
        .collect()
}

/// NTOWFv2: HMAC-MD5 keyed with the MD4 of the password over the upper-cased
/// user name and the domain.
pub fn ntowfv2(user: &str, domain: &str, password: &str) -> [u8; 16] {
    let nt_hash: [u8; 16] = Md4::digest(utf16(password)).into();
    hmac_md5(&nt_hash, &[&utf16(&upper(user)), &utf16(domain)])
}

/// The 8-byte VERSION field: Windows 10 2004, NTLM revision 15, as rdesktop
/// sends.
fn put_version(w: &mut Writer) {
    w.u8(10).u8(0).u16(19041).zeros(3).u8(0x0f);
}

fn put_buffer(w: &mut Writer, len: usize, offset: usize) {
    w.u16(len as u16).u16(len as u16).u32(offset as u32);
}

/// The NEGOTIATE message, with no domain or workstation.
pub fn negotiate() -> Vec<u8> {
    let mut w = Writer::with_capacity(40);
    w.bytes(SIGNATURE).u32(1).u32(NEGOTIATE_FLAGS);
    put_buffer(&mut w, 0, 40);
    put_buffer(&mut w, 0, 40);
    put_version(&mut w);
    w.into_vec()
}

/// A CHALLENGE message, parsed.
#[derive(Debug)]
pub struct Challenge {
    pub flags: u32,
    pub server_challenge: [u8; 8],
    /// The server's domain (or name), which stands in for an empty domain.
    pub target_name: String,
    pub target_info: Vec<u8>,
}

/// A security buffer's contents: length, allocated length and offset into
/// the whole message.
fn read_buffer<'a>(r: &mut Reader<'a>) -> Result<&'a [u8]> {
    let len = r.u16()? as usize;
    let alloc = r.u16()? as usize;
    let offset = r.u32()? as usize;
    if len != alloc {
        bail!("NTLM: malformed security buffer in the server's challenge");
    }
    let msg = r.data();
    match offset.checked_add(len) {
        Some(end) if end <= msg.len() => Ok(&msg[offset..end]),
        _ => Err(Error::Truncated),
    }
}

pub fn parse_challenge(msg: &[u8]) -> Result<Challenge> {
    let mut r = Reader::new(msg);
    r.check(48)?;
    if r.bytes(8)? != SIGNATURE || r.u32()? != 2 {
        bail!("NTLM: the server's reply is not a CHALLENGE message");
    }
    let target_name = crate::text::from_utf16le(read_buffer(&mut r)?);
    let flags = r.u32()?;
    let server_challenge = r.array()?;
    r.skip(8)?;
    let target_info = read_buffer(&mut r)?.to_vec();
    Ok(Challenge { flags, server_challenge, target_name, target_info })
}

/// The AV pairs in `info`, up to MsvAvEOL. Stops quietly at a truncated pair,
/// as rdesktop does.
fn av_pairs(info: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut r = Reader::new(info);
    while let (Ok(id), Ok(len)) = (r.u16(), r.u16()) {
        if id == AV_EOL {
            break;
        }
        match r.bytes(len as usize) {
            Ok(v) => out.push((id, v)),
            Err(_) => break,
        }
    }
    out
}

/// The server's AV pairs as AUTHENTICATE sends them back: MsvAvFlags says a
/// MIC is present, the target name is ours, and no channel bindings (an
/// all-zero one is not the same as none).
fn auth_target_info(info: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    let mut saw_flags = false;
    for (id, v) in av_pairs(info) {
        if id == AV_TARGET_NAME {
            continue;
        }
        w.u16(id).u16(v.len() as u16);
        if id == AV_FLAGS && v.len() == 4 {
            w.u32(u32::from_le_bytes([v[0], v[1], v[2], v[3]]) | AV_FLAG_MIC_PRESENT);
            saw_flags = true;
        } else {
            w.bytes(v);
        }
    }
    if !saw_flags {
        w.u16(AV_FLAGS).u16(4).u32(AV_FLAG_MIC_PRESENT);
    }
    let name = utf16(TARGET_NAME);
    w.u16(AV_TARGET_NAME).u16(name.len() as u16).bytes(&name);
    w.u16(AV_EOL).u16(0);
    w.into_vec()
}

/// The server's MsvAvTimestamp, which the response must use when present.
fn server_timestamp(info: &[u8]) -> Option<u64> {
    av_pairs(info).into_iter().find(|&(id, v)| id == AV_TIMESTAMP && v.len() == 8).map(|(_, v)| {
        let mut b = [0u8; 8];
        b.copy_from_slice(v);
        u64::from_le_bytes(b)
    })
}

/// Now, as a FILETIME.
fn filetime_now() -> u64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    (unix + 11_644_473_600) * 10_000_000
}

/// NTProofStr and SessionBaseKey for the NTLMv2 `temp` blob.
pub(super) fn nt_proof(response_key: &[u8; 16], server_challenge: &[u8; 8], temp: &[u8]) -> ([u8; 16], [u8; 16]) {
    let proof = hmac_md5(response_key, &[server_challenge, temp]);
    let base = hmac_md5(response_key, &[&proof]);
    (proof, base)
}

/// What goes into AUTHENTICATE besides the server's message, fixed by tests.
struct Inputs<'a> {
    user: &'a str,
    domain: &'a str,
    password: &'a str,
    workstation: &'a str,
    client_challenge: [u8; 8],
    session_key: [u8; 16],
    timestamp: u64,
}

/// Build the AUTHENTICATE message answering `challenge_msg`, which answered
/// `negotiate_msg`. An empty `domain` takes the server's. `workstation` is
/// the client's name. Returns the message and the established context.
pub fn authenticate(
    negotiate_msg: &[u8],
    challenge_msg: &[u8],
    user: &str,
    domain: &str,
    password: &str,
    workstation: &str,
) -> Result<(Vec<u8>, Ntlm)> {
    let mut random = [0u8; 24];
    crate::util::random(&mut random);
    let mut client_challenge = [0u8; 8];
    let mut session_key = [0u8; 16];
    client_challenge.copy_from_slice(&random[..8]);
    session_key.copy_from_slice(&random[8..]);
    let challenge = parse_challenge(challenge_msg)?;
    let timestamp = server_timestamp(&challenge.target_info).unwrap_or_else(filetime_now);
    let inputs = Inputs { user, domain, password, workstation, client_challenge, session_key, timestamp };
    build_authenticate(negotiate_msg, challenge_msg, &challenge, &inputs)
}

fn build_authenticate(negotiate_msg: &[u8], challenge_msg: &[u8], challenge: &Challenge, i: &Inputs) -> Result<(Vec<u8>, Ntlm)> {
    if challenge.flags & TARGET_INFO == 0 || challenge.target_info.is_empty() {
        bail!("NTLM: the server did not offer NTLMv2 (no target info in its challenge)");
    }
    let domain = if i.domain.is_empty() { challenge.target_name.as_str() } else { i.domain };
    let response_key = ntowfv2(i.user, domain, i.password);

    let mut temp = Writer::new();
    temp.u8(1).u8(1).u16(0).u32(0).u64(i.timestamp).bytes(&i.client_challenge).u32(0);
    temp.bytes(&auth_target_info(&challenge.target_info));
    let temp = temp.into_vec();

    let (proof, session_base_key) = nt_proof(&response_key, &challenge.server_challenge, &temp);
    let nt_response = [&proof[..], &temp].concat();
    let lm_response = [&hmac_md5(&response_key, &[&challenge.server_challenge, &i.client_challenge])[..], &i.client_challenge].concat();
    let mut encrypted_key = i.session_key;
    rc4(&session_base_key).apply_keystream(&mut encrypted_key);

    let fields: [Vec<u8>; 6] = [
        lm_response,
        nt_response,
        utf16(domain),
        utf16(i.user),
        utf16(if i.workstation.is_empty() { "RDESKTOP" } else { i.workstation }),
        encrypted_key.to_vec(),
    ];
    if fields.iter().any(|f| f.len() > 0xffff) {
        bail!("NTLM: user name, domain or server data too long");
    }
    let flags = (challenge.flags & AUTH_FLAGS)
        | UNICODE
        | KEY_EXCH
        | SIGN
        | SEAL
        | WORKSTATION_SUPPLIED
        | EXTENDED_SESSIONSECURITY
        | NEG_128
        | NEG_56
        | VERSION;

    let mut w = Writer::new();
    w.bytes(SIGNATURE).u32(3);
    let mut offset = AUTH_PAYLOAD;
    for f in &fields {
        put_buffer(&mut w, f.len(), offset);
        offset += f.len();
    }
    w.u32(flags);
    put_version(&mut w);
    w.zeros(16);
    for f in &fields {
        w.bytes(f);
    }
    let mut msg = w.into_vec();
    let mic = hmac_md5(&i.session_key, &[negotiate_msg, challenge_msg, &msg]);
    msg[MIC_OFFSET..MIC_OFFSET + 16].copy_from_slice(&mic);
    Ok((msg, Ntlm::new(&i.session_key)))
}

/// An established NTLM context: sealing (GSS_WrapEx) both ways.
pub struct Ntlm {
    client_signing_key: [u8; 16],
    server_signing_key: [u8; 16],
    client_seal: Rc4<U16>,
    server_seal: Rc4<U16>,
    send_seq: u32,
    recv_seq: u32,
}

impl Ntlm {
    fn new(session_key: &[u8; 16]) -> Ntlm {
        Ntlm {
            client_signing_key: md5(&[session_key, CLIENT_SIGN_MAGIC]),
            server_signing_key: md5(&[session_key, SERVER_SIGN_MAGIC]),
            client_seal: rc4(&md5(&[session_key, CLIENT_SEAL_MAGIC])),
            server_seal: rc4(&md5(&[session_key, SERVER_SEAL_MAGIC])),
            send_seq: 0,
            recv_seq: 0,
        }
    }

    /// The server's end of a context, for tests: the keys swapped.
    #[cfg(test)]
    pub(super) fn server(session_key: &[u8; 16]) -> Ntlm {
        let mut s = Ntlm::new(session_key);
        std::mem::swap(&mut s.client_seal, &mut s.server_seal);
        std::mem::swap(&mut s.client_signing_key, &mut s.server_signing_key);
        s
    }

    /// Seal `plain`: the 16-byte signature, then the encrypted message. The
    /// message goes through the RC4 stream before the checksum does.
    pub fn wrap(&mut self, plain: &[u8]) -> Vec<u8> {
        let seq = self.send_seq;
        let mut out = vec![0u8; 16];
        out.extend_from_slice(plain);
        self.client_seal.apply_keystream(&mut out[16..]);
        let mut checksum = [0u8; 8];
        checksum.copy_from_slice(&hmac_md5(&self.client_signing_key, &[&seq.to_le_bytes(), plain])[..8]);
        self.client_seal.apply_keystream(&mut checksum);
        out[..4].copy_from_slice(&1u32.to_le_bytes());
        out[4..12].copy_from_slice(&checksum);
        out[12..16].copy_from_slice(&seq.to_le_bytes());
        self.send_seq = seq.wrapping_add(1);
        out
    }

    /// Check and unseal a message from the server.
    pub fn unwrap(&mut self, wrapped: &[u8]) -> Result<Vec<u8>> {
        let mut r = Reader::new(wrapped);
        if r.u32()? != 1 {
            bail!("NTLM: the server's sealed message has an unknown signature version");
        }
        let mut checksum: [u8; 8] = r.array()?;
        if r.u32()? != self.recv_seq {
            bail!("NTLM: the server's sealed message is out of sequence");
        }
        let mut plain = r.take_rest().to_vec();
        self.server_seal.apply_keystream(&mut plain);
        self.server_seal.apply_keystream(&mut checksum);
        let expected = hmac_md5(&self.server_signing_key, &[&self.recv_seq.to_le_bytes(), &plain]);
        if checksum != expected[..8] {
            bail!("NTLM: the server's sealed message failed its integrity check");
        }
        self.recv_seq = self.recv_seq.wrapping_add(1);
        Ok(plain)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::util::hex;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // MS-NLMP 4.2.4: NTLMv2 authentication.
    const SERVER_CHALLENGE: [u8; 8] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
    const CLIENT_CHALLENGE: [u8; 8] = [0xaa; 8];
    const SESSION_KEY: [u8; 16] = [0x55; 16];
    /// MsvAvNbDomainName "Domain", MsvAvNbComputerName "Server", EOL.
    const AV_PAIRS: &str = "02000c0044006f006d00610069006e00 01000c005300650072007600650072000000 0000";

    #[test]
    fn ntowfv2_vector() {
        assert_eq!(hex(&ntowfv2("User", "Domain", "Password")), "0c868a403bfd7a93a3001ef22ef02e3f");
    }

    #[test]
    fn ntlmv2_response_vector() {
        let key = ntowfv2("User", "Domain", "Password");
        // temp: header, time 0, client challenge, reserved, AV pairs, reserved.
        let temp = unhex(&format!("0101000000000000 0000000000000000 aaaaaaaaaaaaaaaa 00000000 {AV_PAIRS} 00000000"));
        let (proof, base) = nt_proof(&key, &SERVER_CHALLENGE, &temp);
        assert_eq!(hex(&proof), "68cd0ab851e51c96aabc927bebef6a1c");
        assert_eq!(hex(&base), "8de40ccadbc14a82f15cb0ad0de95ca3");
        let lm = hmac_md5(&key, &[&SERVER_CHALLENGE, &CLIENT_CHALLENGE]);
        assert_eq!(hex(&lm), "86c35097ac9cec102554764a57cccc19");
        let mut enc = SESSION_KEY;
        rc4(&base).apply_keystream(&mut enc);
        assert_eq!(hex(&enc), "c5dad2544fc9799094ce1ce90bc9d03e");
    }

    #[test]
    fn seal_vector() {
        // MS-NLMP 4.2.4.4: GSS_WrapEx of "Plaintext", sequence 0.
        let mut ctx = Ntlm::new(&SESSION_KEY);
        assert_eq!(hex(&ctx.client_signing_key), "4788dc861b4782f35d43fd98fe1a2d39");
        let wrapped = ctx.wrap(&crate::text::to_utf16le("Plaintext", false));
        assert_eq!(hex(&wrapped[16..]), "54e50165bf1936dc996020c1811b0f06fb5f");
        assert_eq!(hex(&wrapped[..16]), "010000007fb38ec5c55d497600000000");
        assert_eq!(ctx.send_seq, 1);
    }

    /// A CHALLENGE message like MS-NLMP 4.2.4.3's.
    pub(crate) fn challenge_message() -> Vec<u8> {
        let target = utf16("Domain");
        let info = unhex(AV_PAIRS);
        let mut w = Writer::new();
        w.bytes(SIGNATURE).u32(2);
        put_buffer(&mut w, target.len(), 56);
        w.u32(0xe28a8233).bytes(&SERVER_CHALLENGE).zeros(8);
        put_buffer(&mut w, info.len(), 56 + target.len());
        put_version(&mut w);
        w.bytes(&target).bytes(&info);
        w.into_vec()
    }

    #[test]
    fn authenticate_message() {
        let neg = negotiate();
        assert_eq!(neg.len(), 40);
        let chal = challenge_message();
        let c = parse_challenge(&chal).unwrap();
        assert_eq!(c.target_name, "Domain");
        assert_eq!(c.server_challenge, SERVER_CHALLENGE);
        let inputs = Inputs {
            user: "User",
            domain: "",
            password: "Password",
            workstation: "COMPUTER",
            client_challenge: CLIENT_CHALLENGE,
            session_key: SESSION_KEY,
            timestamp: 0,
        };
        let (msg, _) = build_authenticate(&neg, &chal, &c, &inputs).unwrap();
        let mut r = Reader::new(&msg);
        assert_eq!(r.bytes(8).unwrap(), SIGNATURE);
        assert_eq!(r.u32().unwrap(), 3);
        let lm = read_buffer(&mut r).unwrap();
        let nt = read_buffer(&mut r).unwrap();
        assert_eq!(read_buffer(&mut r).unwrap(), utf16("Domain"), "empty domain takes the server's");
        assert_eq!(read_buffer(&mut r).unwrap(), utf16("User"));
        assert_eq!(read_buffer(&mut r).unwrap(), utf16("COMPUTER"));
        let encrypted_key = read_buffer(&mut r).unwrap();
        assert_eq!(hex(lm), "86c35097ac9cec102554764a57cccc19aaaaaaaaaaaaaaaa");
        // The NT response proves the blob that follows it.
        let key = ntowfv2("User", "Domain", "Password");
        let (proof, base) = nt_proof(&key, &SERVER_CHALLENGE, &nt[16..]);
        assert_eq!(nt[..16], proof);
        // The session key, encrypted with the base key that proof gives.
        let mut session_key = [0u8; 16];
        session_key.copy_from_slice(encrypted_key);
        rc4(&base).apply_keystream(&mut session_key);
        assert_eq!(session_key, SESSION_KEY);
        // The MIC covers all three messages with the MIC zeroed.
        let mut zeroed = msg.clone();
        zeroed[MIC_OFFSET..MIC_OFFSET + 16].fill(0);
        assert_eq!(msg[MIC_OFFSET..MIC_OFFSET + 16], hmac_md5(&SESSION_KEY, &[&neg, &chal, &zeroed]));
        // MsvAvFlags with MIC present, our target name.
        let pairs = av_pairs(&nt[16 + 28..]);
        assert!(pairs.contains(&(AV_FLAGS, &2u32.to_le_bytes()[..])));
        assert!(pairs.contains(&(AV_TARGET_NAME, &utf16("TERMSRV")[..])));
    }

    #[test]
    fn wrap_unwrap() {
        // The server side of a context is the client side with keys swapped.
        let mut client = Ntlm::new(&SESSION_KEY);
        let mut server = Ntlm::server(&SESSION_KEY);
        for msg in [&b"first"[..], b"", b"third message"] {
            let sealed = server.wrap(msg);
            assert_eq!(client.unwrap(&sealed).unwrap(), msg);
        }
        let mut bad = server.wrap(b"tampered");
        bad[20] ^= 1;
        assert!(client.unwrap(&bad).is_err());
        assert!(client.unwrap(&[1, 0, 0]).is_err());
    }

    #[test]
    fn hostile_challenges() {
        let good = challenge_message();
        for n in 0..good.len() {
            let _ = parse_challenge(&good[..n]);
        }
        let mut huge = good.clone();
        huge[12..14].copy_from_slice(&0xffffu16.to_le_bytes());
        huge[14..16].copy_from_slice(&0xffffu16.to_le_bytes());
        assert!(parse_challenge(&huge).is_err());
        let mut far = good.clone();
        far[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_challenge(&far).is_err());
        assert_eq!(av_pairs(&[2, 0, 0xff, 0xff, 1]).len(), 0);
    }
}
