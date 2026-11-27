//! Thin lines: mizerline.c (miZeroLine, miZeroDashLine), midash.c,
//! mipolypnt.c, mipolyseg.c and mipolyrect.c, ported.

use super::*;

// STUB(x11): being ported (mizerline.c, midash.c, mipolypnt.c, mipolyrect.c)
pub fn poly_point(_sink: &mut dyn Sink, _gc: &Gc, _mode: CoordMode, _pts: &[Point]) {}
pub fn zero_line(_sink: &mut dyn Sink, _gc: &Gc, _mode: CoordMode, _pts: &[Point]) {}
pub fn zero_dash_line(_sink: &mut dyn Sink, _gc: &Gc, _mode: CoordMode, _pts: &[Point]) {}
pub fn zero_segments(_sink: &mut dyn Sink, _gc: &Gc, _segs: &[Segment]) {}
pub fn poly_rectangle(_sink: &mut dyn Sink, _gc: &Gc, _rects: &[Rect]) {}
