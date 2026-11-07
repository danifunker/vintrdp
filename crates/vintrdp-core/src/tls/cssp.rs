//! CredSSP (MS-CSSP) as the client, with the built-in NTLMv2: TSRequest
//! messages over TLS carry the NTLM exchange, bind it to the server's TLS
//! public key, then deliver the user's password (TSCredentials). No
//! GSSAPI/Kerberos, as in vintrdp's rdesktop. Port of rdesktop's cssp.c.

use super::TlsStream;
use super::ntlm;
use super::sha256::sha256;
use super::spnego;
use crate::bail;
use crate::der::{self, Der};
use crate::error::{Error, Result};

/// The version we speak: 6 binds the public key with SHA-256 and a nonce
/// (from version 5), and lets the server report errors (from version 3).
const CLIENT_VERSION: u32 = 6;
const NONCE_SIZE: usize = 32;
/// More than any TSRequest an NTLM exchange needs; a bound on what a hostile
/// server can make us buffer.
const MAX_TSREQUEST: usize = 1 << 20;

const CLIENT_BINDING_MAGIC: &[u8] = b"CredSSP Client-To-Server Binding Hash\0";
const SERVER_BINDING_MAGIC: &[u8] = b"CredSSP Server-To-Client Binding Hash\0";

/// A TSRequest. Absent fields are `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TsRequest {
    pub version: u32,
    /// The first negoTokens entry.
    pub nego_token: Option<Vec<u8>>,
    pub auth_info: Option<Vec<u8>>,
    pub pub_key_auth: Option<Vec<u8>>,
    /// An NTSTATUS from the server (version 3 and later).
    pub error_code: Option<u32>,
    pub client_nonce: Option<Vec<u8>>,
}

impl TsRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut fields = vec![der::explicit(0, &der::uint(self.version))];
        if let Some(t) = &self.nego_token {
            let nego_data = der::sequence(&[&der::explicit(0, &der::octet_string(t))]);
            fields.push(der::explicit(1, &der::sequence(&[&nego_data])));
        }
        if let Some(a) = &self.auth_info {
            fields.push(der::explicit(2, &der::octet_string(a)));
        }
        if let Some(p) = &self.pub_key_auth {
            fields.push(der::explicit(3, &der::octet_string(p)));
        }
        if let Some(e) = self.error_code {
            fields.push(der::explicit(4, &der::uint(e)));
        }
        if let Some(n) = &self.client_nonce {
            fields.push(der::explicit(5, &der::octet_string(n)));
        }
        let parts: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
        der::sequence(&parts)
    }

    /// Fields we don't know are skipped.
    pub fn decode(data: &[u8]) -> Result<TsRequest> {
        let seq = Der::new(data).next(der::SEQUENCE)?;
        let mut fields = seq.children();
        let mut req = TsRequest::default();
        while !fields.is_empty() {
            let f = fields.next_any()?;
            let mut inner = f.children();
            match f.tag {
                t if t == der::context(0) => req.version = inner.next(der::INTEGER)?.uint()?,
                t if t == der::context(1) => {
                    let mut tokens = inner.next(der::SEQUENCE)?.children();
                    if let Some(first) = tokens.optional(der::SEQUENCE)? {
                        let token = first.children().next(der::context(0))?.children().next(der::OCTET_STRING)?;
                        req.nego_token = Some(token.value.to_vec());
                    }
                }
                t if t == der::context(2) => req.auth_info = Some(inner.next(der::OCTET_STRING)?.value.to_vec()),
                t if t == der::context(3) => req.pub_key_auth = Some(inner.next(der::OCTET_STRING)?.value.to_vec()),
                t if t == der::context(4) => req.error_code = Some(inner.next(der::INTEGER)?.uint()?),
                t if t == der::context(5) => req.client_nonce = Some(inner.next(der::OCTET_STRING)?.value.to_vec()),
                _ => {}
            }
        }
        Ok(req)
    }
}

/// UTF-16LE with the terminating NUL, as rdesktop's out_utf16s writes the
/// credentials (Windows takes them either way).
fn utf16z(s: &str) -> Vec<u8> {
    crate::text::to_utf16le(s, true)
}

/// TSCredentials holding TSPasswordCreds.
pub fn ts_credentials(user: &str, domain: &str, password: &str) -> Vec<u8> {
    let creds = der::sequence(&[
        &der::explicit(0, &der::octet_string(&utf16z(domain))),
        &der::explicit(1, &der::octet_string(&utf16z(user))),
        &der::explicit(2, &der::octet_string(&utf16z(password))),
    ]);
    // credType 1: password credentials.
    der::sequence(&[&der::explicit(0, &der::uint(1)), &der::explicit(1, &der::octet_string(&creds))])
}

/// What pubKeyAuth proves: the key itself before version 5, a hash of it
/// with the nonce from then on.
fn pubkey_binding(pubkey: &[u8], nonce: &[u8], version: u32, client_to_server: bool) -> Vec<u8> {
    if version < 5 {
        return pubkey.to_vec();
    }
    let magic = if client_to_server { CLIENT_BINDING_MAGIC } else { SERVER_BINDING_MAGIC };
    sha256(&[magic, nonce, pubkey]).to_vec()
}

/// The server proves it holds the key: its hash, or before version 5 the key
/// with the first byte plus one.
fn server_binding_ok(pubkey: &[u8], nonce: &[u8], version: u32, response: &[u8]) -> bool {
    if version >= 5 {
        return response == pubkey_binding(pubkey, nonce, version, false);
    }
    match response.split_first() {
        Some((first, rest)) => response.len() == pubkey.len() && pubkey[0] == first.wrapping_sub(1) && rest == &pubkey[1..],
        None => false,
    }
}

/// What an NTSTATUS from the server means to the user.
fn status_message(status: u32) -> String {
    let what = match status {
        0xc000_006d | 0xc000_006a | 0xc000_0064 => "the user name or password is incorrect",
        0xc000_006e => "the account is restricted (a blank password, or a policy)",
        0xc000_006f => "the account may not log on at this time of day",
        0xc000_0070 => "the account may not log on from this computer",
        0xc000_0071 => "the password has expired",
        0xc000_0072 => "the account is disabled",
        0xc000_0193 => "the account has expired",
        0xc000_0224 => "the password must be changed before logging on",
        0xc000_0234 => "the account is locked out",
        0xc000_015b => "the user has not been granted the right to log on",
        0xc000_0413 => "the account may not use this authentication (authentication firewall)",
        _ => "",
    };
    if what.is_empty() {
        format!("The server refused the network level authentication (NTSTATUS {status:#010x})")
    } else {
        format!("The server refused the credentials: {what}")
    }
}

/// Read one DER element from `tls`: tag and length first, then exactly the
/// rest, so nothing after it is consumed.
fn read_der(tls: &dyn TlsStream) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; 2];
    read_exact(tls, &mut buf)?;
    let mut len = buf[1] as usize;
    if len & 0x80 != 0 {
        let n = len & 0x7f;
        if n == 0 || n > 4 {
            bail!("CredSSP: bad length in the server's message");
        }
        buf.resize(2 + n, 0);
        read_exact(tls, &mut buf[2..])?;
        len = buf[2..].iter().fold(0usize, |l, &b| (l << 8) | b as usize);
    }
    if len > MAX_TSREQUEST {
        bail!("CredSSP: the server's message is too large ({len} bytes)");
    }
    let start = buf.len();
    buf.resize(start + len, 0);
    read_exact(tls, &mut buf[start..])?;
    Ok(buf)
}

fn read_exact(tls: &dyn TlsStream, mut buf: &mut [u8]) -> Result<()> {
    while !buf.is_empty() {
        match tls.read(buf) {
            Ok(0) => return Err(Error::Io(std::io::ErrorKind::UnexpectedEof.into())),
            Ok(n) => buf = &mut buf[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(())
}

/// Read a TSRequest, failing on an error the server reports. `closed` is
/// what it means if the server hangs up instead (servers before CredSSP 3
/// report a refused logon that way).
fn read_request(tls: &dyn TlsStream, closed: &str) -> Result<TsRequest> {
    let data = read_der(tls).map_err(|e| match e {
        Error::Io(io) if matches!(io.kind(), std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset) => {
            Error::Protocol(closed.to_string())
        }
        e => e,
    })?;
    let req = TsRequest::decode(&data)?;
    if let Some(status) = req.error_code {
        return Err(Error::Protocol(status_message(status)));
    }
    Ok(req)
}

/// See [`super::credssp`].
pub fn connect(tls: &dyn TlsStream, user: &str, domain: &str, password: &str, hostname: &str) -> Result<()> {
    if password.is_empty() {
        bail!("Network level authentication (NLA) needs a password: give one with -p");
    }
    let pubkey = tls.server_public_key()?;
    if pubkey.is_empty() {
        bail!("CredSSP: the server's certificate has an empty public key");
    }
    let mut nonce = [0u8; NONCE_SIZE];
    crate::util::random(&mut nonce);

    // NEGOTIATE, raw: Windows takes NTLM without SPNEGO around it. The nonce
    // goes only to servers known to speak version 5, so not yet.
    let negotiate = ntlm::negotiate();
    let send = |req: TsRequest| tls.write_all(&req.encode());
    send(TsRequest { version: CLIENT_VERSION, nego_token: Some(negotiate.clone()), ..Default::default() })?;

    let reply = read_request(tls, "The server closed the connection at the start of network level authentication")?;
    let version = reply.version.min(CLIENT_VERSION);
    let token = reply.nego_token.ok_or_else(|| Error::protocol("CredSSP: the server sent no NTLM challenge"))?;
    let challenge = spnego::extract_ntlm(&token).ok_or_else(|| Error::protocol("CredSSP: the server's reply holds no NTLM challenge"))?;

    let (authenticate, mut ctx) = ntlm::authenticate(&negotiate, challenge, user, domain, password, hostname)?;
    let nonce_field = (version >= 5).then(|| nonce.to_vec());
    let pub_key_auth = ctx.wrap(&pubkey_binding(&pubkey, &nonce, version, true));
    send(TsRequest {
        version: CLIENT_VERSION,
        nego_token: Some(authenticate),
        pub_key_auth: Some(pub_key_auth),
        client_nonce: nonce_field.clone(),
        ..Default::default()
    })?;

    let reply = read_request(
        tls,
        "The server closed the connection during network level authentication: check the user name, domain and password",
    )?;
    let response = reply.pub_key_auth.ok_or_else(|| Error::protocol("CredSSP: the server did not prove its public key"))?;
    let response = ctx.unwrap(&response)?;
    if !server_binding_ok(&pubkey, &nonce, version, &response) {
        bail!("The server could not prove it holds its certificate's key: the connection may have been intercepted");
    }

    let creds = ctx.wrap(&ts_credentials(user, domain, password));
    send(TsRequest { version: CLIENT_VERSION, auth_info: Some(creds), client_nonce: nonce_field, ..Default::default() })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsrequest_round_trip() {
        let full = TsRequest {
            version: 6,
            nego_token: Some(b"NTLMSSP\0token".to_vec()),
            auth_info: Some(vec![1, 2, 3]),
            pub_key_auth: Some(vec![9; 300]),
            error_code: Some(0xc000_006d),
            client_nonce: Some(vec![7; 32]),
        };
        assert_eq!(TsRequest::decode(&full.encode()).unwrap(), full);
        let bare = TsRequest { version: 2, ..Default::default() };
        assert_eq!(bare.encode(), [0x30, 0x05, 0xa0, 0x03, 0x02, 0x01, 0x02]);
        assert_eq!(TsRequest::decode(&bare.encode()).unwrap(), bare);
    }

    #[test]
    fn tsrequest_layout() {
        // As rdesktop's cssp_send_tsrequest builds it.
        let req = TsRequest { version: 6, nego_token: Some(vec![0xaa, 0xbb]), ..Default::default() };
        assert_eq!(
            req.encode(),
            [0x30, 0x11, 0xa0, 0x03, 0x02, 0x01, 0x06, 0xa1, 0x0a, 0x30, 0x08, 0x30, 0x06, 0xa0, 0x04, 0x04, 0x02, 0xaa, 0xbb]
        );
    }

    #[test]
    fn decodes_windows_errors_and_unknown_fields() {
        // errorCode as Windows sends it, negative; an unknown field [7].
        let data = der::sequence(&[
            &der::explicit(0, &der::uint(6)),
            &der::explicit(7, &der::octet_string(b"?")),
            &der::explicit(4, &der::tlv(der::INTEGER, &[0xc0, 0x00, 0x00, 0x6d])),
        ]);
        let req = TsRequest::decode(&data).unwrap();
        assert_eq!(req.error_code, Some(0xc000_006d));
        assert!(status_message(0xc000_006d).contains("password is incorrect"));
        assert!(status_message(0xc000_9999).contains("0xc0009999"));
    }

    #[test]
    fn hostile_tsrequests() {
        let full = TsRequest { version: 6, nego_token: Some(vec![1; 10]), pub_key_auth: Some(vec![2; 10]), ..Default::default() }.encode();
        for n in 0..full.len() {
            assert!(TsRequest::decode(&full[..n]).is_err());
        }
        assert!(TsRequest::decode(&[0x30, 0x03, 0xa1, 0x01, 0x05]).is_err());
        assert!(TsRequest::decode(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff]).is_err());
        // An empty negoTokens list is no token, not an error.
        let empty = der::sequence(&[&der::explicit(0, &der::uint(2)), &der::explicit(1, &der::sequence(&[]))]);
        assert_eq!(TsRequest::decode(&empty).unwrap().nego_token, None);
    }

    #[test]
    fn credentials_encoding() {
        let c = ts_credentials("u", "", "p");
        let creds =
            [0x30, 0x16, 0xa0, 0x04, 0x04, 0x02, 0, 0, 0xa1, 0x06, 0x04, 0x04, b'u', 0, 0, 0, 0xa2, 0x06, 0x04, 0x04, b'p', 0, 0, 0];
        let mut expect = vec![0x30, 0x21, 0xa0, 0x03, 0x02, 0x01, 0x01, 0xa1, 0x1a, 0x04, 0x18];
        expect.extend(creds);
        assert_eq!(c, expect);
    }

    #[test]
    fn bindings() {
        let key = [0x30, 0x0d, 0x02, 0x01, 0x05];
        let nonce = [3u8; 32];
        assert_eq!(pubkey_binding(&key, &nonce, 2, true), key);
        assert!(server_binding_ok(&key, &nonce, 4, &[0x31, 0x0d, 0x02, 0x01, 0x05]));
        assert!(!server_binding_ok(&key, &nonce, 4, &key));
        assert!(!server_binding_ok(&key, &nonce, 4, &[]));
        let client = pubkey_binding(&key, &nonce, 6, true);
        assert_eq!(client, sha256(&[CLIENT_BINDING_MAGIC, &nonce, &key]));
        let server = pubkey_binding(&key, &nonce, 6, false);
        assert_ne!(client, server);
        assert!(server_binding_ok(&key, &nonce, 6, &server));
        assert!(!server_binding_ok(&key, &nonce, 6, &client));
    }

    /// One end of an in-memory connection.
    struct Pipe {
        rx: std::sync::Mutex<(std::sync::mpsc::Receiver<Vec<u8>>, Vec<u8>)>,
        tx: std::sync::Mutex<std::sync::mpsc::Sender<Vec<u8>>>,
    }

    fn pipe() -> (Pipe, Pipe) {
        let (atx, arx) = std::sync::mpsc::channel();
        let (btx, brx) = std::sync::mpsc::channel();
        let end = |rx, tx| Pipe { rx: std::sync::Mutex::new((rx, Vec::new())), tx: std::sync::Mutex::new(tx) };
        (end(arx, btx), end(brx, atx))
    }

    const PUBKEY: &[u8] = &[0x30, 0x0d, 0x02, 0x06, 1, 2, 3, 4, 5, 6, 0x02, 0x03, 1, 0, 1];

    impl TlsStream for Pipe {
        fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut g = self.rx.lock().unwrap();
            if g.1.is_empty() {
                match g.0.recv() {
                    Ok(data) => g.1 = data,
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(g.1.len());
            buf[..n].copy_from_slice(&g.1[..n]);
            g.1.drain(..n);
            Ok(n)
        }
        fn write_all(&self, buf: &[u8]) -> std::io::Result<()> {
            let _ = self.tx.lock().unwrap().send(buf.to_vec());
            Ok(())
        }
        fn server_public_key(&self) -> Result<Vec<u8>> {
            Ok(PUBKEY.to_vec())
        }
        fn shutdown(&self) {}
    }

    /// A security buffer of an AUTHENTICATE message.
    fn field(msg: &[u8], i: usize) -> &[u8] {
        let at = 12 + 8 * i;
        let len = u16::from_le_bytes([msg[at], msg[at + 1]]) as usize;
        let off = u32::from_le_bytes([msg[at + 4], msg[at + 5], msg[at + 6], msg[at + 7]]) as usize;
        &msg[off..off + len]
    }

    /// A CredSSP server of `version` that knows the password, as far as it
    /// gets before `fail` (an errorCode, or hanging up).
    fn server(end: Pipe, version: u32, password: &str, fail: Option<u32>) -> Option<TsRequest> {
        let first = TsRequest::decode(&read_der(&end).ok()?).unwrap();
        assert_eq!(first.client_nonce, None);
        let chal = super::ntlm::tests::challenge_message();
        let token = if version >= 5 { spnego::wrap_resp(&chal) } else { chal.clone() };
        end.write_all(&TsRequest { version, nego_token: Some(token), ..Default::default() }.encode()).unwrap();

        let auth = TsRequest::decode(&read_der(&end).ok()?).unwrap();
        assert_eq!(auth.client_nonce.is_some(), version >= 5);
        if let Some(code) = fail {
            if code != 0 {
                end.write_all(&TsRequest { version, error_code: Some(code), ..Default::default() }.encode()).unwrap();
            }
            return None;
        }
        let msg = auth.nego_token.unwrap();
        let key = ntlm::ntowfv2("User", "Domain", password);
        let nt = field(&msg, 1);
        let (proof, base) = ntlm::nt_proof(&key, &[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef], &nt[16..]);
        assert_eq!(nt[..16], proof, "wrong password");
        let mut session_key = [0u8; 16];
        session_key.copy_from_slice(field(&msg, 5));
        use rc4::StreamCipher;
        ntlm::rc4(&base).apply_keystream(&mut session_key);
        let mut ctx = ntlm::Ntlm::server(&session_key);

        let nonce = auth.client_nonce.clone().unwrap_or_default();
        let binding = ctx.unwrap(&auth.pub_key_auth.unwrap()).unwrap();
        assert_eq!(binding, pubkey_binding(PUBKEY, &nonce, version, true));
        let reply =
            if version >= 5 { pubkey_binding(PUBKEY, &nonce, version, false) } else { [&[PUBKEY[0] + 1][..], &PUBKEY[1..]].concat() };
        let sealed = ctx.wrap(&reply);
        end.write_all(&TsRequest { version, pub_key_auth: Some(sealed), ..Default::default() }.encode()).unwrap();

        let creds = TsRequest::decode(&read_der(&end).ok()?).unwrap();
        let plain = ctx.unwrap(&creds.auth_info.unwrap()).unwrap();
        Some(TsRequest { auth_info: Some(plain), ..Default::default() })
    }

    fn run(version: u32, password: &str, fail: Option<u32>) -> (Result<()>, Option<TsRequest>) {
        let (client, srv) = pipe();
        let password_srv = password.to_string();
        let t = std::thread::spawn(move || server(srv, version, &password_srv, fail));
        let r = connect(&client, "User", "", password, "WORKSTATION");
        drop(client);
        (r, t.join().unwrap())
    }

    #[test]
    fn end_to_end() {
        for version in [2, 3, 6] {
            let (r, got) = run(version, "Password", None);
            r.unwrap();
            let creds = got.unwrap().auth_info.unwrap();
            assert_eq!(creds, ts_credentials("User", "", "Password"), "version {version}");
        }
    }

    #[test]
    fn refused() {
        let (r, _) = run(6, "Password", Some(0xc000_006d));
        assert!(matches!(r, Err(Error::Protocol(m)) if m.contains("password is incorrect")));
        let (r, _) = run(2, "Password", Some(0));
        assert!(matches!(r, Err(Error::Protocol(m)) if m.contains("check the user name")));
        let (r, _) = run(6, "", None);
        assert!(matches!(r, Err(Error::Protocol(m)) if m.contains("needs a password")));
    }
}
