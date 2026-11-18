//! A whole session against a fake RFB server on localhost.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{Config, Event, Frontend, Session};

#[derive(Default)]
struct Events(Mutex<Vec<String>>);

impl Frontend for Events {
    fn notify(&self, event: Event) {
        let text = match event {
            Event::Updated => return,
            Event::Clipboard(t) => format!("clipboard {t}"),
            e => format!("{e:?}"),
        };
        self.0.lock().unwrap().push(text);
    }
}

fn read_n(s: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut b = vec![0; n];
    s.read_exact(&mut b).unwrap();
    b
}

/// Read client messages until a KeyEvent; returns its (down, keysym).
fn next_key(s: &mut TcpStream) -> (bool, u32) {
    loop {
        let t = read_n(s, 1)[0];
        match t {
            0 => drop(read_n(s, 19)),
            2 => {
                let h = read_n(s, 3);
                drop(read_n(s, 4 * u16::from_be_bytes([h[1], h[2]]) as usize));
            }
            3 => drop(read_n(s, 9)),
            4 => {
                let b = read_n(s, 7);
                return (b[0] == 1, u32::from_be_bytes([b[3], b[4], b[5], b[6]]));
            }
            5 => drop(read_n(s, 5)),
            6 => {
                let h = read_n(s, 7);
                drop(read_n(s, u32::from_be_bytes([h[3], h[4], h[5], h[6]]) as usize));
            }
            t => panic!("client message {t}"),
        }
    }
}

fn wait_for(what: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !what() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn session_against_a_fake_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.write_all(b"RFB 003.008\n").unwrap();
        assert_eq!(read_n(&mut s, 12), b"RFB 003.008\n");
        // Offer None and VNC Authentication; a client with a password picks VNC.
        s.write_all(&[2, 1, 2]).unwrap();
        assert_eq!(read_n(&mut s, 1), [2]);
        let challenge = *b"0123456789abcdef";
        s.write_all(&challenge).unwrap();
        assert_eq!(read_n(&mut s, 16), super::auth::vnc_response("pw", &challenge));
        s.write_all(&0u32.to_be_bytes()).unwrap();
        assert_eq!(read_n(&mut s, 1), [1]); // shared
        // ServerInit: 4x2, a pixel format, the name "fake".
        let mut init = vec![0, 4, 0, 2];
        init.extend(super::decode::Pf::ours(32).to_bytes());
        init.extend(4u32.to_be_bytes());
        init.extend(b"fake");
        s.write_all(&init).unwrap();
        // SetPixelFormat: 32 bpp; then SetEncodings, then a full update request.
        let spf = read_n(&mut s, 20);
        assert_eq!((spf[0], spf[4]), (0, 32));
        let se = read_n(&mut s, 4);
        assert_eq!(se[0], 2);
        let encodings = read_n(&mut s, 4 * u16::from_be_bytes([se[2], se[3]]) as usize);
        assert!(encodings.chunks(4).any(|e| i32::from_be_bytes([e[0], e[1], e[2], e[3]]) == 16), "asks for ZRLE");
        assert_eq!(read_n(&mut s, 10), [3, 0, 0, 0, 0, 0, 0, 4, 0, 2]);
        // An update: a raw 4x2 picture.
        let mut m = vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 4, 0, 2, 0, 0, 0, 0];
        for i in 0..8u32 {
            m.extend((0x10_0000 * i + i).to_le_bytes());
        }
        // A bell and clipboard text.
        m.push(2);
        m.extend([3, 0, 0, 0, 0, 0, 0, 5]);
        m.extend(b"hello");
        s.write_all(&m).unwrap();
        next_key(&mut s)
    });

    let config = Config::from_args(["-p", "pw", &format!("vnc://127.0.0.1::{port}")]).unwrap();
    let events = Arc::new(Events::default());
    let session = Session::start(config, events.clone()).unwrap();
    wait_for(|| session.with_framebuffer(|_, _, px| px[7] == 0x70_0007).unwrap_or(false));
    session.with_framebuffer(|w, h, px| {
        assert_eq!((w, h), (4, 2));
        assert_eq!(px[1], 0x10_0001);
    });
    wait_for(|| events.0.lock().unwrap().iter().any(|e| e == "clipboard hello"));
    session.key(0x1e, false, true);
    assert_eq!(server.join().unwrap(), (true, 'a' as u32));
    let seen = events.0.lock().unwrap().clone();
    assert!(seen.iter().any(|e| e.starts_with("Resized { width: 4, height: 2")), "{seen:?}");
    assert!(seen.iter().any(|e| e == "Bell"), "{seen:?}");
    assert!(seen.iter().any(|e| e.contains("(VNC) - vintrdp")), "{seen:?}");
    session.quit();
    assert!(session.wait(Duration::from_secs(5)).is_some());
}

#[test]
fn versions() {
    let v = |s: &[u8; 12]| super::protocol_minor(s).ok();
    assert_eq!(v(b"RFB 003.003\n"), Some(3));
    assert_eq!(v(b"RFB 003.006\n"), Some(3));
    assert_eq!(v(b"RFB 003.007\n"), Some(7));
    assert_eq!(v(b"RFB 003.008\n"), Some(8));
    assert_eq!(v(b"RFB 003.889\n"), Some(8));
    assert_eq!(v(b"RFB 004.001\n"), Some(8));
    assert_eq!(v(b"SSH-2.0-xyz\n"), None);
}
