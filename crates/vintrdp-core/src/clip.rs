//! Text clipboard over rdesktop's cliprdr channel. Everything here runs on the
//! rdesktop thread; `Session::set_clipboard` hands text in through `Shared`.
//! Text is held with `\n` line endings, and sent with Windows' `\r\n`.

use std::slice;

use crate::ffi::{cliprdr_send_temp_directory, cliprdr_send_data, cliprdr_send_data_request, cliprdr_send_native_format_announce, cliprdr_send_simple_native_format_announce};
use crate::session::{Event, shared};

const CF_TEXT: u32 = 1;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
/// Each format in an announcement: a 32-bit ID and a 32-byte name.
const FORMAT_ENTRY: usize = 36;

fn from_utf16(data: &[u8]) -> String {
    let units: Vec<u16> = data.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

fn to_utf16(text: &str) -> Vec<u8> {
    text.replace('\n', "\r\n").encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
}

/// CF_TEXT is the server's ANSI code page; Latin-1 gets Western text right.
fn from_ansi(data: &[u8]) -> String {
    data.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect()
}

fn to_ansi(text: &str) -> Vec<u8> {
    text.replace('\n', "\r\n").chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).chain(Some(0)).collect()
}

/// A DROPFILES structure: Windows' clipboard form of a list of files.
fn drop_files(paths: &[String]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend(20u32.to_le_bytes()); // where the names start
    data.extend([0u8; 12]); // drop point and fNC
    data.extend(1u32.to_le_bytes()); // fWide
    for path in paths {
        data.extend(path.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes));
    }
    data.extend([0, 0]);
    data
}

/// The file names in a DROPFILES structure.
fn parse_drop_files(data: &[u8]) -> Vec<String> {
    let word = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let (Some(start), Some(wide)) = (word(0), word(16)) else { return Vec::new() };
    let Some(list) = data.get(start..) else { return Vec::new() };
    let names: Vec<String> = if wide != 0 {
        let units: Vec<u16> = list.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        units.split(|&u| u == 0).map(String::from_utf16_lossy).collect()
    } else {
        list.split(|&b| b == 0).map(|n| n.iter().map(|&b| b as char).collect()).collect()
    };
    names.into_iter().take_while(|n| !n.is_empty()).collect()
}

/// Where a server path on the transfer drive is locally. RDP 5.2 servers copy
/// files the user copies into the client's temporary directory (see
/// [`send_temp_directory`]) and name them as `C:\temp\...` or through
/// `\\tsclient\C\...`. Anything else is on the server's own disks.
fn local_path(transfer: &std::path::Path, path: &str) -> Option<std::path::PathBuf> {
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

/// Without this message a server treats the client as having no way to move
/// files, and keeps them off the clipboard in both directions. The path is on
/// the redirected drive; a fixed 260 UTF-16 characters.
fn send_temp_directory() {
    let mut path = [0u8; 520];
    for (i, unit) in "C:\\temp".encode_utf16().enumerate() {
        path[i * 2..i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
    // SAFETY: as in pump(); the C side copies the bytes.
    unsafe { cliprdr_send_temp_directory(path.as_mut_ptr(), path.len() as u32) };
}

/// VINTRDP_TRACE_CLIP=1 logs clipboard traffic.
fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VINTRDP_TRACE_CLIP").is_some())
}

/// Each announced format's ID and name (RDP 5 short names: 32 bytes of
/// UTF-16, or of ASCII from older servers).
fn formats(data: &[u8]) -> Vec<(u32, String)> {
    data.chunks_exact(FORMAT_ENTRY)
        .map(|e| {
            let id = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
            let raw = &e[4..];
            let wide = raw.len() >= 2 && raw[1] == 0 && raw[0] != 0;
            let name = if wide { from_utf16(raw) } else { from_ansi(raw) };
            (id, name)
        })
        .collect()
}

/// The format to fetch from a server announcement: files when we can take
/// them, else the best text.
fn pick_format(formats: &[u8], files: bool) -> Option<u32> {
    let ids: Vec<u32> = formats.chunks_exact(FORMAT_ENTRY).map(|e| u32::from_le_bytes([e[0], e[1], e[2], e[3]])).collect();
    let wanted: &[u32] = if files { &[CF_HDROP, CF_UNICODETEXT, CF_TEXT] } else { &[CF_UNICODETEXT, CF_TEXT] };
    wanted.iter().copied().find(|f| ids.contains(f))
}

/// Tell the server about new local text, if there is any. From the main loop.
pub(crate) fn pump() {
    let sh = shared();
    if sh.take_clipboard_change() {
        let format = if sh.clipboard_files().is_empty() { CF_UNICODETEXT } else { CF_HDROP };
        // SAFETY: rdesktop is connected; we're on its thread.
        unsafe { cliprdr_send_simple_native_format_announce(format) };
    }
}

/// The server is ready for clipboard traffic, and expects our formats.
#[unsafe(no_mangle)]
pub extern "C" fn ui_clip_sync() {
    shared().clipboard_ready();
    if shared().config.transfer.is_some() {
        send_temp_directory();
    }
    // SAFETY: as in pump(); an empty announcement is valid.
    unsafe {
        match shared().clipboard_text().is_empty() {
            true => cliprdr_send_native_format_announce(std::ptr::null_mut(), 0),
            false => cliprdr_send_simple_native_format_announce(CF_UNICODETEXT),
        }
    }
}

/// The server has something new: fetch it straight away if it's text.
///
/// # Safety
/// `data` points to `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_clip_format_announce(data: *const u8, length: u32) {
    // SAFETY: as documented.
    let formats = if data.is_null() { &[][..] } else { unsafe { slice::from_raw_parts(data, length as usize) } };
    if trace() {
        eprintln!("vintrdp: clip: server announced {:?}", self::formats(formats));
    }
    if let Some(format) = pick_format(formats, shared().config.transfer.is_some()) {
        shared().set_clipboard_format(format);
        // SAFETY: as in pump().
        unsafe { cliprdr_send_data_request(format) };
    }
}

/// # Safety
/// `data` points to `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_clip_handle_data(data: *const u8, length: u32) {
    if data.is_null() {
        return;
    }
    // SAFETY: as documented.
    let bytes = unsafe { slice::from_raw_parts(data, length as usize) };
    let sh = shared();
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
    let text = if sh.clipboard_format() == CF_TEXT { from_ansi(bytes) } else { from_utf16(bytes) }.replace("\r\n", "\n");
    if sh.note_remote_clipboard(&text) {
        sh.frontend.notify(Event::Clipboard(text));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_clip_request_failed() {}

/// The server wants the text we announced.
#[unsafe(no_mangle)]
pub extern "C" fn ui_clip_request_data(format: u32) {
    let text = shared().clipboard_text();
    let mut bytes = match format {
        CF_UNICODETEXT => to_utf16(&text),
        CF_TEXT => to_ansi(&text),
        CF_HDROP => drop_files(&shared().clipboard_files()),
        _ => Vec::new(),
    };
    // SAFETY: as in pump(); the C side copies the bytes.
    unsafe { cliprdr_send_data(bytes.as_mut_ptr(), bytes.len() as u32) };
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_clip_set_mode(_mode: *const std::os::raw::c_char) {}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips() {
        let bytes = to_utf16("héllo\nwörld");
        assert_eq!(from_utf16(&bytes).replace("\r\n", "\n"), "héllo\nwörld");
        assert_eq!(from_ansi(&to_ansi("caf\u{e9}\n")), "caf\u{e9}\r\n");
        assert_eq!(to_ansi("\u{4e16}"), [b'?', 0]);
    }

    #[test]
    fn file_lists_are_dropfiles() {
        let data = drop_files(&["C:\\a".to_string()]);
        assert_eq!(&data[..4], &[20, 0, 0, 0]);
        assert_eq!(&data[16..20], &[1, 0, 0, 0]);
        assert_eq!(from_utf16(&data[20..]), "C:\\a");
        assert_eq!(&data[data.len() - 4..], &[0, 0, 0, 0]);
    }

    #[test]
    fn prefers_unicode() {
        let entry = |id: u32| {
            let mut e = id.to_le_bytes().to_vec();
            e.resize(FORMAT_ENTRY, 0);
            e
        };
        assert_eq!(pick_format(&[entry(1), entry(13)].concat(), true), Some(13));
        assert_eq!(pick_format(&entry(1), true), Some(1));
        assert_eq!(pick_format(&entry(8), true), None);
        assert_eq!(pick_format(&[entry(13), entry(15)].concat(), true), Some(15));
        assert_eq!(pick_format(&[entry(13), entry(15)].concat(), false), Some(13));
    }

    #[test]
    fn server_files_map_to_the_transfer_drive() {
        let data = drop_files(&["C:\\temp\\_TS1.tmp\\a b.txt".to_string(), "D:\\x".to_string()]);
        let names = parse_drop_files(&data);
        assert_eq!(names, ["C:\\temp\\_TS1.tmp\\a b.txt", "D:\\x"]);
        let t = std::path::Path::new("/t");
        assert_eq!(local_path(t, &names[0]), Some(t.join("temp").join("_TS1.tmp").join("a b.txt")));
        assert_eq!(local_path(t, "\\\\TSCLIENT\\C\\temp\\f"), Some(t.join("temp").join("f")));
        assert_eq!(local_path(t, &names[1]), None);
        assert_eq!(local_path(t, "C:\\..\\etc"), None);
    }
}
