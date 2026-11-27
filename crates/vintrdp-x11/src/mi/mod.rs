//! Scan conversion: X.org's mi ("machine independent") drawing code, ported,
//! so lines, arcs and polygons come out pixel for pixel as X.org (and Xvfb)
//! draws them. Everything turns into spans, which a [`Sink`] fills with the
//! GC's fill style, function, plane mask and clip. See COPYING here for the
//! X.org notices.
//!
//! Coordinates are the drawable's, unclipped: X clients send 16-bit ones, the
//! arithmetic is in i32 (or f64, where X.org's is double).

mod arc;
mod fillarc;
mod poly;
mod wideline;
mod zeroarc;
mod zeroline;

#[cfg(test)]
pub(crate) mod xvfb;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// `w` pixels from (x, y) rightwards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub x: i32,
    pub y: i32,
    pub w: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

/// xArc: the bounding box, then angles in 64ths of a degree, counter-clockwise
/// from three o'clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Arc {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub angle1: i32,
    pub angle2: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineStyle {
    Solid,
    OnOffDash,
    DoubleDash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapStyle {
    NotLast,
    Butt,
    Round,
    Projecting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinStyle {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillStyle {
    Solid,
    Tiled,
    Stippled,
    OpaqueStippled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    EvenOdd,
    Winding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArcMode {
    Chord,
    PieSlice,
}

/// Points after the first are relative to the one before (CoordModePrevious),
/// or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordMode {
    Origin,
    Previous,
}

/// FillPoly's shape hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Complex,
    Nonconvex,
    Convex,
}

/// What scan conversion needs of a GC.
#[derive(Clone, Debug)]
pub struct Gc {
    pub function: u8,
    pub line_width: u32,
    pub line_style: LineStyle,
    pub cap_style: CapStyle,
    pub join_style: JoinStyle,
    pub fill_style: FillStyle,
    pub fill_rule: FillRule,
    pub arc_mode: ArcMode,
    pub fg: u32,
    pub bg: u32,
    /// Never empty, no zero entries.
    pub dashes: Vec<u8>,
    pub dash_offset: u32,
}

impl Gc {
    /// X.org's miSpansEasyRop: a pixel drawn twice comes out as if drawn
    /// once, so overlapping spans needn't be made unique.
    pub fn easy_rop(&self) -> bool {
        let rop = self.function;
        !((rop & 0xc) == 0x8 || (rop & 0x3) == 0x2)
    }
}

impl Default for Gc {
    /// The protocol's defaults.
    fn default() -> Self {
        Gc {
            function: 3,
            line_width: 0,
            line_style: LineStyle::Solid,
            cap_style: CapStyle::Butt,
            join_style: JoinStyle::Miter,
            fill_style: FillStyle::Solid,
            fill_rule: FillRule::EvenOdd,
            arc_mode: ArcMode::PieSlice,
            fg: 0,
            bg: 1,
            dashes: vec![4],
            dash_offset: 0,
        }
    }
}

/// Where spans go: the drawable, through the GC.
pub trait Sink {
    /// Fill spans as the GC's fill style does, with `pixel` in place of the
    /// foreground (double dashes draw their odd dashes with the background,
    /// as X.org does by changing the GC's foreground). Any order; they may
    /// overlap.
    fn fill_spans(&mut self, pixel: u32, spans: &[Span]);

    /// The extents of the drawable's clip, in its coordinates (None: all of
    /// it). fb's thin lines draw one pixel differently where a closed line
    /// crosses it.
    fn clip_extents(&self) -> Option<Rect> {
        None
    }

    /// The rectangle (drawable coordinates) is wholly inside the clip, as
    /// RegionContainsRect sees it: fb's thin arcs take a faster path then.
    fn clip_contains(&self, r: Rect) -> bool {
        self.clip_extents().is_none_or(|e| r.x >= e.x && r.y >= e.y && r.x + r.w <= e.x + e.w && r.y + r.h <= e.y + e.h)
    }

    fn fill_rects(&mut self, pixel: u32, rects: &[Rect]) {
        let mut spans = Vec::new();
        for r in rects.iter().filter(|r| r.w > 0 && r.h > 0) {
            spans.extend((r.y..r.y + r.h).map(|y| Span { x: r.x, y, w: r.w }));
        }
        self.fill_spans(pixel, &spans);
    }
}

/// Absolute points, from CoordModePrevious ones.
pub fn absolute(mode: CoordMode, pts: &[Point]) -> Vec<Point> {
    let mut out = pts.to_vec();
    if mode == CoordMode::Previous {
        for i in 1..out.len() {
            out[i].x += out[i - 1].x;
            out[i].y += out[i - 1].y;
        }
    }
    out
}

// The requests, dispatched as fb (Xvfb's frame buffer code) dispatches them:
// fbValidateGC and fbPolyLine and friends in xserver/fb.

/// PolyPoint (miPolyPoint).
pub fn poly_point(sink: &mut dyn Sink, gc: &Gc, mode: CoordMode, pts: &[Point]) {
    zeroline::poly_point(sink, gc, mode, pts)
}

/// PolyLine.
pub fn poly_line(sink: &mut dyn Sink, gc: &Gc, mode: CoordMode, pts: &[Point]) {
    match (gc.line_width, gc.line_style) {
        (0, LineStyle::Solid) => zeroline::zero_line(sink, gc, mode, pts),
        (0, _) => zeroline::zero_dash_line(sink, gc, mode, pts),
        (_, LineStyle::Solid) => wideline::wide_line(sink, gc, mode, pts),
        _ => wideline::wide_dash(sink, gc, mode, pts),
    }
}

/// PolySegment.
pub fn poly_segment(sink: &mut dyn Sink, gc: &Gc, segs: &[Segment]) {
    if gc.line_width == 0 && gc.line_style == LineStyle::Solid {
        zeroline::zero_segments(sink, gc, segs)
    } else {
        // miPolySegment: each segment a two-point line.
        for s in segs {
            poly_line(sink, gc, CoordMode::Origin, &[Point { x: s.x1, y: s.y1 }, Point { x: s.x2, y: s.y2 }]);
        }
    }
}

/// PolyRectangle (miPolyRectangle).
pub fn poly_rectangle(sink: &mut dyn Sink, gc: &Gc, rects: &[Rect]) {
    zeroline::poly_rectangle(sink, gc, rects)
}

/// PolyArc (fbPolyArc: thin arcs as miZeroPolyArc draws them, dashed ones
/// included; wide ones miPolyArc).
pub fn poly_arc(sink: &mut dyn Sink, gc: &Gc, arcs: &[Arc]) {
    if gc.line_width == 0 {
        zeroarc::zero_poly_arc(sink, gc, arcs)
    } else {
        arc::poly_arc(sink, gc, arcs)
    }
}

/// FillPoly (miFillPolygon).
pub fn fill_polygon(sink: &mut dyn Sink, gc: &Gc, shape: Shape, mode: CoordMode, pts: &[Point]) {
    poly::fill_polygon(sink, gc, shape, mode, pts)
}

/// PolyFillRectangle.
pub fn poly_fill_rect(sink: &mut dyn Sink, gc: &Gc, rects: &[Rect]) {
    sink.fill_rects(gc.fg, rects)
}

/// PolyFillArc (miPolyFillArc).
pub fn poly_fill_arc(sink: &mut dyn Sink, gc: &Gc, arcs: &[Arc]) {
    fillarc::poly_fill_arc(sink, gc, arcs)
}
