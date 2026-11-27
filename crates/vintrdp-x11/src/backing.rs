//! Backing store and save-unders.
//!
//! Backing store as X.org does it (Composite's automatic redirection): a
//! window whose backing-store attribute isn't NotUseful gets, while it's
//! viewable, a pixmap of its own, border included. It and its inferiors draw
//! into that pixmap, clipped by nothing outside the window (so it's always
//! unobscured, for VisibilityNotify too), and what shows of it is copied to
//! the screen, or into the pixmap of a backed ancestor ([`State::composite`]).
//! Uncovering it so needs no Expose, GetImage reads what's covered, and
//! CopyArea from covered parts gives no GraphicsExpose. WhenMapped and Always
//! are the same, as in X.org: the pixmap goes when the window is unmapped.
//!
//! Save-unders, which X.org no longer has: while a window that asked for one
//! (Motif's menus do) is up, the screen beneath it is kept, and drawing
//! beneath it goes there too; when it goes, what it covered comes back
//! without Expose. Anything else changing beneath it (a window moved,
//! mapped, restacked) forgets what's beneath that window, which then gets
//! Expose as usual.

use std::collections::HashMap;

use crate::draw::Pixmap;
use crate::region::{Box, Region};
use crate::server::*;

pub struct Backing {
    /// Each pixmap made gets a new number, so the screen's pixels and a
    /// pixmap replaced by another (the window resized) can be told apart.
    pub serial: u64,
    /// The pixmap's origin on the screen: the window's outer corner.
    pub x: i32,
    pub y: i32,
    pub px: Pixmap,
    /// What changed since it was last copied out, in screen coordinates.
    pub dirty: Vec<Box>,
}

/// What a save-under window covers.
pub struct Save {
    /// Screen pixels, where `valid`.
    pub px: Vec<u32>,
    pub valid: Region,
    /// What of each window beneath it covers: those windows' clips (as they
    /// were when covered) within `valid`.
    pub under: HashMap<u32, Region>,
    /// The window's own place when its save was last taken.
    pub shown: Region,
}

impl State {
    /// Whether a window should have a pixmap of its own.
    pub fn redirected(&self, id: u32) -> bool {
        let w = &self.windows[&id];
        self.backing_store && w.backing_store != 0 && w.viewable && !w.input_only() && id != ROOT
    }

    /// The pixels, stride and screen origin of a buffer: the screen (0) or a
    /// window's pixmap.
    pub fn buffer_mut(&mut self, buf: u32) -> (&mut [u32], usize, i32, i32) {
        match self.backings.get_mut(&buf) {
            Some(b) if buf != 0 => (&mut b.px.px, b.px.width as usize, b.x, b.y),
            _ => (&mut self.fb, self.width as usize, 0, 0),
        }
    }

    pub fn buffer(&self, buf: u32) -> (&[u32], usize, i32, i32, Region) {
        match self.backings.get(&buf) {
            Some(b) if buf != 0 => (
                &b.px.px,
                b.px.width as usize,
                b.x,
                b.y,
                Region::rect(0, 0, b.px.width as i32, b.px.height as i32),
            ),
            _ => (
                &self.fb,
                self.width as usize,
                0,
                0,
                Region::rect(0, 0, self.width as i32, self.height as i32),
            ),
        }
    }

    /// A buffer by generation, current or just replaced (0: the screen).
    fn buffer_by_serial(&self, serial: u64) -> Option<(&[u32], usize, i32, i32, i32, i32)> {
        if serial == 0 {
            return Some((
                &self.fb,
                self.width as usize,
                0,
                0,
                self.width as i32,
                self.height as i32,
            ));
        }
        let b = self
            .backings
            .values()
            .chain(self.retired.values())
            .find(|b| b.serial == serial)?;
        Some((
            &b.px.px,
            b.px.width as usize,
            b.x,
            b.y,
            b.px.width as i32,
            b.px.height as i32,
        ))
    }

    pub fn buffer_origin_by_serial(&self, serial: u64) -> Option<(i32, i32)> {
        self.buffer_by_serial(serial).map(|b| (b.2, b.3))
    }

    pub fn serial_of(&self, buf: u32) -> u64 {
        if buf == 0 {
            0
        } else {
            self.backings.get(&buf).map_or(0, |b| b.serial)
        }
    }

    /// Something was drawn in a buffer, at `b` on the screen.
    pub fn touch_buffer(&mut self, buf: u32, b: Box) {
        match self.backings.get_mut(&buf) {
            Some(back) if buf != 0 => back.dirty.push(b),
            _ => self.damage(b),
        }
    }

    /// After `validate`: every window that should have a pixmap has one of
    /// its size, where it is; others have none. A new pixmap starts as what
    /// shows beneath (X.org's compNewPixmap); a replaced one is kept until
    /// the exposures are worked out, to copy from.
    pub fn sync_backings(&mut self, order: &[u32]) {
        let gone: Vec<u32> = self
            .backings
            .keys()
            .copied()
            .filter(|&id| !self.windows.contains_key(&id) || !self.redirected(id))
            .collect();
        for id in gone {
            let b = self.backings.remove(&id).unwrap();
            self.retired.insert(b.serial, b);
        }
        for &id in order {
            if !self.redirected(id) {
                continue;
            }
            let w = &self.windows[&id];
            let bw = w.border_width as i32;
            let (x, y, width, height) = (
                w.ax - bw,
                w.ay - bw,
                w.width as i32 + 2 * bw,
                w.height as i32 + 2 * bw,
            );
            let parent_buf = self.windows.get(&w.parent).map_or(0, |p| p.buf);
            let depth = w.depth;
            if let Some(b) = self.backings.get_mut(&id) {
                if (b.px.width as i32, b.px.height as i32) == (width, height) {
                    (b.x, b.y) = (x, y);
                    continue;
                }
                let b = self.backings.remove(&id).unwrap();
                self.retired.insert(b.serial, b);
            }
            let mut px = Pixmap::new(width as u16, height as u16, depth);
            {
                let (src, stride, ox, oy, avail) = self.buffer(parent_buf);
                let avail = avail
                    .translated(ox, oy)
                    .intersect(&Region::rect(x, y, width, height));
                for b in avail.boxes() {
                    for yy in b.y1..b.y2 {
                        let s = (yy - oy) as usize * stride + (b.x1 - ox) as usize;
                        let d = (yy - y) as usize * width as usize + (b.x1 - x) as usize;
                        let n = b.width() as usize;
                        px.px[d..d + n].copy_from_slice(&src[s..s + n]);
                    }
                }
            }
            self.next_serial += 1;
            self.backings.insert(
                id,
                Backing {
                    serial: self.next_serial,
                    x,
                    y,
                    px,
                    dirty: Vec::new(),
                },
            );
        }
    }

    /// Copy what changed in each pixmap to where it shows: the screen, or
    /// a backed ancestor's pixmap; the deepest first.
    pub fn composite(&mut self) {
        if self.backings.values().all(|b| b.dirty.is_empty()) {
            return;
        }
        let depth = |s: &Self, mut w: u32| {
            let mut n = 0;
            while let Some(win) = s.windows.get(&w) {
                n += 1;
                w = win.parent;
            }
            n
        };
        let mut ids: Vec<(usize, u32)> = self
            .backings
            .keys()
            .map(|&id| (depth(self, id), id))
            .collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        for (_, id) in ids {
            let dirty = std::mem::take(&mut self.backings.get_mut(&id).unwrap().dirty);
            if dirty.is_empty() {
                continue;
            }
            let Some(w) = self.windows.get(&id) else {
                continue;
            };
            let region = Region::from_boxes(&dirty).intersect(&w.shown);
            if region.is_empty() {
                continue;
            }
            let parent_buf = self.windows.get(&w.parent).map_or(0, |p| p.buf);
            let b = &self.backings[&id];
            let (bx, by, bstride) = (b.x, b.y, b.px.width as usize);
            let rows: Vec<Vec<u32>> = region
                .boxes()
                .iter()
                .flat_map(|r| (r.y1..r.y2).map(move |y| (y, r.x1, r.x2)))
                .map(|(y, x1, x2)| {
                    let s = (y - by) as usize * bstride + (x1 - bx) as usize;
                    b.px.px[s..s + (x2 - x1) as usize].to_vec()
                })
                .collect();
            let (dst, stride, ox, oy) = self.buffer_mut(parent_buf);
            let mut i = 0;
            for r in region.boxes() {
                for y in r.y1..r.y2 {
                    let d = (y - oy) as usize * stride + (r.x1 - ox) as usize;
                    dst[d..d + rows[i].len()].copy_from_slice(&rows[i]);
                    i += 1;
                }
            }
            for r in region.boxes() {
                self.touch_buffer(parent_buf, *r);
            }
        }
    }

    /// Copy pixels between buffers, by generation: `region` (screen
    /// coordinates, where they go) from `region` moved by (-dx, -dy) in the
    /// source. All read before any is written.
    pub fn copy_between(&mut self, copies: &[(u64, u64, Region, i32, i32)]) {
        let mut data = Vec::new();
        for (src, _, region, dx, dy) in copies {
            let Some((px, stride, ox, oy, w, h)) = self.buffer_by_serial(*src) else {
                data.push(None);
                continue;
            };
            // The source region, in screen coordinates, within the buffer.
            let from = region
                .translated(-dx, -dy)
                .intersect(&Region::rect(ox, oy, w, h));
            let mut rows = Vec::new();
            for b in from.boxes() {
                for y in b.y1..b.y2 {
                    let s = (y - oy) as usize * stride + (b.x1 - ox) as usize;
                    rows.push(px[s..s + b.width() as usize].to_vec());
                }
            }
            data.push(Some((from, rows)));
        }
        for ((_, dst, _, dx, dy), d) in copies.iter().zip(data) {
            let Some((from, rows)) = d else { continue };
            let Some(buf) = self.buf_for_serial(*dst) else {
                continue;
            };
            let (px, stride, ox, oy) = self.buffer_mut(buf);
            let mut i = 0;
            let mut touched = Vec::new();
            for b in from.boxes() {
                let (x1, n) = (b.x1 + dx, b.width() as usize);
                for y in b.y1..b.y2 {
                    let d = (y + dy - oy) as usize * stride + (x1 - ox) as usize;
                    px[d..d + n].copy_from_slice(&rows[i]);
                    i += 1;
                }
                touched.push(Box {
                    x1: b.x1 + dx,
                    y1: b.y1 + dy,
                    x2: b.x2 + dx,
                    y2: b.y2 + dy,
                });
            }
            for t in touched {
                self.touch_buffer(buf, t);
            }
        }
    }

    /// The buffer (0 or a window) with a generation, if it's current.
    fn buf_for_serial(&self, serial: u64) -> Option<u32> {
        if serial == 0 {
            return Some(0);
        }
        self.backings
            .iter()
            .find(|(_, b)| b.serial == serial)
            .map(|(&id, _)| id)
    }

    // Save-unders.

    /// Windows on the screen (not in a pixmap) that asked for a save-under.
    fn wants_save(&self, id: u32) -> bool {
        let w = &self.windows[&id];
        self.save_unders
            && w.save_under
            && w.viewable
            && !w.input_only()
            && w.buf == 0
            && id != ROOT
    }

    /// Before the exposures: take what's newly beneath each save-under
    /// window, from the screen as it was, and what of each window it covers.
    /// `before`: each window's clip as it was.
    pub fn take_saves(&mut self, before: &HashMap<u32, (bool, Region)>, order: &[u32]) {
        let (sw, sh) = (self.width as usize, self.height as usize);
        // Those that went cover nothing now; what they covered comes back.
        let went: Vec<u32> = self
            .saves
            .keys()
            .copied()
            .filter(|&id| !self.windows.contains_key(&id) || !self.wants_save(id))
            .collect();
        for id in went {
            self.saves.get_mut(&id).unwrap().shown = Region::new();
        }
        for &id in order {
            if !self.wants_save(id) {
                continue;
            }
            let shown = self.windows[&id].border_clip.clone();
            let tree = self.subtree(id);
            let mut save = self.saves.remove(&id).unwrap_or_else(|| Save {
                px: vec![0; sw * sh],
                valid: Region::new(),
                under: HashMap::new(),
                shown: Region::new(),
            });
            let new = shown.subtract(&save.shown);
            if !new.is_empty() {
                for b in new.boxes() {
                    for y in b.y1..b.y2 {
                        let s = y as usize * sw + b.x1 as usize;
                        save.px[s..s + b.width() as usize]
                            .copy_from_slice(&self.fb[s..s + b.width() as usize]);
                    }
                }
                for (&w, (was_on_screen, clip)) in before {
                    if !was_on_screen || tree.contains(&w) {
                        continue;
                    }
                    let under = clip.intersect(&new);
                    if !under.is_empty() {
                        let u = save.under.entry(w).or_default();
                        *u = u.union(&under);
                    }
                }
                save.valid = save.valid.union(&new);
            }
            save.shown = shown;
            self.saves.insert(id, save);
        }
    }

    /// Something beneath a save-under changed: forget what's beneath there.
    pub fn forget_saved(&mut self, window: u32, area: &Region) {
        for save in self.saves.values_mut() {
            if let Some(u) = save.under.get_mut(&window) {
                let lost = u.intersect(area);
                if !lost.is_empty() {
                    *u = u.subtract(&lost);
                    save.valid = save.valid.subtract(&lost);
                }
            }
        }
    }

    /// A window's newly exposed region: what saves can give back is put on
    /// the screen, and the rest returned, to paint and expose.
    pub fn restore_saved(&mut self, window: u32, exposed: &Region) -> Region {
        let mut rest = exposed.clone();
        let sw = self.width as usize;
        let ids: Vec<u32> = self.saves.keys().copied().collect();
        for id in ids {
            let save = &self.saves[&id];
            let Some(u) = save.under.get(&window) else {
                continue;
            };
            let back = rest
                .intersect(u)
                .intersect(&save.valid)
                .subtract(&save.shown);
            if back.is_empty() {
                continue;
            }
            for b in back.boxes() {
                for y in b.y1..b.y2 {
                    let s = y as usize * sw + b.x1 as usize;
                    let n = b.width() as usize;
                    let (save, fb) = (&self.saves[&id].px, &mut self.fb);
                    fb[s..s + n].copy_from_slice(&save[s..s + n]);
                }
                self.damage(*b);
            }
            rest = rest.subtract(&back);
        }
        rest
    }

    /// After the exposures: what save-unders no longer cover is forgotten,
    /// and saves of windows gone are dropped.
    pub fn trim_saves(&mut self) {
        let ids: Vec<u32> = self.saves.keys().copied().collect();
        for id in ids {
            if !self.windows.contains_key(&id) || !self.wants_save(id) {
                self.saves.remove(&id);
                continue;
            }
            let save = self.saves.get_mut(&id).unwrap();
            save.valid = save.valid.intersect(&save.shown);
            for u in save.under.values_mut() {
                *u = u.intersect(&save.shown);
            }
            save.under.retain(|_, u| !u.is_empty());
        }
    }

    /// Where drawing on a window also goes into saves (screen coordinates),
    /// for a GC's clip in screen coordinates (None: none).
    pub fn saved_beneath(
        &self,
        id: u32,
        include_inferiors: bool,
        gc_clip: Option<&Region>,
    ) -> Vec<(u32, Region)> {
        if self.saves.is_empty() {
            return Vec::new();
        }
        let tree = if include_inferiors {
            self.subtree(id)
        } else {
            vec![id]
        };
        let mut out = Vec::new();
        for (&s, save) in &self.saves {
            let mut r = Region::new();
            for w in &tree {
                if let Some(u) = save.under.get(w) {
                    r = r.union(u);
                }
            }
            let mut r = r.intersect(&save.valid);
            if let Some(c) = gc_clip {
                r = r.intersect(c);
            }
            if !r.is_empty() {
                out.push((s, r));
            }
        }
        out
    }
}
