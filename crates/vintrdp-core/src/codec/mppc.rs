//! MPPC bulk decompression (RFC 2118): RDP 4's 8 KB history and RDP 5's
//! 64 KB one. Port of rdesktop's mppc.c.

/// PACKET_COMPRESSED and friends, from the compression flags of a share data
/// header or a fast-path update.
pub const PACKET_COMPR_TYPE_8K: u8 = 0x00;
pub const PACKET_COMPR_TYPE_64K: u8 = 0x01;
pub const PACKET_COMPRESSED: u8 = 0x20;
pub const PACKET_AT_FRONT: u8 = 0x40;
pub const PACKET_FLUSHED: u8 = 0x80;

/// The decompressor's history, kept for the whole connection.
pub struct Mppc {
    _private: (),
}

impl Mppc {
    pub fn new() -> Self {
        todo!("codec::mppc::Mppc::new")
    }

    /// Decompress one PDU. `flags` is the full compression-flags byte
    /// (type in the low nibble, PACKET_* above). Returns the decompressed
    /// bytes, which stay valid until the next call. Data that isn't
    /// PACKET_COMPRESSED is returned as is (after honouring FLUSHED).
    pub fn expand<'a>(&'a mut self, data: &'a [u8], flags: u8) -> crate::error::Result<&'a [u8]> {
        let _ = (data, flags);
        todo!("codec::mppc::Mppc::expand")
    }
}

impl Default for Mppc {
    fn default() -> Self {
        Self::new()
    }
}
