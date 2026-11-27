//! Regions, as X keeps them: rectangles in bands of equal top and bottom,
//! top to bottom, each band's left to right and not touching, and no two
//! neighbouring bands alike. Windows' visible areas, exposures, clip lists
//! and shapes are all regions.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Box {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl Box {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Box {
        Box {
            x1: x,
            y1: y,
            x2: x + w,
            y2: y + h,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.x1 >= self.x2 || self.y1 >= self.y2
    }

    pub fn width(&self) -> i32 {
        self.x2 - self.x1
    }

    pub fn height(&self) -> i32 {
        self.y2 - self.y1
    }

    pub fn intersect(&self, o: &Box) -> Box {
        Box {
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
            x2: self.x2.min(o.x2),
            y2: self.y2.min(o.y2),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Region {
    boxes: Vec<Box>,
}

impl Region {
    pub fn new() -> Region {
        Region::default()
    }

    pub fn from_box(b: Box) -> Region {
        if b.is_empty() {
            Region::new()
        } else {
            Region { boxes: vec![b] }
        }
    }

    pub fn rect(x: i32, y: i32, w: i32, h: i32) -> Region {
        Region::from_box(Box::new(x, y, w, h))
    }

    /// The union of any rectangles.
    pub fn from_boxes(boxes: &[Box]) -> Region {
        let mut boxes: Vec<Box> = boxes.iter().copied().filter(|b| !b.is_empty()).collect();
        if boxes.len() <= 1 {
            return Region { boxes };
        }
        // Halve and merge, so n rectangles cost n log n, not n squared.
        fn build(b: &[Box]) -> Region {
            if b.len() == 1 {
                return Region { boxes: vec![b[0]] };
            }
            let (l, r) = b.split_at(b.len() / 2);
            build(l).union(&build(r))
        }
        boxes.sort_by_key(|b| (b.y1, b.x1));
        build(&boxes)
    }

    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    pub fn boxes(&self) -> &[Box] {
        &self.boxes
    }

    pub fn extents(&self) -> Box {
        let Some(first) = self.boxes.first() else {
            return Box {
                x1: 0,
                y1: 0,
                x2: 0,
                y2: 0,
            };
        };
        let mut e = Box {
            x1: first.x1,
            y1: first.y1,
            x2: first.x2,
            y2: self.boxes[self.boxes.len() - 1].y2,
        };
        for b in &self.boxes {
            e.x1 = e.x1.min(b.x1);
            e.x2 = e.x2.max(b.x2);
        }
        e
    }

    pub fn translate(&mut self, dx: i32, dy: i32) {
        for b in &mut self.boxes {
            *b = Box {
                x1: b.x1 + dx,
                y1: b.y1 + dy,
                x2: b.x2 + dx,
                y2: b.y2 + dy,
            };
        }
    }

    pub fn translated(&self, dx: i32, dy: i32) -> Region {
        let mut r = self.clone();
        r.translate(dx, dy);
        r
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.band_at(y).iter().any(|b| b.x1 <= x && x < b.x2)
    }

    /// The boxes of the band covering row `y`, if any.
    pub fn band_at(&self, y: i32) -> &[Box] {
        // The first box whose band ends below y.
        let i = self.boxes.partition_point(|b| b.y2 <= y);
        match self.boxes.get(i) {
            Some(b) if b.y1 <= y => {
                let y1 = b.y1;
                let n = self.boxes[i..].iter().take_while(|c| c.y1 == y1).count();
                &self.boxes[i..i + n]
            }
            _ => &[],
        }
    }

    /// Every box is in `other` too.
    pub fn within(&self, other: &Region) -> bool {
        self.subtract(other).is_empty()
    }

    pub fn union(&self, o: &Region) -> Region {
        if self.is_empty() {
            return o.clone();
        }
        if o.is_empty() {
            return self.clone();
        }
        combine(self, o, |a, b| a || b)
    }

    pub fn intersect(&self, o: &Region) -> Region {
        if self.is_empty() || o.is_empty() {
            return Region::new();
        }
        let (a, b) = (self.extents(), o.extents());
        if a.intersect(&b).is_empty() {
            return Region::new();
        }
        if o.boxes.len() == 1
            && o.boxes[0].x1 <= a.x1
            && o.boxes[0].y1 <= a.y1
            && o.boxes[0].x2 >= a.x2
            && o.boxes[0].y2 >= a.y2
        {
            return self.clone();
        }
        combine(self, o, |a, b| a && b)
    }

    pub fn intersect_box(&self, b: Box) -> Region {
        self.intersect(&Region::from_box(b))
    }

    pub fn subtract(&self, o: &Region) -> Region {
        if self.is_empty() || o.is_empty() || self.extents().intersect(&o.extents()).is_empty() {
            return self.clone();
        }
        combine(self, o, |a, b| a && !b)
    }
}

/// The bands of a region: (top, bottom, boxes).
fn bands(r: &Region) -> Vec<(i32, i32, &[Box])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < r.boxes.len() {
        let y1 = r.boxes[i].y1;
        let n = r.boxes[i..].iter().take_while(|b| b.y1 == y1).count();
        out.push((y1, r.boxes[i].y2, &r.boxes[i..i + n]));
        i += n;
    }
    out
}

/// A row's spans combined: the x ranges where `op(in a, in b)`.
fn combine_row(a: &[Box], b: &[Box], op: fn(bool, bool) -> bool, out: &mut Vec<(i32, i32)>) {
    let mut xs: Vec<i32> = a.iter().chain(b).flat_map(|b| [b.x1, b.x2]).collect();
    xs.sort_unstable();
    xs.dedup();
    let (mut ia, mut ib) = (0, 0);
    for w in xs.windows(2) {
        let x = w[0];
        while ia < a.len() && a[ia].x2 <= x {
            ia += 1;
        }
        while ib < b.len() && b[ib].x2 <= x {
            ib += 1;
        }
        let ina = ia < a.len() && a[ia].x1 <= x;
        let inb = ib < b.len() && b[ib].x1 <= x;
        if op(ina, inb) {
            match out.last_mut() {
                Some(last) if last.1 == x => last.1 = w[1],
                _ => out.push((x, w[1])),
            }
        }
    }
}

fn combine(a: &Region, b: &Region, op: fn(bool, bool) -> bool) -> Region {
    let (ba, bb) = (bands(a), bands(b));
    let mut ys: Vec<i32> = ba.iter().chain(&bb).flat_map(|b| [b.0, b.1]).collect();
    ys.sort_unstable();
    ys.dedup();
    let mut out: Vec<Box> = Vec::new();
    // Where the last band starts in `out`, and its spans.
    let mut last: Option<(usize, Vec<(i32, i32)>)> = None;
    let (mut ia, mut ib) = (0, 0);
    let mut row = Vec::new();
    for w in ys.windows(2) {
        let (y1, y2) = (w[0], w[1]);
        while ia < ba.len() && ba[ia].1 <= y1 {
            ia += 1;
        }
        while ib < bb.len() && bb[ib].1 <= y1 {
            ib += 1;
        }
        let ra: &[Box] = if ia < ba.len() && ba[ia].0 <= y1 {
            ba[ia].2
        } else {
            &[]
        };
        let rb: &[Box] = if ib < bb.len() && bb[ib].0 <= y1 {
            bb[ib].2
        } else {
            &[]
        };
        row.clear();
        combine_row(ra, rb, op, &mut row);
        if row.is_empty() {
            last = None;
            continue;
        }
        // The same spans as the band just above, touching it: make it taller.
        if let Some((start, spans)) = &last {
            if *spans == row && out[*start].y2 == y1 {
                for b in &mut out[*start..] {
                    b.y2 = y2;
                }
                continue;
            }
        }
        let start = out.len();
        out.extend(row.iter().map(|&(x1, x2)| Box { x1, y1, x2, y2 }));
        last = Some((start, row.clone()));
    }
    Region { boxes: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations() {
        let a = Region::rect(0, 0, 10, 10);
        let b = Region::rect(5, 5, 10, 10);
        let u = a.union(&b);
        assert_eq!(
            u.boxes(),
            [
                Box::new(0, 0, 10, 5),
                Box::new(0, 5, 15, 5),
                Box::new(5, 10, 10, 5)
            ]
        );
        assert_eq!(a.intersect(&b).boxes(), [Box::new(5, 5, 5, 5)]);
        let s = a.subtract(&b);
        assert_eq!(s.boxes(), [Box::new(0, 0, 10, 5), Box::new(0, 5, 5, 5)]);
        assert!(s.contains(0, 9) && !s.contains(5, 5) && !s.contains(10, 0));
        // A hole in the middle.
        let ring = Region::rect(0, 0, 9, 9).subtract(&Region::rect(3, 3, 3, 3));
        assert_eq!(ring.boxes().len(), 4);
        assert_eq!(
            ring.band_at(4),
            [Box::new(0, 3, 3, 3), Box::new(6, 3, 3, 3)]
        );
        assert_eq!(
            ring.union(&Region::rect(3, 3, 3, 3)),
            Region::rect(0, 0, 9, 9)
        );
        assert_eq!(ring.extents(), Box::new(0, 0, 9, 9));
        // Bands that come out alike merge.
        let r = Region::from_boxes(&[
            Box::new(0, 0, 4, 2),
            Box::new(0, 2, 4, 2),
            Box::new(2, 0, 4, 4),
        ]);
        assert_eq!(r, Region::rect(0, 0, 6, 4));
        assert!(Region::rect(1, 1, 2, 2).within(&r));
        assert_eq!(a.translated(3, -1).extents(), Box::new(3, -1, 10, 10));
    }
}
