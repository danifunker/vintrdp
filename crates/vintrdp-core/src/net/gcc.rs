//! T.124 GCC conference create request and response, and the RDP data blocks
//! they carry: what the client is (CS_CORE, CS_CLUSTER, CS_SECURITY, CS_NET)
//! and what the server answers (SC_CORE, SC_SECURITY, SC_NET). Port of
//! rdesktop's sec_out_mcs_connect_initial_pdu and sec_process_mcs_data.

use crate::error::{Error, Result};
use crate::stream::{Reader, Writer};

const CS_CORE: u16 = 0xc001;
const CS_SECURITY: u16 = 0xc002;
const CS_NET: u16 = 0xc003;
const CS_CLUSTER: u16 = 0xc004;

const SC_CORE: u16 = 0x0c01;
const SC_SECURITY: u16 = 0x0c02;
const SC_NET: u16 = 0x0c03;

const RDP_40: u32 = 0x0008_0001;
const RDP_50: u32 = 0x0008_0004;

const RNS_UD_COLOR_8BPP: u16 = 0xca01;
const RNS_UD_SAS_DEL: u16 = 0xaa03;
const RNS_UD_24BPP_SUPPORT: u16 = 0x0001;
const RNS_UD_16BPP_SUPPORT: u16 = 0x0002;
const RNS_UD_32BPP_SUPPORT: u16 = 0x0008;
const RNS_UD_CS_SUPPORT_ERRINFO_PDU: u16 = 0x0001;
const RNS_UD_CS_WANT_32BPP_SESSION: u16 = 0x0002;
const RNS_UD_CS_SUPPORT_STATUSINFO_PDU: u16 = 0x0004;
const RNS_UD_CS_SUPPORT_HEARTBEAT_PDU: u16 = 0x0400;

const SEC_CC_REDIRECTION_SUPPORTED: u32 = 0x01;
const SEC_CC_REDIRECT_SESSIONID_FIELD_VALID: u32 = 0x02;
const SEC_CC_REDIRECT_VERSION_4: u32 = 0x03;

/// What the client tells the server about itself.
pub struct ClientData<'a> {
    pub rdp5: bool,
    pub width: u16,
    pub height: u16,
    /// 8, 15, 16, 24 or 32.
    pub depth: u32,
    pub keyboard_layout: u32,
    pub hostname: &'a str,
    /// The protocol X.224 negotiation picked (PROTOCOL_*).
    pub selected_protocol: u32,
    /// Offer 40- and 128-bit RDP encryption (else none).
    pub encryption: bool,
    /// Connect to session `session_id` (the console, or a redirection target).
    pub session_id: Option<u32>,
    /// Static virtual channels: name and options.
    pub channels: Vec<([u8; 8], u32)>,
}

impl ClientData<'_> {
    fn blocks(&self) -> Vec<u8> {
        let mut w = Writer::new();

        // TS_UD_CS_CORE
        let depth = self.depth.min(24) as u16;
        let mut early = RNS_UD_CS_SUPPORT_ERRINFO_PDU | RNS_UD_CS_SUPPORT_STATUSINFO_PDU | RNS_UD_CS_SUPPORT_HEARTBEAT_PDU;
        if self.depth == 32 {
            early |= RNS_UD_CS_WANT_32BPP_SESSION;
        }
        w.u16(CS_CORE).u16(216);
        w.u32(if self.rdp5 { RDP_50 } else { RDP_40 });
        w.u16(self.width).u16(self.height);
        w.u16(RNS_UD_COLOR_8BPP).u16(RNS_UD_SAS_DEL);
        w.u32(self.keyboard_layout);
        // Client build 2600: "Windows XP", and smart cards as of XP.
        w.u32(2600);
        w.utf16_fixed(self.hostname, 32);
        w.u32(4).u32(0).u32(12); // keyboard type, subtype, function keys
        w.zeros(64); // imeFileName
        w.u16(RNS_UD_COLOR_8BPP); // postBeta2ColorDepth
        w.u16(1).u32(0); // clientProductId, serialNumber
        w.u16(depth); // highColorDepth
        w.u16(RNS_UD_24BPP_SUPPORT | RNS_UD_16BPP_SUPPORT | RNS_UD_32BPP_SUPPORT);
        w.u16(early);
        w.zeros(64); // clientDigProductId
        w.u8(0).u8(0); // connectionType, pad
        w.u32(self.selected_protocol);

        // TS_UD_CS_CLUSTER
        let mut cluster = SEC_CC_REDIRECTION_SUPPORTED | (SEC_CC_REDIRECT_VERSION_4 << 2);
        if self.session_id.is_some() {
            cluster |= SEC_CC_REDIRECT_SESSIONID_FIELD_VALID;
        }
        w.u16(CS_CLUSTER).u16(12).u32(cluster).u32(self.session_id.unwrap_or(0));

        // TS_UD_CS_SEC
        w.u16(CS_SECURITY).u16(12).u32(if self.encryption { 0x3 } else { 0 }).u32(0);

        // TS_UD_CS_NET
        if !self.channels.is_empty() {
            w.u16(CS_NET).u16((self.channels.len() * 12 + 8) as u16).u32(self.channels.len() as u32);
            for (name, options) in &self.channels {
                w.bytes(name).u32(*options);
            }
        }
        w.into_vec()
    }

    /// The conference create request carrying the client data blocks.
    pub fn conference_create_request(&self) -> Vec<u8> {
        let blocks = self.blocks();
        let mut w = Writer::new();
        // T.124 ConnectData with key t124 (0.0.20.124.0.1), as rdesktop writes it.
        w.u16_be(5).u16_be(0x14).u8(0x7c).u16_be(1);
        w.u16_be((blocks.len() + 14) as u16 | 0x8000);
        // ConferenceCreateRequest
        w.u16_be(8).u16_be(16).u8(0).u16(0xc001).u8(0);
        w.u32(0x6163_7544); // H.221 key "Duca"
        w.u16_be(blocks.len() as u16 | 0x8000);
        w.bytes(&blocks);
        w.into_vec()
    }
}

/// What the server tells us in the conference create response.
#[derive(Debug, Default)]
pub struct ServerData {
    /// 4 when the server speaks RDP 4 only (NT 4.0 TSE), else 5.
    pub rdp_version: u8,
    /// SC_SECURITY's body, for `sec::parse_server_security`.
    pub security: Vec<u8>,
    /// The I/O channel and the channels assigned to ours, in order.
    pub io_channel: u16,
    pub channel_ids: Vec<u16>,
}

/// Parse the conference create response's user data.
pub fn parse_conference_create_response(gcc: &[u8]) -> Result<ServerData> {
    let mut r = Reader::new(gcc);
    // T.124 ConnectData and ConferenceCreateResponse headers, of fixed size
    // in practice, then a PER length.
    r.skip(21)?;
    let len = r.u8()?;
    if len & 0x80 != 0 {
        r.u8()?;
    }
    let mut out = ServerData { rdp_version: 5, io_channel: crate::net::mcs::GLOBAL_CHANNEL, ..ServerData::default() };
    while r.remaining() >= 4 {
        let tag = r.u16()?;
        let length = r.u16()? as usize;
        if length <= 4 {
            break;
        }
        let mut block = r.sub((length - 4).min(r.remaining()))?;
        match tag {
            SC_CORE => {
                // The low word is the major version: 1 for RDP 4.
                if block.u16()? == 1 {
                    out.rdp_version = 4;
                }
            }
            SC_SECURITY => out.security = block.rest().to_vec(),
            SC_NET => {
                out.io_channel = block.u16()?;
                let count = block.u16()?;
                for _ in 0..count {
                    out.channel_ids.push(block.u16()?);
                }
            }
            _ => {}
        }
    }
    if out.security.is_empty() {
        return Err(Error::protocol("the server sent no security data"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_blocks_have_the_lengths_rdesktop_used() {
        let cd = ClientData {
            rdp5: true,
            width: 1024,
            height: 768,
            depth: 16,
            keyboard_layout: 0x409,
            hostname: "box",
            selected_protocol: 0,
            encryption: true,
            session_id: None,
            channels: vec![(*b"rdpdr\0\0\0", 0x8080_0000), (*b"cliprdr\0", 0xc0a0_0000)],
        };
        let gcc = cd.conference_create_request();
        // rdesktop's length: 162 + 76 + 12 + 4 after the first 9 bytes, plus 12
        // per channel and 8.
        assert_eq!(gcc.len(), 9 + 162 + 76 + 12 + 4 + 2 * 12 + 8);
        assert_eq!(&gcc[23..27], [0x01, 0xc0, 216, 0]);
    }

    #[test]
    fn server_data() {
        let mut w = Writer::new();
        w.zeros(21).u8(0x80 | 0).u8(40);
        w.u16(SC_CORE).u16(8).u32(0x0008_0001);
        w.u16(SC_SECURITY).u16(12).u32(0).u32(0);
        w.u16(SC_NET).u16(10).u16(1003).u16(1).u16(1004);
        let sd = parse_conference_create_response(w.as_slice()).unwrap();
        assert_eq!(sd.rdp_version, 4);
        assert_eq!(sd.security, [0; 8]);
        assert_eq!(sd.channel_ids, [1004]);
    }
}
