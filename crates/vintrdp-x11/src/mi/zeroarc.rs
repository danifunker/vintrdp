//! Thin arcs, ported from X.org's mi/mizerarc.c and mi/mizerarc.h
//! (miZeroPolyArc, miZeroArcSetup, the dashed variant) and the thin-arc
//! dispatch of fb/fbarc.c and fb/fbbits.h (fbPolyArc, fbArc32). COPYING in
//! this directory has their notices.
//!
//! The arithmetic is C's int, so it wraps where X.org's would; points are
//! DDXPointRecs, 16-bit, as X.org's are.

use super::*;

const FULLCIRCLE: i32 = 360 * 64;
const OCTANT: i32 = 45 * 64;
const QUADRANT: i32 = 90 * 64;
const HALFCIRCLE: i32 = 180 * 64;
const QUADRANT3: i32 = 270 * 64;
const EPSILON45: i32 = 64;

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

/// miCanZeroArc.
fn can_zero_arc(a: &XArc) -> bool {
    a.width == a.height || (a.width <= 800 && a.height <= 800)
}

fn dsin(d: i32) -> f64 {
    match d {
        0 => 0.0,
        QUADRANT => 1.0,
        HALFCIRCLE => 0.0,
        QUADRANT3 => -1.0,
        _ => (d as f64 * (std::f64::consts::PI / 11520.0)).sin(),
    }
}

fn dcos(d: i32) -> f64 {
    match d {
        0 => 1.0,
        QUADRANT => 0.0,
        HALFCIRCLE => -1.0,
        QUADRANT3 => 0.0,
        _ => (d as f64 * (std::f64::consts::PI / 11520.0)).cos(),
    }
}

/// miZeroArcPtRec.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ArcPt {
    x: i32,
    y: i32,
    mask: i32,
}

const OOB: ArcPt = ArcPt { x: 65536, y: 65536, mask: 0 };

/// miZeroArcRec.
#[derive(Clone, Copy)]
struct ZeroArc {
    x: i32,
    y: i32,
    k1: i32,
    k3: i32,
    a: i32,
    b: i32,
    d: i32,
    dx: i32,
    dy: i32,
    xorg: i32,
    yorg: i32,
    xorgo: i32,
    yorgo: i32,
    w: i32,
    h: i32,
    initial_mask: i32,
    start: ArcPt,
    altstart: ArcPt,
    end: ArcPt,
    altend: ArcPt,
    firstx: i32,
    firsty: i32,
    start_angle: i32,
    end_angle: i32,
}

/// miZeroArcSetup: the stepping state, and true for a whole ellipse when
/// `ok360`.
fn zero_arc_setup(arc: &XArc, ok360: bool) -> (ZeroArc, bool) {
    let l = arc.width & 1;
    let mut info = ZeroArc {
        x: 0,
        y: 0,
        k1: 0,
        k3: 0,
        a: 0,
        b: 0,
        d: 0,
        dx: 1,
        dy: 0,
        xorg: 0,
        yorg: 0,
        xorgo: 0,
        yorgo: 0,
        w: 0,
        h: 0,
        initial_mask: 0,
        start: OOB,
        altstart: OOB,
        end: OOB,
        altend: OOB,
        firstx: 0,
        firsty: 0,
        start_angle: 0,
        end_angle: 0,
    };
    if arc.width == arc.height {
        info.k1 = -8;
        info.k3 = -16;
        info.b = 12;
        info.a = (arc.width << 2) - 12;
        info.d = 17 - (arc.width << 1);
        if l != 0 {
            info.b -= 4;
            info.a += 4;
            info.d -= 7;
        }
    } else if arc.width == 0 || arc.height == 0 {
        info.k1 = 0;
        info.k3 = 0;
        info.a = -arc.height;
        info.b = 0;
        info.d = -1;
    } else {
        // Initial conditions.
        let alpha = arc.width.wrapping_mul(arc.width) << 2;
        let beta = arc.height.wrapping_mul(arc.height) << 2;
        info.k1 = beta << 1;
        info.k3 = info.k1.wrapping_add(alpha << 1);
        info.b = if l != 0 { 0 } else { beta.wrapping_neg() };
        info.a = alpha.wrapping_mul(arc.height);
        info.d = info.b.wrapping_sub(info.a >> 1).wrapping_sub(alpha >> 2);
        if l != 0 {
            info.d = info.d.wrapping_sub(beta >> 2);
        }
        info.a = info.a.wrapping_sub(info.b);
        // Take the first step, d < 0 always.
        info.b = info.b.wrapping_sub(info.k1);
        info.a = info.a.wrapping_add(info.k1);
        info.d = info.d.wrapping_add(info.b);
        // Octant change, b < 0 always.
        info.k1 = info.k1.wrapping_neg();
        info.k3 = info.k3.wrapping_neg();
        info.b = info.b.wrapping_neg();
        info.d = info.b.wrapping_sub(info.a).wrapping_sub(info.d);
        info.a = info.a.wrapping_sub(info.b << 1);
    }
    info.dx = 1;
    info.dy = 0;
    info.w = (arc.width + 1) >> 1;
    info.h = arc.height >> 1;
    info.xorg = arc.x + (arc.width >> 1);
    info.yorg = arc.y;
    info.xorgo = info.xorg + l;
    info.yorgo = info.yorg + arc.height;
    if arc.width == 0 {
        if arc.height == 0 {
            info.x = 0;
            info.y = 0;
            info.initial_mask = 0;
            info.start_angle = 0;
            info.end_angle = 0;
            info.start = OOB;
            info.end = OOB;
            return (info, false);
        }
        info.x = 0;
        info.y = 1;
    } else {
        info.x = 1;
        info.y = 0;
    }
    let angle1 = arc.angle1;
    let mut angle2 = arc.angle2;
    let (start_angle, end_angle);
    if angle1 == 0 && angle2 >= FULLCIRCLE {
        start_angle = 0;
        end_angle = 0;
    } else {
        angle2 = angle2.clamp(-FULLCIRCLE, FULLCIRCLE);
        let (mut s, mut e) = if angle2 < 0 { (angle1 + angle2, angle1) } else { (angle1, angle1 + angle2) };
        if s < 0 {
            s = FULLCIRCLE - (-s) % FULLCIRCLE;
        }
        if s >= FULLCIRCLE {
            s %= FULLCIRCLE;
        }
        if e < 0 {
            e = FULLCIRCLE - (-e) % FULLCIRCLE;
        }
        if e >= FULLCIRCLE {
            e %= FULLCIRCLE;
        }
        start_angle = s;
        end_angle = e;
    }
    info.start_angle = start_angle;
    info.end_angle = end_angle;
    if ok360 && start_angle == end_angle && arc.angle2 != 0 && arc.width != 0 && arc.height != 0 {
        info.initial_mask = 0xf;
        info.start = OOB;
        info.end = OOB;
        return (info, true);
    }
    let mut start = ArcPt { x: 0, y: 0, mask: 0 };
    let mut end = ArcPt { x: 0, y: 0, mask: 0 };
    let mut startseg = start_angle / OCTANT;
    if arc.height == 0 || (((startseg + 1) & 2) != 0 && arc.width != 0) {
        start.x = (dcos(start_angle) * ((arc.width + 1) as f64 / 2.0)) as i32;
        if start.x < 0 {
            start.x = -start.x;
        }
        start.y = -1;
    } else {
        start.y = (dsin(start_angle) * (arc.height as f64 / 2.0)) as i32;
        if start.y < 0 {
            start.y = -start.y;
        }
        start.y = info.h - start.y;
        start.x = 65536;
    }
    let mut endseg = end_angle / OCTANT;
    if arc.height == 0 || (((endseg + 1) & 2) != 0 && arc.width != 0) {
        end.x = (dcos(end_angle) * ((arc.width + 1) as f64 / 2.0)) as i32;
        if end.x < 0 {
            end.x = -end.x;
        }
        end.y = -1;
    } else {
        end.y = (dsin(end_angle) * (arc.height as f64 / 2.0)) as i32;
        if end.y < 0 {
            end.y = -end.y;
        }
        end.y = info.h - end.y;
        end.x = 65536;
    }
    info.firstx = start.x;
    info.firsty = start.y;
    info.initial_mask = 0;
    let mut overlap = arc.angle2 != 0 && end_angle <= start_angle;
    for i in 0..4 {
        let hit = if overlap {
            i * QUADRANT <= end_angle || (i + 1) * QUADRANT > start_angle
        } else {
            i * QUADRANT <= end_angle && (i + 1) * QUADRANT > start_angle
        };
        if hit {
            info.initial_mask |= 1 << i;
        }
    }
    start.mask = info.initial_mask;
    end.mask = info.initial_mask;
    startseg >>= 1;
    endseg >>= 1;
    overlap = overlap && endseg == startseg;
    if start.x != end.x || start.y != end.y || !overlap {
        if startseg & 1 != 0 {
            if !overlap {
                info.initial_mask &= !(1 << startseg);
            }
            if start.x > end.x || start.y > end.y {
                end.mask &= !(1 << startseg);
            }
        } else {
            start.mask &= !(1 << startseg);
            if ((start.x < end.x || start.y < end.y) || (start.x == end.x && start.y == end.y && (endseg & 1) != 0)) && !overlap {
                end.mask &= !(1 << startseg);
            }
        }
        if endseg & 1 != 0 {
            end.mask &= !(1 << endseg);
            if ((start.x > end.x || start.y > end.y) || (start.x == end.x && start.y == end.y && (startseg & 1) == 0)) && !overlap {
                start.mask &= !(1 << endseg);
            }
        } else {
            if !overlap {
                info.initial_mask &= !(1 << endseg);
            }
            if start.x < end.x || start.y < end.y {
                start.mask &= !(1 << endseg);
            }
        }
    }
    // Start and end both near 45 degrees.
    if start_angle != 0 && ((start.y < 0 && end.y >= 0) || (start.y >= 0 && end.y < 0)) {
        let i = (start_angle + OCTANT) % OCTANT;
        if !(EPSILON45..=OCTANT - EPSILON45).contains(&i) {
            let i = (end_angle + OCTANT) % OCTANT;
            if !(EPSILON45..=OCTANT - EPSILON45).contains(&i) {
                if start.y < 0 {
                    let i = ((dsin(start_angle) * (arc.height as f64 / 2.0)) as i32).abs();
                    if info.h - i == end.y {
                        start.mask = end.mask;
                    }
                } else {
                    let i = ((dsin(end_angle) * (arc.height as f64 / 2.0)) as i32).abs();
                    if info.h - i == start.y {
                        end.mask = start.mask;
                    }
                }
            }
        }
    }
    if startseg & 1 != 0 {
        info.start = start;
        info.end = OOB;
    } else {
        info.end = start;
        info.start = OOB;
    }
    if endseg & 1 != 0 {
        info.altend = end;
        if info.altend.x < info.end.x || info.altend.y < info.end.y {
            std::mem::swap(&mut info.altend, &mut info.end);
        }
        info.altstart = OOB;
    } else {
        info.altstart = end;
        if info.altstart.x < info.start.x || info.altstart.y < info.start.y {
            std::mem::swap(&mut info.altstart, &mut info.start);
        }
        info.altend = OOB;
    }
    if info.start.x == 0 || info.start.y == 0 {
        info.initial_mask = info.start.mask;
        info.start = info.altstart;
    }
    if arc.width == 0 && arc.height == 1 {
        // X.org's kludge.
        info.initial_mask |= info.end.mask;
        info.initial_mask |= info.initial_mask << 1;
        info.end.x = 0;
        info.end.mask = 0;
    }
    (info, false)
}

/// The stepping registers (MIARCSETUP).
struct Step {
    x: i32,
    y: i32,
    k1: i32,
    k3: i32,
    a: i32,
    b: i32,
    d: i32,
    dx: i32,
    dy: i32,
}

impl Step {
    fn new(i: &ZeroArc) -> Step {
        Step { x: i.x, y: i.y, k1: i.k1, k3: i.k3, a: i.a, b: i.b, d: i.d, dx: i.dx, dy: i.dy }
    }

    /// MIARCOCTANTSHIFT.
    fn octant_shift(&mut self, h: i32) {
        if self.a < 0 {
            if self.y == h {
                self.d = -1;
                self.a = 0;
                self.b = 0;
                self.k1 = 0;
            } else {
                self.dx = (self.k1 << 1).wrapping_sub(self.k3);
                self.k1 = self.dx.wrapping_sub(self.k1);
                self.k3 = self.k3.wrapping_neg();
                self.b = self.b.wrapping_add(self.a).wrapping_sub(self.k1 >> 1);
                self.d = self.b.wrapping_add(self.a.wrapping_neg() >> 1).wrapping_sub(self.d).wrapping_add(self.k3 >> 3);
                self.a = if self.dx < 0 {
                    (self.dx.wrapping_neg() >> 1).wrapping_neg().wrapping_sub(self.a)
                } else {
                    (self.dx >> 1).wrapping_sub(self.a)
                };
                self.dx = 0;
                self.dy = 1;
            }
        }
    }

    /// MIARCSTEP.
    fn step(&mut self) {
        self.b = self.b.wrapping_sub(self.k1);
        if self.d < 0 {
            self.x += self.dx;
            self.y += self.dy;
            self.a = self.a.wrapping_add(self.k1);
            self.d = self.d.wrapping_add(self.b);
        } else {
            self.x += 1;
            self.y += 1;
            self.a = self.a.wrapping_add(self.k3);
            self.d = self.d.wrapping_sub(self.a);
        }
    }

    /// MIARCCIRCLESTEP.
    fn circle_step(&mut self) {
        self.b = self.b.wrapping_sub(self.k1);
        self.x += 1;
        if self.d < 0 {
            self.a = self.a.wrapping_add(self.k1);
            self.d = self.d.wrapping_add(self.b);
        } else {
            self.y += 1;
            self.a = self.a.wrapping_add(self.k3);
            self.d = self.d.wrapping_sub(self.a);
        }
    }
}

/// miZeroArcPts, or with `fb` fbArc32's pixel order (which doesn't drop
/// the four repeated points at the end of a whole even circle).
fn zero_arc_pts(arc: &XArc, fb: bool, pts: &mut Vec<Point>) {
    let (mut info, do360) = zero_arc_setup(arc, true);
    let mut s = Step::new(&info);
    let mut mask = info.initial_mask;
    let pix = |pts: &mut Vec<Point>, x: i32, y: i32| pts.push(Point { x, y });
    let dopix = |pts: &mut Vec<Point>, mask: i32, idx: i32, x: i32, y: i32| {
        if mask & (1 << idx) != 0 {
            pts.push(Point { x, y });
        }
    };
    if arc.width & 1 == 0 {
        dopix(pts, mask, 1, info.xorgo, info.yorg);
        dopix(pts, mask, 3, info.xorgo, info.yorgo);
    }
    if info.end.x == 0 || info.end.y == 0 {
        mask = info.end.mask;
        info.end = info.altend;
    }
    if do360 && arc.width == arc.height && arc.width & 1 == 0 {
        let yorgh = info.yorg + info.h;
        let xorghp = info.xorg + info.h;
        let xorghn = info.xorg - info.h;
        loop {
            pix(pts, info.xorg + s.x, info.yorg + s.y);
            pix(pts, info.xorg - s.x, info.yorg + s.y);
            pix(pts, info.xorg - s.x, info.yorgo - s.y);
            pix(pts, info.xorg + s.x, info.yorgo - s.y);
            if s.a < 0 {
                break;
            }
            pix(pts, xorghp - s.y, yorgh - s.x);
            pix(pts, xorghn + s.y, yorgh - s.x);
            pix(pts, xorghn + s.y, yorgh + s.x);
            pix(pts, xorghp - s.y, yorgh + s.x);
            s.circle_step();
        }
        let n = pts.len();
        if !fb && s.x > 1 && n >= 5 && pts[n - 1] == pts[n - 5] {
            pts.truncate(n - 4);
        }
        s.x = info.w;
        s.y = info.h;
    } else if do360 {
        while s.y < info.h || s.x < info.w {
            s.octant_shift(info.h);
            pix(pts, info.xorg + s.x, info.yorg + s.y);
            pix(pts, info.xorgo - s.x, info.yorg + s.y);
            pix(pts, info.xorgo - s.x, info.yorgo - s.y);
            pix(pts, info.xorg + s.x, info.yorgo - s.y);
            s.step();
        }
    } else {
        while s.y < info.h || s.x < info.w {
            s.octant_shift(info.h);
            if s.x == info.start.x || s.y == info.start.y {
                mask = info.start.mask;
                info.start = info.altstart;
            }
            dopix(pts, mask, 0, info.xorg + s.x, info.yorg + s.y);
            dopix(pts, mask, 1, info.xorgo - s.x, info.yorg + s.y);
            dopix(pts, mask, 2, info.xorgo - s.x, info.yorgo - s.y);
            dopix(pts, mask, 3, info.xorg + s.x, info.yorgo - s.y);
            if s.x == info.end.x || s.y == info.end.y {
                mask = info.end.mask;
                info.end = info.altend;
            }
            s.step();
        }
    }
    if s.x == info.start.x || s.y == info.start.y {
        mask = info.start.mask;
    }
    dopix(pts, mask, 0, info.xorg + s.x, info.yorg + s.y);
    dopix(pts, mask, 2, info.xorgo - s.x, info.yorgo - s.y);
    if arc.height & 1 != 0 {
        dopix(pts, mask, 1, info.xorgo - s.x, info.yorg + s.y);
        dopix(pts, mask, 3, info.xorg + s.x, info.yorgo - s.y);
    }
}

/// DashInfo.
struct DashInfo {
    skip_start: bool,
    have_start: bool,
    start_pt: Point,
    have_last: bool,
    skip_last: bool,
    end_pt: Point,
    dash_index: usize,
    dash_offset: i64,
    dash_index_init: usize,
    dash_offset_init: i64,
}

/// miStepDash.
fn step_dash(dist: i64, index: &mut usize, dash: &[u8], offset: &mut i64) {
    let mut dist = dist;
    let mut i = *index;
    if dist < dash[i] as i64 - *offset {
        *offset += dist;
        return;
    }
    dist -= dash[i] as i64 - *offset;
    i = (i + 1) % dash.len();
    let total: i64 = dash.iter().map(|&d| d as i64).sum();
    if total <= dist {
        dist %= total;
    }
    while dist >= dash[i] as i64 {
        dist -= dash[i] as i64;
        i = (i + 1) % dash.len();
    }
    *index = i;
    *offset = dist;
}

/// miZeroArcDashPts: the arc's points, split into even and odd dashes.
fn zero_arc_dash_pts(dash: &[u8], arc: &XArc, dinfo: &mut DashInfo, even: &mut Vec<Point>, odd: &mut Vec<Point>) {
    let (mut info, _) = zero_arc_setup(arc, false);
    let mut s = Step::new(&info);
    // One list per quadrant (C's arcPts[4]).
    let mut q: [Vec<Point>; 4] = Default::default();
    let mut mask = info.initial_mask;
    let startseg = (info.start_angle / QUADRANT) as usize & 3;
    let mut start_cnt = 0usize;
    let dopix = |q: &mut [Vec<Point>; 4], mask: i32, idx: usize, x: i32, y: i32| {
        if mask & (1 << idx) != 0 {
            q[idx].push(Point { x, y });
        }
    };
    if arc.width & 1 == 0 {
        dopix(&mut q, mask, 1, info.xorgo, info.yorg);
        dopix(&mut q, mask, 3, info.xorgo, info.yorgo);
    }
    if info.end.x == 0 || info.end.y == 0 {
        mask = info.end.mask;
        info.end = info.altend;
    }
    while s.y < info.h || s.x < info.w {
        s.octant_shift(info.h);
        if s.x == info.firstx || s.y == info.firsty {
            start_cnt = q[startseg].len();
        }
        if s.x == info.start.x || s.y == info.start.y {
            mask = info.start.mask;
            info.start = info.altstart;
        }
        dopix(&mut q, mask, 0, info.xorg + s.x, info.yorg + s.y);
        dopix(&mut q, mask, 1, info.xorgo - s.x, info.yorg + s.y);
        dopix(&mut q, mask, 2, info.xorgo - s.x, info.yorgo - s.y);
        dopix(&mut q, mask, 3, info.xorg + s.x, info.yorgo - s.y);
        if s.x == info.end.x || s.y == info.end.y {
            mask = info.end.mask;
            info.end = info.altend;
        }
        s.step();
    }
    if s.x == info.firstx || s.y == info.firsty {
        start_cnt = q[startseg].len();
    }
    if s.x == info.start.x || s.y == info.start.y {
        mask = info.start.mask;
    }
    dopix(&mut q, mask, 0, info.xorg + s.x, info.yorg + s.y);
    dopix(&mut q, mask, 2, info.xorgo - s.x, info.yorgo - s.y);
    if arc.height & 1 != 0 {
        dopix(&mut q, mask, 1, info.xorgo - s.x, info.yorg + s.y);
        dopix(&mut q, mask, 3, info.xorg + s.x, info.yorgo - s.y);
    }

    // C's pointers into its points buffer, as indexes into one array with a
    // stretch per quadrant (and a gap before each, for C's "pt - 1").
    let stride = q.iter().map(|v| v.len()).max().unwrap_or(0) as isize + 2;
    let base = |seg: usize| seg as isize * stride + 1;
    let mut flat = vec![Point::default(); (4 * stride) as usize];
    for (seg, v) in q.iter().enumerate() {
        let b = base(seg) as usize;
        flat[b..b + v.len()].copy_from_slice(v);
    }
    let arc_pts = |seg: usize| base(seg) + q[seg].len() as isize;
    let at = |p: isize| flat.get(p as usize).copied().unwrap_or_default();
    let start_pt = base(startseg) + start_cnt as isize;
    let mut start_pts = [0isize; 5];
    let mut end_pts = [0isize; 5];
    let mut deltas = [0isize; 5];
    for i in 0..4 {
        let seg = (startseg + i) & 3;
        let pt = base(seg);
        if seg & 1 != 0 {
            start_pts[i] = pt;
            end_pts[i] = arc_pts(seg);
            deltas[i] = 1;
        } else {
            start_pts[i] = arc_pts(seg) - 1;
            end_pts[i] = pt - 1;
            deltas[i] = -1;
        }
    }
    start_pts[4] = start_pts[0];
    end_pts[4] = start_pt;
    start_pts[0] = start_pt;
    if startseg & 1 != 0 {
        if start_pts[4] != end_pts[4] {
            end_pts[4] -= 1;
        }
        deltas[4] = 1;
    } else {
        if start_pts[0] > start_pts[4] {
            start_pts[0] -= 1;
        }
        if start_pts[4] < end_pts[4] {
            end_pts[4] -= 1;
        }
        deltas[4] = -1;
    }
    if arc.angle2 < 0 {
        let tmpd = deltas[0];
        let tmps = start_pts[0] - tmpd;
        let tmpe = end_pts[0] - tmpd;
        start_pts[0] = end_pts[4] - deltas[4];
        end_pts[0] = start_pts[4] - deltas[4];
        deltas[0] = -deltas[4];
        start_pts[4] = tmpe;
        end_pts[4] = tmps;
        deltas[4] = -tmpd;
        let tmpd = deltas[1];
        let tmps = start_pts[1] - tmpd;
        let tmpe = end_pts[1] - tmpd;
        start_pts[1] = end_pts[3] - deltas[3];
        end_pts[1] = start_pts[3] - deltas[3];
        deltas[1] = -deltas[3];
        start_pts[3] = tmpe;
        end_pts[3] = tmps;
        deltas[3] = -tmpd;
        let tmps = start_pts[2] - deltas[2];
        start_pts[2] = end_pts[2] - deltas[2];
        end_pts[2] = tmps;
        deltas[2] = -deltas[2];
    }
    let Some(i) = (0..5).find(|&i| start_pts[i] != end_pts[i]) else { return };
    let pt = start_pts[i];
    let j = (0..5).rev().find(|&j| start_pts[j] != end_pts[j]).unwrap_or(i);
    let last_pt = end_pts[j] - deltas[j];
    if dinfo.have_last && at(pt) == dinfo.end_pt {
        start_pts[i] += deltas[i];
    } else {
        dinfo.dash_index = dinfo.dash_index_init;
        dinfo.dash_offset = dinfo.dash_offset_init;
    }
    if !dinfo.skip_start && info.start_angle != info.end_angle {
        dinfo.start_pt = at(pt);
        dinfo.have_start = true;
    } else if !dinfo.skip_last && dinfo.have_start && at(last_pt) == dinfo.start_pt && last_pt != start_pts[i] {
        end_pts[j] = last_pt;
    }
    if info.start_angle != info.end_angle {
        dinfo.have_last = true;
        dinfo.end_pt = at(last_pt);
    }
    let mut remaining = dash[dinfo.dash_index] as i64 - dinfo.dash_offset;
    for i in 0..5 {
        let mut pt = start_pts[i];
        let last = end_pts[i];
        let delta = deltas[i];
        // Within the array, so a walk can't run away.
        let in_range = |p: isize| (0..flat.len() as isize).contains(&p);
        while pt != last && in_range(pt) {
            let out = if dinfo.dash_index & 1 != 0 { &mut *odd } else { &mut *even };
            while pt != last && in_range(pt) && {
                remaining -= 1;
                remaining >= 0
            } {
                out.push(at(pt));
                pt += delta;
            }
            if remaining <= 0 {
                dinfo.dash_index = (dinfo.dash_index + 1) % dash.len();
                remaining = dash[dinfo.dash_index] as i64;
            }
        }
    }
    dinfo.dash_offset = dash[dinfo.dash_index] as i64 - remaining;
}

/// Points as PolyPoint draws them: 16-bit, one pixel each.
fn draw_points(sink: &mut dyn Sink, pixel: u32, pts: &[Point], spans: &mut Vec<Span>) {
    spans.clear();
    spans.extend(pts.iter().map(|p| Span { x: p.x as i16 as i32, y: p.y as i16 as i32, w: 1 }));
    if !spans.is_empty() {
        sink.fill_spans(pixel, spans);
    }
}

/// miZeroPolyArc.
fn mi_zero_poly_arc(sink: &mut dyn Sink, gc: &Gc, arcs: &[Arc]) {
    let mut any = false;
    for a in arcs {
        let x = xarc(a);
        if !can_zero_arc(&x) {
            super::arc::poly_arc(sink, gc, std::slice::from_ref(a));
        } else {
            let n = if x.width > x.height { x.width + (x.height >> 1) } else { x.height + (x.width >> 1) };
            any |= n > 0;
        }
    }
    if !any {
        return;
    }
    // SetDashes doubles an odd list.
    let mut dash: Vec<u8> = gc.dashes.iter().copied().filter(|&d| d != 0).collect();
    if dash.is_empty() {
        dash.push(4);
    }
    if dash.len() & 1 != 0 {
        dash.extend_from_within(..);
    }
    let mut dinfo = DashInfo {
        skip_start: false,
        have_start: false,
        start_pt: Point::default(),
        have_last: false,
        skip_last: false,
        end_pt: Point::default(),
        dash_index: 0,
        dash_offset: 0,
        dash_index_init: 0,
        dash_offset_init: 0,
    };
    if gc.line_style != LineStyle::Solid {
        step_dash(gc.dash_offset as i64, &mut dinfo.dash_index_init, &dash, &mut dinfo.dash_offset_init);
    }
    let (mut even, mut odd, mut spans) = (Vec::new(), Vec::new(), Vec::new());
    for (k, a) in arcs.iter().enumerate() {
        let x = xarc(a);
        if !can_zero_arc(&x) {
            continue;
        }
        even.clear();
        odd.clear();
        if gc.line_style == LineStyle::Solid {
            zero_arc_pts(&x, false, &mut even);
        } else {
            dinfo.skip_last = k + 1 < arcs.len();
            zero_arc_dash_pts(&dash, &x, &mut dinfo, &mut even, &mut odd);
            dinfo.skip_start = true;
        }
        draw_points(sink, gc.fg, &even, &mut spans);
        if gc.line_style == LineStyle::DoubleDash {
            draw_points(sink, gc.bg, &odd, &mut spans);
        }
    }
}

/// fbPolyArc with line width 0: solid arcs through fbArc32 when they lie
/// inside the drawable, the rest through miZeroPolyArc (or, for those it
/// can't do, miPolyArc).
///
/// fbArc32 needs the arc's box inside the clip (and within 16 bits). It only
/// matters for whole even circles drawn with a non-idempotent function.
pub fn zero_poly_arc(sink: &mut dyn Sink, gc: &Gc, arcs: &[Arc]) {
    if gc.line_style != LineStyle::Solid || gc.fill_style != FillStyle::Solid {
        return mi_zero_poly_arc(sink, gc, arcs);
    }
    let (mut pts, mut spans) = (Vec::new(), Vec::new());
    for a in arcs {
        let x = xarc(a);
        if !can_zero_arc(&x) {
            super::arc::poly_arc(sink, gc, std::slice::from_ref(a));
            continue;
        }
        let (x2, y2) = (x.x + x.width + 1, x.y + x.height + 1);
        let inside = x2 <= i16::MAX as i32 && y2 <= i16::MAX as i32 && sink.clip_contains(Rect { x: x.x, y: x.y, w: x.width + 1, h: x.height + 1 });
        if inside {
            pts.clear();
            zero_arc_pts(&x, true, &mut pts);
            draw_points(sink, gc.fg, &pts, &mut spans);
        } else {
            mi_zero_poly_arc(sink, gc, std::slice::from_ref(a));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mi::xvfb::{check, Op};

    const SIZES: &[(i32, i32)] = &[
        (0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (0, 6), (6, 0), (1, 7), (7, 1), (2, 9), (9, 2),
        (10, 10), (11, 11), (16, 16), (17, 17), (20, 13), (13, 20), (21, 14), (40, 40), (41, 41),
        (64, 30), (31, 70), (100, 60), (101, 61), (140, 100),
    ];

    const ANGLES: &[(i32, i32)] = &[
        (0, 360 * 64), (0, 90 * 64), (0, 180 * 64), (45 * 64, 90 * 64), (30 * 64, 100 * 64), (30 * 64, -100 * 64),
        (100 * 64, 200 * 64), (200 * 64, 120 * 64), (280 * 64, 70 * 64), (350 * 64, 20 * 64), (-45 * 64, 30 * 64),
        (-400 * 64, 50 * 64), (200 * 64, 400 * 64), (0, -360 * 64), (90 * 64, -360 * 64), (0, 1), (100 * 64, 1),
        (10 * 64, -1), (45 * 64, 0), (44 * 64, 2 * 64), (10 * 64, 32767), (-32768, 32767), (32767, -32768),
        (90 * 64, 90 * 64), (180 * 64, 90 * 64), (270 * 64, 90 * 64), (135 * 64, 180 * 64), (46 * 64, 358 * 64),
    ];

    fn both(name: &str, gc: &Gc, arcs: Vec<Arc>) {
        let op = Op::Arcs(arcs);
        for function in [3, 6] {
            check(&format!("{name} gx{function}"), &Gc { function, ..gc.clone() }, &op);
        }
    }

    fn arc(x: i32, y: i32, w: i32, h: i32, a1: i32, a2: i32) -> Arc {
        Arc { x, y, w, h, angle1: a1, angle2: a2 }
    }

    #[test]
    fn solid() {
        let gc = Gc { fg: 0xff_ffff, ..Gc::default() };
        for &(w, h) in SIZES {
            for &(a1, a2) in ANGLES {
                both(&format!("{w}x{h} {a1} {a2}"), &gc, vec![arc(5, 3, w, h, a1, a2)]);
            }
        }
    }

    #[test]
    fn solid_clipped() {
        // Partly off the left or top: X.org's mi path, not fb's.
        let gc = Gc { fg: 0xff_ffff, ..Gc::default() };
        for &(x, y, w, h) in &[(-20, 10, 60, 60), (10, -30, 61, 61), (-100, -100, 300, 300), (-50, 20, 299, 150), (-300, -200, 700, 500), (-5, -5, 800, 800), (-5, -5, 800, 799), (-300, -400, 799, 800), (-1000, -1000, 2000, 2000)] {
            for &(a1, a2) in ANGLES {
                both(&format!("{x},{y} {w}x{h} {a1} {a2}"), &gc, vec![arc(x, y, w, h, a1, a2)]);
            }
        }
        // Off the right or bottom (here only where fb and mi agree, see
        // zero_poly_arc).
        for &(x, y, w, h) in &[(100, 50, 120, 120), (120, 100, 61, 61), (30, 20, 300, 151)] {
            for &(a1, a2) in ANGLES {
                check(&format!("{x},{y} {w}x{h} {a1} {a2}"), &gc, &Op::Arcs(vec![arc(x, y, w, h, a1, a2)]));
            }
        }
    }

    #[test]
    fn wrapping() {
        // Points past 16 bits wrap, as in X.org's DDXPointRecs.
        for style in [LineStyle::Solid, LineStyle::OnOffDash, LineStyle::DoubleDash] {
            let gc = Gc { fg: 0xff_ffff, bg: 0x00_ff00, line_style: style, dashes: vec![5, 2], ..Gc::default() };
            for &(x, y, w, h) in &[(32767, 5, 65535, 65535), (32700, 20, 65534, 65534), (32767, 20, 700, 50), (-32768, -32768, 65535, 65535)] {
                for &(a1, a2) in &[(0, 360 * 64), (10 * 64, 200 * 64), (100 * 64, -300 * 64), (45 * 64, 90 * 64)] {
                    both(&format!("{style:?} {x},{y} {w}x{h} {a1} {a2}"), &gc, vec![arc(x, y, w, h, a1, a2)]);
                }
            }
        }
    }

    #[test]
    fn several() {
        let gc = Gc { fg: 0xff_ffff, ..Gc::default() };
        both("overlapping", &gc, vec![arc(10, 10, 50, 50, 0, 360 * 64), arc(20, 15, 50, 40, 30 * 64, 200 * 64), arc(-10, 5, 80, 80, 0, 90 * 64), arc(30, 30, 0, 0, 0, 64)]);
    }

    const DASHES: &[(&[u8], u32)] = &[(&[4], 0), (&[1], 0), (&[3, 1], 0), (&[3, 1], 2), (&[1, 2, 3], 0), (&[1, 2, 3], 5), (&[7, 2, 1, 5], 0), (&[7, 2, 1, 5], 11), (&[2, 5], 100), (&[255, 1], 3)];

    #[test]
    fn dashed() {
        for style in [LineStyle::OnOffDash, LineStyle::DoubleDash] {
            for &(dashes, dash_offset) in DASHES {
                let gc = Gc { fg: 0xff_00ff, bg: 0x00_ff00, line_style: style, dashes: dashes.to_vec(), dash_offset, ..Gc::default() };
                for &(w, h) in &[(0, 0), (1, 1), (2, 2), (3, 3), (0, 9), (9, 0), (1, 6), (10, 10), (11, 11), (40, 25), (25, 41), (60, 60), (101, 61)] {
                    for &(a1, a2) in ANGLES {
                        both(&format!("{style:?} {dashes:?}+{dash_offset} {w}x{h} {a1} {a2}"), &gc, vec![arc(5, 3, w, h, a1, a2)]);
                    }
                }
            }
        }
    }

    #[test]
    fn dashed_several() {
        // Arcs that meet, so the dash pattern carries over.
        for style in [LineStyle::OnOffDash, LineStyle::DoubleDash] {
            for &(dashes, dash_offset) in DASHES {
                let gc = Gc { fg: 0xff_00ff, bg: 0x00_ff00, line_style: style, dashes: dashes.to_vec(), dash_offset, ..Gc::default() };
                let arcs = vec![
                    arc(10, 10, 60, 60, 0, 90 * 64),
                    arc(10, 10, 60, 60, 90 * 64, 90 * 64),
                    arc(10, 10, 60, 60, 180 * 64, 180 * 64),
                    arc(80, 10, 40, 60, 0, 360 * 64),
                    arc(80, 10, 40, 60, 45 * 64, -90 * 64),
                    arc(-20, 70, 100, 100, 10 * 64, 300 * 64),
                ];
                both(&format!("{style:?} {dashes:?}+{dash_offset}"), &gc, arcs.clone());
                both(&format!("{style:?} {dashes:?}+{dash_offset} 2"), &gc, arcs[..3].to_vec());
                both(&format!("{style:?} {dashes:?}+{dash_offset} rev"), &gc, vec![arc(10, 10, 60, 60, 90 * 64, -90 * 64), arc(10, 10, 60, 60, 0, -270 * 64)]);
            }
        }
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
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        let pick = |r: u32, v: &[i32]| v[r as usize % v.len()];
        let dims = [0, 1, 2, 3, 799, 800, 801, 1000, 32767, 32768, 65534, 65535];
        let coords = [-32768, -32767, -1, 0, 1, 100, 32766, 32767];
        let angles = [-32768, -23040, -1, 0, 1, 2880, 5760, 23040, 23041, 32767];
        for style in [LineStyle::Solid, LineStyle::OnOffDash, LineStyle::DoubleDash] {
            let gc = Gc { line_style: style, dashes: vec![3, 255, 1], dash_offset: 65535, ..Gc::default() };
            for _ in 0..300 {
                let w = if next() & 1 == 0 { pick(next(), &dims) } else { (next() % 65536) as i32 };
                let h = if next() & 3 == 0 { w } else if next() & 1 == 0 { pick(next(), &dims) } else { (next() % 65536) as i32 };
                // Mostly sizes the zero-arc code takes.
                let (w, h) = if next() & 1 == 0 && w != h { (w % 801, h % 801) } else { (w, h) };
                let a = arc(pick(next(), &coords), pick(next(), &coords), w, h, pick(next(), &angles), pick(next(), &angles));
                zero_poly_arc(&mut Count(0), &gc, &[a, a]);
            }
        }
    }
}
