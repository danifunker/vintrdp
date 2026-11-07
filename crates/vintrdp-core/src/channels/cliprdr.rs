//! The clipboard channel ("cliprdr"): text both ways, and files through the
//! transfer drive. Port of rdesktop's cliprdr.c with the old clip.rs.
//!
//! Text is held with `\n` line endings, and sent with Windows' `\r\n`. We
//! never send a capabilities PDU, so RDP 5.0-5.2 servers all use short
//! format names: 32 bytes each, UTF-16 or (from older servers) ASCII.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use super::{Channel, Ctx, OPTION_COMPRESS_RDP, OPTION_ENCRYPT_RDP, OPTION_INITIALIZED, OPTION_SHOW_PROTOCOL};
use crate::error::Result;
use crate::session::{Event, Shared};
use crate::stream::{Reader, Writer};
use crate::text;

const CONNECT: u16 = 1;
const FORMAT_ANNOUNCE: u16 = 2;
const FORMAT_ACK: u16 = 3;
const DATA_REQUEST: u16 = 4;
const DATA_RESPONSE: u16 = 5;
const TEMP_DIRECTORY: u16 = 6;
/// Clipboard capabilities; Server 2003 SP1 sends them on connect. Ignored,
/// which keeps the server on short format names.
#[cfg(test)]
const CLIP_CAPS: u16 = 7;

const REQUEST: u16 = 0;
const RESPONSE: u16 = 1;
const ERROR: u16 = 2;

const CF_TEXT: u32 = 1;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
/// Each format in an announcement: a 32-bit ID and a 32-byte name.
const FORMAT_ENTRY: usize = 36;

pub struct Cliprdr {
    shared: Arc<Shared>,
    /// What we last announced, to announce again if the server was busy.
    last_formats: Vec<u8>,
}

impl Cliprdr {
    pub fn new(shared: Arc<Shared>) -> Self {
        Cliprdr { shared, last_formats: Vec::new() }
    }

    fn announce(&mut self, ctx: &mut Ctx, formats: Vec<u8>) {
        ctx.send(packet(FORMAT_ANNOUNCE, REQUEST, &formats));
        self.last_formats = formats;
    }

    /// The format our side of the clipboard is in, if there's anything.
    fn local_format(&self) -> Option<u32> {
        if !self.shared.clipboard_files().is_empty() {
            Some(CF_HDROP)
        } else if !self.shared.clipboard_text().is_empty() {
            Some(CF_UNICODETEXT)
        } else {
            None
        }
    }

    /// The server is ready for clipboard traffic, and expects our formats.
    fn sync(&mut self, ctx: &mut Ctx) {
        self.shared.clipboard_ready();
        if self.shared.config.transfer.is_some() {
            ctx.send(temp_directory());
        }
        let formats = self.local_format().map(format_list).unwrap_or_default();
        self.announce(ctx, formats);
    }

    /// The server has something new: fetch it straight away if we can use it.
    fn format_announce(&mut self, list: &[u8], ctx: &mut Ctx) {
        if trace() {
            eprintln!("vintrdp: clip: server announced {:?}", formats(list));
        }
        if let Some(format) = pick_format(list, self.shared.config.transfer.is_some()) {
            self.shared.set_clipboard_format(format);
            ctx.send(data_request(format));
        }
    }

    fn handle_data(&mut self, bytes: &[u8]) {
        let sh = &self.shared;
        if sh.clipboard_format() == CF_HDROP {
            let names = parse_drop_files(bytes);
            let transfer = sh.config.transfer.clone().unwrap_or_default();
            let files: Vec<_> = names.iter().filter_map(|n| local_path(&transfer, n)).filter(|p| p.exists()).collect();
            if trace() {
                eprintln!("vintrdp: clip: server files {names:?} -> {files:?}");
            }
            if !files.is_empty() {
                sh.note_remote_files();
                sh.frontend.notify(Event::ClipboardFiles(files));
            }
            return;
        }
        let text = if sh.clipboard_format() == CF_TEXT { text::from_cp1252(bytes) } else { text::from_utf16le(bytes) };
        let text = text.replace("\r\n", "\n");
        if sh.note_remote_clipboard(&text) {
            sh.frontend.notify(Event::Clipboard(text));
        }
    }

    /// The server wants what we announced.
    fn request_data(&self, format: u32) -> Vec<u8> {
        let text = self.shared.clipboard_text();
        match format {
            CF_UNICODETEXT => to_unicode_text(&text),
            CF_TEXT => to_ansi_text(&text),
            CF_HDROP => drop_files(&self.shared.clipboard_files()),
            _ => Vec::new(),
        }
    }
}

impl Channel for Cliprdr {
    fn name(&self) -> &'static str {
        "cliprdr"
    }

    fn options(&self) -> u32 {
        OPTION_INITIALIZED | OPTION_ENCRYPT_RDP | OPTION_COMPRESS_RDP | OPTION_SHOW_PROTOCOL
    }

    fn connected(&mut self, _ctx: &mut Ctx) {
        self.last_formats.clear();
    }

    fn process(&mut self, data: &[u8], ctx: &mut Ctx) -> Result<()> {
        match parse(data)? {
            Pdu::Connect => self.sync(ctx),
            Pdu::FormatList(list) => {
                self.format_announce(list, ctx);
                ctx.send(packet(FORMAT_ACK, RESPONSE, &[]));
            }
            // We seem to get this when we announce while the server is still
            // busy with a paste; announcing again sorts it out.
            Pdu::AnnounceFailed => {
                let formats = self.last_formats.clone();
                self.announce(ctx, formats);
            }
            Pdu::DataRequest(format) => {
                let bytes = self.request_data(format);
                ctx.send(packet(DATA_RESPONSE, RESPONSE, &bytes));
            }
            Pdu::DataResponse(bytes) => self.handle_data(bytes),
            Pdu::Ignored(kind, status) => {
                if trace() {
                    eprintln!("vintrdp: clip: ignored type {kind} status {status}");
                }
            }
        }
        Ok(())
    }

    /// Tell the server about new local text or files, if there are any.
    fn poll(&mut self, ctx: &mut Ctx) {
        if self.shared.take_clipboard_change() {
            let format = if self.shared.clipboard_files().is_empty() { CF_UNICODETEXT } else { CF_HDROP };
            self.announce(ctx, format_list(format));
        }
    }
}

/// A message from the server.
#[derive(Debug, PartialEq)]
enum Pdu<'a> {
    Connect,
    /// The formats the server's clipboard holds, as 36-byte entries.
    FormatList(&'a [u8]),
    /// The server refused our format list.
    AnnounceFailed,
    DataRequest(u32),
    DataResponse(&'a [u8]),
    /// Everything else, failed data requests included: there's nothing to do.
    Ignored(u16, u16),
}

fn parse(data: &[u8]) -> Result<Pdu<'_>> {
    let mut r = Reader::new(data);
    let kind = r.u16()?;
    let status = r.u16()?;
    let length = r.u32()? as usize;
    // Trust what's there over the length the server claims.
    let body = r.bytes(length.min(r.remaining()))?;
    if status == ERROR {
        return Ok(if kind == FORMAT_ACK { Pdu::AnnounceFailed } else { Pdu::Ignored(kind, status) });
    }
    Ok(match kind {
        CONNECT => Pdu::Connect,
        FORMAT_ANNOUNCE => Pdu::FormatList(body),
        DATA_REQUEST => Pdu::DataRequest(Reader::new(body).u32()?),
        DATA_RESPONSE => Pdu::DataResponse(body),
        _ => Pdu::Ignored(kind, status),
    })
}

/// A CLIPRDR message: type, status, length, the data and 4 bytes of padding.
fn packet(kind: u16, status: u16, data: &[u8]) -> Vec<u8> {
    let mut w = Writer::with_capacity(data.len() + 12);
    w.u16(kind).u16(status).u32(data.len() as u32).bytes(data).u32(0);
    w.into_vec()
}

/// One format with no name: all we ever offer.
fn format_list(format: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(FORMAT_ENTRY);
    w.u32(format).zeros(FORMAT_ENTRY - 4);
    w.into_vec()
}

fn data_request(format: u32) -> Vec<u8> {
    packet(DATA_REQUEST, REQUEST, &format.to_le_bytes())
}

/// Without this message a server treats the client as having no way to move
/// files, and keeps them off the clipboard in both directions. The path is on
/// the redirected drive; a fixed 260 UTF-16 characters.
fn temp_directory() -> Vec<u8> {
    let mut path = Writer::with_capacity(520);
    path.utf16_fixed("C:\\temp", 520);
    packet(TEMP_DIRECTORY, REQUEST, path.as_slice())
}

fn to_unicode_text(text: &str) -> Vec<u8> {
    text::to_utf16le(&text.replace('\n', "\r\n"), true)
}

/// CF_TEXT is in the server's ANSI code page: Windows-1252 for Western text.
fn to_ansi_text(text: &str) -> Vec<u8> {
    let mut bytes = text::to_cp1252(&text.replace('\n', "\r\n"));
    bytes.push(0);
    bytes
}

/// A DROPFILES structure: Windows' clipboard form of a list of files.
fn drop_files(paths: &[String]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(20); // where the names start
    w.zeros(12); // drop point and fNC
    w.u32(1); // fWide
    for path in paths {
        w.utf16(path).u16(0);
    }
    w.u16(0);
    w.into_vec()
}

/// The file names in a DROPFILES structure.
fn parse_drop_files(data: &[u8]) -> Vec<String> {
    let mut r = Reader::new(data);
    let (Ok(start), Ok(_), Ok(_), Ok(wide)) = (r.u32(), r.u64(), r.u32(), r.u32()) else { return Vec::new() };
    let Some(list) = usize::try_from(start).ok().and_then(|s| data.get(s..)) else { return Vec::new() };
    let names: Vec<String> = if wide != 0 {
        let units: Vec<u16> = list.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        units.split(|&u| u == 0).map(String::from_utf16_lossy).collect()
    } else {
        list.split(|&b| b == 0).map(text::from_cp1252).collect()
    };
    names.into_iter().take_while(|n| !n.is_empty()).collect()
}

/// Where a server path on the transfer drive is locally. RDP 5.2 servers copy
/// files the user copies into the client's temporary directory (see
/// [`temp_directory`]) and name them as `C:\temp\...` or through
/// `\\tsclient\C\...`. Anything else is on the server's own disks.
fn local_path(transfer: &Path, path: &str) -> Option<PathBuf> {
    let lower = path.to_ascii_lowercase();
    let rest = ["c:\\", "\\\\tsclient\\c\\"].iter().find_map(|p| lower.starts_with(p).then(|| &path[p.len()..]))?;
    let mut local = transfer.to_path_buf();
    for part in rest.split('\\').filter(|p| !p.is_empty()) {
        if part == ".." {
            return None;
        }
        local.push(part);
    }
    Some(local)
}

/// VINTRDP_TRACE_CLIP=1 logs clipboard traffic.
fn trace() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VINTRDP_TRACE_CLIP").is_some())
}

/// Each announced format's ID and name (short names: 32 bytes of UTF-16, or
/// of ASCII from older servers).
fn formats(list: &[u8]) -> Vec<(u32, String)> {
    list.chunks_exact(FORMAT_ENTRY)
        .map(|e| {
            let id = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
            let raw = &e[4..];
            let wide = raw[1] == 0 && raw[0] != 0;
            let name = if wide { text::from_utf16le(raw) } else { text::from_cp1252(raw) };
            (id, name)
        })
        .collect()
}

/// The format to fetch from a server announcement: files when we can take
/// them, else the best text.
fn pick_format(list: &[u8], files: bool) -> Option<u32> {
    let ids: Vec<u32> = formats(list).into_iter().map(|(id, _)| id).collect();
    let wanted: &[u32] = if files { &[CF_HDROP, CF_UNICODETEXT, CF_TEXT] } else { &[CF_UNICODETEXT, CF_TEXT] };
    wanted.iter().copied().find(|f| ids.contains(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u32) -> Vec<u8> {
        format_list(id)
    }

    #[test]
    fn text_round_trips() {
        let bytes = to_unicode_text("héllo\nwörld");
        assert_eq!(text::from_utf16le(&bytes).replace("\r\n", "\n"), "héllo\nwörld");
        assert_eq!(text::from_cp1252(&to_ansi_text("caf\u{e9} \u{20ac}\n")), "caf\u{e9} \u{20ac}\r\n");
        assert_eq!(to_ansi_text("\u{4e16}"), [b'?', 0]);
    }

    #[test]
    fn file_lists_are_dropfiles() {
        let data = drop_files(&["C:\\a".to_string()]);
        assert_eq!(&data[..4], &[20, 0, 0, 0]);
        assert_eq!(&data[16..20], &[1, 0, 0, 0]);
        assert_eq!(text::from_utf16le(&data[20..]), "C:\\a");
        assert_eq!(&data[data.len() - 4..], &[0, 0, 0, 0]);
        // Hostile ones give nothing rather than a panic.
        assert!(parse_drop_files(&[0xff; 20]).is_empty());
        assert!(parse_drop_files(&[1, 2]).is_empty());
    }

    #[test]
    fn prefers_unicode() {
        assert_eq!(pick_format(&[entry(1), entry(13)].concat(), true), Some(13));
        assert_eq!(pick_format(&entry(1), true), Some(1));
        assert_eq!(pick_format(&entry(8), true), None);
        assert_eq!(pick_format(&[entry(13), entry(15)].concat(), true), Some(15));
        assert_eq!(pick_format(&[entry(13), entry(15)].concat(), false), Some(13));
        // A partial entry at the end is ignored.
        assert_eq!(pick_format(&[entry(1), vec![13, 0, 0]].concat(), true), Some(1));
    }

    #[test]
    fn server_files_map_to_the_transfer_drive() {
        let data = drop_files(&["C:\\temp\\_TS1.tmp\\a b.txt".to_string(), "D:\\x".to_string()]);
        let names = parse_drop_files(&data);
        assert_eq!(names, ["C:\\temp\\_TS1.tmp\\a b.txt", "D:\\x"]);
        let t = Path::new("/t");
        assert_eq!(local_path(t, &names[0]), Some(t.join("temp").join("_TS1.tmp").join("a b.txt")));
        assert_eq!(local_path(t, "\\\\TSCLIENT\\C\\temp\\f"), Some(t.join("temp").join("f")));
        assert_eq!(local_path(t, &names[1]), None);
        assert_eq!(local_path(t, "C:\\..\\etc"), None);
    }

    #[test]
    fn format_names_in_both_styles() {
        // Windows 2000 and later: UTF-16.
        let mut wide = Writer::new();
        // A name that fills all 32 bytes has no NUL.
        wide.u32(0xc004).utf16("Rich Text Format");
        // Older servers: ASCII.
        let mut ascii = Writer::new();
        ascii.u32(0xc00a).bytes(b"Native").zeros(32 - 6);
        let list = [wide.into_vec(), ascii.into_vec(), entry(CF_UNICODETEXT)].concat();
        assert_eq!(formats(&list), [(0xc004, "Rich Text Format".to_string()), (0xc00a, "Native".to_string()), (13, String::new())]);
    }

    #[test]
    fn parses_server_messages() {
        assert_eq!(parse(&packet(CONNECT, REQUEST, &[])).unwrap(), Pdu::Connect);
        let list = entry(CF_TEXT);
        assert_eq!(parse(&packet(FORMAT_ANNOUNCE, REQUEST, &list)).unwrap(), Pdu::FormatList(&list));
        assert_eq!(parse(&packet(FORMAT_ACK, ERROR, &[])).unwrap(), Pdu::AnnounceFailed);
        assert_eq!(parse(&packet(FORMAT_ACK, RESPONSE, &[])).unwrap(), Pdu::Ignored(FORMAT_ACK, RESPONSE));
        assert_eq!(parse(&packet(DATA_RESPONSE, ERROR, &[])).unwrap(), Pdu::Ignored(DATA_RESPONSE, ERROR));
        assert_eq!(parse(&data_request(CF_UNICODETEXT)).unwrap(), Pdu::DataRequest(13));
        assert_eq!(parse(&packet(DATA_RESPONSE, RESPONSE, b"hi\0")).unwrap(), Pdu::DataResponse(b"hi\0"));
        assert_eq!(parse(&packet(CLIP_CAPS, REQUEST, &[1, 0, 0, 0])).unwrap(), Pdu::Ignored(CLIP_CAPS, REQUEST));
        // A length past the end is cut to what's there; a short header fails.
        assert_eq!(parse(&[5, 0, 1, 0, 0xff, 0xff, 0, 0, b'x']).unwrap(), Pdu::DataResponse(b"x"));
        assert!(parse(&[4, 0, 0, 0, 2, 0, 0, 0, 1, 0]).is_err());
        assert!(parse(&[1, 0]).is_err());
    }

    #[test]
    fn builds_client_messages() {
        assert_eq!(data_request(CF_TEXT), [4, 0, 0, 0, 4, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        let announce = packet(FORMAT_ANNOUNCE, REQUEST, &format_list(CF_HDROP));
        assert_eq!(announce.len(), 8 + 36 + 4);
        assert_eq!(&announce[..12], &[2, 0, 0, 0, 36, 0, 0, 0, 15, 0, 0, 0]);
        let response = packet(DATA_RESPONSE, RESPONSE, &to_unicode_text("a"));
        assert_eq!(response, [5, 0, 1, 0, 4, 0, 0, 0, b'a', 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn temp_directory_is_a_fixed_path() {
        let pdu = temp_directory();
        assert_eq!(pdu.len(), 8 + 520 + 4);
        assert_eq!(&pdu[..8], &[6, 0, 0, 0, 0x08, 0x02, 0, 0]);
        assert_eq!(text::from_utf16le(&pdu[8..528]), "C:\\temp");
        assert!(pdu[8 + 14..].iter().all(|&b| b == 0));
    }
}
