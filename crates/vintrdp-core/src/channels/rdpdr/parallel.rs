//! Parallel port redirection on Unix: a local printer port (/dev/lp0,
//! /dev/usb/lp0) as LPTn on the server. Port of rdesktop's parallel.c.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;

use super::*;

const FILE_DEVICE_PARALLEL: u32 = 0x22;

pub(super) struct Parallel {
    file: File,
}

impl Parallel {
    pub fn open(path: &str) -> io::Result<Parallel> {
        let file = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(path)?;
        // Linux's lp driver: retry on errors rather than fail the write.
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            const LPABORT: libc::c_ulong = 0x0604;
            // SAFETY: LPABORT takes an int by value.
            unsafe { libc::ioctl(file.as_raw_fd(), LPABORT as _, 1 as libc::c_int) };
        }
        Ok(Parallel { file })
    }
}

impl Port for Parallel {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.file).read(buf)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        (&self.file).write(data)
    }

    fn read_policy(&self, _length: u32) -> ReadPolicy {
        ReadPolicy::Wait { total: None, interval: None }
    }

    /// What a printer's failed write means.
    fn error_status(&self, e: &io::Error) -> u32 {
        match e.raw_os_error() {
            Some(libc::EAGAIN) | Some(libc::EIO) => STATUS_DEVICE_OFF_LINE,
            Some(libc::ENOSPC) => STATUS_DEVICE_PAPER_EMPTY,
            _ => STATUS_DEVICE_POWERED_OFF,
        }
    }

    /// rdesktop accepted the parallel IOCTLs (IOCTL_PAR_QUERY_RAW_DEVICE_ID
    /// and the rest) without doing anything.
    fn control(&mut self, code: u32, _input: &[u8], _out_max: usize) -> Control {
        if code >> 16 != FILE_DEVICE_PARALLEL {
            return Control::Failed(STATUS_INVALID_PARAMETER);
        }
        Control::Done(Vec::new())
    }

    fn event(&mut self) -> Option<u32> {
        None
    }
}
