//! Display PostScript, by a client speaking the protocol itself (as
//! Adobe's XDPSproto.h has it): a context on a pixmap, painting, output
//! and errors back as output events, setXgcdrawable, a second XID.
//! Skipped without Ghostscript (gs on the PATH, or VINTRDP_GS).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use vintrdp_x11::keymap::Keymap;
use vintrdp_x11::{Host, Settings};

struct Nothing;
impl Host for Nothing {
    fn resize(&self, _: u16, _: u16) {}
    fn draw(&self, _: usize, _: usize, _: usize, _: usize, _: &[u32]) {}
    fn updated(&self) {}
}

fn have_gs() -> bool {
    if std::env::var_os("VINTRDP_GS").is_some() {
        return true;
    }
    std::process::Command::new("gs").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// A client: the requests least significant byte first.
struct X {
    s: TcpStream,
    seq: u16,
    base: u32,
    events: Vec<[u8; 32]>,
}

impl X {
    fn connect(addr: std::net::SocketAddr) -> X {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        s.write_all(&[b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        let mut head = [0u8; 8];
        s.read_exact(&mut head).unwrap();
        assert_eq!(head[0], 1, "setup accepted");
        let mut rest = vec![0u8; u16::from_le_bytes([head[6], head[7]]) as usize * 4];
        s.read_exact(&mut rest).unwrap();
        let base = u32::from_le_bytes(rest[4..8].try_into().unwrap());
        X { s, seq: 0, base, events: Vec::new() }
    }

    fn request(&mut self, major: u8, data: u8, body: &[u8]) -> u16 {
        let mut b = vec![major, data, 0, 0];
        b.extend_from_slice(body);
        while b.len() % 4 != 0 {
            b.push(0);
        }
        let n = (b.len() / 4) as u16;
        b[2..4].copy_from_slice(&n.to_le_bytes());
        self.s.write_all(&b).unwrap();
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    /// The next reply or error to `seq`, keeping events.
    fn answer(&mut self, seq: u16) -> Vec<u8> {
        loop {
            let mut b = [0u8; 32];
            self.s.read_exact(&mut b).unwrap();
            match b[0] {
                1 => {
                    let mut rest = vec![0u8; u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize * 4];
                    self.s.read_exact(&mut rest).unwrap();
                    if u16::from_le_bytes([b[2], b[3]]) == seq {
                        let mut r = b.to_vec();
                        r.extend(rest);
                        return r;
                    }
                }
                0 if u16::from_le_bytes([b[2], b[3]]) == seq => return b.to_vec(),
                0 => panic!("an error: {b:?}"),
                _ => self.events.push(b),
            }
        }
    }

    fn sync(&mut self) {
        let seq = self.request(43, 0, &[]); // GetInputFocus
        self.answer(seq);
    }

    /// Events until `want` says it has what it waits for.
    fn events_until(&mut self, mut want: impl FnMut(&[[u8; 32]]) -> bool) {
        while !want(&self.events) {
            let mut b = [0u8; 32];
            self.s.read_exact(&mut b).unwrap();
            assert_ne!(b[0], 0, "an error: {b:?}");
            if b[0] != 1 {
                self.events.push(b);
            }
        }
    }
}

fn u32s(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// The output events' bytes for a context.
fn output(events: &[[u8; 32]], first: u8, cxid: u32) -> Vec<u8> {
    events.iter().filter(|e| e[0] & 0x7f == first && u32::from_le_bytes(e[4..8].try_into().unwrap()) == cxid).flat_map(|e| e[8..8 + e[1] as usize].to_vec()).collect()
}

#[test]
fn a_context_paints_and_answers() {
    if !have_gs() {
        eprintln!("no Ghostscript: skipped");
        return;
    }
    let settings = Settings {
        width: 320,
        height: 240,
        listen: "127.0.0.1:0".parse().unwrap(),
        unix_socket: false,
        allow: Vec::new(),
        keymap: Keymap::new(|_, _| None),
        xdmcp: None,
        cookie: None,
        font_path: Vec::new(),
        primary: false,
        visual: Default::default(),
        verbose: false,
    };
    let server = vintrdp_x11::start(settings, Box::new(Nothing)).unwrap();
    let mut x = X::connect(server.local_addr());

    // QueryExtension, under both names.
    let name = b"Adobe-DPS-Extension";
    let mut q = (name.len() as u16).to_le_bytes().to_vec();
    q.extend_from_slice(&[0, 0]);
    q.extend_from_slice(name);
    let seq = x.request(98, 0, &q);
    let r = x.answer(seq);
    assert_eq!(r[8], 1, "present");
    let (major, first_event, first_error) = (r[9], r[10], r[11]);
    let name = b"DPSExtension";
    let mut q = (name.len() as u16).to_le_bytes().to_vec();
    q.extend_from_slice(&[0, 0]);
    q.extend_from_slice(name);
    let seq = x.request(98, 0, &q);
    assert_eq!(x.answer(seq)[9], major);

    // PSInit: protocol 9.
    let seq = x.request(major, 1, &u32s(&[9]));
    let r = x.answer(seq);
    assert_eq!(u32::from_le_bytes(r[8..12].try_into().unwrap()), 9);

    // A pixmap of 120x90 (root's depth, 24), black, and a GC.
    let (root, pix, gc, cx, sx) = (0x100, x.base | 1, x.base | 2, x.base | 3, x.base | 4);
    let mut p = u32s(&[pix, root]);
    p.extend_from_slice(&[120, 0, 90, 0]);
    x.request(53, 24, &p);
    x.request(55, 0, &u32s(&[gc, pix, 0]));
    x.request(70, 0, &[&u32s(&[pix, gc])[..], &[0, 0, 0, 0, 120, 0, 90, 0]].concat());

    // PSCreateContext: origin at the bottom left, TrueColor's cube.
    let mut c = u32s(&[cx, sx, pix, gc]);
    c.extend_from_slice(&0i16.to_le_bytes());
    c.extend_from_slice(&90i16.to_le_bytes());
    c.extend(u32s(&[0x3, 0, 255, 65536, 255, 256, 255, 1, 0, 0, 0, 0, 0]));
    let seq = x.request(major, 2, &c);
    let r = x.answer(seq);
    assert_eq!(r[0], 1, "a context: {r:?}");
    let cpsid = u32::from_le_bytes(r[8..12].try_into().unwrap());

    let give = |x: &mut X, cxid: u32, ps: &[u8]| {
        let mut b = (ps.len() as u16).to_le_bytes().to_vec();
        b.extend_from_slice(&[0, 0]);
        b.extend(u32s(&[cxid]));
        b.extend_from_slice(ps);
        x.request(major, 4, &b);
    };
    // Red, 30 points (40 pixels at 96 dpi) square from the origin; then
    // output, a number as a binary object, and an error.
    give(&mut x, cx, b"1 0 0 setrgbcolor 0 0 30 30 rectfill (hello) print currentcontext 1 printobject flush nosuchthing (done) print flush\n");
    x.events_until(|e| output(e, first_event, cx).ends_with(b"done"));
    let out = output(&x.events, first_event, cx);
    assert!(out.starts_with(b"hello"), "{out:?}");
    // currentcontext, tagged 1: a binary object sequence of one integer.
    let i = out.windows(2).position(|w| w[0] >= 128 && w[0] <= 131 && w[1] == 1).expect("a binary object sequence");
    let seq_bytes = &out[i..i + 12];
    assert_eq!(seq_bytes[5], 1, "its tag");
    let big = seq_bytes[0] == 128;
    let v = if big { u32::from_be_bytes(seq_bytes[8..12].try_into().unwrap()) } else { u32::from_le_bytes(seq_bytes[8..12].try_into().unwrap()) };
    assert_eq!(v, cpsid);
    // The error: tagged 250, [/Error /undefined /nosuchthing false].
    assert!(out.windows(9).any(|w| w == b"undefined"), "{out:?}");
    assert!(out.windows(2).any(|w| w[0] >= 128 && w[0] <= 131 && w[1] == 1) && out.iter().any(|&b| b == 250));

    // The pixmap: red where it was painted, black elsewhere.
    x.sync();
    let mut g = u32s(&[pix]);
    g.extend_from_slice(&[0, 0, 0, 0, 120, 0, 90, 0]);
    g.extend(u32s(&[!0]));
    let seq = x.request(73, 2, &g);
    let img = x.answer(seq);
    let px = |x: usize, y: usize| u32::from_le_bytes(img[32 + (y * 120 + x) * 4..][..4].try_into().unwrap()) & 0xff_ffff;
    assert_eq!(px(5, 85), 0xff_0000, "painted, from the bottom left");
    assert_eq!(px(39, 50), 0xff_0000);
    assert_eq!(px(41, 85), 0);
    assert_eq!(px(5, 48), 0);

    // GetStatus: needs input. ContextFromXID gives its number.
    let seq = x.request(major, 5, &u32s(&[cx, 0]));
    assert_eq!(x.answer(seq)[1], 2);
    let seq = x.request(major, 11, &u32s(&[cx]));
    assert_eq!(u32::from_le_bytes(x.answer(seq)[8..12].try_into().unwrap()), cpsid);

    // setXgcdrawable to a second pixmap; the user space follows the new origin.
    let pix2 = x.base | 5;
    let mut p = u32s(&[pix2, root]);
    p.extend_from_slice(&[50, 0, 40, 0]);
    x.request(53, 24, &p);
    x.request(70, 0, &[&u32s(&[pix2, gc])[..], &[0, 0, 0, 0, 50, 0, 40, 0]].concat());
    give(&mut x, cx, format!("{gc} {pix2} 0 40 setXgcdrawable initmatrix 0 0 1 setrgbcolor 0 0 6 6 rectfill (ok) print flush\n").as_bytes());
    x.events_until(|e| output(e, first_event, cx).ends_with(b"ok"));
    x.sync();
    let mut g = u32s(&[pix2]);
    g.extend_from_slice(&[0, 0, 0, 0, 50, 0, 40, 0]);
    g.extend(u32s(&[!0]));
    let seq = x.request(73, 2, &g);
    let img = x.answer(seq);
    let px2 = |x: usize, y: usize| u32::from_le_bytes(img[32 + (y * 50 + x) * 4..][..4].try_into().unwrap()) & 0xff_ffff;
    assert_eq!(px2(2, 38), 0x00_00ff);
    assert_eq!(px2(20, 20), 0);

    // An unknown context: the extension's first error, BadContext.
    let mut b = 0u16.to_le_bytes().to_vec();
    b.extend_from_slice(&[0, 0]);
    b.extend(u32s(&[x.base | 99]));
    let seq = x.request(major, 4, &b);
    let e = x.answer(seq);
    assert_eq!((e[0], e[1], u32::from_le_bytes(e[4..8].try_into().unwrap())), (0, first_error, x.base | 99));
    assert_eq!((e[10], u16::from_le_bytes([e[8], e[9]])), (major, 4));
}
