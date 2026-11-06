//! Wakes the rdesktop thread out of select() from another thread. A loopback
//! UDP socket rather than a pipe, because Winsock can only select() on sockets.

use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::os::raw::c_int;

pub(crate) struct Wake {
    rx: UdpSocket,
    tx: UdpSocket,
}

impl Wake {
    pub fn new() -> io::Result<Self> {
        let rx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        let tx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        tx.connect(rx.local_addr()?)?;
        rx.set_nonblocking(true)?;
        tx.set_nonblocking(true)?;
        Ok(Wake { rx, tx })
    }

    /// A full socket buffer already guarantees a wakeup, so errors don't matter.
    pub fn signal(&self) {
        let _ = self.tx.send(&[0]);
    }

    pub fn drain(&self) {
        let mut buf = [0u8; 64];
        while self.rx.recv(&mut buf).is_ok() {}
    }

    #[cfg(unix)]
    pub fn socket(&self) -> c_int {
        std::os::fd::AsRawFd::as_raw_fd(&self.rx)
    }

    /// Windows socket handles are small numbers, despite the 64-bit type;
    /// rdesktop's own g_sock relies on the same thing.
    #[cfg(windows)]
    pub fn socket(&self) -> c_int {
        std::os::windows::io::AsRawSocket::as_raw_socket(&self.rx) as c_int
    }
}
