//! Thin lines, points and rectangle outlines, as Xvfb draws them: fb's
//! fbPolyLine, fbPolySegment, fbSegment and the Bresenham loops in fbseg.c
//! and fbbits.h (POLYLINE, POLYSEGMENT, BRESDASH), fbPolyPoint (fbpoint.c),
//! with miline.h's octants and bias, and mi's miPolyRectangle
//! (mipolyrect.c), ported. COPYING in this directory has their notices.
//!
//! fb rather than mi's miZeroLine and miZeroDashLine: Xvfb draws thin lines
//! with fb, and its pixels are the reference.

use super::*;

// miline.h's octant bits.
const XDECREASING: u32 = 4;
const YDECREASING: u32 = 2;
const YMAJOR: u32 = 1;

/// The thin line bias (miGetZeroLineBias): which octants step along the
/// major axis on a tie. miScreenInit sets DEFAULTZEROLINEBIAS (octants 2 to
/// 5), but Xvfb draws as if none were set, in every octant: we follow Xvfb.
const BIAS: u32 = 0;

/// Pixels to spans: a run on one row becomes one span, and spans go to the
/// sink in the order drawn, a batch at a time.
struct Pen<'a> {
    sink: &'a mut dyn Sink,
    pixel: u32,
    spans: Vec<Span>,
}

impl<'a> Pen<'a> {
    fn new(sink: &'a mut dyn Sink, pixel: u32) -> Self {
        Pen {
            sink,
            pixel,
            spans: Vec::new(),
        }
    }

    fn plot(&mut self, pixel: u32, x: i64, y: i64) {
        if pixel != self.pixel {
            self.flush();
            self.pixel = pixel;
        }
        let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y)) else {
            return;
        };
        if let Some(s) = self.spans.last_mut() {
            if s.y == y && s.w < i32::MAX {
                if s.x.checked_add(s.w) == Some(x) {
                    s.w += 1;
                    return;
                }
                if s.x.checked_sub(1) == Some(x) {
                    s.x = x;
                    s.w += 1;
                    return;
                }
            }
        }
        if self.spans.len() >= 1024 {
            self.flush();
        }
        self.spans.push(Span { x, y, w: 1 });
    }

    fn flush(&mut self) {
        if !self.spans.is_empty() {
            self.sink.fill_spans(self.pixel, &self.spans);
            self.spans.clear();
        }
    }
}

impl Drop for Pen<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

/// The GC's dashes as fb keeps them: an odd list doubled (as SetDashes does),
/// and their total (fbValidateGC's dashLength).
struct Dashes {
    list: Vec<u8>,
    total: u32,
    double: bool,
}

impl Dashes {
    fn new(gc: &Gc) -> Self {
        let mut list = gc.dashes.clone();
        if list.len() % 2 == 1 {
            list.extend_from_within(..);
        }
        let total = list.iter().map(|&d| d as u32).sum();
        Dashes {
            list,
            total,
            double: gc.line_style == LineStyle::DoubleDash,
        }
    }
}

/// fbSegment and the Bresenham loop it runs (fbBresSolid, fbBresDash,
/// fbBresFill, fbBresFillDash: the same pixels), unclipped: the sink clips.
/// Returns the dash offset for the next segment.
#[allow(clippy::too_many_arguments)]
fn segment(
    pen: &mut Pen,
    gc: &Gc,
    dashes: Option<&Dashes>,
    x1: i64,
    y1: i64,
    x2: i64,
    y2: i64,
    draw_last: bool,
    dash_offset: i32,
) -> i32 {
    // CalcLineDeltas.
    let mut octant = 0;
    let (mut adx, mut sx) = (x2 - x1, 1);
    if adx < 0 {
        adx = -adx;
        sx = -1;
        octant |= XDECREASING;
    }
    let (mut ady, mut sy) = (y2 - y1, 1);
    if ady < 0 {
        ady = -ady;
        sy = -1;
        octant |= YDECREASING;
    }
    let x_axis = adx > ady;
    let (major, minor) = if x_axis { (adx, ady) } else { (ady, adx) };
    if !x_axis {
        octant |= YMAJOR;
    }
    let e1 = minor << 1;
    let e3 = -(major << 1);
    // FIXUP_ERROR, then the error term made to compare against zero.
    let mut e = -major - ((BIAS >> octant) & 1) as i64;
    let mut len = major + draw_last as i64;
    let next = dash_offset.wrapping_add(len as i32);

    // FbDashInit (dashOffset %= dashLength is unsigned in C).
    let mut dash = 0;
    let mut dashlen = 0i64;
    let mut even = true;
    if let Some(d) = dashes {
        if d.total == 0 {
            return next;
        }
        let mut off = ((dash_offset as u32) % d.total) as i64;
        loop {
            dashlen = d.list[dash] as i64;
            if off < dashlen {
                break;
            }
            off -= dashlen;
            even = !even;
            dash = (dash + 1) % d.list.len();
        }
        dashlen -= off;
    }

    let (mut x, mut y) = (x1, y1);
    while len > 0 {
        len -= 1;
        match dashes {
            None => pen.plot(gc.fg, x, y),
            Some(_) if even => pen.plot(gc.fg, x, y),
            Some(d) if d.double => pen.plot(gc.bg, x, y),
            Some(_) => {}
        }
        if x_axis {
            x += sx;
        } else {
            y += sy;
        }
        e += e1;
        if e >= 0 {
            if x_axis {
                y += sy;
            } else {
                x += sx;
            }
            e += e3;
        }
        // FbDashStep.
        if let Some(d) = dashes {
            dashlen -= 1;
            if dashlen == 0 {
                dash = (dash + 1) % d.list.len();
                dashlen = d.list[dash] as i64;
                even = !even;
            }
        }
    }
    next
}

/// Points as shorts, CoordModePrevious ones made absolute in 16 bits, as
/// fbFixCoordModePrevious and fbPolyPoint do it in place.
fn shorts(mode: CoordMode, pts: &[Point]) -> Vec<(i16, i16)> {
    let mut out: Vec<(i16, i16)> = pts.iter().map(|p| (p.x as i16, p.y as i16)).collect();
    if mode == CoordMode::Previous {
        for i in 1..out.len() {
            out[i].0 = out[i].0.wrapping_add(out[i - 1].0);
            out[i].1 = out[i].1.wrapping_add(out[i - 1].1);
        }
    }
    out
}

/// fbPolyPoint: one pixel a point.
pub fn poly_point(sink: &mut dyn Sink, gc: &Gc, mode: CoordMode, pts: &[Point]) {
    let mut pen = Pen::new(sink, gc.fg);
    for (x, y) in shorts(mode, pts) {
        pen.plot(gc.fg, x as i64, y as i64);
    }
}

/// fbbits.h's coordToInt and isClipped: a point outside the clip box, the
/// two 16-bit halves compared at once as fb does.
fn is_clipped(p: (i16, i16), ul: u32, lr: u32) -> bool {
    let c = ((p.1 as u16 as u32) << 16) | p.0 as u16 as u32;
    (c.wrapping_sub(ul) | lr.wrapping_sub(c)) & 0x8000_8000 != 0
}

fn coord_to_int(x: i32, y: i32) -> u32 {
    ((y as u16 as u32) << 16) | x as u16 as u32
}

/// Solid thin PolyLine, as fbPolyLine draws it.
pub fn zero_line(sink: &mut dyn Sink, gc: &Gc, mode: CoordMode, pts: &[Point]) {
    let clip = sink.clip_extents();
    zero_line_in(sink, gc, mode, pts, clip)
}

/// [`zero_line`] for a drawable whose clip is the box `clip` (None: every
/// point inside it). Which lines the clip cuts changes one pixel: POLYLINE
/// leaves out the last point of a closed line, unless fbSegment drew its
/// last segment because it crossed the clip.
pub(crate) fn zero_line_in(
    sink: &mut dyn Sink,
    gc: &Gc,
    mode: CoordMode,
    pts: &[Point],
    clip: Option<Rect>,
) {
    // ProcPolyLine draws nothing for fewer than two points.
    if pts.len() < 2 {
        return;
    }
    let cap = gc.cap_style != CapStyle::NotLast;
    let mut pen = Pen::new(sink, gc.fg);
    if gc.fill_style != FillStyle::Solid {
        // fbZeroLine, with fbBresFill.
        zero_line_segments(&mut pen, gc, None, mode, pts);
        return;
    }
    // POLYLINE (fbPolyline32): the points in 16 bits; segments within the
    // clip drawn there, the others by fbSegment.
    let p = shorts(mode, pts);
    let (ul, lr) = match clip {
        Some(c) => (
            coord_to_int(c.x, c.y),
            coord_to_int(c.x + c.w - 1, c.y + c.h - 1),
        ),
        None => (0, 0x7fff_7fff),
    };
    let n = p.len();
    for i in 1..n {
        let (a, b) = (p[i - 1], p[i]);
        let last = i == n - 1;
        let draw_last = if clip.is_some() && (is_clipped(a, ul, lr) || is_clipped(b, ul, lr)) {
            last && cap
        } else {
            last && cap && b != p[0]
        };
        segment(
            &mut pen, gc, None, a.0 as i64, a.1 as i64, b.0 as i64, b.1 as i64, draw_last, 0,
        );
    }
}

/// fbZeroLine: each segment by fbSegment, the points added up in ints, the
/// dash offset carried from one segment to the next.
fn zero_line_segments(
    pen: &mut Pen,
    gc: &Gc,
    dashes: Option<&Dashes>,
    mode: CoordMode,
    pts: &[Point],
) {
    let cap = gc.cap_style != CapStyle::NotLast;
    let (mut x1, mut y1) = (pts[0].x as i16 as i32, pts[0].y as i16 as i32);
    let mut dash_offset = gc.dash_offset as i32;
    for (i, p) in pts.iter().enumerate().skip(1) {
        let (mut x2, mut y2) = (p.x as i16 as i32, p.y as i16 as i32);
        if mode == CoordMode::Previous {
            x2 = x2.wrapping_add(x1);
            y2 = y2.wrapping_add(y1);
        }
        let draw_last = i == pts.len() - 1 && cap;
        dash_offset = segment(
            pen,
            gc,
            dashes,
            x1 as i64,
            y1 as i64,
            x2 as i64,
            y2 as i64,
            draw_last,
            dash_offset,
        );
        (x1, y1) = (x2, y2);
    }
}

/// Dashed thin PolyLine (OnOffDash, DoubleDash): fbZeroLine with
/// fbBresDash or fbBresFillDash.
pub fn zero_dash_line(sink: &mut dyn Sink, gc: &Gc, mode: CoordMode, pts: &[Point]) {
    if pts.len() < 2 {
        return;
    }
    let dashes = Dashes::new(gc);
    let mut pen = Pen::new(sink, gc.fg);
    zero_line_segments(&mut pen, gc, Some(&dashes), mode, pts);
}

/// Solid thin PolySegment: fbPolySegment (POLYSEGMENT, fbZeroSegment).
pub fn zero_segments(sink: &mut dyn Sink, gc: &Gc, segs: &[Segment]) {
    let draw_last = gc.cap_style != CapStyle::NotLast;
    let mut pen = Pen::new(sink, gc.fg);
    for s in segs {
        let c = |v: i32| v as i16 as i64;
        segment(
            &mut pen,
            gc,
            None,
            c(s.x1),
            c(s.y1),
            c(s.x2),
            c(s.y2),
            draw_last,
            0,
        );
    }
}

/// miPolyRectangle.
pub fn poly_rectangle(sink: &mut dyn Sink, gc: &Gc, rects: &[Rect]) {
    let clip = sink.clip_extents();
    poly_rectangle_in(sink, gc, rects, clip)
}

/// [`poly_rectangle`] for a drawable clipped to `clip` (see [`zero_line_in`]).
pub(crate) fn poly_rectangle_in(sink: &mut dyn Sink, gc: &Gc, rects: &[Rect], clip: Option<Rect>) {
    // Polylines, as fbPolyLine dispatches it.
    let lines = |sink: &mut dyn Sink, pts: &[Point]| match (gc.line_width, gc.line_style) {
        (0, LineStyle::Solid) => zero_line_in(sink, gc, CoordMode::Origin, pts, clip),
        _ => super::poly_line(sink, gc, CoordMode::Origin, pts),
    };
    // The rectangle's fields as the protocol has them.
    let field = |r: &Rect| {
        (
            r.x as i16 as i32,
            r.y as i16 as i32,
            r.w as u16 as i32,
            r.h as u16 as i32,
        )
    };
    let min_bound = |v: i32| v.max(-32768);
    let max_bound = |v: i32| v.min(32767);
    let max_ubound = |v: i32| v.min(65535);

    if gc.line_style == LineStyle::Solid && gc.join_style == JoinStyle::Miter && gc.line_width != 0
    {
        // Mitred solid outlines: up to four filled rectangles each.
        let offset2 = gc.line_width.min(65535) as i32;
        let offset1 = offset2 >> 1;
        let offset3 = offset2 - offset1;
        let mut out = Vec::new();
        for r in rects {
            let (x, y, width, height) = field(r);
            if width == 0 && height == 0 {
                let p = Point { x, y };
                lines(sink, &[p, p]);
            } else if height < offset2 || width < offset1 {
                let (tx, tw) = if height == 0 {
                    (x, width)
                } else {
                    (min_bound(x - offset1), max_ubound(width + offset2))
                };
                let (ty, th) = if width == 0 {
                    (y, height)
                } else {
                    (min_bound(y - offset1), max_ubound(height + offset2))
                };
                out.push(Rect {
                    x: tx,
                    y: ty,
                    w: tw,
                    h: th,
                });
            } else {
                out.push(Rect {
                    x: min_bound(x - offset1),
                    y: min_bound(y - offset1),
                    w: max_ubound(width + offset2),
                    h: offset2,
                });
                out.push(Rect {
                    x: min_bound(x - offset1),
                    y: max_bound(y + offset3),
                    w: offset2,
                    h: height - offset2,
                });
                out.push(Rect {
                    x: max_bound(x + width - offset1),
                    y: max_bound(y + offset3),
                    w: offset2,
                    h: height - offset2,
                });
                out.push(Rect {
                    x: min_bound(x - offset1),
                    y: max_bound(y + height - offset1),
                    w: max_ubound(width + offset2),
                    h: offset2,
                });
            }
        }
        // PolyFillRect, as xRectangles: the fields are 16-bit.
        for r in &mut out {
            (r.x, r.y, r.w, r.h) = field(r);
        }
        sink.fill_rects(gc.fg, &out);
    } else {
        for r in rects {
            let (x, y, width, height) = field(r);
            let x1 = max_bound(x + width);
            let y1 = max_bound(y + height);
            let p = [
                Point { x, y },
                Point { x: x1, y },
                Point { x: x1, y: y1 },
                Point { x, y: y1 },
                Point { x, y },
            ];
            lines(sink, &p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::xvfb::{self, Op, check};
    use super::*;

    fn pt(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    fn seg(x1: i32, y1: i32, x2: i32, y2: i32) -> Segment {
        Segment { x1, y1, x2, y2 }
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    /// As [`check`], ours drawn knowing the drawable's clip box, as fb does.
    fn check_in(name: &str, gc: &Gc, op: &Op) {
        let Some(want) = xvfb::xorg(gc, op) else {
            return;
        };
        let mut b = xvfb::Buffer {
            px: vec![xvfb::GROUND; (xvfb::W * xvfb::H) as usize],
            function: gc.function,
        };
        let clip = Some(rect(0, 0, xvfb::W, xvfb::H));
        match op {
            Op::Lines(m, p) if gc.line_width == 0 && gc.line_style == LineStyle::Solid => {
                zero_line_in(&mut b, gc, *m, p, clip)
            }
            Op::Rects(r) => poly_rectangle_in(&mut b, gc, r, clip),
            _ => {
                check(name, gc, op);
                return;
            }
        }
        let bad = (0..want.len()).filter(|&i| want[i] != b.px[i]).count();
        if bad != 0 {
            // Let check draw the picture (through the clip-less version).
            check(name, gc, op);
            panic!("{name}: {bad} pixels differ from X.org's (clipped)");
        }
    }

    fn gcs() -> Vec<Gc> {
        let mut v = Vec::new();
        for function in [3, 6] {
            for cap_style in [
                CapStyle::NotLast,
                CapStyle::Butt,
                CapStyle::Round,
                CapStyle::Projecting,
            ] {
                v.push(Gc {
                    function,
                    cap_style,
                    fg: 0xff_ffff,
                    bg: 0x12_3456,
                    ..Gc::default()
                });
            }
        }
        v
    }

    /// Lines from a centre to points all round it, every octant and the axes.
    fn star(cx: i32, cy: i32, r: i32) -> Vec<(i32, i32)> {
        let mut v = Vec::new();
        for i in -r..=r {
            v.push((cx + i, cy - r));
            v.push((cx + i, cy + r));
            v.push((cx - r, cy + i));
            v.push((cx + r, cy + i));
        }
        v
    }

    #[test]
    fn points() {
        for gc in gcs() {
            let p = vec![
                pt(1, 1),
                pt(5, 5),
                pt(5, 5),
                pt(5, 5),
                pt(-1, 3),
                pt(159, 119),
                pt(160, 0),
                pt(0, 120),
                pt(-32768, 32767),
                pt(80, 60),
            ];
            check("points", &gc, &Op::Points(CoordMode::Origin, p));
            let p = vec![
                pt(10, 10),
                pt(1, 0),
                pt(0, 1),
                pt(-2, -2),
                pt(0, 0),
                pt(32767, 0),
                pt(32767, 0),
                pt(3, 5),
            ];
            check("points previous", &gc, &Op::Points(CoordMode::Previous, p));
        }
    }

    #[test]
    fn lines_all_directions() {
        for gc in gcs() {
            for (x, y) in star(80, 60, 7) {
                check(
                    "line",
                    &gc,
                    &Op::Lines(CoordMode::Origin, vec![pt(80, 60), pt(x, y)]),
                );
                check(
                    "line back",
                    &gc,
                    &Op::Lines(CoordMode::Origin, vec![pt(x, y), pt(80, 60)]),
                );
            }
            for (x, y) in star(80, 60, 50).into_iter().step_by(7) {
                check(
                    "long line",
                    &gc,
                    &Op::Lines(CoordMode::Origin, vec![pt(80, 60), pt(x, y)]),
                );
            }
        }
    }

    #[test]
    fn segments_all_directions() {
        for gc in gcs() {
            let s: Vec<Segment> = star(40, 40, 9)
                .into_iter()
                .map(|(x, y)| seg(40, 40, x, y))
                .collect();
            check("segments", &gc, &Op::Segments(s));
            let s: Vec<Segment> = star(110, 70, 30)
                .into_iter()
                .step_by(3)
                .map(|(x, y)| seg(x, y, 110, 70))
                .collect();
            check("segments in", &gc, &Op::Segments(s));
            let s = vec![
                seg(5, 5, 5, 5),
                seg(5, 5, 5, 5),
                seg(10, 10, 20, 10),
                seg(20, 12, 10, 12),
                seg(10, 14, 12, 14),
                seg(30, 30, 30, 40),
                seg(31, 40, 31, 30),
            ];
            check("segments short", &gc, &Op::Segments(s));
            let s = vec![
                seg(-50, -30, 200, 150),
                seg(-10, 60, 300, 61),
                seg(80, -32768, 81, 32767),
                seg(-32768, -32768, 32767, 32767),
                seg(170, 5, 150, 6),
                seg(-5, 119, 3, 125),
            ];
            check("segments outside", &gc, &Op::Segments(s));
        }
    }

    #[test]
    fn polylines() {
        for gc in gcs() {
            let p = vec![pt(10, 10), pt(50, 20), pt(30, 70), pt(30, 70), pt(10, 10)];
            check("closed", &gc, &Op::Lines(CoordMode::Origin, p));
            let p = vec![pt(10, 10), pt(50, 20), pt(30, 70), pt(100, 100), pt(150, 3)];
            check("open", &gc, &Op::Lines(CoordMode::Origin, p));
            let p = vec![pt(20, 20), pt(20, 20)];
            check("zero length", &gc, &Op::Lines(CoordMode::Origin, p));
            let p = vec![pt(20, 20), pt(20, 20), pt(20, 20)];
            check("zero length twice", &gc, &Op::Lines(CoordMode::Origin, p));
            check(
                "one point",
                &gc,
                &Op::Lines(CoordMode::Origin, vec![pt(20, 20)]),
            );
            let p = vec![pt(20, 20), pt(30, 0), pt(0, 30), pt(-30, 0), pt(0, -30)];
            check("previous closed", &gc, &Op::Lines(CoordMode::Previous, p));
            let p = vec![pt(100, 50), pt(-3, 7), pt(11, -2), pt(0, 0), pt(-40, 20)];
            check("previous", &gc, &Op::Lines(CoordMode::Previous, p));
            let p = vec![pt(50, 50), pt(32767, 0), pt(32767, 0), pt(2, 10)];
            check_in("previous wraps", &gc, &Op::Lines(CoordMode::Previous, p));
            // Partly outside: the clip matters to the closing point.
            let p = vec![
                pt(-20, 30),
                pt(60, 10),
                pt(200, 90),
                pt(40, 140),
                pt(-20, 30),
            ];
            check_in("outside closed", &gc, &Op::Lines(CoordMode::Origin, p));
            let p = vec![
                pt(10, 30),
                pt(60, -10),
                pt(150, 50),
                pt(170, 100),
                pt(10, 30),
            ];
            check_in("outside closed 2", &gc, &Op::Lines(CoordMode::Origin, p));
            let p = vec![
                pt(-32768, -32768),
                pt(32767, 32767),
                pt(-32768, 32767),
                pt(32767, -32768),
            ];
            check_in("huge", &gc, &Op::Lines(CoordMode::Origin, p));
            let p = vec![
                pt(-1000, 7),
                pt(1000, 113),
                pt(80, -3000),
                pt(81, 3000),
                pt(-200, 60),
                pt(155, 61),
            ];
            check_in("through", &gc, &Op::Lines(CoordMode::Origin, p));
        }
    }

    #[test]
    fn dashes() {
        let patterns: [(&[u8], u32); 8] = [
            (&[4], 0),
            (&[3], 1),
            (&[1, 2, 3], 0),
            (&[1, 2, 3], 17),
            (&[5, 1], 3),
            (&[1], 0),
            (&[2, 3, 4, 5], 1000),
            (&[255, 1], 300),
        ];
        for (dashes, dash_offset) in patterns {
            for line_style in [LineStyle::OnOffDash, LineStyle::DoubleDash] {
                for gc in gcs() {
                    let gc = Gc {
                        line_style,
                        dashes: dashes.to_vec(),
                        dash_offset,
                        ..gc
                    };
                    let p = vec![
                        pt(10, 10),
                        pt(70, 20),
                        pt(30, 90),
                        pt(30, 90),
                        pt(150, 100),
                        pt(10, 10),
                    ];
                    check("dash", &gc, &Op::Lines(CoordMode::Origin, p));
                    let p = vec![
                        pt(-30, 50),
                        pt(200, 60),
                        pt(80, -100),
                        pt(80, 300),
                        pt(-32768, 0),
                        pt(32767, 119),
                    ];
                    check("dash outside", &gc, &Op::Lines(CoordMode::Origin, p));
                    let p = vec![
                        pt(80, 60),
                        pt(9, 2),
                        pt(-3, 11),
                        pt(-20, -20),
                        pt(0, 0),
                        pt(30, -1),
                    ];
                    check("dash previous", &gc, &Op::Lines(CoordMode::Previous, p));
                    let s: Vec<Segment> = star(80, 60, 25)
                        .into_iter()
                        .step_by(5)
                        .map(|(x, y)| seg(80, 60, x, y))
                        .collect();
                    check("dash segments", &gc, &Op::Segments(s));
                    check(
                        "dash rects",
                        &gc,
                        &Op::Rects(vec![
                            rect(5, 5, 30, 20),
                            rect(10, 10, 0, 0),
                            rect(100, 50, 0, 30),
                            rect(-10, 100, 200, 40),
                        ]),
                    );
                }
            }
        }
    }

    #[test]
    fn rectangles() {
        for gc in gcs() {
            let r = vec![
                rect(5, 5, 30, 20),
                rect(10, 10, 30, 20),
                rect(50, 50, 0, 0),
                rect(60, 50, 0, 10),
                rect(70, 50, 10, 0),
                rect(90, 90, 1, 1),
            ];
            check("rects", &gc, &Op::Rects(r));
            let r = vec![
                rect(-10, -10, 30, 30),
                rect(150, 100, 30, 30),
                rect(-5, 50, 200, 10),
                rect(30, -32768, 65535, 65535),
                rect(32767, 0, 10, 10),
            ];
            check_in("rects outside", &gc, &Op::Rects(r));
            // Mitred wide outlines are filled rectangles.
            for line_width in [1, 2, 3, 4, 7, 40] {
                let gc = Gc {
                    line_width,
                    ..gc.clone()
                };
                let r = vec![
                    rect(20, 20, 30, 20),
                    rect(30, 25, 30, 20),
                    rect(80, 10, 2, 50),
                    rect(100, 30, 50, 1),
                    rect(10, 90, 0, 20),
                    rect(30, 90, 20, 0),
                    rect(70, 80, 3, 3),
                    rect(-20, 110, 50, 50),
                ];
                check("wide rects", &gc, &Op::Rects(r));
            }
        }
    }

    #[test]
    fn not_solid_fill() {
        // Tiled thin lines go through fbZeroLine: the closing point is drawn.
        let gc = Gc {
            function: 6,
            fg: 0xff_ffff,
            fill_style: FillStyle::Tiled,
            ..Gc::default()
        };
        let line = [pt(1, 1), pt(5, 1), pt(1, 1)];
        let mut b = xvfb::Buffer {
            px: vec![0; (xvfb::W * xvfb::H) as usize],
            function: 6,
        };
        zero_line(&mut b, &gc, CoordMode::Origin, &line);
        assert_eq!(b.px.iter().filter(|&&p| p != 0).count(), 1);
        // Solid ones leave it out, so it's drawn once.
        let gc = Gc {
            fill_style: FillStyle::Solid,
            ..gc
        };
        check(
            "closed xor",
            &gc,
            &Op::Lines(CoordMode::Origin, line.to_vec()),
        );
        let mut b = xvfb::Buffer {
            px: vec![0; (xvfb::W * xvfb::H) as usize],
            function: 6,
        };
        zero_line(&mut b, &gc, CoordMode::Origin, &line);
        assert_eq!(b.px.iter().filter(|&&p| p != 0).count(), 2);
    }

    #[test]
    fn robust() {
        // Nothing panics or hangs on extreme input.
        let mut b = xvfb::Buffer {
            px: vec![0; (xvfb::W * xvfb::H) as usize],
            function: 6,
        };
        let gc = Gc {
            line_style: LineStyle::DoubleDash,
            dashes: vec![255, 1, 7],
            dash_offset: 65535,
            ..Gc::default()
        };
        let pts: Vec<Point> = (0..2000)
            .map(|i| pt(if i % 2 == 0 { 32767 } else { -32768 }, 32767))
            .collect();
        zero_dash_line(&mut b, &gc, CoordMode::Previous, &pts);
        zero_line(&mut b, &gc, CoordMode::Previous, &pts);
        poly_point(&mut b, &gc, CoordMode::Previous, &pts);
        let gc = Gc {
            line_width: 65535,
            ..Gc::default()
        };
        poly_rectangle(
            &mut b,
            &gc,
            &[
                rect(-32768, -32768, 65535, 65535),
                rect(32767, 32767, 65535, 65535),
            ],
        );
        zero_dash_line(
            &mut b,
            &Gc {
                line_style: LineStyle::OnOffDash,
                dashes: vec![],
                ..Gc::default()
            },
            CoordMode::Origin,
            &pts[..3],
        );
    }
}
