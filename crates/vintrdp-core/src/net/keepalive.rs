//! Noticing a server that went away without closing the connection (powered
//! off, unplugged, its network down). RDP 4 and 5 have no heartbeat, and an
//! idle session can go minutes without a byte, so the reader would wait
//! forever. TCP keepalive probes an idle connection; a timeout on unacknowledged
//! data covers a connection we were writing to. Either way the reader then
//! fails, and the session ends with a message.
//!
//! Each setting is best effort: a system without it keeps its defaults (NT 4.0
//! has no per-socket keepalive timing, so it keeps its two hours).

use std::net::TcpStream;

/// Probe after this many seconds of silence.
const IDLE: u32 = 15;
/// Then every this many seconds.
const INTERVAL: u32 = 3;
/// Give up after this many unanswered probes: about 30 s in all. (Windows
/// fixes the count itself.)
#[cfg(unix)]
const PROBES: u32 = 5;
/// Unacknowledged data gives up after this many seconds.
const DATA_TIMEOUT: u32 = 30;

#[cfg(unix)]
pub fn enable(tcp: &TcpStream) {
    use std::os::fd::AsRawFd;
    let fd = tcp.as_raw_fd();
    let set = |level: libc::c_int, name: libc::c_int, value: u32| {
        let v = value as libc::c_int;
        // SAFETY: a valid socket and a c_int option value of the right size.
        unsafe { libc::setsockopt(fd, level, name, (&v as *const libc::c_int).cast(), size_of::<libc::c_int>() as libc::socklen_t) };
    };
    set(libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
    #[cfg(target_vendor = "apple")]
    {
        // netinet/tcp.h's TCP_RXT_CONNDROPTIME, which libc doesn't name.
        const TCP_RXT_CONNDROPTIME: libc::c_int = 0x80;
        set(libc::IPPROTO_TCP, libc::TCP_KEEPALIVE, IDLE);
        set(libc::IPPROTO_TCP, TCP_RXT_CONNDROPTIME, DATA_TIMEOUT);
    }
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd", target_os = "netbsd", target_os = "dragonfly"))]
    set(libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, IDLE);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    set(libc::IPPROTO_TCP, libc::TCP_USER_TIMEOUT, DATA_TIMEOUT * 1000);
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android", target_os = "freebsd", target_os = "netbsd", target_os = "dragonfly"))]
    {
        set(libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, INTERVAL);
        set(libc::IPPROTO_TCP, libc::TCP_KEEPCNT, PROBES);
    }
}

#[cfg(windows)]
pub fn enable(tcp: &TcpStream) {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawSocket;
    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn setsockopt(s: usize, level: i32, name: i32, value: *const c_void, len: i32) -> i32;
        #[link_name = "WSAIoctl"]
        fn wsa_ioctl(
            s: usize,
            code: u32,
            input: *const c_void,
            input_len: u32,
            output: *mut c_void,
            output_len: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
            completion: *mut c_void,
        ) -> i32;
    }
    const SOL_SOCKET: i32 = 0xffff;
    const SO_KEEPALIVE: i32 = 8;
    const IPPROTO_TCP: i32 = 6;
    const TCP_MAXRT: i32 = 5;
    const SIO_KEEPALIVE_VALS: u32 = 0x9800_0004;
    #[repr(C)]
    struct TcpKeepalive {
        on: u32,
        time_ms: u32,
        interval_ms: u32,
    }
    let s = tcp.as_raw_socket() as usize;
    let on: u32 = 1;
    let maxrt: u32 = DATA_TIMEOUT;
    // Windows sends 10 probes (Vista on) or 5 (before) at this interval.
    let vals = TcpKeepalive { on: 1, time_ms: IDLE * 1000, interval_ms: INTERVAL * 1000 / 2 };
    let mut returned = 0u32;
    // SAFETY: a valid socket; each option is a DWORD, the ioctl takes a
    // tcp_keepalive and returns nothing. Failure (NT 4.0 knows neither
    // SIO_KEEPALIVE_VALS nor TCP_MAXRT) leaves the defaults.
    unsafe {
        setsockopt(s, SOL_SOCKET, SO_KEEPALIVE, (&on as *const u32).cast(), 4);
        setsockopt(s, IPPROTO_TCP, TCP_MAXRT, (&maxrt as *const u32).cast(), 4);
        wsa_ioctl(
            s,
            SIO_KEEPALIVE_VALS,
            (&vals as *const TcpKeepalive).cast(),
            size_of::<TcpKeepalive>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(any(unix, windows)))]
pub fn enable(_tcp: &TcpStream) {}

#[cfg(all(test, unix))]
mod tests {
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsRawFd;

    fn get(tcp: &TcpStream, level: libc::c_int, name: libc::c_int) -> libc::c_int {
        let mut v: libc::c_int = 0;
        let mut len = size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: a valid socket and a c_int-sized buffer.
        unsafe { libc::getsockopt(tcp.as_raw_fd(), level, name, (&mut v as *mut libc::c_int).cast(), &mut len) };
        v
    }

    #[test]
    fn sets_keepalive_and_timers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        super::enable(&tcp);
        assert_ne!(get(&tcp, libc::SOL_SOCKET, libc::SO_KEEPALIVE), 0);
        #[cfg(target_vendor = "apple")]
        assert_eq!(get(&tcp, libc::IPPROTO_TCP, libc::TCP_KEEPALIVE), super::IDLE as libc::c_int);
        #[cfg(target_os = "linux")]
        assert_eq!(get(&tcp, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE), super::IDLE as libc::c_int);
        #[cfg(any(target_vendor = "apple", target_os = "linux"))]
        {
            assert_eq!(get(&tcp, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL), super::INTERVAL as libc::c_int);
            assert_eq!(get(&tcp, libc::IPPROTO_TCP, libc::TCP_KEEPCNT), super::PROBES as libc::c_int);
        }
    }
}
