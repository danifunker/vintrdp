//! Serial and parallel port redirection on Windows. Port of vintrdp's
//! win32_ports.c.
//!
//! The server's requests for a port are Windows' own serial and parallel
//! IOCTLs, so most go straight to DeviceIoControl() on the local port. The
//! session thread can't wait on a port, so three kinds are handled here:
//!
//! - Reads complete at once: the port's own timeouts are set so a read
//!   returns whatever has arrived, possibly nothing.
//! - The server's timeouts (SET/GET_TIMEOUTS) are kept for it, not applied.
//! - WAIT_ON_MASK stays pending, and rdpdr's poll completes it from
//!   [`Port::event`].
//!
//! Everything used here is in NT 4.0's kernel32.

use std::ffi::c_void;
use std::io;
use std::ptr;

use super::*;

type Handle = *mut c_void;

const INVALID_HANDLE_VALUE: Handle = ptr::without_provenance_mut(usize::MAX);
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;

const IOCTL_SERIAL_SET_TIMEOUTS: u32 = 0x001b_001c;
const IOCTL_SERIAL_GET_TIMEOUTS: u32 = 0x001b_0020;
const IOCTL_SERIAL_GET_WAIT_MASK: u32 = 0x001b_0040;
const IOCTL_SERIAL_SET_WAIT_MASK: u32 = 0x001b_0044;
const IOCTL_SERIAL_WAIT_ON_MASK: u32 = 0x001b_0048;

const EV_RXCHAR: u32 = 0x0001;
const EV_TXEMPTY: u32 = 0x0004;
const EV_CTS: u32 = 0x0008;
const EV_DSR: u32 = 0x0010;
const EV_RLSD: u32 = 0x0020;
const EV_ERR: u32 = 0x0080;
const EV_RING: u32 = 0x0100;

const MS_CTS_ON: u32 = 0x10;
const MS_DSR_ON: u32 = 0x20;
const MS_RING_ON: u32 = 0x40;
const MS_RLSD_ON: u32 = 0x80;

#[repr(C)]
struct CommTimeouts {
    read_interval: u32,
    read_total_multiplier: u32,
    read_total_constant: u32,
    write_total_multiplier: u32,
    write_total_constant: u32,
}

#[repr(C)]
#[derive(Default)]
struct ComStat {
    flags: u32,
    in_queue: u32,
    out_queue: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(name: *const u16, access: u32, share: u32, security: *mut c_void, disposition: u32, flags: u32, template: Handle) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn ReadFile(file: Handle, buf: *mut c_void, len: u32, read: *mut u32, overlapped: *mut c_void) -> i32;
    fn WriteFile(file: Handle, buf: *const c_void, len: u32, written: *mut u32, overlapped: *mut c_void) -> i32;
    fn DeviceIoControl(device: Handle, code: u32, input: *const c_void, in_len: u32, output: *mut c_void, out_len: u32, returned: *mut u32, overlapped: *mut c_void) -> i32;
    fn SetCommTimeouts(file: Handle, timeouts: *const CommTimeouts) -> i32;
    fn GetCommModemStatus(file: Handle, status: *mut u32) -> i32;
    fn ClearCommError(file: Handle, errors: *mut u32, stat: *mut ComStat) -> i32;
}

pub(super) struct WinPort {
    port: Handle,
    serial: bool,
    wait_mask: u32,
    /// The timeouts as the server set them.
    timeouts: [u32; 5],
    /// The modem status at the last event check.
    modem: u32,
    /// Written since the last EV_TXEMPTY.
    sent: bool,
}

// SAFETY: a kernel handle may be used from any thread; the port is only used
// from the session thread, one call at a time.
unsafe impl Send for WinPort {}

impl WinPort {
    /// Open `path`: "COM3" (as `\\.\COM3`, which also reaches COM10 and up)
    /// or a full `\\.\` device path.
    pub fn open(path: &str, serial: bool) -> io::Result<WinPort> {
        let full = if path.starts_with("\\\\") { path.to_string() } else { format!("\\\\.\\{path}") };
        let wide: Vec<u16> = full.encode_utf16().chain(Some(0)).collect();
        // SAFETY: a NUL-terminated name; no security attributes or template.
        let port = unsafe { CreateFileW(wide.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, ptr::null_mut(), OPEN_EXISTING, 0, ptr::null_mut()) };
        if port == INVALID_HANDLE_VALUE || port.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut p = WinPort { port, serial, wait_mask: 0, timeouts: [0; 5], modem: 0, sent: false };
        if serial {
            // Reads return at once with what's there.
            let now = CommTimeouts { read_interval: u32::MAX, read_total_multiplier: 0, read_total_constant: 0, write_total_multiplier: 0, write_total_constant: 0 };
            // SAFETY: an open port and valid structures.
            unsafe {
                SetCommTimeouts(p.port, &now);
                GetCommModemStatus(p.port, &mut p.modem);
            }
        }
        Ok(p)
    }
}

impl Drop for WinPort {
    fn drop(&mut self) {
        // SAFETY: ours to close.
        unsafe { CloseHandle(self.port) };
    }
}

impl Port for WinPort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut n = 0u32;
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        // SAFETY: a writable buffer of at least `len` bytes.
        if unsafe { ReadFile(self.port, buf.as_mut_ptr().cast(), len, &mut n, ptr::null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut n = 0u32;
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        // SAFETY: a readable buffer of at least `len` bytes.
        if unsafe { WriteFile(self.port, data.as_ptr().cast(), len, &mut n, ptr::null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.sent = true;
        Ok(n as usize)
    }

    fn read_policy(&self, _length: u32) -> ReadPolicy {
        ReadPolicy::Immediate
    }

    fn error_status(&self, _e: &io::Error) -> u32 {
        STATUS_INVALID_HANDLE
    }

    fn control(&mut self, code: u32, input: &[u8], out_max: usize) -> Control {
        let mut r = Reader::new(input);
        match code {
            IOCTL_SERIAL_SET_TIMEOUTS => {
                for t in &mut self.timeouts {
                    match r.u32() {
                        Ok(v) => *t = v,
                        Err(_) => break,
                    }
                }
                return Control::Done(Vec::new());
            }
            IOCTL_SERIAL_GET_TIMEOUTS => return Control::Done(self.timeouts.iter().flat_map(|t| t.to_le_bytes()).collect()),
            IOCTL_SERIAL_SET_WAIT_MASK => {
                if let Ok(mask) = r.u32() {
                    self.wait_mask = mask;
                }
                // SAFETY: an open port and an out pointer.
                unsafe { GetCommModemStatus(self.port, &mut self.modem) };
                return Control::Done(Vec::new());
            }
            IOCTL_SERIAL_GET_WAIT_MASK => return Control::Done(self.wait_mask.to_le_bytes().to_vec()),
            IOCTL_SERIAL_WAIT_ON_MASK => {
                return match self.event() {
                    Some(ev) => Control::Done(ev.to_le_bytes().to_vec()),
                    None => Control::Pending,
                };
            }
            _ => {}
        }
        // Everything else is the local port's business.
        let mut out = vec![0u8; out_max];
        let mut returned = 0u32;
        let in_len = u32::try_from(input.len()).unwrap_or(u32::MAX);
        let out_len = u32::try_from(out.len()).unwrap_or(u32::MAX);
        let in_ptr = if input.is_empty() { ptr::null() } else { input.as_ptr().cast() };
        let out_ptr = if out.is_empty() { ptr::null_mut() } else { out.as_mut_ptr().cast() };
        // SAFETY: buffers of the lengths given, or NULL for none.
        if unsafe { DeviceIoControl(self.port, code, in_ptr, in_len, out_ptr, out_len, &mut returned, ptr::null_mut()) } == 0 {
            return Control::Failed(STATUS_INVALID_PARAMETER);
        }
        out.truncate((returned as usize).min(out_max));
        Control::Done(out)
    }

    /// The wait mask events that have happened since the last check.
    fn event(&mut self) -> Option<u32> {
        if !self.serial || self.wait_mask == 0 {
            return None;
        }
        let mut events = 0;
        let (mut errors, mut stat, mut modem) = (0u32, ComStat::default(), 0u32);
        // SAFETY: an open port and out pointers.
        if unsafe { ClearCommError(self.port, &mut errors, &mut stat) } != 0 {
            if stat.in_queue > 0 {
                events |= EV_RXCHAR;
            }
            if self.sent && stat.out_queue == 0 {
                events |= EV_TXEMPTY;
            }
            if errors != 0 {
                events |= EV_ERR;
            }
        }
        // SAFETY: as above.
        if unsafe { GetCommModemStatus(self.port, &mut modem) } != 0 {
            let changed = modem ^ self.modem;
            for (ms, ev) in [(MS_CTS_ON, EV_CTS), (MS_DSR_ON, EV_DSR), (MS_RLSD_ON, EV_RLSD)] {
                if changed & ms != 0 {
                    events |= ev;
                }
            }
            if changed & modem & MS_RING_ON != 0 {
                events |= EV_RING;
            }
        }
        events &= self.wait_mask;
        if events == 0 {
            return None;
        }
        self.modem = modem;
        if events & EV_TXEMPTY != 0 {
            self.sent = false;
        }
        Some(events)
    }
}
