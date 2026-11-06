//! Printing for redirected printers: the jobs printer.c opens, and the local
//! printers a front-end can offer. On macOS, Linux and the BSDs a job is piped
//! to `lpr` (CUPS), run directly rather than through a shell; on Windows it's
//! a RAW job for the spooler.
//!
//! What arrives is the output of the server's printer driver. With rdesktop's
//! default driver, "MS Publisher Imagesetter", that's PostScript, which CUPS
//! prints on any printer. The Windows spooler passes RAW data straight to the
//! printer, so there the default is the local printer's own driver, which the
//! server must also have; see [`default_driver`].

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use crate::ffi::RD_BOOL;

/// rdesktop's driver for printers given without one: a PostScript driver every
/// Windows server has.
pub const POSTSCRIPT_DRIVER: &str = "MS Publisher Imagesetter";

static NEXT_JOB: AtomicI32 = AtomicI32::new(1);
static JOBS: Mutex<Option<HashMap<i32, Job>>> = Mutex::new(None);

fn with_jobs<R>(f: impl FnOnce(&mut HashMap<i32, Job>) -> R) -> R {
    f(JOBS.lock().unwrap().get_or_insert_with(HashMap::new))
}

fn trace() -> bool {
    std::env::var_os("VINTRDP_TRACE_PRINT").is_some()
}

/// Start a job on `printer` (NULL for the default printer). 0 if it can't.
///
/// # Safety
/// `printer` is NULL or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_printer_open(printer: *const c_char) -> c_int {
    // SAFETY: as documented.
    let name = (!printer.is_null()).then(|| unsafe { CStr::from_ptr(printer) }.to_string_lossy().into_owned());
    match Job::start(name.as_deref()) {
        Ok(job) => {
            let id = NEXT_JOB.fetch_add(1, Ordering::Relaxed) & 0x0fff_ffff;
            with_jobs(|j| j.insert(id, job));
            if trace() {
                eprintln!("vintrdp: print: job {id} on {}", name.as_deref().unwrap_or("the default printer"));
            }
            id
        }
        Err(e) => {
            eprintln!("vintrdp: could not print to {}: {e}", name.as_deref().unwrap_or("the default printer"));
            0
        }
    }
}

/// # Safety
/// `data` points to `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ui_printer_write(job: c_int, data: *const u8, length: u32) -> RD_BOOL {
    // SAFETY: as documented.
    let data = if data.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(data, length as usize) } };
    let ok = with_jobs(|j| j.get_mut(&job).map(|job| job.write(data)));
    match ok {
        Some(Ok(())) => 1,
        Some(Err(e)) => {
            eprintln!("vintrdp: printing failed: {e}");
            0
        }
        None => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ui_printer_close(job: c_int) {
    if let Some(job) = with_jobs(|j| j.remove(&job)) {
        job.finish();
    }
}

/// The printer to use when none is named, if there is one.
pub fn default_printer() -> Option<String> {
    imp::default_printer()
}

/// The local printers, the default first.
pub fn printers() -> Vec<String> {
    let mut list = imp::printers();
    if let Some(default) = default_printer() {
        if let Some(i) = list.iter().position(|p| *p == default) {
            let d = list.remove(i);
            list.insert(0, d);
        }
    }
    list
}

/// The driver to announce for `printer` when the user names none: PostScript
/// where CUPS prints, the printer's own driver on Windows.
pub fn default_driver(printer: Option<&str>) -> String {
    imp::driver(printer).unwrap_or_else(|| POSTSCRIPT_DRIVER.to_string())
}

use imp::Job;

#[cfg(unix)]
mod imp {
    use std::io::Write;
    use std::process::{Child, Command, Stdio};

    pub struct Job(Child);

    impl Job {
        pub fn start(printer: Option<&str>) -> std::io::Result<Job> {
            let mut lpr = Command::new("lpr");
            lpr.args(["-T", "vintrdp"]);
            if let Some(p) = printer {
                lpr.args(["-P", p]);
            }
            Ok(Job(lpr.stdin(Stdio::piped()).spawn()?))
        }

        pub fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
            self.0.stdin.as_mut().ok_or_else(|| std::io::Error::other("lpr has exited"))?.write_all(data)
        }

        /// End the job; lpr is waited for elsewhere, so a slow spooler can't
        /// hold up the session.
        pub fn finish(mut self) {
            drop(self.0.stdin.take());
            std::thread::spawn(move || match self.0.wait() {
                Ok(s) if !s.success() => eprintln!("vintrdp: lpr failed ({s})"),
                Err(e) => eprintln!("vintrdp: lpr: {e}"),
                _ => {}
            });
        }
    }

    fn lpstat(args: &[&str]) -> String {
        Command::new("lpstat").args(args).stderr(Stdio::null()).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    }

    pub fn default_printer() -> Option<String> {
        // "system default destination: NAME"
        lpstat(&["-d"]).lines().find_map(|l| l.split_once(": ").map(|(_, name)| name.trim().to_string())).filter(|n| !n.is_empty())
    }

    pub fn printers() -> Vec<String> {
        lpstat(&["-e"]).lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
    }

    pub fn driver(_printer: Option<&str>) -> Option<String> {
        None
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::ptr;

    type Handle = *mut c_void;

    #[repr(C)]
    struct DocInfo1 {
        name: *const u16,
        output_file: *const u16,
        datatype: *const u16,
    }

    #[repr(C)]
    struct PrinterInfo2 {
        server_name: *const u16,
        printer_name: *const u16,
        share_name: *const u16,
        port_name: *const u16,
        driver_name: *const u16,
        // ... more fields we don't read; the buffer is sized by the call.
    }

    #[repr(C)]
    struct PrinterInfo4 {
        printer_name: *const u16,
        server_name: *const u16,
        attributes: u32,
    }

    #[link(name = "winspool")]
    unsafe extern "system" {
        fn OpenPrinterW(name: *const u16, printer: *mut Handle, defaults: *const c_void) -> i32;
        fn ClosePrinter(printer: Handle) -> i32;
        fn StartDocPrinterW(printer: Handle, level: u32, info: *const DocInfo1) -> u32;
        fn EndDocPrinter(printer: Handle) -> i32;
        fn WritePrinter(printer: Handle, data: *const c_void, length: u32, written: *mut u32) -> i32;
        fn GetDefaultPrinterW(buffer: *mut u16, size: *mut u32) -> i32;
        fn GetPrinterW(printer: Handle, level: u32, buffer: *mut u8, size: u32, needed: *mut u32) -> i32;
        fn EnumPrintersW(flags: u32, name: *const u16, level: u32, buffer: *mut u8, size: u32, needed: *mut u32, returned: *mut u32) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// # Safety
    /// `p` is NULL or a NUL-terminated UTF-16 string.
    unsafe fn from_wide(p: *const u16) -> Option<String> {
        if p.is_null() {
            return None;
        }
        // SAFETY: as documented.
        let len = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
        Some(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) }))
    }

    fn open(printer: Option<&str>) -> std::io::Result<(Handle, String)> {
        let name = printer.map(String::from).or_else(default_printer).ok_or_else(|| std::io::Error::other("no default printer"))?;
        let mut handle = ptr::null_mut();
        // SAFETY: a NUL-terminated name and an out pointer.
        if unsafe { OpenPrinterW(wide(&name).as_ptr(), &mut handle, ptr::null()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((handle, name))
    }

    pub struct Job(Handle);

    // SAFETY: a spooler handle may be used from any thread, one at a time;
    // jobs live behind a Mutex.
    unsafe impl Send for Job {}

    impl Job {
        pub fn start(printer: Option<&str>) -> std::io::Result<Job> {
            let (handle, _) = open(printer)?;
            let (name, raw) = (wide("vintrdp"), wide("RAW"));
            let info = DocInfo1 { name: name.as_ptr(), output_file: ptr::null(), datatype: raw.as_ptr() };
            // SAFETY: an open printer and a DOC_INFO_1 whose strings outlive the call.
            if unsafe { StartDocPrinterW(handle, 1, &info) } == 0 {
                let e = std::io::Error::last_os_error();
                // SAFETY: ours to close.
                unsafe { ClosePrinter(handle) };
                return Err(e);
            }
            Ok(Job(handle))
        }

        pub fn write(&mut self, mut data: &[u8]) -> std::io::Result<()> {
            while !data.is_empty() {
                let mut written = 0;
                // SAFETY: a started job and a readable buffer.
                if unsafe { WritePrinter(self.0, data.as_ptr().cast(), data.len() as u32, &mut written) } == 0 || written == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                data = &data[written as usize..];
            }
            Ok(())
        }

        pub fn finish(self) {
            // SAFETY: the job and printer we started and opened.
            unsafe {
                EndDocPrinter(self.0);
                ClosePrinter(self.0);
            }
        }
    }

    pub fn default_printer() -> Option<String> {
        let mut buf = [0u16; 512];
        let mut size = buf.len() as u32;
        // SAFETY: a buffer and its size in characters.
        if unsafe { GetDefaultPrinterW(buf.as_mut_ptr(), &mut size) } == 0 {
            return None;
        }
        // SAFETY: filled in and NUL-terminated by the call.
        unsafe { from_wide(buf.as_ptr()) }
    }

    pub fn printers() -> Vec<String> {
        const PRINTER_ENUM_LOCAL: u32 = 2;
        const PRINTER_ENUM_CONNECTIONS: u32 = 4;
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        let (mut needed, mut count) = (0u32, 0u32);
        // SAFETY: the first call only sizes; the second fills an 8-aligned
        // buffer of that size with PRINTER_INFO_4 records.
        unsafe {
            EnumPrintersW(flags, ptr::null(), 4, ptr::null_mut(), 0, &mut needed, &mut count);
            if needed == 0 {
                return Vec::new();
            }
            let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
            if EnumPrintersW(flags, ptr::null(), 4, buf.as_mut_ptr().cast(), needed, &mut needed, &mut count) == 0 {
                return Vec::new();
            }
            let info = std::slice::from_raw_parts(buf.as_ptr().cast::<PrinterInfo4>(), count as usize);
            info.iter().filter_map(|i| from_wide(i.printer_name)).collect()
        }
    }

    /// The printer's own driver: the server renders with it, and the spooler
    /// passes its output through untouched.
    pub fn driver(printer: Option<&str>) -> Option<String> {
        let (handle, _) = open(printer).ok()?;
        let mut needed = 0u32;
        // SAFETY: as in printers(), for one PRINTER_INFO_2; then closed.
        unsafe {
            GetPrinterW(handle, 2, ptr::null_mut(), 0, &mut needed);
            let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
            let ok = GetPrinterW(handle, 2, buf.as_mut_ptr().cast(), needed, &mut needed) != 0;
            ClosePrinter(handle);
            if !ok {
                return None;
            }
            from_wide((*buf.as_ptr().cast::<PrinterInfo2>()).driver_name)
        }
    }
}
