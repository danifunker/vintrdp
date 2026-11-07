//! Static virtual channels (RDP 5): the clipboard, sound and device
//! redirection. Each is a [`Channel`]; this module splits what they send into
//! chunks and puts the server's chunks back together. RDP 4 has no channels.
//!
//! Everything here runs on the session thread.

pub mod audio;
pub mod cliprdr;
pub mod rdpdr;
pub mod rdpsnd;

use std::time::Instant;

use crate::error::Result;
use crate::stream::{Reader, Writer};

/// CHANNEL_DEF options (MS-RDPBCGR 2.2.1.3.4.1).
pub const OPTION_INITIALIZED: u32 = 0x8000_0000;
pub const OPTION_ENCRYPT_RDP: u32 = 0x4000_0000;
pub const OPTION_COMPRESS_RDP: u32 = 0x0080_0000;
pub const OPTION_SHOW_PROTOCOL: u32 = 0x0020_0000;

const FLAG_FIRST: u32 = 0x01;
const FLAG_LAST: u32 = 0x02;
const FLAG_SHOW_PROTOCOL: u32 = 0x10;

/// The chunk size unless the server's virtual channel capability names one.
pub const CHUNK_LENGTH: usize = 1600;

/// What a channel can do while handling a message or being polled. Channels
/// keep their own `Arc<Shared>` for the session's state.
pub struct Ctx<'a> {
    out: &'a mut Vec<Vec<u8>>,
}

impl<'a> Ctx<'a> {
    pub fn new(out: &'a mut Vec<Vec<u8>>) -> Self {
        Ctx { out }
    }

    /// Queue a whole message for the server on this channel; it's split into
    /// chunks on the way out.
    pub fn send(&mut self, data: impl Into<Vec<u8>>) {
        self.out.push(data.into());
    }
}

/// One static virtual channel's client end.
pub trait Channel: Send {
    /// The channel's name, at most 7 ASCII characters ("cliprdr").
    fn name(&self) -> &'static str;

    /// CHANNEL_DEF options to announce.
    fn options(&self) -> u32 {
        OPTION_INITIALIZED | OPTION_ENCRYPT_RDP
    }

    /// The server joined the channel; a new connection starts here.
    fn connected(&mut self, _ctx: &mut Ctx) {}

    /// One whole message from the server, reassembled from its chunks.
    fn process(&mut self, data: &[u8], ctx: &mut Ctx) -> Result<()>;

    /// Called after every message the session receives and whenever the
    /// front-end wakes the session, and at [`Channel::deadline`]: do any
    /// work that doesn't wait on the server (local clipboard changes, device
    /// I/O that has completed, sound that has finished playing).
    fn poll(&mut self, _ctx: &mut Ctx) {}

    /// When [`Channel::poll`] next needs to run even if nothing happens.
    fn deadline(&self) -> Option<Instant> {
        None
    }

    /// The connection is going away (it may come back: see `connected`).
    fn disconnected(&mut self) {}
}

/// A channel as registered for a connection.
pub struct Registered {
    pub channel: Box<dyn Channel>,
    /// The MCS channel the server assigned; 0 if it declined the channel.
    pub mcs_id: u16,
    /// A message coming in chunks.
    incoming: Vec<u8>,
}

impl Registered {
    pub fn new(channel: Box<dyn Channel>) -> Self {
        Registered { channel, mcs_id: 0, incoming: Vec::new() }
    }

    /// One chunk from the server: the whole message once the last chunk is in.
    pub fn reassemble(&mut self, r: &mut Reader) -> Result<Option<Vec<u8>>> {
        let length = r.u32()? as usize;
        let flags = r.u32()?;
        let data = r.take_rest();
        if flags & FLAG_FIRST != 0 && flags & FLAG_LAST != 0 {
            return Ok(Some(data.to_vec()));
        }
        if flags & FLAG_FIRST != 0 {
            self.incoming.clear();
            // Don't let the server make us reserve absurd amounts up front.
            self.incoming.reserve(length.min(1 << 20));
        }
        self.incoming.extend_from_slice(data);
        if flags & FLAG_LAST != 0 {
            return Ok(Some(std::mem::take(&mut self.incoming)));
        }
        Ok(None)
    }

    /// `data` as channel PDUs: each a CHANNEL_PDU_HEADER and up to
    /// `chunk_size` bytes, ready for the security layer.
    pub fn chunks(&self, data: &[u8], chunk_size: usize) -> Vec<Vec<u8>> {
        let total = data.len();
        let show = if self.channel.options() & OPTION_SHOW_PROTOCOL != 0 { FLAG_SHOW_PROTOCOL } else { 0 };
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let n = (total - offset).min(chunk_size.max(1));
            let mut flags = show;
            if offset == 0 {
                flags |= FLAG_FIRST;
            }
            if offset + n == total {
                flags |= FLAG_LAST;
            }
            let mut w = Writer::with_capacity(n + 8);
            w.u32(total as u32).u32(flags).bytes(&data[offset..offset + n]);
            out.push(w.into_vec());
            offset += n;
            if offset >= total {
                return out;
            }
        }
    }
}

/// A CHANNEL_DEF name: 8 bytes, NUL-padded.
pub fn def_name(name: &str) -> [u8; 8] {
    let mut out = [0u8; 8];
    for (o, b) in out.iter_mut().zip(name.bytes().take(7)) {
        *o = b;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Null;
    impl Channel for Null {
        fn name(&self) -> &'static str {
            "null"
        }
        fn process(&mut self, _: &[u8], _: &mut Ctx) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn chunks_reassemble() {
        let mut reg = Registered::new(Box::new(Null));
        let data: Vec<u8> = (0..4000u32).map(|i| i as u8).collect();
        let chunks = reg.chunks(&data, CHUNK_LENGTH);
        assert_eq!(chunks.len(), 3);
        let mut whole = None;
        for c in &chunks {
            whole = reg.reassemble(&mut Reader::new(c)).unwrap();
        }
        assert_eq!(whole.unwrap(), data);
        // An empty message is one chunk, first and last.
        assert_eq!(reg.chunks(&[], CHUNK_LENGTH), vec![vec![0, 0, 0, 0, 3, 0, 0, 0]]);
    }
}
