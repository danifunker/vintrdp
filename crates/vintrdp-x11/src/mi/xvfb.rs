//! Scan conversion checked against Xvfb: the same request drawn by X.org
//! into a pixmap there and by [`super`] into a buffer here, pixel for pixel.
//! Skipped when there's no Xvfb (`XVFB=path`, or on the PATH, or XQuartz's),
//! unless VINTRDP_XVFB=required, as in CI.

use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self as xp, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

use super::*;

pub const W: i32 = 160;
pub const H: i32 = 120;
/// What the pixmap starts as, so XOR shows pixels drawn twice.
pub const GROUND: u32 = 0x40_4040;

/// One request, to draw both ways.
#[derive(Clone, Debug)]
pub enum Op {
    Points(CoordMode, Vec<Point>),
    Lines(CoordMode, Vec<Point>),
    Segments(Vec<Segment>),
    Rects(Vec<Rect>),
    Arcs(Vec<Arc>),
    FillPoly(Shape, CoordMode, Vec<Point>),
    FillRects(Vec<Rect>),
    FillArcs(Vec<Arc>),
}

struct Xvfb {
    _child: Child,
    conn: RustConnection,
    root: u32,
}

fn find() -> Option<String> {
    if let Some(p) = std::env::var_os("XVFB") {
        return Some(p.to_string_lossy().into_owned());
    }
    for p in ["/usr/bin/Xvfb", "/opt/X11/bin/Xvfb", "/usr/local/bin/Xvfb"] {
        if std::path::Path::new(p).exists() {
            return Some(p.into());
        }
    }
    None
}

fn start() -> Option<Xvfb> {
    let Some(path) = find() else {
        assert!(std::env::var("VINTRDP_XVFB").as_deref() != Ok("required"), "no Xvfb");
        eprintln!("no Xvfb: skipped");
        return None;
    };
    // A display number of our own, by process.
    let display = 200 + std::process::id() % 500;
    let child = Command::new(&path)
        .arg(format!(":{display}"))
        .args(["-screen", "0", "320x240x24", "-listen", "tcp", "-nolisten", "unix", "-ac"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{path}: {e}"));
    for _ in 0..100 {
        if let Ok((conn, screen)) = x11rb::connect(Some(&format!("127.0.0.1:{display}"))) {
            let root = conn.setup().roots[screen].root;
            return Some(Xvfb { _child: child, conn, root });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("Xvfb didn't start");
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self._child.kill();
    }
}

fn server() -> &'static Mutex<Option<Xvfb>> {
    static X: OnceLock<Mutex<Option<Xvfb>>> = OnceLock::new();
    X.get_or_init(|| Mutex::new(start()))
}

/// Kill Xvfb when the test process ends: statics aren't dropped.
extern "C" fn stop() {
    if let Some(m) = SERVER.get() {
        if let Ok(mut x) = m.lock() {
            x.take();
        }
    }
}
static SERVER: OnceLock<&'static Mutex<Option<Xvfb>>> = OnceLock::new();

fn p16(p: &Point) -> xp::Point {
    xp::Point { x: p.x as i16, y: p.y as i16 }
}

fn r16(r: &Rect) -> xp::Rectangle {
    xp::Rectangle { x: r.x as i16, y: r.y as i16, width: r.w as u16, height: r.h as u16 }
}

fn a16(a: &Arc) -> xp::Arc {
    xp::Arc { x: a.x as i16, y: a.y as i16, width: a.w as u16, height: a.h as u16, angle1: a.angle1 as i16, angle2: a.angle2 as i16 }
}

fn mode(m: CoordMode) -> xp::CoordMode {
    if m == CoordMode::Origin { xp::CoordMode::ORIGIN } else { xp::CoordMode::PREVIOUS }
}

/// X.org's drawing, or None without Xvfb.
pub fn xorg(gc: &Gc, op: &Op) -> Option<Vec<u32>> {
    let lock = server();
    let _ = SERVER.set(lock);
    unsafe extern "C" {
        fn atexit(f: extern "C" fn()) -> i32;
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        atexit(stop);
    });
    let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let x = guard.as_ref()?;
    let c = &x.conn;
    let pix = c.generate_id().unwrap();
    c.create_pixmap(24, pix, x.root, W as u16, H as u16).unwrap();
    let ground = c.generate_id().unwrap();
    c.create_gc(ground, pix, &xp::CreateGCAux::new().foreground(GROUND)).unwrap();
    c.poly_fill_rectangle(pix, ground, &[xp::Rectangle { x: 0, y: 0, width: W as u16, height: H as u16 }]).unwrap();
    let g = c.generate_id().unwrap();
    let style = |s: LineStyle| match s {
        LineStyle::Solid => xp::LineStyle::SOLID,
        LineStyle::OnOffDash => xp::LineStyle::ON_OFF_DASH,
        LineStyle::DoubleDash => xp::LineStyle::DOUBLE_DASH,
    };
    let aux = xp::CreateGCAux::new()
        .function(xp::GX::from(gc.function))
        .foreground(gc.fg)
        .background(gc.bg)
        .line_width(gc.line_width)
        .line_style(style(gc.line_style))
        .cap_style(xp::CapStyle::from(gc.cap_style as u32))
        .join_style(xp::JoinStyle::from(gc.join_style as u32))
        .fill_rule(xp::FillRule::from(gc.fill_rule as u32))
        .arc_mode(xp::ArcMode::from(gc.arc_mode as u32))
        .graphics_exposures(0);
    c.create_gc(g, pix, &aux).unwrap();
    c.set_dashes(g, gc.dash_offset as u16, &gc.dashes).unwrap();
    match op {
        Op::Points(m, p) => c.poly_point(mode(*m), pix, g, &p.iter().map(p16).collect::<Vec<_>>()),
        Op::Lines(m, p) => c.poly_line(mode(*m), pix, g, &p.iter().map(p16).collect::<Vec<_>>()),
        Op::Segments(s) => {
            let s: Vec<xp::Segment> = s.iter().map(|s| xp::Segment { x1: s.x1 as i16, y1: s.y1 as i16, x2: s.x2 as i16, y2: s.y2 as i16 }).collect();
            c.poly_segment(pix, g, &s)
        }
        Op::Rects(r) => c.poly_rectangle(pix, g, &r.iter().map(r16).collect::<Vec<_>>()),
        Op::Arcs(a) => c.poly_arc(pix, g, &a.iter().map(a16).collect::<Vec<_>>()),
        Op::FillPoly(s, m, p) => {
            let shape = match s {
                Shape::Complex => xp::PolyShape::COMPLEX,
                Shape::Nonconvex => xp::PolyShape::NONCONVEX,
                Shape::Convex => xp::PolyShape::CONVEX,
            };
            c.fill_poly(pix, g, shape, mode(*m), &p.iter().map(p16).collect::<Vec<_>>())
        }
        Op::FillRects(r) => c.poly_fill_rectangle(pix, g, &r.iter().map(r16).collect::<Vec<_>>()),
        Op::FillArcs(a) => c.poly_fill_arc(pix, g, &a.iter().map(a16).collect::<Vec<_>>()),
    }
    .unwrap();
    let img = c.get_image(xp::ImageFormat::Z_PIXMAP, pix, 0, 0, W as u16, H as u16, !0).unwrap().reply().unwrap();
    c.free_gc(g).unwrap();
    c.free_gc(ground).unwrap();
    c.free_pixmap(pix).unwrap();
    Some(img.data.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0xff_ffff).collect())
}

/// A W x H buffer, filled solid through the GC's function.
pub struct Buffer {
    pub px: Vec<u32>,
    pub function: u8,
}

pub fn rop(function: u8, src: u32, dst: u32) -> u32 {
    match function & 15 {
        0 => 0,
        1 => src & dst,
        2 => src & !dst,
        3 => src,
        4 => !src & dst,
        5 => dst,
        6 => src ^ dst,
        7 => src | dst,
        8 => !(src | dst),
        9 => !src ^ dst,
        10 => !dst,
        11 => src | !dst,
        12 => !src,
        13 => !src | dst,
        14 => !(src & dst),
        _ => !0,
    }
}

impl Sink for Buffer {
    fn fill_spans(&mut self, pixel: u32, spans: &[Span]) {
        for s in spans {
            if s.y < 0 || s.y >= H {
                continue;
            }
            for x in s.x.max(0)..(s.x + s.w).min(W) {
                let d = &mut self.px[(s.y * W + x) as usize];
                *d = rop(self.function, pixel, *d) & 0xff_ffff;
            }
        }
    }
}

/// Ours.
pub fn ours(gc: &Gc, op: &Op) -> Vec<u32> {
    let mut b = Buffer { px: vec![GROUND; (W * H) as usize], function: gc.function };
    match op {
        Op::Points(m, p) => poly_point(&mut b, gc, *m, p),
        Op::Lines(m, p) => poly_line(&mut b, gc, *m, p),
        Op::Segments(s) => poly_segment(&mut b, gc, s),
        Op::Rects(r) => poly_rectangle(&mut b, gc, r),
        Op::Arcs(a) => poly_arc(&mut b, gc, a),
        Op::FillPoly(s, m, p) => fill_polygon(&mut b, gc, *s, *m, p),
        Op::FillRects(r) => poly_fill_rect(&mut b, gc, r),
        Op::FillArcs(a) => poly_fill_arc(&mut b, gc, a),
    }
    b.px
}

/// Draw `op` both ways and panic, with a picture of the difference, if they
/// differ.
pub fn check(name: &str, gc: &Gc, op: &Op) {
    let Some(want) = xorg(gc, op) else { return };
    let got = ours(gc, op);
    let bad: Vec<usize> = (0..want.len()).filter(|&i| want[i] != got[i]).collect();
    if bad.is_empty() {
        return;
    }
    // The bounding box of the differences: '#' both, 'x' X.org only, 'o' ours only.
    let (mut x0, mut y0, mut x1, mut y1) = (W, H, 0, 0);
    for &i in &bad {
        let (x, y) = (i as i32 % W, i as i32 / W);
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let (x0, y0, x1, y1) = ((x0 - 2).max(0), (y0 - 2).max(0), (x1 + 2).min(W - 1).min(x0 + 100), (y1 + 2).min(H - 1).min(y0 + 60));
    let mut pic = String::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = (y * W + x) as usize;
            let (w, g) = (want[i] != GROUND, got[i] != GROUND);
            pic.push(match (want[i] == got[i], w, g) {
                (true, true, _) => '#',
                (true, false, _) => '.',
                (false, true, false) => 'x',
                (false, false, true) => 'o',
                (false, _, _) => '?',
            });
        }
        pic.push('\n');
    }
    panic!("{name}: {} pixels differ from X.org's, around ({x0}, {y0}):\n{pic}gc: {gc:?}\nop: {op:?}", bad.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_rects() {
        let gc = Gc { fg: 0xff_ffff, function: 6, ..Gc::default() };
        check("fill rects", &gc, &Op::FillRects(vec![Rect { x: 5, y: 5, w: 20, h: 10 }, Rect { x: 10, y: 8, w: 20, h: 10 }, Rect { x: -5, y: 100, w: 300, h: 50 }]));
    }
}
