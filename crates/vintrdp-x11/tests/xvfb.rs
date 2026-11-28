//! The same drawing on Xvfb and on this server, read back with GetImage and
//! compared pixel for pixel: lines, arcs, fills, GC fill styles and clips,
//! CopyArea and CopyPlane, images, text, and windows (clipping and
//! exposures). Skipped without Xvfb (`XVFB=path`, or the usual places),
//! unless VINTRDP_XVFB=required, as in CI.

use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use vintrdp_x11::keymap::Keymap;
use vintrdp_x11::{Host, Server, Settings};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::{DefaultStream, RustConnection};
use x11rb::wrapper::ConnectionExt as _;

struct Nothing;
impl Host for Nothing {
    fn resize(&self, _: u16, _: u16) {}
    fn draw(&self, _: usize, _: usize, _: usize, _: usize, _: &[u32]) {}
    fn updated(&self) {}
}

fn ours() -> (Server, RustConnection) {
    ours_with(Default::default())
}

fn ours_with(visual: vintrdp_x11::RootVisual) -> (Server, RustConnection) {
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
        visual,
        verbose: false,
    };
    let server = vintrdp_x11::start(settings, Box::new(Nothing)).unwrap();
    let stream = DefaultStream::from_tcp_stream(TcpStream::connect(server.local_addr()).unwrap()).unwrap().0;
    let conn = RustConnection::connect_to_stream(stream, 0).unwrap();
    (server, conn)
}

struct Xvfb(Child);

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn xvfb(n: u32) -> Option<(Xvfb, RustConnection, u32)> {
    xvfb_at(n, 24)
}

fn xvfb_at(n: u32, depth: u8) -> Option<(Xvfb, RustConnection, u32)> {
    let path = std::env::var("XVFB").ok().or_else(|| ["/usr/bin/Xvfb", "/opt/X11/bin/Xvfb", "/usr/local/bin/Xvfb"].iter().find(|p| std::path::Path::new(p).exists()).map(|p| p.to_string()));
    let Some(path) = path else {
        assert!(std::env::var("VINTRDP_XVFB").as_deref() != Ok("required"), "no Xvfb");
        eprintln!("no Xvfb: skipped");
        return None;
    };
    let display = 700 + std::process::id() % 100 * 10 + n;
    let child = Command::new(&path)
        .arg(format!(":{display}"))
        .args(["-screen", "0", &format!("320x240x{depth}"), "-listen", "tcp", "-nolisten", "unix", "-ac", "-extension", "GLX"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let x = Xvfb(child);
    for _ in 0..100 {
        if let Ok((conn, _)) = x11rb::connect(Some(&format!("127.0.0.1:{display}"))) {
            return Some((x, conn, display));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("Xvfb didn't start");
}

const W: u16 = 200;
const H: u16 = 150;

/// What both servers draw, on a pixmap or a window of W x H.
fn scene(c: &RustConnection, d: Drawable, root: Window) {
    scene_at(c, d, root, c.setup().roots[0].root_depth)
}

/// The scene on a drawable of `depth`.
fn scene_at(c: &RustConnection, d: Drawable, root: Window, depth: u8) {
    let gc = c.generate_id().unwrap();
    c.create_gc(gc, d, &CreateGCAux::new().foreground(0x30_3030).graphics_exposures(0)).unwrap();
    c.poly_fill_rectangle(d, gc, &[Rectangle { x: 0, y: 0, width: W, height: H }]).unwrap();
    let set = |aux: &ChangeGCAux| c.change_gc(gc, aux).unwrap();
    let p = |x: i16, y: i16| Point { x, y };

    // Thin lines, all ways, with every function on the way.
    set(&ChangeGCAux::new().foreground(0xff_ffff));
    c.poly_line(CoordMode::ORIGIN, d, gc, &[p(5, 5), p(60, 20), p(30, 60), p(5, 5)]).unwrap();
    c.poly_segment(d, gc, &[Segment { x1: 70, y1: 5, x2: 70, y2: 40 }, Segment { x1: 75, y1: 40, x2: 110, y2: 8 }]).unwrap();
    set(&ChangeGCAux::new().function(GX::XOR).foreground(0x00_ff00));
    c.poly_line(CoordMode::PREVIOUS, d, gc, &[p(10, 70), p(40, 3), p(-20, 20), p(50, -10)]).unwrap();
    set(&ChangeGCAux::new().function(GX::COPY).foreground(0xff_8000).line_style(LineStyle::DOUBLE_DASH).background(0x0000ff));
    c.set_dashes(gc, 2, &[4, 2, 1, 2]).unwrap();
    c.poly_line(CoordMode::ORIGIN, d, gc, &[p(5, 90), p(100, 80), p(120, 100)]).unwrap();
    c.poly_point(CoordMode::ORIGIN, d, gc, &[p(150, 5), p(152, 5), p(154, 7)]).unwrap();
    c.poly_rectangle(d, gc, &[Rectangle { x: 130, y: 10, width: 30, height: 20 }]).unwrap();

    // Wide lines, caps and joins.
    set(&ChangeGCAux::new().line_style(LineStyle::SOLID).line_width(7).cap_style(CapStyle::ROUND).join_style(JoinStyle::ROUND).foreground(0x80_80ff));
    c.poly_line(CoordMode::ORIGIN, d, gc, &[p(120, 40), p(180, 50), p(150, 80)]).unwrap();
    set(&ChangeGCAux::new().line_width(5).cap_style(CapStyle::PROJECTING).join_style(JoinStyle::MITER).foreground(0xff_ff00).line_style(LineStyle::ON_OFF_DASH));
    c.poly_line(CoordMode::ORIGIN, d, gc, &[p(10, 110), p(60, 100), p(40, 140)]).unwrap();
    set(&ChangeGCAux::new().line_width(3).line_style(LineStyle::SOLID).join_style(JoinStyle::BEVEL));
    c.poly_rectangle(d, gc, &[Rectangle { x: 70, y: 110, width: 25, height: 25 }]).unwrap();

    // Arcs and fills.
    set(&ChangeGCAux::new().line_width(0).foreground(0xff_00ff));
    let arc = |x, y, w, h, a1, a2| Arc { x, y, width: w, height: h, angle1: a1, angle2: a2 };
    c.poly_arc(d, gc, &[arc(100, 100, 40, 30, 0, 360 * 64), arc(110, 105, 20, 20, 45 * 64, 200 * 64)]).unwrap();
    set(&ChangeGCAux::new().line_width(4).foreground(0x00_ffff));
    c.poly_arc(d, gc, &[arc(150, 95, 40, 40, 30 * 64, 250 * 64)]).unwrap();
    set(&ChangeGCAux::new().arc_mode(ArcMode::PIE_SLICE).foreground(0x80_ff80));
    c.poly_fill_arc(d, gc, &[arc(160, 110, 30, 30, 0, 270 * 64)]).unwrap();
    set(&ChangeGCAux::new().arc_mode(ArcMode::CHORD).foreground(0xff_8080));
    c.poly_fill_arc(d, gc, &[arc(5, 115, 30, 30, 20 * 64, 200 * 64)]).unwrap();
    set(&ChangeGCAux::new().fill_rule(FillRule::WINDING).foreground(0xc0_c000));
    c.fill_poly(d, gc, PolyShape::COMPLEX, CoordMode::ORIGIN, &[p(40, 20), p(55, 60), p(20, 35), p(60, 35), p(25, 60)]).unwrap();

    // Fill styles: a tile, a stipple, an opaque stipple; a clip.
    let tile = c.generate_id().unwrap();
    c.create_pixmap(depth, tile, root, 4, 3).unwrap();
    let tgc = c.generate_id().unwrap();
    c.create_gc(tgc, tile, &CreateGCAux::new().foreground(0x11_22ff)).unwrap();
    c.poly_fill_rectangle(tile, tgc, &[Rectangle { x: 0, y: 0, width: 4, height: 3 }]).unwrap();
    c.change_gc(tgc, &ChangeGCAux::new().foreground(0xee_dd11)).unwrap();
    c.poly_point(CoordMode::ORIGIN, tile, tgc, &[p(0, 0), p(1, 1), p(3, 2)]).unwrap();
    let st = c.generate_id().unwrap();
    c.create_pixmap(1, st, root, 5, 5).unwrap();
    let sgc = c.generate_id().unwrap();
    c.create_gc(sgc, st, &CreateGCAux::new().foreground(0)).unwrap();
    c.poly_fill_rectangle(st, sgc, &[Rectangle { x: 0, y: 0, width: 5, height: 5 }]).unwrap();
    c.change_gc(sgc, &ChangeGCAux::new().foreground(1)).unwrap();
    c.poly_line(CoordMode::ORIGIN, st, sgc, &[p(0, 0), p(4, 4)]).unwrap();
    c.poly_point(CoordMode::ORIGIN, st, sgc, &[p(4, 0)]).unwrap();
    set(&ChangeGCAux::new().fill_style(FillStyle::TILED).tile(tile).tile_stipple_x_origin(1).tile_stipple_y_origin(2).line_width(0));
    c.poly_fill_rectangle(d, gc, &[Rectangle { x: 100, y: 60, width: 30, height: 20 }]).unwrap();
    set(&ChangeGCAux::new().fill_style(FillStyle::STIPPLED).stipple(st).foreground(0xff_ffff));
    c.poly_fill_arc(d, gc, &[arc(60, 60, 35, 25, 0, 360 * 64)]).unwrap();
    set(&ChangeGCAux::new().fill_style(FillStyle::OPAQUE_STIPPLED).background(0x40_0000));
    c.set_clip_rectangles(ClipOrdering::UNSORTED, gc, 3, 4, &[Rectangle { x: 130, y: 55, width: 20, height: 10 }, Rectangle { x: 140, y: 60, width: 30, height: 15 }]).unwrap();
    c.poly_fill_rectangle(d, gc, &[Rectangle { x: 120, y: 50, width: 70, height: 40 }]).unwrap();
    set(&ChangeGCAux::new().fill_style(FillStyle::SOLID).clip_mask(x11rb::NONE));

    // Images: Z, XY and Bitmap formats; then GetImage, back as Z.
    let z: Vec<u8> = if depth == 8 { (0..8 * 6).map(|i: u32| (i * 5) as u8).collect() } else { (0..8 * 6).flat_map(|i: u32| (i * 0x050301).to_le_bytes()).collect() };
    c.put_image(ImageFormat::Z_PIXMAP, d, gc, 8, 6, 180, 5, 0, depth, &z).unwrap();
    // A 9x4 bitmap, 4 bytes a row, left pad 3.
    let bits: Vec<u8> = (0..4).flat_map(|r: u8| [0xa5u8.rotate_left(r as u32), 0x3c, 0x0f, 0]).collect();
    set(&ChangeGCAux::new().foreground(0xff_0000).background(0x00_ff00));
    c.put_image(ImageFormat::XY_BITMAP, d, gc, 9, 4, 180, 15, 3, 1, &bits).unwrap();
    // XY pixmap of depth 24: 24 planes of the same bitmap, all but a few blank.
    let mut xy = vec![0u8; depth as usize * 4 * 4];
    for plane in [0usize, 3, 7, 15, 23].into_iter().filter(|&p| p < depth as usize) {
        xy[plane * 16..plane * 16 + 16].copy_from_slice(&bits);
    }
    set(&ChangeGCAux::new().function(GX::OR));
    c.put_image(ImageFormat::XY_PIXMAP, d, gc, 9, 4, 180, 22, 3, depth, &xy).unwrap();
    set(&ChangeGCAux::new().function(GX::COPY));

    // CopyArea within the drawable (overlapping), and CopyPlane from a bitmap.
    c.copy_area(d, d, gc, 0, 0, 10, 100, 50, 40).unwrap();
    set(&ChangeGCAux::new().foreground(0xffff00).background(0x202080));
    c.copy_plane(st, d, gc, 0, 0, 185, 40, 5, 5, 1).unwrap();

    // Text in the fixed font, both kinds.
    let font = c.generate_id().unwrap();
    c.open_font(font, b"fixed").unwrap();
    set(&ChangeGCAux::new().font(font).foreground(0xffffff).background(0x000080));
    c.image_text8(d, gc, 5, 75, b"Image Text 8").unwrap();
    set(&ChangeGCAux::new().foreground(0xff_c000).function(GX::XOR));
    c.poly_text8(d, gc, 30, 95, &[5, 0, b'P', b'o', b'l', b'y', b'!', 3, 4, b'x', b'y', b'z']).unwrap();

    // The 75dpi and 100dpi families, as login screens use them; PolyText
    // switching fonts as it goes.
    let fonts: Vec<u32> = [
        &b"-adobe-helvetica-bold-r-normal--12-120-75-75-p-70-iso8859-1"[..],
        b"-adobe-times-medium-i-normal--14-140-75-75-p-73-iso8859-1",
        b"-b&h-lucida-medium-r-normal-sans-18-180-75-75-p-106-iso8859-1",
        b"-adobe-courier-bold-o-normal--17-120-100-100-m-100-iso8859-1",
        b"-adobe-new century schoolbook-medium-r-normal--24-240-75-75-p-137-iso8859-1",
    ]
    .iter()
    .map(|name| {
        let f = c.generate_id().unwrap();
        c.open_font(f, name).unwrap();
        f
    })
    .collect();
    set(&ChangeGCAux::new().function(GX::COPY).foreground(0x00_ff80).font(fonts[0]));
    c.image_text8(d, gc, 2, 112, b"Login: root").unwrap();
    let mut items = vec![255];
    items.extend_from_slice(&fonts[1].to_be_bytes());
    items.extend_from_slice(&[6, 2, b'T', b'i', b'm', b'e', b's', b' ', 255]);
    items.extend_from_slice(&fonts[2].to_be_bytes());
    items.extend_from_slice(&[5, 0, b'L', b'u', b'c', b'i', b'd']);
    c.poly_text8(d, gc, 2, 130, &items).unwrap();
    set(&ChangeGCAux::new().font(fonts[3]).foreground(0xff_80ff));
    c.poly_text8(d, gc, 100, 128, &[6, 0, b'C', b'o', b'u', b'r', b'i', b'e']).unwrap();
    set(&ChangeGCAux::new().font(fonts[4]).foreground(0xffffff));
    c.image_text8(d, gc, 60, 148, b"Welcome\xe9").unwrap();
    c.free_gc(gc).unwrap();
}

fn image(c: &RustConnection, d: Drawable) -> Vec<u32> {
    let img = c.get_image(ImageFormat::Z_PIXMAP, d, 0, 0, W, H, !0).unwrap().reply().unwrap();
    pixels(&img.data, img.depth)
}

/// A Z image's pixels, at depth 8 (a byte each; rows are already 4-byte
/// multiples here) or 24.
fn pixels(data: &[u8], depth: u8) -> Vec<u32> {
    if depth == 8 {
        return data.iter().map(|&b| b as u32).collect();
    }
    data.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0xff_ffff).collect()
}

/// Draw the scene on a pixmap and on a window, on one server.
fn run(c: &RustConnection) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    let root = c.setup().roots[0].root;
    let depth = c.setup().roots[0].root_depth;
    let pix = c.generate_id().unwrap();
    c.create_pixmap(depth, pix, root, W, H).unwrap();
    scene(c, pix, root);
    // A window with a child covering part of it: drawing is clipped by the child.
    let win = c.generate_id().unwrap();
    c.create_window(depth, win, root, 10, 10, W, H, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().background_pixel(0x102030).override_redirect(1)).unwrap();
    let child = c.generate_id().unwrap();
    c.create_window(depth, child, win, 120, 70, 50, 40, 3, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().background_pixel(0x804020).border_pixel(0x00ff00)).unwrap();
    c.map_subwindows(win).unwrap();
    c.map_window(win).unwrap();
    scene(c, win, root);
    // IncludeInferiors draws over the child too.
    let gc = c.generate_id().unwrap();
    c.create_gc(gc, win, &CreateGCAux::new().foreground(0xffffff).subwindow_mode(SubwindowMode::INCLUDE_INFERIORS).line_width(2)).unwrap();
    c.poly_line(CoordMode::ORIGIN, win, gc, &[Point { x: 100, y: 60 }, Point { x: 190, y: 120 }]).unwrap();
    // A sibling on top, then moved away: what it uncovers is the background.
    let lid = c.generate_id().unwrap();
    c.create_window(depth, lid, root, 30, 30, 60, 40, 1, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().background_pixel(0xc0c0c0).override_redirect(1)).unwrap();
    c.map_window(lid).unwrap();
    c.configure_window(lid, &ConfigureWindowAux::new().x(250).y(180)).unwrap();
    let screen = c.get_image(ImageFormat::Z_PIXMAP, root, 0, 0, 320, 240, !0).unwrap().reply().unwrap();
    let screen = pixels(&screen.data, depth);
    (image(c, pix), image(c, win), screen)
}

fn compare(name: &str, want: &[u32], got: &[u32], w: usize) {
    let bad: Vec<usize> = (0..want.len()).filter(|&i| want[i] != got[i]).collect();
    if bad.is_empty() {
        return;
    }
    // VINTRDP_XVFB_DUMP=dir saves both, as PPM, to look at.
    if let Some(dir) = std::env::var_os("VINTRDP_XVFB_DUMP") {
        for (who, px) in [("xvfb", want), ("ours", got)] {
            let mut f = format!("P6 {w} {} 255\n", px.len() / w).into_bytes();
            f.extend(px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]));
            std::fs::write(std::path::Path::new(&dir).join(format!("{name}-{who}.ppm")), f).unwrap();
        }
    }
    let first = bad[0];
    panic!("{name}: {} of {} pixels differ from Xvfb's, the first at ({}, {}): {:06x} there, {:06x} here", bad.len(), want.len(), first % w, first / w, want[first], got[first]);
}

#[test]
fn drawing_matches_xvfb() {
    drawing_at(0, 24, Default::default());
}

/// The same at depth 8, PseudoColor: the pixels as the colormap numbers them.
#[test]
fn pseudocolor_drawing_matches_xvfb() {
    drawing_at(3, 8, vintrdp_x11::RootVisual::PseudoColor8);
}

fn drawing_at(n: u32, depth: u8, visual: vintrdp_x11::RootVisual) {
    let Some((_x, xc, _)) = xvfb_at(n, depth) else { return };
    let (_server, oc) = ours_with(visual);
    let want = run(&xc);
    let got = run(&oc);
    let tag = |s: &str| format!("{s}{depth}");
    compare(&tag("pixmap"), &want.0, &got.0, W as usize);
    compare(&tag("window"), &want.1, &got.1, W as usize);
    // The windows on the screen; the root's background pattern is ours, not Xvfb's.
    let outside = |s: &[u32]| -> Vec<u32> { s.iter().enumerate().map(|(i, &p)| if (10..10 + W as usize).contains(&(i % 320)) && (10..10 + H as usize).contains(&(i / 320)) || ((250..312).contains(&(i % 320)) && (180..222).contains(&(i / 320))) { p } else { 0 }).collect() };
    compare(&tag("screen"), &outside(&want.2), &outside(&got.2), 320);
}

/// What a client saw: its events and errors, ids and times made comparable
/// between the two servers.
struct Log {
    lines: Vec<String>,
}

impl Log {
    fn take(&mut self, who: &str, c: &RustConnection, bases: &[(u32, &str)], root: u32) {
        c.get_input_focus().unwrap().reply().unwrap();
        while let Some(ev) = c.poll_for_event().unwrap() {
            self.lines.push(format!("{who}: {}", normalize(&format!("{ev:?}"), bases, root)));
        }
    }

    fn reply(&mut self, what: &str, s: String, bases: &[(u32, &str)], root: u32) {
        self.lines.push(format!("{what}: {}", normalize(&s, bases, root)));
    }
}

/// Ids as client and offset, the root as R, times as T.
fn normalize(s: &str, bases: &[(u32, &str)], root: u32) -> String {
    const IDS: [&str; 14] = ["event", "window", "affected_window", "parent", "above_sibling", "requestor", "owner", "drawable", "child", "root", "sibling", "bad_value", "children", "focus"];
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(": ") {
        let key_start = rest[..i].rfind(|c: char| !(c.is_alphanumeric() || c == '_')).map_or(0, |k| k + 1);
        let key = &rest[key_start..i];
        out.push_str(&rest[..i + 2]);
        rest = &rest[i + 2..];
        let n = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if n == 0 {
            continue;
        }
        let v: u64 = rest[..n].parse().unwrap();
        let v32 = v as u32;
        if key == "time" || key == "server_time" {
            out.push('T');
        } else if IDS.contains(&key) && v32 == root {
            out.push('R');
        } else if let Some((b, who)) = IDS.contains(&key).then(|| bases.iter().find(|(b, _)| v32 & !0x1f_ffff == *b && v32 != 0)).flatten() {
            out.push_str(&format!("{who}{}", v32 - b));
        } else {
            out.push_str(&rest[..n]);
        }
        rest = &rest[n..];
    }
    out.push_str(rest);
    out
}

/// Windows, properties, selections and SendEvent between two clients, and
/// the errors and replies of the rest.
fn behaviour(c: &RustConnection, d: &RustConnection) -> Vec<String> {
    use x11rb::protocol::shape::{self, ConnectionExt as _};
    use x11rb::protocol::Event;
    let root = c.setup().roots[0].root;
    let bases = [(c.setup().resource_id_base, "A"), (d.setup().resource_id_base, "B")];
    let mut log = Log { lines: Vec::new() };
    let mask = EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE | EventMask::VISIBILITY_CHANGE;
    let w = c.generate_id().unwrap();
    c.create_window(24, w, root, 10, 10, 100, 80, 2, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().background_pixel(0x123456).event_mask(mask).override_redirect(1)).unwrap();
    let (c1, c2) = (c.generate_id().unwrap(), c.generate_id().unwrap());
    c.create_window(24, c1, w, 5, 5, 30, 20, 1, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().event_mask(mask)).unwrap();
    c.create_window(24, c2, w, 20, 15, 40, 30, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().event_mask(mask).win_gravity(Gravity::SOUTH_EAST)).unwrap();
    c.map_subwindows(w).unwrap();
    c.map_window(w).unwrap();
    log.take("A map", c, &bases, root);

    c.change_property8(PropMode::REPLACE, w, AtomEnum::WM_NAME, AtomEnum::STRING, b"vintrdp").unwrap();
    let p = c.get_property(true, w, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 10).unwrap().reply().unwrap();
    log.reply("GetProperty", format!("{:?}", (p.type_, p.format, p.value)), &bases, root);
    c.delete_property(w, AtomEnum::WM_NAME.into()).unwrap();
    log.take("A props", c, &bases, root);

    c.configure_window(c2, &ConfigureWindowAux::new().x(2).y(3).width(50).stack_mode(StackMode::BELOW)).unwrap();
    c.configure_window(c2, &ConfigureWindowAux::new().sibling(c1).stack_mode(StackMode::ABOVE)).unwrap();
    // Resized: c2 moves by its gravity.
    c.configure_window(w, &ConfigureWindowAux::new().width(120).height(90)).unwrap();
    c.clear_area(true, w, 0, 0, 0, 0).unwrap();
    c.unmap_window(c1).unwrap();
    c.map_window(c1).unwrap();
    c.reparent_window(c2, root, 50, 60).unwrap();
    c.reparent_window(c2, w, 1, 1).unwrap();
    c.circulate_window(Circulate::RAISE_LOWEST, w).unwrap();
    log.take("A configure", c, &bases, root);

    let t = c.query_tree(w).unwrap().reply().unwrap();
    log.reply("QueryTree", format!("parent: {}, children: {:?}", t.parent, t.children), &bases, root);
    let g = c.get_geometry(c2).unwrap().reply().unwrap();
    log.reply("GetGeometry", format!("{:?}", (g.depth, g.x, g.y, g.width, g.height, g.border_width)), &bases, root);
    let tc = c.translate_coordinates(c2, root, 3, 4).unwrap().reply().unwrap();
    log.reply("Translate", format!("{:?}", (tc.dst_x, tc.dst_y)), &bases, root);
    let a = c.get_window_attributes(c1).unwrap().reply().unwrap();
    log.reply("Attributes", format!("{:?}", (a.class, a.bit_gravity, a.win_gravity, a.map_state, a.override_redirect, a.all_event_masks, a.your_event_mask)), &bases, root);

    // Selections between the two clients.
    let dw = d.generate_id().unwrap();
    d.create_window(0, dw, root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE)).unwrap();
    d.set_selection_owner(dw, AtomEnum::PRIMARY.into(), x11rb::CURRENT_TIME).unwrap();
    d.get_input_focus().unwrap().reply().unwrap();
    let o = c.get_selection_owner(AtomEnum::PRIMARY.into()).unwrap().reply().unwrap();
    log.reply("Owner", format!("owner: {}", o.owner), &bases, root);
    c.convert_selection(w, AtomEnum::PRIMARY.into(), AtomEnum::STRING.into(), AtomEnum::CUT_BUFFER0, x11rb::CURRENT_TIME).unwrap();
    c.get_input_focus().unwrap().reply().unwrap();
    log.take("B request", d, &bases, root);
    let note = SelectionNotifyEvent { response_type: SELECTION_NOTIFY_EVENT, sequence: 0, time: 0, requestor: w, selection: AtomEnum::PRIMARY.into(), target: AtomEnum::STRING.into(), property: AtomEnum::CUT_BUFFER0.into() };
    d.send_event(false, w, EventMask::NO_EVENT, note).unwrap();
    let cm = ClientMessageEvent::new(32, w, AtomEnum::WM_COMMAND, [1u32, 2, 3, 4, 5]);
    d.send_event(true, w, EventMask::STRUCTURE_NOTIFY, cm).unwrap();
    d.get_input_focus().unwrap().reply().unwrap();
    log.take("A sent", c, &bases, root);
    c.set_selection_owner(w, AtomEnum::PRIMARY.into(), x11rb::CURRENT_TIME).unwrap();
    c.convert_selection(w, AtomEnum::SECONDARY.into(), AtomEnum::STRING.into(), AtomEnum::CUT_BUFFER0, x11rb::CURRENT_TIME).unwrap();
    c.get_input_focus().unwrap().reply().unwrap();
    log.take("B clear", d, &bases, root);
    log.take("A none", c, &bases, root);

    // Grabs: status codes.
    let unmapped = c.generate_id().unwrap();
    c.create_window(24, unmapped, root, 0, 0, 5, 5, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap();
    let s = c.grab_pointer(false, unmapped, EventMask::BUTTON_PRESS, GrabMode::ASYNC, GrabMode::ASYNC, x11rb::NONE, x11rb::NONE, x11rb::CURRENT_TIME).unwrap().reply().unwrap();
    log.reply("GrabPointer unmapped", format!("{:?}", s.status), &bases, root);
    let s = c.grab_pointer(false, w, EventMask::BUTTON_PRESS, GrabMode::ASYNC, GrabMode::ASYNC, x11rb::NONE, x11rb::NONE, x11rb::CURRENT_TIME).unwrap().reply().unwrap();
    log.reply("GrabPointer", format!("{:?}", s.status), &bases, root);
    let s = d.grab_pointer(false, dw, EventMask::BUTTON_PRESS, GrabMode::ASYNC, GrabMode::ASYNC, x11rb::NONE, x11rb::NONE, x11rb::CURRENT_TIME).unwrap().reply().unwrap();
    log.reply("GrabPointer other", format!("{:?}", s.status), &bases, root);
    c.ungrab_pointer(x11rb::CURRENT_TIME).unwrap();
    let s = c.grab_keyboard(true, w, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC).unwrap().reply().unwrap();
    log.reply("GrabKeyboard", format!("{:?}", s.status), &bases, root);
    c.ungrab_keyboard(x11rb::CURRENT_TIME).unwrap();
    c.grab_button(false, w, EventMask::BUTTON_PRESS, GrabMode::ASYNC, GrabMode::ASYNC, x11rb::NONE, x11rb::NONE, ButtonIndex::M1, ModMask::ANY).unwrap();
    d.grab_button(false, w, EventMask::BUTTON_PRESS, GrabMode::ASYNC, GrabMode::ASYNC, x11rb::NONE, x11rb::NONE, ButtonIndex::M1, ModMask::SHIFT).unwrap();
    d.get_input_focus().unwrap().reply().unwrap();
    log.take("B grab clash", d, &bases, root);

    // Colours.
    let cmap = c.setup().roots[0].default_colormap;
    let n = c.alloc_named_color(cmap, b"LightSteelBlue").unwrap().reply().unwrap();
    log.reply("AllocNamedColor", format!("{:?}", (n.pixel, n.exact_red, n.exact_green, n.exact_blue, n.visual_red, n.visual_green, n.visual_blue)), &bases, root);
    let a = c.alloc_color(cmap, 0x1234, 0x8000, 0xffff).unwrap().reply().unwrap();
    log.reply("AllocColor", format!("{:?}", (a.pixel, a.red, a.green, a.blue)), &bases, root);
    let l = c.lookup_color(cmap, b"Navy Blue").unwrap().reply().unwrap();
    log.reply("LookupColor", format!("{:?}", (l.exact_red, l.exact_green, l.exact_blue, l.visual_red, l.visual_green, l.visual_blue)), &bases, root);
    let q = c.query_colors(cmap, &[0, 0xff_ffff, 0x80_4020]).unwrap().reply().unwrap();
    log.reply("QueryColors", format!("{:?}", q.colors), &bases, root);
    log.reply("AllocColorCells", format!("{:?}", c.alloc_color_cells(false, cmap, 1, 0).unwrap().reply().err().map(|e| format!("{e:?}"))), &bases, root);
    log.reply("BadName", format!("{:?}", c.lookup_color(cmap, b"no such colour").unwrap().reply().err().map(|e| format!("{e:?}"))), &bases, root);

    // The fixed font.
    let font = c.generate_id().unwrap();
    c.open_font(font, b"fixed").unwrap();
    let q = c.query_font(font).unwrap().reply().unwrap();
    log.reply("QueryFont", format!("{:?}", (q.min_bounds, q.max_bounds, q.min_char_or_byte2, q.max_char_or_byte2, q.default_char, q.font_ascent, q.font_descent, q.all_chars_exist, q.char_infos.len(), &q.char_infos[65])), &bases, root);
    let e = c.query_text_extents(font, &[Char2b { byte1: 0, byte2: b'H' }, Char2b { byte1: 0, byte2: b'i' }]).unwrap().reply().unwrap();
    log.reply("QueryTextExtents", format!("{:?}", (e.font_ascent, e.font_descent, e.overall_ascent, e.overall_descent, e.overall_width, e.overall_left, e.overall_right)), &bases, root);

    // The 75dpi set: XLFD patterns, aliases, a proportional font.
    let times = c.generate_id().unwrap();
    c.open_font(times, b"-ADOBE-Times-Bold-R-Normal--18-*-75-75-*-*-ISO8859-1").unwrap();
    let q = c.query_font(times).unwrap().reply().unwrap();
    log.reply("QueryFont times", format!("{:?}", (q.min_bounds, q.max_bounds, q.min_char_or_byte2, q.max_char_or_byte2, q.default_char, q.font_ascent, q.font_descent, q.all_chars_exist, q.char_infos.len(), &q.char_infos[87], q.properties.len())), &bases, root);
    let e = c.query_text_extents(times, &b"Wave".iter().map(|&b| Char2b { byte1: 0, byte2: b }).collect::<Vec<_>>()).unwrap().reply().unwrap();
    log.reply("QueryTextExtents times", format!("{:?}", (e.font_ascent, e.font_descent, e.overall_ascent, e.overall_descent, e.overall_width, e.overall_left, e.overall_right)), &bases, root);
    // Names alike (Xvfb may have a font directory twice in its path, and
    // scales bitmaps where vintrdp knowingly doesn't: sizes given exactly).
    let list = c.list_fonts(100, b"-adobe-helvetica-*-*-normal--12-120-75-75-*-*-iso8859-1").unwrap().reply().unwrap();
    let scaled = |n: &str| n.split('-').nth(12) == Some("0");
    let mut names: Vec<String> = list.names.iter().map(|n| String::from_utf8_lossy(&n.name).into_owned()).filter(|n| !scaled(n)).collect();
    names.sort();
    names.dedup();
    log.reply("ListFonts", format!("{names:?}"), &bases, root);
    let info = c.list_fonts_with_info(5, b"-b&h-lucida-bold-r-normal-sans-12-120-75-75-*-*-iso8859-1").unwrap();
    let mut info: Vec<_> = info.map(|r| r.unwrap()).map(|r| (String::from_utf8_lossy(&r.name).into_owned(), r.max_bounds, r.font_ascent, r.properties.len())).filter(|r| !scaled(&r.0)).collect();
    info.dedup();
    log.reply("ListFontsWithInfo", format!("{info:?}"), &bases, root);

    // SHAPE.
    c.shape_select_input(w, true).unwrap();
    c.shape_rectangles(shape::SO::SET, shape::SK::BOUNDING, ClipOrdering::UNSORTED, w, 0, 0, &[Rectangle { x: -2, y: -2, width: 60, height: 40 }, Rectangle { x: 30, y: 30, width: 50, height: 50 }]).unwrap();
    let ex = c.shape_query_extents(w).unwrap().reply().unwrap();
    log.reply("ShapeExtents", format!("{:?}", (ex.bounding_shaped, ex.clip_shaped, ex.bounding_shape_extents_x, ex.bounding_shape_extents_y, ex.bounding_shape_extents_width, ex.bounding_shape_extents_height, ex.clip_shape_extents_width)), &bases, root);
    let gr = c.shape_get_rectangles(w, shape::SK::BOUNDING).unwrap().reply().unwrap();
    log.reply("ShapeRectangles", format!("{:?}", gr.rectangles), &bases, root);
    log.take("A shape", c, &bases, root);

    // Errors.
    let bad = c.generate_id().unwrap();
    log.reply("zero size", format!("{:?}", c.create_window(24, bad, root, 0, 0, 0, 5, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap().check().err()), &bases, root);
    log.reply("sibling alone", format!("{:?}", c.configure_window(c1, &ConfigureWindowAux::new().sibling(c2)).unwrap().check().err()), &bases, root);
    log.reply("no window", format!("{:?}", c.map_window(bad).unwrap().check().err()), &bases, root);
    log.reply("bad atom", format!("{:?}", c.get_property(false, w, 99999u32, AtomEnum::ANY, 0, 1).unwrap().reply().err()), &bases, root);
    log.reply("input only drawing", format!("{:?}", {
        let gc = c.generate_id().unwrap();
        c.create_gc(gc, w, &CreateGCAux::new()).unwrap();
        c.poly_point(CoordMode::ORIGIN, dw, gc, &[Point { x: 0, y: 0 }]).unwrap().check().err()
    }), &bases, root);

    c.destroy_window(c1).unwrap();
    c.destroy_window(w).unwrap();
    log.take("A destroy", c, &bases, root);
    // Event codes alone, should a layout differ.
    let _ = Event::Unknown(Vec::new());
    log.lines
}

#[test]
fn behaviour_matches_xvfb() {
    let Some((_x, xc, display)) = xvfb(1) else { return };
    let (server, oc) = ours();
    // Second clients.
    let xd = {
        let stream = DefaultStream::from_tcp_stream(TcpStream::connect(("127.0.0.1", 6000 + display as u16)).unwrap()).unwrap().0;
        RustConnection::connect_to_stream(stream, 0).unwrap()
    };
    let od = {
        let stream = DefaultStream::from_tcp_stream(TcpStream::connect(server.local_addr()).unwrap()).unwrap().0;
        RustConnection::connect_to_stream(stream, 0).unwrap()
    };
    let want = behaviour(&xc, &xd);
    let got = behaviour(&oc, &od);
    if std::env::var_os("VINTRDP_XVFB_LOG").is_some() {
        eprintln!("Xvfb:\n{}\nours:\n{}", want.join("\n"), got.join("\n"));
    }
    let n = want.len().max(got.len());
    let mut diff = String::new();
    for i in 0..n {
        let (a, b) = (want.get(i).map_or("", |s| s.as_str()), got.get(i).map_or("", |s| s.as_str()));
        if a != b {
            diff.push_str(&format!("Xvfb: {a}\nours: {b}\n"));
        }
    }
    assert!(diff.is_empty(), "differences from Xvfb:\n{diff}");
}

/// Backing store (WhenMapped, as Xvfb's Composite does it): what's covered is
/// kept and drawing there isn't lost, so uncovering exposes nothing;
/// GetImage and CopyArea read what's covered; resizing exposes what's new;
/// unmapping loses it all. A window without it, alongside, for contrast.
fn backing(c: &RustConnection) -> (Vec<String>, Vec<Vec<u32>>) {
    let root = c.setup().roots[0].root;
    let bases = [(c.setup().resource_id_base, "A")];
    let mut log = Log { lines: Vec::new() };
    let mut images = Vec::new();
    let mask = EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY | EventMask::VISIBILITY_CHANGE;
    let new = |x, y, w, h, bw, bg: u32, bs: BackingStore, parent: u32| {
        let id = c.generate_id().unwrap();
        c.create_window(
            24,
            id,
            parent,
            x,
            y,
            w,
            h,
            bw,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().background_pixel(bg).border_pixel(0xffff00).event_mask(mask).override_redirect(1).backing_store(bs).bit_gravity(Gravity::NORTH_WEST),
        )
        .unwrap();
        id
    };
    let a = new(10, 10, 120, 90, 2, 0x204060, BackingStore::WHEN_MAPPED, root);
    let child = new(60, 40, 40, 30, 1, 0x806020, BackingStore::NOT_USEFUL, a);
    let plain = new(150, 10, 120, 90, 2, 0x206040, BackingStore::NOT_USEFUL, root);
    c.map_subwindows(a).unwrap();
    c.map_window(a).unwrap();
    c.map_window(plain).unwrap();
    log.take("map", c, &bases, root);
    let gc = c.generate_id().unwrap();
    c.create_gc(gc, a, &CreateGCAux::new().foreground(0xff8080).graphics_exposures(1)).unwrap();
    let both = |what: &str, log: &mut Log| {
        for w in [a, plain] {
            c.poly_fill_rectangle(w, gc, &[Rectangle { x: 5, y: 5, width: 100, height: 20 }]).unwrap();
            c.poly_line(CoordMode::ORIGIN, w, gc, &[Point { x: 0, y: 0 }, Point { x: 119, y: 89 }]).unwrap();
        }
        log.take(what, c, &bases, root);
    };
    both("draw", &mut log);

    // A lid over both, drawn under, lifted.
    let lid = new(40, 20, 200, 60, 1, 0xc0c0c0, BackingStore::NOT_USEFUL, root);
    c.map_window(lid).unwrap();
    log.take("lid", c, &bases, root);
    c.change_gc(gc, &ChangeGCAux::new().foreground(0x80ff80)).unwrap();
    both("draw under", &mut log);
    // CopyArea from under the lid: GraphicsExpose only from the plain one.
    for w in [a, plain] {
        c.copy_area(w, w, gc, 40, 15, 2, 50, 50, 30).unwrap();
    }
    log.take("copy under", c, &bases, root);
    // GetImage of what's covered.
    for w in [a, child] {
        let g = c.get_geometry(w).unwrap().reply().unwrap();
        let img = c.get_image(ImageFormat::Z_PIXMAP, w, 0, 0, g.width, g.height, !0).unwrap().reply().unwrap();
        images.push(img.data.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0xff_ffff).collect());
    }
    c.unmap_window(lid).unwrap();
    log.take("lift", c, &bases, root);
    // Moved, then resized: what's new is exposed.
    c.configure_window(a, &ConfigureWindowAux::new().x(20).y(30)).unwrap();
    log.take("move", c, &bases, root);
    c.configure_window(a, &ConfigureWindowAux::new().width(140).height(100)).unwrap();
    c.configure_window(plain, &ConfigureWindowAux::new().width(140).height(100)).unwrap();
    log.take("resize", c, &bases, root);
    // Raised over by the plain one, then lowered again.
    c.configure_window(plain, &ConfigureWindowAux::new().x(60).stack_mode(StackMode::ABOVE)).unwrap();
    log.take("cover", c, &bases, root);
    c.configure_window(plain, &ConfigureWindowAux::new().x(160).stack_mode(StackMode::BELOW)).unwrap();
    log.take("uncover", c, &bases, root);
    // Unmapped: all is lost.
    c.unmap_window(a).unwrap();
    c.map_window(a).unwrap();
    log.take("remap", c, &bases, root);
    // Backing store turned off and on while half covered.
    c.map_window(lid).unwrap();
    log.take("lid again", c, &bases, root);
    c.change_window_attributes(a, &ChangeWindowAttributesAux::new().backing_store(BackingStore::NOT_USEFUL)).unwrap();
    log.take("off", c, &bases, root);
    c.change_window_attributes(a, &ChangeWindowAttributesAux::new().backing_store(BackingStore::ALWAYS)).unwrap();
    log.take("on", c, &bases, root);
    c.unmap_window(lid).unwrap();
    log.take("lift again", c, &bases, root);
    let at = c.get_window_attributes(a).unwrap().reply().unwrap();
    log.reply("attributes", format!("{:?}", at.backing_store), &bases, root);
    let screen = c.get_image(ImageFormat::Z_PIXMAP, root, 0, 0, 320, 240, !0).unwrap().reply().unwrap();
    let screen: Vec<u32> = screen.data.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0xff_ffff).collect();
    // Only where the windows are (the root's pattern differs).
    let inside = |i: usize, x: usize, y: usize, w: usize, h: usize| (x..x + w).contains(&(i % 320)) && (y..y + h).contains(&(i / 320));
    images.push(screen.iter().enumerate().map(|(i, &p)| if inside(i, 20, 30, 144, 104) || inside(i, 160, 10, 144, 104) { p } else { 0 }).collect());
    (log.lines, images)
}

#[test]
fn backing_store_matches_xvfb() {
    let Some((_x, xc, _)) = xvfb(2) else { return };
    let (_server, oc) = ours();
    let (want, want_px) = backing(&xc);
    let (got, got_px) = backing(&oc);
    if std::env::var_os("VINTRDP_XVFB_LOG").is_some() {
        eprintln!("Xvfb:\n{}\nours:\n{}", want.join("\n"), got.join("\n"));
    }
    let mut diff = String::new();
    for i in 0..want.len().max(got.len()) {
        let (a, b) = (want.get(i).map_or("", |s| s.as_str()), got.get(i).map_or("", |s| s.as_str()));
        if a != b {
            diff.push_str(&format!("Xvfb: {a}\nours: {b}\n"));
        }
    }
    assert!(diff.is_empty(), "differences from Xvfb:\n{diff}");
    for (i, (w, g)) in want_px.iter().zip(&got_px).enumerate() {
        let width = if i == 2 {
            320
        } else if i == 0 {
            120
        } else {
            40
        };
        compare(&format!("backing{i}"), w, g, width);
    }
}

/// Colormaps on an 8-bit PseudoColor screen (and DirectColor at 24): the
/// visuals, allocation in the default map and private ones, writable
/// cells, errors, CopyColormapAndFree, installing with ColormapNotify.
fn colormaps(c: &RustConnection) -> Vec<String> {
    let s = &c.setup().roots[0];
    let (root, depth) = (s.root, s.root_depth);
    let bases = [(c.setup().resource_id_base, "A")];
    let mut log = Log { lines: Vec::new() };
    let r = |log: &mut Log, what: &str, v: String| log.reply(what, v, &bases, root);
    let visuals: Vec<_> = s.allowed_depths.iter().filter(|d| d.depth == depth).flat_map(|d| d.visuals.iter().map(move |v| (d.depth, v.visual_id, v.class, v.bits_per_rgb_value, v.colormap_entries, v.red_mask, v.green_mask, v.blue_mask))).collect();
    r(&mut log, "screen", format!("{:?}", (depth, s.root_visual, s.black_pixel, s.white_pixel, s.min_installed_maps, s.max_installed_maps)));
    r(&mut log, "visuals", format!("{visuals:?}"));
    let def = s.default_colormap;
    let e = |x: Result<String, x11rb::errors::ReplyError>| match x {
        Ok(s) => s,
        Err(x11rb::errors::ReplyError::X11Error(e)) => format!("error {:?} bad_value: {}", e.error_kind, e.bad_value),
        Err(e) => format!("{e:?}"),
    };
    let alloc = |m: u32, rgb: (u16, u16, u16)| e(c.alloc_color(m, rgb.0, rgb.1, rgb.2).unwrap().reply().map(|a| format!("{:?}", (a.pixel, a.red, a.green, a.blue))));
    let named = |m: u32, n: &[u8]| e(c.alloc_named_color(m, n).unwrap().reply().map(|a| format!("{:?}", (a.pixel, a.exact_red, a.visual_red, a.visual_green, a.visual_blue))));
    let query = |m: u32, p: &[u32]| e(c.query_colors(m, p).unwrap().reply().map(|q| format!("{:?}", q.colors.iter().map(|c| (c.red, c.green, c.blue)).collect::<Vec<_>>())));
    let cells = |m: u32, n: u16, planes: u16, contig: bool| e(c.alloc_color_cells(contig, m, n, planes).unwrap().reply().map(|a| format!("{:?}", (a.pixels, a.masks))));
    let planes = |m: u32, n: u16, rgb: (u16, u16, u16)| e(c.alloc_color_planes(false, m, n, rgb.0, rgb.1, rgb.2).unwrap().reply().map(|a| format!("{:?}", (a.pixels, a.red_mask, a.green_mask, a.blue_mask))));
    let check = |x: Result<(), x11rb::errors::ReplyError>| e(x.map(|_| "ok".to_string()));
    let item = |pixel: u32, r: u16, flags: ColorFlag| Coloritem { pixel, red: r, green: r / 2, blue: 0x1234, flags };

    r(&mut log, "alloc red", alloc(def, (0xffff, 0, 0)));
    r(&mut log, "alloc red again", alloc(def, (0xffff, 0, 0)));
    r(&mut log, "alloc grey", alloc(def, (0x8000, 0x8000, 0x8000)));
    r(&mut log, "named", named(def, b"LightSteelBlue"));
    r(&mut log, "cells", cells(def, 3, 2, true));
    r(&mut log, "cells noncontig", cells(def, 2, 1, false));
    r(&mut log, "planes", planes(def, 2, (1, 1, 1)));
    r(&mut log, "store", check(c.store_colors(def, &[item(80, 0xa000, ColorFlag::RED | ColorFlag::GREEN | ColorFlag::BLUE)]).unwrap().check().map_err(Into::into)));
    r(&mut log, "store named", check(c.store_named_color(ColorFlag::RED | ColorFlag::BLUE, def, 81, b"orange").unwrap().check().map_err(Into::into)));
    r(&mut log, "store read-only", check(c.store_colors(def, &[item(1, 0, ColorFlag::RED)]).unwrap().check().map_err(Into::into)));
    r(&mut log, "store free", check(c.store_colors(def, &[item(250, 0, ColorFlag::RED)]).unwrap().check().map_err(Into::into)));
    r(&mut log, "store too big", check(c.store_colors(def, &[item(1000, 0, ColorFlag::RED)]).unwrap().check().map_err(Into::into)));
    r(&mut log, "query", query(def, &[0, 1, 73, 74, 80, 81, 200]));
    r(&mut log, "free", check(c.free_colors(def, 0, &[80, 81]).unwrap().check().map_err(Into::into)));
    r(&mut log, "free again", check(c.free_colors(def, 0, &[80]).unwrap().check().map_err(Into::into)));
    r(&mut log, "free not ours", check(c.free_colors(def, 0, &[1]).unwrap().check().map_err(Into::into)));

    // A private map, of each visual of the root's depth.
    let mut maps = Vec::new();
    for v in s.allowed_depths.iter().filter(|d| d.depth == depth).flat_map(|d| d.visuals.iter()) {
        let m = c.generate_id().unwrap();
        let all = matches!(v.class, VisualClass::PSEUDO_COLOR | VisualClass::GRAY_SCALE | VisualClass::DIRECT_COLOR);
        r(&mut log, "create", check(c.create_colormap(ColormapAlloc::NONE, m, root, v.visual_id).unwrap().check().map_err(Into::into)));
        r(&mut log, "alloc white", alloc(m, (0xffff, 0xffff, 0xffff)));
        r(&mut log, "alloc blue", alloc(m, (0x1000, 0x2000, 0xf000)));
        r(&mut log, "cells", cells(m, 4, 0, false));
        r(&mut log, "query", query(m, &[0, 1, 2, 3, 7, 0x38, 0xc0]));
        if all {
            let m2 = c.generate_id().unwrap();
            r(&mut log, "alloc all", check(c.create_colormap(ColormapAlloc::ALL, m2, root, v.visual_id).unwrap().check().map_err(Into::into)));
            r(&mut log, "alloc in all", alloc(m2, (0, 0, 0)));
            r(&mut log, "store in all", check(c.store_colors(m2, &[item(5, 0x4000, ColorFlag::RED | ColorFlag::GREEN | ColorFlag::BLUE)]).unwrap().check().map_err(Into::into)));
            let m3 = c.generate_id().unwrap();
            r(&mut log, "copy", check(c.copy_colormap_and_free(m3, m).unwrap().check().map_err(Into::into)));
            r(&mut log, "copy query", query(m3, &[0, 1, 2, 3]));
            r(&mut log, "after copy", alloc(m, (0x1000, 0x2000, 0xf000)));
        }
        maps.push((m, v.visual_id));
    }

    // Windows: a visual of their own needs a colormap; installing one tells
    // the windows using it.
    let (m, vis) = maps[1];
    let w = c.generate_id().unwrap();
    r(&mut log, "no colormap", check(c.create_window(depth, w, root, 0, 0, 10, 10, 0, WindowClass::INPUT_OUTPUT, vis, &CreateWindowAux::new()).unwrap().check().map_err(Into::into)));
    r(&mut log, "wrong colormap", check(c.create_window(depth, w, root, 0, 0, 10, 10, 0, WindowClass::INPUT_OUTPUT, vis, &CreateWindowAux::new().colormap(def)).unwrap().check().map_err(Into::into)));
    r(&mut log, "window", check(c.create_window(depth, w, root, 0, 0, 10, 10, 0, WindowClass::INPUT_OUTPUT, vis, &CreateWindowAux::new().colormap(m).event_mask(EventMask::COLOR_MAP_CHANGE)).unwrap().check().map_err(Into::into)));
    let w2 = c.generate_id().unwrap();
    c.create_window(depth, w2, root, 0, 0, 10, 10, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().event_mask(EventMask::COLOR_MAP_CHANGE)).unwrap();
    let a = c.get_window_attributes(w).unwrap().reply().unwrap();
    r(&mut log, "attributes", format!("{:?}", (a.visual == vis, a.colormap == m, a.map_is_installed)));
    c.install_colormap(m).unwrap();
    log.take("install", c, &bases, root);
    r(&mut log, "installed", format!("{:?}", c.list_installed_colormaps(root).unwrap().reply().unwrap().cmaps.iter().map(|&x| x == m).collect::<Vec<_>>()));
    c.uninstall_colormap(m).unwrap();
    log.take("uninstall", c, &bases, root);
    c.install_colormap(m).unwrap();
    c.free_colormap(m).unwrap();
    log.take("free installed", c, &bases, root);
    c.change_window_attributes(w2, &ChangeWindowAttributesAux::new().colormap(def)).unwrap();
    log.take("set colormap", c, &bases, root);
    log.lines
}

#[test]
fn colormaps_match_xvfb() {
    for (n, depth, visual) in [(4, 8, vintrdp_x11::RootVisual::PseudoColor8), (5, 24, Default::default())] {
        let Some((_x, xc, _)) = xvfb_at(n, depth) else { return };
        let (_server, oc) = ours_with(visual);
        let want = colormaps(&xc);
        let got = colormaps(&oc);
        if std::env::var_os("VINTRDP_XVFB_LOG").is_some() {
            eprintln!("Xvfb:\n{}\nours:\n{}", want.join("\n"), got.join("\n"));
        }
        let mut diff = String::new();
        for i in 0..want.len().max(got.len()) {
            let (a, b) = (want.get(i).map_or("", |s| s.as_str()), got.get(i).map_or("", |s| s.as_str()));
            if a != b {
                diff.push_str(&format!("Xvfb: {a}\nours: {b}\n"));
            }
        }
        assert!(diff.is_empty(), "depth {depth}: differences from Xvfb:\n{diff}");
    }
}

/// What the front-end is shown, for the 8-bit windows on the 24-bit screen.
#[derive(Clone)]
struct Shown(std::sync::Arc<std::sync::Mutex<Vec<u32>>>);

impl Host for Shown {
    fn resize(&self, _: u16, _: u16) {}
    fn draw(&self, x: usize, y: usize, w: usize, h: usize, px: &[u32]) {
        let mut fb = self.0.lock().unwrap();
        for row in 0..h {
            fb[(y + row) * 320 + x..][..w].copy_from_slice(&px[row * w..][..w]);
        }
    }
    fn updated(&self) {}
}

/// The colours of a private 256-entry map, the same on both servers.
fn ramp(p: u32, shift: u32) -> (u16, u16, u16) {
    let c = |m: u32| (((p * m + shift) & 0xff) * 0x101) as u16;
    (c(7), c(13), c(29))
}

fn store_ramp(c: &RustConnection, map: Colormap, shift: u32) {
    let items: Vec<Coloritem> = (0..256).map(|p| {
        let (red, green, blue) = ramp(p, shift);
        Coloritem { pixel: p, red, green, blue, flags: ColorFlag::RED | ColorFlag::GREEN | ColorFlag::BLUE }
    }).collect();
    c.store_colors(map, &items).unwrap();
}

/// A depth-8 window of `visual` with a private map: the scene drawn in
/// it, and a child of its own. Its pixels, by GetImage.
fn eight_bit_window(c: &RustConnection, visual: Visualid, at: (i16, i16), shift: u32) -> (Window, Colormap, Vec<u32>) {
    let root = c.setup().roots[0].root;
    let map = c.generate_id().unwrap();
    c.create_colormap(ColormapAlloc::ALL, map, root, visual).unwrap();
    store_ramp(c, map, shift);
    let win = c.generate_id().unwrap();
    let aux = CreateWindowAux::new().background_pixel(17).border_pixel(200).colormap(map).override_redirect(1);
    c.create_window(8, win, root, at.0, at.1, W, H, 2, WindowClass::INPUT_OUTPUT, visual, &aux).unwrap();
    let child = c.generate_id().unwrap();
    c.create_window(8, child, win, 120, 70, 50, 40, 3, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().background_pixel(99).border_pixel(250)).unwrap();
    c.map_subwindows(win).unwrap();
    c.map_window(win).unwrap();
    scene_at(c, win, root, 8);
    (win, map, image(c, win))
}

fn depth_visual(c: &RustConnection, depth: u8, class: VisualClass) -> Option<Visualid> {
    c.setup().roots[0].allowed_depths.iter().filter(|d| d.depth == depth).flat_map(|d| d.visuals.iter()).find(|v| v.class == class).map(|v| v.visual_id)
}

/// Depth-8 PseudoColor windows on the 24-bit screen, as an overlay plane:
/// drawn the same as on an 8-bit Xvfb, and each shown through its own
/// colormap, the 24-bit default one still installed.
#[test]
fn eight_bit_windows_on_the_24_bit_screen_match_xvfb() {
    let Some((_x, xc, _)) = xvfb_at(6, 8) else { return };
    let shown = Shown(std::sync::Arc::new(std::sync::Mutex::new(vec![0; 320 * 240])));
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
    let _server = vintrdp_x11::start(settings, Box::new(shown.clone())).unwrap();
    let stream = DefaultStream::from_tcp_stream(TcpStream::connect(_server.local_addr()).unwrap()).unwrap().0;
    let oc = RustConnection::connect_to_stream(stream, 0).unwrap();
    assert_eq!(oc.setup().roots[0].root_depth, 24);
    let ours8 = depth_visual(&oc, 8, VisualClass::PSEUDO_COLOR).expect("an 8-bit PseudoColor visual");
    let xvfb8 = depth_visual(&xc, 8, VisualClass::PSEUDO_COLOR).unwrap();

    // The pixels: as the 8-bit Xvfb's.
    let (_, _, want) = eight_bit_window(&xc, xvfb8, (10, 10), 0);
    let (win, map, got) = eight_bit_window(&oc, ours8, (10, 10), 0);
    compare("eight-bit window", &want, &got, W as usize);

    // A second, another map, partly over the first.
    let (_, _, want2) = eight_bit_window(&xc, xvfb8, (100, 80), 50);
    let (win2, map2, got2) = eight_bit_window(&oc, ours8, (100, 80), 50);
    compare("second eight-bit window", &want2, &got2, W as usize);

    // What shows: each window's pixels through its own map. The root's
    // GetImage reads what shows, too (another depth's inferiors are
    // undefined in X, but this is what an overlay plane shows).
    let rgb = |p: u32, shift: u32| {
        let (r, g, b) = ramp(p, shift);
        ((r as u32 >> 8) << 16) | ((g as u32 >> 8) << 8) | (b as u32 >> 8)
    };
    let mut xscreen = xc.get_image(ImageFormat::Z_PIXMAP, xc.setup().roots[0].root, 0, 0, 320, 240, !0).unwrap().reply().unwrap().data;
    xscreen.truncate(320 * 240);
    let check = |shown: &[u32], label: &str, shift2: u32| {
        // Each pixel of the windows' rectangles (borders included): which map
        // shows there is the topmost window's.
        let (mut want, mut got) = (Vec::new(), Vec::new());
        for y in 0..240usize {
            for x in 0..320usize {
                let in1 = (10..10 + W as usize + 4).contains(&x) && (10..10 + H as usize + 4).contains(&y);
                let in2 = (100..100 + W as usize + 4).contains(&x) && (80..80 + H as usize + 4).contains(&y);
                let p = xscreen[y * 320 + x] as u32;
                want.push(if in2 { rgb(p, shift2) } else if in1 { rgb(p, 0) } else { 0 });
                got.push(if in1 || in2 { shown[y * 320 + x] } else { 0 });
            }
        }
        compare(label, &want, &got, 320);
    };
    let settle = || {
        oc.get_input_focus().unwrap().reply().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        shown.0.lock().unwrap().clone()
    };
    check(&settle(), "shown through each map", 50);
    let root_image = pixels(&oc.get_image(ImageFormat::Z_PIXMAP, oc.setup().roots[0].root, 10, 10, W, H, !0).unwrap().reply().unwrap().data, 24);
    // (The root's own pixels are its background pattern: only the windows' count.)
    let s = settle();
    let on_screen: Vec<u32> = (0..H as usize).flat_map(|y| s[(10 + y) * 320 + 10..][..W as usize].to_vec()).collect();
    compare("root's GetImage over them", &on_screen, &root_image, W as usize);

    // The second map's colours change: its window shows them at once.
    store_ramp(&oc, map2, 90);
    check(&settle(), "after StoreColors", 90);
    // The second window goes: the first shows where it was (inside its
    // border, at 12, 12).
    oc.destroy_window(win2).unwrap();
    let s = settle();
    let (want, got): (Vec<u32>, Vec<u32>) = (0..H as usize).flat_map(|y| (0..W as usize).map(move |x| (x, y))).map(|(x, y)| (rgb(want[y * W as usize + x], 0), s[(12 + y) * 320 + 12 + x])).unzip();
    compare("the first, uncovered", &want, &got, W as usize);
    let _ = (win, map, got2, want2);

    // Depths that don't match: X's errors.
    let root = oc.setup().roots[0].root;
    let w = oc.generate_id().unwrap();
    let no_map = oc.create_window(8, w, root, 0, 0, 10, 10, 0, WindowClass::INPUT_OUTPUT, ours8, &CreateWindowAux::new().border_pixel(0)).unwrap().check();
    assert!(matches!(no_map, Err(x11rb::errors::ReplyError::X11Error(ref e)) if e.error_kind == x11rb::protocol::ErrorKind::Match), "{no_map:?}");
    let gc = oc.generate_id().unwrap();
    oc.create_gc(gc, root, &CreateGCAux::new()).unwrap();
    let copy = oc.copy_area(win, root, gc, 0, 0, 0, 0, 10, 10).unwrap().check();
    assert!(matches!(copy, Err(x11rb::errors::ReplyError::X11Error(ref e)) if e.error_kind == x11rb::protocol::ErrorKind::Match), "{copy:?}");
}
