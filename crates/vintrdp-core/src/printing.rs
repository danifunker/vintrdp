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


/// rdesktop's driver for printers given without one: a PostScript driver every
/// Windows server has.
pub const POSTSCRIPT_DRIVER: &str = "MS Publisher Imagesetter";

fn trace() -> bool {
    std::env::var_os("VINTRDP_TRACE_PRINT").is_some()
}

/// A print job in progress: what the server's printer driver produces goes
/// to [`PrintJob::write`], and the job prints when it's dropped.
pub(crate) struct PrintJob(Option<Job>);

impl PrintJob {
    /// Start a job on `printer` (`None` for the default printer).
    pub fn open(printer: Option<&str>) -> std::io::Result<PrintJob> {
        let job = Job::start(printer)
            .inspect_err(|e| eprintln!("vintrdp: could not print to {}: {e}", printer.unwrap_or("the default printer")))?;
        if trace() {
            eprintln!("vintrdp: print: job on {}", printer.unwrap_or("the default printer"));
        }
        Ok(PrintJob(Some(job)))
    }

    pub fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        match &mut self.0 {
            Some(job) => job.write(data).inspect_err(|e| eprintln!("vintrdp: printing failed: {e}")),
            None => Ok(()),
        }
    }
}

impl Drop for PrintJob {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            job.finish();
        }
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

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
        fn GetProfileStringW(app: *const u16, key: *const u16, default: *const u16, out: *mut u16, size: u32) -> u32;
    }

    /// GetDefaultPrinterW where winspool has it (Windows 2000 and later);
    /// NT 4.0 keeps the default printer in win.ini's `[windows] device=`
    /// as "name,driver,port".
    pub fn default_printer() -> Option<String> {
        type GetDefaultPrinter = unsafe extern "system" fn(*mut u16, *mut u32) -> i32;
        let mut buf = [0u16; 512];
        // SAFETY: NUL-terminated names; the pointer GetProcAddress returns
        // for GetDefaultPrinterW has that signature.
        unsafe {
            let winspool = LoadLibraryW(wide("winspool.drv").as_ptr());
            let f = if winspool.is_null() { std::ptr::null_mut() } else { GetProcAddress(winspool, c"GetDefaultPrinterW".as_ptr().cast()) };
            if !f.is_null() {
                let f: GetDefaultPrinter = std::mem::transmute(f);
                let mut size = buf.len() as u32;
                return if f(buf.as_mut_ptr(), &mut size) == 0 { None } else { from_wide(buf.as_ptr()) };
            }
            let n = GetProfileStringW(wide("windows").as_ptr(), wide("device").as_ptr(), wide("").as_ptr(), buf.as_mut_ptr(), buf.len() as u32);
            if n == 0 {
                return None;
            }
            let entry = String::from_utf16_lossy(&buf[..n as usize]);
            entry.split(',').next().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
        }
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
