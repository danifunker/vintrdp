//! Helper functions rdesktop.c used to provide to the rest of the C core:
//! allocation, randomness, licence storage and the ~/.rdesktop cache files.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString};
#[cfg(unix)]
use std::io::Read;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::path::PathBuf;
use std::sync::Mutex;
use std::ptr;

use crate::ffi::{self, RD_BOOL};
use crate::state::g_hostname;

const EX_UNAVAILABLE: c_int = 69;

fn out_of_memory(what: &str) -> ! {
    eprintln!("vintrdp: {what}: out of memory");
    std::process::exit(EX_UNAVAILABLE);
}

#[unsafe(no_mangle)]
pub extern "C" fn xmalloc(size: c_int) -> *mut c_void {
    // SAFETY: plain malloc; NULL is handled.
    let p = unsafe { libc::malloc(size.max(1) as usize) };
    if p.is_null() {
        out_of_memory("xmalloc");
    }
    p
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn xrealloc(old: *mut c_void, size: usize) -> *mut c_void {
    // SAFETY: old is NULL or from malloc/realloc.
    let p = unsafe { libc::realloc(old, size.max(1)) };
    if p.is_null() {
        out_of_memory("xrealloc");
    }
    p
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn xstrdup(s: *const c_char) -> *mut c_char {
    // SAFETY: s is a NUL-terminated string from the caller.
    let p = unsafe { libc::strdup(s) };
    if p.is_null() {
        out_of_memory("xstrdup");
    }
    p
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn xfree(mem: *mut c_void) {
    // SAFETY: mem is NULL or from the allocators above.
    unsafe { libc::free(mem) };
}

#[unsafe(no_mangle)]
pub extern "C" fn exit_if_null(p: *mut c_void) {
    if p.is_null() {
        out_of_memory("exit_if_null");
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn hexdump(p: *const u8, len: c_uint) {
    // SAFETY: caller passes len readable bytes.
    let data = unsafe { std::slice::from_raw_parts(p, len as usize) };
    for (i, line) in data.chunks(16).enumerate() {
        let hex: String = line.iter().map(|b| format!("{b:02x} ")).collect();
        let text: String = line.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
        eprintln!("{:04x} {hex:<48}{text}", i * 16);
    }
}

#[cfg(unix)]
fn fill_random(buf: &mut [u8]) -> bool {
    // SAFETY: getentropy fills at most 256 bytes into a valid buffer.
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(buf)).is_ok()
        || unsafe { libc::getentropy(buf.as_mut_ptr().cast(), buf.len()) } == 0
}

#[cfg(windows)]
fn fill_random(buf: &mut [u8]) -> bool {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        #[link_name = "SystemFunction036"]
        fn rtl_gen_random(buf: *mut c_void, len: u32) -> u8;
    }
    // SAFETY: a valid buffer of the given length.
    unsafe { rtl_gen_random(buf.as_mut_ptr().cast(), buf.len() as u32) != 0 }
}

/// A licence file the user imported, applied to every connection.
static IMPORTED_LICENCE: Mutex<Option<PathBuf>> = Mutex::new(None);

pub(crate) fn set_imported_licence(path: Option<PathBuf>) {
    *IMPORTED_LICENCE.lock().unwrap() = path;
}

/// 32 random bytes for the security exchange.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn generate_random(random: *mut u8) {
    let mut buf = [0u8; 32];
    if !fill_random(&mut buf) {
        eprintln!("vintrdp: no source of randomness");
        std::process::abort();
    }
    // SAFETY: caller passes a 32-byte buffer.
    unsafe { ptr::copy_nonoverlapping(buf.as_ptr(), random, 32) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn str_startswith(s: *const c_char, prefix: *const c_char) -> RD_BOOL {
    // SAFETY: both are NUL-terminated strings.
    unsafe { CStr::from_ptr(s).to_bytes().starts_with(CStr::from_ptr(prefix).to_bytes()) as RD_BOOL }
}

/// Upper-cases ASCII letters in place.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn toupper_str(mut p: *mut c_char) {
    // SAFETY: p is a NUL-terminated, writable string.
    unsafe {
        while *p != 0 {
            *p = (*p as u8).to_ascii_uppercase() as c_char;
            p = p.add(1);
        }
    }
}

/// rdesktop's argument splitter. Finds the next `needle` in `src` that isn't
/// backslash-escaped (escapes are removed in place), replaces it with NUL and
/// returns the text after it. With no more needles it returns a pointer to
/// the terminating NUL; only an empty `src` returns NULL. Callers loop with
/// `while ((pos = next_arg(s, ',')))`, so the last item relies on that.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn next_arg(src: *mut c_char, needle: c_char) -> *mut c_char {
    // SAFETY: src is a NUL-terminated, writable string; we only shrink it.
    unsafe {
        if *src == 0 {
            return ptr::null_mut();
        }
        let mut p = src;
        loop {
            let found = libc::strchr(p, needle as c_int);
            if found.is_null() {
                return src.add(libc::strlen(src));
            }
            if found > src && *found.sub(1) == b'\\' as c_char {
                // Remove the backslash; the needle is now at found-1.
                libc::memmove(found.sub(1).cast(), found.cast(), libc::strlen(found) + 1);
                p = found;
            } else {
                *found = 0;
                return found.add(1);
            }
        }
    }
}

/// `n` in `base` as a string, in a buffer reused by the next call (as
/// rdesktop.c's). printer.c numbers its devices with it.
#[unsafe(no_mangle)]
pub extern "C" fn l_to_a(n: libc::c_long, base: c_int) -> *mut c_char {
    static mut BUF: [u8; 72] = [0; 72];
    let base = base.clamp(2, 36) as u64;
    let mut digits = Vec::new();
    let mut v = n.unsigned_abs() as u64;
    loop {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(v % base) as usize]);
        v /= base;
        if v == 0 {
            break;
        }
    }
    if n < 0 {
        digits.push(b'-');
    }
    digits.reverse();
    digits.push(0);
    // SAFETY: only the rdesktop thread calls this, and the caller copies the
    // result before the next call, as with rdesktop's own.
    unsafe {
        let buf = &mut *(&raw mut BUF);
        buf[..digits.len()].copy_from_slice(&digits);
        buf.as_mut_ptr().cast()
    }
}

type LineHandler = unsafe extern "C" fn(line: *const c_char, data: *mut c_void) -> RD_BOOL;

/// Split `input` into lines, keeping an incomplete trailing line in `*rest`
/// (a malloc'ed string, initially NULL) for the next call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn str_handle_lines(
    input: *const c_char,
    rest: *mut *mut c_char,
    handler: LineHandler,
    data: *mut c_void,
) -> RD_BOOL {
    // SAFETY: input is NUL-terminated; *rest is NULL or a string from xstrdup.
    unsafe {
        let mut buf = Vec::new();
        if !(*rest).is_null() {
            buf.extend_from_slice(CStr::from_ptr(*rest).to_bytes());
        }
        buf.extend_from_slice(CStr::from_ptr(input).to_bytes());

        let mut ok = true;
        let mut start = 0;
        while let Some(nl) = buf[start..].iter().position(|&b| b == b'\n') {
            let line = CString::new(&buf[start..start + nl]).unwrap_or_default();
            start += nl + 1;
            if handler(line.as_ptr(), data) == 0 {
                ok = false;
                break;
            }
        }
        let remaining = CString::new(&buf[start..]).unwrap_or_default();
        libc::free((*rest).cast());
        *rest = xstrdup(remaining.as_ptr());
        ok as RD_BOOL
    }
}

/// Run argv[0] with combined stdout/stderr fed line by line to `handler`.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn subprocess(argv: *const *const c_char, handler: LineHandler, data: *mut c_void) -> RD_BOOL {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::{Command, Stdio};

    // SAFETY: argv is a NULL-terminated array of NUL-terminated strings.
    let args: Vec<String> = unsafe {
        let mut v = Vec::new();
        let mut p = argv;
        while !(*p).is_null() {
            v.push(CStr::from_ptr(*p).to_string_lossy().into_owned());
            p = p.add(1);
        }
        v
    };
    let Some((prog, rest)) = args.split_first() else { return 0 };

    let mut fds = [0; 2];
    // SAFETY: valid two-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return 0;
    }
    // SAFETY: fresh pipe fds owned here.
    let (read_end, write_end) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    let Ok(stderr) = write_end.try_clone() else { return 0 };
    let child = Command::new(prog)
        .args(rest)
        .stdout(Stdio::from(write_end))
        .stderr(Stdio::from(stderr))
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("vintrdp: could not run {prog}");
        return 0;
    };

    let mut reader = std::fs::File::from(read_end);
    let mut rest_line: *mut c_char = ptr::null_mut();
    let mut chunk = [0u8; 255];
    while let Ok(n) = reader.read(&mut chunk) {
        if n == 0 {
            break;
        }
        let text = CString::new(chunk[..n].iter().copied().filter(|&b| b != 0).collect::<Vec<_>>()).unwrap();
        // SAFETY: as documented for str_handle_lines.
        unsafe { str_handle_lines(text.as_ptr(), &mut rest_line, handler, data) };
    }
    // SAFETY: rest_line is NULL or from xstrdup.
    unsafe { libc::free(rest_line.cast()) };
    let _ = child.wait();
    1
}

// ---------------------------------------------------------------------------
// Files under the user's home directory

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

fn mkdirs(path: &PathBuf) -> bool {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path).is_ok()
}

#[unsafe(no_mangle)]
pub extern "C" fn rd_certcache_mkdir() -> RD_BOOL {
    home().is_some_and(|h| mkdirs(&h.join(".local/share/rdesktop/certs"))) as RD_BOOL
}

#[unsafe(no_mangle)]
pub extern "C" fn rd_pstcache_mkdir() -> RD_BOOL {
    home().is_some_and(|h| mkdirs(&h.join(".rdesktop/cache"))) as RD_BOOL
}

/// Open (creating if needed) a file under ~/.rdesktop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rd_open_file(filename: *const c_char) -> c_int {
    let Some(h) = home() else { return -1 };
    // SAFETY: filename is NUL-terminated.
    let name = unsafe { CStr::from_ptr(filename) }.to_string_lossy().into_owned();
    let path = CString::new(h.join(".rdesktop").join(name).to_string_lossy().into_owned()).unwrap();
    #[cfg(unix)]
    let flags = libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC;
    #[cfg(windows)]
    let flags = libc::O_RDWR | libc::O_CREAT | libc::O_BINARY;
    // SAFETY: valid path string.
    let fd = unsafe { libc::open(path.as_ptr(), flags, 0o600 as c_uint) };
    if fd == -1 {
        eprintln!("vintrdp: could not open {}", path.to_string_lossy());
    }
    fd
}

#[unsafe(no_mangle)]
pub extern "C" fn rd_close_file(fd: c_int) {
    // SAFETY: closing a descriptor rdesktop owns.
    unsafe { libc::close(fd) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rd_read_file(fd: c_int, ptr: *mut c_void, len: c_int) -> c_int {
    // SAFETY: caller passes a buffer of len bytes.
    unsafe { libc::read(fd, ptr, len.max(0) as _) as c_int }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rd_write_file(fd: c_int, ptr: *const c_void, len: c_int) -> c_int {
    // SAFETY: caller passes a buffer of len bytes.
    unsafe { libc::write(fd, ptr, len.max(0) as _) as c_int }
}

#[unsafe(no_mangle)]
pub extern "C" fn rd_lseek_file(fd: c_int, offset: c_int) -> c_int {
    // SAFETY: seeking a descriptor rdesktop owns.
    unsafe { libc::lseek(fd, offset as _, libc::SEEK_SET) as c_int }
}

/// The bitmap cache's lock against other clients. One client at a time is the
/// normal case on Windows, and the lock is only a guard, so it always succeeds.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub extern "C" fn rd_lock_file(_fd: c_int, _start: c_int, _len: c_int) -> RD_BOOL {
    1
}

#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn rd_lock_file(fd: c_int, start: c_int, len: c_int) -> RD_BOOL {
    // SAFETY: zeroed flock is a valid starting value; fcntl on our fd.
    unsafe {
        let mut lock: libc::flock = std::mem::zeroed();
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_start = start as _;
        lock.l_len = len as _;
        (libc::fcntl(fd, libc::F_SETLK, &lock) != -1) as RD_BOOL
    }
}

/// ~/.local/share/rdesktop/licenses/<sha1(hostname)>.cal, as rdesktop names it.
fn licence_path() -> Option<PathBuf> {
    let mut hi = [0u8; 16];
    // SAFETY: g_hostname is a NUL-terminated char[16] set by state::apply.
    let host = unsafe { CStr::from_ptr(ptr::addr_of!(g_hostname).cast::<c_char>()) }.to_bytes();
    let n = host.len().min(15);
    hi[..n].copy_from_slice(&host[..n]);
    let mut ho = [0u8; 20];
    let mut hash = [0 as c_char; 41];
    // SAFETY: buffers sized as secure.c expects (20-byte digest, 41-char hex).
    unsafe {
        ffi::sec_hash_sha1_16(ho.as_mut_ptr(), hi.as_mut_ptr(), crate::state::g_static_rdesktop_salt_16.as_ptr().cast_mut());
        ffi::sec_hash_to_string(hash.as_mut_ptr(), hash.len() as c_int, ho.as_mut_ptr(), ho.len() as c_int);
    }
    // SAFETY: sec_hash_to_string NUL-terminates.
    let hash = unsafe { CStr::from_ptr(hash.as_ptr()) }.to_string_lossy().into_owned();
    Some(home()?.join(".local/share/rdesktop/licenses").join(format!("{hash}.cal")))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn load_licence(data: *mut *mut u8) -> c_int {
    // SAFETY: g_hostname as above.
    let host = unsafe { CStr::from_ptr(ptr::addr_of!(g_hostname).cast::<c_char>()) }.to_string_lossy().into_owned();
    let legacy = home().map(|h| h.join(format!(".rdesktop/license.{host}")));
    // The user's imported licence takes precedence over the per-server cache.
    let imported = IMPORTED_LICENCE.lock().unwrap().clone().filter(|p| p.exists());
    let mut candidates = imported.into_iter().chain([licence_path(), legacy].into_iter().flatten());
    let Some(bytes) = candidates.find_map(|p| std::fs::read(p).ok()) else {
        return -1;
    };
    let buf = xmalloc(bytes.len() as c_int).cast::<u8>();
    // SAFETY: buf has bytes.len() bytes; *data is an out-pointer.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
        *data = buf;
    }
    bytes.len() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn save_licence(data: *const u8, length: c_int) {
    let Some(path) = licence_path() else { return };
    if !path.parent().is_some_and(|d| mkdirs(&d.to_path_buf())) {
        eprintln!("vintrdp: could not create licence directory");
        return;
    }
    // SAFETY: caller passes length bytes.
    let bytes = unsafe { std::slice::from_raw_parts(data, length.max(0) as usize) };
    let tmp = path.with_extension("cal.new");
    if std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("vintrdp: could not save licence to {}", path.display());
    }
}

/// Create the session window (or reuse it on reconnect).
#[unsafe(no_mangle)]
pub extern "C" fn rd_create_ui() {
    use crate::shim::{ui_create_window, ui_have_window, ui_reset_clip};
    use crate::state::{g_requested_session_height, g_requested_session_width};
    if ui_have_window() == 0 {
        // SAFETY: rdesktop thread; plain reads of the requested size.
        let (w, h) = unsafe { (g_requested_session_width, g_requested_session_height) };
        ui_create_window(w, h);
    } else {
        ui_reset_clip();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(input: &str, needle: u8) -> Vec<String> {
        let mut buf = CString::new(input).unwrap().into_bytes_with_nul();
        let mut cur = buf.as_mut_ptr().cast::<c_char>();
        let mut out = Vec::new();
        // SAFETY: buf is a writable NUL-terminated buffer that outlives the loop.
        unsafe {
            loop {
                let next = next_arg(cur, needle as c_char);
                if next.is_null() {
                    break;
                }
                out.push(CStr::from_ptr(cur).to_string_lossy().into_owned());
                cur = next;
            }
        }
        out
    }

    #[test]
    fn next_arg_matches_rdesktop() {
        assert_eq!(split("a=1,b=2", b','), ["a=1", "b=2"]);
        assert_eq!(split("one", b','), ["one"]);
        assert_eq!(split("x\\,y,z", b','), ["x,y", "z"]);
        assert!(split("", b',').is_empty());
    }
}
