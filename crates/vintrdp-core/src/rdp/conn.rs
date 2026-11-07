//! One connection to the server: the connection sequence, the security
//! header on everything sent and received, share control and data PDUs,
//! fast-path output, virtual channels and input. Port of rdesktop's rdp.c,
//! rdp5.c and the packet handling in secure.c, iso.c and mcs.c.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::caps;
use super::input::*;
use crate::channels::{self, Ctx, Registered};
use crate::codec::mppc::{Mppc, PACKET_COMPRESSED};
use crate::error::{Error, Result};
use crate::graphics::{Graphics, Params};
use crate::locks::{Locks, TYPE_LOCKS};
use crate::net::gcc::{ClientData, ServerData};
use crate::net::transport::{TPKT_VERSION, Transport};
use crate::net::{gcc, mcs, x224};
use crate::sec::licence::{self, Licence};
use crate::sec::{self, Crypto};
use crate::session::{Event, Shared};
use crate::stream::{Reader, Writer};

const PDU_DEMAND_ACTIVE: u16 = 1;
const PDU_CONFIRM_ACTIVE: u16 = 3;
const PDU_REDIRECT: u16 = 4;
const PDU_DEACTIVATE: u16 = 6;
const PDU_DATA: u16 = 7;
const PDU_ENHANCED_REDIRECT: u16 = 10;

const DATA_UPDATE: u8 = 0x02;
const DATA_CONTROL: u8 = 0x14;
const DATA_POINTER: u8 = 0x1b;
const DATA_INPUT: u8 = 0x1c;
const DATA_SYNCHRONISE: u8 = 0x1f;
const DATA_BELL: u8 = 0x22;
const DATA_LOGON: u8 = 0x26;
const DATA_FONTLIST: u8 = 0x27;
const DATA_SET_ERROR_INFO: u8 = 0x2f;
const DATA_PERSISTENT_KEY_LIST: u8 = 0x2b;

const CTL_REQUEST_CONTROL: u16 = 1;
const CTL_COOPERATE: u16 = 4;

pub const INFO_MOUSE: u32 = 0x1;
pub const INFO_DISABLECTRLALTDEL: u32 = 0x2;
pub const INFO_AUTOLOGON: u32 = 0x8;
pub const INFO_UNICODE: u32 = 0x10;
pub const INFO_MAXIMIZESHELL: u32 = 0x20;
pub const INFO_COMPRESSION: u32 = 0x80;
pub const INFO_ENABLEWINDOWSKEY: u32 = 0x100;
pub const INFO_COMPRESSION2: u32 = 0x200;

const FASTPATH_OUTPUT_ENCRYPTED: u8 = 0x2;
const FASTPATH_FRAGMENT_SINGLE: u8 = 0;
const FASTPATH_FRAGMENT_LAST: u8 = 1;
const FASTPATH_FRAGMENT_FIRST: u8 = 2;
const FASTPATH_COMPRESSION_USED: u8 = 0x2;

/// Graphics draws DrawNineGrid orders, so we offer them as rdesktop did.
const NINE_GRID: bool = true;

/// rdesktop's errinfo codes the session loop needs.
pub const ERRINFO_UNSET: u32 = u32::MAX;

/// An auto-reconnect cookie from the server (ARC_SC_PRIVATE_PACKET).
#[derive(Clone)]
pub struct ReconnectCookie {
    pub logon_id: u32,
    pub random: [u8; 16],
}

/// Where the server sent us (session directory / load balancing).
#[derive(Clone, Debug, Default)]
pub struct Redirect {
    pub session_id: u32,
    pub server: Option<String>,
    pub username: Option<String>,
    pub domain: Option<String>,
    /// A password or cookie to pass back verbatim in the client info.
    pub cookie: Option<Vec<u8>>,
}

/// What a connection carries over to the next one in the same session.
#[derive(Default)]
pub struct Carry {
    pub reconnect: Option<ReconnectCookie>,
    pub redirect: Option<Redirect>,
}

/// What the session loop asks of a connection.
pub struct Settings {
    pub server: String,
    pub username: String,
    pub domain: String,
    pub password: String,
    /// 4 or 5: what we ask for. The server may answer 4.
    pub rdp_version: u8,
    pub depth: u32,
    pub width: u16,
    pub height: u16,
    pub info_flags: u32,
    /// Ask for RDP's own encryption. The server decides: if it picks a
    /// cipher anyway we use it, and if it has none we go without.
    pub encryption: bool,
}

/// How a connection ended.
#[derive(Debug)]
pub enum End {
    /// The connection sequence failed: never activated.
    Failed { message: String, licence_refused: bool, server_rdp4: bool },
    UserQuit,
    /// The connection closed after the session ran; `network` when it was
    /// an error rather than an orderly close.
    Closed { deactivated: bool, ext_disc_reason: u32, network: bool },
    Redirect(Redirect),
    /// The server sent something we couldn't handle.
    Error(String),
}

pub struct Conn<'a> {
    shared: &'a Arc<Shared>,
    graphics: &'a mut Graphics,
    channels: &'a mut Vec<Registered>,
    pub id: u32,
    transport: Transport,
    settings: Settings,
    rdp_version: u8,
    depth: u32,
    user_id: u16,
    io_channel: u16,
    crypto: Option<Crypto>,
    /// Encrypt what we send (standard security, unless `encrypt_login_only`).
    encrypt: bool,
    client_random: [u8; 32],
    licence: Licence,
    /// Licensing is over (issued, or not needed): the security header is
    /// dropped from unencrypted PDUs.
    licensed: bool,
    licence_refused: bool,
    share_id: u32,
    activated: bool,
    deactivated: bool,
    /// Share control PDUs to swallow after confirm active (rdesktop reads
    /// three and ignores them before sending input sync and fonts, then
    /// one more after).
    skip_pdus: u8,
    ext_disc_reason: u32,
    server_input_flags: u16,
    chunk_size: usize,
    mppc: Mppc,
    fragments: Vec<Vec<u8>>,
    locks: Locks,
    started: Instant,
    heartbeat: Option<Heartbeat>,
    carry: &'a mut Carry,
}

struct Heartbeat {
    period: Duration,
    reconnect_after: Duration,
    last: Instant,
}

fn trace(shared: &Shared, msg: impl AsRef<str>) {
    if shared.config.verbose {
        eprintln!("vintrdp: {}", msg.as_ref());
    }
}

/// Read one MCS data PDU during the connection sequence.
fn read_mcs(t: &mut Transport) -> Result<Vec<u8>> {
    let frame = t.read_frame()?;
    Ok(x224::parse_data(&frame)?.to_vec())
}

impl<'a> Conn<'a> {
    /// Run the connection sequence up to sending the client info PDU.
    /// Licensing and capability exchange follow in the main loop.
    pub fn connect(
        shared: &'a Arc<Shared>,
        graphics: &'a mut Graphics,
        channels: &'a mut Vec<Registered>,
        carry: &'a mut Carry,
        id: u32,
        settings: Settings,
    ) -> std::result::Result<Conn<'a>, End> {
        let failed = |e: Error, server_rdp4: bool| End::Failed { message: e.to_string(), licence_refused: false, server_rdp4 };
        let config = &shared.config;
        let rdp5 = settings.rdp_version >= 5;
        let hostname = config.client_hostname();

        // X.224, with TLS/CredSSP where the server wants them.
        #[cfg(feature = "tls")]
        let mut requested = Some(x224::PROTOCOL_SSL | x224::PROTOCOL_HYBRID);
        #[cfg(not(feature = "tls"))]
        let mut requested = Some(x224::PROTOCOL_RDP);
        if !rdp5 {
            requested = None;
        }
        let (mut transport, selected) = loop {
            let mut t = Transport::connect(&settings.server, config.port).map_err(|e| {
                failed(Error::protocol(format!("could not connect to {}: {e}", settings.server)), false)
            })?;
            t.send(&x224::connection_request(&settings.username, requested)).map_err(|e| failed(e.into(), false))?;
            let frame = t.read_frame().map_err(|e| failed(e.into(), false))?;
            match x224::parse_confirm(&frame).map_err(|e| failed(e, false))? {
                x224::Confirm::Legacy => break (t, x224::PROTOCOL_RDP),
                x224::Confirm::Failed(code) => {
                    let reason = x224::failure_reason(code);
                    if matches!(code, x224::SSL_NOT_ALLOWED_BY_SERVER | x224::SSL_CERT_NOT_ON_SERVER) && requested.is_some() {
                        trace(shared, format!("protocol negotiation failed ({reason}); retrying without"));
                        requested = None;
                        continue;
                    }
                    let mut message = format!("protocol negotiation failed: {reason}");
                    if !cfg!(feature = "tls") && matches!(code, x224::SSL_REQUIRED_BY_SERVER | x224::HYBRID_REQUIRED_BY_SERVER) {
                        message.push_str(" (this build of vintrdp has no TLS)");
                    } else if code == x224::HYBRID_REQUIRED_BY_SERVER
                        && config.tls_version.as_deref().is_none_or(|v| v < "1.2")
                    {
                        message.push_str(" (if the server has disabled old TLS versions, choose a newer one)");
                    }
                    return Err(failed(Error::protocol(message), false));
                }
                x224::Confirm::Selected { protocol, .. } => {
                    #[cfg(feature = "tls")]
                    if protocol == x224::PROTOCOL_SSL || protocol == x224::PROTOCOL_HYBRID {
                        let tls = match t.start_tls(&settings.server, shared) {
                            Ok(tls) => tls,
                            Err(Error::Cancelled) => return Err(End::UserQuit),
                            Err(e) => {
                                trace(shared, format!("TLS failed ({e}); trying plain RDP"));
                                requested = Some(x224::PROTOCOL_RDP);
                                continue;
                            }
                        };
                        if protocol == x224::PROTOCOL_HYBRID
                            && let Err(e) = crate::tls::credssp(&*tls, &settings.username, &settings.domain, &settings.password, &hostname)
                        {
                            if matches!(e, Error::Cancelled) {
                                return Err(End::UserQuit);
                            }
                            trace(shared, format!("NLA failed ({e}); trying TLS alone"));
                            requested = Some(x224::PROTOCOL_SSL);
                            continue;
                        }
                        break (t, protocol);
                    }
                    if protocol != x224::PROTOCOL_RDP {
                        return Err(failed(Error::protocol(format!("the server chose an unknown protocol {protocol:#x}")), false));
                    }
                    break (t, protocol);
                }
            }
        };
        // TLS replaces RDP's own encryption.
        let rdp_security = selected == x224::PROTOCOL_RDP;
        let encryption = settings.encryption && rdp_security;

        // MCS connect-initial with the GCC client data.
        let channel_defs: Vec<([u8; 8], u32)> =
            if rdp5 { channels.iter().map(|c| (channels::def_name(c.channel.name()), c.channel.options())).collect() } else { Vec::new() };
        let session_id = carry.redirect.as_ref().map(|r| r.session_id).filter(|&s| s != 0).or(config.console.then_some(0));
        let client = ClientData {
            rdp5,
            width: settings.width,
            height: settings.height,
            depth: settings.depth,
            keyboard_layout: config.keyboard_layout,
            hostname: &hostname,
            selected_protocol: selected,
            encryption,
            session_id,
            channels: channel_defs,
        };
        let step = |t: &mut Transport, data: &[u8]| t.send(&x224::data(data)).map_err(Error::from);
        let result: Result<(ServerData, u16)> = (|| {
            step(&mut transport, &mcs::connect_initial(&client.conference_create_request()))?;
            let resp = read_mcs(&mut transport)?;
            let server = gcc::parse_conference_create_response(mcs::parse_connect_response(&resp)?)?;
            step(&mut transport, &mcs::erect_domain_request())?;
            step(&mut transport, &mcs::attach_user_request())?;
            let user_id = mcs::parse_attach_user_confirm(&read_mcs(&mut transport)?)?;
            let mut join = vec![user_id + mcs::USERCHANNEL_BASE, mcs::GLOBAL_CHANNEL];
            join.extend(server.channel_ids.iter().copied().filter(|&id| id != 0));
            for chan in join {
                step(&mut transport, &mcs::channel_join_request(user_id, chan))?;
                mcs::parse_channel_join_confirm(&read_mcs(&mut transport)?)?;
            }
            Ok((server, user_id))
        })();
        let (server, user_id) = result.map_err(|e| failed(e, false))?;

        let rdp_version = if server.rdp_version < 5 { 4 } else { settings.rdp_version };
        let depth = if rdp_version < 5 { 8 } else { settings.depth };
        for (i, reg) in channels.iter_mut().enumerate() {
            // Servers that leave SC_NET's list short use rdesktop's numbering.
            reg.mcs_id = if rdp_version < 5 { 0 } else { server.channel_ids.get(i).copied().unwrap_or(mcs::GLOBAL_CHANNEL + 1 + i as u16) };
        }

        let security = sec::parse_server_security(&server.security, server.rdp_version).map_err(|e| failed(e, rdp_version < 5))?;
        let mut client_random = [0u8; 32];
        let mut crypto = None;
        let mut exchange = None;
        // The server's choice stands even if we offered none: it encrypts,
        // or the session can't go ahead.
        if rdp_security && security.method != sec::METHOD_NONE {
            let (Some(server_random), Some(key)) = (security.server_random, &security.public_key) else {
                return Err(failed(Error::protocol("the server sent no key for encryption"), rdp_version < 5));
            };
            crate::util::random(&mut client_random);
            exchange = Some(sec::encrypt_client_random(&client_random, key));
            crypto = Some(Crypto::new(&client_random, &server_random, security.method));
        }

        let licence = Licence::new(licence::Info {
            hostname: hostname.clone(),
            username: settings.username.clone(),
            imported: config.licence.clone(),
            rdp_version,
        });
        let mut conn = Conn {
            shared,
            graphics,
            channels,
            id,
            transport,
            rdp_version,
            depth,
            user_id,
            io_channel: server.io_channel,
            encrypt: crypto.is_some(),
            crypto,
            client_random,
            licence,
            licensed: false,
            licence_refused: false,
            share_id: 0,
            activated: false,
            deactivated: false,
            skip_pdus: 0,
            ext_disc_reason: ERRINFO_UNSET,
            server_input_flags: 0,
            chunk_size: channels::CHUNK_LENGTH,
            mppc: Mppc::new(),
            fragments: vec![Vec::new(); 16],
            locks: Locks::default(),
            started: Instant::now(),
            heartbeat: None,
            settings,
            carry,
        };
        let sent: Result<()> = (|| {
            if let Some(blob) = exchange {
                let mut w = Writer::new();
                w.u32(blob.len() as u32).bytes(&blob);
                conn.send_sec(sec::SEC_EXCHANGE_PKT, w.as_slice(), mcs::GLOBAL_CHANNEL)?;
            }
            conn.send_client_info()
        })();
        sent.map_err(|e| failed(e, rdp_version < 5))?;
        Ok(conn)
    }

    /// Start the reader thread; frames arrive in the session loop from now on.
    pub fn start_reader(&mut self) -> io::Result<()> {
        match self.transport.take_reader() {
            Some(r) => r.spawn(self.id, self.shared.tx.clone()),
            None => Ok(()),
        }
    }

    /// Stop encrypting what we send (`-E`: only the logon is encrypted).
    pub fn stop_encrypting(&mut self) {
        self.encrypt = false;
    }

    // -------------------------------------------------------------------
    // Sending

    /// Send `payload` on `channel` with a security header as needed: while
    /// licensing is going on, and when encrypting (`flags` without
    /// SEC_ENCRYPT; it's added here when we encrypt). Once licensing is over
    /// an unencrypted PDU has no header at all, whatever its flags: that's
    /// what rdesktop sends, and what servers without encryption (TLS
    /// sessions, ntrdps) read.
    fn send_sec(&mut self, flags: u16, payload: &[u8], channel: u16) -> Result<()> {
        let encrypt = self.encrypt && flags & (sec::SEC_EXCHANGE_PKT | sec::SEC_LICENSE_PKT) == 0;
        let mut w = Writer::with_capacity(payload.len() + 12);
        if encrypt {
            let mut data = payload.to_vec();
            let mac = self.crypto.as_mut().expect("encrypting without keys").encrypt(&mut data);
            w.u16(flags | sec::SEC_ENCRYPT).u16(0).bytes(&mac).bytes(&data);
        } else {
            if !self.licensed {
                w.u16(flags).u16(0);
            }
            w.bytes(payload);
        }
        let mcs = mcs::send_data_request(self.user_id, channel, w.as_slice());
        self.transport.send(&x224::data(&mcs))?;
        Ok(())
    }

    /// A share data PDU (TS_SHAREDATAHEADER) of `kind`.
    fn send_data(&mut self, kind: u8, body: &[u8]) -> Result<()> {
        let mut w = Writer::with_capacity(body.len() + 18);
        let len = (body.len() + 18) as u16;
        w.u16(len).u16(PDU_DATA | 0x10).u16(self.user_id + mcs::USERCHANNEL_BASE);
        w.u32(self.share_id).u8(0).u8(1).u16(len - 14).u8(kind).u8(0).u16(0);
        w.bytes(body);
        self.send_sec(0, &w.into_vec(), mcs::GLOBAL_CHANNEL)
    }

    fn send_client_info(&mut self) -> Result<()> {
        let s = &self.settings;
        let mut flags = s.info_flags;
        let redirect = self.carry.redirect.take();
        let password_blob = match redirect.as_ref().and_then(|r| r.cookie.clone()) {
            Some(cookie) => {
                flags |= INFO_AUTOLOGON;
                cookie
            }
            None => crate::text::to_utf16le(&s.password, true),
        };
        let mut w = Writer::new();
        let str16 = |w: &mut Writer, v: &str| {
            w.utf16(v).u16(0);
        };
        let ulen = |v: &str| (v.encode_utf16().count() * 2) as u16;
        w.u32(0).u32(flags);
        w.u16(ulen(&s.domain)).u16(ulen(&s.username)).u16(password_blob.len().saturating_sub(2) as u16);
        w.u16(ulen(&self.shared.config.shell)).u16(ulen(&self.shared.config.directory));
        str16(&mut w, &s.domain);
        str16(&mut w, &s.username);
        w.bytes(&password_blob);
        str16(&mut w, &self.shared.config.shell);
        str16(&mut w, &self.shared.config.directory);
        if self.rdp_version >= 5 {
            // TS_EXTENDED_INFO_PACKET
            let ip = match self.transport.local.ip() {
                std::net::IpAddr::V4(a) => a.to_string(),
                std::net::IpAddr::V6(_) => "127.0.0.1".to_string(),
            };
            const DLL: &str = "C:\\WINNT\\System32\\mstscax.dll";
            w.u16(2).u16(ulen(&ip) + 2);
            str16(&mut w, &ip);
            w.u16(ulen(DLL) + 2);
            str16(&mut w, DLL);
            // TS_TIME_ZONE_INFORMATION, as rdesktop has always sent it.
            w.u32(super::utc_bias_minutes() as u32);
            w.utf16_fixed("GTB, normaltid", 64);
            w.u32(0x0a0000).u32(0x050000).u32(3).u32(0).u32(0);
            w.utf16_fixed("GTB, sommartid", 64);
            w.u32(0x30000).u32(0x050000).u32(2).u32(0).u32(0xffff_ffc4);
            w.u32(0); // clientSessionId
            let mut perf = self.shared.config.experience.flags();
            if self.shared.config.local_cursor {
                perf |= crate::config::PERF_DISABLE_CURSOR_SHADOW;
            }
            w.u32(perf);
            match &self.carry.reconnect {
                Some(cookie) => {
                    use hmac::{Hmac, Mac};
                    let mut mac = <Hmac<md5::Md5> as Mac>::new_from_slice(&cookie.random).expect("HMAC takes any key");
                    mac.update(&self.client_random);
                    w.u16(28).u32(28).u32(1).u32(cookie.logon_id).bytes(&mac.finalize().into_bytes());
                }
                None => {
                    w.u16(0);
                }
            }
        }
        self.send_sec(sec::SEC_INFO_PKT, &w.into_vec(), mcs::GLOBAL_CHANNEL)
    }

    fn send_confirm_active(&mut self) -> Result<()> {
        let persistent = self.rdp_version >= 5 && self.shared.config.persistent_bitmap_cache && {
            let p = self.graphics_params();
            self.graphics.persistent_cache_ready(&p)
        };
        let (capsets, count) = caps::client(&caps::Client {
            rdp5: self.rdp_version >= 5,
            depth: self.depth,
            width: self.settings.width,
            height: self.settings.height,
            bitmap_cache: self.shared.config.bitmap_cache,
            persistent_cache: persistent,
            nine_grid: NINE_GRID,
        });
        let pdu = confirm_active(self.user_id, self.share_id, &capsets, count);
        // SEC_RESET_SEQNO | SEC_IGNORE_SEQNO, as rdesktop sends it.
        self.send_sec(0x30, &pdu, mcs::GLOBAL_CHANNEL)
    }

    fn send_input(&mut self, events: &[Input]) -> Result<()> {
        let time = self.started.elapsed().as_millis() as u32;
        for ev in events {
            if crate::session::trace_input() {
                eprintln!("input: type={:#06x} flags={:#06x} {:#06x} {:#06x}", ev.message_type, ev.flags, ev.param1, ev.param2);
            }
            let mut w = Writer::new();
            w.u16(1).u16(0).u32(time).u16(ev.message_type).u16(ev.flags).u16(ev.param1).u16(ev.param2);
            self.send_data(DATA_INPUT, w.as_slice())?;
        }
        Ok(())
    }

    fn send_control(&mut self, action: u16) -> Result<()> {
        let mut w = Writer::new();
        w.u16(action).u16(0).u32(0);
        self.send_data(DATA_CONTROL, w.as_slice())
    }

    fn send_fonts(&mut self, seq: u16) -> Result<()> {
        let mut w = Writer::new();
        w.u16(0).u16(0).u16(seq).u16(0x32);
        self.send_data(DATA_FONTLIST, w.as_slice())
    }

    /// The persistent bitmap cache's keys, so the server can use what we kept.
    fn send_persistent_keys(&mut self) -> Result<()> {
        const PER_PDU: usize = 169;
        let keys = self.graphics.persistent_keys(2);
        for (i, chunk) in keys.chunks(PER_PDU).enumerate() {
            let offset = i * PER_PDU;
            let mut flags = 0u32;
            if offset == 0 {
                flags |= 1;
            }
            if keys.len() - offset <= PER_PDU {
                flags |= 2;
            }
            let mut w = Writer::new();
            w.u32(0).u16(chunk.len() as u16).u16(0).u16(0).u16(0).u16(0).u16(keys.len() as u16).u32(0).u32(flags);
            for k in chunk {
                w.bytes(k);
            }
            self.send_data(DATA_PERSISTENT_KEY_LIST, w.as_slice())?;
        }
        Ok(())
    }

    /// Send what the channels queued, in chunks.
    fn flush_channels(&mut self, queued: Vec<(usize, Vec<Vec<u8>>)>) -> Result<()> {
        for (i, messages) in queued {
            let id = self.channels[i].mcs_id;
            if id == 0 {
                continue;
            }
            for m in messages {
                for chunk in self.channels[i].chunks(&m, self.chunk_size) {
                    self.send_sec(0, &chunk, id)?;
                }
            }
        }
        Ok(())
    }

    /// Disconnect politely.
    pub fn disconnect(&mut self) {
        let _ = self.transport.send(&x224::data(&mcs::disconnect_provider_ultimatum()));
        let _ = self.transport.send(&x224::disconnect_request());
        self.transport.shutdown();
        for c in self.channels.iter_mut() {
            c.channel.disconnected();
        }
    }

    // -------------------------------------------------------------------
    // Receiving

    fn graphics_params(&self) -> Params {
        let c = &self.shared.config;
        Params {
            depth: self.depth,
            width: self.settings.width as u32,
            height: self.settings.height as u32,
            rdp_version: self.rdp_version,
            bitmap_cache: c.bitmap_cache,
            persistent_cache: c.persistent_bitmap_cache,
            local_cursor: c.local_cursor,
            cache_namespace: crate::graphics::cache_namespace(&self.settings.server, c.port, &self.settings.domain, &self.settings.username),
        }
    }

    /// One frame from the reader thread. `Some` when the connection is over.
    pub fn handle_frame(&mut self, frame: &[u8]) -> Result<Option<End>> {
        if let Some(h) = &mut self.heartbeat {
            h.last = Instant::now();
        }
        if frame.first() != Some(&TPKT_VERSION) {
            self.fastpath(frame)?;
            self.graphics.flush();
            return Ok(None);
        }
        let payload = x224::parse_data(frame)?;
        let (channel, data) = match mcs::parse_data(payload)? {
            mcs::Incoming::Disconnect => return Ok(Some(self.closed(false))),
            mcs::Incoming::Data { channel, data } => (channel, data),
        };
        let mut data = data.to_vec();
        let mut start = 0;
        if self.crypto.is_some() || !self.licensed {
            let mut r = Reader::new(&data);
            let flags = r.u16()?;
            r.skip(2)?;
            start = 4;
            if self.crypto.is_some() && flags & (sec::SEC_ENCRYPT | sec::SEC_REDIRECTION_PKT) != 0 {
                if data.len() < 12 {
                    return Err(Error::Truncated);
                }
                start = 12;
                self.crypto.as_mut().unwrap().decrypt(&mut data[12..]);
            }
            if flags & sec::SEC_LICENSE_PKT != 0 {
                self.licensing(&data[start..])?;
                return Ok(self.licence_refused.then(|| End::Failed {
                    message: "the server refused a licence".into(),
                    licence_refused: true,
                    server_rdp4: false,
                }));
            }
            if flags & sec::SEC_HEARTBEAT != 0 {
                let mut r = Reader::new(&data[start..]);
                r.skip(1)?;
                let (period, count1, count2) = (r.u8()?, r.u8()?, r.u8()?);
                if period != 0 {
                    let period = Duration::from_secs(period as u64);
                    let n = (if count1 == 0 { 3 } else { count1 }) as u32 + (if count2 == 0 { 3 } else { count2 }) as u32;
                    self.heartbeat = Some(Heartbeat { period, reconnect_after: period * n, last: Instant::now() });
                }
                return Ok(None);
            }
            if flags & sec::SEC_REDIRECTION_PKT != 0 && data.len() >= start + 4 && data[start] == 0 && data[start + 1] == 4 {
                // A redirection PDU arrives as 00 04 LL LL: rdesktop swaps it
                // into a share control header (length, type 4).
                data.swap(start, start + 2);
                data.swap(start + 1, start + 3);
            }
        }
        let body = &data[start..];
        if channel != self.io_channel && channel != mcs::GLOBAL_CHANNEL {
            self.channel_data(channel, body)?;
            return Ok(None);
        }
        let end = self.share_control(body)?;
        self.graphics.flush();
        Ok(end)
    }

    fn licensing(&mut self, data: &[u8]) -> Result<()> {
        let mut out = Vec::new();
        let state = self.licence.process(&mut Reader::new(data), &mut out)?;
        for pdu in out {
            self.send_sec(sec::SEC_LICENSE_PKT, &pdu, mcs::GLOBAL_CHANNEL)?;
        }
        match state {
            licence::State::Pending => {}
            licence::State::Done => self.licensed = true,
            licence::State::Refused => self.licence_refused = true,
        }
        Ok(())
    }

    fn channel_data(&mut self, mcs_id: u16, body: &[u8]) -> Result<()> {
        let Some(i) = self.channels.iter().position(|c| c.mcs_id == mcs_id) else { return Ok(()) };
        let Some(message) = self.channels[i].reassemble(&mut Reader::new(body))? else { return Ok(()) };
        let mut out = Vec::new();
        let mut ctx = Ctx::new(&mut out);
        if let Err(e) = self.channels[i].channel.process(&message, &mut ctx) {
            trace(self.shared, format!("{} channel: {e}", self.channels[i].channel.name()));
        }
        self.flush_channels(vec![(i, out)])
    }

    /// Every share control PDU in `data` (servers pack several).
    fn share_control(&mut self, data: &[u8]) -> Result<Option<End>> {
        let mut r = Reader::new(data);
        while r.remaining() >= 2 {
            let start = r.pos();
            let total = r.u16()? as usize;
            if total == 0x8000 {
                // A T.128 flow PDU: 8 bytes, ignored.
                r.seek((start + 8).min(r.len()))?;
                continue;
            }
            if total < 4 {
                break;
            }
            let kind = r.u16()? & 0xf;
            // XP leaves out pduSource in deactivate all.
            if total > 4 {
                r.skip(2)?;
            }
            let body_len = total.saturating_sub(6).min(r.remaining());
            let mut body = r.sub(if total > 4 { body_len } else { 0 })?;
            r.seek((start + total).min(r.len()))?;

            if self.skip_pdus > 0 && kind != PDU_DEMAND_ACTIVE {
                self.skip_pdus -= 1;
                if self.skip_pdus == 0 && !self.activated {
                    self.finish_activation()?;
                }
                continue;
            }
            match kind {
                PDU_DEMAND_ACTIVE => self.demand_active(&mut body)?,
                PDU_DEACTIVATE => self.deactivated = true,
                PDU_REDIRECT | PDU_ENHANCED_REDIRECT => {
                    if let Some(redirect) = parse_redirect(&mut body, kind == PDU_ENHANCED_REDIRECT)? {
                        return Ok(Some(End::Redirect(redirect)));
                    }
                }
                PDU_DATA => self.data_pdu(&mut body)?,
                other => trace(self.shared, format!("unhandled PDU type {other}")),
            }
        }
        Ok(None)
    }

    fn demand_active(&mut self, r: &mut Reader) -> Result<()> {
        self.share_id = r.u32()?;
        let source_len = r.u16()? as usize;
        let _caps_len = r.u16()?;
        r.skip(source_len)?;
        let server = caps::parse_server(r)?;
        if server.rdp4 {
            self.rdp_version = 4;
        }
        if let Some(d) = server.depth
            && d != self.depth
        {
            trace(self.shared, format!("the server changed the colour depth from {} to {d}", self.depth));
            self.depth = d;
        }
        if server.width != 0 && server.height != 0 {
            self.settings.width = server.width;
            self.settings.height = server.height;
        }
        self.server_input_flags = server.input_flags;
        if let Some(size) = server.chunk_size.filter(|&s| s > 0) {
            self.chunk_size = size as usize;
        }
        let params = self.graphics_params();
        self.graphics.activate(&params);

        self.send_confirm_active()?;
        let mut w = Writer::new();
        w.u16(1).u16(1002);
        self.send_data(DATA_SYNCHRONISE, w.as_slice())?;
        self.send_control(CTL_COOPERATE)?;
        self.send_control(CTL_REQUEST_CONTROL)?;
        // rdesktop waits for the server's synchronise, cooperate and granted
        // control before sending the rest; see finish_activation.
        self.activated = false;
        self.skip_pdus = 3;
        Ok(())
    }

    fn finish_activation(&mut self) -> Result<()> {
        let wanted = self.shared.locks();
        let sync = self.locks.at_connect(self.rdp_version < 5, wanted);
        self.send_input(&[Input { message_type: RDP_INPUT_SYNCHRONIZE, flags: 0, param1: sync, param2: 0 }])?;
        if self.rdp_version >= 5 {
            self.send_persistent_keys()?;
            self.send_fonts(3)?;
        } else {
            self.send_fonts(1)?;
            self.send_fonts(2)?;
        }
        // The server's font map; rdesktop ignores it too.
        self.skip_pdus = 1;
        self.activated = true;
        self.deactivated = false;
        let mut queued = Vec::new();
        for (i, c) in self.channels.iter_mut().enumerate() {
            if c.mcs_id == 0 {
                continue;
            }
            let mut out = Vec::new();
            c.channel.connected(&mut Ctx::new(&mut out));
            queued.push((i, out));
        }
        self.flush_channels(queued)
    }

    fn data_pdu(&mut self, r: &mut Reader) -> Result<()> {
        r.skip(6)?; // share ID, pad, stream ID
        let _len = r.u16()?;
        let kind = r.u8()?;
        let ctype = r.u8()?;
        let _clen = r.u16()?;
        let rest = r.take_rest();
        let data: Vec<u8> = if ctype & PACKET_COMPRESSED != 0 { self.mppc.expand(rest, ctype)?.to_vec() } else { rest.to_vec() };
        let mut r = Reader::new(&data);
        match kind {
            DATA_UPDATE => {
                let update_type = r.u16()?;
                self.graphics.update(update_type, &mut r)?;
            }
            DATA_POINTER => {
                // Graphics skips the pad after the message type itself.
                let message_type = r.u16()?;
                self.graphics.pointer(message_type, &mut r)?;
            }
            DATA_BELL => self.shared.frontend.notify(Event::Bell),
            DATA_LOGON => self.logon_info(&mut r)?,
            DATA_SET_ERROR_INFO => {
                self.ext_disc_reason = r.u32()?;
                trace(self.shared, format!("error info {:#x}", self.ext_disc_reason));
            }
            DATA_CONTROL | DATA_SYNCHRONISE | DATA_FONTLIST => {}
            0x32 => trace(self.shared, "automatic reconnect with the cookie failed"),
            other => trace(self.shared, format!("unhandled data PDU type {other:#x}")),
        }
        Ok(())
    }

    /// TS_SAVE_SESSION_INFO_PDU_DATA: all we keep is the auto-reconnect cookie.
    fn logon_info(&mut self, r: &mut Reader) -> Result<()> {
        const INFOTYPE_LOGON_EXTENDED_INF: u32 = 3;
        const LOGON_EX_AUTORECONNECTCOOKIE: u32 = 1;
        if r.u32()? != INFOTYPE_LOGON_EXTENDED_INF {
            return Ok(());
        }
        r.skip(2)?;
        let fields = r.u32()?;
        if fields & LOGON_EX_AUTORECONNECTCOOKIE != 0 {
            r.skip(4)?;
            if r.u32()? != 28 || r.u32()? != 1 {
                return Ok(());
            }
            let logon_id = r.u32()?;
            let random = r.array()?;
            self.carry.reconnect = Some(ReconnectCookie { logon_id, random });
        }
        Ok(())
    }

    fn fastpath(&mut self, frame: &[u8]) -> Result<()> {
        let mut r = Reader::new(frame);
        let header = r.u8()?;
        let b = r.u8()?;
        if b & 0x80 != 0 {
            r.u8()?;
        }
        let mut data = r.rest().to_vec();
        if (header >> 6) & FASTPATH_OUTPUT_ENCRYPTED != 0 {
            if data.len() < 8 {
                return Err(Error::Truncated);
            }
            data.drain(..8);
            match &mut self.crypto {
                Some(c) => c.decrypt(&mut data),
                None => return Err(Error::protocol("an encrypted fast-path PDU without encryption")),
            }
        }
        let mut r = Reader::new(&data);
        while !r.is_empty() {
            let hdr = r.u8()?;
            let code = hdr & 0x0f;
            let frag = (hdr >> 4) & 0x3;
            let ctype = if (hdr >> 6) & FASTPATH_COMPRESSION_USED != 0 { r.u8()? } else { 0 };
            let len = r.u16()? as usize;
            let raw = r.bytes(len)?;
            let update: Vec<u8> = if ctype & PACKET_COMPRESSED != 0 { self.mppc.expand(raw, ctype)?.to_vec() } else { raw.to_vec() };
            match frag {
                FASTPATH_FRAGMENT_SINGLE => self.graphics.fastpath_update(code, &mut Reader::new(&update))?,
                _ => {
                    let buf = &mut self.fragments[code as usize];
                    if frag == FASTPATH_FRAGMENT_FIRST {
                        buf.clear();
                    }
                    if buf.len() + update.len() > caps::MULTIFRAGMENT_MAX as usize {
                        return Err(Error::protocol("fast-path update too large"));
                    }
                    buf.extend_from_slice(&update);
                    if frag == FASTPATH_FRAGMENT_LAST {
                        let whole = std::mem::take(buf);
                        self.graphics.fastpath_update(code, &mut Reader::new(&whole))?;
                    }
                }
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------------
    // The front-end's side

    /// Input, clipboard changes and channel work, after a wake or a frame.
    pub fn pump(&mut self) -> Result<()> {
        if self.activated {
            let input = self.shared.take_input();
            if !input.is_empty() {
                let unicode = self.server_input_flags & INPUT_FLAG_UNICODE != 0;
                let mut events = Vec::new();
                for ev in input {
                    match ev.message_type {
                        TYPE_LOCKS => events.extend(self.locks.expand(ev.param1)),
                        TYPE_CHAR => {
                            let c = char::from_u32(ev.param1 as u32 | (ev.param2 as u32) << 16).unwrap_or('\0');
                            events.extend(self.locks.flush());
                            events.extend(expand_char(c, unicode));
                        }
                        _ => {
                            events.extend(self.locks.before_key(&ev));
                            self.locks.note_key(&ev);
                            events.push(ev);
                        }
                    }
                }
                self.send_input(&events)?;
            }
        }
        let mut queued = Vec::new();
        for (i, c) in self.channels.iter_mut().enumerate() {
            if c.mcs_id == 0 {
                continue;
            }
            let mut out = Vec::new();
            c.channel.poll(&mut Ctx::new(&mut out));
            if !out.is_empty() {
                queued.push((i, out));
            }
        }
        self.flush_channels(queued)?;
        self.graphics.flush();
        Ok(())
    }

    /// When the loop must wake up even if nothing arrives.
    pub fn deadline(&self) -> Option<Instant> {
        let channels = self.channels.iter().filter(|c| c.mcs_id != 0).filter_map(|c| c.channel.deadline());
        let heartbeat = self.heartbeat.as_ref().map(|h| h.last + h.period);
        channels.chain(heartbeat).min()
    }

    /// The server's heartbeat has been missing too long: treat the
    /// connection as broken, as rdesktop does.
    pub fn heartbeat_lost(&self) -> bool {
        self.heartbeat.as_ref().is_some_and(|h| h.last.elapsed() >= h.reconnect_after)
    }

    pub fn closed(&self, network: bool) -> End {
        if !self.activated && self.share_id == 0 {
            return End::Failed {
                message: "the server closed the connection".into(),
                licence_refused: self.licence_refused,
                server_rdp4: self.rdp_version < self.settings.rdp_version,
            };
        }
        End::Closed { deactivated: self.deactivated, ext_disc_reason: self.ext_disc_reason, network }
    }
}

/// The confirm active PDU carrying `capsets` (`count` capability sets).
fn confirm_active(user_id: u16, share_id: u32, capsets: &[u8], count: u16) -> Vec<u8> {
    const SOURCE: &[u8] = b"MSTSC\0";
    let mut w = Writer::new();
    w.u16(0).u16(PDU_CONFIRM_ACTIVE | 0x10).u16(user_id + mcs::USERCHANNEL_BASE);
    w.u32(share_id).u16(0x3ea).u16(SOURCE.len() as u16).u16((capsets.len() + 4) as u16);
    w.bytes(SOURCE).u16(count).u16(0).bytes(capsets);
    // totalLength covers the whole PDU, its own header included.
    let total = w.len() as u16;
    w.set_u16(0, total);
    w.into_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_active_length_is_the_pdu_length() {
        let (capsets, count) = caps::client(&caps::Client {
            rdp5: true,
            depth: 16,
            width: 1024,
            height: 768,
            bitmap_cache: true,
            persistent_cache: false,
            nine_grid: true,
        });
        let pdu = confirm_active(1004, 0x103ea, &capsets, count);
        let mut r = Reader::new(&pdu);
        assert_eq!(r.u16().unwrap() as usize, pdu.len(), "totalLength");
        assert_eq!(r.u16().unwrap(), PDU_CONFIRM_ACTIVE | 0x10);
        r.skip(2 + 4 + 2).unwrap();
        let (source_len, caps_len) = (r.u16().unwrap() as usize, r.u16().unwrap() as usize);
        r.skip(source_len).unwrap();
        assert_eq!(caps_len, r.remaining(), "lengthCombinedCapabilities");
        // The length rdesktop sends: 2 + 14 + caplen + sizeof(RDP_SOURCE).
        assert_eq!(pdu.len(), 2 + 14 + capsets.len() + 4 + 6);
    }
}

/// A server redirection PDU. `None` when it only says not to redirect.
fn parse_redirect(r: &mut Reader, enhanced: bool) -> Result<Option<Redirect>> {
    const LB_TARGET_NET_ADDRESS: u32 = 0x1;
    const LB_LOAD_BALANCE_INFO: u32 = 0x2;
    const LB_USERNAME: u32 = 0x4;
    const LB_DOMAIN: u32 = 0x8;
    const LB_PASSWORD: u32 = 0x10;
    const LB_NOREDIRECT: u32 = 0x80;
    const LB_TARGET_FQDN: u32 = 0x100;
    let mut out = Redirect::default();
    r.skip(2)?;
    if enhanced {
        r.skip(4)?; // identifier 0x0400, length
        out.session_id = r.u32()?;
    }
    let flags = r.u32()?;
    let string = |r: &mut Reader| -> Result<String> {
        let len = r.u32()? as usize;
        r.utf16(len)
    };
    if flags & LB_TARGET_NET_ADDRESS != 0 {
        out.server = Some(string(r)?);
    }
    if flags & LB_LOAD_BALANCE_INFO != 0 {
        let len = r.u32()? as usize;
        r.skip(len)?;
    }
    if flags & LB_USERNAME != 0 {
        out.username = Some(string(r)?);
    }
    if flags & LB_DOMAIN != 0 {
        out.domain = Some(string(r)?);
    }
    if flags & LB_PASSWORD != 0 {
        let len = r.u32()? as usize;
        out.cookie = Some(r.bytes(len)?.to_vec());
    }
    if flags & LB_TARGET_FQDN != 0 {
        out.server = Some(string(r)?);
    }
    if flags & LB_NOREDIRECT != 0 {
        return Ok(None);
    }
    Ok(Some(out))
}
