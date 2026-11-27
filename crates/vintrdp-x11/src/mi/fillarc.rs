//! Filled arcs, ported from X.org's mi/mifillarc.c and mi/mifillarc.h
//! (miPolyFillArc: whole ellipses and slices, chords and pie slices, in
//! integer and double arithmetic). COPYING in this directory has their
//! notices.
//!
//! The arithmetic is C's: int where X.org's is int (wrapping where it can
//! overflow), double where it's double. Span origins are 16-bit, as X.org's
//! DDXPointRecs are.

use super::*;

const FULLCIRCLE: i32 = 360 * 64;
const QUADRANT: i32 = 90 * 64;
const HALFCIRCLE: i32 = 180 * 64;
const QUADRANT3: i32 = 270 * 64;

/// An xArc as it comes off the wire.
#[derive(Clone, Copy)]
struct XArc {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    angle1: i32,
    angle2: i32,
}

fn xarc(a: &Arc) -> XArc {
    XArc {
        x: a.x as i16 as i32,
        y: a.y as i16 as i32,
        width: a.w as u16 as i32,
        height: a.h as u16 as i32,
        angle1: a.angle1 as i16 as i32,
        angle2: a.angle2 as i16 as i32,
    }
}

fn dsin(d: i32) -> f64 {
    (d as f64 * (std::f64::consts::PI / 11520.0)).sin()
}

fn dcos(d: i32) -> f64 {
    (d as f64 * (std::f64::consts::PI / 11520.0)).cos()
}

/// miFillArcEmpty.
fn fill_arc_empty(a: &XArc) -> bool {
    a.angle2 == 0 || a.width == 0 || a.height == 0 || (a.width == 1 && a.height & 1 != 0)
}

/// miCanFillArc.
fn can_fill_arc(a: &XArc) -> bool {
    a.width == a.height || (a.width <= 800 && a.height <= 800)
}

/// The stepping state of miFillArcRec (`T` = i64, holding C ints that
/// can't overflow for the arcs miCanFillArc takes) or miFillArcDRec (f64).
#[derive(Clone, Copy)]
struct FillArc<T> {
    xorg: i32,
    yorg: i32,
    y: i32,
    dx: i32,
    dy: i32,
    e: T,
    ym: T,
    yk: T,
    xm: T,
    xk: T,
}

/// miFillArcSetup.
fn fill_arc_setup(arc: &XArc) -> FillArc<i64> {
    let mut y = arc.height >> 1;
    let dy = arc.height & 1;
    let yorg = arc.y + y;
    let dx = arc.width & 1;
    let xorg = arc.x + (arc.width >> 1) + dx;
    let dx = 1 - dx;
    let (ym, xm, mut yk, xk, e);
    if arc.width == arc.height {
        ym = 8;
        xm = 8;
        yk = (y as i64) << 3;
        if dx == 0 {
            xk = 0;
            e = -1;
        } else {
            y += 1;
            yk += 4;
            xk = -4;
            e = -((y as i64) << 3);
        }
    } else {
        let w = arc.width as i64;
        let h = arc.height as i64;
        ym = (w * w) << 3;
        xm = (h * h) << 3;
        yk = y as i64 * ym;
        if dy == 0 {
            yk -= ym >> 1;
        }
        if dx == 0 {
            xk = 0;
            e = -(xm >> 3);
        } else {
            y += 1;
            yk += ym;
            xk = -(xm >> 1);
            e = xk - yk;
        }
    }
    FillArc { xorg, yorg, y, dx, dy, e, ym, yk, xm, xk }
}

/// miFillArcDSetup.
fn fill_arc_d_setup(arc: &XArc) -> FillArc<f64> {
    let mut y = arc.height >> 1;
    let dy = arc.height & 1;
    let yorg = arc.y + y;
    let dx = arc.width & 1;
    let xorg = arc.x + (arc.width >> 1) + dx;
    let dx = 1 - dx;
    let ym = arc.width as f64 * (arc.width * 8) as f64;
    let xm = arc.height as f64 * (arc.height * 8) as f64;
    let mut yk = y as f64 * ym;
    if dy == 0 {
        yk -= ym / 2.0;
    }
    let (xk, e);
    if dx == 0 {
        xk = 0.0;
        e = -(xm / 8.0);
    } else {
        y += 1;
        yk += ym;
        xk = -xm / 2.0;
        e = xk - yk;
    }
    FillArc { xorg, yorg, y, dx, dy, e, ym, yk, xm, xk }
}

/// The arithmetic MIFILLARCSTEP needs, for both variants.
trait Num: Copy + PartialOrd + std::ops::AddAssign + std::ops::SubAssign {
    const ZERO: Self;
}

impl Num for i64 {
    const ZERO: i64 = 0;
}

impl Num for f64 {
    const ZERO: f64 = 0.0;
}

/// The running registers (MIFILLARCSETUP).
struct Fill<T> {
    x: i32,
    y: i32,
    e: T,
    xk: T,
    xm: T,
    yk: T,
    ym: T,
    dx: i32,
    dy: i32,
    xorg: i32,
    yorg: i32,
}

impl<T: Num> Fill<T> {
    fn new(i: &FillArc<T>) -> Self {
        Fill { x: 0, y: i.y, e: i.e, xk: i.xk, xm: i.xm, yk: i.yk, ym: i.ym, dx: i.dx, dy: i.dy, xorg: i.xorg, yorg: i.yorg }
    }

    /// MIFILLARCSTEP: the next row's span width.
    fn step(&mut self) -> i32 {
        self.e += self.yk;
        while self.e >= T::ZERO {
            self.x += 1;
            self.xk -= self.xm;
            self.e += self.xk;
        }
        self.y -= 1;
        self.yk -= self.ym;
        let mut slw = (self.x << 1) + self.dx;
        if self.e == self.xk && slw > 1 {
            slw -= 1;
        }
        slw
    }

    /// miFillArcLower.
    fn lower(&self, slw: i32) -> bool {
        self.y + self.dy != 0 && (slw > 1 || self.e != self.xk)
    }
}

/// A span at a 16-bit origin, as in a DDXPointRec.
fn span(x: i32, y: i32, w: i32) -> Span {
    Span { x: x as i16 as i32, y: y as i16 as i32, w }
}

/// miFillEllipseI and miFillEllipseD.
fn fill_ellipse<T: Num>(info: &FillArc<T>, out: &mut Vec<Span>) {
    let mut f = Fill::new(info);
    while f.y > 0 {
        let slw = f.step();
        out.push(span(f.xorg - f.x, f.yorg - f.y, slw));
        if f.lower(slw) {
            out.push(span(f.xorg - f.x, f.yorg + f.y + f.dy, slw));
        }
    }
}

/// miSliceEdgeRec.
#[derive(Clone, Copy, Default)]
struct SliceEdge {
    x: i32,
    stepx: i32,
    deltax: i32,
    e: i32,
    dy: i32,
    dx: i32,
}

impl SliceEdge {
    /// MIARCSLICESTEP.
    fn step(&mut self) {
        self.x = self.x.wrapping_sub(self.stepx);
        self.e = self.e.wrapping_sub(self.dx);
        if self.e <= 0 {
            self.x = self.x.wrapping_sub(self.deltax);
            self.e = self.e.wrapping_add(self.dy);
        }
    }
}

/// miArcSliceRec.
#[derive(Clone, Copy, Default)]
struct ArcSlice {
    edge1: SliceEdge,
    edge2: SliceEdge,
    min_top_y: i32,
    max_top_y: i32,
    min_bot_y: i32,
    max_bot_y: i32,
    edge1_top: bool,
    edge2_top: bool,
    flip_top: bool,
    flip_bot: bool,
}

/// miGetArcEdge.
fn get_arc_edge(arc: &XArc, edge: &mut SliceEdge, k: i32, top: bool, left: bool) {
    let mut y = arc.height >> 1;
    if arc.width & 1 == 0 {
        y += 1;
    }
    if !top {
        y = -y;
        if arc.height & 1 != 0 {
            y -= 1;
        }
    }
    let xady = k.wrapping_add(y.wrapping_mul(edge.dx));
    if xady <= 0 {
        edge.x = (xady.wrapping_neg() / edge.dy + 1).wrapping_neg();
    } else {
        edge.x = (xady - 1) / edge.dy;
    }
    edge.e = xady.wrapping_sub(edge.x.wrapping_mul(edge.dy));
    if (top && edge.dx < 0) || (!top && edge.dx > 0) {
        edge.e = edge.dy.wrapping_sub(edge.e).wrapping_add(1);
    }
    if left {
        edge.x = edge.x.wrapping_add(1);
    }
    edge.x = edge.x.wrapping_add(arc.x + (arc.width >> 1));
    if edge.dx > 0 {
        edge.deltax = 1;
        edge.stepx = edge.dx / edge.dy;
        edge.dx %= edge.dy;
    } else {
        edge.deltax = -1;
        edge.stepx = -(edge.dx.wrapping_neg() / edge.dy);
        edge.dx = edge.dx.wrapping_neg() % edge.dy;
    }
    if !top {
        edge.deltax = -edge.deltax;
        edge.stepx = edge.stepx.wrapping_neg();
    }
}

/// miEllipseAngleToSlope, without the double outputs nobody here uses.
fn ellipse_angle_to_slope(angle: i32, width: i32, height: i32) -> (i32, i32) {
    match angle {
        0 => (-1, 0),
        QUADRANT => (0, 1),
        HALFCIRCLE => (1, 0),
        QUADRANT3 => (0, -1),
        _ => {
            let mut d_dx = dcos(angle) * width as f64;
            let mut d_dy = dsin(angle) * height as f64;
            let negative_dx = d_dx < 0.0;
            if negative_dx {
                d_dx = -d_dx;
            }
            let negative_dy = d_dy < 0.0;
            if negative_dy {
                d_dy = -d_dy;
            }
            let scale = if d_dy > d_dx { d_dy } else { d_dx };
            let mut dx = ((d_dx * 32768.0) / scale + 0.5).floor() as i32;
            if negative_dx {
                dx = -dx;
            }
            let mut dy = ((d_dy * 32768.0) / scale + 0.5).floor() as i32;
            if negative_dy {
                dy = -dy;
            }
            (dx, dy)
        }
    }
}

/// miGetPieEdge.
fn get_pie_edge(arc: &XArc, angle: i32, edge: &mut SliceEdge, top: bool, left: bool) {
    let (mut dx, mut dy) = ellipse_angle_to_slope(angle, arc.width, arc.height);
    if dy == 0 {
        edge.x = if left { -65536 } else { 65536 };
        edge.stepx = 0;
        edge.e = 0;
        edge.dx = -1;
        return;
    }
    if dx == 0 {
        edge.x = arc.x + (arc.width >> 1);
        if left && arc.width & 1 != 0 {
            edge.x += 1;
        } else if !left && arc.width & 1 == 0 {
            edge.x -= 1;
        }
        edge.stepx = 0;
        edge.e = 0;
        edge.dx = -1;
        return;
    }
    if dy < 0 {
        dx = -dx;
        dy = -dy;
    }
    let mut k = if arc.height & 1 != 0 { dx } else { 0 };
    if arc.width & 1 != 0 {
        k += dy;
    }
    edge.dx = dx << 1;
    edge.dy = dy << 1;
    get_arc_edge(arc, edge, k, top, left);
}

/// miFillArcSliceSetup.
fn fill_arc_slice_setup(arc: &XArc, mode: ArcMode) -> ArcSlice {
    let mut slice = ArcSlice::default();
    let mut angle1 = arc.angle1;
    let mut angle2;
    if arc.angle2 < 0 {
        angle2 = angle1;
        angle1 += arc.angle2;
    } else {
        angle2 = angle1 + arc.angle2;
    }
    angle1 = angle1.rem_euclid(FULLCIRCLE);
    angle2 = angle2.rem_euclid(FULLCIRCLE);
    slice.min_top_y = 0;
    slice.max_top_y = arc.height >> 1;
    slice.min_bot_y = 1 - (arc.height & 1);
    slice.max_bot_y = slice.max_top_y - 1;
    slice.flip_top = false;
    slice.flip_bot = false;
    if mode == ArcMode::PieSlice {
        slice.edge1_top = angle1 < HALFCIRCLE;
        slice.edge2_top = angle2 <= HALFCIRCLE;
        if angle2 == 0 || angle1 == HALFCIRCLE {
            if if angle2 != 0 { slice.edge2_top } else { slice.edge1_top } {
                slice.min_top_y = slice.min_bot_y;
            } else {
                slice.min_top_y = arc.height;
            }
            slice.min_bot_y = 0;
        } else if angle1 == 0 || angle2 == HALFCIRCLE {
            slice.min_top_y = slice.min_bot_y;
            if if angle1 != 0 { slice.edge1_top } else { slice.edge2_top } {
                slice.min_bot_y = arc.height;
            } else {
                slice.min_bot_y = 0;
            }
        } else if slice.edge1_top == slice.edge2_top {
            if angle2 < angle1 {
                slice.flip_top = slice.edge1_top;
                slice.flip_bot = !slice.edge1_top;
            } else if slice.edge1_top {
                slice.min_top_y = 1;
                slice.min_bot_y = arc.height;
            } else {
                slice.min_bot_y = 0;
                slice.min_top_y = arc.height;
            }
        }
        let (t1, t2) = (slice.edge1_top, slice.edge2_top);
        get_pie_edge(arc, angle1, &mut slice.edge1, t1, !t1);
        get_pie_edge(arc, angle2, &mut slice.edge2, t2, t2);
    } else {
        let w2 = arc.width as f64 / 2.0;
        let h2 = arc.height as f64 / 2.0;
        let (mut x1, mut y1, mut x2, mut y2);
        let (mut is_int1, mut is_int2) = (true, true);
        if angle1 == 0 || angle1 == HALFCIRCLE {
            x1 = if angle1 != 0 { -w2 } else { w2 };
            y1 = 0.0;
        } else if angle1 == QUADRANT || angle1 == QUADRANT3 {
            x1 = 0.0;
            y1 = if angle1 == QUADRANT { h2 } else { -h2 };
        } else {
            is_int1 = false;
            x1 = dcos(angle1) * w2;
            y1 = dsin(angle1) * h2;
        }
        if angle2 == 0 || angle2 == HALFCIRCLE {
            x2 = if angle2 != 0 { -w2 } else { w2 };
            y2 = 0.0;
        } else if angle2 == QUADRANT || angle2 == QUADRANT3 {
            x2 = 0.0;
            y2 = if angle2 == QUADRANT { h2 } else { -h2 };
        } else {
            is_int2 = false;
            x2 = dcos(angle2) * w2;
            y2 = dsin(angle2) * h2;
        }
        let mut dx = x2 - x1;
        let mut dy = y2 - y1;
        if arc.height & 1 != 0 {
            y1 -= 0.5;
            y2 -= 0.5;
        }
        if arc.width & 1 != 0 {
            x1 += 0.5;
            x2 += 0.5;
        }
        let signdy = if dy < 0.0 {
            dy = -dy;
            -1
        } else {
            1
        };
        let signdx = if dx < 0.0 {
            dx = -dx;
            -1
        } else {
            1
        };
        if is_int1 && is_int2 {
            slice.edge1.dx = (dx * 2.0) as i32;
            slice.edge1.dy = (dy * 2.0) as i32;
        } else {
            let scale = if dx > dy { dx } else { dy };
            slice.edge1.dx = ((dx * 32768.0) / scale + 0.5).floor() as i32;
            slice.edge1.dy = ((dy * 32768.0) / scale + 0.5).floor() as i32;
        }
        if slice.edge1.dy == 0 {
            if signdx < 0 {
                let y = (y1 + 1.0).floor() as i32;
                if y >= 0 {
                    slice.min_top_y = y;
                    slice.min_bot_y = arc.height;
                } else {
                    slice.max_bot_y = -y - (arc.height & 1);
                }
            } else {
                let y = y1.floor() as i32;
                if y >= 0 {
                    slice.max_top_y = y;
                } else {
                    slice.min_top_y = arc.height;
                    slice.min_bot_y = -y - (arc.height & 1);
                }
            }
            slice.edge1_top = true;
            slice.edge1.x = 65536;
            slice.edge1.stepx = 0;
            slice.edge1.e = 0;
            slice.edge1.dx = -1;
            slice.edge2 = slice.edge1;
            slice.edge2_top = false;
        } else if slice.edge1.dx == 0 {
            if signdy < 0 {
                x1 -= 1.0;
            }
            slice.edge1.x = x1.ceil() as i32;
            slice.edge1_top = signdy < 0;
            slice.edge1.x += arc.x + (arc.width >> 1);
            slice.edge1.stepx = 0;
            slice.edge1.e = 0;
            slice.edge1.dx = -1;
            slice.edge2_top = !slice.edge1_top;
            slice.edge2 = slice.edge1;
        } else {
            if signdx < 0 {
                slice.edge1.dx = -slice.edge1.dx;
            }
            if signdy < 0 {
                slice.edge1.dx = -slice.edge1.dx;
            }
            let k = (((x1 + x2) * slice.edge1.dy as f64 - (y1 + y2) * slice.edge1.dx as f64) / 2.0).ceil() as i32;
            slice.edge2.dx = slice.edge1.dx;
            slice.edge2.dy = slice.edge1.dy;
            slice.edge1_top = signdy < 0;
            slice.edge2_top = !slice.edge1_top;
            let (t1, t2) = (slice.edge1_top, slice.edge2_top);
            get_arc_edge(arc, &mut slice.edge1, k, t1, !t1);
            get_arc_edge(arc, &mut slice.edge2, k, t2, t2);
        }
    }
    slice
}

/// ADDSPAN: from l to r inclusive, if any.
fn add_span(out: &mut Vec<Span>, l: i32, r: i32, ya: i32) {
    if r >= l {
        out.push(span(l, ya, r.wrapping_sub(l).wrapping_add(1)));
    }
}

/// ADDSLICESPANS.
fn add_slice_spans<T>(out: &mut Vec<Span>, f: &Fill<T>, flip: bool, xl: i32, xr: i32, slw: i32, ya: i32) {
    if !flip {
        add_span(out, xl, xr, ya);
    } else {
        let xc = f.xorg - f.x;
        add_span(out, xc, xr, ya);
        add_span(out, xl, xc + slw - 1, ya);
    }
}

/// miFillArcSliceI and miFillArcSliceD.
fn fill_arc_slice<T: Num>(arc: &XArc, info: &FillArc<T>, mode: ArcMode, out: &mut Vec<Span>) {
    let mut slice = fill_arc_slice_setup(arc, mode);
    let mut f = Fill::new(info);
    while f.y > 0 {
        let slw = f.step();
        slice.edge1.step();
        slice.edge2.step();
        let y = f.y;
        if y >= slice.min_top_y && y <= slice.max_top_y {
            // MIARCSLICEUPPER.
            let ya = f.yorg - y;
            let mut xl = f.xorg - f.x;
            let mut xr = xl + slw - 1;
            if slice.edge1_top && slice.edge1.x < xr {
                xr = slice.edge1.x;
            }
            if slice.edge2_top && slice.edge2.x > xl {
                xl = slice.edge2.x;
            }
            add_slice_spans(out, &f, slice.flip_top, xl, xr, slw, ya);
        }
        if y >= slice.min_bot_y && y <= slice.max_bot_y {
            // MIARCSLICELOWER.
            let ya = f.yorg + y + f.dy;
            let mut xl = f.xorg - f.x;
            let mut xr = xl + slw - 1;
            if !slice.edge1_top && slice.edge1.x > xl {
                xl = slice.edge1.x;
            }
            if !slice.edge2_top && slice.edge2.x < xr {
                xr = slice.edge2.x;
            }
            add_slice_spans(out, &f, slice.flip_bot, xl, xr, slw, ya);
        }
    }
}

/// miPolyFillArc (fbPolyFillArc is it).
pub fn poly_fill_arc(sink: &mut dyn Sink, gc: &Gc, arcs: &[Arc]) {
    let mut spans = Vec::new();
    for a in arcs {
        let arc = xarc(a);
        if fill_arc_empty(&arc) {
            continue;
        }
        let whole = arc.angle2 >= FULLCIRCLE || arc.angle2 <= -FULLCIRCLE;
        match (whole, can_fill_arc(&arc)) {
            (true, true) => fill_ellipse(&fill_arc_setup(&arc), &mut spans),
            (true, false) => fill_ellipse(&fill_arc_d_setup(&arc), &mut spans),
            (false, true) => fill_arc_slice(&arc, &fill_arc_setup(&arc), gc.arc_mode, &mut spans),
            (false, false) => fill_arc_slice(&arc, &fill_arc_d_setup(&arc), gc.arc_mode, &mut spans),
        }
    }
    if !spans.is_empty() {
        sink.fill_spans(gc.fg, &spans);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mi::xvfb::{check, Op};

    const SIZES: &[(i32, i32)] = &[
        (0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (0, 6), (6, 0), (1, 6), (1, 7), (7, 1), (2, 9), (9, 2),
        (10, 10), (11, 11), (16, 16), (17, 17), (20, 13), (13, 20), (21, 14), (40, 40), (41, 41),
        (64, 30), (31, 70), (100, 60), (101, 61), (140, 100),
    ];

    const ANGLES: &[(i32, i32)] = &[
        (0, 360 * 64), (0, 90 * 64), (0, 180 * 64), (45 * 64, 90 * 64), (30 * 64, 100 * 64), (30 * 64, -100 * 64),
        (100 * 64, 200 * 64), (200 * 64, 120 * 64), (280 * 64, 70 * 64), (350 * 64, 20 * 64), (-45 * 64, 30 * 64),
        (-400 * 64, 50 * 64), (200 * 64, 400 * 64), (0, -360 * 64), (90 * 64, -360 * 64), (0, 1), (100 * 64, 1),
        (10 * 64, -1), (45 * 64, 0), (44 * 64, 2 * 64), (10 * 64, 32767), (-32768, 32767), (32767, -32768),
        (90 * 64, 90 * 64), (180 * 64, 90 * 64), (270 * 64, 90 * 64), (135 * 64, 180 * 64), (46 * 64, 358 * 64),
        (0, 270 * 64), (180 * 64, 180 * 64), (90 * 64, 180 * 64), (270 * 64, 180 * 64), (10 * 64, 340 * 64), (190 * 64, 340 * 64),
        (170 * 64, 20 * 64), (20 * 64, 140 * 64), (200 * 64, 140 * 64), (0, 359 * 64 + 63),
    ];

    fn both(name: &str, gc: &Gc, arcs: Vec<Arc>) {
        let op = Op::FillArcs(arcs);
        for arc_mode in [ArcMode::Chord, ArcMode::PieSlice] {
            for function in [3, 6] {
                check(&format!("{name} {arc_mode:?} gx{function}"), &Gc { function, arc_mode, ..gc.clone() }, &op);
            }
        }
    }

    fn arc(x: i32, y: i32, w: i32, h: i32, a1: i32, a2: i32) -> Arc {
        Arc { x, y, w, h, angle1: a1, angle2: a2 }
    }

    #[test]
    fn inside() {
        let gc = Gc { fg: 0xff_ffff, ..Gc::default() };
        for &(w, h) in SIZES {
            for &(a1, a2) in ANGLES {
                both(&format!("{w}x{h} {a1} {a2}"), &gc, vec![arc(5, 3, w, h, a1, a2)]);
            }
        }
    }

    #[test]
    fn large() {
        // Bigger than the buffer, partly outside; past 800 the double variant.
        let gc = Gc { fg: 0xff_ffff, ..Gc::default() };
        for &(x, y, w, h) in &[
            (-20, 10, 60, 60), (130, 100, 61, 40), (-100, -100, 300, 300), (-50, 20, 299, 150), (-300, -200, 700, 500),
            (-5, -5, 800, 800), (-5, -5, 800, 799), (-300, -400, 799, 800), (-400, -300, 801, 700), (-700, -10, 1500, 140), (-10, -900, 170, 1801), (-1000, -1000, 2000, 2000),
            (-2000, -1500, 4001, 3000),
        ] {
            for &(a1, a2) in ANGLES {
                both(&format!("{x},{y} {w}x{h} {a1} {a2}"), &gc, vec![arc(x, y, w, h, a1, a2)]);
            }
        }
    }

    #[test]
    fn wrapping() {
        // Span origins past 16 bits wrap, as in X.org's DDXPointRecs.
        let gc = Gc { fg: 0xff_ffff, ..Gc::default() };
        for &(x, y, w, h) in &[(32767, 10, 65535, 100), (32767, 32767, 65535, 65535), (32000, -32768, 65535, 65535), (-32768, -32768, 65535, 65535), (32767, 20, 700, 50)] {
            for &(a1, a2) in &[(0, 360 * 64), (10 * 64, 200 * 64), (100 * 64, -300 * 64), (0, 180 * 64)] {
                both(&format!("{x},{y} {w}x{h} {a1} {a2}"), &gc, vec![arc(x, y, w, h, a1, a2)]);
            }
        }
    }

    #[test]
    fn several() {
        let gc = Gc { fg: 0xff_ffff, ..Gc::default() };
        both("overlapping", &gc, vec![arc(10, 10, 50, 50, 0, 360 * 64), arc(20, 15, 50, 40, 30 * 64, 200 * 64), arc(-10, 5, 80, 80, 0, 90 * 64), arc(30, 30, 0, 0, 0, 64)]);
    }

    /// A sink that only counts.
    struct Count(u64);

    impl Sink for Count {
        fn fill_spans(&mut self, _pixel: u32, spans: &[Span]) {
            self.0 += spans.len() as u64;
        }
    }

    /// Any input: no panics, no hangs.
    #[test]
    fn any_input() {
        let mut seed = 0x0fed_cba9_8765_4321u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        let pick = |r: u32, v: &[i32]| v[r as usize % v.len()];
        let dims = [0, 1, 2, 3, 799, 800, 801, 1000, 32767, 32768, 65534, 65535];
        let coords = [-32768, -32767, -1, 0, 1, 100, 32766, 32767];
        let angles = [-32768, -23040, -1, 0, 1, 2880, 5760, 11520, 17280, 23040, 23041, 32767];
        for arc_mode in [ArcMode::Chord, ArcMode::PieSlice] {
            let gc = Gc { arc_mode, ..Gc::default() };
            for _ in 0..300 {
                let w = if next() & 1 == 0 { pick(next(), &dims) } else { (next() % 65536) as i32 };
                let h = if next() & 3 == 0 { w } else if next() & 1 == 0 { pick(next(), &dims) } else { (next() % 65536) as i32 };
                let a = arc(pick(next(), &coords), pick(next(), &coords), w, h, pick(next(), &angles), pick(next(), &angles));
                poly_fill_arc(&mut Count(0), &gc, &[a, a]);
            }
        }
    }
}
