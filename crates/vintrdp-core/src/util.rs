//! Small helpers shared across the core: randomness, the user's files, and
//! the hashing rdesktop uses to name per-host files.

use std::path::{Path, PathBuf};

/// Fill `buf` with random bytes from the system. Aborts if there's no source
/// of randomness: the security exchange can't go ahead without one.
pub fn random(buf: &mut [u8]) {
    if !fill_random(buf) {
        eprintln!("vintrdp: no source of randomness");
        std::process::abort();
    }
}

#[cfg(unix)]
fn fill_random(buf: &mut [u8]) -> bool {
    use std::io::Read;
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(buf)).is_ok()
}

/// RtlGenRandom, which every Windows from NT 4.0 SP0 exports from advapi32.
#[cfg(windows)]
fn fill_random(buf: &mut [u8]) -> bool {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        #[link_name = "SystemFunction036"]
        fn rtl_gen_random(buf: *mut u8, len: u32) -> u8;
    }
    buf.chunks_mut(u32::MAX as usize).all(|c| {
        // SAFETY: a valid buffer of the given length.
        unsafe { rtl_gen_random(c.as_mut_ptr(), c.len() as u32) != 0 }
    })
}

/// The user's home directory.
pub fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// Create `path` and its parents, private to the user where that means anything.
pub fn mkdirs(path: &Path) -> bool {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path).is_ok()
}

/// rdesktop's data directory, `~/.local/share/rdesktop` (licences, certificates).
pub fn data_dir() -> Option<PathBuf> {
    Some(home()?.join(".local/share/rdesktop"))
}

/// rdesktop's own directory, `~/.rdesktop` (the persistent bitmap cache,
/// older licence files).
pub fn rdesktop_dir() -> Option<PathBuf> {
    Some(home()?.join(".rdesktop"))
}

/// Write `bytes` to `path` through a temporary file, so a crash never leaves
/// half a file behind.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".new");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// rdesktop's salt for hashing the client hostname into licence file names.
pub const RDESKTOP_SALT: [u8; 16] = [0xb8, 0x82, 0x29, 0x31, 0xc5, 0x39, 0xd9, 0x44, 0x54, 0x15, 0x5e, 0x14, 0x71, 0x38, 0xd5, 0x4d];
