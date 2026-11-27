//! Filled polygons: mipoly.c (miFillPolygon, miFillConvexPoly,
//! miFillGeneralPoly and its edge tables), mipoly.h and miscanfill.h,
//! ported. COPYING in this directory has their notices.

use super::*;

/// miscanfill.h's BRESINFO: an edge stepped one scanline at a time.
#[derive(Clone, Copy, Default)]
struct Bres {
    minor: i64,
    d: i64,
    m: i64,
    m1: i64,
    incr1: i64,
    incr2: i64,
}

impl Bres {
    /// BRESINITPGON: a horizontal edge (dy 0) leaves it as it was.
    fn init(&mut self, dy: i64, x1: i64, x2: i64) {
        if dy == 0 {
            return;
        }
        self.minor = x1;
        let dx = x2 - x1;
        // C's division truncates, as Rust's does.
        self.m = dx / dy;
        if dx < 0 {
            self.m1 = self.m - 1;
            self.incr1 = -2 * dx + 2 * dy * self.m1;
            self.incr2 = -2 * dx + 2 * dy * self.m;
            self.d = 2 * self.m * dy - 2 * dx - 2 * dy;
        } else {
            self.m1 = self.m + 1;
            self.incr1 = 2 * dx - 2 * dy * self.m1;
            self.incr2 = 2 * dx - 2 * dy * self.m;
            self.d = -2 * self.m * dy + 2 * dx;
        }
    }

    /// BRESINCRPGON.
    fn step(&mut self) {
        let big = if self.m1 > 0 { self.d > 0 } else { self.d >= 0 };
        if big {
            self.minor += self.m1;
            self.d += self.incr1;
        } else {
            self.minor += self.m;
            self.d += self.incr2;
        }
    }
}

/// FillPoly (miFillPolygon): the points made absolute in 16 bits, as X.org
/// does it in place, then the convex filler for Convex, the general one
/// otherwise.
pub fn fill_polygon(sink: &mut dyn Sink, gc: &Gc, shape: Shape, mode: CoordMode, pts: &[Point]) {
    if pts.is_empty() {
        return;
    }
    let mut p: Vec<(i16, i16)> = pts.iter().map(|p| (p.x as i16, p.y as i16)).collect();
    if mode == CoordMode::Previous {
        for i in 1..p.len() {
            p[i].0 = p[i].0.wrapping_add(p[i - 1].0);
            p[i].1 = p[i].1.wrapping_add(p[i - 1].1);
        }
    }
    let p: Vec<(i64, i64)> = p.into_iter().map(|(x, y)| (x as i64, y as i64)).collect();
    if shape == Shape::Convex {
        convex(sink, gc, &p)
    } else {
        general(sink, gc, &p)
    }
}

fn span(x: i64, y: i64, w: i64) -> Span {
    // Points are 16-bit and edges stay between them: these fit.
    Span {
        x: x as i32,
        y: y as i32,
        w: w as i32,
    }
}

/// miFillConvexPoly: a left and a right edge walked down from the top
/// vertex. A polygon that isn't convex gives up, drawing nothing, where a
/// walk turns back up.
fn convex(sink: &mut dyn Sink, gc: &Gc, pts: &[(i64, i64)]) {
    let count = pts.len();
    // getPolyYBounds.
    let mut imin = 0;
    let (mut ymin, mut ymax) = (pts[0].1, pts[0].1);
    for (i, p) in pts.iter().enumerate().skip(1) {
        if p.1 < ymin {
            imin = i;
            ymin = p.1;
        }
        ymax = ymax.max(p.1);
    }
    let dy = ymax - ymin + 1;
    if count < 3 || dy < 0 {
        return;
    }
    let mut spans = Vec::with_capacity(dy as usize);
    let (mut l, mut r) = (Bres::default(), Bres::default());
    let (mut nextleft, mut nextright) = (imin, imin);
    let mut y = pts[imin].1;
    loop {
        // A left edge, if we need one.
        if pts[nextleft].1 == y {
            let left = nextleft;
            nextleft = if nextleft + 1 >= count {
                0
            } else {
                nextleft + 1
            };
            l.init(pts[nextleft].1 - pts[left].1, pts[left].0, pts[nextleft].0);
        }
        // A right edge, if we need one.
        if pts[nextright].1 == y {
            let right = nextright;
            nextright = if nextright == 0 {
                count - 1
            } else {
                nextright - 1
            };
            r.init(
                pts[nextright].1 - pts[right].1,
                pts[right].0,
                pts[nextright].0,
            );
        }
        let i = pts[nextleft].1.min(pts[nextright].1) - y;
        // Not convex after all.
        if i < 0 {
            return;
        }
        for _ in 0..i {
            if l.minor < r.minor {
                spans.push(span(l.minor, y, r.minor - l.minor));
            } else {
                spans.push(span(r.minor, y, l.minor - r.minor));
            }
            y += 1;
            l.step();
            r.step();
        }
        if y == ymax {
            break;
        }
    }
    sink.fill_spans(gc.fg, &spans);
}

/// mipoly.h's EdgeTableEntry, in a Vec: links are indices, NIL none.
#[derive(Clone, Copy)]
struct Edge {
    ymax: i64,
    bres: Bres,
    next: usize,
    back: usize,
    next_wete: usize,
    clockwise: bool,
}

const NIL: usize = usize::MAX;
/// The active edge table's head node.
const AET: usize = 0;

/// miFillGeneralPoly's tables: the edges (the AET head first) and the edge
/// table, a bucket of edges by the scanline they start on.
struct Tables {
    e: Vec<Edge>,
    et: std::collections::BTreeMap<i64, Vec<usize>>,
    ymin: i64,
    ymax: i64,
}

impl Tables {
    /// miCreateETandAET, with miInsertEdgeInET: each bucket sorted by x, a
    /// new edge before those with the same x.
    fn new(pts: &[(i64, i64)]) -> Self {
        let head = Edge {
            ymax: 0,
            bres: Bres {
                minor: i64::MIN,
                ..Bres::default()
            },
            next: NIL,
            back: NIL,
            next_wete: NIL,
            clockwise: false,
        };
        let mut t = Tables {
            e: vec![head],
            et: Default::default(),
            ymin: i32::MAX as i64,
            ymax: i32::MIN as i64,
        };
        let mut prev = pts[pts.len() - 1];
        for &curr in pts {
            let (top, bottom, clockwise) = if prev.1 > curr.1 {
                (curr, prev, false)
            } else {
                (prev, curr, true)
            };
            // Horizontal edges aren't in the table.
            if bottom.1 != top.1 {
                let mut bres = Bres::default();
                bres.init(bottom.1 - top.1, top.0, bottom.0);
                let id = t.e.len();
                // ymax - 1, so the last scanline isn't drawn.
                t.e.push(Edge {
                    ymax: bottom.1 - 1,
                    bres,
                    next: NIL,
                    back: NIL,
                    next_wete: NIL,
                    clockwise,
                });
                let bucket = t.et.entry(top.1).or_default();
                let at = bucket
                    .iter()
                    .position(|&o| t.e[o].bres.minor >= bres.minor)
                    .unwrap_or(bucket.len());
                bucket.insert(at, id);
                t.ymax = t.ymax.max(prev.1);
                t.ymin = t.ymin.min(prev.1);
            }
            prev = curr;
        }
        t
    }

    /// miloadAET: a bucket's edges into the AET, which stays sorted by x.
    fn load(&mut self, edges: &[usize]) {
        let mut prev = AET;
        let mut aet = self.e[AET].next;
        for &ete in edges {
            while aet != NIL && self.e[aet].bres.minor < self.e[ete].bres.minor {
                prev = aet;
                aet = self.e[aet].next;
            }
            self.e[ete].next = aet;
            if aet != NIL {
                self.e[aet].back = ete;
            }
            self.e[ete].back = prev;
            self.e[prev].next = ete;
            prev = ete;
        }
    }

    /// micomputeWAET: link the edges where the winding number goes between
    /// zero and not.
    fn compute_waet(&mut self) {
        let mut inside = true;
        let mut is_inside = 0i64;
        self.e[AET].next_wete = NIL;
        let mut wete = AET;
        let mut aet = self.e[AET].next;
        while aet != NIL {
            is_inside += if self.e[aet].clockwise { 1 } else { -1 };
            if (!inside && is_inside == 0) || (inside && is_inside != 0) {
                self.e[wete].next_wete = aet;
                wete = aet;
                inside = !inside;
            }
            aet = self.e[aet].next;
        }
        self.e[wete].next_wete = NIL;
    }

    /// miInsertionSort: re-sort the AET by x; whether anything moved.
    fn insertion_sort(&mut self) -> bool {
        let mut changed = false;
        let mut aet = self.e[AET].next;
        while aet != NIL {
            let insert = aet;
            let mut chase = aet;
            while self.e[self.e[chase].back].bres.minor > self.e[aet].bres.minor {
                chase = self.e[chase].back;
            }
            aet = self.e[aet].next;
            if chase != insert {
                let chase_back = self.e[chase].back;
                let insert_back = self.e[insert].back;
                self.e[insert_back].next = aet;
                if aet != NIL {
                    self.e[aet].back = insert_back;
                }
                self.e[insert].next = chase;
                self.e[chase_back].next = insert;
                self.e[chase].back = insert;
                self.e[insert].back = chase_back;
                changed = true;
            }
        }
        changed
    }

    /// EVALUATEEDGEEVENODD and EVALUATEEDGEWINDING: drop the edge if it
    /// ends on this scanline, else step it; on to the next. Whether it was
    /// dropped.
    fn evaluate(&mut self, aet: &mut usize, prev: &mut usize, y: i64) -> bool {
        if self.e[*aet].ymax == y {
            self.e[*prev].next = self.e[*aet].next;
            *aet = self.e[*prev].next;
            if *aet != NIL {
                self.e[*aet].back = *prev;
            }
            true
        } else {
            self.e[*aet].bres.step();
            *prev = *aet;
            *aet = self.e[*aet].next;
            false
        }
    }
}

/// miFillGeneralPoly: an edge table, scanned with the even-odd or the
/// winding rule.
fn general(sink: &mut dyn Sink, gc: &Gc, pts: &[(i64, i64)]) {
    if pts.len() < 3 {
        return;
    }
    let mut t = Tables::new(pts);
    let buckets: Vec<(i64, Vec<usize>)> = std::mem::take(&mut t.et).into_iter().collect();
    let mut sll = buckets.iter().peekable();
    let mut spans = Vec::new();
    let flush = |spans: &mut Vec<Span>, sink: &mut dyn Sink| {
        if spans.len() >= 200 {
            sink.fill_spans(gc.fg, spans);
            spans.clear();
        }
    };
    let mut y = t.ymin;
    if gc.fill_rule == FillRule::EvenOdd {
        while y < t.ymax {
            if let Some((_, edges)) = sll.next_if(|b| b.0 == y) {
                t.load(edges);
            }
            let mut prev = AET;
            let mut aet = t.e[AET].next;
            while aet != NIL {
                let next = t.e[aet].next;
                if next == NIL {
                    break;
                }
                let x = t.e[aet].bres.minor;
                spans.push(span(x, y, t.e[next].bres.minor - x));
                flush(&mut spans, sink);
                t.evaluate(&mut aet, &mut prev, y);
                if aet != NIL {
                    t.evaluate(&mut aet, &mut prev, y);
                }
            }
            t.insertion_sort();
            y += 1;
        }
    } else {
        let mut fix_waet = false;
        while y < t.ymax {
            if let Some((_, edges)) = sll.next_if(|b| b.0 == y) {
                t.load(edges);
                t.compute_waet();
            }
            let mut prev = AET;
            let mut aet = t.e[AET].next;
            let mut wete = aet;
            while aet != NIL {
                // The next edge in the AET is the next in the winding one.
                if wete == aet {
                    let to = t.e[aet].next_wete;
                    if to == NIL {
                        break;
                    }
                    let x = t.e[aet].bres.minor;
                    spans.push(span(x, y, t.e[to].bres.minor - x));
                    flush(&mut spans, sink);
                    wete = to;
                    while aet != NIL && wete != aet {
                        fix_waet |= t.evaluate(&mut aet, &mut prev, y);
                    }
                    if wete == NIL {
                        break;
                    }
                    wete = t.e[wete].next_wete;
                }
                if aet == NIL {
                    break;
                }
                fix_waet |= t.evaluate(&mut aet, &mut prev, y);
            }
            // The winding links again if the order changed or an edge ended.
            if t.insertion_sort() || fix_waet {
                t.compute_waet();
                fix_waet = false;
            }
            y += 1;
        }
    }
    if !spans.is_empty() {
        sink.fill_spans(gc.fg, &spans);
    }
}

#[cfg(test)]
mod tests {
    use super::super::xvfb::{Op, check};
    use super::*;

    fn pts(v: &[(i32, i32)]) -> Vec<Point> {
        v.iter().map(|&(x, y)| Point { x, y }).collect()
    }

    fn gcs() -> Vec<Gc> {
        let mut v = Vec::new();
        for function in [3, 6] {
            for fill_rule in [FillRule::EvenOdd, FillRule::Winding] {
                v.push(Gc {
                    function,
                    fill_rule,
                    fg: 0xff_ffff,
                    ..Gc::default()
                });
            }
        }
        v
    }

    fn all_shapes(name: &str, p: &[(i32, i32)]) {
        for gc in gcs() {
            for shape in [Shape::Complex, Shape::Nonconvex] {
                check(name, &gc, &Op::FillPoly(shape, CoordMode::Origin, pts(p)));
            }
        }
    }

    fn convex(name: &str, p: &[(i32, i32)]) {
        all_shapes(name, p);
        for gc in gcs() {
            check(
                name,
                &gc,
                &Op::FillPoly(Shape::Convex, CoordMode::Origin, pts(p)),
            );
        }
    }

    #[test]
    fn convex_polygons() {
        convex("triangle", &[(10, 10), (60, 30), (20, 80)]);
        convex("triangle ccw", &[(10, 10), (20, 80), (60, 30)]);
        convex("square", &[(10, 10), (50, 10), (50, 50), (10, 50)]);
        convex("diamond", &[(80, 5), (130, 60), (80, 115), (30, 60)]);
        convex("thin", &[(5, 5), (150, 7), (6, 9)]);
        convex("steep", &[(70, 0), (72, 119), (69, 60)]);
        convex(
            "hexagon",
            &[(40, 10), (80, 10), (100, 50), (80, 90), (40, 90), (20, 50)],
        );
        convex("outside", &[(-50, -20), (200, 10), (170, 200), (-10, 140)]);
        convex(
            "huge",
            &[
                (-32768, -32768),
                (32767, -32768),
                (32767, 32767),
                (-32768, 32767),
            ],
        );
        convex(
            "huge triangle",
            &[(-30000, 50), (30000, -20000), (200, 30000)],
        );
        convex("collinear", &[(10, 10), (50, 50), (90, 90)]);
        convex("horizontal", &[(10, 10), (50, 10), (90, 10)]);
        convex("vertical", &[(10, 10), (10, 50), (10, 90)]);
        convex(
            "repeated",
            &[(10, 10), (10, 10), (60, 20), (60, 20), (30, 70), (10, 10)],
        );
        convex("one point", &[(10, 10)]);
        convex("two points", &[(10, 10), (40, 60)]);
        convex("same point", &[(10, 10), (10, 10), (10, 10)]);
    }

    #[test]
    fn general_polygons() {
        all_shapes(
            "concave",
            &[(10, 10), (80, 10), (40, 40), (80, 90), (10, 90)],
        );
        all_shapes(
            "star",
            &[(80, 5), (100, 110), (20, 40), (140, 40), (60, 110)],
        );
        all_shapes("bowtie", &[(10, 10), (100, 80), (100, 10), (10, 80)]);
        all_shapes(
            "spiral",
            &[
                (10, 10),
                (150, 10),
                (150, 110),
                (30, 110),
                (30, 30),
                (130, 30),
                (130, 90),
                (50, 90),
                (50, 50),
                (110, 50),
                (110, 70),
                (70, 70),
            ],
        );
        all_shapes(
            "loops twice",
            &[
                (20, 20),
                (100, 20),
                (100, 100),
                (20, 100),
                (20, 20),
                (100, 20),
                (100, 100),
                (20, 100),
            ],
        );
        all_shapes(
            "comb",
            &[
                (5, 100),
                (5, 10),
                (15, 90),
                (25, 10),
                (35, 90),
                (45, 10),
                (55, 90),
                (65, 10),
                (75, 100),
            ],
        );
        all_shapes("crossing", &[(0, 0), (159, 119), (0, 119), (159, 0)]);
        all_shapes(
            "outside",
            &[(-100, 60), (80, -100), (300, 60), (80, 50), (80, 300)],
        );
        all_shapes(
            "huge",
            &[
                (-32768, 0),
                (32767, 100),
                (-32768, 32767),
                (0, -32768),
                (32767, 32767),
            ],
        );
        all_shapes("degenerate", &[(10, 10), (50, 50), (10, 10), (50, 50)]);
    }

    #[test]
    fn not_convex_as_convex() {
        // Clients mustn't, but Xvfb does something: match it.
        for gc in gcs() {
            for p in [
                &[(10, 10), (80, 10), (40, 40), (80, 90), (10, 90)][..],
                &[(80, 5), (100, 110), (20, 40), (140, 40), (60, 110)],
                &[(10, 10), (100, 80), (100, 10), (10, 80)],
                &[(10, 50), (50, 10), (90, 50), (50, 30)],
                &[
                    (10, 10),
                    (150, 10),
                    (150, 110),
                    (30, 110),
                    (30, 30),
                    (130, 30),
                    (130, 90),
                ],
            ] {
                check(
                    "non-convex as convex",
                    &gc,
                    &Op::FillPoly(Shape::Convex, CoordMode::Origin, pts(p)),
                );
            }
        }
    }

    #[test]
    fn previous() {
        for gc in gcs() {
            for shape in [Shape::Complex, Shape::Nonconvex, Shape::Convex] {
                check(
                    "previous",
                    &gc,
                    &Op::FillPoly(
                        shape,
                        CoordMode::Previous,
                        pts(&[(20, 20), (60, 0), (0, 50), (-60, 0)]),
                    ),
                );
                check(
                    "previous wraps",
                    &gc,
                    &Op::FillPoly(
                        shape,
                        CoordMode::Previous,
                        pts(&[(20, 20), (32767, 0), (32767, 60), (-30, 0)]),
                    ),
                );
            }
        }
    }

    #[test]
    fn robust() {
        let mut b = super::super::xvfb::Buffer {
            px: vec![0; (super::super::xvfb::W * super::super::xvfb::H) as usize],
            function: 6,
        };
        // Many edges across the whole 16-bit range, both fill rules.
        let p: Vec<Point> = (0..200)
            .map(|i| Point {
                x: if i % 2 == 0 { -32768 } else { 32767 },
                y: (i * 7919) % 65536 - 32768,
            })
            .collect();
        for gc in &gcs()[..2] {
            for shape in [Shape::Complex, Shape::Convex] {
                fill_polygon(&mut b, gc, shape, CoordMode::Origin, &p);
                fill_polygon(&mut b, gc, shape, CoordMode::Previous, &p);
            }
        }
    }
}
