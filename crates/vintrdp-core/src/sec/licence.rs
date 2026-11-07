//! Licensing (MS-RDPELE): new licence requests, platform challenges, storing
//! the licence the server issues and presenting it next time. Port of
//! rdesktop's licence.c and glue.rs's licence storage.

use std::path::PathBuf;

use super::{MODULUS_SIZE, PADDING_SIZE, RANDOM_SIZE, hash_16, hash_48, hash_sha1_16, rc4_16, sign};
use crate::bail;
use crate::error::Result;
use crate::stream::{Reader, Writer};
use crate::{text, util};

pub struct Info {
    /// The client hostname (as sent in CS_CORE), which names the licence file.
    pub hostname: String,
    pub username: String,
    /// A licence file the user imported, presented before any stored one.
    pub imported: Option<PathBuf>,
    /// 4 or 5.
    pub rdp_version: u8,
}

/// Where licensing stands after a PDU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// More licensing PDUs to come.
    Pending,
    /// Licensing is over; the session goes ahead.
    Done,
    /// The server refused the client a licence (rdesktop's
    /// g_licence_error_result): the caller may retry with RDP 4.
    Refused,
}

/// bMsgType in the licensing preamble.
const LICENSE_REQUEST: u8 = 0x01;
const PLATFORM_CHALLENGE: u8 = 0x02;
const NEW_LICENSE: u8 = 0x03;
const UPGRADE_LICENSE: u8 = 0x04;
const LICENSE_INFO: u8 = 0x12;
const NEW_LICENSE_REQUEST: u8 = 0x13;
const PLATFORM_CHALLENGE_RESPONSE: u8 = 0x15;
const ERROR_ALERT: u8 = 0xff;

/// The error alert that means all is well and no licence is coming.
const STATUS_VALID_CLIENT: u32 = 0x07;

const BB_CLIENT_USER_NAME_BLOB: u16 = 0x000f;
const BB_CLIENT_MACHINE_NAME_BLOB: u16 = 0x0010;

const TOKEN_SIZE: usize = 10;
const HWID_SIZE: usize = 20;
const SIGNATURE_SIZE: usize = 16;

/// KEY_EXCHANGE_ALG_RSA.
const KEY_EXCHANGE_RSA: u32 = 1;
/// PlatformId: rdesktop's 0x0201 in the top half, with 0xff01 for new
/// licence requests and 0x0201 for licence info, as it has always sent.
const PLATFORM_NEW_REQUEST: u32 = 0xff01_0000;
const PLATFORM_INFO: u32 = 0x0201_0000;

/// Where licences are read from and written to.
struct Store {
    /// The user's imported licence, then the per-host cache, then
    /// rdesktop's old ~/.rdesktop/license.<host>.
    read: Vec<PathBuf>,
    /// The per-host cache, where an issued licence goes.
    write: Option<PathBuf>,
}

pub struct Licence {
    info: Info,
    store: Store,
    /// The client hostname as rdesktop's g_hostname: Windows-1252, at most
    /// 15 bytes.
    host: Vec<u8>,
    sign_key: [u8; 16],
    licence_key: [u8; 16],
}

impl Licence {
    pub fn new(info: Info) -> Self {
        let mut host = text::to_cp1252(&info.hostname);
        host.truncate(15);
        let cache = licence_path(&host);
        let legacy = util::rdesktop_dir().map(|d| d.join(format!("license.{}", text::from_cp1252(&host))));
        let read = info.imported.clone().into_iter().chain(cache.clone()).chain(legacy).collect();
        Licence { info, store: Store { read, write: cache }, host, sign_key: [0; 16], licence_key: [0; 16] }
    }

    /// A licensing PDU from the server: everything after the basic security
    /// header (which had SEC_LICENSE_PKT). Pushes the payloads of any replies
    /// onto `out`; each goes to the server with SEC_LICENSE_PKT and no
    /// encryption, the caller adds the header.
    pub fn process(&mut self, r: &mut Reader, out: &mut Vec<Vec<u8>>) -> Result<State> {
        let tag = r.u8()?;
        r.skip(3)?; // version, length
        match tag {
            LICENSE_REQUEST => {
                out.push(self.process_request(r)?);
                Ok(State::Pending)
            }
            PLATFORM_CHALLENGE => {
                out.push(self.process_platform_challenge(r)?);
                Ok(State::Pending)
            }
            NEW_LICENSE | UPGRADE_LICENSE => self.process_new_licence(r),
            ERROR_ALERT => process_error_alert(r),
            _ => {
                eprintln!("vintrdp: unhandled licensing PDU {tag:#04x}");
                Ok(State::Pending)
            }
        }
    }

    /// The preamble version: 2 for RDP 4 (NT 4.0 TSE), 3 for RDP 5.
    fn preamble_version(&self) -> u8 {
        if self.info.rdp_version >= 5 { 3 } else { 2 }
    }

    /// A reply PDU: the preamble, with its length, then `body`.
    fn pdu(&self, tag: u8, body: &Writer) -> Vec<u8> {
        let mut w = Writer::with_capacity(body.len() + 4);
        w.u8(tag).u8(self.preamble_version()).u16((body.len() + 4) as u16).bytes(body.as_slice());
        w.into_vec()
    }

    /// Licensing keys from the randoms (licence_generate_keys). rdesktop
    /// uses a null client random and premaster secret, so nothing needs
    /// encrypting with the server's key; servers accept that.
    fn generate_keys(&mut self, client_random: &[u8; 32], server_random: &[u8; 32], pre_master: &[u8; 48]) {
        let master = hash_48(pre_master, client_random, server_random, b'A');
        let block = hash_48(&master, server_random, client_random, b'A');
        self.sign_key.copy_from_slice(&block[..16]);
        self.licence_key = hash_16(&block[16..32], client_random, server_random);
    }

    /// The hardware ID: PLATFORMID 2 and the hostname, NUL-padded.
    fn hwid(&self) -> [u8; HWID_SIZE] {
        let mut hwid = [0u8; HWID_SIZE];
        hwid[..4].copy_from_slice(&2u32.to_le_bytes());
        hwid[4..4 + self.host.len()].copy_from_slice(&self.host);
        hwid
    }

    /// A licence request: present a stored licence if there is one,
    /// otherwise ask for a new one.
    fn process_request(&mut self, r: &mut Reader) -> Result<Vec<u8>> {
        let server_random: [u8; RANDOM_SIZE] = r.array()?;
        let null = [0u8; 48];
        self.generate_keys(&[0; 32], &server_random, &null);

        let mut w = Writer::new();
        w.u32(KEY_EXCHANGE_RSA);
        if let Some(licence) = self.load() {
            let mut hwid = self.hwid();
            let signature = sign(&self.sign_key, &hwid);
            rc4_16(&self.licence_key, &mut hwid);
            w.u32(PLATFORM_INFO);
            client_random_and_secret(&mut w);
            w.u16(1).u16(licence.len() as u16).bytes(&licence);
            w.u16(1).u16(HWID_SIZE as u16).bytes(&hwid);
            w.bytes(&signature);
            Ok(self.pdu(LICENSE_INFO, &w))
        } else {
            let mut user = text::to_cp1252(&self.info.username);
            user.push(0);
            let mut host = self.host.clone();
            host.push(0);
            w.u32(PLATFORM_NEW_REQUEST);
            client_random_and_secret(&mut w);
            w.u16(BB_CLIENT_USER_NAME_BLOB).u16(user.len() as u16).bytes(&user);
            w.u16(BB_CLIENT_MACHINE_NAME_BLOB).u16(host.len() as u16).bytes(&host);
            Ok(self.pdu(NEW_LICENSE_REQUEST, &w))
        }
    }

    /// A platform challenge: decrypt the token, sign it with the hardware
    /// ID, and send both back with the token as it came and the HWID
    /// encrypted.
    fn process_platform_challenge(&mut self, r: &mut Reader) -> Result<Vec<u8>> {
        r.skip(6)?; // ConnectFlags, wBlobType
        let len = r.u16()? as usize;
        if len != TOKEN_SIZE {
            bail!("licence platform challenge token is {len} bytes, expected {TOKEN_SIZE}");
        }
        let token: [u8; TOKEN_SIZE] = r.array()?;
        r.skip(SIGNATURE_SIZE)?;

        let mut sealed = [0u8; TOKEN_SIZE + HWID_SIZE];
        sealed[..TOKEN_SIZE].copy_from_slice(&token);
        rc4_16(&self.licence_key, &mut sealed[..TOKEN_SIZE]); // "TEST" in UTF-16
        let mut hwid = self.hwid();
        sealed[TOKEN_SIZE..].copy_from_slice(&hwid);
        let signature = sign(&self.sign_key, &sealed);
        rc4_16(&self.licence_key, &mut hwid);

        let mut w = Writer::new();
        w.u16(1).u16(TOKEN_SIZE as u16).bytes(&token);
        w.u16(1).u16(HWID_SIZE as u16).bytes(&hwid);
        w.bytes(&signature);
        Ok(self.pdu(PLATFORM_CHALLENGE_RESPONSE, &w))
    }

    /// A new or upgraded licence: decrypt it and keep the licence info,
    /// which is what goes back to the server next time. One that doesn't
    /// parse is passed over, as rdesktop does, and licensing goes on.
    fn process_new_licence(&mut self, r: &mut Reader) -> Result<State> {
        match self.read_new_licence(r) {
            Ok(licence) => {
                self.save(&licence);
                Ok(State::Done)
            }
            Err(e) => {
                eprintln!("vintrdp: bad licence from the server: {e}");
                Ok(State::Pending)
            }
        }
    }

    fn read_new_licence(&self, r: &mut Reader) -> Result<Vec<u8>> {
        r.skip(2)?; // wBlobType
        let len = r.u16()? as usize;
        let mut data = r.bytes(len)?.to_vec();
        rc4_16(&self.licence_key, &mut data);

        // NEW_LICENSE_INFO: dwVersion, then Scope, CompanyName and
        // ProductId to skip, then the licence itself, each length-prefixed.
        let mut d = Reader::new(&data);
        d.skip(4)?;
        for _ in 0..3 {
            let n = d.u32()? as usize;
            d.skip(n)?;
        }
        let n = d.u32()? as usize;
        Ok(d.bytes(n)?.to_vec())
    }

    /// The first stored licence that can be read, if any.
    fn load(&self) -> Option<Vec<u8>> {
        self.store.read.iter().find_map(|p| std::fs::read(p).ok()).filter(|l| !l.is_empty() && l.len() <= u16::MAX as usize)
    }

    fn save(&self, licence: &[u8]) {
        let Some(path) = &self.store.write else { return };
        if !path.parent().is_some_and(util::mkdirs) {
            eprintln!("vintrdp: could not create the licence directory");
            return;
        }
        if let Err(e) = util::write_atomic(path, licence) {
            eprintln!("vintrdp: could not save the licence to {}: {e}", path.display());
        }
    }
}

/// The null client random and the null "encrypted" premaster secret that
/// rdesktop sends in both licence info and new licence requests: 64 bytes
/// of key and 8 of padding, whatever the server's key size.
fn client_random_and_secret(w: &mut Writer) {
    w.zeros(RANDOM_SIZE);
    w.u16(2).u16((MODULUS_SIZE + PADDING_SIZE) as u16).zeros(MODULUS_SIZE + PADDING_SIZE);
}

/// An error alert: "valid client" ends licensing well; anything else is a
/// refusal.
fn process_error_alert(r: &mut Reader) -> Result<State> {
    let code = r.u32()?;
    let transition = r.u32()?;
    if code == STATUS_VALID_CLIENT {
        return Ok(State::Done);
    }
    let what = match code {
        0x06 => "no licence server".to_string(),
        0x08 => "invalid client".to_string(),
        _ => format!("code {code}, state transition {transition}"),
    };
    eprintln!("vintrdp: licence error from the server: {what}");
    Ok(State::Refused)
}

/// The per-host licence file rdesktop uses:
/// `licenses/<sha1(hostname padded to 16, salt)>.cal` in its data directory.
fn licence_path(host: &[u8]) -> Option<PathBuf> {
    let mut padded = [0u8; 16];
    let n = host.len().min(15);
    padded[..n].copy_from_slice(&host[..n]);
    let hash = hash_sha1_16(&padded, &util::RDESKTOP_SALT);
    Some(util::data_dir()?.join("licenses").join(format!("{}.cal", util::hex(&hash))))
}

#[cfg(test)]
mod tests {
    //! Expected values computed independently in Python (hashlib and a
    //! textbook RC4).

    use super::*;

    const SERVER_RANDOM: [u8; 32] = {
        let mut r = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            r[i] = 0xa0 + i as u8;
            i += 1;
        }
        r
    };
    const SIGN_KEY: [u8; 16] = [0x4a, 0xba, 0xfe, 0xba, 0x3e, 0x7b, 0x51, 0x4c, 0x61, 0xed, 0xe2, 0x83, 0x13, 0x06, 0x70, 0x5d];
    const LICENCE_KEY: [u8; 16] = [0x5b, 0x45, 0xb8, 0x9c, 0x9e, 0xe2, 0x71, 0x71, 0x20, 0xd6, 0x1d, 0xdf, 0x4e, 0xcc, 0x23, 0xb4];
    /// The HWID for VINTAGE-PC, encrypted, and its signature.
    const HWID: [u8; 20] = [
        0x60, 0x00, 0x33, 0x6b, 0x85, 0x40, 0x92, 0xe7, 0x50, 0x72, 0x36, 0x02, 0x23, 0x42, 0x7d, 0x62, 0x96, 0xbd, 0x46, 0x52,
    ];
    const HWID_SIG: [u8; 16] = [0x03, 0x47, 0xe6, 0x41, 0x5f, 0xf5, 0xeb, 0x98, 0xf4, 0xcd, 0xa1, 0x43, 0x82, 0xb4, 0x05, 0x6b];
    /// "TEST" in UTF-16 with a NUL, encrypted, and the signature over it
    /// (decrypted) and the HWID.
    const TOKEN: [u8; 10] = [0x36, 0x00, 0x76, 0x6b, 0x80, 0x09, 0x88, 0xb3, 0x11, 0x35];
    const CHALLENGE_SIG: [u8; 16] = [0x15, 0xd5, 0xea, 0xa9, 0xa5, 0xe7, 0x15, 0x91, 0x85, 0x89, 0x1a, 0x4a, 0xf0, 0xbc, 0x8f, 0x90];
    /// NEW_LICENSE_INFO, encrypted: version 0x30000, scope "microsoft.com",
    /// company "Microsoft Corporation", product "A02", and 37 bytes of 0x99.
    const NEW_LICENCE: [u8; 97] = [
        0x62, 0x00, 0x30, 0x6b, 0xdd, 0x09, 0xdc, 0xb3, 0x7c, 0x5c, 0x10, 0x5d, 0x1c, 0x72, 0x12, 0x04, 0xe2, 0x93, 0x25, 0x3d, 0x26, 0xe7,
        0x7f, 0x16, 0x63, 0xda, 0xb2, 0x33, 0x8a, 0xcc, 0x98, 0x7e, 0xcd, 0x4f, 0xf9, 0xbb, 0x7f, 0xf8, 0xc8, 0xec, 0xc6, 0x4a, 0x00, 0x76,
        0x4f, 0x1e, 0x97, 0x11, 0xbf, 0xfe, 0xbb, 0xd8, 0x50, 0xc1, 0x88, 0x3b, 0x7a, 0x6a, 0x33, 0x2f, 0x12, 0xd3, 0x9a, 0x7e, 0x83, 0xb0,
        0x83, 0x9b, 0xc9, 0x52, 0x58, 0x45, 0xf1, 0xd8, 0x8d, 0x95, 0x94, 0xcd, 0xe5, 0xf7, 0x79, 0x9c, 0x5d, 0x14, 0x51, 0x7b, 0x08, 0xc7,
        0xfa, 0xab, 0x55, 0xca, 0xd0, 0x1d, 0x67, 0xc0, 0xb2,
    ];

    /// A scratch directory of the test's own.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vintrdp-licence-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn licence(rdp_version: u8, dir: &std::path::Path) -> Licence {
        let info = Info { hostname: "VINTAGE-PC".into(), username: "dani".into(), imported: None, rdp_version };
        let mut l = Licence::new(info);
        l.store = Store { read: vec![dir.join("imported.lic"), dir.join("cache.cal")], write: Some(dir.join("sub/cache.cal")) };
        l
    }

    /// A server PDU: the preamble (RDP 5's version) and `body`.
    fn server_pdu(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(tag).u8(3).u16(body.len() as u16 + 4).bytes(body);
        w.into_vec()
    }

    fn licence_request() -> Vec<u8> {
        let mut w = Writer::new();
        w.bytes(&SERVER_RANDOM).bytes(b"product info and key exchange list, unread");
        server_pdu(LICENSE_REQUEST, w.as_slice())
    }

    fn run(l: &mut Licence, pdu: &[u8]) -> (State, Vec<Vec<u8>>) {
        let mut out = Vec::new();
        let state = l.process(&mut Reader::new(pdu), &mut out).unwrap();
        (state, out)
    }

    /// The fields every client licensing PDU of these two kinds starts with.
    fn check_preamble_and_randoms(pdu: &[u8], tag: u8, version: u8, platform: u32) -> Reader<'_> {
        let mut r = Reader::new(pdu);
        assert_eq!(r.u8().unwrap(), tag);
        assert_eq!(r.u8().unwrap(), version);
        assert_eq!(r.u16().unwrap() as usize, pdu.len());
        assert_eq!(r.u32().unwrap(), KEY_EXCHANGE_RSA);
        assert_eq!(r.u32().unwrap(), platform);
        assert_eq!(r.bytes(32).unwrap(), [0; 32]);
        assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (2, 72));
        assert_eq!(r.bytes(72).unwrap(), [0; 72]);
        r
    }

    #[test]
    fn new_licence_request_rdp4_and_rdp5() {
        let dir = scratch("request");
        for (rdp_version, preamble) in [(4, 2), (5, 3)] {
            let mut l = licence(rdp_version, &dir);
            let (state, out) = run(&mut l, &licence_request());
            assert_eq!(state, State::Pending);
            assert_eq!((l.sign_key, l.licence_key), (SIGN_KEY, LICENCE_KEY));
            let mut r = check_preamble_and_randoms(&out[0], NEW_LICENSE_REQUEST, preamble, PLATFORM_NEW_REQUEST);
            assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (BB_CLIENT_USER_NAME_BLOB, 5));
            assert_eq!(r.bytes(5).unwrap(), b"dani\0");
            assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (BB_CLIENT_MACHINE_NAME_BLOB, 11));
            assert_eq!(r.bytes(11).unwrap(), b"VINTAGE-PC\0");
            assert!(r.is_empty());
            assert_eq!(out[0].len(), 128 + 5 + 11); // rdesktop's length
        }
    }

    #[test]
    fn presents_a_stored_licence() {
        let dir = scratch("present");
        std::fs::write(dir.join("cache.cal"), b"the stored licence").unwrap();
        let mut l = licence(5, &dir);
        let (_, out) = run(&mut l, &licence_request());
        let mut r = check_preamble_and_randoms(&out[0], LICENSE_INFO, 3, PLATFORM_INFO);
        assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (1, 18));
        assert_eq!(r.bytes(18).unwrap(), b"the stored licence");
        assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (1, 20));
        assert_eq!(r.bytes(20).unwrap(), HWID);
        assert_eq!(r.bytes(16).unwrap(), HWID_SIG);
        assert!(r.is_empty());

        // The imported licence goes first.
        std::fs::write(dir.join("imported.lic"), b"imported").unwrap();
        let (_, out) = run(&mut l, &licence_request());
        assert_eq!(&out[0][124..132], b"imported");
    }

    #[test]
    fn platform_challenge_new_licence_and_storage() {
        let dir = scratch("challenge");
        let mut l = licence(4, &dir);
        run(&mut l, &licence_request());

        let mut w = Writer::new();
        w.u32(0).u16(0x0009).u16(10).bytes(&TOKEN).bytes(&[0xee; 16]);
        let (state, out) = run(&mut l, &server_pdu(PLATFORM_CHALLENGE, w.as_slice()));
        assert_eq!(state, State::Pending);
        let mut r = Reader::new(&out[0]);
        assert_eq!((r.u8().unwrap(), r.u8().unwrap(), r.u16().unwrap()), (PLATFORM_CHALLENGE_RESPONSE, 2, 58));
        assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (1, 10));
        assert_eq!(r.bytes(10).unwrap(), TOKEN);
        assert_eq!((r.u16().unwrap(), r.u16().unwrap()), (1, 20));
        assert_eq!(r.bytes(20).unwrap(), HWID);
        assert_eq!(r.bytes(16).unwrap(), CHALLENGE_SIG);
        assert!(r.is_empty());

        let mut w = Writer::new();
        w.u16(0x0009).u16(NEW_LICENCE.len() as u16).bytes(&NEW_LICENCE).bytes(&[0xee; 16]);
        let (state, out) = run(&mut l, &server_pdu(NEW_LICENSE, w.as_slice()));
        assert_eq!(state, State::Done);
        assert!(out.is_empty());
        assert_eq!(std::fs::read(dir.join("sub/cache.cal")).unwrap(), [0x99; 37]);

        // A token of the wrong size is an error, not a panic.
        let mut w = Writer::new();
        w.u32(0).u16(0x0009).u16(9).bytes(&TOKEN);
        assert!(l.process(&mut Reader::new(&server_pdu(PLATFORM_CHALLENGE, w.as_slice())), &mut Vec::new()).is_err());
        // A licence that doesn't decrypt to anything sensible is passed over.
        let mut w = Writer::new();
        w.u16(0x0009).u16(8).bytes(&[0x42; 8]);
        assert_eq!(run(&mut l, &server_pdu(UPGRADE_LICENSE, w.as_slice())).0, State::Pending);
    }

    #[test]
    fn error_alerts() {
        let dir = scratch("alerts");
        let mut l = licence(5, &dir);
        let alert = |code: u32| {
            let mut w = Writer::new();
            w.u32(code).u32(2).u16(4).u16(0);
            server_pdu(ERROR_ALERT, w.as_slice())
        };
        assert_eq!(run(&mut l, &alert(STATUS_VALID_CLIENT)), (State::Done, vec![]));
        assert_eq!(run(&mut l, &alert(0x08)).0, State::Refused);
        assert_eq!(run(&mut l, &alert(0x06)).0, State::Refused);
        assert_eq!(run(&mut l, &server_pdu(0x42, &[])).0, State::Pending);
    }

    #[test]
    fn licence_file_name() {
        let path = licence_path(b"VINTAGE-PC");
        if let Some(p) = path {
            assert!(p.ends_with("licenses/3838c63e0b3b84165d81ecde18b551929f41c58a.cal"));
        }
    }

    #[test]
    fn truncated_pdus_never_panic() {
        let dir = scratch("truncated");
        let mut l = licence(5, &dir);
        run(&mut l, &licence_request());
        let mut w = Writer::new();
        w.u16(0x0009).u16(NEW_LICENCE.len() as u16).bytes(&NEW_LICENCE);
        let pdus = [licence_request(), server_pdu(PLATFORM_CHALLENGE, &[0; 32]), server_pdu(NEW_LICENSE, w.as_slice()), server_pdu(ERROR_ALERT, &[7, 0, 0, 0, 2, 0, 0, 0])];
        for pdu in &pdus {
            for n in 0..pdu.len() {
                let _ = l.process(&mut Reader::new(&pdu[..n]), &mut Vec::new());
            }
        }
    }
}
