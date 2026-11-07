//! Serial port redirection on Unix: a tty as COMn on the server. Port of
//! rdesktop's serial.c, through termios with the `libc` crate.
//!
//! The server sends the serial IOCTLs of Windows' own serial driver; they're
//! carried out on the tty, or remembered (timeouts, the wait mask) for the
//! reads and WAIT_ON_MASK requests rdpdr keeps pending.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

use libc::{c_int, c_ulong};

use super::{Control, Port, ReadPolicy, STATUS_INVALID_PARAMETER};

const FILE_DEVICE_SERIAL_PORT: u32 = 0x1b;

// IOCTL_SERIAL_* function numbers.
const SET_BAUD_RATE: u32 = 1;
const SET_QUEUE_SIZE: u32 = 2;
const SET_LINE_CONTROL: u32 = 3;
const SET_BREAK_ON: u32 = 4;
const SET_BREAK_OFF: u32 = 5;
const IMMEDIATE_CHAR: u32 = 6;
const SET_TIMEOUTS: u32 = 7;
const GET_TIMEOUTS: u32 = 8;
const SET_DTR: u32 = 9;
const CLR_DTR: u32 = 10;
const RESET_DEVICE: u32 = 11;
const SET_RTS: u32 = 12;
const CLR_RTS: u32 = 13;
const SET_XOFF: u32 = 14;
const SET_XON: u32 = 15;
const GET_WAIT_MASK: u32 = 16;
const SET_WAIT_MASK: u32 = 17;
const WAIT_ON_MASK: u32 = 18;
const PURGE: u32 = 19;
const GET_BAUD_RATE: u32 = 20;
const GET_LINE_CONTROL: u32 = 21;
const GET_CHARS: u32 = 22;
const SET_CHARS: u32 = 23;
const GET_HANDFLOW: u32 = 24;
const SET_HANDFLOW: u32 = 25;
const GET_MODEMSTATUS: u32 = 26;
const GET_COMMSTATUS: u32 = 27;
const CONFIG_SIZE: u32 = 32;

const STOP_BITS_1: u8 = 0;
const STOP_BITS_2: u8 = 2;
const NO_PARITY: u8 = 0;
const ODD_PARITY: u8 = 1;
const EVEN_PARITY: u8 = 2;

const PURGE_TXABORT: u32 = 0x01;
const PURGE_RXABORT: u32 = 0x02;
const PURGE_TXCLEAR: u32 = 0x04;
const PURGE_RXCLEAR: u32 = 0x08;

// SERIAL_EV_* wait mask events.
const EV_RXCHAR: u32 = 0x0001;
const EV_RXFLAG: u32 = 0x0002;
const EV_TXEMPTY: u32 = 0x0004;
const EV_CTS: u32 = 0x0008;
const EV_DSR: u32 = 0x0010;
const EV_RLSD: u32 = 0x0020;

// Modem status.
const MS_DTR: u32 = 0x01;
const MS_RTS: u32 = 0x02;
const MS_CTS: u32 = 0x10;
const MS_DSR: u32 = 0x20;
const MS_RNG: u32 = 0x40;
const MS_CAR: u32 = 0x80;

// Handflow.
const DTR_CONTROL: u32 = 0x01;
const CTS_HANDSHAKE: u32 = 0x08;
const ERROR_ABORT: u32 = 0x8000_0000;
const XON_HANDSHAKE: u32 = 0x01;
const XOFF_HANDSHAKE: u32 = 0x02;
const DSR_SENSITIVITY: u32 = 0x40;

// SERIAL_CHARS indices.
const CHAR_EOF: usize = 0;
const CHAR_ERROR: usize = 1;
const CHAR_BREAK: usize = 2;
const CHAR_XON: usize = 4;
const CHAR_XOFF: usize = 5;

/// The modem-control ioctls, which libc has for Linux but not for macOS and
/// the BSDs.
#[cfg(any(target_os = "linux", target_os = "android"))]
mod tio {
    use libc::{c_int, c_ulong};
    pub const TIOCMGET: c_ulong = libc::TIOCMGET as c_ulong;
    pub const TIOCMSET: c_ulong = libc::TIOCMSET as c_ulong;
    pub const TIOCOUTQ: c_ulong = libc::TIOCOUTQ as c_ulong;
    pub const FIONREAD: c_ulong = libc::FIONREAD as c_ulong;
    pub const TIOCM_DTR: c_int = libc::TIOCM_DTR;
    pub const TIOCM_RTS: c_int = libc::TIOCM_RTS;
    pub const TIOCM_CTS: c_int = libc::TIOCM_CTS;
    pub const TIOCM_CAR: c_int = libc::TIOCM_CAR;
    pub const TIOCM_RNG: c_int = libc::TIOCM_RNG;
    pub const TIOCM_DSR: c_int = libc::TIOCM_DSR;
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
mod tio {
    use libc::{c_int, c_ulong};
    // <sys/ttycom.h>: _IOR('t', 106, int), _IOW('t', 109, int), _IOR('t', 115, int).
    pub const TIOCMGET: c_ulong = 0x4004_746a;
    pub const TIOCMSET: c_ulong = 0x8004_746d;
    pub const TIOCOUTQ: c_ulong = 0x4004_7473;
    pub const FIONREAD: c_ulong = libc::FIONREAD as c_ulong;
    pub const TIOCM_DTR: c_int = 0x002;
    pub const TIOCM_RTS: c_int = 0x004;
    pub const TIOCM_CTS: c_int = 0x020;
    pub const TIOCM_CAR: c_int = 0x040;
    pub const TIOCM_RNG: c_int = 0x080;
    pub const TIOCM_DSR: c_int = 0x100;
}

/// Baud rates and their termios speeds.
const SPEEDS: &[(u32, libc::speed_t)] = &[
    (75, libc::B75),
    (110, libc::B110),
    (134, libc::B134),
    (150, libc::B150),
    (300, libc::B300),
    (600, libc::B600),
    (1200, libc::B1200),
    (1800, libc::B1800),
    (2400, libc::B2400),
    (4800, libc::B4800),
    (9600, libc::B9600),
    (19200, libc::B19200),
    (38400, libc::B38400),
    (57600, libc::B57600),
    (115200, libc::B115200),
    (230400, libc::B230400),
    #[cfg(any(target_os = "linux", target_os = "android"))]
    (460800, libc::B460800),
];

/// An int-valued ioctl that reads.
fn ioctl_get(fd: c_int, request: c_ulong) -> io::Result<c_int> {
    let mut v: c_int = 0;
    // SAFETY: the request writes one int.
    if unsafe { libc::ioctl(fd, request as _, &mut v) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(v)
}

fn ioctl_set(fd: c_int, request: c_ulong, v: c_int) {
    // SAFETY: the request reads one int.
    unsafe { libc::ioctl(fd, request as _, &v) };
}

pub(super) struct Serial {
    file: File,
    termios: libc::termios,
    baud_rate: u32,
    stop_bits: u8,
    parity: u8,
    word_length: u8,
    dtr: bool,
    control: u32,
    xonoff: u32,
    onlimit: u32,
    offlimit: u32,
    chars: [u8; 6],
    /// ReadIntervalTimeout, ReadTotalTimeoutMultiplier and -Constant,
    /// WriteTotalTimeoutMultiplier and -Constant, as the server set them.
    timeouts: [u32; 5],
    wait_mask: u32,
    // What the last event check saw.
    event_txempty: c_int,
    event_cts: c_int,
    event_dsr: c_int,
    event_rlsd: c_int,
}

impl Serial {
    pub fn open(path: &str) -> io::Result<Serial> {
        let file = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK).open(path)?;
        // SAFETY: a zeroed termios is a valid value for tcgetattr to fill.
        let termios = unsafe { std::mem::zeroed() };
        let mut s = Serial {
            file,
            termios,
            baud_rate: 9600,
            stop_bits: STOP_BITS_1,
            parity: NO_PARITY,
            word_length: 8,
            dtr: true,
            control: 0,
            xonoff: 0,
            onlimit: 0,
            offlimit: 0,
            chars: [0; 6],
            // rdesktop's: reads wait 5 ms.
            timeouts: [0, 0, 5, 0, 0],
            wait_mask: 0,
            event_txempty: 0,
            event_cts: 0,
            event_dsr: 0,
            event_rlsd: 0,
        };
        s.get_termios()?;
        // Raw 8-bit I/O.
        let t = &mut s.termios;
        t.c_iflag &= !(libc::IGNBRK | libc::BRKINT | libc::PARMRK | libc::ISTRIP | libc::INLCR | libc::IGNCR | libc::ICRNL | libc::IXON);
        t.c_oflag &= !libc::OPOST;
        t.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN);
        t.c_cflag &= !(libc::CSIZE | libc::PARENB);
        t.c_cflag |= libc::CS8;
        // Reads never wait in the tty: rdpdr times them.
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 0;
        s.apply();
        Ok(s)
    }

    fn fd(&self) -> c_int {
        self.file.as_raw_fd()
    }

    fn apply(&self) {
        // SAFETY: an open tty and a termios from tcgetattr.
        unsafe { libc::tcsetattr(self.fd(), libc::TCSANOW, &self.termios) };
    }

    /// Read the tty's settings into the server's terms.
    fn get_termios(&mut self) -> io::Result<()> {
        // SAFETY: an open descriptor and a termios to fill.
        if unsafe { libc::tcgetattr(self.fd(), &mut self.termios) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let t = &self.termios;
        // SAFETY: a termios filled in above.
        let (ispeed, ospeed) = unsafe { (libc::cfgetispeed(t), libc::cfgetospeed(t)) };
        self.baud_rate = SPEEDS.iter().find(|&&(_, s)| s == ispeed).map_or(9600, |&(b, _)| b);
        self.dtr = ospeed != libc::B0;
        self.stop_bits = if t.c_cflag & libc::CSTOPB != 0 { STOP_BITS_2 } else { STOP_BITS_1 };
        self.parity = match (t.c_cflag & libc::PARENB != 0, t.c_cflag & libc::PARODD != 0) {
            (false, _) => NO_PARITY,
            (true, true) => ODD_PARITY,
            (true, false) => EVEN_PARITY,
        };
        self.word_length = match t.c_cflag & libc::CSIZE {
            libc::CS5 => 5,
            libc::CS6 => 6,
            libc::CS7 => 7,
            _ => 8,
        };
        self.control = DTR_CONTROL | ERROR_ABORT | if t.c_cflag & libc::CRTSCTS != 0 { CTS_HANDSHAKE } else { 0 };
        self.xonoff = DSR_SENSITIVITY;
        if t.c_iflag & libc::IXON != 0 {
            self.xonoff |= XON_HANDSHAKE;
        }
        if t.c_iflag & libc::IXOFF != 0 {
            self.xonoff |= XOFF_HANDSHAKE;
        }
        self.chars[CHAR_XON] = t.c_cc[libc::VSTART];
        self.chars[CHAR_XOFF] = t.c_cc[libc::VSTOP];
        self.chars[CHAR_EOF] = t.c_cc[libc::VEOF];
        self.chars[CHAR_BREAK] = t.c_cc[libc::VINTR];
        self.chars[CHAR_ERROR] = t.c_cc[libc::VKILL];
        Ok(())
    }

    /// Apply the server's settings to the tty.
    fn set_termios(&mut self) {
        let speed = SPEEDS.iter().find(|&&(b, _)| b == self.baud_rate).map_or(libc::B9600, |&(_, s)| s);
        let t = &mut self.termios;
        // SAFETY: a valid termios. Where input and output speeds are
        // separate, the input speed keeps the rate while B0 output drops DTR.
        unsafe {
            libc::cfsetispeed(t, speed);
            let out = if cfg!(any(target_os = "linux", target_os = "android")) || self.dtr { speed } else { libc::B0 };
            libc::cfsetospeed(t, out);
        }
        t.c_cflag &= !(libc::CSTOPB | libc::PARENB | libc::PARODD | libc::CSIZE | libc::CRTSCTS);
        if self.stop_bits == STOP_BITS_2 {
            t.c_cflag |= libc::CSTOPB;
        }
        match self.parity {
            EVEN_PARITY => t.c_cflag |= libc::PARENB,
            ODD_PARITY => t.c_cflag |= libc::PARENB | libc::PARODD,
            _ => {}
        }
        t.c_cflag |= match self.word_length {
            5 => libc::CS5,
            6 => libc::CS6,
            7 => libc::CS7,
            _ => libc::CS8,
        };
        if self.control & CTS_HANDSHAKE != 0 {
            t.c_cflag |= libc::CRTSCTS;
        }
        if self.xonoff & XON_HANDSHAKE != 0 {
            t.c_iflag |= libc::IXON | libc::IMAXBEL;
        }
        if self.xonoff & XOFF_HANDSHAKE != 0 {
            t.c_iflag |= libc::IXOFF | libc::IMAXBEL;
        }
        if self.xonoff & (XON_HANDSHAKE | XOFF_HANDSHAKE) == 0 {
            t.c_iflag &= !(libc::IXON | libc::IXOFF);
        }
        t.c_cc[libc::VSTART] = self.chars[CHAR_XON];
        t.c_cc[libc::VSTOP] = self.chars[CHAR_XOFF];
        t.c_cc[libc::VEOF] = self.chars[CHAR_EOF];
        t.c_cc[libc::VINTR] = self.chars[CHAR_BREAK];
        t.c_cc[libc::VKILL] = self.chars[CHAR_ERROR];
        self.apply();
    }

    fn modem_lines(&self) -> c_int {
        ioctl_get(self.fd(), tio::TIOCMGET).unwrap_or(0)
    }

    fn set_modem_line(&self, line: c_int, on: bool) {
        let lines = self.modem_lines();
        ioctl_set(self.fd(), tio::TIOCMSET, if on { lines | line } else { lines & !line });
    }

    fn control_from(&mut self, op: u32, r: &mut crate::stream::Reader) -> crate::error::Result<Control> {
        let mut w = crate::stream::Writer::new();
        match op {
            SET_BAUD_RATE => {
                self.baud_rate = r.u32()?;
                self.set_termios();
            }
            GET_BAUD_RATE => {
                w.u32(self.baud_rate);
            }
            SET_QUEUE_SIZE => {}
            SET_LINE_CONTROL => {
                self.stop_bits = r.u8()?;
                self.parity = r.u8()?;
                self.word_length = r.u8()?;
                self.set_termios();
            }
            GET_LINE_CONTROL => {
                w.u8(self.stop_bits).u8(self.parity).u8(self.word_length);
            }
            IMMEDIATE_CHAR => {
                let c = r.u8()?;
                if (&self.file).write(&[c]).is_ok_and(|n| n > 0) {
                    self.event_txempty = 1;
                }
            }
            CONFIG_SIZE => {
                w.u32(0);
            }
            GET_CHARS => {
                w.bytes(&self.chars);
            }
            SET_CHARS => {
                self.chars.copy_from_slice(r.bytes(6)?);
                self.set_termios();
            }
            GET_HANDFLOW => {
                let _ = self.get_termios();
                w.u32(self.control).u32(self.xonoff).u32(self.onlimit).u32(self.offlimit);
            }
            SET_HANDFLOW => {
                self.control = r.u32()?;
                self.xonoff = r.u32()?;
                self.onlimit = r.u32()?;
                self.offlimit = r.u32()?;
                self.set_termios();
            }
            SET_TIMEOUTS => {
                for t in &mut self.timeouts {
                    *t = r.u32()?;
                }
            }
            GET_TIMEOUTS => {
                for t in self.timeouts {
                    w.u32(t);
                }
            }
            GET_WAIT_MASK => {
                w.u32(self.wait_mask);
            }
            SET_WAIT_MASK => self.wait_mask = r.u32()?,
            SET_DTR | CLR_DTR => {
                self.dtr = op == SET_DTR;
                self.set_modem_line(tio::TIOCM_DTR, self.dtr);
            }
            SET_RTS | CLR_RTS => self.set_modem_line(tio::TIOCM_RTS, op == SET_RTS),
            GET_MODEMSTATUS => {
                let lines = self.modem_lines();
                let mut state = 0;
                for (line, ms) in [(tio::TIOCM_CTS, MS_CTS), (tio::TIOCM_DSR, MS_DSR), (tio::TIOCM_RNG, MS_RNG), (tio::TIOCM_CAR, MS_CAR), (tio::TIOCM_DTR, MS_DTR), (tio::TIOCM_RTS, MS_RTS)] {
                    if lines & line != 0 {
                        state |= ms;
                    }
                }
                w.u32(state);
            }
            GET_COMMSTATUS => {
                let inq = ioctl_get(self.fd(), tio::FIONREAD).unwrap_or(0);
                let outq = ioctl_get(self.fd(), tio::TIOCOUTQ).unwrap_or(0);
                w.u32(0).u32(0); // errors, hold reasons
                w.u32(inq.max(0) as u32).u32(outq.max(0) as u32);
                w.u8(0).u8(0); // EofReceived, WaitForImmediate
            }
            PURGE => {
                let mask = r.u32()?;
                let queue = match (mask & PURGE_TXCLEAR != 0, mask & PURGE_RXCLEAR != 0) {
                    (true, true) => Some(libc::TCIOFLUSH),
                    (true, false) => Some(libc::TCOFLUSH),
                    (false, true) => Some(libc::TCIFLUSH),
                    (false, false) => None,
                };
                if let Some(q) = queue {
                    // SAFETY: an open tty.
                    unsafe { libc::tcflush(self.fd(), q) };
                }
                return Ok(Control::Purge { read: mask & PURGE_RXABORT != 0, write: mask & PURGE_TXABORT != 0 });
            }
            WAIT_ON_MASK => {
                return Ok(match self.event() {
                    Some(ev) => Control::Done(ev.to_le_bytes().to_vec()),
                    None => Control::Pending,
                });
            }
            SET_BREAK_ON => {
                // SAFETY: an open tty.
                unsafe { libc::tcsendbreak(self.fd(), 0) };
            }
            SET_XON => {
                // SAFETY: an open tty.
                unsafe { libc::tcflow(self.fd(), libc::TCION) };
            }
            RESET_DEVICE | SET_BREAK_OFF | SET_XOFF => {}
            _ => return Ok(Control::Failed(STATUS_INVALID_PARAMETER)),
        }
        Ok(Control::Done(w.into_vec()))
    }
}

impl Port for Serial {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.file).read(buf)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = (&self.file).write(data)?;
        if n > 0 {
            self.event_txempty = c_int::try_from(n).unwrap_or(c_int::MAX);
        }
        Ok(n)
    }

    /// Windows' COMMTIMEOUTS: the total is multiplier × length + constant, the
    /// interval runs between characters, and MAXDWORD with no total means
    /// "return what's there".
    fn read_policy(&self, length: u32) -> ReadPolicy {
        let [interval, multiplier, constant, _, _] = self.timeouts;
        if interval == u32::MAX && multiplier == 0 && constant == 0 {
            return ReadPolicy::Immediate;
        }
        let total = u64::from(multiplier) * u64::from(length) + u64::from(constant);
        ReadPolicy::Wait {
            total: (total > 0).then(|| Duration::from_millis(total)),
            interval: (interval > 0 && interval != u32::MAX).then(|| Duration::from_millis(u64::from(interval))),
        }
    }

    fn control(&mut self, code: u32, input: &[u8], _out_max: usize) -> Control {
        if code >> 16 != FILE_DEVICE_SERIAL_PORT {
            return Control::Failed(STATUS_INVALID_PARAMETER);
        }
        let mut r = crate::stream::Reader::new(input);
        self.control_from((code >> 2) & 0xfff, &mut r).unwrap_or(Control::Failed(STATUS_INVALID_PARAMETER))
    }

    /// rdesktop's serial_get_event().
    fn event(&mut self) -> Option<u32> {
        let mut result = 0;
        let inq = ioctl_get(self.fd(), tio::FIONREAD).unwrap_or(0);
        if inq > 0 {
            if inq > self.event_rlsd {
                self.event_rlsd = inq;
                result |= EV_RLSD;
            }
            if inq > 1 {
                result |= EV_RXFLAG;
            }
            result |= EV_RXCHAR;
        } else {
            self.event_rlsd = 0;
        }
        if let Ok(outq) = ioctl_get(self.fd(), tio::TIOCOUTQ) {
            if outq == 0 && self.event_txempty > 0 {
                result |= EV_TXEMPTY;
            }
            self.event_txempty = outq;
        }
        let lines = self.modem_lines();
        if lines & tio::TIOCM_DSR != self.event_dsr {
            self.event_dsr = lines & tio::TIOCM_DSR;
            result |= EV_DSR;
        }
        if lines & tio::TIOCM_CTS != self.event_cts {
            self.event_cts = lines & tio::TIOCM_CTS;
            result |= EV_CTS;
        }
        let result = result & self.wait_mask;
        (result != 0).then_some(result)
    }
}
