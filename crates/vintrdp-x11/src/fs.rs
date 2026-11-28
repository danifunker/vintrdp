//! A font server client (the X Font Service Protocol, version 2, as libFS
//! speaks it): `tcp/host:port` font path elements, as X.org's fs code
//! has them. A font is fetched whole when it's opened (QueryXInfo,
//! QueryXExtents16 and QueryXBitmaps16 of every character) and drawn as
//! the bundled ones are; the server keeps it from then on.
//!
//! Everything is big-endian (the client says 'B'); requests are answered
//! in order, so each call waits for its own reply, skipping errors' and
//! events' packets that aren't for it. A connection that fails is made
//! again on the next call.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::font::{Font, Glyph, Metrics, PropValue};

const LIST_FONTS: u8 = 13;
const OPEN_BITMAP_FONT: u8 = 15;
const QUERY_X_INFO: u8 = 16;
const QUERY_X_EXTENTS16: u8 = 18;
const QUERY_X_BITMAPS16: u8 = 20;
const CLOSE_FONT: u8 = 21;

/// Bitmaps MSB first (bytes and bits), the glyph's own rectangle, rows
/// padded to a byte.
const FORMAT: u32 = 1 | 2;
/// Which parts of FORMAT are given: all of them (byte and bit order,
/// image rectangle, scanline pad and unit, BitmapFormatMask*).
const FORMAT_MASK: u32 = 0x1f;
/// How long the font server has to answer.
const TIMEOUT: Duration = Duration::from_secs(15);

pub struct Conn {
    /// `host:port`.
    addr: String,
    stream: Option<TcpStream>,
    seq: u16,
    next_id: u32,
}

/// `tcp/host:port` (or `inet/`, as X.org also takes): the font server's address.
pub fn address(element: &str) -> Option<String> {
    let rest = element.strip_prefix("tcp/").or_else(|| element.strip_prefix("inet/"))?;
    let rest = rest.split('/').next()?;
    let (host, port) = rest.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    (!host.is_empty()).then(|| rest.to_string())
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn pad4(n: usize) -> usize {
    (4 - n % 4) % 4
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("font server: {why}"))
}

impl Conn {
    /// Connected, or the reason it couldn't be (SetFontPath then refuses the element).
    pub fn new(addr: &str) -> io::Result<Conn> {
        let mut c = Conn { addr: addr.to_string(), stream: None, seq: 0, next_id: 1 };
        c.stream()?;
        Ok(c)
    }

    fn stream(&mut self) -> io::Result<&mut TcpStream> {
        if self.stream.is_none() {
            let a = self.addr.to_socket_addrs()?.next().ok_or_else(|| bad("no address"))?;
            let mut s = TcpStream::connect_timeout(&a, TIMEOUT)?;
            s.set_read_timeout(Some(TIMEOUT))?;
            s.set_write_timeout(Some(TIMEOUT))?;
            let _ = s.set_nodelay(true);
            // fsConnClientPrefix: 'B', no authorizations, version 2.0.
            s.write_all(&[b'B', 0, 0, 2, 0, 0, 0, 0])?;
            let mut setup = [0u8; 12];
            s.read_exact(&mut setup)?;
            let (status, alternates, auth) = (be16(&setup[0..]), be16(&setup[8..]) as usize, be16(&setup[10..]) as usize);
            let mut skip = vec![0u8; (alternates + auth) * 4];
            s.read_exact(&mut skip)?;
            if status != 0 {
                return Err(bad(&format!("refused the connection (status {status})")));
            }
            // fsConnSetupAccept: its length counts itself.
            let mut accept = [0u8; 12];
            s.read_exact(&mut accept)?;
            let rest = (be32(&accept) as usize * 4).checked_sub(12).ok_or_else(|| bad("bad setup"))?;
            let mut vendor = vec![0u8; rest];
            s.read_exact(&mut vendor)?;
            self.stream = Some(s);
            self.seq = 0;
        }
        Ok(self.stream.as_mut().unwrap())
    }

    /// A request, and its reply's bytes (header included); None for an error.
    fn call(&mut self, op: u8, data: u8, body: &[u8], reply: bool) -> io::Result<Option<Vec<u8>>> {
        let r = self.try_call(op, data, body, reply);
        if r.is_err() {
            self.stream = None;
        }
        r
    }

    fn try_call(&mut self, op: u8, data: u8, body: &[u8], reply: bool) -> io::Result<Option<Vec<u8>>> {
        let mut req = vec![op, data, 0, 0];
        req.extend_from_slice(body);
        req.resize(req.len() + pad4(req.len()), 0);
        let words = (req.len() / 4) as u16;
        req[2..4].copy_from_slice(&words.to_be_bytes());
        let s = self.stream()?;
        s.write_all(&req)?;
        self.seq = self.seq.wrapping_add(1);
        if !reply {
            return Ok(None);
        }
        let seq = self.seq;
        loop {
            let s = self.stream.as_mut().unwrap();
            let mut head = [0u8; 8];
            s.read_exact(&mut head)?;
            let len = be32(&head[4..]) as usize * 4;
            let mut rest = vec![0u8; len.checked_sub(8).ok_or_else(|| bad("short packet"))?];
            s.read_exact(&mut rest)?;
            if be16(&head[2..]) != seq {
                continue;
            }
            match head[0] {
                0 => {
                    let mut all = head.to_vec();
                    all.extend_from_slice(&rest);
                    return Ok(Some(all));
                }
                1 => return Ok(None),
                _ => continue,
            }
        }
    }

    /// ListFonts: the names that match.
    pub fn list(&mut self, pattern: &[u8], max: u32) -> io::Result<Vec<String>> {
        let mut body = max.to_be_bytes().to_vec();
        body.extend_from_slice(&(pattern.len() as u16).to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(pattern);
        let Some(r) = self.call(LIST_FONTS, 0, &body, true)? else { return Ok(Vec::new()) };
        let n = be32(r.get(12..16).ok_or_else(|| bad("short ListFonts reply"))?) as usize;
        let mut names = Vec::with_capacity(n);
        let mut at = 16;
        for _ in 0..n {
            let Some(&len) = r.get(at) else { break };
            let Some(name) = r.get(at + 1..at + 1 + len as usize) else { break };
            names.push(name.iter().map(|&c| (c as char).to_ascii_lowercase()).collect());
            at += 1 + len as usize;
        }
        Ok(names)
    }

    /// OpenBitmapFont of a name or pattern, the whole font fetched; then
    /// closed on the font server. None if it has no such font.
    pub fn open(&mut self, pattern: &[u8]) -> io::Result<Option<Font>> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let mut body = id.to_be_bytes().to_vec();
        body.extend_from_slice(&FORMAT_MASK.to_be_bytes());
        body.extend_from_slice(&FORMAT.to_be_bytes());
        body.push(pattern.len() as u8);
        body.extend_from_slice(pattern);
        if self.call(OPEN_BITMAP_FONT, 0, &body, true)?.is_none() {
            return Ok(None);
        }
        let font = self.fetch(id);
        let _ = self.call(CLOSE_FONT, 0, &id.to_be_bytes(), false);
        font.map(Some)
    }

    fn fetch(&mut self, id: u32) -> io::Result<Font> {
        let info = self.call(QUERY_X_INFO, 0, &id.to_be_bytes(), true)?.ok_or_else(|| bad("QueryXInfo failed"))?;
        let h = info.get(8..48).ok_or_else(|| bad("short QueryXInfo reply"))?;
        let flags = be32(h);
        let (b1_min, b2_min, b1_max, b2_max) = (h[4], h[5], h[6], h[7]);
        let default_char = (h[10] as u16) << 8 | h[11] as u16;
        let metrics = |b: &[u8]| Metrics {
            lb: be16(&b[0..]) as i16,
            rb: be16(&b[2..]) as i16,
            width: be16(&b[4..]) as i16,
            ascent: be16(&b[6..]) as i16,
            descent: be16(&b[8..]) as i16,
            attributes: be16(&b[10..]),
        };
        let (min_bounds, max_bounds) = (metrics(&h[12..24]), metrics(&h[24..36]));
        let (ascent, descent) = (be16(&h[36..]) as i16, be16(&h[38..]) as i16);
        let properties = properties(&info[48..]).ok_or_else(|| bad("bad font properties"))?;
        if b1_min > b1_max || b2_min > b2_max {
            return Err(bad("empty font"));
        }
        // Every character, by ranges of one row each (an empty list may
        // mean all of them to some servers, none to others).
        let mut ranges = Vec::new();
        for b1 in b1_min..=b1_max {
            ranges.extend_from_slice(&[b1, b2_min, b1, b2_max]);
        }
        let n = (b1_max as usize - b1_min as usize + 1) * (b2_max as usize - b2_min as usize + 1);
        let mut body = id.to_be_bytes().to_vec();
        body.extend_from_slice(&((ranges.len() / 2) as u32).to_be_bytes());
        body.extend_from_slice(&ranges);
        let ext = self.call(QUERY_X_EXTENTS16, 1, &body, true)?.ok_or_else(|| bad("QueryXExtents16 failed"))?;
        let count = be32(&ext[8..]) as usize;
        if count != n || ext.len() < 12 + n * 12 {
            return Err(bad("QueryXExtents16 gave the wrong number of characters"));
        }
        let mut body = id.to_be_bytes().to_vec();
        body.extend_from_slice(&FORMAT.to_be_bytes());
        body.extend_from_slice(&((ranges.len() / 2) as u32).to_be_bytes());
        body.extend_from_slice(&ranges);
        let bits = self.call(QUERY_X_BITMAPS16, 1, &body, true)?.ok_or_else(|| bad("QueryXBitmaps16 failed"))?;
        let chars = be32(&bits[12..]) as usize;
        if chars != n || bits.len() < 20 + n * 8 {
            return Err(bad("QueryXBitmaps16 gave the wrong number of characters"));
        }
        let data = &bits[20 + n * 8..];
        let mut glyphs = Vec::with_capacity(n);
        for i in 0..n {
            let m = metrics(&ext[12 + i * 12..]);
            let (pos, len) = (be32(&bits[20 + i * 8..]) as usize, be32(&bits[24 + i * 8..]) as usize);
            // X.org's fs code: a character that isn't there has no metrics.
            if m == Metrics::default() {
                glyphs.push(None);
                continue;
            }
            let (w, rows) = ((m.rb - m.lb).max(0) as usize, (m.ascent + m.descent).max(0) as usize);
            let stride = w.div_ceil(8);
            let src = data.get(pos..pos + len).unwrap_or(&[]);
            let words = w.div_ceil(64).max(1);
            let mut px = vec![0u64; words * rows];
            for y in 0..rows {
                for x in 0..w {
                    if src.get(y * stride + x / 8).is_some_and(|&b| b >> (7 - x % 8) & 1 != 0) {
                        px[y * words + x / 64] |= 1 << (x % 64);
                    }
                }
            }
            let g = Glyph { m, ink: m, words, rows: px };
            let ink = crate::font::ink_of(&g);
            glyphs.push(Some(Glyph { ink, ..g }));
        }
        Ok(Font {
            glyphs,
            min_char: b2_min as u16,
            max_char: b2_max as u16,
            min_byte1: b1_min,
            max_byte1: b1_max,
            default_char,
            ascent,
            descent,
            min_bounds,
            max_bounds,
            all_exist: flags & 1 != 0,
            properties,
        })
    }
}

/// fsPropInfo: offsets, then the strings they point into.
fn properties(b: &[u8]) -> Option<Vec<(String, PropValue)>> {
    let (n, len) = (be32(b.get(0..4)?) as usize, be32(b.get(4..8)?) as usize);
    let offsets = b.get(8..8 + n * 20)?;
    let data = b.get(8 + n * 20..8 + n * 20 + len)?;
    let text = |pos: usize, len: usize| data.get(pos..pos + len).map(|s| s.iter().map(|&c| c as char).collect::<String>());
    let mut out = Vec::with_capacity(n);
    for o in offsets.chunks_exact(20) {
        let name = text(be32(&o[0..]) as usize, be32(&o[4..]) as usize)?;
        let value = match o[16] {
            0 => PropValue::Atom(text(be32(&o[8..]) as usize, be32(&o[12..]) as usize)?),
            _ => PropValue::Int(be32(&o[8..]) as i32),
        };
        out.push((name, value));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(address("tcp/fonthost:7100").as_deref(), Some("fonthost:7100"));
        assert_eq!(address("inet/10.0.0.1:7000/"), Some("10.0.0.1:7000".into()));
        assert_eq!(address("tcp/:7100"), None);
        assert_eq!(address("/usr/lib/X11/fonts/misc"), None);
        assert_eq!(address("tcp/host"), None);
    }

    /// Against a real font server (XQuartz's or X.org's xfs, if there's
    /// one here): fonts come out as they do read from their files.
    #[test]
    fn same_fonts_as_from_their_files() {
        let xfs = ["/opt/X11/bin/xfs", "/usr/bin/xfs"].into_iter().find(|p| std::path::Path::new(p).exists());
        let misc = ["/opt/X11/share/fonts/misc", "/usr/share/fonts/X11/misc"].into_iter().find(|p| std::path::Path::new(p).join("fonts.dir").exists());
        let (Some(xfs), Some(misc)) = (xfs, misc) else {
            eprintln!("no xfs here: skipped");
            return;
        };
        let dir = std::env::temp_dir().join(format!("vintrdp-xfs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("xfs.conf");
        std::fs::write(&conf, format!("catalogue = {misc}/\nuse-syslog = off\nerror-file = {}\n", dir.join("err").display())).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut child = std::process::Command::new(xfs).arg("-config").arg(&conf).arg("-port").arg(port.to_string()).spawn().unwrap();
        let addr = format!("127.0.0.1:{port}");
        let mut conn = None;
        for _ in 0..50 {
            if let Ok(c) = Conn::new(&addr) {
                conn = Some(c);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut c = conn.expect("xfs didn't start");
            let names = c.list(b"-misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso8859-*", 100).unwrap();
            assert!(names.iter().any(|n| n.ends_with("iso8859-1")) && names.iter().any(|n| n.ends_with("iso8859-2")), "{names:?}");
            assert!(c.open(b"-no-such-font-*").unwrap().is_none());
            for (file, name) in [("6x13-ISO8859-1.pcf.gz", "-misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso8859-1"), ("6x13-ISO8859-2.pcf.gz", "-misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso8859-2"), ("9x15B.pcf.gz", "-misc-fixed-bold-r-normal--15-140-75-75-c-90-iso10646-1")] {
                let Ok(data) = std::fs::read(std::path::Path::new(misc).join(file)) else { continue };
                let want = crate::pcf::load(&data).unwrap();
                let got = c.open(name.as_bytes()).unwrap().unwrap_or_else(|| panic!("{name}"));
                assert_eq!((got.min_byte1, got.max_byte1, got.min_char, got.max_char, got.default_char), (want.min_byte1, want.max_byte1, want.min_char, want.max_char, want.default_char), "{name}");
                assert_eq!((got.ascent, got.descent, got.all_exist), (want.ascent, want.descent, want.all_exist), "{name}");
                assert_eq!((got.min_bounds, got.max_bounds), (want.min_bounds, want.max_bounds), "{name}");
                assert_eq!(got.glyphs.len(), want.glyphs.len());
                for (i, (g, w)) in got.glyphs.iter().zip(&want.glyphs).enumerate() {
                    match (g, w) {
                        (Some(g), Some(w)) => {
                            // A font server gives the ink as the metrics (so
                            // X.org's fs fonts have them), and the same pixels.
                            assert_eq!((g.ink, g.m.width), (w.ink, w.m.width), "{name} #{i}");
                            let lit = |g: &Glyph| {
                                let mut v = Vec::new();
                                for y in 0..g.height() {
                                    for x in 0..(g.m.rb - g.m.lb) as usize {
                                        if g.row(y)[x / 64] >> (x % 64) & 1 != 0 {
                                            v.push((x as i16 + g.m.lb, y as i16 - g.m.ascent));
                                        }
                                    }
                                }
                                v
                            };
                            assert_eq!(lit(g), lit(w), "{name} #{i}");
                        }
                        (None, None) => {}
                        _ => panic!("{name} #{i}: {} vs {}", g.is_some(), w.is_some()),
                    }
                }
                let prop = |f: &Font, k: &str| f.properties.iter().find(|p| p.0 == k).map(|p| match &p.1 { PropValue::Atom(s) => s.clone(), PropValue::Int(i) => i.to_string() });
                for k in ["FOUNDRY", "PIXEL_SIZE", "CHARSET_REGISTRY", "POINT_SIZE"] {
                    assert_eq!(prop(&got, k), prop(&want, k), "{name} {k}");
                }
            }
        }));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
}
