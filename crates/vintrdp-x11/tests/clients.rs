//! Real clients against the server: x11rb's (in this machine's byte order),
//! and a big-endian one written out by hand, as a SPARC would speak.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use vintrdp_x11::keymap::Keymap;
use vintrdp_x11::{Host, Server, Settings};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, PropMode};
use x11rb::rust_connection::{DefaultStream, RustConnection};
use x11rb::wrapper::ConnectionExt as _;

#[derive(Clone, Default)]
struct Screen(Arc<Mutex<(u16, u16, usize)>>);

impl Host for Screen {
    fn resize(&self, width: u16, height: u16) {
        let mut s = self.0.lock().unwrap();
        (s.0, s.1) = (width, height);
    }
    fn draw(&self, _x: usize, _y: usize, w: usize, h: usize, _px: &[u32]) {
        self.0.lock().unwrap().2 += w * h;
    }
    fn updated(&self) {}
}

fn server() -> (Server, Screen) {
    let screen = Screen::default();
    let settings = Settings {
        width: 800,
        height: 600,
        listen: "127.0.0.1:0".parse().unwrap(),
        unix_socket: false,
        allow: Vec::new(),
        keymap: Keymap::new(|sc, ext| (sc == 0x1e && !ext).then_some([0x61, 0x41])),
        xdmcp: None,
        cookie: None,
        font_path: Vec::new(),
        primary: false,
        visual: Default::default(),
        verbose: false,
    };
    (vintrdp_x11::start(settings, Box::new(screen.clone())).unwrap(), screen)
}

fn connect(server: &Server) -> RustConnection {
    let stream = DefaultStream::from_tcp_stream(TcpStream::connect(server.local_addr()).unwrap()).unwrap().0;
    RustConnection::connect_to_stream(stream, 0).unwrap()
}

#[test]
fn xdpyinfo_and_xprop() {
    let (server, screen) = server();
    let conn = connect(&server);
    let setup = conn.setup();
    assert_eq!(setup.vendor, b"vintrdp");
    let root = setup.roots[0].root;
    assert_eq!((setup.roots[0].width_in_pixels, setup.roots[0].height_in_pixels, setup.roots[0].root_depth), (800, 600, 24));
    assert_eq!(*screen.0.lock().unwrap(), (800, 600, 800 * 600));

    // BIG-REQUESTS, through x11rb's own negotiation.
    assert_eq!(conn.maximum_request_bytes(), 16 << 20);
    let exts = conn.list_extensions().unwrap().reply().unwrap();
    let names: Vec<&[u8]> = exts.names.iter().map(|n| n.name.as_slice()).collect();
    assert_eq!(names, [&b"BIG-REQUESTS"[..], b"SHAPE", b"XC-MISC"]);
    assert!(!conn.query_extension(b"XKEYBOARD").unwrap().reply().unwrap().present);

    let atom = conn.intern_atom(false, b"_VINTRDP_TEST").unwrap().reply().unwrap().atom;
    assert_eq!(conn.get_atom_name(atom).unwrap().reply().unwrap().name, b"_VINTRDP_TEST");
    assert_eq!(conn.intern_atom(true, b"_NOT_THERE").unwrap().reply().unwrap().atom, 0);

    conn.change_property32(PropMode::REPLACE, root, atom, AtomEnum::CARDINAL, &[1, 2]).unwrap();
    conn.change_property32(PropMode::APPEND, root, atom, AtomEnum::CARDINAL, &[3]).unwrap();
    conn.change_property8(PropMode::REPLACE, root, AtomEnum::RESOURCE_MANAGER, AtomEnum::STRING, b"*background: grey\n").unwrap();
    let p = conn.get_property(false, root, atom, AtomEnum::ANY, 0, 100).unwrap().reply().unwrap();
    assert_eq!(p.value32().unwrap().collect::<Vec<_>>(), [1, 2, 3]);
    // Part of it: offset and length count 4-byte units.
    let p = conn.get_property(false, root, atom, AtomEnum::CARDINAL, 1, 1).unwrap().reply().unwrap();
    assert_eq!((p.value32().unwrap().collect::<Vec<_>>(), p.bytes_after), (vec![2], 4));
    // The wrong type: the property's type and size, no value.
    let p = conn.get_property(false, root, atom, AtomEnum::STRING, 0, 100).unwrap().reply().unwrap();
    assert_eq!((p.type_, p.bytes_after, p.value.len()), (AtomEnum::CARDINAL.into(), 12, 0));
    let list = conn.list_properties(root).unwrap().reply().unwrap().atoms;
    assert_eq!(list, [atom, AtomEnum::RESOURCE_MANAGER.into()]);
    conn.delete_property(root, atom).unwrap();
    assert_eq!(conn.list_properties(root).unwrap().reply().unwrap().atoms.len(), 1);
    // Prepending a different format is an error, not a disconnect.
    let e = conn.change_property16(PropMode::PREPEND, root, AtomEnum::RESOURCE_MANAGER, AtomEnum::STRING, &[1]).unwrap().check();
    assert!(e.is_err());

    let keys = conn.get_keyboard_mapping(8, 248).unwrap().reply().unwrap();
    assert_eq!((keys.keysyms_per_keycode, keys.keysyms.len()), (2, 496));
    assert_eq!(&keys.keysyms[(38 - 8) * 2..][..2], &[0x61, 0x41]);
    let mods = conn.get_modifier_mapping().unwrap().reply().unwrap();
    assert_eq!(&mods.keycodes[..2], &[50, 62]);
    assert_eq!(conn.get_pointer_mapping().unwrap().reply().unwrap().map, [1, 2, 3, 4, 5]);
    assert_eq!(conn.get_input_focus().unwrap().reply().unwrap().focus, 1);
    let attrs = conn.get_window_attributes(root).unwrap().reply().unwrap();
    assert_eq!(attrs.colormap, setup.roots[0].default_colormap);
    let geo = conn.get_geometry(root).unwrap().reply().unwrap();
    assert_eq!((geo.width, geo.height, geo.depth), (800, 600, 24));
    assert!(conn.query_tree(root).unwrap().reply().unwrap().children.is_empty());
    let best = conn.query_best_size(x11rb::protocol::xproto::QueryShapeOf::LARGEST_CURSOR, root, 65535, 65535).unwrap().reply().unwrap();
    assert_eq!((best.width, best.height), (64, 64));
    conn.get_keyboard_control().unwrap().reply().unwrap();
    conn.get_screen_saver().unwrap().reply().unwrap();
    assert_eq!(conn.list_fonts(100, b"fixed").unwrap().reply().unwrap().names[0].name, b"fixed");

    // An error, and the connection carries on.
    let window = conn.generate_id().unwrap();
    assert!(conn.create_window(0, window, root, 0, 0, 0, 10, 0, x11rb::protocol::xproto::WindowClass::COPY_FROM_PARENT, 0, &Default::default()).unwrap().check().is_err());
    assert!(conn.get_geometry(12345).unwrap().reply().is_err());
    conn.get_input_focus().unwrap().reply().unwrap();

    // A big request: 1 MB of property.
    let big = vec![7u8; 1 << 20];
    conn.change_property8(PropMode::REPLACE, root, AtomEnum::CUT_BUFFER0, AtomEnum::STRING, &big).unwrap();
    let p = conn.get_property(false, root, AtomEnum::CUT_BUFFER0, AtomEnum::STRING, 0, 1 << 20).unwrap().reply().unwrap();
    assert_eq!(p.value.len(), 1 << 20);
}

#[test]
fn properties_between_byte_orders() {
    let (server, _) = server();
    let conn = connect(&server);
    let root = conn.setup().roots[0].root;
    conn.change_property32(PropMode::REPLACE, root, AtomEnum::CUT_BUFFER1, AtomEnum::CARDINAL, &[0x0102_0304]).unwrap();
    conn.sync().unwrap();

    // A big-endian client sees the same number, in its order.
    let mut s = TcpStream::connect(server.local_addr()).unwrap();
    s.write_all(&[b'B', 0, 0, 11, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
    let mut head = [0u8; 8];
    s.read_exact(&mut head).unwrap();
    assert_eq!((head[0], u16::from_be_bytes([head[2], head[3]])), (1, 11));
    let mut rest = vec![0u8; u16::from_be_bytes([head[6], head[7]]) as usize * 4];
    s.read_exact(&mut rest).unwrap();
    // The screen's root window, after the vendor and the 7 formats.
    let vendor = u16::from_be_bytes([rest[16], rest[17]]) as usize;
    let screen = 32 + vendor.div_ceil(4) * 4 + 7 * 8;
    assert_eq!(u32::from_be_bytes(rest[screen..][..4].try_into().unwrap()), root);
    assert_eq!(u16::from_be_bytes(rest[screen + 20..][..2].try_into().unwrap()), 800);

    // GetProperty(root, CUT_BUFFER1, any, 0, 1).
    let mut req = vec![20, 0, 0, 6];
    for v in [root, 10, 0, 0, 1] {
        req.extend_from_slice(&v.to_be_bytes());
    }
    // InternAtom("WM_NAME", only if exists).
    req.extend_from_slice(&[16, 1, 0, 4, 0, 7, 0, 0]);
    req.extend_from_slice(b"WM_NAME\0");
    s.write_all(&req).unwrap();
    let mut reply = [0u8; 36];
    s.read_exact(&mut reply).unwrap();
    assert_eq!((reply[0], reply[1], u16::from_be_bytes([reply[2], reply[3]])), (1, 32, 1));
    assert_eq!(u32::from_be_bytes(reply[8..12].try_into().unwrap()), 6); // CARDINAL
    assert_eq!(&reply[32..], &[1, 2, 3, 4]);
    let mut reply = [0u8; 32];
    s.read_exact(&mut reply).unwrap();
    assert_eq!(u16::from_be_bytes([reply[2], reply[3]]), 2);
    assert_eq!(u32::from_be_bytes(reply[8..12].try_into().unwrap()), 39);
}

#[test]
fn refuses_other_protocol_versions() {
    let (server, _) = server();
    let mut s = TcpStream::connect(server.local_addr()).unwrap();
    s.write_all(&[b'l', 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
    let mut head = [0u8; 8];
    s.read_exact(&mut head).unwrap();
    assert_eq!(head[0], 0);
    let mut reason = vec![0u8; head[1] as usize];
    s.read_exact(&mut reason).unwrap();
    assert_eq!(reason, b"protocol version mismatch");
}

#[test]
fn graphics_contexts_and_ids() {
    use x11rb::protocol::xc_misc::ConnectionExt as _;
    use x11rb::protocol::xproto::{ChangeGCAux, CreateGCAux};
    let (server, _) = server();
    let conn = connect(&server);
    let root = conn.setup().roots[0].root;
    let base = conn.setup().resource_id_base;
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, root, &CreateGCAux::new().foreground(0xff0000)).unwrap().check().unwrap();
    conn.change_gc(gc, &ChangeGCAux::new().line_width(2)).unwrap().check().unwrap();
    // The same id twice, or one outside the client's range.
    assert!(conn.create_gc(gc, root, &CreateGCAux::new()).unwrap().check().is_err());
    assert!(conn.create_gc(base + (1 << 21), root, &CreateGCAux::new()).unwrap().check().is_err());
    let range = conn.xc_misc_get_xid_range().unwrap().reply().unwrap();
    assert!(range.count > 0 && !(range.start_id..range.start_id + range.count).contains(&gc));
    conn.free_gc(gc).unwrap().check().unwrap();
    assert!(conn.free_gc(gc).unwrap().check().is_err());
}

/// The press that activates a passive grab reaches the grabbing client even
/// when its grab's mask leaves presses out, as in X.org (HP VUE's window
/// manager grabs its front panel's buttons that way).
#[test]
fn passive_grabs_get_their_press() {
    use vintrdp_x11::InputEvent;
    use x11rb::protocol::xproto::{ButtonIndex, CreateWindowAux, EventMask, GrabMode, ModMask, WindowClass};
    use x11rb::protocol::Event;
    let (server, _) = server();
    let wm = connect(&server);
    let root = wm.setup().roots[0].root;
    let w = wm.generate_id().unwrap();
    wm.create_window(24, w, root, 10, 10, 100, 100, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap();
    wm.map_window(w).unwrap();
    wm.grab_button(false, w, EventMask::BUTTON_RELEASE, GrabMode::SYNC, GrabMode::ASYNC, x11rb::NONE, x11rb::NONE, ButtonIndex::M1, ModMask::from(0u16)).unwrap();
    wm.get_input_focus().unwrap().reply().unwrap();
    server.input(InputEvent::Motion { x: 50, y: 50 });
    server.input(InputEvent::Button { button: 1, down: true });
    let press = loop {
        match wm.wait_for_event().unwrap() {
            Event::ButtonPress(e) => break e,
            _ => continue,
        }
    };
    assert_eq!((press.event, press.detail), (w, 1));
    // Frozen by the grab until the client lets it go; then the release.
    server.input(InputEvent::Button { button: 1, down: false });
    wm.allow_events(x11rb::protocol::xproto::Allow::ASYNC_POINTER, x11rb::CURRENT_TIME).unwrap();
    wm.flush().unwrap();
    let release = loop {
        match wm.wait_for_event().unwrap() {
            Event::ButtonRelease(e) => break e,
            _ => continue,
        }
    };
    assert_eq!(release.event, w);
}

/// Save-unders: what a menu covered comes back when it goes, drawing
/// beneath it included, without Expose; a window that moved beneath it
/// gets Expose as usual.
#[test]
fn save_unders() {
    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::*;
    let (server, _) = server();
    let c = connect(&server);
    let root = c.setup().roots[0].root;
    assert!(c.setup().roots[0].save_unders);
    let new = |x, y, w, h, bg: u32, save: bool| {
        let id = c.generate_id().unwrap();
        c.create_window(
            24,
            id,
            root,
            x,
            y,
            w,
            h,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().background_pixel(bg).event_mask(EventMask::EXPOSURE).override_redirect(1).save_under(save as u32),
        )
        .unwrap();
        c.map_window(id).unwrap();
        id
    };
    let exposes = |c: &RustConnection| {
        c.get_input_focus().unwrap().reply().unwrap();
        let mut v = Vec::new();
        while let Some(ev) = c.poll_for_event().unwrap() {
            if let Event::Expose(e) = ev {
                v.push((e.window, e.x, e.y, e.width, e.height));
            }
        }
        v
    };
    let (a, b) = (new(10, 10, 100, 100, 0x112233, false), new(200, 10, 100, 100, 0x445566, false));
    exposes(&c);
    let gc = c.generate_id().unwrap();
    c.create_gc(gc, a, &CreateGCAux::new().foreground(0xff0000)).unwrap();
    let menu = new(50, 20, 200, 50, 0xcccccc, true);
    assert_eq!(exposes(&c), [(menu, 0, 0, 200, 50)]);
    // Drawn beneath the menu: kept.
    c.poly_fill_rectangle(a, gc, &[Rectangle { x: 0, y: 0, width: 100, height: 30 }]).unwrap();
    c.unmap_window(menu).unwrap();
    assert_eq!(exposes(&c), []);
    let px = |w, x, y| {
        let i = c.get_image(ImageFormat::Z_PIXMAP, w, x, y, 1, 1, !0).unwrap().reply().unwrap();
        u32::from_le_bytes(i.data[..4].try_into().unwrap()) & 0xff_ffff
    };
    assert_eq!((px(a, 60, 15), px(a, 60, 40), px(b, 20, 30)), (0xff0000, 0x112233, 0x445566));
    // Up again; b moves beneath it: b's part comes back by Expose.
    c.map_window(menu).unwrap();
    exposes(&c);
    c.configure_window(b, &ConfigureWindowAux::new().x(210)).unwrap();
    exposes(&c);
    c.unmap_window(menu).unwrap();
    assert_eq!(exposes(&c), [(b, 0, 10, 40, 50)]);
}

#[derive(Clone, Default)]
struct Clip(Arc<Mutex<Vec<String>>>);

impl Host for Clip {
    fn resize(&self, _: u16, _: u16) {}
    fn draw(&self, _: usize, _: usize, _: usize, _: usize, _: &[u32]) {}
    fn updated(&self) {}
    fn clipboard(&self, text: &str) {
        self.0.lock().unwrap().push(text.to_string());
    }
}

/// The local clipboard and CLIPBOARD, both ways: the server as an owner
/// (TARGETS, the text types, INCR for big text), and as a requestor of a
/// client's selection, INCR included.
#[test]
fn clipboard_both_ways() {
    use x11rb::protocol::xproto::*;
    use x11rb::protocol::Event;
    let clip = Clip::default();
    let settings = Settings {
        width: 200,
        height: 100,
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
    let server = vintrdp_x11::start(settings, Box::new(clip.clone())).unwrap();
    let c = connect(&server);
    let root = c.setup().roots[0].root;
    let atom = |n: &[u8]| c.intern_atom(false, n).unwrap().reply().unwrap().atom;
    let (clipboard, targets, utf8, ctext, incr, prop) = (atom(b"CLIPBOARD"), atom(b"TARGETS"), atom(b"UTF8_STRING"), atom(b"COMPOUND_TEXT"), atom(b"INCR"), atom(b"MINE"));
    let w = c.generate_id().unwrap();
    c.create_window(0, w, root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE)).unwrap();
    let wait = |c: &RustConnection, f: &mut dyn FnMut(&Event) -> bool| loop {
        let ev = c.wait_for_event().unwrap();
        if f(&ev) {
            return ev;
        }
    };

    // Local text: the server owns CLIPBOARD.
    server.set_clipboard("caf\u{e9} \u{2713}".into());
    let owner = loop {
        let o = c.get_selection_owner(clipboard).unwrap().reply().unwrap().owner;
        if o != 0 {
            break o;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let get = |target: u32| -> (u32, Vec<u8>) {
        c.convert_selection(w, clipboard, target, prop, x11rb::CURRENT_TIME).unwrap();
        c.flush().unwrap();
        let ev = wait(&c, &mut |e| matches!(e, Event::SelectionNotify(_)));
        let Event::SelectionNotify(n) = ev else { unreachable!() };
        assert_eq!(n.property, prop);
        let p = c.get_property(true, w, prop, AtomEnum::ANY, 0, 1 << 20).unwrap().reply().unwrap();
        (p.type_, p.value)
    };
    assert_ne!(owner, w);
    let (t, list) = get(targets);
    assert_eq!(t, u32::from(AtomEnum::ATOM));
    let list: Vec<u32> = list.chunks(4).map(|b| u32::from_ne_bytes(b.try_into().unwrap())).collect();
    assert!(list.contains(&utf8) && list.contains(&ctext) && list.contains(&u32::from(AtomEnum::STRING)));
    assert_eq!(get(utf8), (utf8, "caf\u{e9} \u{2713}".as_bytes().to_vec()));
    assert_eq!(get(AtomEnum::STRING.into()), (AtomEnum::STRING.into(), b"caf\xe9 ?".to_vec()));
    assert_eq!(get(ctext).1, b"caf\xe9 ?");

    // MULTIPLE: two targets at once, one the server hasn't (set to None).
    let (multiple, pair, p1, p2) = (atom(b"MULTIPLE"), atom(b"ATOM_PAIR"), atom(b"P1"), atom(b"P2"));
    assert!(list.contains(&multiple));
    let pairs = [utf8, p1, atom(b"NO_SUCH_TARGET"), p2];
    c.change_property32(PropMode::REPLACE, w, prop, pair, &pairs).unwrap();
    c.convert_selection(w, clipboard, multiple, prop, x11rb::CURRENT_TIME).unwrap();
    c.flush().unwrap();
    let Event::SelectionNotify(n) = wait(&c, &mut |e| matches!(e, Event::SelectionNotify(_))) else { unreachable!() };
    assert_eq!(n.property, prop);
    let p = c.get_property(true, w, prop, AtomEnum::ANY, 0, 1 << 20).unwrap().reply().unwrap();
    let back: Vec<u32> = p.value32().unwrap().collect();
    assert_eq!((p.type_, back), (pair, vec![utf8, p1, pairs[2], 0]));
    let v = c.get_property(true, w, p1, AtomEnum::ANY, 0, 1 << 20).unwrap().reply().unwrap();
    assert_eq!((v.type_, v.value), (utf8, "caf\u{e9} \u{2713}".as_bytes().to_vec()));

    // Big: INCR, in pieces, each after the last is deleted.
    let big: String = (0..300_000).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    server.set_clipboard(big.clone());
    std::thread::sleep(std::time::Duration::from_millis(50));
    let (t, size) = get(utf8);
    assert_eq!((t, u32::from_ne_bytes(size[..4].try_into().unwrap())), (incr, 300_000));
    let mut got = Vec::new();
    loop {
        wait(&c, &mut |e| matches!(e, Event::PropertyNotify(p) if p.state == Property::NEW_VALUE && p.atom == prop));
        let p = c.get_property(true, w, prop, AtomEnum::ANY, 0, 1 << 20).unwrap().reply().unwrap();
        if p.value.is_empty() {
            break;
        }
        got.extend_from_slice(&p.value);
    }
    assert_eq!(got, big.as_bytes());

    // A client's text: the server asks for TARGETS, then UTF8_STRING.
    let answer = |c: &RustConnection, text: &[u8], use_incr: bool| {
        loop {
            let Event::SelectionRequest(r) = wait(c, &mut |e| matches!(e, Event::SelectionRequest(_))) else { unreachable!() };
            if r.target == targets {
                c.change_property32(PropMode::REPLACE, r.requestor, r.property, AtomEnum::ATOM, &[targets, utf8]).unwrap();
            } else if use_incr {
                c.change_window_attributes(r.requestor, &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE)).unwrap();
                c.change_property32(PropMode::REPLACE, r.requestor, r.property, incr, &[text.len() as u32]).unwrap();
            } else {
                c.change_property8(PropMode::REPLACE, r.requestor, r.property, r.target, text).unwrap();
            }
            let n = SelectionNotifyEvent { response_type: SELECTION_NOTIFY_EVENT, sequence: 0, time: r.time, requestor: r.requestor, selection: r.selection, target: r.target, property: r.property };
            c.send_event(false, r.requestor, EventMask::NO_EVENT, n).unwrap();
            c.flush().unwrap();
            if r.target != targets {
                if use_incr {
                    for piece in text.chunks(1000).chain([&[][..]]) {
                        wait(c, &mut |e| matches!(e, Event::PropertyNotify(p) if p.state == Property::DELETE && p.window == r.requestor));
                        c.change_property8(PropMode::REPLACE, r.requestor, r.property, r.target, piece).unwrap();
                        c.flush().unwrap();
                    }
                }
                return;
            }
        }
    };
    let latest = |n: usize| {
        for _ in 0..200 {
            if let Some(t) = clip.0.lock().unwrap().get(n) {
                return t.clone();
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("no clipboard text");
    };
    c.set_selection_owner(w, clipboard, x11rb::CURRENT_TIME).unwrap();
    c.flush().unwrap();
    answer(&c, "na\u{ef}ve".as_bytes(), false);
    assert_eq!(latest(0), "na\u{ef}ve");
    let long: Vec<u8> = (0..5000).map(|i| b'0' + (i % 10) as u8).collect();
    c.set_selection_owner(w, clipboard, x11rb::CURRENT_TIME).unwrap();
    c.flush().unwrap();
    answer(&c, &long, true);
    assert_eq!(latest(1).as_bytes(), &long[..]);
}

#[derive(Clone, Default)]
struct Pixels(Arc<Mutex<Vec<u32>>>);

impl Host for Pixels {
    fn resize(&self, w: u16, h: u16) {
        *self.0.lock().unwrap() = vec![0; w as usize * h as usize];
    }
    fn draw(&self, x: usize, y: usize, w: usize, h: usize, px: &[u32]) {
        let mut s = self.0.lock().unwrap();
        for r in 0..h {
            s[(y + r) * 100 + x..][..w].copy_from_slice(&px[r * w..][..w]);
        }
    }
    fn updated(&self) {}
}

/// An 8-bit PseudoColor screen is shown through the installed colormap:
/// storing a cell recolours what's drawn with it, and installing another
/// map shows everything through that one.
#[test]
fn pseudocolor_through_the_installed_map() {
    use x11rb::protocol::xproto::*;
    let px = Pixels::default();
    let settings = Settings {
        width: 100,
        height: 100,
        listen: "127.0.0.1:0".parse().unwrap(),
        unix_socket: false,
        allow: Vec::new(),
        keymap: Keymap::new(|_, _| None),
        xdmcp: None,
        cookie: None,
        font_path: Vec::new(),
        primary: false,
        visual: vintrdp_x11::RootVisual::PseudoColor8,
        verbose: false,
    };
    let server = vintrdp_x11::start(settings, Box::new(px.clone())).unwrap();
    let c = connect(&server);
    let s = &c.setup().roots[0];
    assert_eq!((s.root_depth, s.black_pixel, s.white_pixel), (8, 0, 1));
    let (root, def, vis) = (s.root, s.default_colormap, s.root_visual);
    let cells = c.alloc_color_cells(false, def, 1, 0).unwrap().reply().unwrap().pixels;
    let cell = cells[0];
    let store = |m, r, g, b| c.store_colors(m, &[Coloritem { pixel: cell, red: r, green: g, blue: b, flags: ColorFlag::RED | ColorFlag::GREEN | ColorFlag::BLUE }]).unwrap();
    store(def, 0xffff, 0, 0);
    let w = c.generate_id().unwrap();
    c.create_window(0, w, root, 10, 10, 20, 20, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().background_pixel(cell).override_redirect(1)).unwrap();
    c.map_window(w).unwrap();
    let at = |want: u32| {
        c.get_input_focus().unwrap().reply().unwrap();
        for _ in 0..100 {
            if px.0.lock().unwrap()[15 * 100 + 15] == want {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("{:06x}, not {want:06x}", px.0.lock().unwrap()[15 * 100 + 15]);
    };
    at(0xff0000);
    store(def, 0, 0xffff, 0);
    at(0x00ff00);
    // A private map, installed: the window's cell is whatever it says there.
    let m = c.generate_id().unwrap();
    c.create_colormap(ColormapAlloc::ALL, m, root, vis).unwrap();
    store(m, 0, 0, 0xffff);
    c.install_colormap(m).unwrap();
    at(0x0000ff);
    c.uninstall_colormap(m).unwrap();
    at(0x00ff00);
}

/// A window manager's click-to-focus: a button grab, both devices Sync,
/// then ReplayPointer. The grab ends, and with it the keyboard's freeze:
/// keys reach the focus window again (dtwm on AIX left the keyboard dead).
#[test]
fn replay_pointer_lets_the_keyboard_go() {
    use vintrdp_x11::InputEvent;
    use x11rb::protocol::xproto::*;
    use x11rb::protocol::Event;
    let (server, _) = server();
    let c = connect(&server);
    let root = c.setup().roots[0].root;
    let w = c.generate_id().unwrap();
    c.create_window(24, w, root, 0, 0, 100, 100, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::BUTTON_PRESS).override_redirect(1)).unwrap();
    c.map_window(w).unwrap();
    c.set_input_focus(InputFocus::POINTER_ROOT, w, x11rb::CURRENT_TIME).unwrap();
    c.get_input_focus().unwrap().reply().unwrap();
    let wm = connect(&server);
    wm.grab_button(false, w, EventMask::BUTTON_PRESS, GrabMode::SYNC, GrabMode::SYNC, x11rb::NONE, x11rb::NONE, ButtonIndex::M1, ModMask::ANY).unwrap();
    wm.get_input_focus().unwrap().reply().unwrap();
    server.input(InputEvent::Motion { x: 50, y: 50 });
    server.input(InputEvent::Button { button: 1, down: true });
    let p = loop {
        if let Event::ButtonPress(p) = wm.wait_for_event().unwrap() {
            break p;
        }
    };
    wm.allow_events(Allow::REPLAY_POINTER, p.time).unwrap();
    wm.get_input_focus().unwrap().reply().unwrap();
    server.input(InputEvent::Button { button: 1, down: false });
    server.input(InputEvent::Key { code: 38, down: true });
    loop {
        match c.wait_for_event().unwrap() {
            Event::KeyPress(k) => {
                assert_eq!((k.detail, k.event), (38, w));
                break;
            }
            _ => continue,
        }
    }
}

/// Characters with no key get a spare keycode (MappingNotify first), ones
/// on a key's Shift column get Shift; the lock keys follow the local ones.
#[test]
fn typed_characters_and_locks() {
    use vintrdp_x11::InputEvent;
    use x11rb::protocol::xproto::{CreateWindowAux, EventMask, InputFocus, WindowClass};
    use x11rb::protocol::Event;
    let settings = Settings {
        width: 800,
        height: 600,
        listen: "127.0.0.1:0".parse().unwrap(),
        unix_socket: false,
        allow: Vec::new(),
        // a/A, Caps Lock, Shift_L.
        keymap: Keymap::new(|sc, ext| match (sc, ext) {
            (0x1e, false) => Some([0x61, 0x41]),
            (0x3a, false) => Some([0xffe5, 0]),
            (0x2a, false) => Some([0xffe1, 0]),
            _ => None,
        }),
        xdmcp: None,
        cookie: None,
        font_path: Vec::new(),
        primary: false,
        visual: Default::default(),
        verbose: false,
    };
    let server = vintrdp_x11::start(settings, Box::new(Screen::default())).unwrap();
    let c = connect(&server);
    let root = c.setup().roots[0].root;
    let w = c.generate_id().unwrap();
    let aux = CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE);
    c.create_window(24, w, root, 0, 0, 100, 100, 0, WindowClass::INPUT_OUTPUT, 0, &aux).unwrap();
    c.map_window(w).unwrap();
    c.set_input_focus(InputFocus::NONE, w, x11rb::CURRENT_TIME).unwrap();
    c.get_input_focus().unwrap().reply().unwrap();
    let mut next = || loop {
        match c.wait_for_event().unwrap() {
            Event::KeyPress(e) => break (true, e.detail, u16::from(e.state)),
            Event::KeyRelease(e) => break (false, e.detail, u16::from(e.state)),
            Event::MappingNotify(e) => break (false, e.first_keycode, 0xffff),
            _ => continue,
        }
    };
    server.input(InputEvent::Char { keysym: 0x41 });
    assert_eq!([next(), next(), next(), next()], [(true, 50, 0), (true, 38, 1), (false, 38, 1), (false, 50, 1)]);
    server.input(InputEvent::Char { keysym: 0x61 });
    assert_eq!([next(), next()], [(true, 38, 0), (false, 38, 0)]);
    // é: a spare keycode, the highest free one, mapped to it first.
    server.input(InputEvent::Char { keysym: 0xe9 });
    assert_eq!([next(), next(), next()], [(false, 255, 0xffff), (true, 255, 0), (false, 255, 0)]);
    drop(next);
    let map = c.get_keyboard_mapping(255, 1).unwrap().reply().unwrap();
    assert_eq!(map.keysyms[..2], [0xe9, 0xe9]);
    // Typed again: the same key, no new mapping.
    server.input(InputEvent::Char { keysym: 0xe9 });
    let mut next = || loop {
        match c.wait_for_event().unwrap() {
            Event::KeyPress(e) => break (true, e.detail, u16::from(e.state)),
            Event::KeyRelease(e) => break (false, e.detail, u16::from(e.state)),
            Event::MappingNotify(e) => break (false, e.first_keycode, 0xffff),
            _ => continue,
        }
    };
    assert_eq!([next(), next()], [(true, 255, 0), (false, 255, 0)]);
    // Caps Lock on, then (already on) nothing, then off.
    server.input(InputEvent::Locks { caps: true, num: false });
    assert_eq!([next(), next()], [(true, 66, 0), (false, 66, 2)]);
    server.input(InputEvent::Locks { caps: true, num: false });
    server.input(InputEvent::Locks { caps: false, num: false });
    assert_eq!([next(), next()], [(true, 66, 2), (false, 66, 0)]);
}
