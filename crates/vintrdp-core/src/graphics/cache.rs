//! The caches the server fills and draws from: bitmaps (with the LRU list
//! the persistent cache needs), glyphs, text fragments, the desktop save
//! area, cursors and brushes. Port of rdesktop's cache.c.

use std::sync::Arc;

use super::surface::{Bitmap, Glyph};
use crate::session::CursorImage;

/// Cells per bitmap cache. Cache 2 addresses this many on disk.
pub const BMPCACHE_CELLS: usize = 0xa00;
/// What cache 2 keeps in memory when it's persistent: rdesktop's
/// BMPCACHE2_C2_CELLS. The rest is reloaded from disk on demand.
pub const BMPCACHE2_C2_CELLS: usize = 0x150;
/// The volatile slot of each cache (bitmap cache rev. 2).
pub const VOLATILE_IDX: u16 = 0x7fff;
/// How far up the LRU list a used bitmap moves: less than all the way, so
/// rarely used ones go first (rdesktop's BUMP_COUNT).
const BUMP_COUNT: i32 = 40;
const NOT_SET: i16 = -1;

#[derive(Default)]
struct Cell {
    bitmap: Option<Bitmap>,
    previous: i16,
    next: i16,
}

struct Cache {
    cells: Vec<Cell>,
    volatile: Option<Bitmap>,
    lru: i16,
    mru: i16,
    count: usize,
    /// Backed by the persistent cache, so kept in LRU order.
    persistent: bool,
}

impl Cache {
    fn new() -> Self {
        Cache {
            cells: (0..BMPCACHE_CELLS).map(|_| Cell { bitmap: None, previous: NOT_SET, next: NOT_SET }).collect(),
            volatile: None,
            lru: NOT_SET,
            mru: NOT_SET,
            count: 0,
            persistent: false,
        }
    }

    fn cell(&mut self, idx: i16) -> Option<&mut Cell> {
        usize::try_from(idx).ok().and_then(|i| self.cells.get_mut(i))
    }
}

/// The three bitmap caches.
pub struct BitmapCache {
    caches: [Cache; 3],
}

impl BitmapCache {
    pub fn new() -> Self {
        BitmapCache { caches: [Cache::new(), Cache::new(), Cache::new()] }
    }

    pub fn set_persistent(&mut self, id: u8, on: bool) {
        if let Some(c) = self.caches.get_mut(id as usize) {
            c.persistent = on;
        }
    }

    pub fn is_persistent(&self, id: u8) -> bool {
        self.caches.get(id as usize).is_some_and(|c| c.persistent)
    }

    /// Whether `get` would find something there without the disk.
    pub fn contains(&self, id: u8, idx: u16) -> bool {
        let Some(c) = self.caches.get(id as usize) else { return false };
        if idx == VOLATILE_IDX {
            return c.volatile.is_some();
        }
        c.cells.get(idx as usize).is_some_and(|cell| cell.bitmap.is_some())
    }

    /// A cached bitmap, moved up the LRU list if the cache is persistent.
    pub fn get(&mut self, id: u8, idx: u16) -> Option<&Bitmap> {
        let i = id as usize;
        if i >= 3 {
            return None;
        }
        if (idx as usize) < BMPCACHE_CELLS {
            self.caches[i].cells[idx as usize].bitmap.as_ref()?;
            if self.caches[i].persistent {
                self.bump(id, idx, BUMP_COUNT);
            }
            return self.caches[i].cells[idx as usize].bitmap.as_ref();
        }
        if idx == VOLATILE_IDX {
            return self.caches[i].volatile.as_ref();
        }
        None
    }

    /// The volatile bitmap, when it's this size and can be overwritten in
    /// place (cache_get_bitmap_for_update).
    pub fn volatile_for_update(&mut self, id: u8, idx: u16, width: usize, height: usize) -> Option<&mut Bitmap> {
        if idx != VOLATILE_IDX {
            return None;
        }
        let b = self.caches.get_mut(id as usize)?.volatile.as_mut()?;
        (b.width == width && b.height == height).then_some(b)
    }

    /// Store a bitmap. Returns the index of a bitmap evicted to make room,
    /// whose persistent cache stamp should be cleared.
    pub fn put(&mut self, id: u8, idx: u16, bitmap: Bitmap) -> Option<u16> {
        let i = id as usize;
        if i >= 3 {
            crate::graphics::warn(format_args!("bitmap cache {id} doesn't exist"));
            return None;
        }
        if idx == VOLATILE_IDX {
            self.caches[i].volatile = Some(bitmap);
            return None;
        }
        let Some(cell) = self.caches[i].cells.get_mut(idx as usize) else {
            crate::graphics::warn(format_args!("bitmap cache {id} has no cell {idx}"));
            return None;
        };
        let was_empty = cell.bitmap.replace(bitmap).is_none();
        if !self.caches[i].persistent {
            return None;
        }
        if was_empty {
            cell.previous = NOT_SET;
            cell.next = NOT_SET;
        }
        self.bump(id, idx, -1);
        if self.caches[i].count > BMPCACHE2_C2_CELLS { self.evict(id) } else { None }
    }

    /// Move a bitmap `bump` places up from the bottom of the LRU list, or to
    /// the top when `bump` is negative (cache_bump_bitmap).
    fn bump(&mut self, id: u8, idx: u16, bump: i32) {
        let c = &mut self.caches[id as usize];
        let idx = idx as i16;
        if !c.persistent || c.mru == idx {
            return;
        }
        let (mut p_idx, mut n_idx) = match c.cell(idx) {
            Some(cell) => (cell.previous, cell.next),
            None => return,
        };
        if n_idx != NOT_SET {
            // In the list: take it out.
            c.count = c.count.saturating_sub(1);
            match c.cell(p_idx) {
                Some(p) => p.next = n_idx,
                None => c.lru = n_idx,
            }
            if let Some(n) = c.cell(n_idx) {
                n.previous = p_idx;
            }
        } else {
            p_idx = NOT_SET;
            n_idx = c.lru;
        }
        if bump >= 0 {
            let mut n = 0;
            while n < bump && n_idx != NOT_SET {
                p_idx = n_idx;
                n_idx = c.cell(p_idx).map_or(NOT_SET, |cell| cell.next);
                n += 1;
            }
        } else {
            p_idx = c.mru;
            n_idx = NOT_SET;
        }
        c.count += 1;
        if let Some(cell) = c.cell(idx) {
            cell.previous = p_idx;
            cell.next = n_idx;
        }
        match c.cell(p_idx) {
            Some(p) => p.next = idx,
            None => c.lru = idx,
        }
        match c.cell(n_idx) {
            Some(n) => n.previous = idx,
            None => c.mru = idx,
        }
    }

    /// Drop the least recently used bitmap; returns its index.
    fn evict(&mut self, id: u8) -> Option<u16> {
        let c = &mut self.caches[id as usize];
        let idx = c.lru;
        let cell = c.cell(idx)?;
        let next = cell.next;
        cell.bitmap = None;
        c.count = c.count.saturating_sub(1);
        c.lru = next;
        if let Some(n) = c.cell(next) {
            n.previous = NOT_SET;
        } else {
            c.mru = NOT_SET;
        }
        u16::try_from(idx).ok()
    }

    /// Relink the LRU list from cells in least to most recently used order,
    /// skipping empty ones (cache_rebuild_bmpcache_linked_list).
    pub fn rebuild(&mut self, id: u8, order: &[u16]) {
        let Some(c) = self.caches.get_mut(id as usize) else { return };
        let live: Vec<i16> = order
            .iter()
            .filter(|&&i| c.cells.get(i as usize).is_some_and(|cell| cell.bitmap.is_some()))
            .map(|&i| i as i16)
            .collect();
        c.lru = live.first().copied().unwrap_or(NOT_SET);
        c.mru = live.last().copied().unwrap_or(NOT_SET);
        c.count = live.len();
        for (k, &i) in live.iter().enumerate() {
            let previous = if k > 0 { live[k - 1] } else { NOT_SET };
            let next = live.get(k + 1).copied().unwrap_or(NOT_SET);
            if let Some(cell) = c.cell(i) {
                cell.previous = previous;
                cell.next = next;
            }
        }
    }

    /// The cells of a cache from least to most recently used.
    pub fn lru_order(&self, id: u8) -> Vec<u16> {
        let mut out = Vec::new();
        let Some(c) = self.caches.get(id as usize) else { return out };
        let mut idx = c.lru;
        while let Ok(i) = usize::try_from(idx) {
            // A corrupt list can't loop forever.
            if out.len() >= BMPCACHE_CELLS {
                break;
            }
            out.push(i as u16);
            idx = c.cells.get(i).map_or(NOT_SET, |cell| cell.next);
        }
        out
    }
}

/// A glyph from the glyph cache, as FONTGLYPH.
pub struct FontGlyph {
    pub offset: i16,
    pub baseline: i16,
    pub width: u16,
    pub height: u16,
    pub glyph: Glyph,
}

/// Twelve fonts of 256 glyphs.
pub struct GlyphCache {
    fonts: Vec<Vec<Option<FontGlyph>>>,
}

impl GlyphCache {
    pub fn new() -> Self {
        GlyphCache { fonts: (0..12).map(|_| (0..256).map(|_| None).collect()).collect() }
    }

    pub fn get(&self, font: u8, character: u16) -> Option<&FontGlyph> {
        self.fonts.get(font as usize)?.get(character as usize)?.as_ref()
    }

    pub fn put(&mut self, font: u8, character: u16, glyph: FontGlyph) {
        match self.fonts.get_mut(font as usize).and_then(|f| f.get_mut(character as usize)) {
            Some(slot) => *slot = Some(glyph),
            None => crate::graphics::warn(format_args!("no glyph cache slot for font {font}, character {character}")),
        }
    }
}

/// The text fragment cache: 256 runs of glyph indices for text2 to replay.
pub struct TextCache {
    entries: Vec<Vec<u8>>,
}

impl TextCache {
    pub fn new() -> Self {
        TextCache { entries: vec![Vec::new(); 256] }
    }

    pub fn get(&self, id: u8) -> &[u8] {
        &self.entries[id as usize]
    }

    pub fn put(&mut self, id: u8, data: &[u8]) {
        self.entries[id as usize] = data.to_vec();
    }
}

/// The desktop save area, in pixels (rdesktop keeps 0x38400 4-byte ones).
pub const DESK_CACHE_PIXELS: usize = 0x38400;

pub struct DeskCache {
    px: Vec<u32>,
}

impl DeskCache {
    pub fn new() -> Self {
        DeskCache { px: vec![0; DESK_CACHE_PIXELS] }
    }

    /// Where `len` pixels at `offset` would go, if they fit. Like rdesktop,
    /// an offset past the end means the start.
    fn range(offset: u32, len: u64) -> Option<std::ops::Range<usize>> {
        let offset = if offset as u64 > DESK_CACHE_PIXELS as u64 { 0 } else { offset as u64 };
        let end = offset.checked_add(len)?;
        (end <= DESK_CACHE_PIXELS as u64).then_some(offset as usize..end as usize)
    }

    /// Whether `cx` x `cy` pixels fit at `offset`.
    pub fn fits(offset: u32, cx: i32, cy: i32) -> bool {
        cx > 0 && cy > 0 && Self::range(offset, cx as u64 * cy as u64).is_some()
    }

    pub fn put(&mut self, offset: u32, px: &[u32]) {
        match Self::range(offset, px.len() as u64) {
            Some(r) => self.px[r].copy_from_slice(px),
            None => crate::graphics::warn(format_args!("desktop save of {} pixels at {offset} doesn't fit", px.len())),
        }
    }

    pub fn get(&self, offset: u32, len: usize) -> Option<&[u32]> {
        Self::range(offset, len as u64).map(|r| &self.px[r])
    }
}

/// Cached pointers. `None` in a slot is a pointer we couldn't make, which
/// hides the cursor like rdesktop's NULL one.
pub struct CursorCache {
    slots: Vec<Option<Arc<CursorImage>>>,
}

impl CursorCache {
    pub fn new() -> Self {
        CursorCache { slots: vec![None; 0x20] }
    }

    pub fn get(&self, idx: u16) -> Option<Arc<CursorImage>> {
        self.slots.get(idx as usize).cloned().flatten()
    }

    pub fn put(&mut self, idx: u16, cursor: Option<Arc<CursorImage>>) {
        match self.slots.get_mut(idx as usize) {
            Some(slot) => *slot = cursor,
            None => crate::graphics::warn(format_args!("no cursor cache slot {idx}")),
        }
    }
}

/// A cached brush: `colour_code` 1 is 8 bytes of 1 bpp rows, top first;
/// 3 to 6 are 8x8 pixels of `colour_code - 2` bytes.
#[derive(Clone)]
pub struct BrushData {
    pub colour_code: u8,
    pub data: Vec<u8>,
}

/// Two-colour brushes, and multi-colour ones, 64 of each.
pub struct BrushCache {
    slots: [Vec<Option<BrushData>>; 2],
}

impl BrushCache {
    pub fn new() -> Self {
        BrushCache { slots: [vec![None; 64], vec![None; 64]] }
    }

    fn set(colour_code: u8) -> usize {
        if colour_code == 1 { 0 } else { 1 }
    }

    pub fn get(&self, colour_code: u8, idx: u8) -> Option<&BrushData> {
        self.slots[Self::set(colour_code)].get(idx as usize)?.as_ref()
    }

    pub fn put(&mut self, idx: u8, brush: BrushData) {
        match self.slots[Self::set(brush.colour_code)].get_mut(idx as usize) {
            Some(slot) => *slot = Some(brush),
            None => crate::graphics::warn(format_args!("no brush cache slot {idx}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bmp(tag: u32) -> Bitmap {
        Bitmap { width: 1, height: 1, px: vec![tag] }
    }

    #[test]
    fn plain_cache_stores_and_replaces() {
        let mut c = BitmapCache::new();
        assert!(c.get(0, 5).is_none());
        assert_eq!(c.put(0, 5, bmp(1)), None);
        assert_eq!(c.get(0, 5).unwrap().px, [1]);
        c.put(0, 5, bmp(2));
        assert_eq!(c.get(0, 5).unwrap().px, [2]);
        // Out of range is refused, not a panic.
        assert_eq!(c.put(3, 0, bmp(1)), None);
        assert_eq!(c.put(0, 0xa00, bmp(1)), None);
        assert!(c.get(7, 0).is_none());
        // Nothing is ordered when the cache isn't persistent.
        assert!(c.lru_order(0).is_empty());
    }

    #[test]
    fn volatile_slot() {
        let mut c = BitmapCache::new();
        c.put(1, VOLATILE_IDX, Bitmap { width: 2, height: 1, px: vec![1, 2] });
        assert_eq!(c.get(1, VOLATILE_IDX).unwrap().px, [1, 2]);
        assert!(c.volatile_for_update(1, VOLATILE_IDX, 2, 1).is_some());
        assert!(c.volatile_for_update(1, VOLATILE_IDX, 1, 2).is_none());
        assert!(c.volatile_for_update(1, 3, 2, 1).is_none());
    }

    #[test]
    fn lru_order_and_bump() {
        let mut c = BitmapCache::new();
        c.set_persistent(2, true);
        for i in 0..5 {
            c.put(2, i, bmp(i as u32));
        }
        assert_eq!(c.lru_order(2), [0, 1, 2, 3, 4]);
        // Use moves a bitmap BUMP_COUNT places up, here past the top.
        c.get(2, 0);
        assert_eq!(c.lru_order(2), [1, 2, 3, 4, 0]);
        // Replacing puts it on top.
        c.put(2, 2, bmp(9));
        assert_eq!(c.lru_order(2), [1, 3, 4, 0, 2]);
        // Using the top one changes nothing.
        c.get(2, 2);
        assert_eq!(c.lru_order(2), [1, 3, 4, 0, 2]);
    }

    #[test]
    fn bump_is_partial_in_a_long_list() {
        let mut c = BitmapCache::new();
        c.set_persistent(2, true);
        for i in 0..100 {
            c.put(2, i, bmp(0));
        }
        c.get(2, 0);
        let order = c.lru_order(2);
        assert_eq!(order.len(), 100);
        // Removed from the bottom, then 40 places up.
        assert_eq!(order.iter().position(|&i| i == 0), Some(40));
    }

    #[test]
    fn evicts_least_recently_used() {
        let mut c = BitmapCache::new();
        c.set_persistent(2, true);
        for i in 0..BMPCACHE2_C2_CELLS as u16 {
            assert_eq!(c.put(2, i, bmp(0)), None);
        }
        c.get(2, 0);
        assert_eq!(c.put(2, 1000, bmp(0)), Some(1));
        assert!(!c.contains(2, 1));
        assert!(c.contains(2, 0));
        assert_eq!(c.lru_order(2).len(), BMPCACHE2_C2_CELLS);
        // Back from disk later: it's put again, which evicts the next one.
        assert_eq!(c.put(2, 1, bmp(0)), Some(2));
    }

    #[test]
    fn rebuild_skips_empty_cells() {
        let mut c = BitmapCache::new();
        c.set_persistent(2, true);
        c.put(2, 7, bmp(0));
        c.put(2, 3, bmp(0));
        c.rebuild(2, &[3, 5, 7]);
        assert_eq!(c.lru_order(2), [3, 7]);
        c.rebuild(2, &[5]);
        assert!(c.lru_order(2).is_empty());
    }

    #[test]
    fn desk_cache_bounds() {
        let mut d = DeskCache::new();
        d.put(10, &[1, 2, 3]);
        assert_eq!(d.get(10, 3).unwrap(), [1, 2, 3]);
        assert!(d.get(DESK_CACHE_PIXELS as u32 - 1, 2).is_none());
        // Past the end means from the start.
        assert_eq!(d.get(u32::MAX, 1).unwrap(), [0]);
        assert!(DeskCache::fits(0, 640, 360));
        assert!(!DeskCache::fits(0, 1024, 768));
        assert!(!DeskCache::fits(0, -1, 5));
    }

    #[test]
    fn small_caches_ignore_bad_indices() {
        let mut g = GlyphCache::new();
        let glyph = || FontGlyph { offset: -1, baseline: 2, width: 1, height: 1, glyph: Glyph { width: 1, height: 1, stride: 1, bits: vec![0x80] } };
        g.put(11, 255, glyph());
        g.put(12, 0, glyph());
        g.put(0, 256, glyph());
        assert_eq!(g.get(11, 255).unwrap().offset, -1);
        assert!(g.get(12, 0).is_none());
        let mut b = BrushCache::new();
        b.put(63, BrushData { colour_code: 1, data: vec![0; 8] });
        b.put(64, BrushData { colour_code: 4, data: vec![] });
        assert!(b.get(1, 63).is_some());
        assert!(b.get(4, 63).is_none());
        let mut cur = CursorCache::new();
        cur.put(0x20, None);
        assert!(cur.get(0x1f).is_none());
    }
}
