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

/// A server, a client, and the extension's numbers; None without Ghostscript.
struct Session {
    _server: vintrdp_x11::Server,
    x: X,
    major: u8,
    first_event: u8,
    first_error: u8,
    next: u32,
}

impl Session {
    fn new() -> Option<Session> {
        if !have_gs() {
            eprintln!("no Ghostscript: skipped");
            return None;
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
        let name = b"Adobe-DPS-Extension";
        let mut q = (name.len() as u16).to_le_bytes().to_vec();
        q.extend_from_slice(&[0, 0]);
        q.extend_from_slice(name);
        let seq = x.request(98, 0, &q);
        let r = x.answer(seq);
        Some(Session { _server: server, x, major: r[9], first_event: r[10], first_error: r[11], next: 1 })
    }

    fn id(&mut self) -> u32 {
        self.next += 1;
        self.x.base | self.next
    }

    /// A pixmap of the depth, filled with pixel 0, and a GC for it.
    fn pixmap(&mut self, depth: u8, w: u16, h: u16) -> (u32, u32) {
        let (pix, gc) = (self.id(), self.id());
        let mut p = u32s(&[pix, 0x100]);
        p.extend_from_slice(&w.to_le_bytes());
        p.extend_from_slice(&h.to_le_bytes());
        self.x.request(53, depth, &p);
        self.x.request(55, 0, &u32s(&[gc, pix, 0]));
        self.x.request(70, 0, &[&u32s(&[pix, gc])[..], &[0, 0, 0, 0], &w.to_le_bytes(), &h.to_le_bytes()].concat());
        (pix, gc)
    }

    /// A context on the pixmap, the origin at its bottom left; TrueColor's
    /// cube unless one's given.
    fn context(&mut self, pix: u32, gc: u32, h: i16, cmap: u32, cube: [u32; 10]) -> u32 {
        let (cx, sx) = (self.id(), self.id());
        let mut c = u32s(&[cx, sx, pix, gc]);
        c.extend_from_slice(&0i16.to_le_bytes());
        c.extend_from_slice(&h.to_le_bytes());
        c.extend(u32s(&[0x3, cmap]));
        c.extend(u32s(&cube));
        c.extend(u32s(&[0]));
        let seq = self.x.request(self.major, 2, &c);
        assert_eq!(self.x.answer(seq)[0], 1);
        cx
    }

    fn give(&mut self, cx: u32, ps: &str) {
        let mut b = (ps.len() as u16).to_le_bytes().to_vec();
        b.extend_from_slice(&[0, 0]);
        b.extend(u32s(&[cx]));
        b.extend_from_slice(ps.as_bytes());
        self.x.request(self.major, 4, &b);
    }

    /// The context's output, once it ends with `marker`.
    fn output_until(&mut self, cx: u32, marker: &str) -> String {
        let first = self.first_event;
        self.x.events_until(|e| output(e, first, cx).ends_with(marker.as_bytes()));
        let out = String::from_utf8_lossy(&output(&self.x.events, first, cx)).into_owned();
        self.x.events.clear();
        out
    }

    fn pixels(&mut self, pix: u32, w: u16, h: u16) -> Vec<u8> {
        self.x.sync();
        let mut g = u32s(&[pix]);
        g.extend_from_slice(&[0, 0, 0, 0]);
        g.extend_from_slice(&w.to_le_bytes());
        g.extend_from_slice(&h.to_le_bytes());
        g.extend(u32s(&[!0]));
        let seq = self.x.request(73, 2, &g);
        self.x.answer(seq)[32..].to_vec()
    }
}

const TRUECOLOR: [u32; 10] = [255, 65536, 255, 256, 255, 1, 0, 0, 0, 0];

#[test]
fn fork_join_and_the_context_operators() {
    let Some(mut s) = Session::new() else { return };
    let (pix, gc) = s.pixmap(24, 60, 40);
    let cx = s.context(pix, gc, 40, 0, TRUECOLOR);
    // A child gets its operands and the parent's definitions; join gives
    // its operand stack back, on a mark.
    s.give(cx, "/sq { dup mul } def mark 3 4 { sq exch sq add } fork join counttomark (n=) print =string cvs print (;) print exch pop =string cvs print (;) print flush\n");
    assert_eq!(s.output_until(cx, ";25;"), "n=1;25;");
    // Strings, arrays and procedures go both ways.
    s.give(cx, "mark (ab) [1 2] { length exch length add { 9 } } fork join exch pop exec =string cvs print (.) print flush\n");
    assert_eq!(s.output_until(cx, "."), "9.");
    // A child that notifies a condition its parent waits on, under a lock.
    s.give(
        cx,
        "/l lock def /c condition def \
         l { mark l c { /c exch def /l exch def l { c notify } monitor (child) } fork /k exch def l c wait } monitor \
         k join exch pop print (|) print flush\n",
    );
    assert_eq!(s.output_until(cx, "|"), "child|");
    // yield goes on; a detached context can't be joined; a failed one
    // can't either; monitor gives the lock back when its proc fails.
    s.give(cx, "yield (y) print mark { 1 } fork dup detach { join } stopped { $error /errorname get =string cvs print } if (;) print flush\n");
    assert_eq!(s.output_until(cx, ";"), "yinvalidcontext;");
    s.give(cx, "mark { nosuchthing } fork { join } stopped { $error /errorname get =string cvs print } if (;) print flush\n");
    assert_eq!(s.output_until(cx, ";"), "invalidcontext;");
    s.give(cx, "/m lock def { m { nosuch } monitor } stopped pop m { (again) print } monitor (;) print flush\n");
    assert!(s.output_until(cx, ";").ends_with("again;"));
    // wait without the lock, and joining yourself, are errors.
    s.give(cx, "{ lock condition wait } stopped { $error /errorname get =string cvs print } if (;) print flush\n");
    assert!(s.output_until(cx, ";").ends_with("invalidcontext;"));
    s.give(cx, "currentcontext { join } stopped { $error /errorname get =string cvs print } if (;) print flush\n");
    assert!(s.output_until(cx, ";").ends_with("invalidcontext;"));
    // A child paints on the parent's drawable, in its user space.
    s.give(cx, "mark { 0 1 0 setrgbcolor 0 0 10 10 rectfill flush 0 } fork join pop pop (p) print flush\n");
    s.output_until(cx, "p");
    let px = s.pixels(pix, 60, 40);
    let at = |x: usize, y: usize| u32::from_le_bytes(px[(y * 60 + x) * 4..][..4].try_into().unwrap()) & 0xff_ffff;
    assert_eq!((at(5, 35), at(20, 10)), (0x00_ff00, 0));
}

#[test]
fn view_clips() {
    let Some(mut s) = Session::new() else { return };
    let (pix, gc) = s.pixmap(24, 100, 80);
    let cx = s.context(pix, gc, 80, 0, TRUECOLOR);
    // 10..30 points: 13..40 pixels across, 40..67 down from the top.
    s.give(cx, "10 10 20 20 rectviewclip 1 0 0 setrgbcolor clippath fill viewclippath pathbbox 4 { round cvi =string cvs print ( ) print } repeat newpath (;) print flush\n");
    assert_eq!(s.output_until(cx, ";"), "30 30 10 10 ;");
    let px = s.pixels(pix, 100, 80);
    let at = |px: &[u8], x: usize, y: usize| u32::from_le_bytes(px[(y * 100 + x) * 4..][..4].try_into().unwrap()) & 0xff_ffff;
    assert_eq!((at(&px, 20, 55), at(&px, 5, 55), at(&px, 20, 30), at(&px, 60, 10)), (0xff_0000, 0, 0, 0));
    // A triangle, by the path; then none.
    s.give(cx, "newpath 40 0 moveto 70 0 lineto 70 30 lineto closepath viewclip 0 0 1 setrgbcolor 0 0 100 100 rectfill initviewclip 0 1 0 setrgbcolor 0 0 3 3 rectfill (;) print flush\n");
    s.output_until(cx, ";");
    let px = s.pixels(pix, 100, 80);
    // Inside the triangle (below its diagonal), outside it, and the corner.
    assert_eq!(at(&px, 90, 75), 0x00_00ff);
    assert_eq!(at(&px, 56, 45), 0);
    assert_eq!(at(&px, 1, 78), 0x00_ff00);
    // Even-odd: a square with a hole.
    s.give(cx, "newpath 0 40 20 20 vrect 5 45 10 10 vrect eoviewclip 1 1 0 setrgbcolor 0 0 100 100 rectfill initviewclip (;) print flush\n".replace("vrect", "4 dict begin /h exch def /w exch def /y exch def /x exch def x y moveto w 0 rlineto 0 h rlineto w neg 0 rlineto closepath end").as_str());
    s.output_until(cx, ";");
    let px = s.pixels(pix, 100, 80);
    assert_eq!((at(&px, 2, 25), at(&px, 13, 13)), (0xff_ff00, 0));
}

#[test]
fn reset_and_interrupt() {
    let Some(mut s) = Session::new() else { return };
    let (pix, gc) = s.pixmap(24, 20, 20);
    let cx = s.context(pix, gc, 20, 0, TRUECOLOR);
    let reset = |s: &mut Session| {
        let m = s.major;
        s.x.request(m, 7, &u32s(&[cx]));
    };
    let interrupt = |s: &mut Session| {
        let m = s.major;
        s.x.request(m, 8, &[&u32s(&[cx])[..], &[1, 0, 0, 0]].concat());
    };
    // PSReset: the stacks emptied; input not yet run thrown away.
    s.give(cx, "/keep 42 def 1 2 3 (a) print flush\n");
    s.output_until(cx, "a");
    reset(&mut s);
    s.give(cx, "count =string cvs print (;) print flush\n");
    assert_eq!(s.output_until(cx, ";"), "0;");
    s.give(cx, "0 1 30000000 { pop } for (busy) print flush\n");
    s.give(cx, "(thrown away) print flush\n");
    reset(&mut s);
    s.give(cx, "(after) print flush\n");
    let out = s.output_until(cx, "after");
    assert!(out == "busyafter", "{out}");
    // An interrupt while it waits for input, and while it paints in a
    // loop: interrupt is raised (reported, tagged 250), and the context
    // goes on with its VM.
    interrupt(&mut s);
    s.give(cx, "(i) print flush\n");
    let out = s.output_until(cx, "i");
    assert!(out.contains("interrupt"), "{out:?}");
    s.give(cx, "{ 0 0 1 1 rectfill } loop\n");
    std::thread::sleep(Duration::from_millis(500));
    interrupt(&mut s);
    s.give(cx, "keep =string cvs print (;) print flush\n");
    let out = s.output_until(cx, ";");
    assert!(out.contains("interrupt") && out.ends_with("42;"), "{out:?}");
    // A loop that never paints: started again after three seconds, with
    // interrupt raised there, and the VM gone.
    s.give(cx, "{ } loop\n");
    std::thread::sleep(Duration::from_millis(300));
    interrupt(&mut s);
    s.give(cx, "/keep where { pop (kept) } { (lost) } ifelse print (;) print flush\n");
    let out = s.output_until(cx, ";");
    assert!(out.contains("interrupt") && out.ends_with("lost;"), "{out:?}");
    let _ = s.first_error;
}

#[test]
fn rgb_actual() {
    let Some(mut s) = Session::new() else { return };
    // TrueColor: always exact.
    let (pix, gc) = s.pixmap(24, 20, 20);
    let cx = s.context(pix, gc, 20, 0, TRUECOLOR);
    s.give(cx, "0.2 0.4 0.6 setXrgbactual =string cvs print 0 0 20 20 rectfill (;) print flush\n");
    assert_eq!(s.output_until(cx, ";"), "true;");
    let px = s.pixels(pix, 20, 20);
    assert_eq!(u32::from_le_bytes(px[..4].try_into().unwrap()) & 0xff_ffff, 0x33_6699);
    // An 8-bit PseudoColor map, with a 2x2x2 cube from pixel 0 that can't
    // make the colour: setXrgbactual gets a cell for it, and it's painted.
    let visual = 0x23; // the 8-bit visual beside the 24-bit screen (visual.rs)
    let cmap = s.id();
    s.x.request(78, 0, &u32s(&[cmap, 0x100, visual]));
    let (pix8, gc8) = s.pixmap(8, 20, 20);
    let cx8 = s.context(pix8, gc8, 20, cmap, [1, 4, 1, 2, 1, 1, 0, 0, 0, 0]);
    s.give(cx8, "0.2 0.4 0.6 setXrgbactual =string cvs print 0 0 20 20 rectfill (;) print flush\n");
    assert_eq!(s.output_until(cx8, ";"), "true;");
    let px = s.pixels(pix8, 20, 20);
    let pixel = px[0] as u32;
    // QueryColors: it is that colour.
    let seq = s.x.request(91, 0, &u32s(&[cmap, pixel]));
    let r = s.x.answer(seq);
    let rgb: Vec<u16> = (0..3).map(|i| u16::from_le_bytes([r[32 + 2 * i], r[33 + 2 * i]])).collect();
    assert_eq!(rgb, [0x3333, 0x6666, 0x9999]);
    // Another colour, without setXrgbactual: the cube's nearest, 1 0 0,
    // pixel 4.
    s.give(cx8, "0.7 0.1 0.1 setrgbcolor 0 0 5 5 rectfill (;) print flush\n");
    s.output_until(cx8, ";");
    assert_eq!(s.pixels(pix8, 20, 20)[19 * 20], 4);
}
