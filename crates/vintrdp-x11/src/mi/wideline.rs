//! Wide lines: X.org's mi/miwideline.c and mi/miwideline.h (miWideLine,
//! miWideDash, the span groups), with ICEIL from mi/mifpoly.h and a private
//! copy of miStepDash from mi/midash.c, ported. COPYING in this directory has
//! their notices.
//!
//! Integers are C ints, so arithmetic that can overflow wraps as X.org's
//! does in practice; doubles are doubles. Span and point coordinates are
//! 16-bit in X.org (DDXPointRec, xRectangle), so they're cut to 16 bits on
//! the way out, as there.

use super::*;

// miwideline.h's structures.

#[derive(Clone, Copy, Debug, Default)]
struct PolyEdge {
    height: i32,
    x: i32,
    stepx: i32,
    signdx: i32,
    e: i32,
    dy: i32,
    dx: i32,
}

#[derive(Clone, Copy, Debug, Default)]
struct PolyVertex {
    x: f64,
    y: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct PolySlope {
    dx: i32,
    dy: i32,
    k: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct LineFace {
    xa: f64,
    ya: f64,
    dx: i32,
    dy: i32,
    x: i32,
    y: i32,
    k: f64,
}

/// 1/sin^2(11/2): the miter limit.
const SQSECANT: f64 = 108.856472512142;

/// Rows one polygon may have: far more than any real one, so absurd input
/// can't hang us.
const MAX_ROWS: i32 = 1 << 20;

/// Whether a * b + c * d rounds once for the first product, as when the C
/// compiler contracts it to a fused multiply-add: clang does on arm64
/// (XQuartz's Xvfb, which the tests check against); x86-64 builds without
/// FMA don't (Ubuntu's, in CI). Compilers on arm64 contract by default, so
/// follow the target: off by a pixel here and there either way.
const FUSED: bool = cfg!(target_arch = "aarch64");

/// a * b + c * d, as X.org's compiled code computes it.
fn mul2_add(a: f64, b: f64, c: f64, d: f64) -> f64 {
    if FUSED { a.mul_add(b, c * d) } else { a * b + c * d }
}

/// a * b - c * d, likewise.
fn mul2_sub(a: f64, b: f64, c: f64, d: f64) -> f64 {
    if FUSED { a.mul_add(b, -(c * d)) } else { a * b - c * d }
}

/// ICEIL (mifpoly.h).
fn iceil(x: f64) -> i32 {
    let t = x as i32;
    if x == t as f64 || x < 0.0 { t } else { t.wrapping_add(1) }
}

/// C's int division, without the crash on zero.
fn div(a: i32, b: i32) -> i32 {
    if b == 0 { 0 } else { a.wrapping_div(b) }
}

fn rem(a: i32, b: i32) -> i32 {
    if b == 0 { 0 } else { a.wrapping_rem(b) }
}

/// A DDXPointRec coordinate.
fn s16(v: i32) -> i32 {
    v as i16 as i32
}

fn span(x: i32, y: i32, w: i32) -> Span {
    Span { x: s16(x), y: s16(y), w }
}

// Span groups: each pixel touched once, for functions where that matters.

struct SpanGroup {
    group: Vec<Vec<Span>>,
    ymin: i32,
    ymax: i32,
}

impl SpanGroup {
    /// miInitSpanGroup.
    fn new() -> Self {
        SpanGroup { group: Vec::new(), ymin: i16::MAX as i32, ymax: i16::MIN as i32 }
    }
}

/// miSubtractSpans: take `sub`'s pixels out of every span in the group.
fn subtract_spans(group: &mut SpanGroup, sub: &[Span]) {
    let ymin = sub[0].y;
    let ymax = sub[sub.len() - 1].y;
    for spans in group.group.iter_mut() {
        // X.org would read before the array of an emptied list; skip it.
        if spans.is_empty() {
            continue;
        }
        if !(spans[0].y <= ymax && ymin <= spans[spans.len() - 1].y) {
            continue;
        }
        let mut si = 0usize;
        let mut sub_count = sub.len();
        let mut pi = 0usize;
        let mut spans_count = spans.len();
        loop {
            while spans_count > 0 && spans[pi].y < sub[si].y {
                pi += 1;
                spans_count -= 1;
            }
            if spans_count == 0 {
                break;
            }
            while sub_count > 0 && sub[si].y < spans[pi].y {
                si += 1;
                sub_count -= 1;
            }
            if sub_count == 0 {
                break;
            }
            let mut advance = true;
            if sub[si].y == spans[pi].y {
                let xmin = sub[si].x;
                let xmax = xmin.wrapping_add(sub[si].w);
                let p = spans[pi];
                let pend = p.x.wrapping_add(p.w);
                if xmin >= pend || p.x >= xmax {
                } else if xmin <= p.x {
                    if xmax >= pend {
                        // All of it goes: the next one moves into its place.
                        spans.remove(pi);
                        advance = false;
                    } else {
                        spans[pi].w = p.w.wrapping_sub(xmax.wrapping_sub(p.x));
                        spans[pi].x = xmax;
                    }
                } else if xmax >= pend {
                    spans[pi].w = xmin.wrapping_sub(p.x);
                } else {
                    // A hole in the middle: split in two.
                    spans.insert(pi, p);
                    spans[pi].w = xmin.wrapping_sub(p.x);
                    pi += 1;
                    spans[pi].w = p.w.wrapping_sub(xmax.wrapping_sub(p.x));
                    spans[pi].x = xmax;
                }
            }
            if advance {
                pi += 1;
            }
            spans_count -= 1;
        }
    }
}

/// miAppendSpans.
fn append_spans(group: &mut SpanGroup, other: Option<&mut SpanGroup>, spans: Vec<Span>) {
    if spans.is_empty() {
        return;
    }
    let ymin = spans[0].y;
    let ymax = spans[spans.len() - 1].y;
    group.ymin = group.ymin.min(ymin);
    group.ymax = group.ymax.max(ymax);
    if let Some(o) = other.filter(|o| o.ymin < ymax && ymin < o.ymax) {
        subtract_spans(o, &spans);
    }
    group.group.push(spans);
}

/// miFillUniqueSpanGroup: sort by y and x, merge overlaps (UniquifySpansX)
/// and fill.
fn fill_unique_span_group(sink: &mut dyn Sink, pixel: u32, group: &mut SpanGroup) {
    if group.group.is_empty() {
        return;
    }
    if group.group.len() == 1 {
        sink.fill_spans(pixel, &group.group[0]);
    } else {
        let (ymin, ymax) = (group.ymin, group.ymax);
        let mut all: Vec<Span> = group.group.iter().flatten().filter(|s| s.y >= ymin && s.y <= ymax).copied().collect();
        // Within a row the merged result doesn't depend on the order of
        // equal x's, so any sort gives X.org's spans.
        all.sort_unstable_by_key(|s| (s.y, s.x));
        let mut out = Vec::with_capacity(all.len());
        let mut i = 0;
        while i < all.len() {
            let y = all[i].y;
            let mut x1 = all[i].x;
            let mut x2 = x1.wrapping_add(all[i].w);
            i += 1;
            while i < all.len() && all[i].y == y {
                let s = all[i];
                if s.x > x2 {
                    out.push(Span { x: x1, y, w: x2.wrapping_sub(x1) });
                    x1 = s.x;
                    x2 = s.x.wrapping_add(s.w);
                } else {
                    x2 = x2.max(s.x.wrapping_add(s.w));
                }
                i += 1;
            }
            out.push(Span { x: x1, y, w: x2.wrapping_sub(x1) });
        }
        sink.fill_spans(pixel, &out);
    }
    *group = SpanGroup::new();
}

/// SpanDataRec.
struct SpanData {
    fg: SpanGroup,
    bg: SpanGroup,
}

/// One wide line request: the drawable, the GC and, for functions that
/// mustn't touch a pixel twice, the span groups.
struct Wide<'a> {
    sink: &'a mut dyn Sink,
    gc: &'a Gc,
    lw: i32,
    data: Option<SpanData>,
}

/// miStepDash (midash.c): move `dist` along the dash list.
fn step_dash(dist: i32, index: &mut usize, dashes: &[u8], offset: &mut i32) {
    let mut dist = dist;
    let mut i = *index;
    let d = dashes[i] as i32;
    if dist < d - *offset {
        *offset += dist;
        return;
    }
    dist -= d - *offset;
    i += 1;
    if i == dashes.len() {
        i = 0;
    }
    let total: i32 = dashes.iter().map(|&d| d as i32).sum();
    if total <= dist {
        dist %= total;
    }
    while dist >= dashes[i] as i32 {
        dist -= dashes[i] as i32;
        i += 1;
        if i == dashes.len() {
            i = 0;
        }
    }
    *index = i;
    *offset = dist;
}

/// miPolyBuildEdge: an edge through (x0, y0) of slope dx/dy, k being
/// x0 * dy - y0 * dx. Returns its first y.
#[allow(clippy::too_many_arguments)]
fn poly_build_edge(_x0: f64, y0: f64, k: f64, dx: i32, dy: i32, xi: i32, yi: i32, left: bool, edge: &mut PolyEdge) -> i32 {
    let (mut k, mut dx, mut dy) = (k, dx, dy);
    if dy < 0 {
        dy = dy.wrapping_neg();
        dx = dx.wrapping_neg();
        k = -k;
    }
    let y = iceil(y0);
    let xady = iceil(k).wrapping_add(y.wrapping_mul(dx));
    let x = if xady <= 0 { div(xady.wrapping_neg(), dy).wrapping_neg().wrapping_sub(1) } else { div(xady.wrapping_sub(1), dy) };
    let mut e = xady.wrapping_sub(x.wrapping_mul(dy));
    if dx >= 0 {
        edge.signdx = 1;
        edge.stepx = div(dx, dy);
        edge.dx = rem(dx, dy);
    } else {
        edge.signdx = -1;
        edge.stepx = div(dx.wrapping_neg(), dy).wrapping_neg();
        edge.dx = rem(dx.wrapping_neg(), dy);
        e = dy.wrapping_sub(e).wrapping_add(1);
    }
    edge.dy = dy;
    edge.x = x.wrapping_add(left as i32).wrapping_add(xi);
    edge.e = e.wrapping_sub(dy);
    y.wrapping_add(yi)
}

/// StepAround.
fn step_around(v: usize, incr: i32, max: usize) -> usize {
    let n = v as i32 + incr;
    if n < 0 {
        max - 1
    } else if n as usize == max {
        0
    } else {
        n as usize
    }
}

/// miPolyBuildPoly: a convex polygon's left and right edges. Returns
/// (top y, left count, right count, height).
fn poly_build_poly(vertices: &[PolyVertex], slopes: &[PolySlope], xi: i32, yi: i32, left: &mut [PolyEdge; 4], right: &mut [PolyEdge; 4]) -> (i32, usize, usize, i32) {
    let count = vertices.len();
    let mut miny = vertices[0].y;
    let mut maxy = miny;
    let (mut top, mut bottom) = (0usize, 0usize);
    for (i, v) in vertices.iter().enumerate().skip(1) {
        if v.y < miny {
            top = i;
            miny = v.y;
        }
        if v.y >= maxy {
            bottom = i;
            maxy = v.y;
        }
    }
    let mut clockwise = 1;
    let mut slopeoff = 0;
    let i = top;
    let j = step_around(top, -1, count);
    if slopes[j].dy as i64 * slopes[i].dx as i64 > slopes[i].dy as i64 * slopes[j].dx as i64 {
        clockwise = -1;
        slopeoff = -1;
    }
    let bottomy = iceil(maxy).wrapping_add(yi);
    let (mut lasty, mut topy) = (0i32, 0i32);

    let mut nright = 0;
    let mut s = step_around(top, slopeoff, count);
    let mut i = top;
    while i != bottom {
        if slopes[s].dy != 0 {
            let y = poly_build_edge(vertices[i].x, vertices[i].y, slopes[s].k, slopes[s].dx, slopes[s].dy, xi, yi, false, &mut right[nright]);
            if nright != 0 {
                right[nright - 1].height = y.wrapping_sub(lasty);
            } else {
                topy = y;
            }
            nright += 1;
            lasty = y;
        }
        i = step_around(i, clockwise, count);
        s = step_around(s, clockwise, count);
    }
    if nright != 0 {
        right[nright - 1].height = bottomy.wrapping_sub(lasty);
    }

    let slopeoff = if slopeoff == 0 { -1 } else { 0 };
    let mut nleft = 0;
    let mut s = step_around(top, slopeoff, count);
    let mut i = top;
    while i != bottom {
        if slopes[s].dy != 0 {
            let y = poly_build_edge(vertices[i].x, vertices[i].y, slopes[s].k, slopes[s].dx, slopes[s].dy, xi, yi, true, &mut left[nleft]);
            if nleft != 0 {
                left[nleft - 1].height = y.wrapping_sub(lasty);
            }
            nleft += 1;
            lasty = y;
        }
        i = step_around(i, -clockwise, count);
        s = step_around(s, -clockwise, count);
    }
    if nleft != 0 {
        left[nleft - 1].height = bottomy.wrapping_sub(lasty);
    }
    (topy, nleft, nright, bottomy.wrapping_sub(topy))
}

/// One CLIPSTEPEDGE: clip a round cap or join's row to a face's edge.
fn clip_step_edge(ybase: i32, edgey: &mut i32, edge: &mut PolyEdge, edgeleft: bool, xcl: &mut i32, xcr: &mut i32) {
    if ybase == *edgey {
        if edgeleft {
            if edge.x > *xcl {
                *xcl = edge.x;
            }
        } else if edge.x < *xcr {
            *xcr = edge.x;
        }
        *edgey = edgey.wrapping_add(1);
        edge.x = edge.x.wrapping_add(edge.stepx);
        edge.e = edge.e.wrapping_add(edge.dx);
        if edge.e > 0 {
            edge.x = edge.x.wrapping_add(edge.signdx);
            edge.e = edge.e.wrapping_sub(edge.dy);
        }
    }
}

/// miRoundJoinFace.
fn round_join_face(face: &LineFace, edge: &mut PolyEdge) -> (i32, bool) {
    let mut dx = face.dy.wrapping_neg();
    let mut dy = face.dx;
    let (mut xa, mut ya) = (face.xa, face.ya);
    let mut left = true;
    if ya > 0.0 {
        ya = 0.0;
        xa = 0.0;
    }
    if dy < 0 || (dy == 0 && dx > 0) {
        dx = dx.wrapping_neg();
        dy = dy.wrapping_neg();
        left = !left;
    }
    if dx == 0 && dy == 0 {
        dy = 1;
    }
    let y;
    if dy == 0 {
        y = iceil(face.ya).wrapping_add(face.y);
        *edge = PolyEdge { x: -32767, stepx: 0, signdx: 0, e: -1, dy: 0, dx: 0, height: 0 };
    } else {
        y = poly_build_edge(xa, ya, 0.0, dx, dy, face.x, face.y, !left, edge);
        edge.height = 32767;
    }
    (y, !left)
}

/// miRoundCapClip.
fn round_cap_clip(face: &LineFace, is_int: bool, edge: &mut PolyEdge) -> (i32, bool) {
    let mut dx = face.dy.wrapping_neg();
    let mut dy = face.dx;
    let (mut xa, mut ya) = (face.xa, face.ya);
    let k = if is_int { 0.0 } else { face.k };
    let mut left = true;
    if dy < 0 || (dy == 0 && dx > 0) {
        dx = dx.wrapping_neg();
        dy = dy.wrapping_neg();
        xa = -xa;
        ya = -ya;
        left = !left;
    }
    if dx == 0 && dy == 0 {
        dy = 1;
    }
    let y;
    if dy == 0 {
        y = iceil(face.ya).wrapping_add(face.y);
        *edge = PolyEdge { x: -32767, stepx: 0, signdx: 0, e: -1, dy: 0, dx: 0, height: 0 };
    } else {
        y = poly_build_edge(xa, ya, k, dx, dy, face.x, face.y, !left, edge);
        edge.height = 32767;
    }
    (y, !left)
}

impl Wide<'_> {
    /// fillSpans: straight to the drawable, or into a span group.
    fn fill_spans(&mut self, pixel: u32, spans: Vec<Span>) {
        match &mut self.data {
            None => {
                if !spans.is_empty() {
                    self.sink.fill_spans(pixel, &spans);
                }
            }
            // AppendSpanGroup.
            Some(d) => {
                if pixel == self.gc.fg {
                    let other = if self.gc.line_style == LineStyle::DoubleDash { Some(&mut d.bg) } else { None };
                    append_spans(&mut d.fg, other, spans);
                } else {
                    append_spans(&mut d.bg, Some(&mut d.fg), spans);
                }
            }
        }
    }

    /// miFillPolyHelper: fill between left and right edge lists.
    fn fill_poly_helper(&mut self, pixel: u32, y: i32, overall_height: i32, left: &[PolyEdge], right: &[PolyEdge]) {
        if overall_height <= 0 {
            return;
        }
        let mut rows = overall_height.min(MAX_ROWS);
        let mut spans = Vec::with_capacity(rows as usize);
        let mut y = y;
        let (mut l, mut r) = (PolyEdge::default(), PolyEdge::default());
        let (mut lh, mut rh) = (0i32, 0i32);
        let (mut li, mut ri) = (0usize, 0usize);
        while (li < left.len() || lh != 0) && (ri < right.len() || rh != 0) {
            if lh == 0 && li < left.len() {
                l = left[li];
                lh = l.height;
                li += 1;
            }
            if rh == 0 && ri < right.len() {
                r = right[ri];
                rh = r.height;
                ri += 1;
            }
            let mut height = lh.min(rh);
            lh = lh.wrapping_sub(height);
            rh = rh.wrapping_sub(height);
            while height > 0 {
                height -= 1;
                if rows == 0 {
                    break;
                }
                rows -= 1;
                if r.x >= l.x {
                    spans.push(span(l.x, y, r.x.wrapping_sub(l.x).wrapping_add(1)));
                }
                y = y.wrapping_add(1);
                l.x = l.x.wrapping_add(l.stepx);
                l.e = l.e.wrapping_add(l.dx);
                if l.e > 0 {
                    l.x = l.x.wrapping_add(l.signdx);
                    l.e = l.e.wrapping_sub(l.dy);
                }
                r.x = r.x.wrapping_add(r.stepx);
                r.e = r.e.wrapping_add(r.dx);
                if r.e > 0 {
                    r.x = r.x.wrapping_add(r.signdx);
                    r.e = r.e.wrapping_sub(r.dy);
                }
            }
            if rows == 0 {
                break;
            }
        }
        self.fill_spans(pixel, spans);
    }

    /// miFillRectPolyHelper.
    fn fill_rect_poly_helper(&mut self, pixel: u32, x: i32, y: i32, w: i32, h: i32) {
        if self.data.is_none() {
            // PolyFillRect, through an xRectangle.
            let r = Rect { x: s16(x), y: s16(y), w: w as u16 as i32, h: h as u16 as i32 };
            self.sink.fill_rects(pixel, &[r]);
        } else {
            if h <= 0 {
                return;
            }
            let spans = (0..h).map(|i| span(x, y.wrapping_add(i), w)).collect();
            self.fill_spans(pixel, spans);
        }
    }

    /// miLineOnePoint.
    fn line_one_point(&mut self, pixel: u32, x: i32, y: i32) {
        self.sink.fill_spans(pixel, &[span(x, y, 1)]);
    }

    /// miLineJoin.
    fn line_join(&mut self, pixel: u32, pl: &mut LineFace, pr: &mut LineFace) {
        let mut join = self.gc.join_style;
        let lw = self.lw;
        let denom;
        let denom_of = |pl: &LineFace, pr: &LineFace| pl.dx.wrapping_neg() as f64 * pr.dy as f64 + pr.dx as f64 * pl.dy as f64;
        if lw == 1 && self.data.is_none() {
            // One of the lines may draw the joining pixel.
            if pl.dx > 0 || (pl.dx == 0 && pl.dy > 0) {
                return;
            }
            if pr.dx > 0 || (pr.dx == 0 && pr.dy > 0) {
                return;
            }
            if join != JoinStyle::Round {
                denom = denom_of(pl, pr);
                if denom == 0.0 {
                    return;
                }
            } else {
                denom = 0.0;
            }
            if join != JoinStyle::Miter {
                self.line_one_point(pixel, pl.x, pl.y);
                return;
            }
        } else {
            if join == JoinStyle::Round {
                self.line_arc(pixel, Some(pl), Some(pr), 0.0, 0.0, true);
                return;
            }
            denom = denom_of(pl, pr);
            if denom == 0.0 {
                return;
            }
        }

        let mut swapslopes = false;
        if denom > 0.0 {
            pl.xa = -pl.xa;
            pl.ya = -pl.ya;
            pl.dx = pl.dx.wrapping_neg();
            pl.dy = pl.dy.wrapping_neg();
        } else {
            swapslopes = true;
            pr.xa = -pr.xa;
            pr.ya = -pr.ya;
            pr.dx = pr.dx.wrapping_neg();
            pr.dy = pr.dy.wrapping_neg();
        }

        let mut vertices = [PolyVertex::default(); 4];
        let mut slopes = [PolySlope::default(); 4];
        vertices[0] = PolyVertex { x: pr.xa, y: pr.ya };
        slopes[0] = PolySlope { dx: pr.dy.wrapping_neg(), dy: pr.dx, k: 0.0 };
        vertices[1] = PolyVertex { x: 0.0, y: 0.0 };
        slopes[1] = PolySlope { dx: pl.dy, dy: pl.dx.wrapping_neg(), k: 0.0 };
        vertices[2] = PolyVertex { x: pl.xa, y: pl.ya };

        let (mut mx, mut my) = (0.0, 0.0);
        if join == JoinStyle::Miter {
            let a = mul2_sub(pr.xa, pr.dy as f64, pr.ya, pr.dx as f64);
            let b = mul2_sub(pl.xa, pl.dy as f64, pl.ya, pl.dx as f64);
            my = mul2_sub(pl.dy as f64, a, pr.dy as f64, b) / denom;
            if pl.dy != 0 {
                mx = pl.xa + (my - pl.ya) * pl.dx as f64 / pl.dy as f64;
            } else {
                mx = pr.xa + (my - pr.ya) * pr.dx as f64 / pr.dy as f64;
            }
            if mul2_add(mx, mx, my, my) * 4.0 > SQSECANT * lw as f64 * lw as f64 {
                join = JoinStyle::Bevel;
            }
        }

        let edgecount = if join == JoinStyle::Miter {
            slopes[2] = PolySlope { dx: pl.dx, dy: pl.dy, k: pl.k };
            if swapslopes {
                slopes[2] = PolySlope { dx: slopes[2].dx.wrapping_neg(), dy: slopes[2].dy.wrapping_neg(), k: -slopes[2].k };
            }
            vertices[3] = PolyVertex { x: mx, y: my };
            slopes[3] = PolySlope { dx: pr.dx, dy: pr.dy, k: pr.k };
            if swapslopes {
                slopes[3] = PolySlope { dx: slopes[3].dx.wrapping_neg(), dy: slopes[3].dy.wrapping_neg(), k: -slopes[3].k };
            }
            4
        } else {
            let dx = pr.xa - pl.xa;
            let dy = pr.ya - pl.ya;
            let (adx, ady) = (dx.abs(), dy.abs());
            let scale = if adx > ady { adx } else { ady };
            slopes[2].dx = ((dx * 65536.0) / scale) as i32;
            slopes[2].dy = ((dy * 65536.0) / scale) as i32;
            slopes[2].k = mul2_sub(pl.xa + pr.xa, slopes[2].dy as f64, pl.ya + pr.ya, slopes[2].dx as f64) / 2.0;
            3
        };

        let (mut left, mut right) = ([PolyEdge::default(); 4], [PolyEdge::default(); 4]);
        let (y, nleft, nright, height) = poly_build_poly(&vertices[..edgecount], &slopes[..edgecount], pl.x, pl.y, &mut left, &mut right);
        self.fill_poly_helper(pixel, y, height, &left[..nleft], &right[..nright]);
    }

    /// miLineArcI: a round cap or join centred on a pixel.
    fn line_arc_i(&self, xorg: i32, yorg: i32) -> Vec<Span> {
        let lw = self.lw;
        if lw == 1 {
            return vec![span(xorg, yorg, 1)];
        }
        let mut pts = vec![Span::default(); lw as usize];
        let mut t = 0usize;
        let mut b = lw as usize;
        let mut y = (lw >> 1) + 1;
        let mut e = if lw & 1 != 0 { -((y << 2) + 3) } else { -(y << 3) };
        let mut ex = -4;
        let mut x = 0i32;
        while y != 0 {
            e += (y << 3) - 4;
            while e >= 0 {
                x += 1;
                ex = -((x << 3) + 4);
                e += ex;
            }
            y -= 1;
            let mut slw = (x << 1) + 1;
            if e == ex && slw > 1 {
                slw -= 1;
            }
            if let Some(p) = pts.get_mut(t) {
                *p = span(xorg.wrapping_sub(x), yorg.wrapping_sub(y), slw);
            }
            t += 1;
            if y != 0 && (slw > 1 || e != ex) && b > 0 {
                b -= 1;
                pts[b] = span(xorg.wrapping_sub(x), yorg.wrapping_add(y), slw);
            }
        }
        pts
    }

    /// miLineArcD: a round cap or join centred anywhere, clipped to up to
    /// two face edges.
    #[allow(clippy::too_many_arguments)]
    fn line_arc_d(&self, xorg: f64, yorg: f64, edge1: &mut PolyEdge, edgey1: i32, edgeleft1: bool, edge2: &mut PolyEdge, edgey2: i32, edgeleft2: bool) -> Vec<Span> {
        let (mut edgey1, mut edgey2) = (edgey1, edgey2);
        let mut pts = Vec::new();
        let xbase = xorg.floor() as i32;
        let x0 = xorg - xbase as f64;
        let mut ybase = iceil(yorg);
        let y0 = yorg - ybase as f64;
        let xlk = x0 + x0 + 1.0;
        let xrk = x0 + x0 - 1.0;
        let yk = y0 + y0 - 1.0;
        let radius = self.lw as f64 / 2.0;
        let mut y = (radius - y0 + 1.0).floor() as i32;
        ybase = ybase.wrapping_sub(y);
        let mut ymin = ybase;
        let mut ymax = 65536i32;
        let mut edge1_is_min = false;
        let ymin1 = edgey1;
        if edge1.dy >= 0 {
            if edge1.dy == 0 {
                if edgeleft1 {
                    edge1_is_min = true;
                } else {
                    ymax = edgey1;
                }
                edgey1 = 65536;
            } else if (edge1.signdx < 0) == edgeleft1 {
                edge1_is_min = true;
            }
        }
        let mut edge2_is_min = false;
        let ymin2 = edgey2;
        if edge2.dy >= 0 {
            if edge2.dy == 0 {
                if edgeleft2 {
                    edge2_is_min = true;
                } else {
                    ymax = edgey2;
                }
                edgey2 = 65536;
            } else if (edge2.signdx < 0) == edgeleft2 {
                edge2_is_min = true;
            }
        }
        if edge1_is_min {
            ymin = ymin1;
            if edge2_is_min && ymin1 > ymin2 {
                ymin = ymin2;
            }
        } else if edge2_is_min {
            ymin = ymin2;
        }
        let a = y as f64 + y0;
        let mut el = if FUSED { (-x0).mul_add(x0, radius.mul_add(radius, -(a * a))) } else { radius * radius - (a * a) - (x0 * x0) };
        let mut er = el + xrk;
        let mut xl = 1i32;
        let mut xr = 0i32;
        if x0 < 0.5 {
            xl = 0;
            el -= xlk;
        }
        // Rows: about lw of them; the bound only stops absurd input.
        let mut budget = 4i64 * self.lw as i64 + 16;
        let mut boty = if y0 < -0.5 { 1 } else { 0 };
        if ybase.wrapping_add(y).wrapping_sub(boty) > ymax {
            boty = ymax.wrapping_sub(ybase).wrapping_sub(y);
        }
        while y > boty && budget > 0 {
            budget -= 1;
            let k = y.wrapping_mul(2) as f64 + yk;
            er += k;
            while er > 0.0 {
                xr = xr.wrapping_add(1);
                er += xrk - xr.wrapping_mul(2) as f64;
            }
            el += k;
            while el >= 0.0 {
                xl = xl.wrapping_sub(1);
                el += xl.wrapping_mul(2) as f64 - xlk;
            }
            y -= 1;
            ybase = ybase.wrapping_add(1);
            if ybase < ymin {
                continue;
            }
            let mut xcl = xl.wrapping_add(xbase);
            let mut xcr = xr.wrapping_add(xbase);
            clip_step_edge(ybase, &mut edgey1, edge1, edgeleft1, &mut xcl, &mut xcr);
            clip_step_edge(ybase, &mut edgey2, edge2, edgeleft2, &mut xcl, &mut xcr);
            if xcr >= xcl {
                pts.push(span(xcl, ybase, xcr.wrapping_sub(xcl).wrapping_add(1)));
            }
        }
        er = xrk - xr.wrapping_mul(2) as f64 - er;
        el = xl.wrapping_mul(2) as f64 - xlk - el;
        let mut boty = (-y0 - radius + 1.0).floor() as i32;
        if ybase.wrapping_add(y).wrapping_sub(boty) > ymax {
            boty = ymax.wrapping_sub(ybase).wrapping_sub(y);
        }
        while y > boty && budget > 0 {
            budget -= 1;
            let k = y.wrapping_mul(2) as f64 + yk;
            er -= k;
            while er >= 0.0 && xr >= 0 {
                xr -= 1;
                er += xrk - xr.wrapping_mul(2) as f64;
            }
            el -= k;
            while el > 0.0 && xl <= 0 {
                xl += 1;
                el += xl.wrapping_mul(2) as f64 - xlk;
            }
            y -= 1;
            ybase = ybase.wrapping_add(1);
            if ybase < ymin {
                continue;
            }
            let mut xcl = xl.wrapping_add(xbase);
            let mut xcr = xr.wrapping_add(xbase);
            clip_step_edge(ybase, &mut edgey1, edge1, edgeleft1, &mut xcl, &mut xcr);
            clip_step_edge(ybase, &mut edgey2, edge2, edgeleft2, &mut xcl, &mut xcr);
            if xcr >= xcl {
                pts.push(span(xcl, ybase, xcr.wrapping_sub(xcl).wrapping_add(1)));
            }
        }
        pts
    }

    /// miLineArc: a round cap (one face) or join (two faces), or a dot.
    fn line_arc(&mut self, pixel: u32, left: Option<&mut LineFace>, right: Option<&mut LineFace>, xorg: f64, yorg: f64, is_int: bool) {
        let (mut xorg, mut yorg, mut is_int) = (xorg, yorg, is_int);
        let (mut xorgi, mut yorgi) = (0, 0);
        if let Some(f) = left.as_deref().or(right.as_deref()).filter(|_| is_int) {
            xorgi = f.x;
            yorgi = f.y;
        }
        let (mut edgey1, mut edgey2) = (65536, 65536);
        let mut edge1 = PolyEdge { dy: -1, ..PolyEdge::default() };
        let mut edge2 = PolyEdge { dy: -1, ..PolyEdge::default() };
        let (mut edgeleft1, mut edgeleft2) = (false, false);
        let gc = self.gc;
        if (gc.line_style != LineStyle::Solid || self.lw > 2)
            && ((gc.cap_style == CapStyle::Round && gc.join_style != JoinStyle::Round) || (gc.join_style == JoinStyle::Round && gc.cap_style == CapStyle::Butt))
        {
            if is_int {
                xorg = xorgi as f64;
                yorg = yorgi as f64;
            }
            match (left, right) {
                (Some(l), Some(r)) => {
                    // miRoundJoinClip.
                    let denom = l.dx.wrapping_neg() as f64 * r.dy as f64 + r.dx as f64 * l.dy as f64;
                    if denom >= 0.0 {
                        l.xa = -l.xa;
                        l.ya = -l.ya;
                    } else {
                        r.xa = -r.xa;
                        r.ya = -r.ya;
                    }
                    (edgey1, edgeleft1) = round_join_face(l, &mut edge1);
                    (edgey2, edgeleft2) = round_join_face(r, &mut edge2);
                }
                (Some(l), None) => (edgey1, edgeleft1) = round_cap_clip(l, is_int, &mut edge1),
                (None, Some(r)) => (edgey2, edgeleft2) = round_cap_clip(r, is_int, &mut edge2),
                (None, None) => {}
            }
            is_int = false;
        }
        let spans = if is_int { self.line_arc_i(xorgi, yorgi) } else { self.line_arc_d(xorg, yorg, &mut edge1, edgey1, edgeleft1, &mut edge2, edgey2, edgeleft2) };
        self.fill_spans(pixel, spans);
    }

    /// miLineProjectingCap: a projecting cap on its own (OnOffDash closed
    /// lines that end in an off dash).
    fn line_projecting_cap(&mut self, pixel: u32, face: &LineFace, is_left: bool) {
        let mut is_left = is_left;
        // Always called with isInt true.
        let (xorgi, yorgi) = (face.x, face.y);
        let lw = self.lw;
        let dx = face.dx;
        let mut dy = face.dy;
        let mut k = face.k;
        if dy == 0 {
            let mut l = PolyEdge { height: lw, x: xorgi, stepx: 0, signdx: 1, e: lw.wrapping_neg(), dx: 0, dy: lw };
            if is_left {
                l.x = l.x.wrapping_sub(lw >> 1);
            }
            let mut r = l;
            r.x = xorgi;
            if !is_left {
                r.x = r.x.wrapping_add((lw + 1) >> 1);
            }
            self.fill_poly_helper(pixel, yorgi.wrapping_sub(lw >> 1), lw, &[l], &[r]);
        } else if dx == 0 {
            if dy < 0 {
                dy = dy.wrapping_neg();
                is_left = !is_left;
            }
            let mut topy = yorgi;
            let mut bottomy = yorgi.wrapping_add(dy);
            if is_left {
                topy = topy.wrapping_sub(lw >> 1);
            } else {
                bottomy = bottomy.wrapping_add(lw >> 1);
            }
            let h = bottomy.wrapping_sub(topy);
            let l = PolyEdge { height: h, x: xorgi.wrapping_sub(lw >> 1), stepx: 0, signdx: 1, e: dy.wrapping_neg(), dx, dy };
            let r = PolyEdge { x: l.x.wrapping_add(lw - 1), ..l };
            self.fill_poly_helper(pixel, topy, h, &[l], &[r]);
        } else {
            let mut xa = face.xa;
            let mut ya = face.ya;
            let project_xoff = -ya;
            let project_yoff = xa;
            let (mut right, mut left, mut top, mut bottom) = (PolyEdge::default(), PolyEdge::default(), PolyEdge::default(), PolyEdge::default());
            let (righty, lefty, topy, bottomy, maxy);
            if is_left {
                righty = poly_build_edge(xa, ya, k, dx, dy, xorgi, yorgi, false, &mut right);
                xa = -xa;
                ya = -ya;
                k = -k;
                lefty = poly_build_edge(xa - project_xoff, ya - project_yoff, k, dx, dy, xorgi, yorgi, true, &mut left);
                if dx > 0 {
                    ya = -ya;
                    xa = -xa;
                }
                let xap = xa - project_xoff;
                let yap = ya - project_yoff;
                topy = poly_build_edge(xap, yap, mul2_add(xap, dx as f64, yap, dy as f64), dy.wrapping_neg(), dx, xorgi, yorgi, dx > 0, &mut top);
                bottomy = poly_build_edge(xa, ya, 0.0, dy.wrapping_neg(), dx, xorgi, yorgi, dx < 0, &mut bottom);
                maxy = -ya;
            } else {
                righty = poly_build_edge(xa - project_xoff, ya - project_yoff, k, dx, dy, xorgi, yorgi, false, &mut right);
                xa = -xa;
                ya = -ya;
                k = -k;
                lefty = poly_build_edge(xa, ya, k, dx, dy, xorgi, yorgi, true, &mut left);
                if dx > 0 {
                    ya = -ya;
                    xa = -xa;
                }
                let xap = xa - project_xoff;
                let yap = ya - project_yoff;
                // X.org passes xorgi as the y here, twice.
                topy = poly_build_edge(xa, ya, 0.0, dy.wrapping_neg(), dx, xorgi, xorgi, dx > 0, &mut top);
                bottomy = poly_build_edge(xap, yap, mul2_add(xap, dx as f64, yap, dy as f64), dy.wrapping_neg(), dx, xorgi, xorgi, dx < 0, &mut bottom);
                maxy = -ya + project_yoff;
            }
            let finaly = iceil(maxy).wrapping_add(yorgi);
            if dx < 0 {
                left.height = bottomy.wrapping_sub(lefty);
                right.height = finaly.wrapping_sub(righty);
                top.height = righty.wrapping_sub(topy);
            } else {
                right.height = bottomy.wrapping_sub(righty);
                left.height = finaly.wrapping_sub(lefty);
                top.height = lefty.wrapping_sub(topy);
            }
            bottom.height = finaly.wrapping_sub(bottomy);
            let (lefts, rights) = if dx < 0 { ([left, bottom], [top, right]) } else { ([top, left], [right, bottom]) };
            self.fill_poly_helper(pixel, topy, bottom.height.wrapping_add(bottomy).wrapping_sub(topy), &lefts, &rights);
        }
    }

    /// miWideSegment: one segment of a solid wide line, and its faces.
    #[allow(clippy::too_many_arguments)]
    fn wide_segment(&mut self, pixel: u32, x1: i32, y1: i32, x2: i32, y2: i32, project_left: bool, project_right: bool, left_face: &mut LineFace, right_face: &mut LineFace) {
        let lw = self.lw;
        let (mut x1, mut y1, mut x2, mut y2) = (x1, y1, x2, y2);
        let (mut project_left, mut project_right) = (project_left, project_right);
        let (mut left_face, mut right_face) = (left_face, right_face);
        // Draw top to bottom, always.
        if y2 < y1 || (y2 == y1 && x2 < x1) {
            std::mem::swap(&mut x1, &mut x2);
            std::mem::swap(&mut y1, &mut y2);
            std::mem::swap(&mut project_left, &mut project_right);
            std::mem::swap(&mut left_face, &mut right_face);
        }
        let mut dy = y2.wrapping_sub(y1);
        let mut dx = x2.wrapping_sub(x1);
        let signdx = if dx < 0 { -1 } else { 1 };

        left_face.x = x1;
        left_face.y = y1;
        left_face.dx = dx;
        left_face.dy = dy;
        right_face.x = x2;
        right_face.y = y2;
        right_face.dx = dx.wrapping_neg();
        right_face.dy = dy.wrapping_neg();

        if dy == 0 {
            right_face.xa = 0.0;
            right_face.ya = lw as f64 / 2.0;
            right_face.k = -(lw.wrapping_mul(dx) as f64) / 2.0;
            left_face.xa = 0.0;
            left_face.ya = -right_face.ya;
            left_face.k = right_face.k;
            let mut x = x1;
            if project_left {
                x = x.wrapping_sub(lw >> 1);
            }
            let y = y1.wrapping_sub(lw >> 1);
            dx = x2.wrapping_sub(x);
            if project_right {
                dx = dx.wrapping_add((lw + 1) >> 1);
            }
            self.fill_rect_poly_helper(pixel, x, y, dx, lw);
        } else if dx == 0 {
            left_face.xa = lw as f64 / 2.0;
            left_face.ya = 0.0;
            left_face.k = lw.wrapping_mul(dy) as f64 / 2.0;
            right_face.xa = -left_face.xa;
            right_face.ya = 0.0;
            right_face.k = left_face.k;
            let mut y = y1;
            if project_left {
                y = y.wrapping_sub(lw >> 1);
            }
            let x = x1.wrapping_sub(lw >> 1);
            dy = y2.wrapping_sub(y);
            if project_right {
                dy = dy.wrapping_add((lw + 1) >> 1);
            }
            self.fill_rect_poly_helper(pixel, x, y, lw, dy);
        } else {
            let l = lw as f64 / 2.0;
            let big_l = (dx as f64).hypot(dy as f64);
            let r = l / big_l;
            // The upper bound at integral y.
            let mut ya = -r * dx as f64;
            let mut xa = r * dy as f64;
            let (mut project_xoff, mut project_yoff) = (0.0, 0.0);
            if project_left | project_right {
                project_xoff = -ya;
                project_yoff = xa;
            }
            let mut k = l * big_l;
            left_face.xa = xa;
            left_face.ya = ya;
            left_face.k = k;
            right_face.xa = -xa;
            right_face.ya = -ya;
            right_face.k = k;

            let (mut right, mut left, mut top, mut bottom) = (PolyEdge::default(), PolyEdge::default(), PolyEdge::default(), PolyEdge::default());
            let righty = if project_left {
                poly_build_edge(xa - project_xoff, ya - project_yoff, k, dx, dy, x1, y1, false, &mut right)
            } else {
                poly_build_edge(xa, ya, k, dx, dy, x1, y1, false, &mut right)
            };
            // The lower bound.
            ya = -ya;
            xa = -xa;
            k = -k;
            let lefty = if project_left {
                poly_build_edge(xa - project_xoff, ya - project_yoff, k, dx, dy, x1, y1, true, &mut left)
            } else {
                poly_build_edge(xa, ya, k, dx, dy, x1, y1, true, &mut left)
            };
            // The top face.
            if signdx > 0 {
                ya = -ya;
                xa = -xa;
            }
            let topy = if project_left {
                let xap = xa - project_xoff;
                let yap = ya - project_yoff;
                poly_build_edge(xap, yap, mul2_add(xap, dx as f64, yap, dy as f64), dy.wrapping_neg(), dx, x1, y1, dx > 0, &mut top)
            } else {
                poly_build_edge(xa, ya, 0.0, dy.wrapping_neg(), dx, x1, y1, dx > 0, &mut top)
            };
            // The bottom face.
            let (bottomy, maxy) = if project_right {
                let xap = xa + project_xoff;
                let yap = ya + project_yoff;
                (poly_build_edge(xap, yap, mul2_add(xap, dx as f64, yap, dy as f64), dy.wrapping_neg(), dx, x2, y2, dx < 0, &mut bottom), -ya + project_yoff)
            } else {
                (poly_build_edge(xa, ya, 0.0, dy.wrapping_neg(), dx, x2, y2, dx < 0, &mut bottom), -ya)
            };
            let finaly = iceil(maxy).wrapping_add(y2);
            if dx < 0 {
                left.height = bottomy.wrapping_sub(lefty);
                right.height = finaly.wrapping_sub(righty);
                top.height = righty.wrapping_sub(topy);
            } else {
                right.height = bottomy.wrapping_sub(righty);
                left.height = finaly.wrapping_sub(lefty);
                top.height = lefty.wrapping_sub(topy);
            }
            bottom.height = finaly.wrapping_sub(bottomy);
            let (lefts, rights) = if dx < 0 { ([left, bottom], [top, right]) } else { ([top, left], [right, bottom]) };
            self.fill_poly_helper(pixel, topy, bottom.height.wrapping_add(bottomy).wrapping_sub(topy), &lefts, &rights);
        }
    }

    /// miWideDashSegment: one segment of a dashed wide line, cut into
    /// dashes; returns its faces and moves the dash position along.
    #[allow(clippy::too_many_arguments)]
    fn wide_dash_segment(&mut self, dash_offset: &mut i32, dash_index: &mut usize, x1: i32, y1: i32, x2: i32, y2: i32, project_left: bool, project_right: bool, left_face: &mut LineFace, right_face: &mut LineFace) {
        const T: usize = 0;
        const R: usize = 1;
        const B: usize = 2;
        const L: usize = 3;
        let gc = self.gc;
        let dash = &gc.dashes;
        let on_off = gc.line_style == LineStyle::OnOffDash;
        let dx = x2.wrapping_sub(x1);
        let dy = y2.wrapping_sub(y1);
        let mut dash_index_v = *dash_index;
        let mut dash_remain = dash[dash_index_v] as i32 - *dash_offset;
        let fg_pixel = gc.fg;
        let mut bg_pixel = gc.bg;
        if gc.fill_style == FillStyle::OpaqueStippled || gc.fill_style == FillStyle::Tiled {
            bg_pixel = fg_pixel;
        }

        let l = self.lw as f64 / 2.0;
        let (big_l, rdx, rdy);
        if dx == 0 {
            if dy < 0 {
                big_l = -(dy as f64);
                rdy = -l;
            } else {
                big_l = dy as f64;
                rdy = l;
            }
            rdx = 0.0;
        } else if dy == 0 {
            if dx < 0 {
                big_l = -(dx as f64);
                rdx = -l;
            } else {
                big_l = dx as f64;
                rdx = l;
            }
            rdy = 0.0;
        } else {
            big_l = (dx as f64).hypot(dy as f64);
            let r = l / big_l;
            rdx = r * dx as f64;
            rdy = r * dy as f64;
        }
        let k = l * big_l;
        let mut lremain = big_l;
        let mut slopes = [
            PolySlope { dx, dy, k },
            PolySlope { dx: dy.wrapping_neg(), dy: dx, k: 0.0 },
            PolySlope { dx: dx.wrapping_neg(), dy: dy.wrapping_neg(), k },
            PolySlope { dx: dy, dy: dx.wrapping_neg(), k: 0.0 },
        ];
        let mut vertices = [PolyVertex::default(); 4];
        vertices[R] = PolyVertex { x: rdy, y: -rdx };
        vertices[T] = vertices[R];
        vertices[B] = PolyVertex { x: -rdy, y: rdx };
        vertices[L] = vertices[B];
        if project_left {
            vertices[T].x -= rdx;
            vertices[T].y -= rdy;
            vertices[L].x -= rdx;
            vertices[L].y -= rdy;
            slopes[L].k = mul2_add(rdx, dx as f64, rdy, dy as f64);
        }
        let mut lcenterx = x1 as f64;
        let mut lcentery = y1 as f64;
        let (mut rcenterx, mut rcentery) = (0.0, 0.0);
        let mut lcap = LineFace::default();
        let mut rcap = LineFace::default();
        if gc.cap_style == CapStyle::Round {
            lcap.dx = dx;
            lcap.dy = dy;
            lcap.x = x1;
            lcap.y = y1;
            rcap.dx = dx.wrapping_neg();
            rcap.dy = dy.wrapping_neg();
            rcap.x = x1;
            rcap.y = y1;
        }
        let mut first = true;
        let mut save_right = PolyVertex::default();
        let mut save_bottom = PolyVertex::default();
        let mut save_k = 0.0;
        let (mut left, mut right) = ([PolyEdge::default(); 4], [PolyEdge::default(); 4]);
        while lremain > dash_remain as f64 {
            let dash_dx = dash_remain.wrapping_mul(dx) as f64 / big_l;
            let dash_dy = dash_remain.wrapping_mul(dy) as f64 / big_l;
            rcenterx = lcenterx + dash_dx;
            rcentery = lcentery + dash_dy;
            vertices[R].x += dash_dx;
            vertices[R].y += dash_dy;
            vertices[B].x += dash_dx;
            vertices[B].y += dash_dy;
            slopes[R].k = mul2_add(vertices[R].x, dx as f64, vertices[R].y, dy as f64);

            if !on_off || dash_index_v & 1 == 0 {
                if on_off && gc.cap_style == CapStyle::Projecting {
                    save_right = vertices[R];
                    save_bottom = vertices[B];
                    save_k = slopes[R].k;
                    if !first {
                        vertices[T].x -= rdx;
                        vertices[T].y -= rdy;
                        vertices[L].x -= rdx;
                        vertices[L].y -= rdy;
                        slopes[L].k = mul2_sub(vertices[L].x, slopes[L].dy as f64, vertices[L].y, slopes[L].dx as f64);
                    }
                    vertices[R].x += rdx;
                    vertices[R].y += rdy;
                    vertices[B].x += rdx;
                    vertices[B].y += rdy;
                    slopes[R].k = mul2_sub(vertices[R].x, slopes[R].dy as f64, vertices[R].y, slopes[R].dx as f64);
                }
                let (y, nleft, nright, h) = poly_build_poly(&vertices, &slopes, x1, y1, &mut left, &mut right);
                let pixel = if dash_index_v & 1 != 0 { bg_pixel } else { fg_pixel };
                self.fill_poly_helper(pixel, y, h, &left[..nleft], &right[..nright]);

                if on_off {
                    match gc.cap_style {
                        CapStyle::Projecting => {
                            vertices[B] = save_bottom;
                            vertices[R] = save_right;
                            slopes[R].k = save_k;
                        }
                        CapStyle::Round => {
                            if !first {
                                if dx < 0 {
                                    lcap.xa = -vertices[L].x;
                                    lcap.ya = -vertices[L].y;
                                    lcap.k = slopes[L].k;
                                } else {
                                    lcap.xa = vertices[T].x;
                                    lcap.ya = vertices[T].y;
                                    lcap.k = -slopes[L].k;
                                }
                                self.line_arc(pixel, Some(&mut lcap), None, lcenterx, lcentery, false);
                            }
                            if dx < 0 {
                                rcap.xa = vertices[B].x;
                                rcap.ya = vertices[B].y;
                                rcap.k = slopes[R].k;
                            } else {
                                rcap.xa = -vertices[R].x;
                                rcap.ya = -vertices[R].y;
                                rcap.k = -slopes[R].k;
                            }
                            self.line_arc(pixel, None, Some(&mut rcap), rcenterx, rcentery, false);
                        }
                        _ => {}
                    }
                }
            }
            lremain -= dash_remain as f64;
            dash_index_v += 1;
            if dash_index_v == dash.len() {
                dash_index_v = 0;
            }
            dash_remain = dash[dash_index_v] as i32;
            lcenterx = rcenterx;
            lcentery = rcentery;
            vertices[T] = vertices[R];
            vertices[L] = vertices[B];
            slopes[L].k = -slopes[R].k;
            first = false;
        }

        if !on_off || dash_index_v & 1 == 0 {
            vertices[T].x -= dx as f64;
            vertices[T].y -= dy as f64;
            vertices[L].x -= dx as f64;
            vertices[L].y -= dy as f64;
            vertices[R] = PolyVertex { x: rdy, y: -rdx };
            vertices[B] = PolyVertex { x: -rdy, y: rdx };
            if project_right {
                vertices[R].x += rdx;
                vertices[R].y += rdy;
                vertices[B].x += rdx;
                vertices[B].y += rdy;
                slopes[R].k = mul2_sub(vertices[R].x, slopes[R].dy as f64, vertices[R].y, slopes[R].dx as f64);
            } else {
                slopes[R].k = 0.0;
            }
            if !first && on_off && gc.cap_style == CapStyle::Projecting {
                vertices[T].x -= rdx;
                vertices[T].y -= rdy;
                vertices[L].x -= rdx;
                vertices[L].y -= rdy;
                slopes[L].k = mul2_sub(vertices[L].x, slopes[L].dy as f64, vertices[L].y, slopes[L].dx as f64);
            } else {
                slopes[L].k += dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy)) as f64;
            }
            let (y, nleft, nright, h) = poly_build_poly(&vertices, &slopes, x2, y2, &mut left, &mut right);
            let pixel = if dash_index_v & 1 != 0 { gc.bg } else { gc.fg };
            self.fill_poly_helper(pixel, y, h, &left[..nleft], &right[..nright]);
            if !first && on_off && gc.cap_style == CapStyle::Round {
                lcap.x = x2;
                lcap.y = y2;
                if dx < 0 {
                    lcap.xa = -vertices[L].x;
                    lcap.ya = -vertices[L].y;
                    lcap.k = slopes[L].k;
                } else {
                    lcap.xa = vertices[T].x;
                    lcap.ya = vertices[T].y;
                    lcap.k = -slopes[L].k;
                }
                self.line_arc(pixel, Some(&mut lcap), None, rcenterx, rcentery, false);
            }
        }
        dash_remain = (dash_remain as f64 - lremain) as i32;
        if dash_remain == 0 {
            dash_index_v += 1;
            if dash_index_v == dash.len() {
                dash_index_v = 0;
            }
            dash_remain = dash[dash_index_v] as i32;
        }

        *left_face = LineFace { x: x1, y: y1, dx, dy, xa: rdy, ya: -rdx, k };
        *right_face = LineFace { x: x2, y: y2, dx: dx.wrapping_neg(), dy: dy.wrapping_neg(), xa: -rdy, ya: rdx, k };
        *dash_index = dash_index_v;
        *dash_offset = dash[dash_index_v] as i32 - dash_remain;
    }

    /// miCleanupSpanData: fill the span groups, background first.
    fn cleanup(&mut self) {
        if let Some(mut d) = self.data.take() {
            if self.gc.line_style == LineStyle::DoubleDash {
                fill_unique_span_group(self.sink, self.gc.bg, &mut d.bg);
            }
            fill_unique_span_group(self.sink, self.gc.fg, &mut d.fg);
        }
    }
}

/// miSetupSpanData.
fn setup<'a>(sink: &'a mut dyn Sink, gc: &'a Gc, npt: usize) -> Wide<'a> {
    let data = if (npt < 3 && gc.cap_style != CapStyle::Round) || gc.easy_rop() { None } else { Some(SpanData { fg: SpanGroup::new(), bg: SpanGroup::new() }) };
    Wide { sink, gc, lw: gc.line_width as u16 as i32, data }
}

/// Whether the line ends where it starts (selfJoin).
fn self_join(mode: CoordMode, pts: &[Point]) -> bool {
    let (x2, y2) = (pts[0].x, pts[0].y);
    if mode == CoordMode::Previous {
        let (mut x1, mut y1) = (x2, y2);
        for p in &pts[1..] {
            x1 = x1.wrapping_add(p.x);
            y1 = y1.wrapping_add(p.y);
        }
        x2 == x1 && y2 == y1
    } else {
        x2 == pts[pts.len() - 1].x && y2 == pts[pts.len() - 1].y
    }
}

/// miWideLine: a solid wide line.
pub fn wide_line(sink: &mut dyn Sink, gc: &Gc, mode: CoordMode, pts: &[Point]) {
    // dix's ProcPolyLine draws nothing for fewer than two points.
    if pts.len() < 2 {
        return;
    }
    let mut w = setup(sink, gc, pts.len());
    let pixel = gc.fg;
    let (mut x1, mut y1);
    let (mut x2, mut y2) = (pts[0].x, pts[0].y);
    let mut first = true;
    let self_join = pts.len() > 1 && self_join(mode, pts);
    let mut project_left = gc.cap_style == CapStyle::Projecting && !self_join;
    let mut project_right = false;
    let (mut left_face, mut right_face, mut prev_right_face, mut first_face) = (LineFace::default(), LineFace::default(), LineFace::default(), LineFace::default());
    let mut something_drawn = false;
    let n = pts.len();
    for (i, p) in pts.iter().enumerate().skip(1) {
        let last = i == n - 1;
        x1 = x2;
        y1 = y2;
        x2 = p.x;
        y2 = p.y;
        if mode == CoordMode::Previous {
            x2 = x2.wrapping_add(x1);
            y2 = y2.wrapping_add(y1);
        }
        if x1 != x2 || y1 != y2 {
            something_drawn = true;
            if last && gc.cap_style == CapStyle::Projecting && !self_join {
                project_right = true;
            }
            w.wide_segment(pixel, x1, y1, x2, y2, project_left, project_right, &mut left_face, &mut right_face);
            if first {
                if self_join {
                    first_face = left_face;
                } else if gc.cap_style == CapStyle::Round {
                    if w.lw == 1 && w.data.is_none() {
                        w.line_one_point(pixel, x1, y1);
                    } else {
                        w.line_arc(pixel, Some(&mut left_face), None, 0.0, 0.0, true);
                    }
                }
            } else {
                w.line_join(pixel, &mut left_face, &mut prev_right_face);
            }
            prev_right_face = right_face;
            first = false;
            project_left = false;
        }
        if last && something_drawn {
            if self_join {
                w.line_join(pixel, &mut first_face, &mut right_face);
            } else if gc.cap_style == CapStyle::Round {
                if w.lw == 1 && w.data.is_none() {
                    w.line_one_point(pixel, x2, y2);
                } else {
                    w.line_arc(pixel, None, Some(&mut right_face), 0.0, 0.0, true);
                }
            }
        }
    }
    // All the points the same.
    if !something_drawn {
        let project = gc.cap_style == CapStyle::Projecting;
        w.wide_segment(pixel, x2, y2, x2, y2, project, project, &mut left_face, &mut right_face);
        if gc.cap_style == CapStyle::Round {
            w.line_arc(pixel, Some(&mut left_face), None, 0.0, 0.0, true);
            right_face.dx = -1; // X.org's "sleezy hack"
            w.line_arc(pixel, None, Some(&mut right_face), 0.0, 0.0, true);
        }
    }
    w.cleanup();
}

/// miWideDash: a dashed wide line, OnOffDash or DoubleDash.
pub fn wide_dash(sink: &mut dyn Sink, gc: &Gc, mode: CoordMode, pts: &[Point]) {
    if gc.line_style == LineStyle::DoubleDash && (gc.fill_style == FillStyle::OpaqueStippled || gc.fill_style == FillStyle::Tiled) {
        wide_line(sink, gc, mode, pts);
        return;
    }
    // dix's ProcPolyLine draws nothing for fewer than two points.
    if pts.len() < 2 || gc.dashes.is_empty() || gc.dashes.contains(&0) {
        return;
    }
    // dix's SetDashes keeps an odd dash list twice over.
    let doubled;
    let gc = if gc.dashes.len() % 2 == 1 {
        doubled = Gc { dashes: gc.dashes.repeat(2), ..gc.clone() };
        &doubled
    } else {
        gc
    };
    let mut w = setup(sink, gc, pts.len());
    let double = gc.line_style == LineStyle::DoubleDash;
    let (mut x1, mut y1);
    let (mut x2, mut y2) = (pts[0].x, pts[0].y);
    let mut first = true;
    let self_join = self_join(mode, pts);
    let mut project_left = gc.cap_style == CapStyle::Projecting && !self_join;
    let mut project_right = false;
    let mut dash_index = 0usize;
    let mut dash_offset = 0i32;
    step_dash(gc.dash_offset as u16 as i32, &mut dash_index, &gc.dashes, &mut dash_offset);
    let (mut left_face, mut right_face, mut prev_right_face, mut first_face) = (LineFace::default(), LineFace::default(), LineFace::default(), LineFace::default());
    let mut something_drawn = false;
    let (mut end_is_fg, mut first_is_fg, mut prev_is_fg) = (false, false, false);
    let n = pts.len();
    for (i, p) in pts.iter().enumerate().skip(1) {
        let last = i == n - 1;
        x1 = x2;
        y1 = y2;
        x2 = p.x;
        y2 = p.y;
        if mode == CoordMode::Previous {
            x2 = x2.wrapping_add(x1);
            y2 = y2.wrapping_add(y1);
        }
        if x1 != x2 || y1 != y2 {
            something_drawn = true;
            if last && gc.cap_style == CapStyle::Projecting && (!self_join || !first_is_fg) {
                project_right = true;
            }
            let prev_dash_index = dash_index;
            w.wide_dash_segment(&mut dash_offset, &mut dash_index, x1, y1, x2, y2, project_left, project_right, &mut left_face, &mut right_face);
            let start_is_fg = prev_dash_index & 1 == 0;
            end_is_fg = ((dash_index & 1) != 0) ^ (dash_offset != 0);
            if double || start_is_fg {
                let pixel = if start_is_fg { gc.fg } else { gc.bg };
                if first || (!double && !prev_is_fg) {
                    if first && self_join {
                        first_face = left_face;
                        first_is_fg = start_is_fg;
                    } else if gc.cap_style == CapStyle::Round {
                        w.line_arc(pixel, Some(&mut left_face), None, 0.0, 0.0, true);
                    }
                } else {
                    w.line_join(pixel, &mut left_face, &mut prev_right_face);
                }
            }
            prev_right_face = right_face;
            prev_is_fg = end_is_fg;
            first = false;
            project_left = false;
        }
        if last && something_drawn {
            if double || end_is_fg {
                let pixel = if end_is_fg { gc.fg } else { gc.bg };
                if self_join && (double || first_is_fg) {
                    w.line_join(pixel, &mut first_face, &mut right_face);
                } else if gc.cap_style == CapStyle::Round {
                    w.line_arc(pixel, None, Some(&mut right_face), 0.0, 0.0, true);
                }
            } else if self_join && first_is_fg {
                // OnOffDash ending in an off dash: glue a cap to the start.
                let pixel = gc.fg;
                if gc.cap_style == CapStyle::Projecting {
                    w.line_projecting_cap(pixel, &first_face, true);
                } else if gc.cap_style == CapStyle::Round {
                    w.line_arc(pixel, Some(&mut first_face), None, 0.0, 0.0, true);
                }
            }
        }
    }
    // All the points the same.
    if !something_drawn && (double || dash_index & 1 == 0) {
        let pixel = if dash_index & 1 != 0 { gc.bg } else { gc.fg };
        match gc.cap_style {
            CapStyle::Round => w.line_arc(pixel, None, None, x2 as f64, y2 as f64, false),
            CapStyle::Projecting => {
                let lw = w.lw;
                w.fill_rect_poly_helper(pixel, x2.wrapping_sub(lw >> 1), y2.wrapping_sub(lw >> 1), lw, lw);
            }
            _ => {}
        }
    }
    w.cleanup();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mi::xvfb::{check, Op};

    const CAPS: [CapStyle; 4] = [CapStyle::NotLast, CapStyle::Butt, CapStyle::Round, CapStyle::Projecting];
    const JOINS: [JoinStyle; 3] = [JoinStyle::Miter, JoinStyle::Round, JoinStyle::Bevel];

    fn pts(p: &[(i32, i32)]) -> Vec<Point> {
        p.iter().map(|&(x, y)| Point { x, y }).collect()
    }

    /// Lines in every direction, polylines with every kind of corner,
    /// closed ones, degenerate ones and ones partly outside.
    fn shapes() -> Vec<(&'static str, CoordMode, Vec<Point>)> {
        let o = CoordMode::Origin;
        let mut v = vec![
            ("zigzag sharp", o, pts(&[(15, 20), (145, 30), (15, 42), (140, 100)])),
            ("right angle", o, pts(&[(20, 60), (80, 60), (80, 110), (30, 100)])),
            ("shallow", o, pts(&[(10, 60), (80, 63), (150, 60)])),
            ("closed triangle", o, pts(&[(30, 30), (130, 42), (60, 100), (30, 30)])),
            ("closed square", o, pts(&[(40, 30), (110, 30), (110, 90), (40, 90), (40, 30)])),
            ("one point", o, pts(&[(50, 50)])),
            ("zero length", o, pts(&[(60, 50), (60, 50)])),
            ("all same", o, pts(&[(70, 40), (70, 40), (70, 40)])),
            ("repeated", o, pts(&[(20, 20), (20, 20), (80, 50), (80, 50), (140, 20), (140, 20)])),
            ("previous", CoordMode::Previous, pts(&[(20, 100), (30, -50), (30, 50), (40, -70), (-10, 5)])),
            ("previous closed", CoordMode::Previous, pts(&[(30, 30), (90, 10), (-40, 60), (-50, -70)])),
            ("outside", o, pts(&[(-20, -10), (100, 50), (190, 140)])),
            ("outside far", o, pts(&[(150, 10), (400, 60), (-300, 300), (5, 115)])),
            ("hairpin", o, pts(&[(20, 60), (140, 61), (20, 62)])),
            ("star", o, pts(&[(80, 10), (100, 100), (20, 40), (140, 40), (60, 100), (80, 10)])),
        ];
        // Single segments in all directions, on and off the axes.
        for &(dx, dy) in &[(40, 0), (0, 40), (37, 11), (11, 37), (30, 30), (45, 3), (1, 44)] {
            for &(sx, sy) in &[(1, 1), (-1, 1), (1, -1), (-1, -1)] {
                v.push(("segment", o, pts(&[(80, 60), (80 + sx * dx, 60 + sy * dy)])));
            }
        }
        v
    }

    /// Run every case, then fail listing the ones that differ from X.org.
    fn run(cases: Vec<(String, Gc, Op)>) {
        let mut bad = Vec::new();
        let n = cases.len();
        for (name, gc, op) in cases {
            if let Err(e) = std::panic::catch_unwind(|| check(&name, &gc, &op)) {
                bad.push(e.downcast_ref::<String>().cloned().unwrap_or_default());
            }
        }
        if !bad.is_empty() {
            let names: Vec<&str> = bad.iter().map(|s| s.split(':').next().unwrap_or("")).collect();
            panic!("{} of {} cases differ: {:?}\n\nfirst:\n{}", bad.len(), n, names, bad.iter().take(2).cloned().collect::<Vec<_>>().join("\n\n"));
        }
    }

    fn solid_cases(widths: &[u32]) -> Vec<(String, Gc, Op)> {
        let mut cases = Vec::new();
        for (name, mode, p) in shapes() {
            for &lw in widths {
                for cap in CAPS {
                    for join in JOINS {
                        for function in [3u8, 6] {
                            let gc = Gc { function, line_width: lw, cap_style: cap, join_style: join, fg: 0xff_c020, ..Gc::default() };
                            cases.push((format!("{name} w{lw} {cap:?} {join:?} f{function}"), gc, Op::Lines(mode, p.clone())));
                        }
                    }
                }
            }
        }
        cases
    }

    #[test]
    fn solid_thin_widths() {
        run(solid_cases(&[1, 2, 3]));
    }

    #[test]
    fn solid_wide() {
        run(solid_cases(&[5, 10, 25]));
    }

    #[test]
    fn solid_huge() {
        let mut cases = Vec::new();
        let lines = [(60u32, pts(&[(20, 20), (140, 100)])), (300, pts(&[(80, 60), (90, 61)])), (65535, pts(&[(80, 60), (81, 70)])), (40, pts(&[(-32768, -32768), (32767, 32767)])), (7, pts(&[(-32768, 50), (32767, 60), (-32768, 70)]))];
        for (lw, p) in lines {
            for cap in CAPS {
                for join in JOINS {
                    for function in [3u8, 6] {
                        let gc = Gc { function, line_width: lw, cap_style: cap, join_style: join, fg: 0x12_3456, ..Gc::default() };
                        cases.push((format!("huge w{lw} {cap:?} {join:?} f{function}"), gc, Op::Lines(CoordMode::Origin, p.clone())));
                    }
                }
            }
        }
        run(cases);
    }

    #[test]
    fn segments() {
        let segs = vec![
            Segment { x1: 10, y1: 10, x2: 100, y2: 40 },
            Segment { x1: 100, y1: 10, x2: 10, y2: 40 },
            Segment { x1: 50, y1: 5, x2: 50, y2: 110 },
            Segment { x1: 120, y1: 80, x2: 120, y2: 80 },
            Segment { x1: 5, y1: 90, x2: 150, y2: 90 },
        ];
        let mut cases = Vec::new();
        for lw in [1u32, 4, 9] {
            for cap in CAPS {
                for (style, function) in [(LineStyle::Solid, 3u8), (LineStyle::Solid, 6), (LineStyle::OnOffDash, 6), (LineStyle::DoubleDash, 6), (LineStyle::DoubleDash, 3)] {
                    let gc = Gc { function, line_width: lw, line_style: style, cap_style: cap, fg: 0xff_0000, bg: 0x00_ff00, dashes: vec![5, 3], ..Gc::default() };
                    cases.push((format!("segments w{lw} {cap:?} {style:?} f{function}"), gc, Op::Segments(segs.clone())));
                }
            }
        }
        run(cases);
    }

    fn dash_cases(widths: &[u32]) -> Vec<(String, Gc, Op)> {
        let patterns: [(&[u8], u32); 6] = [(&[4], 0), (&[6, 3], 2), (&[5, 2, 1], 0), (&[1], 0), (&[10, 3, 2, 7], 30), (&[8, 4], 101)];
        let mut cases = Vec::new();
        for (i, (name, mode, p)) in shapes().into_iter().enumerate() {
            for (j, &lw) in widths.iter().enumerate() {
                let (dashes, offset) = patterns[(i + j) % patterns.len()];
                for cap in CAPS {
                    for join in JOINS {
                        for style in [LineStyle::OnOffDash, LineStyle::DoubleDash] {
                            for function in [3u8, 6] {
                                let gc = Gc { function, line_width: lw, line_style: style, cap_style: cap, join_style: join, fg: 0xff_0000, bg: 0x00_80ff, dashes: dashes.to_vec(), dash_offset: offset, ..Gc::default() };
                                cases.push((format!("{name} w{lw} {style:?} {cap:?} {join:?} f{function} {dashes:?}+{offset}"), gc, Op::Lines(mode, p.clone())));
                            }
                        }
                    }
                }
            }
        }
        cases
    }

    #[test]
    fn dashed_thin_widths() {
        run(dash_cases(&[1, 2, 3]));
    }

    #[test]
    fn dashed_wide() {
        run(dash_cases(&[5, 10, 25]));
    }

    #[test]
    fn dashed_same_fg_bg() {
        let mut cases = Vec::new();
        for (name, mode, p) in shapes().into_iter().take(6) {
            for function in [3u8, 6] {
                for cap in CAPS {
                    let gc = Gc { function, line_width: 6, line_style: LineStyle::DoubleDash, cap_style: cap, join_style: JoinStyle::Round, fg: 0xff_00ff, bg: 0xff_00ff, dashes: vec![7, 3], ..Gc::default() };
                    cases.push((format!("{name} same pixels {cap:?} f{function}"), gc, Op::Lines(mode, p.clone())));
                }
            }
        }
        run(cases);
    }

    /// Absurd input mustn't panic or hang.
    #[test]
    fn robust() {
        struct Null;
        impl Sink for Null {
            fn fill_spans(&mut self, _pixel: u32, _spans: &[Span]) {}
        }
        let big = pts(&[(-32768, -32768), (32767, 32767), (-32768, 32767), (32767, -32768), (-32768, -32768)]);
        // Past 16 bits, as CoordModePrevious can go.
        let prev = pts(&[(32767, 32767), (32767, 32767), (32767, 32767), (-32768, 32767), (32767, -32768)]);
        let short_prev = pts(&[(32767, 32767), (5, 3), (-2, 7), (300, 1)]);
        for lw in [1u32, 2, 3, 255, 65535, 65536 + 3] {
            for cap in CAPS {
                for join in JOINS {
                    for style in [LineStyle::Solid, LineStyle::OnOffDash, LineStyle::DoubleDash] {
                        for function in [3u8, 6] {
                            let gc = Gc { function, line_width: lw, line_style: style, cap_style: cap, join_style: join, dashes: vec![255, 1], dash_offset: 65535, ..Gc::default() };
                            if lw == 65535 {
                                // Lines this wide are slow (as in X.org): a
                                // few short ones.
                                if function == 6 && join != JoinStyle::Bevel {
                                    poly_line(&mut Null, &gc, CoordMode::Origin, &pts(&[(-32768, 5), (-32700, 90), (-32768, 100)]));
                                    poly_line(&mut Null, &gc, CoordMode::Previous, &short_prev);
                                }
                            } else if function == 6 && join == JoinStyle::Round {
                                poly_line(&mut Null, &gc, CoordMode::Origin, &big);
                                poly_line(&mut Null, &gc, CoordMode::Previous, &prev);
                            }
                        }
                    }
                }
            }
        }
    }
}
