//! The persistent bitmap cache: bitmap cache 2 kept on disk between
//! sessions, so the server can skip sending bitmaps we already hold. The
//! files are rdesktop's, `~/.rdesktop/cache/pstcache_<namespace>_<id>_<Bpp>`:
//! fixed-size cells of a 16-byte header (key, width, height, length, stamp)
//! and the bitmap in the server's pixel format, rows top first. Port of
//! rdesktop's pstcache.c.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// Cells in the persistent cache (BMPCACHE2_NUM_PSTCELLS).
pub const NUM_PSTCELLS: usize = 0x9f6;
/// Pixels a cell holds.
const MAX_CELL_SIZE: u64 = 0x1000;
const HEADER: u64 = 16;

/// A cell's header, as CELLHEADER (little-endian on disk).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellHeader {
    pub key: [u8; 8],
    pub width: u8,
    pub height: u8,
    pub length: u16,
    pub stamp: u32,
}

impl CellHeader {
    fn parse(b: &[u8; 16]) -> Self {
        CellHeader {
            key: b[..8].try_into().unwrap_or_default(),
            width: b[8],
            height: b[9],
            length: u16::from_le_bytes([b[10], b[11]]),
            stamp: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        }
    }

    fn bytes(&self) -> [u8; 16] {
        let mut b = [0; 16];
        b[..8].copy_from_slice(&self.key);
        b[8] = self.width;
        b[9] = self.height;
        b[10..12].copy_from_slice(&self.length.to_le_bytes());
        b[12..].copy_from_slice(&self.stamp.to_le_bytes());
        b
    }
}

/// rdesktop's file name part for a connection: a djb2 hash of
/// `server_port_domain_user` and a filesystem-safe copy of it.
pub fn namespace(server: &str, port: u16, domain: &str, username: &str) -> String {
    let server = if server.is_empty() { "unknown" } else { server };
    let username = if username.is_empty() { "nouser" } else { username };
    let mut raw = format!("{server}_{port}_{domain}_{username}");
    truncate(&mut raw, 511);
    let hash = raw.bytes().fold(5381u32, |h, c| h.wrapping_mul(33).wrapping_add(c as u32));
    let mut clean = String::new();
    for c in raw.bytes() {
        if clean.len() >= 95 {
            break;
        }
        if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' {
            clean.push(c as char);
        } else if !clean.is_empty() && !clean.ends_with('_') {
            clean.push('_');
        }
    }
    if clean.is_empty() {
        clean.push('d');
    }
    let mut ns = format!("{hash:08x}_{clean}");
    truncate(&mut ns, 127);
    ns
}

/// Cut to at most `n` bytes, on a character boundary.
fn truncate(s: &mut String, n: usize) {
    if s.len() > n {
        let mut end = n;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

pub struct PstCache {
    /// Open cache files, by bitmap cache id.
    files: [Option<File>; 3],
    /// Bytes per pixel of the cells, from the session depth.
    bpp_bytes: u64,
    /// The key list has gone to the server. It may only go once per
    /// process: the server disconnects if it comes again.
    pub enumerated: bool,
    /// Where the files go; `None` uses `~/.rdesktop/cache`.
    pub dir: Option<PathBuf>,
}

impl PstCache {
    pub fn new() -> Self {
        PstCache { files: [None, None, None], bpp_bytes: 1, enumerated: false, dir: None }
    }

    pub fn is_open(&self, id: u8) -> bool {
        self.files.get(id as usize).is_some_and(Option::is_some)
    }

    fn cell_offset(&self, idx: u16) -> u64 {
        idx as u64 * (self.bpp_bytes * MAX_CELL_SIZE + HEADER)
    }

    /// Open the file for cache `id` (pstcache_init). Once the keys have gone
    /// to the server the answer can't change, so it's yes from then on.
    pub fn init(&mut self, id: u8, depth: u32, namespace: &str) -> bool {
        if self.enumerated {
            return true;
        }
        let Some(slot) = self.files.get_mut(id as usize) else { return false };
        *slot = None;
        let Some(dir) = self.dir.clone().or_else(|| crate::util::rdesktop_dir().map(|d| d.join("cache"))) else {
            return false;
        };
        if !crate::util::mkdirs(&dir) {
            crate::graphics::warn(format_args!("can't make {}; no persistent bitmap cache", dir.display()));
            return false;
        }
        let bpp_bytes = (depth as u64).div_ceil(8).clamp(1, 4);
        let ns = if namespace.is_empty() { "default" } else { namespace };
        let path = dir.join(format!("pstcache_{ns}_{id}_{bpp_bytes}"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = match options.open(&path) {
            Ok(f) => f,
            Err(e) => {
                crate::graphics::warn(format_args!("can't open {}: {e}", path.display()));
                return false;
            }
        };
        // Another client using the same cache would corrupt it. On Windows
        // one client at a time is the norm, and NT 4.0 lacks what the lock
        // needs, so there it's left out as before.
        #[cfg(not(windows))]
        if file.try_lock().is_err() {
            crate::graphics::warn(format_args!("{} is in use; no persistent bitmap cache", path.display()));
            return false;
        }
        self.bpp_bytes = bpp_bytes;
        *slot = Some(file);
        true
    }

    fn file(&mut self, id: u8, idx: u16) -> Option<&mut File> {
        if idx as usize >= NUM_PSTCELLS {
            return None;
        }
        self.files.get_mut(id as usize)?.as_mut()
    }

    /// Set a cell's MRU stamp (0 marks it evicted from memory).
    pub fn touch(&mut self, id: u8, idx: u16, stamp: u32) {
        let at = self.cell_offset(idx) + 12;
        if let Some(f) = self.file(id, idx) {
            let _ = f.seek(SeekFrom::Start(at)).and_then(|_| f.write_all(&stamp.to_le_bytes()));
        }
    }

    fn read_header(&mut self, id: u8, idx: u16) -> Option<CellHeader> {
        let at = self.cell_offset(idx);
        let f = self.file(id, idx)?;
        let mut b = [0u8; 16];
        f.seek(SeekFrom::Start(at)).ok()?;
        f.read_exact(&mut b).ok()?;
        Some(CellHeader::parse(&b))
    }

    /// A cell's bitmap: width, height and pixels in the server's format.
    pub fn load(&mut self, id: u8, idx: u16) -> Option<(usize, usize, Vec<u8>)> {
        let hdr = self.read_header(id, idx)?;
        let (w, h) = (hdr.width as usize, hdr.height as usize);
        let need = w * h * self.bpp_bytes as usize;
        if hdr.key == [0; 8] || (hdr.length as usize) < need || hdr.length as u64 > self.bpp_bytes * MAX_CELL_SIZE {
            return None;
        }
        let mut data = vec![0; hdr.length as usize];
        self.file(id, idx)?.read_exact(&mut data).ok()?;
        Some((w, h, data))
    }

    /// Store a bitmap the server marked persistent.
    pub fn save(&mut self, id: u8, idx: u16, key: [u8; 8], width: u8, height: u8, data: &[u8]) -> bool {
        let Ok(length) = u16::try_from(data.len()) else { return false };
        if data.len() as u64 > self.bpp_bytes * MAX_CELL_SIZE {
            return false;
        }
        let at = self.cell_offset(idx);
        let Some(f) = self.file(id, idx) else { return false };
        let hdr = CellHeader { key, width, height, length, stamp: 0 };
        f.seek(SeekFrom::Start(at)).and_then(|_| f.write_all(&hdr.bytes())).and_then(|_| f.write_all(data)).is_ok()
    }

    /// The cells in use, in order, up to the first empty one: their keys
    /// and stamps.
    pub fn cells(&mut self, id: u8) -> Vec<CellHeader> {
        let mut out = Vec::new();
        for idx in 0..NUM_PSTCELLS as u16 {
            match self.read_header(id, idx) {
                Some(h) if h.key != [0; 8] => out.push(h),
                _ => break,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vintrdp-pstcache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn namespace_matches_rdesktop() {
        // djb2("a_3389__nouser") and the sanitised name.
        let ns = namespace("a", 3389, "", "");
        let hash = "a_3389__nouser".bytes().fold(5381u32, |h, c| h.wrapping_mul(33).wrapping_add(c as u32));
        assert_eq!(ns, format!("{hash:08x}_a_3389__nouser"));
        assert!(namespace("nt4.lab", 3389, "LAB", "dani").ends_with("_nt4_lab_3389_LAB_dani"));
        assert!(namespace("::1", 1, "", "x").contains("_1_1__x"));
    }

    #[test]
    fn cells_round_trip() {
        let dir = temp_dir("rt");
        let mut p = PstCache::new();
        p.dir = Some(dir.clone());
        assert!(p.init(2, 16, "ns"));
        assert!(dir.join("pstcache_ns_2_2").exists());
        let data: Vec<u8> = (0..2 * 2 * 2).collect();
        assert!(p.save(2, 0, [1; 8], 2, 2, &data));
        assert!(p.save(2, 1, [2; 8], 1, 1, &[9, 9]));
        p.touch(2, 1, 77);
        let cells = p.cells(2);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].key, [2; 8]);
        assert_eq!(cells[1].stamp, 77);
        assert_eq!(p.load(2, 0), Some((2, 2, data)));
        // Out of range, or a cache that isn't open.
        assert!(p.load(2, NUM_PSTCELLS as u16).is_none());
        assert!(p.load(1, 0).is_none());
        assert!(!p.save(2, 0, [1; 8], 128, 128, &vec![0; 128 * 128 * 2]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
