//! Pixels: pixmaps, and the canvas that fills spans and rows into a window
//! (on the screen) or a pixmap through a GC's function, plane mask, fill
//! style and clip, as X.org's fb does.

use std::cell::RefCell;
use std::rc::Rc;

use crate::mi::{self, Span};
use crate::region::{Box, Region};

/// A pixmap: one u32 a pixel whatever its depth, the bits above the depth
/// zero.
pub struct Pixmap {
    pub width: u16,
    pub height: u16,
    pub depth: u8,
    pub px: Vec<u32>,
}

pub type PixmapRef = Rc<RefCell<Pixmap>>;

impl Pixmap {
    pub fn new(width: u16, height: u16, depth: u8) -> Pixmap {
        Pixmap {
            width,
            height,
            depth,
            px: vec![0; width as usize * height as usize],
        }
    }

    pub fn get(&self, x: i32, y: i32) -> u32 {
        self.px[y as usize * self.width as usize + x as usize]
    }
}

pub fn depth_mask(depth: u8) -> u32 {
    if depth >= 32 { !0 } else { (1 << depth) - 1 }
}

/// GX functions.
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

/// What a span is filled with, its pattern's origin in buffer coordinates.
#[derive(Clone)]
pub enum Fill {
    Solid,
    Tiled {
        px: Rc<Vec<u32>>,
        w: i32,
        h: i32,
        x: i32,
        y: i32,
    },
    /// A stipple: 1 bits drawn; 0 bits drawn in the background if opaque.
    Stippled {
        bits: Rc<Vec<u32>>,
        w: i32,
        h: i32,
        x: i32,
        y: i32,
        opaque: bool,
    },
}

impl Fill {
    /// A tile from a pixmap, its origin at (x, y) in the buffer.
    pub fn tile(p: &Pixmap, x: i32, y: i32) -> Fill {
        Fill::Tiled {
            px: Rc::new(p.px.clone()),
            w: p.width as i32,
            h: p.height as i32,
            x,
            y,
        }
    }

    pub fn stipple(p: &Pixmap, x: i32, y: i32, opaque: bool) -> Fill {
        Fill::Stippled {
            bits: Rc::new(p.px.clone()),
            w: p.width as i32,
            h: p.height as i32,
            x,
            y,
            opaque,
        }
    }
}

/// Drawing into a buffer: a window's (the screen) or a pixmap's.
pub struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub stride: usize,
    /// Where the drawable's origin is in the buffer.
    pub dx: i32,
    pub dy: i32,
    /// In buffer coordinates, within the buffer.
    pub clip: &'a Region,
    pub function: u8,
    /// Already limited to the drawable's depth.
    pub plane_mask: u32,
    pub depth_mask: u32,
    pub fill: &'a Fill,
    pub bg: u32,
    /// The bounding box of what was drawn, in buffer coordinates.
    pub touched: Option<Box>,
}

impl Canvas<'_> {
    fn touch(&mut self, x1: i32, y: i32, x2: i32) {
        let b = Box {
            x1,
            y1: y,
            x2,
            y2: y + 1,
        };
        self.touched = Some(match self.touched {
            None => b,
            Some(t) => Box {
                x1: t.x1.min(b.x1),
                y1: t.y1.min(b.y1),
                x2: t.x2.max(b.x2),
                y2: t.y2.max(b.y2),
            },
        });
    }

    #[inline]
    fn put(&mut self, i: usize, src: u32) {
        let d = self.px[i];
        let pm = self.plane_mask;
        self.px[i] = (rop(self.function, src, d) & pm) | (d & !pm);
    }

    /// Fill buffer row `y` from x1 to x2 with the fill, `pixel` as foreground.
    fn fill_row(&mut self, x1: i32, x2: i32, y: i32, pixel: u32) {
        let clip = self.clip;
        let mut drawn = None::<(i32, i32)>;
        for b in clip.band_at(y) {
            let (a, z) = (x1.max(b.x1), x2.min(b.x2));
            if a >= z {
                continue;
            }
            drawn = Some(drawn.map_or((a, z), |(l, r)| (l.min(a), r.max(z))));
            let row = y as usize * self.stride;
            match self.fill {
                Fill::Solid => {
                    if self.function == 3 && self.plane_mask == self.depth_mask {
                        self.px[row + a as usize..row + z as usize].fill(pixel);
                    } else {
                        for x in a..z {
                            self.put(row + x as usize, pixel);
                        }
                    }
                }
                Fill::Tiled {
                    px,
                    w,
                    h,
                    x: ox,
                    y: oy,
                } => {
                    let (px, w, h) = (px.clone(), *w, *h);
                    let ty = (y - oy).rem_euclid(h) * w;
                    for x in a..z {
                        let src = px[(ty + (x - ox).rem_euclid(w)) as usize];
                        self.put(row + x as usize, src);
                    }
                }
                Fill::Stippled {
                    bits,
                    w,
                    h,
                    x: ox,
                    y: oy,
                    opaque,
                } => {
                    let (bits, w, h, opaque, bg) = (bits.clone(), *w, *h, *opaque, self.bg);
                    let ty = (y - oy).rem_euclid(h) * w;
                    for x in a..z {
                        if bits[(ty + (x - ox).rem_euclid(w)) as usize] != 0 {
                            self.put(row + x as usize, pixel);
                        } else if opaque {
                            self.put(row + x as usize, bg);
                        }
                    }
                }
            }
        }
        if let Some((l, r)) = drawn {
            self.touch(l, y, r);
        }
    }

    /// Pixels straight from `row` (an image, a copy), at (x, y) in drawable
    /// coordinates, through the function and plane mask; `None` pixels left.
    pub fn put_row(&mut self, x: i32, y: i32, row: &[Option<u32>]) {
        let (bx, by) = (x + self.dx, y + self.dy);
        let clip = self.clip;
        let mut drawn = None::<(i32, i32)>;
        for b in clip.band_at(by) {
            let (a, z) = (bx.max(b.x1), (bx + row.len() as i32).min(b.x2));
            if a >= z {
                continue;
            }
            drawn = Some(drawn.map_or((a, z), |(l, r)| (l.min(a), r.max(z))));
            let base = by as usize * self.stride;
            for xx in a..z {
                if let Some(src) = row[(xx - bx) as usize] {
                    self.put(base + xx as usize, src);
                }
            }
        }
        if let Some((l, r)) = drawn {
            self.touch(l, by, r);
        }
    }
}

impl mi::Sink for Canvas<'_> {
    fn clip_contains(&self, r: mi::Rect) -> bool {
        let b = Region::rect(r.x + self.dx, r.y + self.dy, r.w, r.h);
        b.within(self.clip)
    }

    fn clip_extents(&self) -> Option<mi::Rect> {
        let e = self.clip.extents();
        Some(mi::Rect {
            x: e.x1 - self.dx,
            y: e.y1 - self.dy,
            w: e.width(),
            h: e.height(),
        })
    }

    fn fill_spans(&mut self, pixel: u32, spans: &[Span]) {
        let e = self.clip.extents();
        for s in spans {
            let (y, x1, x2) = (s.y + self.dy, s.x + self.dx, s.x + self.dx + s.w);
            if s.w <= 0 || y < e.y1 || y >= e.y2 || x2 <= e.x1 || x1 >= e.x2 {
                continue;
            }
            self.fill_row(x1, x2, y, pixel);
        }
    }

    fn fill_rects(&mut self, pixel: u32, rects: &[mi::Rect]) {
        let e = self.clip.extents();
        for r in rects.iter().filter(|r| r.w > 0 && r.h > 0) {
            let (x1, x2) = (r.x + self.dx, r.x + self.dx + r.w);
            for y in (r.y + self.dy).max(e.y1)..(r.y + self.dy + r.h).min(e.y2) {
                self.fill_row(x1, x2, y, pixel);
            }
        }
    }
}

/// Read a rectangle of a buffer: `None` where `avail` (buffer coordinates)
/// doesn't cover it.
pub fn read(
    px: &[u32],
    stride: usize,
    avail: &Region,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Vec<Vec<Option<u32>>> {
    (y..y + h)
        .map(|yy| {
            let mut row = vec![None; w.max(0) as usize];
            for b in avail.band_at(yy) {
                for xx in x.max(b.x1)..(x + w).min(b.x2) {
                    row[(xx - x) as usize] = Some(px[yy as usize * stride + xx as usize]);
                }
            }
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mi::Sink;

    #[test]
    fn spans_through_clip_and_fills() {
        let mut px = vec![0u32; 8 * 4];
        let clip = Region::rect(1, 0, 6, 4).subtract(&Region::rect(3, 1, 2, 1));
        let tile = Pixmap {
            width: 2,
            height: 1,
            depth: 24,
            px: vec![5, 6],
        };
        let fill = Fill::tile(&tile, 0, 0);
        let mut c = Canvas {
            px: &mut px,
            stride: 8,
            dx: 0,
            dy: 0,
            clip: &clip,
            function: 3,
            plane_mask: !0,
            depth_mask: !0,
            fill: &fill,
            bg: 0,
            touched: None,
        };
        c.fill_spans(9, &[Span { x: -2, y: 1, w: 20 }]);
        assert_eq!(&px[8..16], &[0, 6, 5, 0, 0, 6, 5, 0]);
        let fill = Fill::Solid;
        let mut c = Canvas {
            px: &mut px,
            stride: 8,
            dx: 1,
            dy: 1,
            clip: &clip,
            function: 6,
            plane_mask: 0xf,
            depth_mask: 0xff,
            fill: &fill,
            bg: 0,
            touched: None,
        };
        c.fill_rects(
            0xff,
            &[mi::Rect {
                x: 0,
                y: 1,
                w: 2,
                h: 1,
            }],
        );
        assert_eq!(
            c.touched,
            Some(Box {
                x1: 1,
                y1: 2,
                x2: 3,
                y2: 3
            })
        );
        assert_eq!(&px[16..20], &[0, 0xf, 0xf, 0]);
    }
}
