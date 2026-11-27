//! Wide and dashed arcs: X.org's mi/miarc.c (miPolyArc, miWideArc and what
//! they use, with ICEIL from mi/mifpoly.h and the wide-arc macros from
//! mi/mifillarc.h), ported. See COPYING in this directory for X.org's
//! notices.
//!
//! The arithmetic is X.org's, step for step: doubles where it uses doubles,
//! the same rounding, and the same 16-bit fields (xArc, the span points, the
//! per-row spans of an ellipse) where it keeps values in them.

use std::f64::consts::PI;

use super::*;

const FULLCIRCLE: i32 = 360 * 64;
const EPSILON: f64 = 0.000001;
#[allow(clippy::excessive_precision)]
const CUBED_ROOT_2: f64 = 1.2599210498948732038115849718451499938964;
#[allow(clippy::excessive_precision)]
const CUBED_ROOT_4: f64 = 1.5874010519681993173435330390930175781250;
const RIGHT_END: usize = 0;
const LEFT_END: usize = 1;
const DASH_MAP_SIZE: usize = 91;
/// More rows than any polygon of a real arc has: treated as X.org treats a
/// failed allocation.
const MAX_ROWS: i64 = 1 << 22;

fn isequal(a: f64, b: f64) -> bool {
    (a - b).abs() <= EPSILON
}

fn unequal(a: f64, b: f64) -> bool {
    (a - b).abs() > EPSILON
}

/// mifpoly.h's ICEIL. `as` saturates, as the C conversion does on arm64.
fn iceil(x: f64) -> i32 {
    let t = x as i32;
    if x == t as f64 || x < 0.0 {
        t
    } else {
        t.wrapping_add(1)
    }
}

fn bit(q: i32) -> i32 {
    if (0..31).contains(&q) { 1 << q } else { 0 }
}

/// xArc, as the wire has it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct XArc {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    angle1: i16,
    angle2: i16,
}

impl XArc {
    fn from(a: &Arc) -> XArc {
        XArc {
            x: a.x as i16,
            y: a.y as i16,
            width: a.w as u16,
            height: a.h as u16,
            angle1: a.angle1 as i16,
            angle2: a.angle2 as i16,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Spp {
    x: f64,
    y: f64,
}

/// SppArcRec.
#[derive(Clone, Copy, Debug, Default)]
struct SppArc {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    angle1: f64,
    angle2: f64,
}

/// miArcFaceRec: where an arc ends.
#[derive(Clone, Copy, Debug, Default)]
struct Face {
    clock: Spp,
    center: Spp,
    counter_clock: Spp,
}

#[derive(Clone, Copy, Debug, Default)]
struct ArcData {
    arc: XArc,
    render: bool,
    join: usize,
    cap: usize,
    self_join: bool,
    bounds: [Face; 2],
}

#[derive(Clone, Copy, Debug)]
struct CapRec {
    arc_index: i32,
    end: usize,
}

#[derive(Clone, Copy, Debug)]
struct JoinRec {
    arc_index0: i32,
    arc_index1: i32,
    phase0: usize,
    phase1: usize,
    end0: usize,
    end1: usize,
}

/// miPolyArcRec.
#[derive(Default)]
struct PolyArcs {
    arcs: Vec<ArcData>,
    caps: Vec<CapRec>,
    joins: Vec<JoinRec>,
}

impl PolyArcs {
    /// An arc by index; X.org reads whatever is there for an index past the
    /// end, we read an empty one.
    fn at(&self, i: i32) -> ArcData {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.arcs.get(i))
            .copied()
            .unwrap_or_default()
    }

    fn add_arc(&mut self, arc: XArc) -> usize {
        self.arcs.push(ArcData {
            arc,
            ..ArcData::default()
        });
        self.arcs.len() - 1
    }
}

/// miArcSpan: 16-bit, as in X.org.
#[derive(Clone, Copy, Debug, Default)]
struct ArcSpan {
    lx: i16,
    lw: i16,
    rx: i16,
    rw: i16,
}

/// miArcSpanData.
struct SpanData {
    spans: Vec<ArcSpan>,
    count1: i32,
    count2: i32,
    k: i32,
    top: bool,
    bot: bool,
    hole: bool,
}

impl SpanData {
    fn at(&self, i: usize) -> ArcSpan {
        self.spans.get(i).copied().unwrap_or_default()
    }

    fn at_mut(&mut self, i: usize) -> &mut ArcSpan {
        if i >= self.spans.len() {
            self.spans.resize(i + 1, ArcSpan::default());
        }
        &mut self.spans[i]
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Bound {
    min: f64,
    max: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct IBound {
    min: i32,
    max: i32,
}

fn bounded_le(v: f64, b: &Bound) -> bool {
    b.min <= v && v <= b.max
}

fn ibounded_le(v: i32, b: &IBound) -> bool {
    b.min <= v && v <= b.max
}

#[derive(Clone, Copy, Debug, Default)]
struct Line {
    m: f64,
    b: f64,
    valid: bool,
}

fn intersect_line(y: f64, l: &Line) -> f64 {
    l.m * y + l.b
}

#[derive(Clone, Copy, Debug, Default)]
struct ArcBound {
    ellipse: Bound,
    inner: Bound,
    outer: Bound,
    right: Bound,
    left: Bound,
    inneri: IBound,
    outeri: IBound,
}

#[derive(Clone, Copy, Debug, Default)]
struct Acc {
    tail_y: f64,
    h2: f64,
    w2: f64,
    h4: f64,
    w4: f64,
    h2mw2: f64,
    h2l: f64,
    w2l: f64,
    from_int_x: f64,
    from_int_y: f64,
    left: Line,
    right: Line,
    yorgu: i32,
    yorgl: i32,
    xorg: i32,
}

#[derive(Clone, Copy, Debug, Default)]
struct ArcDef {
    w: f64,
    h: f64,
    l: f64,
    a0: f64,
    a1: f64,
}

/// x ranges by row, for rows base.. (as realFindSpan grows finalSpans).
#[derive(Default)]
struct Rows {
    base: i32,
    rows: Vec<Vec<(i32, i32)>>,
}

impl Rows {
    fn row(&mut self, y: i32) -> &mut Vec<(i32, i32)> {
        if self.rows.is_empty() {
            self.base = y;
        }
        if y < self.base {
            let add = ((self.base - y) as usize).max(self.rows.len());
            self.rows
                .splice(0..0, std::iter::repeat_with(Vec::new).take(add));
            self.base -= add as i32;
        }
        let i = (y - self.base) as usize;
        if i >= self.rows.len() {
            let n = (i + 1).max(2 * self.rows.len());
            self.rows.resize_with(n, Vec::new);
        }
        &mut self.rows[i]
    }

    /// (y, row) in order, emptying it.
    fn take(&mut self) -> impl Iterator<Item = (i32, Vec<(i32, i32)>)> {
        let base = self.base;
        std::mem::take(&mut self.rows)
            .into_iter()
            .enumerate()
            .filter(|(_, r)| !r.is_empty())
            .map(move |(i, r)| (base + i as i32, r))
    }
}

/// The finalSpans accumulator: per row, x ranges merged as newFinalSpan
/// merges them.
#[derive(Default)]
struct Finals {
    rows: Rows,
}

impl Finals {
    /// newFinalSpan.
    fn add(&mut self, y: i32, mut xmin: i32, mut xmax: i32) {
        let row = self.rows.row(y);
        let mut old: Option<usize> = None;
        loop {
            let found =
                (0..row.len()).find(|&i| Some(i) != old && row[i].0 <= xmax && xmin <= row[i].1);
            let Some(i) = found else { break };
            let o = match old {
                Some(mut o) => {
                    row[o] = (row[i].0.min(xmin), row[i].1.max(xmax));
                    row.remove(i);
                    if i < o {
                        o -= 1;
                    }
                    o
                }
                None => {
                    row[i] = (row[i].0.min(xmin), row[i].1.max(xmax));
                    i
                }
            };
            old = Some(o);
            xmin = row[o].0;
            xmax = row[o].1;
        }
        if old.is_none() {
            row.push((xmin, xmax));
        }
    }
}

/// The scratch bitmap miWideArc draws into when the function reads the
/// destination: what it holds is pushed through the GC once per batch, so
/// each pixel is drawn once. Rows of disjoint x ranges, clipped to it.
struct Scratch {
    w: i32,
    h: i32,
    rows: Rows,
}

impl Scratch {
    fn add(&mut self, y: i32, x1: i32, x2: i32) {
        if y < 0 || y >= self.h {
            return;
        }
        let (mut a, mut b) = (x1.max(0), x2.min(self.w));
        if b <= a {
            return;
        }
        let row = self.rows.row(y);
        // Merge with everything overlapping or touching.
        let i = row.partition_point(|r| r.1 < a);
        while i < row.len() && row[i].0 <= b {
            a = a.min(row[i].0);
            b = b.max(row[i].1);
            row.remove(i);
        }
        row.insert(i, (a, b));
    }
}

/// Where spans go: the drawable through the GC (pGC->ops), or the scratch
/// bitmap. Holds the GC state the drawing reads.
struct Out<'a> {
    sink: &'a mut dyn Sink,
    pixel: u32,
    buf: Vec<Span>,
    scratch: Option<Scratch>,
    finals: Finals,
    line_width: i32,
    join_style: JoinStyle,
    cap_style: CapStyle,
}

impl Out<'_> {
    /// One span of a FillSpans call: the point is a DDXPointRec, 16 bits.
    fn span(&mut self, x: i32, y: i32, w: i32) {
        let (x, y) = (x as i16 as i32, y as i16 as i32);
        let Some(x2) = x.checked_add(w) else { return };
        if x2 <= x {
            return;
        }
        if let Some(s) = &mut self.scratch {
            s.add(y, x, x2);
        } else {
            // Nothing is wider than 32767 pixels: keep the sink's sums small.
            self.buf.push(Span {
                x,
                y,
                w: x2.min(x + 0x20000) - x,
            });
        }
    }

    /// PolyFillRect of one xRectangle (16-bit fields).
    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let (x, y, w, h) = (
            x as i16 as i32,
            y as i16 as i32,
            w as u16 as i32,
            h as u16 as i32,
        );
        if w == 0 || h == 0 {
            return;
        }
        if let Some(s) = &mut self.scratch {
            for yy in y.max(0)..(y + h).min(s.h) {
                s.add(yy, x, x + w);
            }
        } else {
            self.sink.fill_rects(self.pixel, &[Rect { x, y, w, h }]);
        }
    }

    fn flush(&mut self) {
        if !self.buf.is_empty() {
            self.sink.fill_spans(self.pixel, &self.buf);
            self.buf.clear();
        }
    }

    fn set_pixel(&mut self, pixel: u32) {
        self.flush();
        self.pixel = pixel;
    }

    /// PushPixels of the scratch bitmap at (xorg, yorg), then clear it.
    fn push(&mut self, xorg: i32, yorg: i32) {
        let Some(s) = &mut self.scratch else { return };
        let spans: Vec<Span> = s
            .rows
            .take()
            .flat_map(|(y, r)| {
                r.into_iter().map(move |(a, b)| Span {
                    x: a + xorg,
                    y: y + yorg,
                    w: b - a,
                })
            })
            .collect();
        if !spans.is_empty() {
            self.sink.fill_spans(self.pixel, &spans);
        }
    }

    fn width_or_1(&self) -> f64 {
        if self.line_width != 0 {
            self.line_width as f64
        } else {
            1.0
        }
    }
}

/// fillSpans: the accumulated final spans, drawn and forgotten.
fn fill_final_spans(out: &mut Out) {
    let rows: Vec<_> = out.finals.rows.take().collect();
    for (y, row) in rows {
        for (min, max) in row {
            if max <= min {
                continue;
            }
            out.span(min, y, max.wrapping_sub(min));
        }
    }
}

// The trig helpers, exact at the cardinal angles.

/// The Dsin macro.
fn dsin(d: f64) -> f64 {
    if d == 0.0 {
        0.0
    } else if d == 90.0 {
        1.0
    } else {
        (d * PI / 180.0).sin()
    }
}

/// The Dcos macro.
fn dcos(d: f64) -> f64 {
    if d == 0.0 {
        1.0
    } else if d == 90.0 {
        0.0
    } else {
        (d * PI / 180.0).cos()
    }
}

/// The mod macro: for a negative multiple of b it gives b, which matches no
/// case below, as in X.org.
fn cmod(a: i32, b: i32) -> i32 {
    if a >= 0 {
        a % b
    } else {
        b - a.wrapping_neg() % b
    }
}

/// miDcos.
fn mi_dcos(a: f64) -> f64 {
    if (a / 90.0).floor() == a / 90.0 {
        let i = (a / 90.0) as i32;
        match cmod(i, 4) {
            0 => return 1.0,
            1 => return 0.0,
            2 => return -1.0,
            3 => return 0.0,
            _ => {}
        }
    }
    (a * PI / 180.0).cos()
}

/// miDsin.
fn mi_dsin(a: f64) -> f64 {
    if (a / 90.0).floor() == a / 90.0 {
        let i = (a / 90.0) as i32;
        match cmod(i, 4) {
            0 => return 0.0,
            1 => return 1.0,
            2 => return 0.0,
            3 => return -1.0,
            _ => {}
        }
    }
    (a * PI / 180.0).sin()
}

/// miDasin.
fn mi_dasin(v: f64) -> f64 {
    if v == 0.0 {
        return 0.0;
    }
    if v == 1.0 {
        return 90.0;
    }
    if v == -1.0 {
        return -90.0;
    }
    v.asin() * (180.0 / PI)
}

/// miDatan2.
fn mi_datan2(dy: f64, dx: f64) -> f64 {
    if dy == 0.0 {
        if dx >= 0.0 {
            return 0.0;
        }
        180.0
    } else if dx == 0.0 {
        if dy > 0.0 {
            return 90.0;
        }
        -90.0
    } else if dy.abs() == dx.abs() {
        if dy > 0.0 {
            if dx > 0.0 {
                return 45.0;
            }
            135.0
        } else {
            if dx > 0.0 {
                return 315.0;
            }
            225.0
        }
    } else {
        dy.atan2(dx) * (180.0 / PI)
    }
}

/// PolyArc with a line width (and the arcs the thin-arc code can't draw,
/// with width 0): miWideArc. Each request's arcs as X.org gets them.
pub fn poly_arc(sink: &mut dyn Sink, gc: &Gc, arcs: &[Arc]) {
    if arcs.is_empty() {
        return;
    }
    let mut parcs: Vec<XArc> = arcs.iter().map(XArc::from).collect();
    wide_arc(sink, gc, &mut parcs);
}

/// miWideArc.
fn wide_arc(sink: &mut dyn Sink, gc: &Gc, parcs: &mut [XArc]) {
    let width = gc.line_width as u16 as i32;
    let mut out = Out {
        sink,
        pixel: gc.fg,
        buf: Vec::new(),
        scratch: None,
        finals: Finals::default(),
        line_width: width,
        join_style: gc.join_style,
        cap_style: gc.cap_style,
    };

    if width == 0 && gc.line_style == LineStyle::Solid {
        for a in parcs.iter() {
            arc_segment(&mut out, *a, None, None);
        }
        fill_final_spans(&mut out);
        out.flush();
        return;
    }

    // Whole solid ellipses at the start go the quick way.
    let mut first = 0;
    if gc.line_style == LineStyle::Solid {
        while parcs[first].width != 0
            && parcs[first].height != 0
            && (parcs[first].angle2 as i32 >= FULLCIRCLE
                || parcs[first].angle2 as i32 <= -FULLCIRCLE)
        {
            fill_wide_ellipse(&mut out, width, &parcs[first]);
            first += 1;
            if first == parcs.len() {
                out.flush();
                return;
            }
        }
    }
    let parcs = &mut parcs[first..];

    // A function that reads the destination: draw into a scratch bitmap,
    // pushed through the GC. Its size is limited to the drawable in X.org;
    // what that cuts off is outside the drawable, so it's left out here.
    let tricky = !matches!(gc.function & 15, 0 | 3 | 12 | 15);
    let (mut x_org, mut y_org) = (0, 0);
    if tricky {
        let (mut x_min, mut y_min) = (i16::MAX as i32, i16::MAX as i32);
        let (mut x_max, mut y_max) = (i16::MIN as i32, i16::MIN as i32);
        for a in parcs.iter() {
            x_min = x_min.min(a.x as i32);
            y_min = y_min.min(a.y as i32);
            x_max = x_max.max(a.x as i32 + a.width as i32);
            y_max = y_max.max(a.y as i32 + a.height as i32);
        }
        let half = (width + 1) / 2;
        x_min -= half;
        y_min -= half;
        x_max += half;
        y_max += half;
        x_org = x_min.max(0);
        y_org = y_min.max(0);
        let pw = x_max - x_org;
        let ph = y_max - y_org;
        if pw <= 0 || ph <= 0 {
            out.flush();
            return;
        }
        for a in parcs.iter_mut() {
            a.x = (a.x as i32 - x_org) as i16;
            a.y = (a.y as i32 - y_org) as i16;
        }
        out.flush();
        out.scratch = Some(Scratch {
            w: pw,
            h: ph,
            rows: Rows::default(),
        });
    }

    let fg = gc.fg;
    let mut bg = gc.bg;
    if matches!(gc.fill_style, FillStyle::Tiled | FillStyle::OpaqueStippled) {
        bg = fg;
    }

    let mut polys = compute_arcs(parcs, gc);
    let double = gc.line_style == LineStyle::DoubleDash;
    let mut cap = [0usize; 2];
    let mut join = [0usize; 2];
    for iphase in (0..=usize::from(double)).rev() {
        out.set_pixel(if iphase == 1 { bg } else { fg });
        let mut spdata: Option<SpanData> = None;
        let mut last = XArc::default();
        for i in 0..polys[iphase].arcs.len() {
            let a = polys[iphase].arcs[i].arc;
            if spdata.is_some() && (last.width != a.width || last.height != a.height) {
                spdata = None;
            }
            last = a;
            let mut bounds = polys[iphase].arcs[i].bounds;
            spdata = arc_segment(&mut out, a, Some(&mut bounds), spdata);
            polys[iphase].arcs[i].bounds = bounds;
            let d = polys[iphase].arcs[i];
            if !d.render {
                continue;
            }
            fill_final_spans(&mut out);
            // Self-joining arcs aren't capped.
            if d.self_join && cap[iphase] < d.cap {
                cap[iphase] += 1;
            }
            while cap[iphase] < d.cap {
                let Some(c) = polys[iphase].caps.get(cap[iphase]).copied() else {
                    break;
                };
                let a0 = polys[iphase].at(c.arc_index);
                arc_cap(
                    &mut out,
                    &a0.bounds[c.end],
                    a0.arc.x as i32,
                    a0.arc.y as i32,
                    a0.arc.width as f64 / 2.0,
                    a0.arc.height as f64 / 2.0,
                );
                cap[iphase] += 1;
            }
            while join[iphase] < d.join {
                let Some(j) = polys[iphase].joins.get(join[iphase]).copied() else {
                    break;
                };
                let a0 = polys[j.phase0].at(j.arc_index0);
                let a1 = polys[j.phase1].at(j.arc_index1);
                arc_join(
                    &mut out,
                    &a0.bounds[j.end0],
                    &a1.bounds[j.end1],
                    a0.arc.x as i32,
                    a0.arc.y as i32,
                    a0.arc.width as f64 / 2.0,
                    a0.arc.height as f64 / 2.0,
                    a1.arc.x as i32,
                    a1.arc.y as i32,
                    a1.arc.width as f64 / 2.0,
                    a1.arc.height as f64 / 2.0,
                );
                join[iphase] += 1;
            }
            if tricky {
                out.push(x_org, y_org);
            }
        }
    }
    out.flush();
}

/// miArcSegment: one arc (or dash) into the final spans; arcs without
/// width or height are drawn at once.
fn arc_segment(
    out: &mut Out,
    tarc: XArc,
    faces: Option<&mut [Face; 2]>,
    spdata: Option<SpanData>,
) -> Option<SpanData> {
    let mut l = out.line_width;
    if l == 0 {
        l = 1;
    }
    if tarc.width == 0 || tarc.height == 0 {
        draw_zero_arc(out, &tarc, l, faces);
        return spdata;
    }
    let a0 = tarc.angle1 as i32;
    let mut a1 = tarc.angle2 as i32;
    a1 = a1.clamp(-FULLCIRCLE, FULLCIRCLE);
    let (mut right, mut left) = (RIGHT_END, LEFT_END);
    let (mut start_angle, mut end_angle);
    if a1 < 0 {
        start_angle = a0 + a1;
        end_angle = a0;
        std::mem::swap(&mut right, &mut left);
    } else {
        start_angle = a0;
        end_angle = a0 + a1;
    }
    if start_angle < 0 {
        start_angle = FULLCIRCLE - (-start_angle) % FULLCIRCLE;
    }
    if start_angle >= FULLCIRCLE {
        start_angle %= FULLCIRCLE;
    }
    if end_angle < 0 {
        end_angle = FULLCIRCLE - (-end_angle) % FULLCIRCLE;
    }
    if end_angle > FULLCIRCLE {
        end_angle = (end_angle - 1) % FULLCIRCLE + 1;
    }
    if start_angle == end_angle && a1 != 0 {
        start_angle = 0;
        end_angle = FULLCIRCLE;
    }
    Some(draw_arc(
        out,
        &tarc,
        l,
        start_angle,
        end_angle,
        faces,
        right,
        left,
        spdata,
    ))
}

/// miComputeCircleSpans: the per-row spans of a wide circle, in integers.
fn compute_circle_spans(lw: i32, parc: &XArc, sp: &mut SpanData) {
    let width = parc.width as i32;
    let height = parc.height as i32;
    let mut doinner = -lw;
    let mut slw = width - doinner;
    let dy = height & 1;
    let dx = 1 - dy;
    // MIWIDEARCSETUP for the outer edge.
    let mut x = 0;
    let mut y = slw >> 1;
    let mut yk = y << 3;
    let xm = 8;
    let ym = 8;
    let mut xk;
    let mut e;
    if dy != 0 {
        xk = 0;
        e = if slw & 1 != 0 { -1 } else { -(y << 2) - 2 };
    } else {
        y += 1;
        yk += 4;
        xk = -4;
        e = if slw & 1 != 0 {
            -(y << 2) - 3
        } else {
            -(y << 3)
        };
    }
    let mut inslw = width + doinner;
    let (mut inx, mut ine, mut inxk, mut inyk) = (0, 0, 0, 0);
    let (inxm, inym) = (8, 8);
    if inslw > 0 {
        sp.hole = sp.top;
        // And for the inner one.
        let mut iny = inslw >> 1;
        inyk = iny << 3;
        if dy != 0 {
            inxk = 0;
            ine = if inslw & 1 != 0 { -1 } else { -(iny << 2) - 2 };
        } else {
            iny += 1;
            inyk += 4;
            inxk = -4;
            ine = if inslw & 1 != 0 {
                -(iny << 2) - 3
            } else {
                -(iny << 3)
            };
        }
    } else {
        sp.hole = false;
        doinner = -y;
    }
    sp.count1 = -doinner - i32::from(sp.top);
    sp.count2 = y + doinner;
    let mut si = 0usize;
    while y != 0 {
        // MIFILLARCSTEP(slw)
        e += yk;
        while e >= 0 {
            x += 1;
            xk -= xm;
            e += xk;
        }
        y -= 1;
        yk -= ym;
        slw = (x << 1) + dx;
        if e == xk && slw > 1 {
            slw -= 1;
        }
        let s = sp.at_mut(si);
        s.lx = (dy - x) as i16;
        doinner += 1;
        if doinner <= 0 {
            s.lw = slw as i16;
            s.rx = 0;
            s.rw = (s.lx as i32 + slw) as i16;
        } else {
            // MIFILLINARCSTEP(inslw)
            ine += inyk;
            while ine >= 0 {
                inx += 1;
                inxk -= inxm;
                ine += inxk;
            }
            inyk -= inym;
            inslw = (inx << 1) + dx;
            if ine == inxk && inslw > 1 {
                inslw -= 1;
            }
            s.lw = (x - inx) as i16;
            s.rx = (dy - inx + inslw) as i16;
            s.rw = (inx - x + slw - inslw) as i16;
        }
        si += 1;
    }
    if sp.bot {
        if sp.count2 != 0 {
            sp.count2 -= 1;
        } else {
            if si > 0 {
                let s = sp.at_mut(si - 1);
                if lw > height {
                    let v = (-((lw - height) >> 1)) as i16;
                    s.rx = v;
                    s.rw = v;
                } else {
                    s.rw = 0;
                }
            }
            sp.count1 -= 1;
        }
    }
}

/// miComputeEllipseSpans: the per-row spans of a wide ellipse, from the
/// quartic (see miarc.c).
fn compute_ellipse_spans(lw: i32, parc: &XArc, sp: &mut SpanData) {
    let w = parc.width as f64 / 2.0;
    let h = parc.height as f64 / 2.0;
    let r = lw as f64 / 2.0;
    let rs = r * r;
    let hs = h * h;
    let wh = w * w - hs;
    let mut nk = w * r;
    let vk = (nk * hs) / (wh + wh);
    let hf = hs * hs;
    nk = (hf - nk * nk) / wh;
    let fk = hf / wh;
    let hepp = h + EPSILON;
    let hepm = h - EPSILON;
    let mut k = h + ((lw - 1) >> 1) as f64;
    let xorg = if parc.width & 1 != 0 { 0.5 } else { 0.0 };
    let mut si = 0usize;
    if sp.top {
        let s = sp.at_mut(si);
        s.lx = 0;
        s.lw = 1;
        si += 1;
    }
    sp.count1 = 0;
    sp.count2 = 0;
    sp.hole = sp.top
        && (parc.height as i32).wrapping_mul(lw)
            <= (parc.width as i32).wrapping_mul(parc.width as i32)
        && lw < parc.height as i32;
    let mut outx = 0.0;
    while k > 0.0 {
        let n = (k * k + nk) / 6.0;
        let nc = n * n * n;
        let vr = vk * k;
        let mut t = nc + vr * vr;
        let mut d = nc + t;
        let z;
        let flip;
        if d < 0.0 {
            d = nc;
            let mut b = n;
            if (b < 0.0) == (t < 0.0) {
                b = -b;
                d = -d;
            }
            z = n - 2.0 * b * ((-t / d).acos() / 3.0).cos();
            flip = if (z < 0.0) == (vr < 0.0) { 2 } else { 1 };
        } else {
            d = vr * d.sqrt();
            z = n + (t + d).cbrt() + (t - d).cbrt();
            flip = 0;
        }
        let a = ((z + z) - nk).sqrt();
        let tt = (fk - z) * k / a;
        let mut inx = 0.0;
        let mut solution = false;
        let mut b = -a + k;
        d = b * b - 4.0 * (z + tt);
        if d >= 0.0 {
            d = d.sqrt();
            let mut y = (b + d) / 2.0;
            if y >= 0.0 && y < hepp {
                solution = true;
                if y > hepm {
                    y = h;
                }
                t = y / h;
                let x = w * (1.0 - (t * t)).sqrt();
                t = k - y;
                if rs - (t * t) >= 0.0 {
                    t = (rs - (t * t)).sqrt();
                } else {
                    t = 0.0;
                }
                if flip == 2 {
                    inx = x - t;
                } else {
                    outx = x + t;
                }
            }
        }
        b = a + k;
        d = b * b - 4.0 * (z - tt);
        // X.org's workaround for precision lost near the axis.
        if d < 0.0 && !solution {
            d = 0.0;
        }
        if d >= 0.0 {
            d = d.sqrt();
            let mut y = (b + d) / 2.0;
            if y < hepp {
                if y > hepm {
                    y = h;
                }
                t = y / h;
                let x = w * (1.0 - (t * t)).sqrt();
                t = k - y;
                if rs - (t * t) >= 0.0 {
                    inx = x - (rs - (t * t)).sqrt();
                } else {
                    inx = x;
                }
            }
            y = (b - d) / 2.0;
            if y >= 0.0 {
                if y > hepm {
                    y = h;
                }
                t = y / h;
                let x = w * (1.0 - (t * t)).sqrt();
                t = k - y;
                if rs - (t * t) >= 0.0 {
                    t = (rs - (t * t)).sqrt();
                } else {
                    t = 0.0;
                }
                if flip == 1 {
                    inx = x - t;
                } else {
                    outx = x + t;
                }
            }
        }
        let s = sp.at_mut(si);
        s.lx = iceil(xorg - outx) as i16;
        let lx = s.lx as i32;
        if inx <= 0.0 {
            s.lw = iceil(xorg + outx).wrapping_sub(lx) as i16;
            s.rx = iceil(xorg + inx) as i16;
            s.rw = iceil(xorg - inx).wrapping_neg() as i16;
            sp.count1 += 1;
        } else {
            s.lw = iceil(xorg - inx).wrapping_sub(lx) as i16;
            s.rx = iceil(xorg + inx) as i16;
            let rx = s.rx as i32;
            s.rw = iceil(xorg + outx).wrapping_sub(rx) as i16;
            sp.count2 += 1;
        }
        si += 1;
        k -= 1.0;
    }
    if sp.bot {
        outx = w + r;
        let mut inx;
        if r >= h && r <= w {
            inx = 0.0;
        } else if nk < 0.0 && -nk < hs {
            inx = w * (1.0 + nk / hs).sqrt() - (rs + nk).sqrt();
            if inx > w - r {
                inx = w - r;
            }
        } else {
            inx = w - r;
        }
        let s = sp.at_mut(si);
        s.lx = iceil(xorg - outx) as i16;
        let lx = s.lx as i32;
        if inx <= 0.0 {
            s.lw = iceil(xorg + outx).wrapping_sub(lx) as i16;
            s.rx = iceil(xorg + inx) as i16;
            s.rw = iceil(xorg - inx).wrapping_neg() as i16;
        } else {
            s.lw = iceil(xorg - inx).wrapping_sub(lx) as i16;
            s.rx = iceil(xorg + inx) as i16;
            let rx = s.rx as i32;
            s.rw = iceil(xorg + outx).wrapping_sub(rx) as i16;
        }
    }
    if sp.hole {
        if let Ok(i) = usize::try_from(sp.count1) {
            let s = sp.at_mut(i);
            s.lw = s.lx.wrapping_neg();
            s.rx = 1;
            s.rw = s.lw;
        }
        sp.count1 -= 1;
        sp.count2 += 1;
    }
}

/// tailX: the x of the inner edge's tail at row K.
fn tail_x(k: f64, def: &ArcDef, bounds: &ArcBound, acc: &Acc) -> f64 {
    let w = def.w;
    let h = def.h;
    let r = def.l;
    let rs = r * r;
    let hs = acc.h2;
    let wh = -acc.h2mw2;
    let mut nk = def.w * r;
    let vk = (nk * hs) / (wh + wh);
    let hf = acc.h4;
    nk = (hf - nk * nk) / wh;
    if k == 0.0 {
        if nk < 0.0 && -nk < hs {
            let xs0 = w * (1.0 + nk / hs).sqrt() - (rs + nk).sqrt();
            let xs1 = w - r;
            if acc.left.valid
                && bounded_le(k, &bounds.left)
                && !bounded_le(k, &bounds.outer)
                && xs0 >= 0.0
                && xs1 >= 0.0
            {
                return xs1;
            }
            if acc.right.valid
                && bounded_le(k, &bounds.right)
                && !bounded_le(k, &bounds.inner)
                && xs0 <= 0.0
                && xs1 <= 0.0
            {
                return xs1;
            }
            return xs0;
        }
        return w - r;
    }
    let fk = hf / wh;
    let hepp = h + EPSILON;
    let hepm = h - EPSILON;
    let n = (k * k + nk) / 6.0;
    let nc = n * n * n;
    let vr = vk * k;
    let mut xs = [0.0f64; 3];
    let mut xp = 0usize;
    let mut t = nc + vr * vr;
    let mut d = nc + t;
    let z;
    let flip;
    if d < 0.0 {
        d = nc;
        let mut b = n;
        if (b < 0.0) == (t < 0.0) {
            b = -b;
            d = -d;
        }
        z = n - 2.0 * b * ((-t / d).acos() / 3.0).cos();
        flip = if (z < 0.0) == (vr < 0.0) { 2 } else { 1 };
    } else {
        d = vr * d.sqrt();
        z = n + (t + d).cbrt() + (t - d).cbrt();
        flip = 0;
    }
    let a = ((z + z) - nk).sqrt();
    let tt = (fk - z) * k / a;
    let mut solution = false;
    let mut b = -a + k;
    d = b * b - 4.0 * (z + tt);
    if d >= 0.0 && flip == 2 {
        d = d.sqrt();
        let mut y = (b + d) / 2.0;
        if y >= 0.0 && y < hepp {
            solution = true;
            if y > hepm {
                y = h;
            }
            t = y / h;
            let x = w * (1.0 - (t * t)).sqrt();
            t = k - y;
            if rs - (t * t) >= 0.0 {
                t = (rs - (t * t)).sqrt();
            } else {
                t = 0.0;
            }
            xs[xp] = x - t;
            xp += 1;
        }
    }
    b = a + k;
    d = b * b - 4.0 * (z - tt);
    if d < 0.0 && !solution {
        d = 0.0;
    }
    if d >= 0.0 {
        d = d.sqrt();
        let mut y = (b + d) / 2.0;
        if y < hepp {
            if y > hepm {
                y = h;
            }
            t = y / h;
            let x = w * (1.0 - (t * t)).sqrt();
            t = k - y;
            xs[xp] = if rs - (t * t) >= 0.0 {
                x - (rs - (t * t)).sqrt()
            } else {
                x
            };
            xp += 1;
        }
        y = (b - d) / 2.0;
        if y >= 0.0 && flip == 1 {
            if y > hepm {
                y = h;
            }
            t = y / h;
            let x = w * (1.0 - (t * t)).sqrt();
            t = k - y;
            if rs - (t * t) >= 0.0 {
                t = (rs - (t * t)).sqrt();
            } else {
                t = 0.0;
            }
            xs[xp.min(2)] = x - t;
            xp += 1;
        }
    }
    if xp > 1 {
        if acc.left.valid
            && bounded_le(k, &bounds.left)
            && !bounded_le(k, &bounds.outer)
            && xs[0] >= 0.0
            && xs[1] >= 0.0
        {
            return xs[1];
        }
        if acc.right.valid
            && bounded_le(k, &bounds.right)
            && !bounded_le(k, &bounds.inner)
            && xs[0] <= 0.0
            && xs[1] <= 0.0
        {
            return xs[1];
        }
    }
    xs[0]
}

/// miComputeWideEllipse.
fn compute_wide_ellipse(mut lw: i32, parc: &XArc) -> SpanData {
    if lw == 0 {
        lw = 1;
    }
    let k = (parc.height as i32 >> 1) + ((lw - 1) >> 1);
    let mut sp = SpanData {
        spans: vec![ArcSpan::default(); (k + 2).max(0) as usize],
        count1: 0,
        count2: 0,
        k,
        top: (lw & 1) == 0 && (parc.width & 1) == 0,
        bot: (parc.height & 1) == 0,
        hole: false,
    };
    if parc.width == parc.height {
        compute_circle_spans(lw, parc, &mut sp);
    } else {
        compute_ellipse_spans(lw, parc, &mut sp);
    }
    sp
}

/// miFillWideEllipse: a whole solid wide ellipse, straight to FillSpans.
fn fill_wide_ellipse(out: &mut Out, lw: i32, parc: &XArc) {
    let sp = compute_wide_ellipse(lw, parc);
    let xorg = parc.x as i32 + (parc.width as i32 >> 1);
    let mut yorgu = parc.y as i32 + (parc.height as i32 >> 1);
    let mut yorgl = yorgu + (parc.height as i32 & 1);
    yorgu -= sp.k;
    yorgl += sp.k;
    let mut si = 0usize;
    if sp.top {
        out.span(xorg, yorgu - 1, 1);
        si += 1;
    }
    for _ in 0..sp.count1.max(0) {
        let s = sp.at(si);
        out.span(xorg + s.lx as i32, yorgu, s.lw as i32);
        out.span(xorg + s.lx as i32, yorgl, s.lw as i32);
        yorgu += 1;
        yorgl -= 1;
        si += 1;
    }
    if sp.hole {
        out.span(xorg, yorgl, 1);
    }
    for _ in 0..sp.count2.max(0) {
        let s = sp.at(si);
        out.span(xorg + s.lx as i32, yorgu, s.lw as i32);
        out.span(xorg + s.rx as i32, yorgu, s.rw as i32);
        out.span(xorg + s.lx as i32, yorgl, s.lw as i32);
        out.span(xorg + s.rx as i32, yorgl, s.rw as i32);
        yorgu += 1;
        yorgl -= 1;
        si += 1;
    }
    if sp.bot {
        let s = sp.at(si);
        out.span(xorg + s.lx as i32, yorgu, s.lw as i32);
        if s.rw > 0 {
            out.span(xorg + s.rx as i32, yorgu, s.rw as i32);
        }
    }
}

/// GetFPolyYBounds: the index of the top point, and the rows spanned.
fn get_fpoly_ybounds(pts: &[Spp], y_ftrans: f64) -> (usize, i32, i32) {
    let mut imin = 0;
    let mut ymin = pts[0].y;
    let mut ymax = pts[0].y;
    for (i, p) in pts.iter().enumerate().skip(1) {
        if p.y < ymin {
            imin = i;
            ymin = p.y;
        }
        if p.y > ymax {
            ymax = p.y;
        }
    }
    (imin, iceil(ymin + y_ftrans), iceil(ymax + y_ftrans - 1.0))
}

/// miFillSppPoly: fill a convex polygon of sub-pixel points.
fn fill_spp_poly(
    out: &mut Out,
    pts: &[Spp],
    x_trans: i32,
    y_trans: i32,
    x_ftrans: f64,
    y_ftrans: f64,
) {
    let count = pts.len();
    if count == 0 {
        return;
    }
    let (imin, ymin, ymax) = get_fpoly_ybounds(pts, y_ftrans);
    let rows = ymax as i64 - ymin as i64 + 1;
    if count < 3 || rows <= 0 || rows > MAX_ROWS {
        return;
    }
    let (mut xl, mut xr, mut ml, mut mr) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut marked = vec![0i32; count];
    let mut nextleft = imin;
    let mut nextright = imin;
    marked[imin] = -1;
    let mut y = iceil(pts[nextleft].y + y_ftrans);
    // X.org's loop ends by itself; this only bounds it against the absurd.
    let mut guard = 4 * count as i64 + 2 * rows + 64;
    'scan: loop {
        guard -= 1;
        if guard < 0 {
            break;
        }
        'body: {
            // A new left edge?
            if (y as f64 > pts[nextleft].y + y_ftrans
                || isequal(y as f64, pts[nextleft].y + y_ftrans))
                && marked[nextleft] != 1
            {
                marked[nextleft] += 1;
                let left = nextleft;
                nextleft += 1;
                if nextleft >= count {
                    nextleft = 0;
                }
                let dy = pts[nextleft].y - pts[left].y;
                if dy != 0.0 {
                    ml = (pts[nextleft].x - pts[left].x) / dy;
                    let dy = y as f64 - (pts[left].y + y_ftrans);
                    xl = (pts[left].x + x_ftrans) + ml * if dy > 0.0 { dy } else { 0.0 };
                }
            }
            // A new right edge?
            if (y as f64 > pts[nextright].y + y_ftrans)
                || (isequal(y as f64, pts[nextright].y + y_ftrans) && marked[nextright] != 1)
            {
                marked[nextright] += 1;
                let right = nextright;
                nextright = if nextright == 0 {
                    count - 1
                } else {
                    nextright - 1
                };
                let dy = pts[nextright].y - pts[right].y;
                if dy != 0.0 {
                    mr = (pts[nextright].x - pts[right].x) / dy;
                    let dy = y as f64 - (pts[right].y + y_ftrans);
                    xr = (pts[right].x + x_ftrans) + mr * if dy > 0.0 { dy } else { 0.0 };
                }
            }
            let (a, b) = (pts[nextleft].y, pts[nextright].y);
            let i = ((if a < b { a } else { b }) + y_ftrans) - y as f64;
            let mut j;
            if i < EPSILON {
                if marked[nextleft] != 0 && marked[nextright] != 0 {
                    break 'scan;
                }
                break 'body;
            } else {
                j = i as i32;
                if j == 0 {
                    j += 1;
                }
            }
            while j > 0 {
                let cxl = iceil(xl);
                let cxr = iceil(xr);
                if xl < xr {
                    out.span(
                        cxl.wrapping_add(x_trans),
                        y.wrapping_add(y_trans),
                        cxr.wrapping_sub(cxl),
                    );
                } else {
                    out.span(
                        cxr.wrapping_add(x_trans),
                        y.wrapping_add(y_trans),
                        cxl.wrapping_sub(cxr),
                    );
                }
                y = y.wrapping_add(1);
                xl += ml;
                xr += mr;
                j -= 1;
            }
        }
        if y > ymax {
            break;
        }
    }
}

/// angleBetween.
fn angle_between(center: Spp, p1: Spp, p2: Spp) -> f64 {
    let a1 = mi_datan2(-(p1.y - center.y), p1.x - center.x);
    let a2 = mi_datan2(-(p2.y - center.y), p2.x - center.x);
    let mut a = a2 - a1;
    if a <= -180.0 {
        a += 360.0;
    } else if a > 180.0 {
        a -= 360.0;
    }
    a
}

/// translateBounds.
fn translate_bounds(b: &mut Face, x: i32, y: i32, mut fx: f64, mut fy: f64) {
    fx += x as f64;
    fy += y as f64;
    b.clock.x -= fx;
    b.clock.y -= fy;
    b.center.x -= fx;
    b.center.y -= fy;
    b.counter_clock.x -= fx;
    b.counter_clock.y -= fy;
}

/// miArcJoin: the join between two arcs' faces.
#[allow(clippy::too_many_arguments)]
fn arc_join(
    out: &mut Out,
    p_left: &Face,
    p_right: &Face,
    x_org_left: i32,
    y_org_left: i32,
    xf_left: f64,
    yf_left: f64,
    x_org_right: i32,
    y_org_right: i32,
    xf_right: f64,
    yf_right: f64,
) {
    let x_org = (x_org_right + x_org_left) / 2;
    let y_org = (y_org_right + y_org_left) / 2;
    let x_ftrans = (xf_left + xf_right) / 2.0;
    let y_ftrans = (yf_left + yf_right) / 2.0;
    let mut right = *p_right;
    translate_bounds(
        &mut right,
        x_org - x_org_right,
        y_org - y_org_right,
        x_ftrans - xf_right,
        y_ftrans - yf_right,
    );
    let mut left = *p_left;
    translate_bounds(
        &mut left,
        x_org - x_org_left,
        y_org - y_org_left,
        x_ftrans - xf_left,
        y_ftrans - yf_left,
    );

    if right.clock.x == left.counter_clock.x && right.clock.y == left.counter_clock.y {
        return;
    }
    let center = right.center;
    let mut a = angle_between(center, right.clock, left.counter_clock);
    let (corner, other_corner);
    if 0.0 <= a && a <= 180.0 {
        corner = right.clock;
        other_corner = left.counter_clock;
    } else {
        a = angle_between(center, left.clock, right.counter_clock);
        corner = left.clock;
        other_corner = right.counter_clock;
    }
    let poly: Vec<Spp>;
    match out.join_style {
        JoinStyle::Round => {
            let width = out.width_or_1();
            let arc = SppArc {
                x: center.x - width / 2.0,
                y: center.y - width / 2.0,
                width,
                height: width,
                angle1: -mi_datan2(corner.y - center.y, corner.x - center.x),
                angle2: a,
            };
            let mut pts = vec![other_corner, center, corner];
            let cpt = get_arc_pts(&arc, 3, &mut pts);
            if cpt != 0 {
                // As X.org: the first cpt points.
                fill_spp_poly(
                    out,
                    &pts[..cpt.min(pts.len())],
                    x_org,
                    y_org,
                    x_ftrans,
                    y_ftrans,
                );
            }
            return;
        }
        // Arcs less than 11 degrees apart aren't mitred.
        JoinStyle::Miter if a < 169.0 => {
            let bc2 = (corner.x - other_corner.x) * (corner.x - other_corner.x)
                + (corner.y - other_corner.y) * (corner.y - other_corner.y);
            let ec2 = bc2 / 4.0;
            let ac2 = (corner.x - center.x) * (corner.x - center.x)
                + (corner.y - center.y) * (corner.y - center.y);
            let ae = (ac2 - ec2).sqrt();
            let de = ec2 / ae;
            let e = Spp {
                x: (corner.x + other_corner.x) / 2.0,
                y: (corner.y + other_corner.y) / 2.0,
            };
            let p3 = Spp {
                x: e.x + de * (e.x - center.x) / ae,
                y: e.y + de * (e.y - center.y) / ae,
            };
            poly = vec![corner, center, other_corner, p3, corner];
        }
        _ => {
            poly = vec![corner, center, other_corner, corner];
        }
    }
    fill_spp_poly(out, &poly, x_org, y_org, x_ftrans, y_ftrans);
}

/// miArcCap.
fn arc_cap(out: &mut Out, face: &Face, x_org: i32, y_org: i32, x_ftrans: f64, y_ftrans: f64) {
    let corner = face.clock;
    let other_corner = face.counter_clock;
    let center = face.center;
    match out.cap_style {
        CapStyle::Projecting => {
            let poly = [
                other_corner,
                corner,
                Spp {
                    x: corner.x - (center.y - corner.y),
                    y: corner.y + (center.x - corner.x),
                },
                Spp {
                    x: other_corner.x - (other_corner.y - center.y),
                    y: other_corner.y + (other_corner.x - center.x),
                },
                other_corner,
            ];
            fill_spp_poly(out, &poly, x_org, y_org, x_ftrans, y_ftrans);
        }
        CapStyle::Round => {
            // miRoundCap just needs these to differ.
            let end = Spp {
                x: center.x + 100.0,
                y: center.y,
            };
            round_cap(
                out,
                center,
                end,
                corner,
                other_corner,
                x_org,
                y_org,
                x_ftrans,
                y_ftrans,
            );
        }
        _ => {}
    }
}

/// miRoundCap (its origin arguments un-negated: X.org negates them twice).
#[allow(clippy::too_many_arguments)]
fn round_cap(
    out: &mut Out,
    center: Spp,
    end: Spp,
    corner: Spp,
    other_corner: Spp,
    x_org: i32,
    y_org: i32,
    x_ftrans: f64,
    y_ftrans: f64,
) {
    let width = out.width_or_1();
    let mut arc = SppArc {
        x: center.x - width / 2.0,
        y: center.y - width / 2.0,
        width,
        height: width,
        angle1: 0.0,
        angle2: 0.0,
    };
    arc.angle1 = -mi_datan2(corner.y - center.y, corner.x - center.x);
    if isequal(center.x, end.x) && isequal(center.y, end.y) {
        arc.angle2 = -180.0;
    } else {
        arc.angle2 = -mi_datan2(other_corner.y - center.y, other_corner.x - center.x) - arc.angle1;
        if arc.angle2 < 0.0 {
            arc.angle2 += 360.0;
        }
    }
    let mut pts = Vec::new();
    let cpt = get_arc_pts(&arc, 0, &mut pts);
    if cpt != 0 {
        fill_spp_poly(
            out,
            &pts[..cpt.min(pts.len())],
            x_org,
            y_org,
            x_ftrans,
            y_ftrans,
        );
    }
}

/// miGetArcPts: an arc as points, appended after the first `cpt`. Returns
/// how many it added.
fn get_arc_pts(parc: &SppArc, cpt: usize, pts: &mut Vec<Spp>) -> usize {
    let st = -parc.angle1;
    let et = -parc.angle2;
    let mut cdt = parc.width;
    if parc.height > cdt {
        cdt = parc.height;
    }
    cdt /= 2.0;
    if cdt <= 0.0 {
        return 0;
    }
    if cdt < 1.0 {
        cdt = 1.0;
    }
    let mut dt = mi_dasin(1.0 / cdt);
    let count = (et / dt) as i32;
    let count = count.unsigned_abs() as i64 + 1;
    dt = et / count as f64;
    let count = count + 1;
    if count > MAX_ROWS {
        return 0;
    }
    let count = count as usize;
    let cdt = 2.0 * mi_dcos(dt);
    pts.resize(cpt + count, Spp::default());

    let mut xc = parc.width / 2.0;
    let mut yc = parc.height / 2.0;
    let mut x0 = xc * mi_dcos(st);
    let mut y0 = yc * mi_dsin(st);
    let mut x1 = xc * mi_dcos(st + dt);
    let mut y1 = yc * mi_dsin(st + dt);
    xc += parc.x;
    yc += parc.y;
    pts[cpt] = Spp {
        x: xc + x0,
        y: yc + y0,
    };
    pts[cpt + 1] = Spp {
        x: xc + x1,
        y: yc + y1,
    };
    for p in pts.iter_mut().skip(cpt + 2).take(count - 2) {
        let x2 = cdt * x1 - x0;
        let y2 = cdt * y1 - y0;
        *p = Spp {
            x: xc + x2,
            y: yc + y2,
        };
        x0 = x1;
        y0 = y1;
        x1 = x2;
        y1 = y2;
    }
    // The last point exactly.
    let last = cpt + count - 1;
    if parc.angle2.abs() >= 360.0 {
        pts[last] = pts[0];
    } else {
        pts[last] = Spp {
            x: mi_dcos(st + et) * parc.width / 2.0 + xc,
            y: mi_dsin(st + et) * parc.height / 2.0 + yc,
        };
    }
    count
}

// Dashes: arc length from a polygon approximation of a quadrant.

fn dash_index_to_angle(di: usize) -> f64 {
    (di as f64 * 90.0) / (DASH_MAP_SIZE as f64 - 1.0)
}

fn x_angle_to_dash_index(xa: i32) -> i32 {
    (xa * (DASH_MAP_SIZE as i32 - 1)) / (90 * 64)
}

fn dash_index_to_x_angle(di: i32) -> i32 {
    (di * (90 * 64)) / (DASH_MAP_SIZE as i32 - 1)
}

const DASH_X_ANGLE_STEP: f64 = (90 * 64) as f64 / (DASH_MAP_SIZE as f64 - 1.0);

/// computeDashMap.
fn compute_dash_map(arcp: &XArc, map: &mut [f64; DASH_MAP_SIZE]) {
    let (mut prevx, mut prevy) = (0.0f64, 0.0f64);
    for di in 0..DASH_MAP_SIZE {
        let a = dash_index_to_angle(di);
        let x = (arcp.width as f64 / 2.0) * mi_dcos(a);
        let y = (arcp.height as f64 / 2.0) * mi_dsin(a);
        if di == 0 {
            map[di] = 0.0;
        } else {
            map[di] = map[di - 1] + (x - prevx).hypot(y - prevy);
        }
        prevx = x;
        prevy = y;
    }
}

fn map_at(map: &[f64; DASH_MAP_SIZE], i: i32) -> f64 {
    map[i.clamp(0, DASH_MAP_SIZE as i32 - 1) as usize]
}

/// angleToLength.
fn angle_to_length(mut angle: i32, map: &[f64; DASH_MAP_SIZE]) -> f64 {
    let sidelen = map[DASH_MAP_SIZE - 1];
    let mut totallen = 0.0;
    let mut odd_side = false;
    if angle >= 0 {
        while angle >= 90 * 64 {
            angle -= 90 * 64;
            totallen += sidelen;
            odd_side = !odd_side;
        }
    } else {
        while angle < 0 {
            angle += 90 * 64;
            totallen -= sidelen;
            odd_side = !odd_side;
        }
    }
    if odd_side {
        angle = 90 * 64 - angle;
    }
    let di = x_angle_to_dash_index(angle);
    let excess = angle - dash_index_to_x_angle(di);
    let mut len = map_at(map, di);
    if excess > 0 {
        let excesslen =
            (map_at(map, di + 1) - map_at(map, di)) * (excess as f64) / DASH_X_ANGLE_STEP;
        len += excesslen;
    }
    if odd_side {
        totallen += sidelen - len;
    } else {
        totallen += len;
    }
    totallen
}

/// lengthToAngle: `len` along the arc, maybe more than once round.
fn length_to_angle(mut len: f64, map: &[f64; DASH_MAP_SIZE]) -> i32 {
    let sidelen = map[DASH_MAP_SIZE - 1];
    let mut angle: i32 = 0;
    let mut odd_side = false;
    if len >= 0.0 {
        if sidelen == 0.0 {
            return 2 * FULLCIRCLE;
        }
        while len >= sidelen {
            angle = angle.wrapping_add(90 * 64);
            len -= sidelen;
            odd_side = !odd_side;
        }
    } else {
        if sidelen == 0.0 {
            return -2 * FULLCIRCLE;
        }
        while len < 0.0 {
            angle = angle.wrapping_sub(90 * 64);
            len += sidelen;
            odd_side = !odd_side;
        }
    }
    if odd_side {
        len = sidelen - len;
    }
    let (mut a0, mut a1) = (0i32, DASH_MAP_SIZE as i32 - 1);
    // Binary search for the nearest computed length.
    while a1 - a0 > 1 {
        let a = (a0 + a1) / 2;
        if len > map_at(map, a) {
            a0 = a;
        } else {
            a1 = a;
        }
    }
    let mut angleexcess = dash_index_to_x_angle(a0);
    angleexcess = (angleexcess as f64
        + (len - map_at(map, a0)) / (map_at(map, a0 + 1) - map_at(map, a0)) * DASH_X_ANGLE_STEP)
        as i32;
    if odd_side {
        angle.wrapping_add((90 * 64) - angleexcess)
    } else {
        angle.wrapping_add(angleexcess)
    }
}

/// computeAngleFromPath: where a dash of *lenp ends, from start_angle;
/// *lenp keeps what's left of it past end_angle.
fn compute_angle_from_path(
    start_angle: i32,
    end_angle: i32,
    map: &[f64; DASH_MAP_SIZE],
    lenp: &mut i32,
    backwards: bool,
) -> i32 {
    let mut a0 = start_angle;
    let mut a1 = end_angle;
    let mut len = *lenp;
    if backwards {
        a0 = FULLCIRCLE - a0;
        a1 = FULLCIRCLE - a1;
    }
    if a1 < a0 {
        a1 += FULLCIRCLE;
    }
    let len0 = angle_to_length(a0, map);
    let mut a = length_to_angle(len0 + len as f64, map);
    if a > a1 {
        a = a1;
        len = (len as f64 - (angle_to_length(a1, map) - len0)) as i32;
    } else {
        len = 0;
    }
    if backwards {
        a = FULLCIRCLE.wrapping_sub(a);
    }
    *lenp = len;
    a
}

#[derive(Clone, Copy, PartialEq)]
enum ArcType {
    Horizontal,
    Vertical,
    Other,
}

struct EndPoints {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    self_join: bool,
}

/// miComputeArcs: the arcs as dashes (in one list, or two for double
/// dashes), with the caps and joins between them.
fn compute_arcs(parcs: &[XArc], gc: &Gc) -> Vec<PolyArcs> {
    let is_dashed = gc.line_style != LineStyle::Solid;
    let double = gc.line_style == LineStyle::DoubleDash;
    let default_dash = [4u8];
    let dashes: &[u8] = if gc.dashes.is_empty() {
        &default_dash
    } else {
        &gc.dashes
    };
    // A zero entry would never end; the protocol doesn't allow them.
    let dash = |i: usize| (dashes[i] as i32).max(1);
    let ndash = dashes.len();
    let mut dash_offset = gc.dash_offset as u16 as i32;
    let narcs = parcs.len();

    let data: Vec<EndPoints> = parcs
        .iter()
        .map(|p| {
            let a0 = p.angle1 as f64 / 64.0;
            let angle2 = (p.angle2 as i32).clamp(-FULLCIRCLE, FULLCIRCLE);
            let a1 = (p.angle1 as i32 + angle2) as f64 / 64.0;
            EndPoints {
                self_join: angle2 == FULLCIRCLE || angle2 == -FULLCIRCLE,
                x0: p.x as f64 + p.width as f64 / 2.0 * (1.0 + mi_dcos(a0)),
                y0: p.y as f64 + p.height as f64 / 2.0 * (1.0 - mi_dsin(a0)),
                x1: p.x as f64 + p.width as f64 / 2.0 * (1.0 + mi_dcos(a1)),
                y1: p.y as f64 + p.height as f64 / 2.0 * (1.0 - mi_dsin(a1)),
            }
        })
        .collect();
    let mut arcs: Vec<PolyArcs> = (0..if double { 2 } else { 1 })
        .map(|_| PolyArcs::default())
        .collect();
    let live = |ph: usize| ph == 0 || double;

    let mut iphase = 0usize;
    let mut i_dash = 0usize;
    let mut dash_remaining = 0i32;
    if is_dashed {
        dash_remaining = dash(0);
        while dash_offset > 0 {
            if dash_offset >= dash_remaining {
                dash_offset -= dash_remaining;
                iphase ^= 1;
                i_dash += 1;
                if i_dash == ndash {
                    i_dash = 0;
                }
                dash_remaining = dash(i_dash);
            } else {
                dash_remaining -= dash_offset;
                dash_offset = 0;
            }
        }
    }
    let (i_dash_start, dash_remaining_start, iphase_start) = (i_dash, dash_remaining, iphase);

    // Start after an arc that doesn't join the next.
    let mut i = narcs as isize - 1;
    while i >= 0 {
        let iu = i as usize;
        let j = if iu + 1 == narcs { 0 } else { iu + 1 };
        if data[iu].self_join
            || iu == j
            || unequal(data[iu].x1, data[j].x0)
            || unequal(data[iu].y1, data[j].y0)
        {
            if live(iphase) {
                arcs[iphase].caps.push(CapRec {
                    end: RIGHT_END,
                    arc_index: 0,
                });
            }
            break;
        }
        i -= 1;
    }
    let mut start = (i + 1) as usize;
    if start == narcs {
        start = 0;
    }
    let mut i = start;

    let mut prevphase = 0usize;
    let mut k: i32 = 0;
    let mut nextk: i32 = 0;
    let mut map = [0.0f64; DASH_MAP_SIZE];
    let mut xarc = XArc::default();
    let mut arc: Option<(usize, usize)>;
    loop {
        let j = if i + 1 == narcs { 0 } else { i + 1 };
        let nexti = j;
        let p = parcs[i];
        if is_dashed {
            // Special rules for some arcs without area.
            let mut arc_type = ArcType::Other;
            if p.height == 0
                && (p.angle1 as i32 % FULLCIRCLE) == 0x2d00
                && p.angle2 as i32 == 0x2d00
            {
                arc_type = ArcType::Horizontal;
            } else if p.width == 0
                && (p.angle1 as i32 % FULLCIRCLE) == 0x1680
                && p.angle2 as i32 == 0x2d00
            {
                arc_type = ArcType::Vertical;
            }
            let start_angle;
            let end_angle;
            let mut backwards = false;
            if arc_type == ArcType::Other {
                compute_dash_map(&p, &mut map);
                let mut sa = p.angle1 as i32;
                let span_angle = (p.angle2 as i32).clamp(-FULLCIRCLE, FULLCIRCLE);
                if sa < 0 {
                    sa = FULLCIRCLE - (-sa) % FULLCIRCLE;
                }
                if sa >= FULLCIRCLE {
                    sa %= FULLCIRCLE;
                }
                start_angle = sa;
                end_angle = sa + span_angle;
                backwards = span_angle < 0;
            } else {
                xarc = p;
                if arc_type == ArcType::Vertical {
                    xarc.angle1 = 0x1680;
                    start_angle = p.y as i32;
                    end_angle = start_angle + p.height as i32;
                } else {
                    xarc.angle1 = 0x2d00;
                    start_angle = p.x as i32;
                    end_angle = start_angle + p.width as i32;
                }
            }
            let mut dash_angle = start_angle;
            let self_join = data[i].self_join && live(iphase);
            arc = None;
            // Each step moves at least one 64th of a degree (or pixel).
            let mut guard = 4 * FULLCIRCLE + 4 * 65536;
            while dash_angle != end_angle {
                guard -= 1;
                if guard < 0 {
                    break;
                }
                let prev_dash_angle = dash_angle;
                if arc_type == ArcType::Other {
                    dash_angle = compute_angle_from_path(
                        prev_dash_angle,
                        end_angle,
                        &map,
                        &mut dash_remaining,
                        backwards,
                    );
                    // Huge arcs and small dashes.
                    if dash_angle == prev_dash_angle {
                        if backwards {
                            dash_angle -= 1;
                        } else {
                            dash_angle += 1;
                        }
                    }
                } else {
                    let this_length = (if dash_angle + dash_remaining <= end_angle {
                        dash_remaining
                    } else {
                        end_angle - dash_angle
                    }) as u16 as i32;
                    if arc_type == ArcType::Vertical {
                        xarc.y = dash_angle as i16;
                        xarc.height = this_length as u16;
                    } else {
                        xarc.x = dash_angle as i16;
                        xarc.width = this_length as u16;
                    }
                    dash_angle += this_length;
                    dash_remaining -= this_length;
                }
                if live(iphase) {
                    if arc_type == ArcType::Other {
                        xarc = p;
                        let mut span_angle = prev_dash_angle;
                        if span_angle < 0 {
                            span_angle = FULLCIRCLE - (-span_angle) % FULLCIRCLE;
                        }
                        if span_angle >= FULLCIRCLE {
                            span_angle %= FULLCIRCLE;
                        }
                        xarc.angle1 = span_angle as i16;
                        span_angle = dash_angle - prev_dash_angle;
                        if backwards {
                            if dash_angle > prev_dash_angle {
                                span_angle += -FULLCIRCLE;
                            }
                        } else if dash_angle < prev_dash_angle {
                            span_angle += FULLCIRCLE;
                        }
                        xarc.angle2 = span_angle.clamp(-FULLCIRCLE, FULLCIRCLE) as i16;
                    }
                    let pa = &mut arcs[iphase];
                    let idx = pa.add_arc(xarc);
                    // Cap each end of an on/off dash.
                    if !double {
                        if prev_dash_angle != start_angle {
                            pa.caps.push(CapRec {
                                end: RIGHT_END,
                                arc_index: idx as i32,
                            });
                        }
                        if dash_angle != end_angle {
                            pa.caps.push(CapRec {
                                end: LEFT_END,
                                arc_index: idx as i32,
                            });
                        }
                    }
                    let (ncaps, njoins) = (pa.caps.len(), pa.joins.len());
                    let a = &mut pa.arcs[idx];
                    a.cap = ncaps;
                    a.join = njoins;
                    a.render = false;
                    a.self_join = dash_angle == end_angle && self_join;
                    arc = Some((iphase, idx));
                }
                prevphase = iphase;
                if dash_remaining <= 0 {
                    i_dash += 1;
                    if i_dash == ndash {
                        i_dash = 0;
                    }
                    iphase ^= 1;
                    dash_remaining = dash(i_dash);
                }
            }
            // A place for the position of an arc of no length.
            if start_angle == end_angle {
                prevphase = iphase;
                if !double && iphase == 1 {
                    prevphase = 0;
                }
                let pa = &mut arcs[prevphase];
                let idx = pa.add_arc(p);
                let (ncaps, njoins) = (pa.caps.len(), pa.joins.len());
                let a = &mut pa.arcs[idx];
                a.join = njoins;
                a.cap = ncaps;
                a.self_join = data[i].self_join;
                arc = Some((prevphase, idx));
            }
        } else {
            let pa = &mut arcs[iphase];
            let idx = pa.add_arc(p);
            let (ncaps, njoins) = (pa.caps.len(), pa.joins.len());
            let a = &mut pa.arcs[idx];
            a.join = njoins;
            a.cap = ncaps;
            a.self_join = data[i].self_join;
            prevphase = iphase;
            arc = Some((iphase, idx));
        }
        if live(prevphase) {
            k = arcs[prevphase].arcs.len() as i32 - 1;
        }
        if live(iphase) {
            nextk = arcs[iphase].arcs.len() as i32;
        }
        if nexti == start {
            nextk = 0;
            if is_dashed {
                i_dash = i_dash_start;
                iphase = iphase_start;
                dash_remaining = dash_remaining_start;
            }
        }
        let arcs_join = narcs > 1
            && i != j
            && isequal(data[i].x1, data[j].x0)
            && isequal(data[i].y1, data[j].y0)
            && !data[i].self_join
            && !data[j].self_join;
        if let Some((ph, ix)) = arc {
            arcs[ph].arcs[ix].render = !arcs_join;
        }
        if arcs_join && live(prevphase) && live(iphase) {
            let mut joinphase = iphase;
            if double {
                if nexti == start {
                    joinphase = iphase_start;
                }
                // A join right at a dash's end is drawn in the foreground:
                // it needs the foreground arcs, computed second.
                if joinphase != prevphase {
                    joinphase = 0;
                }
            }
            if live(joinphase) {
                arcs[joinphase].joins.push(JoinRec {
                    end0: LEFT_END,
                    arc_index0: k,
                    phase0: prevphase,
                    end1: RIGHT_END,
                    arc_index1: nextk,
                    phase1: iphase,
                });
                let njoins = arcs[prevphase].joins.len();
                if let Some((ph, ix)) = arc {
                    arcs[ph].arcs[ix].join = njoins;
                }
            }
        } else {
            // Cap the left end of this arc, unless it joins itself.
            if live(prevphase) {
                if let Some((ph, ix)) = arc {
                    if !arcs[ph].arcs[ix].self_join {
                        arcs[prevphase].caps.push(CapRec {
                            end: LEFT_END,
                            arc_index: k,
                        });
                        arcs[ph].arcs[ix].cap = arcs[prevphase].caps.len();
                    }
                }
            }
            if is_dashed && !arcs_join {
                i_dash = i_dash_start;
                iphase = iphase_start;
                dash_remaining = dash_remaining_start;
            }
            nextk = arcs.get(iphase).map_or(0, |pa| pa.arcs.len() as i32);
            if nexti == start {
                nextk = 0;
                i_dash = i_dash_start;
                iphase = iphase_start;
                dash_remaining = dash_remaining_start;
            }
            // Cap the right end of the next arc; the first only if it joins
            // this one (the last dash of an on/off line was off).
            if live(iphase) && (nexti != start || (arcs_join && is_dashed)) {
                arcs[iphase].caps.push(CapRec {
                    end: RIGHT_END,
                    arc_index: nextk,
                });
            }
        }
        i = nexti;
        if i == start {
            break;
        }
    }
    // The last section is rendered.
    for pa in arcs.iter_mut() {
        let (ncaps, njoins) = (pa.caps.len(), pa.joins.len());
        if let Some(last) = pa.arcs.last_mut() {
            last.render = true;
            last.join = njoins;
            last.cap = ncaps;
        }
    }
    arcs
}

/// drawZeroArc: an arc without width or height, as a rectangle.
fn draw_zero_arc(out: &mut Out, tarc: &XArc, lw: i32, faces: Option<&mut [Face; 2]>) {
    let l = lw as f64 / 2.0;
    let a0 = tarc.angle1 as i32;
    let a1 = (tarc.angle2 as i32).clamp(-FULLCIRCLE, FULLCIRCLE);
    let w = tarc.width as f64 / 2.0;
    let h = tarc.height as f64 / 2.0;
    // In X coordinates right away.
    let start_angle = -(a0 as f64 / 64.0);
    let end_angle = -((a0 + a1) as f64 / 64.0);
    let (mut xmax, mut xmin, mut ymax, mut ymin) = (-w, w, -h, h);
    let (mut x0, mut y0, mut x1, mut y1) = (0.0, 0.0, 0.0, 0.0);
    let mut a = start_angle;
    for _ in 0..64 {
        let x = w * mi_dcos(a);
        let y = h * mi_dsin(a);
        if a == start_angle {
            x0 = x;
            y0 = y;
        }
        if a == end_angle {
            x1 = x;
            y1 = y;
        }
        if x > xmax {
            xmax = x;
        }
        if x < xmin {
            xmin = x;
        }
        if y > ymax {
            ymax = y;
        }
        if y < ymin {
            ymin = y;
        }
        if a == end_angle {
            break;
        }
        if a1 < 0 {
            // Clockwise.
            if (a / 90.0).floor() == (end_angle / 90.0).floor() {
                a = end_angle;
            } else {
                a = 90.0 * ((a / 90.0).floor() + 1.0);
            }
        } else if (a / 90.0).ceil() == (end_angle / 90.0).ceil() {
            a = end_angle;
        } else {
            a = 90.0 * ((a / 90.0).ceil() - 1.0);
        }
    }
    let (mut lx, mut ly) = (l, l);
    if (x1 - x0) + (y1 - y0) < 0.0 {
        lx = -l;
        ly = -l;
    }
    if h != 0.0 {
        ly = 0.0;
        lx = -lx;
    } else {
        lx = 0.0;
    }
    if let Some(f) = faces {
        let r = &mut f[RIGHT_END];
        r.center = Spp { x: x0, y: y0 };
        r.clock = Spp {
            x: x0 - lx,
            y: y0 - ly,
        };
        r.counter_clock = Spp {
            x: x0 + lx,
            y: y0 + ly,
        };
        let lf = &mut f[LEFT_END];
        lf.center = Spp { x: x1, y: y1 };
        lf.clock = Spp {
            x: x1 + lx,
            y: y1 + ly,
        };
        lf.counter_clock = Spp {
            x: x1 - lx,
            y: y1 - ly,
        };
    }
    let ymax_was = ymax;
    if ymin != ymax_was {
        xmin = -l;
        xmax = l;
    } else {
        ymin = -l;
        ymax = l;
    }
    if xmax != xmin && ymax != ymin {
        let minx = iceil(xmin + w).wrapping_add(tarc.x as i32);
        let maxx = iceil(xmax + w).wrapping_add(tarc.x as i32);
        let miny = iceil(ymin + h).wrapping_add(tarc.y as i32);
        let maxy = iceil(ymax + h).wrapping_add(tarc.y as i32);
        out.rect(minx, miny, maxx.wrapping_sub(minx), maxy.wrapping_sub(miny));
    }
}

/// tailEllipseY: the ellipse y at the bottom of the tail.
fn tail_ellipse_y(def: &ArcDef, acc: &mut Acc) {
    acc.tail_y = 0.0;
    if def.w == def.h {
        return;
    }
    let mut t = def.l * def.w;
    if def.w > def.h {
        if t < acc.h2 {
            return;
        }
    } else if t > acc.h2 {
        return;
    }
    t = 2.0 * def.h * t;
    t = (CUBED_ROOT_4 * acc.h2 - (t * t).cbrt()) / acc.h2mw2;
    if t > 0.0 {
        acc.tail_y = def.h / CUBED_ROOT_2 * t.sqrt();
    }
}

fn outer_x_from_xy(x: f64, y: f64, acc: &Acc) -> f64 {
    x + (x * acc.h2l) / (x * x * acc.h4 + y * y * acc.w4).sqrt()
}

fn outer_y_from_xy(x: f64, y: f64, acc: &Acc) -> f64 {
    y + (y * acc.w2l) / (x * x * acc.h4 + y * y * acc.w4).sqrt()
}

fn inner_x_from_xy(x: f64, y: f64, acc: &Acc) -> f64 {
    x - (x * acc.h2l) / (x * x * acc.h4 + y * y * acc.w4).sqrt()
}

fn inner_y_from_xy(x: f64, y: f64, acc: &Acc) -> f64 {
    y - (y * acc.w2l) / (x * x * acc.h4 + y * y * acc.w4).sqrt()
}

fn inner_y_from_y(y: f64, def: &ArcDef, acc: &Acc) -> f64 {
    let x = (def.w / def.h) * (acc.h2 - y * y).sqrt();
    y - (y * acc.w2l) / (x * x * acc.h4 + y * y * acc.w4).sqrt()
}

/// computeLine.
fn compute_line(x1: f64, y1: f64, x2: f64, y2: f64) -> Line {
    if y1 == y2 {
        Line {
            valid: false,
            ..Line::default()
        }
    } else {
        let m = (x1 - x2) / (y1 - y2);
        Line {
            m,
            b: x1 - y1 * m,
            valid: true,
        }
    }
}

/// computeAcc.
fn compute_acc(tarc: &XArc, lw: i32) -> (ArcDef, Acc) {
    let def = ArcDef {
        w: tarc.width as f64 / 2.0,
        h: tarc.height as f64 / 2.0,
        l: lw as f64 / 2.0,
        a0: 0.0,
        a1: 0.0,
    };
    let mut acc = Acc {
        h2: def.h * def.h,
        w2: def.w * def.w,
        ..Acc::default()
    };
    acc.h4 = acc.h2 * acc.h2;
    acc.w4 = acc.w2 * acc.w2;
    acc.h2l = acc.h2 * def.l;
    acc.w2l = acc.w2 * def.l;
    acc.h2mw2 = acc.h2 - acc.w2;
    acc.from_int_x = if tarc.width & 1 != 0 { 0.5 } else { 0.0 };
    acc.from_int_y = if tarc.height & 1 != 0 { 0.5 } else { 0.0 };
    acc.xorg = tarc.x as i32 + (tarc.width as i32 >> 1);
    acc.yorgu = tarc.y as i32 + (tarc.height as i32 >> 1);
    acc.yorgl = acc.yorgu + (tarc.height as i32 & 1);
    tail_ellipse_y(&def, &mut acc);
    (def, acc)
}

/// computeBound: the y bounds of the arc's edges in this quadrant, and its
/// end faces.
fn compute_bound(
    def: &ArcDef,
    bound: &mut ArcBound,
    acc: &mut Acc,
    right: Option<usize>,
    left: Option<usize>,
    fc: &mut [Face; 2],
) {
    bound.ellipse.min = dsin(def.a0) * def.h;
    bound.ellipse.max = dsin(def.a1) * def.h;
    let mut ellipsex = Bound::default();
    if def.a0 == 45.0 && def.w == def.h {
        ellipsex.min = bound.ellipse.min;
    } else {
        ellipsex.min = dcos(def.a0) * def.w;
    }
    if def.a1 == 45.0 && def.w == def.h {
        ellipsex.max = bound.ellipse.max;
    } else {
        ellipsex.max = dcos(def.a1) * def.w;
    }
    bound.outer.min = outer_y_from_xy(ellipsex.min, bound.ellipse.min, acc);
    bound.outer.max = outer_y_from_xy(ellipsex.max, bound.ellipse.max, acc);
    bound.inner.min = inner_y_from_xy(ellipsex.min, bound.ellipse.min, acc);
    bound.inner.max = inner_y_from_xy(ellipsex.max, bound.ellipse.max, acc);
    let outerx = Bound {
        min: outer_x_from_xy(ellipsex.min, bound.ellipse.min, acc),
        max: outer_x_from_xy(ellipsex.max, bound.ellipse.max, acc),
    };
    let innerx = Bound {
        min: inner_x_from_xy(ellipsex.min, bound.ellipse.min, acc),
        max: inner_x_from_xy(ellipsex.max, bound.ellipse.max, acc),
    };

    // The ends, for caps and joins (y upwards here).
    if let Some(r) = right {
        fc[r].counter_clock = Spp {
            x: outerx.min,
            y: bound.outer.min,
        };
        fc[r].center = Spp {
            x: ellipsex.min,
            y: bound.ellipse.min,
        };
        fc[r].clock = Spp {
            x: innerx.min,
            y: bound.inner.min,
        };
    }
    if let Some(l) = left {
        fc[l].clock = Spp {
            x: outerx.max,
            y: bound.outer.max,
        };
        fc[l].center = Spp {
            x: ellipsex.max,
            y: bound.ellipse.max,
        };
        fc[l].counter_clock = Spp {
            x: innerx.max,
            y: bound.inner.max,
        };
    }

    bound.left.min = bound.inner.max;
    bound.left.max = bound.outer.max;
    bound.right.min = bound.inner.min;
    bound.right.max = bound.outer.min;

    acc.right = compute_line(innerx.min, bound.inner.min, outerx.min, bound.outer.min);
    acc.left = compute_line(innerx.max, bound.inner.max, outerx.max, bound.outer.max);

    if bound.inner.min > bound.inner.max {
        std::mem::swap(&mut bound.inner.min, &mut bound.inner.max);
    }
    let mut tail_y = acc.tail_y;
    if tail_y > bound.ellipse.max {
        tail_y = bound.ellipse.max;
    } else if tail_y < bound.ellipse.min {
        tail_y = bound.ellipse.min;
    }
    let inner_taily = inner_y_from_y(tail_y, def, acc);
    if bound.inner.min > inner_taily {
        bound.inner.min = inner_taily;
    }
    if bound.inner.max < inner_taily {
        bound.inner.max = inner_taily;
    }
    bound.inneri.min = iceil(bound.inner.min - acc.from_int_y);
    bound.inneri.max = (bound.inner.max - acc.from_int_y).floor() as i32;
    bound.outeri.min = iceil(bound.outer.min - acc.from_int_y);
    bound.outeri.max = (bound.outer.max - acc.from_int_y).floor() as i32;
}

/// hookEllipseY: the point on the ellipse whose normal meets the scan line
/// furthest out.
fn hook_ellipse_y(scan_y: f64, bound: &ArcBound, acc: &Acc, left: bool) -> f64 {
    if acc.h2mw2 == 0.0 {
        if (scan_y > 0.0 && !left) || (scan_y < 0.0 && left) {
            return bound.ellipse.min;
        }
        return bound.ellipse.max;
    }
    let ret = (acc.h4 * scan_y) / acc.h2mw2;
    if ret >= 0.0 {
        ret.cbrt()
    } else {
        -(-ret).cbrt()
    }
}

/// hookX: the scan line's x on the left or right face.
fn hook_x(scan_y: f64, def: &ArcDef, bound: &ArcBound, acc: &Acc, left: bool) -> f64 {
    if def.w != def.h {
        let ellipse_y = hook_ellipse_y(scan_y, bound, acc, left);
        if bounded_le(ellipse_y, &bound.ellipse) {
            // The sign of the second derivative.
            let max_min = ellipse_y * ellipse_y * ellipse_y * acc.h2mw2
                - acc.h2 * scan_y * (3.0 * ellipse_y * ellipse_y - 2.0 * acc.h2);
            if (left && max_min > 0.0) || (!left && max_min < 0.0) {
                if ellipse_y == 0.0 {
                    // As X.org has it: (w + left) ? -l : l.
                    return if def.w + f64::from(u8::from(left)) != 0.0 {
                        -def.l
                    } else {
                        def.l
                    };
                }
                return (acc.h2 * scan_y - ellipse_y * acc.h2mw2)
                    * (acc.h2 - ellipse_y * ellipse_y).sqrt()
                    / (def.h * def.w * ellipse_y);
            }
        }
    }
    if left {
        if acc.left.valid && bounded_le(scan_y, &bound.left) {
            intersect_line(scan_y, &acc.left)
        } else if acc.right.valid {
            intersect_line(scan_y, &acc.right)
        } else {
            def.w - def.l
        }
    } else if acc.right.valid && bounded_le(scan_y, &bound.right) {
        intersect_line(scan_y, &acc.right)
    } else if acc.left.valid {
        intersect_line(scan_y, &acc.left)
    } else {
        def.w - def.l
    }
}

/// arcSpan: the spans of row y, cut by the faces, mirrored by mask.
#[allow(clippy::too_many_arguments)]
fn arc_span(
    y: i32,
    lx: i32,
    lw: i32,
    rx: i32,
    rw: i32,
    def: &ArcDef,
    bounds: &ArcBound,
    acc: &Acc,
    mask: i32,
    fin: &mut Finals,
) {
    let (linx, rinx, loutx, routx);
    let fy = y as f64 + acc.from_int_y;
    if ibounded_le(y, &bounds.inneri) {
        linx = lx.wrapping_add(lw).wrapping_neg();
        rinx = rx;
    } else {
        // Where the left face cuts the row.
        let mut x = hook_x(fy, def, bounds, acc, true);
        if acc.right.valid && bounded_le(fy, &bounds.right) {
            let altx = intersect_line(fy, &acc.right);
            if altx < x {
                x = altx;
            }
        }
        linx = iceil(acc.from_int_x - x).wrapping_neg();
        rinx = iceil(acc.from_int_x + x);
    }
    if ibounded_le(y, &bounds.outeri) {
        loutx = lx.wrapping_neg();
        routx = rx.wrapping_add(rw);
    } else {
        // And the right face.
        let mut x = hook_x(fy, def, bounds, acc, false);
        if acc.left.valid && bounded_le(fy, &bounds.left) {
            let altx = x;
            x = intersect_line(fy, &acc.left);
            if x < altx {
                x = altx;
            }
        }
        loutx = iceil(acc.from_int_x - x).wrapping_neg();
        routx = iceil(acc.from_int_x + x);
    }
    if routx > rinx {
        if mask & 1 != 0 {
            fin.add(
                acc.yorgu - y,
                acc.xorg.wrapping_add(rinx),
                acc.xorg.wrapping_add(routx),
            );
        }
        if mask & 8 != 0 {
            fin.add(
                acc.yorgl + y,
                acc.xorg.wrapping_add(rinx),
                acc.xorg.wrapping_add(routx),
            );
        }
    }
    if loutx > linx {
        if mask & 2 != 0 {
            fin.add(
                acc.yorgu - y,
                acc.xorg.wrapping_sub(loutx),
                acc.xorg.wrapping_sub(linx),
            );
        }
        if mask & 4 != 0 {
            fin.add(
                acc.yorgl + y,
                acc.xorg.wrapping_sub(loutx),
                acc.xorg.wrapping_sub(linx),
            );
        }
    }
}

/// arcSpan0: row 0.
#[allow(clippy::too_many_arguments)]
fn arc_span0(
    lx: i32,
    mut lw: i32,
    mut rx: i32,
    mut rw: i32,
    def: &ArcDef,
    bounds: &ArcBound,
    acc: &Acc,
    mask: i32,
    fin: &mut Finals,
) {
    if ibounded_le(0, &bounds.inneri)
        && acc.left.valid
        && bounded_le(0.0, &bounds.left)
        && acc.left.b > 0.0
    {
        let mut x = def.w - def.l;
        if acc.left.b < x {
            x = acc.left.b;
        }
        lw = iceil(acc.from_int_x - x).wrapping_sub(lx);
        rw = rw.wrapping_add(rx);
        rx = iceil(acc.from_int_x + x);
        rw = rw.wrapping_sub(rx);
    }
    arc_span(0, lx, lw, rx, rw, def, bounds, acc, mask, fin);
}

/// tailSpan: a row through the tail of the inner edge.
#[allow(clippy::too_many_arguments)]
fn tail_span(
    y: i32,
    lw: i32,
    rw: i32,
    def: &ArcDef,
    bounds: &ArcBound,
    acc: &Acc,
    mask: i32,
    fin: &mut Finals,
) {
    if ibounded_le(y, &bounds.outeri) {
        arc_span(y, 0, lw, rw.wrapping_neg(), rw, def, bounds, acc, mask, fin);
    } else if def.w != def.h {
        let yy = y as f64 + acc.from_int_y;
        let x = tail_x(yy, def, bounds, acc);
        if yy == 0.0 && x == -rw as f64 - acc.from_int_x {
            return;
        }
        if acc.right.valid && bounded_le(yy, &bounds.right) {
            let mut rx = x;
            let lx = -x;
            let xalt = intersect_line(yy, &acc.right);
            if xalt >= -rw as f64 - acc.from_int_x && xalt <= rx {
                rx = xalt;
            }
            let mut n = iceil(acc.from_int_x + lx);
            if lw > n {
                if mask & 2 != 0 {
                    fin.add(
                        acc.yorgu - y,
                        acc.xorg.wrapping_add(n),
                        acc.xorg.wrapping_add(lw),
                    );
                }
                if mask & 4 != 0 {
                    fin.add(
                        acc.yorgl + y,
                        acc.xorg.wrapping_add(n),
                        acc.xorg.wrapping_add(lw),
                    );
                }
            }
            n = iceil(acc.from_int_x + rx);
            if n > rw.wrapping_neg() {
                if mask & 1 != 0 {
                    fin.add(
                        acc.yorgu - y,
                        acc.xorg.wrapping_sub(rw),
                        acc.xorg.wrapping_add(n),
                    );
                }
                if mask & 8 != 0 {
                    fin.add(
                        acc.yorgl + y,
                        acc.xorg.wrapping_sub(rw),
                        acc.xorg.wrapping_add(n),
                    );
                }
            }
        }
        arc_span(
            y,
            iceil(acc.from_int_x - x),
            0,
            iceil(acc.from_int_x + x),
            0,
            def,
            bounds,
            acc,
            mask,
            fin,
        );
    }
}

/// mirrorSppPoint: from the first quadrant into `quadrant`, and into X's
/// y-downwards coordinates.
fn mirror_spp_point(quadrant: i32, p: &mut Spp) {
    match quadrant {
        1 => p.x = -p.x,
        2 => {
            p.x = -p.x;
            p.y = -p.y;
        }
        3 => p.y = -p.y,
        _ => {}
    }
    p.y = -p.y;
}

#[derive(Clone, Copy, Default)]
struct Band {
    a0: i32,
    a1: i32,
    mask: i32,
}

/// drawArc: the arc split into sweeps scan-converted in the first quadrant
/// and mirrored into place. `right` and `left` are the faces' indices.
#[allow(clippy::too_many_arguments)]
fn draw_arc(
    out: &mut Out,
    tarc: &XArc,
    l: i32,
    mut a0: i32,
    mut a1: i32,
    faces: Option<&mut [Face; 2]>,
    right: usize,
    left: usize,
    spdata: Option<SpanData>,
) -> SpanData {
    let sp = spdata.unwrap_or_else(|| compute_wide_ellipse(l, tarc));
    let has_faces = faces.is_some();
    let mut fc: [Face; 2] = faces.as_deref().copied().unwrap_or_default();

    if a1 < a0 {
        a1 += 360 * 64;
    }
    let startq = a0 / (90 * 64);
    let mut endq = if a0 == a1 {
        startq
    } else {
        (a1 - 1) / (90 * 64)
    };
    let mut band = [Band::default(); 5];
    let mut bandno = 0;
    let mut curq = startq;
    let mut rightq = -1;
    let (mut leftq, mut righta, mut lefta) = (0, 0, 0);
    let (mut q0, mut q1) = (0, 0);
    loop {
        match curq {
            0 => {
                q0 = if a0 > 90 * 64 { 0 } else { a0 };
                q1 = if a1 < 360 * 64 {
                    a1.min(90 * 64)
                } else {
                    90 * 64
                };
                if curq == startq && a0 == q0 && rightq < 0 {
                    righta = q0;
                    rightq = curq;
                }
                if curq == endq && a1 == q1 {
                    lefta = q1;
                    leftq = curq;
                }
            }
            1 => {
                q0 = if a1 < 90 * 64 {
                    0
                } else {
                    180 * 64 - a1.min(180 * 64)
                };
                q1 = if a0 > 180 * 64 {
                    90 * 64
                } else {
                    180 * 64 - a0.max(90 * 64)
                };
                if curq == startq && 180 * 64 - a0 == q1 {
                    righta = q1;
                    rightq = curq;
                }
                if curq == endq && 180 * 64 - a1 == q0 {
                    lefta = q0;
                    leftq = curq;
                }
            }
            2 => {
                q0 = if a0 > 270 * 64 {
                    0
                } else {
                    a0.max(180 * 64) - 180 * 64
                };
                q1 = if a1 < 180 * 64 {
                    90 * 64
                } else {
                    a1.min(270 * 64) - 180 * 64
                };
                if curq == startq && a0 - 180 * 64 == q0 {
                    righta = q0;
                    rightq = curq;
                }
                if curq == endq && a1 - 180 * 64 == q1 {
                    lefta = q1;
                    leftq = curq;
                }
            }
            3 => {
                q0 = if a1 < 270 * 64 {
                    0
                } else {
                    360 * 64 - a1.min(360 * 64)
                };
                q1 = 360 * 64 - a0.max(270 * 64);
                if curq == startq && 360 * 64 - a0 == q1 {
                    righta = q1;
                    rightq = curq;
                }
                if curq == endq && 360 * 64 - a1 == q0 {
                    lefta = q0;
                    leftq = curq;
                }
            }
            _ => {}
        }
        band[bandno] = Band {
            a0: q0,
            a1: q1,
            mask: bit(curq),
        };
        bandno += 1;
        if curq == endq || bandno == band.len() {
            break;
        }
        curq += 1;
        if curq == 4 {
            a0 = 0;
            a1 -= 360 * 64;
            curq = 0;
            endq -= 4;
        }
    }
    let mut sweep = [Band::default(); 20];
    let mut sweepno = 0;
    while sweepno < sweep.len() {
        q0 = 90 * 64;
        let mut mask = 0;
        // The left-most point.
        for b in &band[..bandno] {
            if b.a0 <= q0 {
                q0 = b.a0;
                q1 = b.a1;
                mask = b.mask;
            }
        }
        if mask == 0 {
            break;
        }
        // The next point of change.
        for b in &band[..bandno] {
            if mask & b.mask == 0 {
                if b.a0 == q0 {
                    if b.a1 < q1 {
                        q1 = b.a1;
                    }
                    mask |= b.mask;
                } else if b.a0 < q1 {
                    q1 = b.a0;
                }
            }
        }
        sweep[sweepno] = Band {
            a0: q0,
            a1: q1,
            mask,
        };
        sweepno += 1;
        // Take the sweep off the bands it covers.
        for b in band[..bandno].iter_mut() {
            if b.a0 == q0 {
                b.a0 = q1;
                if b.a0 == b.a1 {
                    b.a0 = 90 * 64 + 1;
                    b.a1 = 90 * 64 + 1;
                }
            }
        }
    }
    let (mut def, mut acc) = compute_acc(tarc, l);
    let right_p = has_faces.then_some(right);
    let left_p = has_faces.then_some(left);
    let (mut flip_right, mut flip_left, mut copy_end) = (false, false, false);
    for s in &sweep[..sweepno] {
        let mask = s.mask;
        let (mut pass_right, mut pass_left) = (None, None);
        if mask & bit(rightq) != 0 {
            if s.a0 == righta {
                pass_right = right_p;
            } else if s.a1 == righta {
                pass_left = right_p;
                flip_right = true;
            }
        }
        if mask & bit(leftq) != 0 {
            if s.a1 == lefta {
                if pass_left.is_some() {
                    copy_end = true;
                }
                pass_left = left_p;
            } else if s.a0 == lefta {
                if pass_right.is_some() {
                    copy_end = true;
                }
                pass_right = left_p;
                flip_left = true;
            }
        }
        draw_quadrant(
            &mut def,
            &mut acc,
            s.a0,
            s.a1,
            mask,
            pass_right,
            pass_left,
            &mut fc,
            &sp,
            &mut out.finals,
        );
    }
    if let Some(f) = faces {
        // Both ends computed at once: drawQuadrant left them in the left.
        if copy_end {
            fc[right] = fc[left];
        }
        let r = &mut fc[right];
        mirror_spp_point(rightq, &mut r.clock);
        mirror_spp_point(rightq, &mut r.center);
        mirror_spp_point(rightq, &mut r.counter_clock);
        if flip_right {
            std::mem::swap(&mut r.clock, &mut r.counter_clock);
        }
        let lf = &mut fc[left];
        mirror_spp_point(leftq, &mut lf.counter_clock);
        mirror_spp_point(leftq, &mut lf.center);
        mirror_spp_point(leftq, &mut lf.clock);
        if flip_left {
            std::mem::swap(&mut lf.clock, &mut lf.counter_clock);
        }
        *f = fc;
    }
    sp
}

/// drawQuadrant: one sweep of the first quadrant, mirrored by mask.
#[allow(clippy::too_many_arguments)]
fn draw_quadrant(
    def: &mut ArcDef,
    acc: &mut Acc,
    a0: i32,
    a1: i32,
    mask: i32,
    right: Option<usize>,
    left: Option<usize>,
    fc: &mut [Face; 2],
    sp: &SpanData,
    fin: &mut Finals,
) {
    def.a0 = a0 as f64 / 64.0;
    def.a1 = a1 as f64 / 64.0;
    let mut bound = ArcBound::default();
    compute_bound(def, &mut bound, acc, right, left, fc);
    let (def, acc) = (&*def, &*acc);
    let mut yy = bound.inner.min;
    if bound.outer.min < yy {
        yy = bound.outer.min;
    }
    let miny = iceil(yy - acc.from_int_y);
    yy = bound.inner.max;
    if bound.outer.max > yy {
        yy = bound.outer.max;
    }
    let maxy = (yy - acc.from_int_y).floor() as i32;
    let mut y = sp.k;
    let mut si = 0usize;
    if sp.top {
        if a1 == 90 * 64 && mask & 1 != 0 {
            fin.add(acc.yorgu - y - 1, acc.xorg, acc.xorg + 1);
        }
        si += 1;
    }
    // Rows above maxy draw nothing: they're skipped in one go.
    let skip = |n: &mut i32, y: &mut i32, si: &mut usize| {
        if *y > maxy && *n > 0 {
            let s = (*n as i64).min(*y as i64 - maxy as i64 - 1).max(0) as i32;
            *n -= s;
            *y -= s;
            *si += s as usize;
        }
    };
    let mut n = sp.count1;
    while n > 0 {
        n -= 1;
        if y < miny {
            return;
        }
        if y <= maxy {
            let s = sp.at(si);
            let (lx, lw, rx, rw) = (s.lx as i32, s.lw as i32, s.rx as i32, s.rw as i32);
            arc_span(y, lx, -lx, 0, lx + lw, def, &bound, acc, mask, fin);
            if rw + rx != 0 {
                tail_span(y, -rw, -rx, def, &bound, acc, mask, fin);
            }
        } else {
            skip(&mut n, &mut y, &mut si);
        }
        y -= 1;
        si += 1;
    }
    if y < miny {
        return;
    }
    if sp.hole && y <= maxy {
        arc_span(y, 0, 0, 0, 1, def, &bound, acc, mask & 0xc, fin);
    }
    let mut n = sp.count2;
    while n > 0 {
        n -= 1;
        if y < miny {
            return;
        }
        if y <= maxy {
            let s = sp.at(si);
            arc_span(
                y,
                s.lx as i32,
                s.lw as i32,
                s.rx as i32,
                s.rw as i32,
                def,
                &bound,
                acc,
                mask,
                fin,
            );
        } else {
            skip(&mut n, &mut y, &mut si);
        }
        y -= 1;
        si += 1;
    }
    if sp.bot && miny <= y && y <= maxy {
        let mut n = mask;
        if y == miny {
            n &= 0xc;
        }
        let s = sp.at(si);
        let (lx, lw, rx, rw) = (s.lx as i32, s.lw as i32, s.rx as i32, s.rw as i32);
        if rw <= 0 {
            arc_span0(lx, -lx, 0, lx + lw, def, &bound, acc, n, fin);
            if rw + rx != 0 {
                tail_span(y, -rw, -rx, def, &bound, acc, n, fin);
            }
        } else {
            arc_span0(lx, lw, rx, rw, def, &bound, acc, n, fin);
        }
        y -= 1;
    }
    while y >= miny {
        let yy = y as f64 + acc.from_int_y;
        let mut x;
        if def.w == def.h {
            let xalt = def.w - def.l;
            x = -(xalt * xalt - yy * yy).sqrt();
        } else {
            x = tail_x(yy, def, &bound, acc);
            if acc.left.valid && bounded_le(yy, &bound.left) {
                let xalt = intersect_line(yy, &acc.left);
                if xalt < x {
                    x = xalt;
                }
            }
            if acc.right.valid && bounded_le(yy, &bound.right) {
                let xalt = intersect_line(yy, &acc.right);
                if xalt < x {
                    x = xalt;
                }
            }
        }
        arc_span(
            y,
            iceil(acc.from_int_x - x),
            0,
            iceil(acc.from_int_x + x),
            0,
            def,
            &bound,
            acc,
            mask,
            fin,
        );
        y -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::super::xvfb::{Op, check};
    use super::*;

    fn a(x: i32, y: i32, w: i32, h: i32, a1: i32, a2: i32) -> Arc {
        Arc {
            x,
            y,
            w,
            h,
            angle1: a1 * 64,
            angle2: a2 * 64,
        }
    }

    fn gc(lw: u32) -> Gc {
        Gc {
            line_width: lw,
            fg: 0xff_ffff,
            bg: 0x20_80ff,
            ..Gc::default()
        }
    }

    /// With GXcopy and GXxor.
    fn both(name: &str, gc: &Gc, arcs: &[Arc]) {
        for f in [3, 6] {
            let g = Gc {
                function: f,
                ..gc.clone()
            };
            check(
                &format!("{name} (function {f})"),
                &g,
                &Op::Arcs(arcs.to_vec()),
            );
        }
    }

    const WIDTHS: [u32; 6] = [1, 2, 3, 5, 10, 20];

    #[test]
    fn full_circles_and_ellipses() {
        for lw in WIDTHS {
            for (w, h) in [
                (40, 40),
                (41, 41),
                (1, 1),
                (2, 2),
                (3, 3),
                (7, 7),
                (60, 30),
                (31, 50),
                (100, 10),
                (9, 4),
                (200, 150),
            ] {
                both(
                    &format!("full {w}x{h} lw {lw}"),
                    &gc(lw),
                    &[a(20, 15, w, h, 0, 360)],
                );
            }
        }
    }

    #[test]
    fn partial_arcs() {
        for lw in WIDTHS {
            for (w, h) in [(50, 50), (51, 51), (70, 40), (33, 60)] {
                for (a1, a2) in [
                    (0, 90),
                    (45, 90),
                    (100, 70),
                    (190, 80),
                    (280, 60),
                    (30, -120),
                    (-45, 200),
                    (350, 40),
                    (10, 400),
                    (0, -450),
                    (90, 270),
                    (180, 180),
                    (0, 1),
                    (0, 0),
                ] {
                    both(
                        &format!("{w}x{h} {a1}+{a2} lw {lw}"),
                        &gc(lw),
                        &[a(30, 20, w, h, a1, a2)],
                    );
                }
            }
        }
    }

    #[test]
    fn odd_angles() {
        let g = gc(4);
        for (a1, a2) in [
            (1, 7777),
            (-12345, 3000),
            (32767, -32768),
            (-32768, 32767),
            (5760, 5761),
            (100, -23040),
        ] {
            both(
                &format!("raw {a1}+{a2}"),
                &g,
                &[Arc {
                    x: 20,
                    y: 20,
                    w: 60,
                    h: 45,
                    angle1: a1,
                    angle2: a2,
                }],
            );
        }
    }

    #[test]
    fn caps() {
        for cap in [
            CapStyle::NotLast,
            CapStyle::Butt,
            CapStyle::Round,
            CapStyle::Projecting,
        ] {
            for lw in WIDTHS {
                let g = Gc {
                    cap_style: cap,
                    ..gc(lw)
                };
                both(
                    &format!("cap {cap:?} lw {lw}"),
                    &g,
                    &[a(30, 20, 80, 60, 20, 130), a(40, 30, 45, 45, 200, -100)],
                );
            }
        }
    }

    #[test]
    fn joins() {
        for join in [JoinStyle::Miter, JoinStyle::Round, JoinStyle::Bevel] {
            for lw in WIDTHS {
                let g = Gc {
                    join_style: join,
                    cap_style: CapStyle::Round,
                    ..gc(lw)
                };
                // End to end: each starts where the one before ends.
                both(
                    &format!("join {join:?} lw {lw}"),
                    &g,
                    &[
                        a(20, 20, 60, 60, 0, 90),
                        a(50, 20, 60, 60, 180, 90),
                        a(50, 20, 60, 60, 270, 45),
                    ],
                );
                // Different sizes, a sharp angle.
                both(
                    &format!("join2 {join:?} lw {lw}"),
                    &g,
                    &[a(20, 20, 60, 60, 90, 90), a(20, 40, 60, 20, 180, -120)],
                );
                // A closed loop of two halves.
                both(
                    &format!("loop {join:?} lw {lw}"),
                    &g,
                    &[a(30, 20, 80, 70, 0, 180), a(30, 20, 80, 70, 180, 180)],
                );
            }
        }
    }

    #[test]
    fn large_and_clipped() {
        for lw in [1, 3, 10, 20] {
            let g = Gc {
                cap_style: CapStyle::Projecting,
                ..gc(lw)
            };
            both(
                &format!("big circle lw {lw}"),
                &g,
                &[a(-100, -80, 300, 300, 0, 360)],
            );
            both(
                &format!("big ellipse lw {lw}"),
                &g,
                &[a(-50, 10, 400, 180, 30, 200)],
            );
            both(
                &format!("off edge lw {lw}"),
                &g,
                &[a(120, 90, 80, 70, 0, 300)],
            );
        }
    }

    #[test]
    fn zero_sizes() {
        for lw in [1, 4, 9] {
            let g = Gc {
                cap_style: CapStyle::Projecting,
                ..gc(lw)
            };
            both(
                &format!("flat lw {lw}"),
                &g,
                &[
                    a(20, 50, 80, 0, 0, 180),
                    a(60, 10, 0, 80, 90, 180),
                    a(30, 30, 0, 0, 0, 360),
                ],
            );
            both(
                &format!("flat part lw {lw}"),
                &g,
                &[a(20, 50, 80, 0, 30, 100)],
            );
        }
    }

    #[test]
    fn dashes() {
        for style in [LineStyle::OnOffDash, LineStyle::DoubleDash] {
            for (dashes, off) in [
                (vec![4u8], 0u32),
                (vec![10, 3], 0),
                (vec![1], 0),
                (vec![7, 2, 3], 5),
                (vec![20, 5], 17),
            ] {
                for lw in [1, 2, 5, 10] {
                    for cap in [CapStyle::Butt, CapStyle::Round, CapStyle::Projecting] {
                        let g = Gc {
                            line_style: style,
                            dashes: dashes.clone(),
                            dash_offset: off,
                            cap_style: cap,
                            join_style: JoinStyle::Round,
                            ..gc(lw)
                        };
                        let name = format!("{style:?} {dashes:?}+{off} lw {lw} {cap:?}");
                        both(&format!("{name} circle"), &g, &[a(20, 10, 90, 90, 0, 360)]);
                        both(&format!("{name} part"), &g, &[a(30, 20, 100, 70, 60, -200)]);
                        both(
                            &format!("{name} joined"),
                            &g,
                            &[a(20, 20, 60, 60, 0, 90), a(50, 20, 60, 60, 180, 90)],
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn dashed_flat() {
        for style in [LineStyle::OnOffDash, LineStyle::DoubleDash] {
            let g = Gc {
                line_style: style,
                dashes: vec![6, 3],
                ..gc(3)
            };
            both(
                &format!("{style:?} horizontal"),
                &g,
                &[a(20, 50, 100, 0, 180, 180), a(70, 10, 0, 90, 90, 180)],
            );
        }
    }

    /// Width 0, drawn here directly (not through the thin-arc code): for
    /// arcs miCanZeroArc refuses, X.org's drawing is all miWideArc's.
    fn thin(name: &str, gc: &Gc, arcs: &[Arc]) {
        use super::super::xvfb::{Buffer, GROUND, H, W, xorg};
        for f in [3, 6] {
            let g = Gc {
                function: f,
                ..gc.clone()
            };
            let Some(want) = xorg(&g, &Op::Arcs(arcs.to_vec())) else {
                return;
            };
            let mut b = Buffer {
                px: vec![GROUND; (W * H) as usize],
                function: f,
            };
            poly_arc(&mut b, &g, arcs);
            let bad = (0..want.len()).filter(|&i| want[i] != b.px[i]).count();
            assert_eq!(bad, 0, "{name} (function {f}): {bad} pixels differ");
        }
    }

    #[test]
    fn width_zero_from_thin_path() {
        // Ellipses miCanZeroArc refuses (not circles, over 800 pixels).
        let g = gc(0);
        thin("thin big ellipse", &g, &[a(-500, 10, 1700, 90, 0, 360)]);
        thin("thin big part", &g, &[a(-300, -400, 900, 1000, 10, 150)]);
        thin("thin big part 2", &g, &[a(-800, 30, 1000, 900, 80, -100)]);
        for style in [LineStyle::OnOffDash, LineStyle::DoubleDash] {
            let g = Gc {
                line_style: style,
                dashes: vec![5, 2],
                ..gc(0)
            };
            thin(
                &format!("thin {style:?}"),
                &g,
                &[a(-500, 10, 1700, 90, 0, 360)],
            );
            thin(
                &format!("thin {style:?} part"),
                &g,
                &[a(-300, -400, 900, 1000, 10, 150)],
            );
        }
    }

    #[test]
    fn hostile() {
        // Mustn't panic or hang; not compared with Xvfb.
        struct Null;
        impl Sink for Null {
            fn fill_spans(&mut self, _: u32, _: &[Span]) {}
        }
        for lw in [1u32, 2, 255, 65535] {
            for style in [
                LineStyle::Solid,
                LineStyle::OnOffDash,
                LineStyle::DoubleDash,
            ] {
                for join in [JoinStyle::Miter, JoinStyle::Round] {
                    let g = Gc {
                        line_style: style,
                        dashes: vec![255, 1],
                        join_style: join,
                        cap_style: CapStyle::Round,
                        function: 6,
                        ..gc(lw)
                    };
                    let arcs = [
                        Arc {
                            x: 32767,
                            y: -32768,
                            w: 65535,
                            h: 65535,
                            angle1: -32768,
                            angle2: 32767,
                        },
                        Arc {
                            x: -32768,
                            y: 32767,
                            w: 65535,
                            h: 1,
                            angle1: 100,
                            angle2: -32768,
                        },
                        Arc {
                            x: 0,
                            y: 0,
                            w: 1,
                            h: 65535,
                            angle1: 0,
                            angle2: 5000,
                        },
                        Arc {
                            x: 0,
                            y: 0,
                            w: 0,
                            h: 0,
                            angle1: 0,
                            angle2: 0,
                        },
                    ];
                    // Huge dashed arcs with huge lines take X.org's time
                    // (seconds): too slow for a debug test.
                    let n = if lw == 65535 && style != LineStyle::Solid {
                        2
                    } else {
                        0
                    };
                    poly_arc(&mut Null, &g, &arcs[n..]);
                }
            }
        }
    }
}
