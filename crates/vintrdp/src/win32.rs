//! Win32 plumbing shared by the Windows front-end and the connection store.
//!
//! The same binary runs on Windows NT 4.0 SP0 and on Windows 11, so anything
//! newer than NT 4.0 is looked up at run time with [`proc`] and given a
//! fallback, instead of being imported (a missing import stops the program
//! from loading at all).

pub mod credential;

use std::ffi::{CString, c_void};
use std::os::windows::io::IntoRawHandle;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{HANDLE, HMODULE};
use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, SetErrorMode};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::SystemInformation::GetVersion;
use windows::core::{PCSTR, PCWSTR};

/// An export, by name or by (uxtheme's unnamed) ordinal.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Symbol {
    Name(&'static str),
    Ordinal(u16),
}

/// Every lookup so far, found or not: (dll, symbol, address or 0).
static LOOKUPS: Mutex<Vec<(&'static str, Symbol, usize)>> = Mutex::new(Vec::new());

/// `name` from `dll`, or None if this Windows doesn't have it.
///
/// # Safety
/// `T` must be the export's `unsafe extern "system" fn` type.
pub unsafe fn proc<T: Copy>(dll: &'static str, name: &'static str) -> Option<T> {
    unsafe { lookup(dll, Symbol::Name(name)) }
}

/// Export number `ordinal` from `dll`.
///
/// # Safety
/// As [`proc`].
pub unsafe fn proc_ordinal<T: Copy>(dll: &'static str, ordinal: u16) -> Option<T> {
    unsafe { lookup(dll, Symbol::Ordinal(ordinal)) }
}

unsafe fn lookup<T: Copy>(dll: &'static str, symbol: Symbol) -> Option<T> {
    assert_eq!(size_of::<T>(), size_of::<usize>(), "proc wants a function pointer type");
    let mut lookups = LOOKUPS.lock().unwrap_or_else(|e| e.into_inner());
    let address = match lookups.iter().find(|(d, s, _)| *d == dll && *s == symbol) {
        Some(&(_, _, address)) => address,
        None => {
            let address = library(dll).map_or(0, |lib| {
                // SAFETY: a module handle and a NUL-terminated name or an ordinal.
                let found = unsafe {
                    match symbol {
                        Symbol::Name(name) => {
                            let name = CString::new(name).expect("export name");
                            GetProcAddress(lib, PCSTR(name.as_ptr().cast()))
                        }
                        Symbol::Ordinal(n) => GetProcAddress(lib, PCSTR(n as usize as *const u8)),
                    }
                };
                found.map_or(0, |f| f as usize)
            });
            lookups.push((dll, symbol, address));
            address
        }
    };
    // SAFETY: a non-null export address, as the caller's function type.
    (address != 0).then(|| unsafe { std::mem::transmute_copy::<usize, T>(&address) })
}

/// Load `dll` once, for good; None if it isn't there (msimg32, uxtheme and
/// dwmapi don't exist on NT 4.0).
fn library(dll: &'static str) -> Option<HMODULE> {
    static LIBRARIES: Mutex<Vec<(&'static str, usize)>> = Mutex::new(Vec::new());
    let mut libraries = LIBRARIES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&(_, lib)) = libraries.iter().find(|(d, _)| *d == dll) {
        return (lib != 0).then_some(HMODULE(lib as *mut c_void));
    }
    let name: Vec<u16> = dll.encode_utf16().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated name. No "file not found" box if it's missing.
    let lib = unsafe {
        let old = SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
        let lib = LoadLibraryW(PCWSTR(name.as_ptr())).map_or(0, |m| m.0 as usize);
        SetErrorMode(old);
        lib
    };
    libraries.push((dll, lib));
    (lib != 0).then_some(HMODULE(lib as *mut c_void))
}

/// Windows' (major, minor) version: (4, 0) is NT 4.0, (5, 0) 2000, (5, 1) XP.
/// Windows 8.1 and later report at least 6.2, which is all we compare against.
pub fn version() -> (u8, u8) {
    static VERSION: OnceLock<(u8, u8)> = OnceLock::new();
    *VERSION.get_or_init(|| {
        // SAFETY: no preconditions.
        let v = unsafe { GetVersion() };
        (v as u8, (v >> 8) as u8)
    })
}

/// True before Windows 2000.
pub fn is_nt4() -> bool {
    version() < (5, 0)
}

/// Where stderr goes. With arguments, the console we were started from, if any
/// (Windows XP and later; a GUI program can't get one on NT 4.0 or 2000).
/// `VINTRDP_LOG=path` appends diagnostics to that file instead. Failing both,
/// on a Windows with no `AttachConsole` they go to `%TEMP%\vintrdp.log`, so a
/// program with no console can still be debugged.
pub fn set_up_output(attach: bool) {
    type AttachConsole = unsafe extern "system" fn(u32) -> i32;
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // SAFETY: kernel32's export with its prototype; failing just means there's
    // no console.
    let attach_console = unsafe { proc::<AttachConsole>("kernel32.dll", "AttachConsole") };
    if attach {
        if let Some(attach_console) = attach_console {
            unsafe { attach_console(ATTACH_PARENT_PROCESS) };
        }
    }
    let log = match std::env::var_os("VINTRDP_LOG").filter(|p| !p.is_empty()) {
        Some(path) => std::path::PathBuf::from(path),
        None if attach_console.is_none() && !has_handle(STD_ERROR_HANDLE) => std::env::temp_dir().join("vintrdp.log"),
        None => return,
    };
    let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&log) else { return };
    // Rust's stderr asks for the handle on every write, so this redirects it.
    // The handle stays open for the life of the process.
    let handle = HANDLE(file.into_raw_handle());
    // SAFETY: an open file handle we never close.
    unsafe {
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
        // `--version` and the like, if they'd otherwise go nowhere.
        if !has_handle(STD_OUTPUT_HANDLE) {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
        }
    }
    let (major, minor) = version();
    eprintln!("vintrdp {} on Windows {major}.{minor}, process {}", crate::launch::VERSION, std::process::id());
}

/// Whether a standard handle leads anywhere (a console, a pipe or a file).
fn has_handle(which: STD_HANDLE) -> bool {
    // SAFETY: no preconditions; null and INVALID_HANDLE_VALUE come back as Err.
    unsafe { GetStdHandle(which) }.is_ok()
}
