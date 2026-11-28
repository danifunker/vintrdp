//! RENDER on Xvfb and on this server, read back with GetImage and compared
//! pixel for pixel: every operator (with no mask, an alpha mask and a
//! component-alpha one), transforms and filters, repeats, alpha maps and
//! clips, solid fills and gradients, trapezoids and triangles, glyphs,
//! windows as sources; and the formats, filters and errors clients see.
//! Skipped without Xvfb (`XVFB=path`, or the usual places), unless
//! VINTRDP_XVFB=required, as in CI.

use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use vintrdp_x11::keymap::Keymap;
use vintrdp_x11::{Host, Server, Settings};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::render::{self, ConnectionExt as _, CreatePictureAux, PictOp, Pictformat, Pointfix};
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::{DefaultStream, RustConnection};

struct Nothing;
impl Host for Nothing {
    fn resize(&self, _: u16, _: u16) {}
    fn draw(&self, _: usize, _: usize, _: usize, _: usize, _: &[u32]) {}
    fn updated(&self) {}
}

fn ours(visual: vintrdp_x11::RootVisual) -> (Server, RustConnection) {
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

fn xvfb(n: u32, depth: u8) -> Option<(Xvfb, RustConnection)> {
    let path = std::env::var("XVFB").ok().or_else(|| ["/usr/bin/Xvfb", "/opt/X11/bin/Xvfb", "/usr/local/bin/Xvfb"].iter().find(|p| std::path::Path::new(p).exists()).map(|p| p.to_string()));
    let Some(path) = path else {
        assert!(std::env::var("VINTRDP_XVFB").as_deref() != Ok("required"), "no Xvfb");
        eprintln!("no Xvfb: skipped");
        return None;
    };
    let display = 1800 + std::process::id() % 100 * 10 + n;
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
            return Some((x, conn));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("Xvfb didn't start");
}

const W: u16 = 256;
const H: u16 = 236;

/// The formats a scene uses, found as clients find them.
struct Formats {
    argb: Pictformat,
    rgb24: Pictformat,
    a8: Pictformat,
    a1: Pictformat,
    rgb565: Option<Pictformat>,
}

fn formats(c: &RustConnection) -> Formats {
    c.render_query_version(0, 11).unwrap().reply().unwrap();
    let f = c.render_query_pict_formats().unwrap().reply().unwrap();
    let find = |depth: u8, a: (u16, u16), r: (u16, u16), g: (u16, u16), b: (u16, u16)| {
        f.formats.iter().find(|x| x.type_ == render::PictType::DIRECT && x.depth == depth && (x.direct.alpha_shift, x.direct.alpha_mask) == a && (x.direct.red_shift, x.direct.red_mask) == r && (x.direct.green_shift, x.direct.green_mask) == g && (x.direct.blue_shift, x.direct.blue_mask) == b).map(|x| x.id)
    };
    Formats {
        argb: find(32, (24, 0xff), (16, 0xff), (8, 0xff), (0, 0xff)).unwrap(),
        rgb24: find(24, (0, 0), (16, 0xff), (8, 0xff), (0, 0xff)).unwrap(),
        a8: find(8, (0, 0xff), (0, 0), (0, 0), (0, 0)).unwrap(),
        a1: find(1, (0, 1), (0, 0), (0, 0), (0, 0)).unwrap(),
        rgb565: find(16, (0, 0), (11, 0x1f), (5, 0x3f), (0, 0x1f)),
    }
}

fn fixed(v: f64) -> i32 {
    (v * 65536.0).round() as i32
}

fn pf(x: f64, y: f64) -> Pointfix {
    Pointfix { x: fixed(x), y: fixed(y) }
}

fn color(r: u16, g: u16, b: u16, a: u16) -> render::Color {
    render::Color { red: r, green: g, blue: b, alpha: a }
}

/// A pixmap of `depth` with a picture of `format`, its pixels from `px`.
fn picture(c: &RustConnection, root: Window, depth: u8, format: Pictformat, w: u16, h: u16, px: impl Fn(u32, u32) -> u32) -> (Pixmap, render::Picture) {
    let pm = c.generate_id().unwrap();
    c.create_pixmap(depth, pm, root, w, h).unwrap();
    let gc = c.generate_id().unwrap();
    c.create_gc(gc, pm, &CreateGCAux::new()).unwrap();
    let data: Vec<u8> = match depth {
        32 | 24 => (0..h as u32).flat_map(|y| (0..w as u32).map(move |x| (x, y))).flat_map(|(x, y)| px(x, y).to_le_bytes()).collect(),
        8 => {
            let stride = (w as usize).div_ceil(4) * 4;
            let mut v = vec![0u8; stride * h as usize];
            for y in 0..h as usize {
                for x in 0..w as usize {
                    v[y * stride + x] = px(x as u32, y as u32) as u8;
                }
            }
            v
        }
        16 => {
            let stride = (w as usize * 2).div_ceil(4) * 4;
            let mut v = vec![0u8; stride * h as usize];
            for y in 0..h as usize {
                for x in 0..w as usize {
                    v[y * stride + x * 2..y * stride + x * 2 + 2].copy_from_slice(&(px(x as u32, y as u32) as u16).to_le_bytes());
                }
            }
            v
        }
        _ => {
            let stride = (w as usize).div_ceil(32) * 4;
            let mut v = vec![0u8; stride * h as usize];
            for y in 0..h as usize {
                for x in 0..w as usize {
                    if px(x as u32, y as u32) != 0 {
                        v[y * stride + x / 8] |= 1 << (x % 8);
                    }
                }
            }
            v
        }
    };
    c.put_image(ImageFormat::Z_PIXMAP, pm, gc, w, h, 0, 0, 0, depth, &data).unwrap();
    c.free_gc(gc).unwrap();
    let p = c.generate_id().unwrap();
    c.render_create_picture(p, pm, format, &CreatePictureAux::new()).unwrap();
    (pm, p)
}

/// A premultiplied ARGB pattern.
fn argb(x: u32, y: u32, k: u32) -> u32 {
    let a = (x * 23 + y * 7 + k * 50) & 0xff;
    let ch = |v: u32| (v & 0xff) * a / 255;
    a << 24 | ch(x * 31 + k * 90) << 16 | ch(y * 19 + 40) << 8 | ch((x ^ y) * 13 + k * 20)
}

const OPS: [u8; 60] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 3, 3, 3, 3, 3, 3, 3,
];

/// Everything, into the picture `dst` (on `d`, of `depth`).
fn scene(c: &RustConnection, root: Window, d: Drawable, depth: u8, dst: render::Picture, f: &Formats) {
    // A background with some alpha (none on depth 24).
    let gc = c.generate_id().unwrap();
    c.create_gc(gc, d, &CreateGCAux::new()).unwrap();
    let bg: Vec<u8> = (0..H as u32).flat_map(|y| (0..W as u32).map(move |x| (x, y))).flat_map(|(x, y)| argb(x, y, 3).to_le_bytes()).collect();
    if depth == 8 {
        let v: Vec<u8> = (0..H as u32).flat_map(|y| (0..W as u32).map(move |x| ((x * 3 + y * 5) & 0xff) as u8)).collect();
        c.put_image(ImageFormat::Z_PIXMAP, d, gc, W, H, 0, 0, 0, depth, &v).unwrap();
    } else {
        c.put_image(ImageFormat::Z_PIXMAP, d, gc, W, H, 0, 0, 0, depth, &bg).unwrap();
    }
    c.free_gc(gc).unwrap();

    let (_s, src) = picture(c, root, 32, f.argb, 12, 12, |x, y| argb(x, y, 1));
    let (_m, mask) = picture(c, root, 8, f.a8, 12, 12, |x, y| (x * 20 + y * 13) & 0xff);
    let (_cm, ca) = picture(c, root, 32, f.argb, 12, 12, |x, y| (x * 21) << 24 | (y * 20) << 16 | ((x + y) * 10) << 8 | (255 - x * 20));
    c.render_change_picture(ca, &render::ChangePictureAux::new().componentalpha(1)).unwrap();

    // Every operator: no mask, an alpha mask, component alpha.
    for (i, &o) in OPS.iter().enumerate() {
        let (x, y) = ((i % 10) as i16 * 12, (i / 10) as i16 * 12);
        let o = PictOp::from(o);
        c.render_composite(o, src, x11rb::NONE, dst, 0, 0, 0, 0, x, y, 12, 12).unwrap();
        c.render_composite(o, src, mask, dst, 0, 0, 0, 0, 128 + x, y, 12, 12).unwrap();
        c.render_composite(o, src, ca, dst, 0, 0, 0, 0, x, 76 + y, 12, 12).unwrap();
    }

    // Repeats, transforms and filters, from a 7x5 image.
    let (_t, tex) = picture(c, root, 32, f.argb, 7, 5, |x, y| if (x + y) % 2 == 0 { 0xff20_80e0 } else { argb(x * 9, y * 11, 2) | 0x8000_0000 });
    for (i, rep) in [render::Repeat::NONE, render::Repeat::NORMAL, render::Repeat::PAD, render::Repeat::REFLECT].into_iter().enumerate() {
        c.render_change_picture(tex, &render::ChangePictureAux::new().repeat(rep)).unwrap();
        c.render_composite(PictOp::OVER, tex, x11rb::NONE, dst, -3, -2, 0, 0, 128 + i as i16 * 32, 76, 30, 20).unwrap();
    }
    let t = |a: f64, b: f64, cc: f64, dd: f64, e: f64, ff: f64| render::Transform { matrix11: fixed(a), matrix12: fixed(b), matrix13: fixed(cc), matrix21: fixed(dd), matrix22: fixed(e), matrix23: fixed(ff), matrix31: 0, matrix32: 0, matrix33: fixed(1.0) };
    c.render_change_picture(tex, &render::ChangePictureAux::new().repeat(render::Repeat::NORMAL)).unwrap();
    c.render_set_picture_transform(tex, t(0.37, 0.21, 0.5, -0.18, 0.4, 0.25)).unwrap();
    c.render_composite(PictOp::OVER, tex, x11rb::NONE, dst, 0, 0, 0, 0, 128, 98, 40, 24).unwrap();
    c.render_set_picture_filter(tex, b"bilinear", &[]).unwrap();
    c.render_composite(PictOp::OVER, tex, x11rb::NONE, dst, 0, 0, 0, 0, 170, 98, 40, 24).unwrap();
    c.render_set_picture_filter(tex, b"convolution", &[fixed(3.0), fixed(3.0), fixed(0.1), fixed(0.1), fixed(0.1), fixed(0.1), fixed(0.2), fixed(0.1), fixed(0.1), fixed(0.1), fixed(0.1)]).unwrap();
    c.render_composite(PictOp::OVER, tex, x11rb::NONE, dst, 0, 0, 0, 0, 212, 98, 40, 24).unwrap();
    // Projective, bilinear, padded.
    c.render_set_picture_filter(tex, b"good", &[]).unwrap();
    c.render_change_picture(tex, &render::ChangePictureAux::new().repeat(render::Repeat::PAD)).unwrap();
    c.render_set_picture_transform(tex, render::Transform { matrix11: fixed(0.3), matrix12: fixed(0.05), matrix13: 0, matrix21: fixed(0.02), matrix22: fixed(0.25), matrix23: 0, matrix31: fixed(0.004), matrix32: fixed(0.006), matrix33: fixed(1.0) }).unwrap();
    c.render_composite(PictOp::SRC, tex, x11rb::NONE, dst, 0, 0, 0, 0, 128, 124, 40, 24).unwrap();

    // An alpha map on the source, and a clipped destination.
    let (_am, amap) = picture(c, root, 8, f.a8, 12, 12, |x, y| ((x * y * 7) & 0xff) | 0x10);
    let (_s2, src2) = picture(c, root, 32, f.argb, 12, 12, |x, y| argb(x, y, 4));
    c.render_change_picture(src2, &render::ChangePictureAux::new().alphamap(amap).alphaxorigin(2).alphayorigin(1)).unwrap();
    c.render_set_picture_clip_rectangles(dst, 3, 2, &[Rectangle { x: 168, y: 122, width: 20, height: 9 }, Rectangle { x: 180, y: 131, width: 30, height: 12 }]).unwrap();
    c.render_composite(PictOp::OVER, src2, x11rb::NONE, dst, 0, 0, 0, 0, 170, 124, 40, 24).unwrap();
    c.render_composite(PictOp::ATOP, src2, mask, dst, 0, 0, 0, 0, 196, 126, 20, 20).unwrap();
    c.render_change_picture(dst, &render::ChangePictureAux::new().clipmask(x11rb::NONE)).unwrap();

    // Solid fills and gradients, each repeat.
    let stops = [0, fixed(0.3), fixed(1.0)];
    let colors = [color(0xffff, 0, 0, 0xffff), color(0x2000, 0x8000, 0x1000, 0x9000), color(0, 0, 0x4000, 0x4000)];
    let lin = c.generate_id().unwrap();
    c.render_create_linear_gradient(lin, pf(3.0, 2.0), pf(31.0, 23.5), &stops, &colors).unwrap();
    let rad = c.generate_id().unwrap();
    c.render_create_radial_gradient(rad, pf(18.0, 18.0), pf(22.5, 20.0), fixed(2.0), fixed(17.0), &stops, &colors).unwrap();
    let con = c.generate_id().unwrap();
    c.render_create_conical_gradient(con, pf(20.0, 20.0), fixed(30.0), &stops, &colors).unwrap();
    for (i, rep) in [render::Repeat::NONE, render::Repeat::NORMAL, render::Repeat::PAD, render::Repeat::REFLECT].into_iter().enumerate() {
        let x = i as i16 * 40;
        c.render_change_picture(lin, &render::ChangePictureAux::new().repeat(rep)).unwrap();
        c.render_composite(PictOp::OVER, lin, x11rb::NONE, dst, -2, -3, 0, 0, x, 150, 38, 20).unwrap();
        c.render_change_picture(rad, &render::ChangePictureAux::new().repeat(rep)).unwrap();
        c.render_composite(PictOp::OVER, rad, x11rb::NONE, dst, 0, 0, 0, 0, x, 170, 38, 20).unwrap();
    }
    c.render_composite(PictOp::OVER, con, x11rb::NONE, dst, 0, 0, 0, 0, 160, 150, 40, 40).unwrap();
    // A gradient through a transform, and as the mask of the saturate op.
    c.render_set_picture_transform(lin, t(0.8, 0.3, 1.0, -0.2, 1.1, 0.0)).unwrap();
    c.render_composite(PictOp::SATURATE, src, lin, dst, 0, 0, 0, 0, 200, 150, 40, 20).unwrap();
    let solid = c.generate_id().unwrap();
    c.render_create_solid_fill(solid, color(0x1234, 0x8765, 0xfedc, 0xc000)).unwrap();
    c.render_composite(PictOp::OVER, solid, mask, dst, 0, 0, 0, 0, 200, 170, 12, 12).unwrap();
    c.render_composite(PictOp::DISJOINT_OVER, solid, ca, dst, 0, 0, 0, 0, 214, 170, 12, 12).unwrap();
    c.render_fill_rectangles(PictOp::OVER, dst, color(0x4000, 0x2000, 0x1000, 0x6000), &[Rectangle { x: 230, y: 170, width: 20, height: 8 }]).unwrap();
    c.render_fill_rectangles(PictOp::SRC, dst, color(0x8000, 0xffff, 0x1000, 0x8000), &[Rectangle { x: 230, y: 180, width: 9, height: 9 }, Rectangle { x: 241, y: 180, width: 9, height: 9 }]).unwrap();

    // Trapezoids and triangles: through an a8 mask, sharp and smooth.
    let red = c.generate_id().unwrap();
    c.render_create_solid_fill(red, color(0xc000, 0x2000, 0x4000, 0xe000)).unwrap();
    let trap = |t: f64, b: f64, l1: (f64, f64), l2: (f64, f64), r1: (f64, f64), r2: (f64, f64)| render::Trapezoid { top: fixed(t), bottom: fixed(b), left: render::Linefix { p1: pf(l1.0, l1.1), p2: pf(l2.0, l2.1) }, right: render::Linefix { p1: pf(r1.0, r1.1), p2: pf(r2.0, r2.1) } };
    let traps = [trap(192.3, 210.7, (2.2, 190.0), (10.9, 212.0), (30.1, 190.0), (22.6, 212.0)), trap(205.5, 233.2, (14.0, 200.0), (3.0, 236.0), (25.0, 204.0), (40.0, 236.0))];
    c.render_trapezoids(PictOp::OVER, red, dst, f.a8, 0, 0, &traps).unwrap();
    c.render_change_picture(dst, &render::ChangePictureAux::new().polyedge(render::PolyEdge::SHARP)).unwrap();
    c.render_trapezoids(PictOp::OVER, src, dst, x11rb::NONE, 1, 2, &[trap(192.0, 214.4, (42.5, 190.0), (44.0, 216.0), (58.25, 190.0), (64.0, 216.0))]).unwrap();
    c.render_change_picture(dst, &render::ChangePictureAux::new().polyedge(render::PolyEdge::SMOOTH)).unwrap();
    c.render_trapezoids(PictOp::ADD, red, dst, x11rb::NONE, 0, 0, &[trap(216.0, 234.0, (42.5, 214.0), (50.0, 236.0), (60.0, 214.0), (64.0, 236.0))]).unwrap();
    let tri = |a: (f64, f64), b: (f64, f64), cc: (f64, f64)| render::Triangle { p1: pf(a.0, a.1), p2: pf(b.0, b.1), p3: pf(cc.0, cc.1) };
    c.render_triangles(PictOp::OVER, red, dst, f.a8, 0, 0, &[tri((70.0, 192.5), (90.3, 230.0), (66.6, 221.0)), tri((80.0, 194.0), (95.0, 196.0), (88.0, 211.0))]).unwrap();
    c.render_tri_strip(PictOp::OVER, src, dst, f.a1, 0, 0, &[pf(96.0, 192.0), pf(100.0, 234.0), pf(110.5, 195.0), pf(112.0, 230.5), pf(124.0, 198.0)]).unwrap();
    c.render_tri_fan(PictOp::XOR, red, dst, x11rb::NONE, 0, 0, &[pf(110.0, 215.0), pf(100.0, 205.0), pf(120.0, 203.0), pf(126.0, 222.0), pf(104.0, 233.0)]).unwrap();
    // AddTraps into an alpha picture, then through it.
    let (_a, alpha) = picture(c, root, 8, f.a8, 30, 20, |_, _| 0);
    c.render_add_traps(alpha, 2, -1, &[render::Trap { top: render::Spanfix { l: fixed(1.5), r: fixed(20.25), y: fixed(1.2) }, bot: render::Spanfix { l: fixed(6.0), r: fixed(26.0), y: fixed(18.9) } }]).unwrap();
    c.render_composite(PictOp::OVER, red, alpha, dst, 0, 0, 0, 0, 226, 192, 30, 20).unwrap();

    // Glyphs: 8-bit alpha, 1-bit, and component alpha; with a mask and
    // without, switching glyph sets.
    let ga8 = c.generate_id().unwrap();
    c.render_create_glyph_set(ga8, f.a8).unwrap();
    let infos = [render::Glyphinfo { width: 7, height: 9, x: 1, y: 8, x_off: 8, y_off: 0 }, render::Glyphinfo { width: 5, height: 11, x: -1, y: 10, x_off: 6, y_off: 1 }];
    let mut data = Vec::new();
    for g in 0..2u32 {
        let (w, h) = (infos[g as usize].width as u32, infos[g as usize].height as u32);
        for y in 0..h {
            for x in 0..w.div_ceil(4) * 4 {
                data.push(if x < w { ((x * 40 + y * 25 + g * 77) % 256) as u8 } else { 0 });
            }
        }
    }
    c.render_add_glyphs(ga8, &[1, 2], &infos, &data).unwrap();
    let ga1 = c.generate_id().unwrap();
    c.render_create_glyph_set(ga1, f.a1).unwrap();
    let bits: Vec<u8> = (0..10u8).flat_map(|y| [0x5a_u8.rotate_left(y as u32), 0, 0, 0]).collect();
    c.render_add_glyphs(ga1, &[7], &[render::Glyphinfo { width: 8, height: 10, x: 0, y: 9, x_off: 9, y_off: 0 }], &bits).unwrap();
    let gca = c.generate_id().unwrap();
    c.render_create_glyph_set(gca, f.argb).unwrap();
    let cad: Vec<u8> = (0..6u32 * 8).flat_map(|i| ((i * 37) << 24 | (i * 5) << 16 | (i * 11) << 8 | (255 - i * 3)).to_le_bytes()).collect();
    c.render_add_glyphs(gca, &[300], &[render::Glyphinfo { width: 6, height: 8, x: 0, y: 7, x_off: 7, y_off: 0 }], &cad).unwrap();
    let elt = |n: u8, dx: i16, dy: i16, ids: &[u8]| {
        let mut v = vec![n, 0, 0, 0];
        v.extend_from_slice(&dx.to_le_bytes());
        v.extend_from_slice(&dy.to_le_bytes());
        v.extend_from_slice(ids);
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v
    };
    let blue = c.generate_id().unwrap();
    c.render_create_solid_fill(blue, color(0x1000, 0x3000, 0xd000, 0xffff)).unwrap();
    c.render_composite_glyphs8(PictOp::OVER, blue, dst, f.a8, ga8, 0, 0, &[elt(3, 130, 202, &[1, 2, 1]), elt(2, 4, 3, &[2, 1])].concat()).unwrap();
    c.render_composite_glyphs8(PictOp::OVER, src, dst, x11rb::NONE, ga8, 3, 1, &elt(4, 130, 220, &[2, 2, 1, 9])).unwrap();
    let mut cmds = elt(2, 175, 205, &[1, 0, 2, 0]);
    let mut switch = vec![255, 0, 0, 0, 0, 0, 0, 0];
    switch.extend_from_slice(&ga1.to_le_bytes());
    cmds.extend(switch);
    cmds.extend(elt(2, 1, 0, &[7, 0, 7, 0]));
    c.render_composite_glyphs16(PictOp::OVER, red, dst, f.a8, ga8, 0, 0, &cmds).unwrap();
    let mut ids = Vec::new();
    for g in [300u32, 300, 300] {
        ids.extend_from_slice(&g.to_le_bytes());
    }
    c.render_composite_glyphs32(PictOp::OVER, blue, dst, f.argb, gca, 0, 0, &elt(3, 175, 222, &ids)).unwrap();
    c.render_composite_glyphs32(PictOp::ADD, red, dst, x11rb::NONE, gca, 0, 0, &elt(2, 210, 222, &ids[..8])).unwrap();
    for p in [src, mask, ca, tex, amap, src2, lin, rad, con, solid, red, alpha, blue] {
        c.render_free_picture(p).unwrap();
    }
    for g in [ga8, ga1, gca] {
        c.render_free_glyph_set(g).unwrap();
    }
    // A 16-bit picture, composited onto and from.
    if let Some(f565) = f.rgb565 {
        let (_p, p565) = picture(c, root, 16, f565, 20, 12, |x, y| (x * 3000 + y * 700) & 0xffff);
        let (_s, s) = picture(c, root, 32, f.argb, 12, 12, |x, y| argb(x, y, 6));
        c.render_composite(PictOp::OVER, s, x11rb::NONE, p565, 0, 0, 0, 0, 4, 0, 12, 12).unwrap();
        c.render_composite(PictOp::SRC, p565, x11rb::NONE, dst, 0, 0, 0, 0, 236, 130, 20, 12).unwrap();
    }
}

fn image(c: &RustConnection, d: Drawable, depth: u8, w: u16, h: u16) -> Vec<u32> {
    let img = c.get_image(ImageFormat::Z_PIXMAP, d, 0, 0, w, h, !0).unwrap().reply().unwrap();
    match depth {
        8 => {
            let stride = (w as usize).div_ceil(4) * 4;
            (0..h as usize).flat_map(|y| (0..w as usize).map(move |x| (x, y))).map(|(x, y)| img.data[y * stride + x] as u32).collect()
        }
        _ => img.data.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & if depth == 24 { 0xff_ffff } else { !0 }).collect(),
    }
}

fn compare(name: &str, want: &[u32], got: &[u32], w: usize) {
    let bad: Vec<usize> = (0..want.len()).filter(|&i| want[i] != got[i]).collect();
    // VINTRDP_XVFB_DUMP=dir saves both (and where they differ), as PPM.
    if let Some(dir) = std::env::var_os("VINTRDP_XVFB_DUMP") {
        for (who, px) in [("xvfb", want), ("ours", got)] {
            let mut f = format!("P6 {w} {} 255\n", px.len() / w).into_bytes();
            f.extend(px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]));
            std::fs::write(std::path::Path::new(&dir).join(format!("{name}-{who}.ppm")), f).unwrap();
        }
        let mut diff = format!("P6 {w} {} 255\n", want.len() / w).into_bytes();
        diff.extend((0..want.len()).flat_map(|i| if want[i] == got[i] { [0u8, 0, 0] } else { [255, 255, 255] }));
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}-diff.ppm")), diff).unwrap();
    }
    if bad.is_empty() {
        return;
    }
    let near: Vec<String> = bad.iter().take(8).map(|&i| format!("({},{}) {:08x}/{:08x}", i % w, i / w, want[i], got[i])).collect();
    panic!("{name}: {} of {} pixels differ from Xvfb's: {}", bad.len(), want.len(), near.join(" "));
}

/// No errors came back (the scene's requests aren't checked one by one).
fn no_errors(c: &RustConnection, who: &str) {
    c.get_input_focus().unwrap().reply().unwrap();
    while let Some(ev) = c.poll_for_event().unwrap() {
        panic!("{who}: {ev:?}");
    }
}

/// The scene on a 24-bit pixmap, a 32-bit one, and a window.
fn run(c: &RustConnection) -> Vec<(&'static str, Vec<u32>)> {
    let f = formats(c);
    let root = c.setup().roots[0].root;
    let mut out = Vec::new();
    for (name, depth, format) in [("rgb24", 24u8, f.rgb24), ("argb32", 32, f.argb)] {
        let pm = c.generate_id().unwrap();
        c.create_pixmap(depth, pm, root, W, H).unwrap();
        let p = c.generate_id().unwrap();
        c.render_create_picture(p, pm, format, &CreatePictureAux::new()).unwrap();
        scene(c, root, pm, depth, p, &f);
        out.push((name, image(c, pm, depth, W, H)));
    }
    // A window, a child covering part of it.
    let win = c.generate_id().unwrap();
    c.create_window(24, win, root, 20, 2, W, H, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().background_pixel(0x102030).override_redirect(1)).unwrap();
    let child = c.generate_id().unwrap();
    c.create_window(24, child, win, 150, 60, 50, 40, 2, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().background_pixel(0x804020).border_pixel(0x00ff00)).unwrap();
    c.map_subwindows(win).unwrap();
    c.map_window(win).unwrap();
    let p = c.generate_id().unwrap();
    c.render_create_picture(p, win, f.rgb24, &CreatePictureAux::new()).unwrap();
    scene(c, root, win, 24, p, &f);
    // The window as a source, onto a pixmap.
    let pm = c.generate_id().unwrap();
    c.create_pixmap(32, pm, root, 64, 48).unwrap();
    let pp = c.generate_id().unwrap();
    c.render_create_picture(pp, pm, f.argb, &CreatePictureAux::new()).unwrap();
    c.render_fill_rectangles(PictOp::SRC, pp, color(0, 0, 0, 0), &[Rectangle { x: 0, y: 0, width: 64, height: 48 }]).unwrap();
    c.render_composite(PictOp::OVER, p, x11rb::NONE, pp, 140, 50, 0, 0, 0, 0, 64, 48).unwrap();
    c.render_change_picture(p, &render::ChangePictureAux::new().subwindowmode(SubwindowMode::INCLUDE_INFERIORS)).unwrap();
    c.render_fill_rectangles(PictOp::OVER, p, color(0x8000, 0x8000, 0x8000, 0x8000), &[Rectangle { x: 140, y: 50, width: 40, height: 30 }]).unwrap();
    no_errors(c, "scene");
    out.push(("window", image(c, win, 24, W, H)));
    out.push(("from-window", image(c, pm, 32, 64, 48)));
    out
}

#[test]
fn render_matches_xvfb() {
    let Some((_x, xc)) = xvfb(0, 24) else { return };
    let (_server, oc) = ours(Default::default());
    let want = run(&xc);
    let got = run(&oc);
    for ((name, w), (_, g)) in want.iter().zip(&got) {
        let width = if *name == "from-window" { 64 } else { W as usize };
        compare(name, w, g, width);
    }
}

/// On an 8-bit PseudoColor screen: pictures of the indexed format, the
/// colour cube RENDER allocated.
#[test]
fn render_pseudocolor_matches_xvfb() {
    let Some((_x, xc)) = xvfb(1, 8) else { return };
    let (_server, oc) = ours(vintrdp_x11::RootVisual::PseudoColor8);
    let run8 = |c: &RustConnection| -> Vec<u32> {
        let f = formats(c);
        let root = c.setup().roots[0].root;
        let vis = c.setup().roots[0].root_visual;
        let fl = c.render_query_pict_formats().unwrap().reply().unwrap();
        let fmt = fl.screens[0].depths.iter().flat_map(|d| &d.visuals).find(|v| v.visual == vis).unwrap().format;
        let pm = c.generate_id().unwrap();
        c.create_pixmap(8, pm, root, W, H).unwrap();
        let p = c.generate_id().unwrap();
        c.render_create_picture(p, pm, fmt, &CreatePictureAux::new()).unwrap();
        scene(c, root, pm, 8, p, &f);
        no_errors(c, "scene8");
        image(c, pm, 8, W, H)
    };
    compare("pseudocolor8", &run8(&xc), &run8(&oc), W as usize);
}

/// The formats, filters, index values and errors clients see.
fn behaviour(c: &RustConnection) -> Vec<String> {
    let mut log = Vec::new();
    let v = c.render_query_version(0, 11).unwrap().reply().unwrap();
    log.push(format!("version {}.{}", v.major_version, v.minor_version));
    let v = c.render_query_version(0, 5).unwrap().reply().unwrap();
    log.push(format!("version {}.{}", v.major_version, v.minor_version));
    c.render_query_version(0, 11).unwrap().reply().unwrap();
    let f = c.render_query_pict_formats().unwrap().reply().unwrap();
    // Formats by what they are (ids differ); depth 15 is ours alone.
    let mut kinds: Vec<String> = f.formats.iter().filter(|x| x.depth != 15).map(|x| format!("{:?} {} {:?}", x.type_, x.depth, x.direct)).collect();
    kinds.sort();
    log.extend(kinds);
    log.push(format!("subpixels {:?}", f.subpixels));
    let root = c.setup().roots[0].root;
    let q = c.render_query_filters(root).unwrap().reply().unwrap();
    log.push(format!("filters {:?} {:?}", q.aliases, q.filters.iter().map(|s| String::from_utf8_lossy(&s.name).into_owned()).collect::<Vec<_>>()));
    let ff = formats(c);
    let errs: Vec<Box<dyn Fn() -> Result<(), x11rb::errors::ReplyError> + '_>> = vec![
        // A picture of a format of another depth: BadMatch.
        std::boxed::Box::new(|| {
            let p = c.generate_id().unwrap();
            c.render_create_picture(p, root, ff.argb, &CreatePictureAux::new())?.check()
        }),
        // No such picture.
        std::boxed::Box::new(|| c.render_free_picture(0x1234)?.check()),
        // A bad repeat value, a bad operator.
        std::boxed::Box::new(|| {
            let p = c.generate_id().unwrap();
            c.render_create_picture(p, root, ff.rgb24, &CreatePictureAux::new().repeat(render::Repeat::from(7u32)))?.check()
        }),
        std::boxed::Box::new(|| {
            let s = c.generate_id().unwrap();
            c.render_create_solid_fill(s, color(0, 0, 0, 0))?.check()?;
            let p = c.generate_id().unwrap();
            c.render_create_picture(p, root, ff.rgb24, &CreatePictureAux::new())?.check()?;
            c.render_composite(PictOp::from(14u8), s, x11rb::NONE, p, 0, 0, 0, 0, 0, 0, 1, 1)?.check()
        }),
        // A filter that isn't, and one with the wrong parameters.
        std::boxed::Box::new(|| {
            let s = c.generate_id().unwrap();
            c.render_create_solid_fill(s, color(0, 0, 0, 0))?.check()?;
            c.render_set_picture_filter(s, b"gaussian", &[])?.check()
        }),
        std::boxed::Box::new(|| {
            let s = c.generate_id().unwrap();
            c.render_create_solid_fill(s, color(0, 0, 0, 0))?.check()?;
            c.render_set_picture_filter(s, b"Nearest", &[3])?.check()
        }),
        // A glyph set of a 24-bit format; freeing a glyph that isn't.
        std::boxed::Box::new(|| {
            let g = c.generate_id().unwrap();
            c.render_create_glyph_set(g, ff.rgb24)?.check()
        }),
        std::boxed::Box::new(|| {
            let g = c.generate_id().unwrap();
            c.render_create_glyph_set(g, ff.a8)?.check()?;
            c.render_free_glyphs(g, &[5])?.check()
        }),
        // Gradient stops out of order.
        std::boxed::Box::new(|| {
            let g = c.generate_id().unwrap();
            c.render_create_linear_gradient(g, pf(0.0, 0.0), pf(1.0, 1.0), &[fixed(0.5), fixed(0.2)], &[color(0, 0, 0, 0), color(0, 0, 0, 0)])?.check()
        }),
    ];
    let base_err = c.extension_information(render::X11_EXTENSION_NAME).unwrap().unwrap().first_error;
    for e in errs {
        match e() {
            Ok(()) => log.push("ok".into()),
            Err(x11rb::errors::ReplyError::X11Error(e)) => {
                let code = if e.error_code >= 128 { format!("render+{}", e.error_code - base_err) } else { format!("{}", e.error_code) };
                log.push(format!("error {code} minor {}", e.minor_opcode));
            }
            Err(e) => panic!("{e:?}"),
        }
    }
    log
}

#[test]
fn render_behaviour_matches_xvfb() {
    let Some((_x, xc)) = xvfb(2, 24) else { return };
    let (_server, oc) = ours(Default::default());
    let (want, got) = (behaviour(&xc), behaviour(&oc));
    // Xvfb's 32-bit ARGB visual (from Composite) has a format we have too;
    // compare line by line.
    assert_eq!(want, got);
}
