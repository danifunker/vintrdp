//! Drive redirection: a local folder as a drive on the server. Port of
//! rdesktop's disk.c, on `std::fs`.
//!
//! The server names files relative to the drive's root, `\dir\file.txt`;
//! [`resolve`] maps that under the root and refuses anything that would leave
//! it. Byte-range locks are kept here, between the server's own handles: std
//! has no byte-range locking, so they don't bind local programs.

use std::ffi::OsString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

// File attributes.
const FILE_ATTRIBUTE_READONLY: u32 = 0x01;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x02;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

// Access rights.
const FILE_READ_DATA: u32 = 0x0001;
const FILE_WRITE_DATA: u32 = 0x0002;
const FILE_APPEND_DATA: u32 = 0x0004;
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_READ: u32 = 0x8000_0000;

// CreateDisposition.
const FILE_SUPERSEDE: u32 = 0;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_IF: u32 = 3;
const FILE_OVERWRITE: u32 = 4;
const FILE_OVERWRITE_IF: u32 = 5;

// The create response's Information.
const FILE_SUPERSEDED: u8 = 0;
const FILE_OPENED: u8 = 1;
const FILE_CREATED: u8 = 2;
const FILE_OVERWRITTEN: u8 = 3;

// CreateOptions.
const FILE_DIRECTORY_FILE: u32 = 0x0001;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0040;
const FILE_DELETE_ON_CLOSE: u32 = 0x1000;

// File information classes.
const FILE_DIRECTORY_INFORMATION: u32 = 1;
const FILE_FULL_DIRECTORY_INFORMATION: u32 = 2;
const FILE_BOTH_DIRECTORY_INFORMATION: u32 = 3;
const FILE_BASIC_INFORMATION: u32 = 4;
const FILE_STANDARD_INFORMATION: u32 = 5;
const FILE_RENAME_INFORMATION: u32 = 10;
const FILE_NAMES_INFORMATION: u32 = 12;
const FILE_DISPOSITION_INFORMATION: u32 = 13;
const FILE_ALLOCATION_INFORMATION: u32 = 19;
const FILE_END_OF_FILE_INFORMATION: u32 = 20;
/// rdesktop called it FileObjectIdInformation; what it sends is
/// FileAttributeTagInformation's.
const FILE_ATTRIBUTE_TAG_INFORMATION: u32 = 35;

// Volume information classes.
const FILE_FS_VOLUME_INFORMATION: u32 = 1;
const FILE_FS_SIZE_INFORMATION: u32 = 3;
const FILE_FS_ATTRIBUTE_INFORMATION: u32 = 5;
const FILE_FS_FULL_SIZE_INFORMATION: u32 = 7;

const FS_CASE_SENSITIVE: u32 = 0x01;
const FS_CASE_IS_PRESERVED: u32 = 0x02;

// Lock operations.
const RDP_LOWIO_OP_SHAREDLOCK: u32 = 2;
const RDP_LOWIO_OP_EXCLUSIVELOCK: u32 = 3;
const RDP_LOWIO_OP_UNLOCK: u32 = 4;
const RDP_LOWIO_OP_UNLOCK_MULTIPLE: u32 = 5;

/// rdesktop's label and file system name for every drive.
const VOLUME_LABEL: &str = "RDESKTOP";
const FS_TYPE: &str = "RDPFS";

/// The longest search pattern we match; longer ones match nothing.
const MAX_PATTERN: usize = 1024;

/// 100 ns intervals from 1601 (FILETIME's epoch) to 1970.
const EPOCH_DIFF_SECS: u64 = 11_644_473_600;

/// A redirected folder.
pub(super) struct Drive {
    pub root: PathBuf,
}

pub(super) fn device(name: &str, root: PathBuf) -> Device {
    Device { name: name.to_string(), kind: DeviceKind::Disk(Drive { root }) }
}

/// `name`, as the server gives it (`\dir\file`), under `root`; None if it
/// would leave `root`.
pub(super) fn resolve(root: &Path, name: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for part in name.split(['\\', '/']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if !component_ok(part, cfg!(windows)) {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

/// One component of a server path. `..` would climb out of the drive. On
/// Windows more is special: a colon makes a drive-relative path or names a
/// stream, dots and spaces are trimmed (so `.. ` is `..`), and CON or COM1
/// open devices rather than files.
pub(super) fn component_ok(part: &str, windows: bool) -> bool {
    if part == ".." || part.contains('\0') {
        return false;
    }
    if !windows {
        return true;
    }
    if part.chars().any(|c| c < ' ' || "<>:\"|?*".contains(c)) || part.chars().all(|c| c == '.' || c == ' ') {
        return false;
    }
    let stem = part.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    !device
}

/// Whether `name` matches a search pattern the way Windows matches them:
/// without regard to case, with `*` and `?`, and with the DOS wildcards the
/// redirector sends for `*.` and `?.` patterns: `<` (DOS_STAR), `>` (DOS_QM)
/// and `"` (DOS_DOT).
pub(super) fn wildcard_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" || pattern == "*.*" {
        return true;
    }
    let p: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let n: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    if p.len() > MAX_PATTERN {
        return false;
    }
    let last_dot = n.iter().rposition(|&c| c == '.');
    // row[j]: does p[i..] match n[j..]? Built from the end of the pattern.
    let mut next = vec![false; n.len() + 1];
    next[n.len()] = true;
    let mut row = vec![false; n.len() + 1];
    for i in (0..p.len()).rev() {
        for j in (0..=n.len()).rev() {
            let here = j < n.len();
            row[j] = match p[i] {
                '*' => next[j] || (here && row[j + 1]),
                '<' => next[j] || (here && Some(j) != last_dot && row[j + 1]),
                '?' => here && next[j + 1],
                '>' => {
                    if !here || n[j] == '.' {
                        next[j]
                    } else {
                        next[j + 1]
                    }
                }
                '"' => {
                    if !here {
                        next[j]
                    } else {
                        n[j] == '.' && next[j + 1]
                    }
                }
                c => here && n[j] == c && next[j + 1],
            };
        }
        std::mem::swap(&mut row, &mut next);
    }
    next[0]
}

/// A Unix time as a FILETIME.
fn filetime_from_unix(secs: i64, nanos: u32) -> u64 {
    let Some(secs) = secs.checked_add(EPOCH_DIFF_SECS as i64).filter(|&s| s >= 0) else {
        return 0;
    };
    (secs as u64).saturating_mul(10_000_000).saturating_add(u64::from(nanos / 100))
}

fn filetime(t: SystemTime) -> u64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => filetime_from_unix(i64::try_from(d.as_secs()).unwrap_or(i64::MAX), d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            let secs = -i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
            match d.subsec_nanos() {
                0 => filetime_from_unix(secs, 0),
                n => filetime_from_unix(secs - 1, 1_000_000_000 - n),
            }
        }
    }
}

/// A FILETIME the server set, as a time; None for 0 and -1, which leave the
/// time as it is.
fn time_from_filetime(ft: u64) -> Option<SystemTime> {
    if ft == 0 || ft > i64::MAX as u64 {
        return None;
    }
    let secs = ft / 10_000_000;
    let nanos = (ft % 10_000_000) as u32 * 100;
    if secs >= EPOCH_DIFF_SECS {
        UNIX_EPOCH.checked_add(std::time::Duration::new(secs - EPOCH_DIFF_SECS, nanos))
    } else {
        UNIX_EPOCH.checked_sub(std::time::Duration::new(EPOCH_DIFF_SECS - secs, 0))?.checked_add(std::time::Duration::new(0, nanos))
    }
}

/// What the information classes report about a file.
#[derive(Default)]
struct Stat {
    is_dir: bool,
    size: u64,
    links: u32,
    create: u64,
    access: u64,
    write: u64,
    change: u64,
    readonly: bool,
    /// The file's own attributes, where the file system has them.
    attributes: Option<u32>,
}

impl Stat {
    fn of(m: &Metadata) -> Stat {
        let mut st = Stat { is_dir: m.is_dir(), size: m.len(), links: 1, readonly: m.permissions().readonly(), ..Stat::default() };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let (a, w, c) = (
                filetime_from_unix(m.atime(), m.atime_nsec() as u32),
                filetime_from_unix(m.mtime(), m.mtime_nsec() as u32),
                filetime_from_unix(m.ctime(), m.ctime_nsec() as u32),
            );
            st.links = u32::try_from(m.nlink()).unwrap_or(u32::MAX);
            st.readonly = m.mode() & 0o200 == 0;
            st.access = a;
            st.write = w;
            st.change = c;
            // rdesktop's creation time: the earliest of the three, unless 0.
            st.create = match m.created() {
                Ok(t) => filetime(t),
                Err(_) => [a, w, c].into_iter().filter(|&t| t != 0).min().unwrap_or(0),
            };
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            st.create = m.creation_time();
            st.access = m.last_access_time();
            st.write = m.last_write_time();
            st.change = st.write;
            st.attributes = Some(m.file_attributes());
        }
        #[cfg(not(any(unix, windows)))]
        {
            st.access = m.accessed().map(filetime).unwrap_or(0);
            st.write = m.modified().map(filetime).unwrap_or(0);
            st.create = m.created().map(filetime).unwrap_or(st.write);
            st.change = st.write;
        }
        st
    }

    /// The file's attributes as rdesktop made them up: a directory, hidden
    /// if the name starts with a dot, read-only if the owner can't write.
    fn attributes(&self, name: &str) -> u32 {
        if let Some(a) = self.attributes {
            // Keep what the server knows what to do with: no reparse points
            // or sparse files, which it would try to act on.
            return match a & 0x39b7 {
                0 => FILE_ATTRIBUTE_NORMAL,
                a => a,
            };
        }
        let mut a = 0;
        if self.is_dir {
            a |= FILE_ATTRIBUTE_DIRECTORY;
        }
        if name.starts_with('.') && name != "." && name != ".." {
            a |= FILE_ATTRIBUTE_HIDDEN;
        }
        if a == 0 {
            a = FILE_ATTRIBUTE_NORMAL;
        }
        if self.readonly {
            a |= FILE_ATTRIBUTE_READONLY;
        }
        a
    }

    /// The four times, as FILE_BASIC_INFORMATION and the directory classes
    /// lay them out.
    fn write_times(&self, w: &mut Writer) {
        w.u64(self.create).u64(self.access).u64(self.write).u64(self.change);
    }
}

/// What a directory looked like, to tell when it changes.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Snapshot {
    modified: u64,
    entries: u64,
    total: u64,
}

impl Snapshot {
    fn take(dir: &Path) -> io::Result<Snapshot> {
        let m = fs::metadata(dir)?;
        let mut s = Snapshot { modified: Stat::of(&m).write, ..Snapshot::default() };
        for e in fs::read_dir(dir)? {
            let Ok(e) = e else { continue };
            s.entries += 1;
            if let Ok(m) = e.metadata() {
                let st = Stat::of(&m);
                s.total = s.total.wrapping_add(st.write).wrapping_add(st.change).wrapping_add(st.size);
            }
        }
        Ok(s)
    }
}

/// A directory search in progress.
struct Search {
    pattern: String,
    entries: Vec<OsString>,
    next: usize,
}

/// A file or directory the server has open.
pub(super) struct DiskFile {
    pub path: PathBuf,
    /// None for a directory.
    file: Option<File>,
    /// Opened for writing.
    writable: bool,
    is_dir: bool,
    delete_on_close: bool,
    search: Option<Search>,
    notify: Option<Snapshot>,
}

/// How an io::Error reads to the server; rdesktop's mapping.
fn status_of(e: &io::Error) -> u32 {
    match e.kind() {
        io::ErrorKind::NotFound => STATUS_NO_SUCH_FILE,
        io::ErrorKind::PermissionDenied => STATUS_ACCESS_DENIED,
        io::ErrorKind::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
        io::ErrorKind::IsADirectory => STATUS_FILE_IS_A_DIRECTORY,
        io::ErrorKind::NotADirectory => STATUS_NOT_A_DIRECTORY,
        io::ErrorKind::DirectoryNotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => STATUS_DISK_FULL,
        io::ErrorKind::InvalidFilename => STATUS_OBJECT_NAME_INVALID,
        _ => STATUS_NO_SUCH_FILE,
    }
}

impl DiskFile {
    /// IRP_MJ_CREATE: open or create `name` under `root`. Returns the file
    /// and the response's Information (opened, created, overwritten).
    pub fn create(root: &Path, access: u32, disposition: u32, options: u32, name: &str) -> std::result::Result<(DiskFile, u8), u32> {
        let path = resolve(root, name).ok_or(STATUS_ACCESS_DENIED)?;
        let existing = fs::metadata(&path).ok();
        let exists = existing.is_some();
        let existing_dir = existing.as_ref().is_some_and(Metadata::is_dir);
        if existing_dir && options & FILE_NON_DIRECTORY_FILE != 0 {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        }
        let delete_on_close = options & FILE_DELETE_ON_CLOSE != 0;
        let info_for = |created: bool| match disposition {
            _ if created => FILE_CREATED,
            FILE_SUPERSEDE => FILE_SUPERSEDED,
            FILE_OVERWRITE | FILE_OVERWRITE_IF => FILE_OVERWRITTEN,
            _ => FILE_OPENED,
        };

        if existing_dir || options & FILE_DIRECTORY_FILE != 0 {
            let created = match (disposition, exists) {
                (FILE_CREATE, true) => return Err(STATUS_OBJECT_NAME_COLLISION),
                (FILE_OPEN | FILE_OVERWRITE, false) => return Err(STATUS_NO_SUCH_FILE),
                (_, false) => {
                    fs::create_dir(&path).map_err(|e| status_of(&e))?;
                    true
                }
                (_, true) => false,
            };
            if !fs::metadata(&path).map_err(|e| status_of(&e))?.is_dir() {
                return Err(STATUS_NOT_A_DIRECTORY);
            }
            let f = DiskFile { path, file: None, writable: false, is_dir: true, delete_on_close, search: None, notify: None };
            return Ok((f, if created { FILE_CREATED } else { FILE_OPENED }));
        }

        let read = access & (GENERIC_READ | GENERIC_ALL | FILE_READ_DATA | MAXIMUM_ALLOWED) != 0;
        let mut write = access & (GENERIC_WRITE | GENERIC_ALL | FILE_WRITE_DATA | FILE_APPEND_DATA | MAXIMUM_ALLOWED) != 0;
        let mut opts = OpenOptions::new();
        // std won't create or truncate a file it isn't opening for writing.
        match disposition {
            FILE_OPEN => {}
            FILE_CREATE => {
                opts.create_new(true);
                write = true;
            }
            FILE_OPEN_IF => {
                opts.create(true);
                write = true;
            }
            FILE_OVERWRITE => {
                opts.truncate(true);
                write = true;
            }
            FILE_SUPERSEDE | FILE_OVERWRITE_IF => {
                opts.create(true).truncate(true);
                write = true;
            }
            _ => return Err(STATUS_INVALID_PARAMETER),
        }
        opts.read(read || !write).write(write);
        let (file, writable) = match opts.open(&path) {
            Ok(f) => (f, write),
            // MAXIMUM_ALLOWED takes what it can get.
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && access & MAXIMUM_ALLOWED != 0 && disposition == FILE_OPEN => {
                (File::open(&path).map_err(|e| status_of(&e))?, false)
            }
            Err(e) => return Err(status_of(&e)),
        };
        let f = DiskFile { path, file: Some(file), writable, is_dir: false, delete_on_close, search: None, notify: None };
        Ok((f, info_for(!exists)))
    }

    /// IRP_MJ_CLOSE, deleting the file if the server asked to.
    pub fn close(self) -> u32 {
        drop(self.file);
        if self.delete_on_close {
            let gone = if self.is_dir { fs::remove_dir(&self.path) } else { fs::remove_file(&self.path) };
            if let Err(e) = gone {
                eprintln!("vintrdp: can't delete {}: {e}", self.path.display());
                return STATUS_ACCESS_DENIED;
            }
        }
        STATUS_SUCCESS
    }

    pub fn read(&mut self, length: u32, offset: u64) -> (u32, Vec<u8>) {
        let Some(file) = &self.file else {
            // A directory: with this the server doesn't try again.
            return (STATUS_NOT_IMPLEMENTED, Vec::new());
        };
        // Past any file there can be (offsets are signed underneath).
        if offset > i64::MAX as u64 {
            return (STATUS_SUCCESS, Vec::new());
        }
        let mut buf = vec![0; length as usize];
        let mut n = 0;
        while n < buf.len() {
            let Some(at) = offset.checked_add(n as u64) else { break };
            match read_at(file, &mut buf[n..], at) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    if n > 0 {
                        break;
                    }
                    eprintln!("vintrdp: reading {} failed: {e}", self.path.display());
                    return (STATUS_INVALID_PARAMETER, Vec::new());
                }
            }
        }
        buf.truncate(n);
        (STATUS_SUCCESS, buf)
    }

    /// The status and how much was written.
    pub fn write(&mut self, data: &[u8], offset: u64) -> (u32, u32) {
        let Some(file) = &self.file else {
            return (STATUS_ACCESS_DENIED, 0);
        };
        let mut n = 0;
        while n < data.len() {
            let Some(at) = offset.checked_add(n as u64) else { break };
            match write_at(file, &data[n..], at) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    eprintln!("vintrdp: writing {} failed: {e}", self.path.display());
                    let status = if matches!(e.kind(), io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded) { STATUS_DISK_FULL } else { STATUS_ACCESS_DENIED };
                    return (status, len32(n));
                }
            }
        }
        (STATUS_SUCCESS, len32(n))
    }

    fn metadata(&self) -> io::Result<Metadata> {
        match &self.file {
            Some(f) => f.metadata(),
            None => fs::metadata(&self.path),
        }
    }

    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// IRP_MJ_QUERY_INFORMATION.
    pub fn query_information(&self, class: u32, w: &mut Writer) -> u32 {
        let st = match self.metadata() {
            Ok(m) => Stat::of(&m),
            Err(e) => {
                eprintln!("vintrdp: can't stat {}: {e}", self.path.display());
                w.u8(0);
                return STATUS_ACCESS_DENIED;
            }
        };
        let attributes = st.attributes(&self.name());
        match class {
            FILE_BASIC_INFORMATION => {
                st.write_times(w);
                w.u32(attributes);
            }
            FILE_STANDARD_INFORMATION => {
                w.u64(st.size).u64(st.size); // allocation size, end of file
                w.u32(st.links);
                w.u8(u8::from(self.delete_on_close));
                w.u8(u8::from(st.is_dir));
            }
            FILE_ATTRIBUTE_TAG_INFORMATION => {
                w.u32(attributes).u32(0); // attributes, reparse tag
            }
            _ => return STATUS_INVALID_PARAMETER,
        }
        STATUS_SUCCESS
    }

    /// IRP_MJ_SET_INFORMATION, with `buf` the request's SetBuffer.
    pub fn set_information(&mut self, root: &Path, class: u32, buf: &[u8]) -> u32 {
        let mut r = Reader::new(buf);
        match self.set_information_from(root, class, &mut r) {
            Ok(status) => status,
            Err(_) => STATUS_INVALID_PARAMETER,
        }
    }

    fn set_information_from(&mut self, root: &Path, class: u32, r: &mut Reader) -> Result<u32> {
        Ok(match class {
            FILE_BASIC_INFORMATION => {
                let _create = r.u64()?;
                let access = time_from_filetime(r.u64()?);
                let write = time_from_filetime(r.u64()?);
                let change = time_from_filetime(r.u64()?);
                let attributes = r.u32()?;
                let modified = write.or(change);
                if access.is_some() || modified.is_some() {
                    let mut times = fs::FileTimes::new();
                    if let Some(t) = access {
                        times = times.set_accessed(t);
                    }
                    if let Some(t) = modified {
                        times = times.set_modified(t);
                    }
                    // Like rdesktop, not being allowed to is no failure.
                    if let Err(e) = attribute_handle(&self.path).and_then(|f| f.set_times(times)) {
                        if e.kind() != io::ErrorKind::PermissionDenied {
                            return Ok(STATUS_ACCESS_DENIED);
                        }
                    }
                }
                if attributes != 0 {
                    if let Err(e) = set_readonly(&self.path, attributes & FILE_ATTRIBUTE_READONLY != 0) {
                        eprintln!("vintrdp: can't change {}: {e}", self.path.display());
                        return Ok(STATUS_ACCESS_DENIED);
                    }
                }
                STATUS_SUCCESS
            }
            FILE_RENAME_INFORMATION => {
                let replace = r.u8()? != 0;
                let _root_directory = r.u8()?;
                let length = r.u32()?;
                let name = r.utf16(length as usize)?;
                self.rename(root, &name, replace)
            }
            FILE_DISPOSITION_INFORMATION => {
                // Old servers send no DeletePending byte; that means delete.
                let delete = r.u8().map_or(true, |b| b != 0);
                if delete && self.is_dir && fs::read_dir(&self.path).map(|mut d| d.next().is_some()).unwrap_or(false) {
                    return Ok(STATUS_DIRECTORY_NOT_EMPTY);
                }
                self.delete_on_close = delete;
                STATUS_SUCCESS
            }
            FILE_ALLOCATION_INFORMATION | FILE_END_OF_FILE_INFORMATION => {
                let size = r.u64()?;
                let Some(file) = &self.file else {
                    return Ok(STATUS_INVALID_PARAMETER);
                };
                let current = file.metadata().map(|m| m.len()).unwrap_or(0);
                // Allocating more than the file holds doesn't make it bigger;
                // like Samba without strict allocation, nothing is reserved.
                if class == FILE_ALLOCATION_INFORMATION && size >= current {
                    return Ok(STATUS_SUCCESS);
                }
                // Refuse up front to grow past the free space.
                if size > current {
                    if let Ok(space) = volume_space(&self.path) {
                        if size - current > space.free_bytes() {
                            return Ok(STATUS_DISK_FULL);
                        }
                    }
                }
                match file.set_len(size) {
                    Ok(()) => STATUS_SUCCESS,
                    Err(_) => STATUS_DISK_FULL,
                }
            }
            _ => STATUS_INVALID_PARAMETER,
        })
    }

    fn rename(&mut self, root: &Path, name: &str, replace: bool) -> u32 {
        let Some(to) = resolve(root, name) else {
            return STATUS_ACCESS_DENIED;
        };
        if !replace && to != self.path && fs::symlink_metadata(&to).is_ok() {
            return STATUS_OBJECT_NAME_COLLISION;
        }
        if fs::rename(&self.path, &to).is_err() {
            // Some systems won't rename an open file: close it, rename, and
            // open it again, keeping the server's handle.
            if self.file.take().is_none() {
                return STATUS_ACCESS_DENIED;
            }
            let moved = fs::rename(&self.path, &to).is_ok();
            let now = if moved { &to } else { &self.path };
            self.file = OpenOptions::new().read(true).write(self.writable).open(now).or_else(|_| File::open(now)).ok();
            if !moved {
                return STATUS_ACCESS_DENIED;
            }
        }
        self.path = to;
        STATUS_SUCCESS
    }

    /// IRP_MN_QUERY_DIRECTORY: the next entry matching the search, one per
    /// request as rdesktop did. A pattern (`\dir\*.txt`) starts a new search.
    pub fn query_directory(&mut self, class: u32, initial: bool, pattern: Option<&str>, w: &mut Writer) -> u32 {
        if !matches!(class, FILE_DIRECTORY_INFORMATION | FILE_FULL_DIRECTORY_INFORMATION | FILE_BOTH_DIRECTORY_INFORMATION | FILE_NAMES_INFORMATION) {
            return STATUS_INVALID_PARAMETER;
        }
        if !self.is_dir {
            return STATUS_INVALID_PARAMETER;
        }
        let pattern = pattern.filter(|p| !p.is_empty()).map(|p| p.rsplit(['\\', '/']).next().unwrap_or(p).to_string());
        if pattern.is_some() || initial || self.search.is_none() {
            let entries = match fs::read_dir(&self.path) {
                Ok(d) => [OsString::from("."), OsString::from("..")].into_iter().chain(d.filter_map(|e| e.ok().map(|e| e.file_name()))).collect(),
                Err(e) => {
                    eprintln!("vintrdp: can't list {}: {e}", self.path.display());
                    return STATUS_NO_SUCH_FILE;
                }
            };
            let pattern = pattern.or_else(|| self.search.take().map(|s| s.pattern)).unwrap_or_else(|| "*".into());
            self.search = Some(Search { pattern, entries, next: 0 });
        }
        let Some(search) = &mut self.search else {
            return STATUS_NO_MORE_FILES;
        };
        let (entry, name) = loop {
            let Some(entry) = search.entries.get(search.next) else {
                return STATUS_NO_MORE_FILES;
            };
            search.next += 1;
            let name = entry.to_string_lossy().into_owned();
            if wildcard_match(&search.pattern, &name) {
                break (entry.clone(), name);
            }
        };
        // "." and ".." describe the directory itself: what's above the drive
        // is none of the server's business.
        let full = if name == "." || name == ".." { self.path.clone() } else { self.path.join(&entry) };
        let st = match fs::metadata(&full) {
            Ok(m) => Stat::of(&m),
            // A dangling link or an entry we may not see is listed bare.
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied) => Stat::default(),
            Err(e) if e.raw_os_error() == Some(ELOOP) => Stat::default(),
            Err(e) => {
                eprintln!("vintrdp: can't stat {}: {e}", full.display());
                w.u8(0);
                return STATUS_NO_SUCH_FILE;
            }
        };
        let attributes = st.attributes(&name);
        let name16 = crate::text::to_utf16le(&name, false);
        w.u32(0).u32(0); // NextEntryOffset, FileIndex
        if class != FILE_NAMES_INFORMATION {
            st.write_times(w);
            w.u64(st.size).u64(st.size); // end of file, allocation size
            w.u32(attributes);
        }
        w.u32(len32(name16.len()));
        match class {
            FILE_FULL_DIRECTORY_INFORMATION => {
                w.u32(0); // EaSize
            }
            FILE_BOTH_DIRECTORY_INFORMATION => {
                w.u32(0); // EaSize
                w.u8(0).zeros(24); // no short name
            }
            _ => {}
        }
        w.bytes(&name16);
        STATUS_SUCCESS
    }

    /// IRP_MN_NOTIFY_CHANGE_DIRECTORY: remember the directory as it is.
    pub fn start_notify(&mut self) -> u32 {
        if !self.is_dir {
            return STATUS_INVALID_PARAMETER;
        }
        match Snapshot::take(&self.path) {
            Ok(s) => {
                self.notify = Some(s);
                STATUS_PENDING
            }
            Err(_) => STATUS_ACCESS_DENIED,
        }
    }

    /// Has the directory changed since [`start_notify`]? STATUS_PENDING
    /// while it hasn't; STATUS_NOTIFY_ENUM_DIR sends the server to list it
    /// again, since we don't say what changed.
    pub fn check_notify(&mut self) -> u32 {
        let Some(before) = self.notify else {
            return STATUS_INVALID_DEVICE_REQUEST;
        };
        match Snapshot::take(&self.path) {
            Ok(now) if now == before => STATUS_PENDING,
            Ok(now) => {
                self.notify = Some(now);
                STATUS_NOTIFY_ENUM_DIR
            }
            Err(_) => STATUS_ACCESS_DENIED,
        }
    }
}

#[cfg(unix)]
const ELOOP: i32 = libc::ELOOP;
#[cfg(not(unix))]
const ELOOP: i32 = -1;

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, offset)
}

#[cfg(unix)]
fn write_at(f: &File, data: &[u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(f, data, offset)
}

#[cfg(windows)]
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, offset)
}

#[cfg(windows)]
fn write_at(f: &File, data: &[u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(f, data, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_at(mut f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::io::{Read, Seek, SeekFrom};
    f.seek(SeekFrom::Start(offset))?;
    f.read(buf)
}

#[cfg(not(any(unix, windows)))]
fn write_at(mut f: &File, data: &[u8], offset: u64) -> io::Result<usize> {
    use std::io::{Seek, SeekFrom, Write};
    f.seek(SeekFrom::Start(offset))?;
    f.write(data)
}

/// A handle to `path` (file or directory) that may change its times.
fn attribute_handle(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        // Without it a directory can't be opened at all.
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

/// FILE_ATTRIBUTE_READONLY: on Unix, the owner's write permission, which is
/// what rdesktop changed.
fn set_readonly(path: &Path, readonly: bool) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = perms.mode() & 0o777;
        perms.set_mode(if readonly { mode & !0o222 } else { mode | 0o200 });
    }
    #[cfg(not(unix))]
    perms.set_readonly(readonly);
    fs::set_permissions(path, perms)
}

/// How big a volume is and how much of it is free.
struct Space {
    total_units: u64,
    caller_units: u64,
    free_units: u64,
    sectors_per_unit: u32,
    bytes_per_sector: u32,
    name_max: u32,
}

impl Space {
    fn free_bytes(&self) -> u64 {
        self.caller_units.saturating_mul(u64::from(self.sectors_per_unit)).saturating_mul(u64::from(self.bytes_per_sector))
    }
}

#[cfg(unix)]
fn volume_space(path: &Path) -> io::Result<Space> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: a NUL-terminated path and a zeroed struct for the call to fill.
    let s = unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return Err(io::Error::last_os_error());
        }
        s
    };
    // The field types differ between systems; all are unsigned.
    #[allow(clippy::unnecessary_cast)]
    let (frsize, bsize, blocks, bavail, bfree, namemax) = (s.f_frsize as u64, s.f_bsize as u64, s.f_blocks as u64, s.f_bavail as u64, s.f_bfree as u64, s.f_namemax as u64);
    let unit = if frsize > 0 { frsize } else { bsize };
    Ok(Space {
        total_units: blocks,
        caller_units: bavail,
        free_units: bfree,
        sectors_per_unit: u32::try_from(unit / 0x200).unwrap_or(u32::MAX).max(1),
        bytes_per_sector: 0x200,
        name_max: u32::try_from(namemax).unwrap_or(255),
    })
}

#[cfg(windows)]
fn volume_space(path: &Path) -> io::Result<Space> {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Component;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceW(root: *const u16, sectors_per_cluster: *mut u32, bytes_per_sector: *mut u32, free_clusters: *mut u32, clusters: *mut u32) -> i32;
    }
    // NT 4.0 wants the root of the volume, not any directory on it.
    let mut root = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => root.push(c),
            _ => break,
        }
    }
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let root_ptr = if root.as_os_str().is_empty() { std::ptr::null() } else { wide.as_ptr() };
    let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: a NUL-terminated path (or NULL, the current drive) and out pointers.
    if unsafe { GetDiskFreeSpaceW(root_ptr, &mut spc, &mut bps, &mut free, &mut total) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Space {
        total_units: u64::from(total),
        caller_units: u64::from(free),
        free_units: u64::from(free),
        sectors_per_unit: spc.max(1),
        bytes_per_sector: bps.max(1),
        name_max: 255,
    })
}

/// rdesktop's stand-in where there's no statfs.
#[cfg(not(any(unix, windows)))]
fn volume_space(_path: &Path) -> io::Result<Space> {
    Ok(Space { total_units: 262_144, caller_units: 131_072, free_units: 131_072, sectors_per_unit: 1, bytes_per_sector: 512, name_max: 255 })
}

/// IRP_MJ_QUERY_VOLUME_INFORMATION for the volume `path` is on.
pub(super) fn query_volume_information(path: &Path, class: u32, w: &mut Writer) -> u32 {
    let space = match volume_space(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vintrdp: can't see the free space of {}: {e}", path.display());
            return STATUS_ACCESS_DENIED;
        }
    };
    match class {
        FILE_FS_VOLUME_INFORMATION => {
            let label = crate::text::to_utf16le(VOLUME_LABEL, true);
            w.u64(0); // creation time
            w.u32(0); // serial number
            w.u32(len32(label.len()));
            w.u8(0); // no object support
            w.bytes(&label);
        }
        FILE_FS_SIZE_INFORMATION => {
            w.u64(space.total_units).u64(space.free_units);
            w.u32(space.sectors_per_unit).u32(space.bytes_per_sector);
        }
        FILE_FS_FULL_SIZE_INFORMATION => {
            w.u64(space.total_units).u64(space.caller_units).u64(space.free_units);
            w.u32(space.sectors_per_unit).u32(space.bytes_per_sector);
        }
        FILE_FS_ATTRIBUTE_INFORMATION => {
            let name = crate::text::to_utf16le(FS_TYPE, false);
            w.u32(FS_CASE_SENSITIVE | FS_CASE_IS_PRESERVED);
            w.u32(space.name_max);
            w.u32(len32(name.len()));
            w.bytes(&name);
        }
        _ => return STATUS_INVALID_PARAMETER,
    }
    STATUS_SUCCESS
}

/// A byte-range lock one of the server's handles holds.
struct Lock {
    path: PathBuf,
    handle: u32,
    offset: u64,
    length: u64,
    exclusive: bool,
}

impl Lock {
    fn overlaps(&self, offset: u64, length: u64) -> bool {
        let end = offset.saturating_add(length);
        let mine = self.offset.saturating_add(self.length);
        length > 0 && self.length > 0 && offset < mine && self.offset < end
    }
}

/// The byte-range locks the server holds, by file.
#[derive(Default)]
pub(super) struct Locks(Vec<Lock>);

/// More locks than this from one server is an attack, not a program.
const MAX_LOCKS: usize = 4096;

impl Locks {
    /// IRP_MJ_LOCK_CONTROL: lock or unlock `ranges` (offset, length) of the
    /// file at `path` for `handle`. Locking is all or nothing; a lock that
    /// would have to wait is refused, since we can't wait.
    pub fn apply(&mut self, path: &Path, handle: u32, op: u32, ranges: &[(u64, u64)]) -> u32 {
        match op {
            RDP_LOWIO_OP_SHAREDLOCK | RDP_LOWIO_OP_EXCLUSIVELOCK => {
                let exclusive = op == RDP_LOWIO_OP_EXCLUSIVELOCK;
                let conflict = ranges.iter().any(|&(offset, length)| {
                    self.0.iter().any(|l| l.path == path && l.overlaps(offset, length) && (exclusive || l.exclusive))
                });
                if conflict || self.0.len().saturating_add(ranges.len()) > MAX_LOCKS {
                    return STATUS_LOCK_NOT_GRANTED;
                }
                for &(offset, length) in ranges {
                    self.0.push(Lock { path: path.to_path_buf(), handle, offset, length, exclusive });
                }
                STATUS_SUCCESS
            }
            RDP_LOWIO_OP_UNLOCK | RDP_LOWIO_OP_UNLOCK_MULTIPLE => {
                let mut status = STATUS_SUCCESS;
                for &(offset, length) in ranges {
                    match self.0.iter().position(|l| l.handle == handle && l.offset == offset && l.length == length) {
                        Some(i) => {
                            self.0.remove(i);
                        }
                        None => status = STATUS_RANGE_NOT_LOCKED,
                    }
                }
                status
            }
            _ => STATUS_INVALID_PARAMETER,
        }
    }

    /// Drop what a closed handle held.
    pub fn release(&mut self, handle: u32) {
        self.0.retain(|l| l.handle != handle);
    }
}
