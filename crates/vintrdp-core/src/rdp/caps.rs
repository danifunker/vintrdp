//! Capability sets: the client's, for the confirm active PDU, and what we
//! read from the server's demand active. Port of rdesktop's rdp_out_*_caps
//! and rdp_process_*_caps.

use super::input::{INPUT_FLAG_SCANCODES, INPUT_FLAG_UNICODE};
use crate::error::Result;
use crate::stream::{Reader, Writer};

const GENERAL: u16 = 1;
const BITMAP: u16 = 2;
const ORDER: u16 = 3;
const BMPCACHE: u16 = 4;
const CONTROL: u16 = 5;
const ACTIVATE: u16 = 7;
const POINTER: u16 = 8;
const SHARE: u16 = 9;
const COLCACHE: u16 = 10;
const SOUND: u16 = 12;
const INPUT: u16 = 13;
const FONT: u16 = 14;
const BRUSHCACHE: u16 = 15;
const GLYPHCACHE: u16 = 16;
const BMPCACHE2: u16 = 19;
const VIRTUAL_CHANNEL: u16 = 20;
const DRAWNINEGRIDCACHE: u16 = 21;
const MULTIFRAGMENTUPDATE: u16 = 26;
const LARGE_POINTER: u16 = 27;

const BMPCACHE2_C0_CELLS: u32 = 0x78;
const BMPCACHE2_C1_CELLS: u32 = 0x78;
const BMPCACHE2_C2_CELLS: u32 = 0x150;
const BMPCACHE2_NUM_PSTCELLS: u32 = 0x9f6;
const BMPCACHE2_FLAG_PERSIST: u32 = 1 << 31;

/// The largest reassembled fast-path update we accept.
pub const MULTIFRAGMENT_MAX: u32 = 65535;

/// What shapes the client's capabilities.
pub struct Client {
    pub rdp5: bool,
    pub depth: u32,
    pub width: u16,
    pub height: u16,
    pub bitmap_cache: bool,
    /// The persistent bitmap cache is open (bitmap cache rev. 2 only).
    pub persistent_cache: bool,
    /// Graphics draws DrawNineGrid orders.
    pub nine_grid: bool,
}

fn set(w: &mut Writer, kind: u16, body: impl FnOnce(&mut Writer)) {
    let start = w.len();
    w.u16(kind).u16(0);
    body(w);
    let len = (w.len() - start) as u16;
    w.set_u16(start + 2, len);
}

/// The capability sets, and how many there are.
pub fn client(c: &Client) -> (Vec<u8>, u16) {
    let mut w = Writer::new();
    let mut n = 0;
    let mut add = |w: &mut Writer, kind, body: &dyn Fn(&mut Writer)| {
        set(w, kind, body);
        n += 1;
    };

    add(&mut w, GENERAL, &|w| {
        // Fast-path output, auto-reconnect, long credentials, no compression header.
        let extra = if c.rdp5 { 0x0400 | 0x0008 | 0x0004 | 0x0001 } else { 0 };
        w.u16(1).u16(3).u16(0x200).u16(0).u16(0).u16(extra).u16(0).u16(0).u16(0).u8(0).u8(0);
    });
    add(&mut w, BITMAP, &|w| {
        w.u16(c.depth as u16).u16(1).u16(1).u16(1).u16(c.width).u16(c.height).u16(0);
        w.u16(1).u16(1).u8(0).u8(0).u16(1).u16(0);
    });
    add(&mut w, ORDER, &|w| {
        let mut support = [0u8; 32];
        for i in [0x00, 0x01, 0x02, 0x08, 0x16, 0x1b] {
            support[i] = 1; // destblt, patblt, scrblt, lineto, polyline, glyph index
        }
        if c.nine_grid {
            support[0x07] = 1;
            support[0x09] = 1;
        }
        if c.bitmap_cache {
            support[0x03] = 1; // memblt
        }
        support[0x0b] = 1; // desktop save
        for i in [0x14, 0x15, 0x19, 0x1a] {
            support[i] = 1; // polygons and ellipses
        }
        w.zeros(16).zeros(4);
        w.u16(1).u16(20).u16(0).u16(1).u16(0);
        w.u16(0x0002 | 0x0008 | 0x0020); // negotiate, zero bounds deltas, colour index
        w.bytes(&support);
        w.u16(0).u16(0).u32(0);
        w.u32(230_400); // desktop save size
        w.u16(0).u16(0).u16(1252).u16(0);
    });
    if c.rdp5 {
        add(&mut w, BMPCACHE2, &|w| {
            w.u16(if c.persistent_cache { 2 } else { 0 });
            w.u16_be(3);
            w.u32(BMPCACHE2_C0_CELLS).u32(BMPCACHE2_C1_CELLS);
            w.u32(if c.persistent_cache { BMPCACHE2_NUM_PSTCELLS | BMPCACHE2_FLAG_PERSIST } else { BMPCACHE2_C2_CELLS });
            w.zeros(20);
        });
        add(&mut w, POINTER, &|w| {
            w.u16(1).u16(20).u16(20);
        });
    } else {
        add(&mut w, BMPCACHE, &|w| {
            let bpp = c.depth.div_ceil(8) as u16;
            w.zeros(24);
            w.u16(0x258).u16(0x100 * bpp).u16(0x12c).u16(0x400 * bpp).u16(0x106).u16(0x1000 * bpp);
        });
        add(&mut w, POINTER, &|w| {
            w.u16(0).u16(20);
        });
    }
    add(&mut w, COLCACHE, &|w| {
        w.u16(6).u16(0);
    });
    add(&mut w, ACTIVATE, &|w| {
        w.zeros(8);
    });
    add(&mut w, CONTROL, &|w| {
        w.u16(0).u16(0).u16(2).u16(2);
    });
    add(&mut w, SHARE, &|w| {
        w.u16(0).u16(0);
    });
    add(&mut w, BRUSHCACHE, &|w| {
        w.u32(1);
    });
    add(&mut w, VIRTUAL_CHANNEL, &|w| {
        w.u32(1); // VCCAPS_COMPR_SC
    });
    add(&mut w, INPUT, &|w| {
        // vintrdp: Unicode key events, for characters no key makes.
        w.u16(INPUT_FLAG_SCANCODES | INPUT_FLAG_UNICODE).u16(0);
        w.u32(0x409).u32(4).u32(0).u32(12).zeros(64);
    });
    add(&mut w, SOUND, &|w| {
        w.u16(1).u16(0); // beeps
    });
    add(&mut w, FONT, &|w| {
        w.u16(1).u16(0);
    });
    add(&mut w, GLYPHCACHE, &|w| {
        for (entries, size) in [(254, 4), (254, 4), (254, 8), (254, 8), (254, 16), (254, 32), (254, 64), (254, 128), (254, 256), (64, 2048)] {
            w.u16(entries).u16(size);
        }
        w.u32(0x0100_0100).u16(2).u16(0); // fragment cache; full glyph support
    });
    if c.nine_grid {
        add(&mut w, DRAWNINEGRIDCACHE, &|w| {
            w.u32(2).u16(2560).u16(256);
        });
    }
    add(&mut w, MULTIFRAGMENTUPDATE, &|w| {
        w.u32(MULTIFRAGMENT_MAX);
    });
    add(&mut w, LARGE_POINTER, &|w| {
        w.u16(1); // 96x96
    });
    (w.into_vec(), n)
}

/// What we take from the server's capabilities.
#[derive(Debug, Default)]
pub struct Server {
    /// The general set said RDP 4 (no extra flags).
    pub rdp4: bool,
    pub depth: Option<u32>,
    pub width: u16,
    pub height: u16,
    pub input_flags: u16,
    /// The virtual channel chunk size, if the server named one.
    pub chunk_size: Option<u32>,
}

/// Parse the combined capabilities of a demand active PDU.
pub fn parse_server(r: &mut Reader) -> Result<Server> {
    let mut out = Server::default();
    let count = r.u16()?;
    r.skip(2)?;
    for _ in 0..count {
        if r.remaining() < 4 {
            break;
        }
        let kind = r.u16()?;
        let len = r.u16()? as usize;
        let mut c = r.sub(len.saturating_sub(4).min(r.remaining()))?;
        match kind {
            GENERAL => {
                c.skip(10)?;
                out.rdp4 = c.u16()? == 0;
            }
            BITMAP => {
                out.depth = Some(c.u16()? as u32);
                c.skip(6)?;
                out.width = c.u16()?;
                out.height = c.u16()?;
            }
            INPUT if len >= 6 => out.input_flags = c.u16()?,
            VIRTUAL_CHANNEL if len > 8 => {
                c.skip(4)?;
                out.chunk_size = Some(c.u32()?);
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_match_rdesktops() {
        let c = Client { rdp5: true, depth: 16, width: 800, height: 600, bitmap_cache: true, persistent_cache: false, nine_grid: true };
        let (caps, n) = client(&c);
        // RDP_CAPLEN_* summed as in rdp_send_confirm_active.
        let rdesktop = 0x18 + 0x1c + 0x58 + 0x08 + 0x0c + 0x0c + 0x08 + 0x08 + 88 + 8 + 8 + 52 + 12 + 8 + 6 + 0x08 + 0x28 + 0x0a;
        assert_eq!((caps.len(), n), (rdesktop, 18));
        let (caps4, n4) = client(&Client { rdp5: false, ..c });
        assert_eq!((caps4.len(), n4), (rdesktop - 0x28 - 0x0a + 0x28 + 0x08, 18));
    }

    #[test]
    fn parses_server_caps() {
        let mut w = Writer::new();
        w.u16(3).u16(0);
        w.u16(GENERAL).u16(24).zeros(10).u16(0).zeros(8);
        w.u16(BITMAP).u16(28).u16(8).zeros(6).u16(640).u16(480).zeros(12);
        w.u16(INPUT).u16(8).u16(0x31).u16(0);
        let s = parse_server(&mut Reader::new(w.as_slice())).unwrap();
        assert!(s.rdp4);
        assert_eq!((s.depth, s.width, s.height, s.input_flags), (Some(8), 640, 480, 0x31));
    }
}
