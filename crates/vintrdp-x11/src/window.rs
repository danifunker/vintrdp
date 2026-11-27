//! Windows: the tree, what of each is visible, exposures, and the window and
//! property requests. What a window uncovers is painted with its background
//! and the client gets Expose for it, but for backing store and save-unders
//! (backing.rs).

use std::collections::HashMap;
use std::rc::Rc;

use crate::cursor::Cursor;
use crate::draw::{depth_mask, Canvas, Fill, PixmapRef};
use crate::event::{self, Builder, Event};
use crate::region::Region;
use crate::server::*;
use crate::wire::{self, Reader};

pub const INPUT_OUTPUT: u16 = 1;
pub const INPUT_ONLY: u16 = 2;

#[derive(Clone)]
pub enum Background {
    None,
    ParentRelative,
    Pixel(u32),
    Pixmap(PixmapRef),
}

#[derive(Clone)]
pub enum Border {
    Pixel(u32),
    Pixmap(PixmapRef),
}

pub struct Property {
    pub name: u32,
    pub kind: u32,
    pub format: u8,
    /// Kept little-endian, converted for each client.
    pub data: Vec<u8>,
}

pub struct Window {
    pub id: u32,
    pub parent: u32,
    /// Bottom to top.
    pub children: Vec<u32>,
    /// The outer corner (the border's), relative to the parent's inside.
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub border_width: u16,
    pub class: u16,
    pub depth: u8,
    pub visual: u32,
    pub background: Background,
    pub border: Border,
    pub bit_gravity: u8,
    pub win_gravity: u8,
    pub backing_store: u8,
    pub backing_planes: u32,
    pub backing_pixel: u32,
    pub override_redirect: bool,
    pub save_under: bool,
    /// Each client's event mask, by slot.
    pub events: Vec<(u32, u32)>,
    pub do_not_propagate: u32,
    pub colormap: u32,
    pub cursor: Option<Rc<Cursor>>,
    pub mapped: bool,
    pub properties: Vec<Property>,
    /// SHAPE's bounding, clip and input shapes, relative to the inside's
    /// origin; None is the default (unshaped).
    pub shape: [Option<Region>; 3],
    /// Clients that selected ShapeNotify.
    pub shape_events: Vec<u32>,

    // Worked out by `validate`, in screen coordinates.
    pub viewable: bool,
    /// The inside's origin.
    pub ax: i32,
    pub ay: i32,
    /// What shows of the window itself: its inside, less children.
    pub clip: Region,
    /// What shows of it and its inferiors, border included.
    pub border_clip: Region,
    /// The visible part of the inside, inferiors included.
    pub inner: Region,
    /// VisibilityNotify's state: 0 unobscured, 1 partly, 2 fully; 3 not viewable.
    pub visibility: u8,
    /// Where it draws: the screen (0), or the pixmap of the window named,
    /// itself or its nearest ancestor with backing store. The regions above
    /// are then within that window, clipped by nothing outside it.
    pub buf: u32,
    /// With a pixmap of its own: what shows of it where its parent draws.
    pub shown: Region,
}

impl Window {
    pub fn new(id: u32, parent: u32, x: i16, y: i16, width: u16, height: u16, border_width: u16) -> Window {
        Window {
            id,
            parent,
            children: Vec::new(),
            x,
            y,
            width,
            height,
            border_width,
            class: INPUT_OUTPUT,
            depth: ROOT_DEPTH,
            visual: VISUAL,
            background: Background::None,
            border: Border::Pixel(0),
            bit_gravity: 0,
            win_gravity: 1,
            backing_store: 0,
            backing_planes: !0,
            backing_pixel: 0,
            override_redirect: false,
            save_under: false,
            events: Vec::new(),
            do_not_propagate: 0,
            colormap: COLORMAP,
            cursor: None,
            mapped: false,
            properties: Vec::new(),
            shape: [None, None, None],
            shape_events: Vec::new(),
            viewable: false,
            ax: 0,
            ay: 0,
            clip: Region::new(),
            border_clip: Region::new(),
            inner: Region::new(),
            visibility: 3,
            buf: 0,
            shown: Region::new(),
        }
    }

    pub fn mask_of(&self, slot: u32) -> u32 {
        self.events.iter().find(|e| e.0 == slot).map_or(0, |e| e.1)
    }

    pub fn all_masks(&self) -> u32 {
        self.events.iter().fold(0, |m, e| m | e.1)
    }

    pub fn input_only(&self) -> bool {
        self.class == INPUT_ONLY
    }

    /// The default bounding shape: the window and its border.
    pub fn default_bounding(&self) -> Region {
        let bw = self.border_width as i32;
        Region::rect(-bw, -bw, self.width as i32 + 2 * bw, self.height as i32 + 2 * bw)
    }

    pub fn default_clip(&self) -> Region {
        Region::rect(0, 0, self.width as i32, self.height as i32)
    }

    /// The effective bounding shape, relative to the inside's origin.
    pub fn bounding(&self) -> Region {
        match &self.shape[0] {
            Some(s) => s.intersect(&self.default_bounding()),
            None => self.default_bounding(),
        }
    }

    /// The effective clip shape: the inside, as shaped.
    pub fn clip_shape(&self) -> Region {
        let mut r = match &self.shape[1] {
            Some(s) => s.intersect(&self.default_clip()),
            None => self.default_clip(),
        };
        if self.shape[0].is_some() {
            r = r.intersect(&self.bounding());
        }
        r
    }

    /// Where the pointer counts as in the window, relative to the inside.
    pub fn input_shape(&self) -> Region {
        match &self.shape[2] {
            Some(s) => s.intersect(&self.bounding()),
            None => self.bounding(),
        }
    }
}

/// What a window was like before a change, for working out what to copy and
/// expose.
struct Before {
    viewable: bool,
    ax: i32,
    ay: i32,
    width: u16,
    height: u16,
    border_width: u16,
    clip: Region,
    border: Region,
    /// Its buffer's generation and origin (backing.rs).
    serial: u64,
    ox: i32,
    oy: i32,
    shown: Region,
    parent_serial: u64,
    /// Its parent, and place among its siblings.
    parent: u32,
    stack: usize,
}

pub struct Snapshot(HashMap<u32, Before>);

/// Where a corner goes when a window changes size: X.org's GravityTranslate.
/// (x, y) is the new position, (ox, oy) the old.
pub fn gravity_translate(x: i32, y: i32, ox: i32, oy: i32, dw: i32, dh: i32, gravity: u8) -> (i32, i32) {
    match gravity {
        2 => (x + dw / 2, y),
        3 => (x + dw, y),
        4 => (x, y + dh / 2),
        5 => (x + dw / 2, y + dh / 2),
        6 => (x + dw, y + dh / 2),
        7 => (x, y + dh),
        8 => (x + dw / 2, y + dh),
        9 => (x + dw, y + dh),
        10 => (ox, oy),
        _ => (x, y),
    }
}

impl State {
    pub fn win(&self, id: u32) -> Result<&Window, XError> {
        self.windows.get(&id).ok_or(XError(BAD_WINDOW, id))
    }

    pub fn win_mut(&mut self, id: u32) -> Result<&mut Window, XError> {
        self.windows.get_mut(&id).ok_or(XError(BAD_WINDOW, id))
    }

    /// The root window, with X's grey weave.
    pub fn create_root(&mut self) {
        const WEAVE: [u8; 4] = [0x88, 0x22, 0x44, 0x11];
        let mut tile = crate::draw::Pixmap::new(4, 4, self.depth);
        for y in 0..4 {
            for x in 0..4 {
                tile.px[y * 4 + x] = if WEAVE[y] >> x & 1 != 0 { self.white } else { self.black };
            }
        }
        let mut root = Window::new(ROOT, 0, 0, 0, self.width, self.height, 0);
        root.background = Background::Pixmap(Rc::new(std::cell::RefCell::new(tile)));
        root.mapped = true;
        root.depth = self.depth;
        self.windows.insert(ROOT, root);
        self.validate();
        let all = Region::rect(0, 0, self.width as i32, self.height as i32);
        self.paint_background(ROOT, &all);
    }

    pub fn is_ancestor(&self, ancestor: u32, mut w: u32) -> bool {
        while let Some(win) = self.windows.get(&w) {
            if win.parent == ancestor {
                return true;
            }
            w = win.parent;
        }
        false
    }

    /// The window and its inferiors, parents before children.
    pub fn subtree(&self, w: u32) -> Vec<u32> {
        let mut out = vec![w];
        let mut i = 0;
        while i < out.len() {
            if let Some(win) = self.windows.get(&out[i]) {
                out.extend(win.children.iter().copied());
            }
            i += 1;
        }
        out
    }

    /// Every window, parents first, children top to bottom: the order X.org
    /// sends VisibilityNotify and Expose in.
    fn top_down(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut stack = vec![ROOT];
        while let Some(w) = stack.pop() {
            out.push(w);
            if let Some(win) = self.windows.get(&w) {
                stack.extend(win.children.iter().copied());
            }
        }
        out
    }

    /// Work out every window's position and what shows of it, from the root.
    pub fn validate(&mut self) {
        let screen = Region::rect(0, 0, self.width as i32, self.height as i32);
        self.validate_window(ROOT, 0, 0, &screen, true, 0);
        let order = self.top_down();
        self.sync_backings(&order);
    }

    fn validate_window(&mut self, id: u32, px: i32, py: i32, avail: &Region, parent_viewable: bool, parent_buf: u32) {
        let Some(w) = self.windows.get(&id) else { return };
        let bw = w.border_width as i32;
        let (ax, ay) = (px + w.x as i32 + bw, py + w.y as i32 + bw);
        let viewable = parent_viewable && w.mapped;
        let (outer, inside) = if viewable {
            (w.bounding().translated(ax, ay), w.clip_shape().translated(ax, ay))
        } else {
            (Region::new(), Region::new())
        };
        // Backing store: within the window, nothing outside it clips.
        let redirect = self.backing_store && w.backing_store != 0 && viewable && !w.input_only() && id != ROOT;
        let shown = if redirect { outer.intersect(avail) } else { Region::new() };
        let buf = if redirect { id } else { parent_buf };
        let border_clip = if redirect { outer.clone() } else { outer.intersect(avail) };
        let inner = border_clip.intersect(&inside);
        let children = w.children.clone();
        let mut left = inner.clone();
        for &c in children.iter().rev() {
            self.validate_window(c, ax, ay, &left, viewable, buf);
            let cw = &self.windows[&c];
            if cw.viewable && !cw.input_only() {
                let bw = cw.border_width as i32;
                let outer = cw.bounding().translated(ax + cw.x as i32 + bw, ay + cw.y as i32 + bw);
                left = left.subtract(&outer);
            }
        }
        let w = self.windows.get_mut(&id).unwrap();
        w.viewable = viewable;
        (w.ax, w.ay) = (ax, ay);
        w.clip = if w.input_only() { Region::new() } else { left };
        w.border_clip = border_clip;
        w.inner = inner;
        w.buf = buf;
        w.shown = shown;
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut m = HashMap::new();
        for (id, w) in &self.windows {
            let border = w.border_clip.subtract(&w.clip_shape().translated(w.ax, w.ay));
            let (_, _, ox, oy, _) = self.buffer(w.buf);
            let parent_serial = self.windows.get(&w.parent).map_or(0, |p| self.serial_of(p.buf));
            let stack = self.windows.get(&w.parent).and_then(|p| p.children.iter().position(|c| c == id)).unwrap_or(0);
            m.insert(
                *id,
                Before {
                    viewable: w.viewable,
                    ax: w.ax,
                    ay: w.ay,
                    width: w.width,
                    height: w.height,
                    border_width: w.border_width,
                    clip: w.clip.clone(),
                    border,
                    serial: self.serial_of(w.buf),
                    ox,
                    oy,
                    shown: w.shown.clone(),
                    parent_serial,
                    parent: w.parent,
                    stack,
                },
            );
        }
        Snapshot(m)
    }

    /// After a change to the tree: validate, keep what can be kept (moved
    /// where its window went, by bit gravity for a window that changed size,
    /// from the pixmap it drew in), give back what save-unders kept, paint
    /// what's newly visible, and tell clients.
    pub fn exposures(&mut self, before: Snapshot) {
        self.validate();
        let order = self.top_down();
        if self.save_unders {
            // A window beneath a save-under that moved, changed or went: what
            // it had there is lost.
            let changed: Vec<u32> = before
                .0
                .iter()
                .filter(|(id, o)| match self.windows.get(id) {
                    None => true,
                    Some(w) => {
                        let stack = self.windows.get(&w.parent).and_then(|p| p.children.iter().position(|c| c == *id)).unwrap_or(0);
                        (w.viewable, w.ax, w.ay, w.width, w.height, w.border_width, w.parent, stack, w.buf)
                            != (o.viewable, o.ax, o.ay, o.width, o.height, o.border_width, o.parent, o.stack, if o.serial == 0 { 0 } else { w.buf })
                    }
                })
                .map(|(id, _)| *id)
                .collect();
            for id in changed {
                let all = Region::rect(0, 0, self.width as i32, self.height as i32);
                self.forget_saved(id, &all);
            }
            let clips = before.0.iter().map(|(id, o)| (*id, (o.viewable && o.serial == 0, o.clip.clone()))).collect();
            self.take_saves(&clips, &order);
        }
        let mut copies: Vec<(u64, u64, Region, i32, i32)> = Vec::new();
        let mut paint: Vec<(u32, Region, Region)> = Vec::new();
        let mut visibility: Vec<(u32, u8)> = Vec::new();
        let mut recomposite: Vec<(u32, Region)> = Vec::new();
        for &id in &order {
            let w = &self.windows[&id];
            let state = if !w.viewable || w.input_only() {
                3
            } else if w.inner.is_empty() {
                2
            } else if w.inner == w.clip_shape().translated(w.ax, w.ay) {
                0
            } else {
                1
            };
            if state != w.visibility {
                visibility.push((id, state));
            }
            if !w.viewable || w.input_only() {
                continue;
            }
            let serial = self.serial_of(w.buf);
            let (_, _, ox, oy, _) = self.buffer(w.buf);
            let old = before.0.get(&id).filter(|o| o.viewable);
            // A pixmap of its own: show what's newly shown of it.
            if w.buf == id {
                let parent_serial = self.windows.get(&w.parent).map_or(0, |p| self.serial_of(p.buf));
                let fresh = match old {
                    Some(o) if o.serial == serial && (o.ox, o.oy) == (ox, oy) && o.parent_serial == parent_serial => w.shown.subtract(&o.shown),
                    _ => w.shown.clone(),
                };
                recomposite.push((id, fresh));
            }
            let mut kept = Region::new();
            if let Some(o) = old {
                let (dw, dh) = (w.width as i32 - o.width as i32, w.height as i32 - o.height as i32);
                let content = if dw == 0 && dh == 0 {
                    Some((w.ax, w.ay))
                } else if w.bit_gravity == 0 {
                    None
                } else {
                    Some(gravity_translate(w.ax, w.ay, o.ax, o.ay, dw, dh, w.bit_gravity))
                };
                if let Some((nx, ny)) = content {
                    // In each buffer's own coordinates: where the content
                    // was, and where it goes.
                    let (tx, ty) = ((nx - ox) - (o.ax - o.ox), (ny - oy) - (o.ay - o.oy));
                    let kept_b = o.clip.translated(tx - o.ox, ty - o.oy).intersect(&w.clip.translated(-ox, -oy));
                    kept = kept_b.translated(ox, oy);
                    let src_origin = match self.buffer_origin_by_serial(o.serial) {
                        Some(p) => p,
                        None => {
                            kept = Region::new();
                            (0, 0)
                        }
                    };
                    let (dx, dy) = (ox + tx - src_origin.0, oy + ty - src_origin.1);
                    if !kept.is_empty() && (o.serial != serial || (dx, dy) != (0, 0)) {
                        copies.push((o.serial, serial, kept.clone(), dx, dy));
                    }
                }
            }
            let exposed = w.clip.subtract(&kept);
            let border = w.border_clip.subtract(&w.clip_shape().translated(w.ax, w.ay));
            let border = match old {
                Some(o) if (o.ax, o.ay, o.width, o.height, o.border_width, o.serial) == (w.ax, w.ay, w.width, w.height, w.border_width, serial) => border.subtract(&o.border),
                _ => border,
            };
            paint.push((id, exposed, border));
        }
        self.copy_between(&copies);
        for (id, state) in visibility {
            self.windows.get_mut(&id).unwrap().visibility = state;
            if state != 3 {
                let ev = Builder::new(event::VISIBILITY_NOTIFY, 0).u32(id).u8(state).done();
                self.deliver(id, event::VISIBILITY_CHANGE_MASK, &ev);
            }
        }
        for (id, mut exposed, mut border) in paint {
            if !self.saves.is_empty() && self.windows[&id].buf == 0 {
                border = self.restore_saved(id, &border);
                exposed = self.restore_saved(id, &exposed);
            }
            if !border.is_empty() {
                self.paint_border(id, &border);
            }
            if !exposed.is_empty() {
                self.paint_background(id, &exposed);
                self.send_exposes(id, &exposed);
            }
        }
        for (id, region) in recomposite {
            if let Some(b) = self.backings.get_mut(&id) {
                b.dirty.extend_from_slice(region.boxes());
            }
        }
        self.retired.clear();
        self.trim_saves();
        self.composite();
    }

    /// Expose events for a region in screen coordinates.
    pub fn send_exposes(&mut self, id: u32, region: &Region) {
        let w = &self.windows[&id];
        if w.all_masks() & event::EXPOSURE_MASK == 0 {
            return;
        }
        let (ax, ay) = (w.ax, w.ay);
        let boxes = region.boxes();
        let n = boxes.len();
        for (i, b) in boxes.iter().enumerate() {
            let ev = Builder::new(event::EXPOSE, 0)
                .u32(id)
                .u16((b.x1 - ax) as u16)
                .u16((b.y1 - ay) as u16)
                .u16(b.width() as u16)
                .u16(b.height() as u16)
                .u16((n - 1 - i).min(0xffff) as u16)
                .done();
            self.deliver(id, event::EXPOSURE_MASK, &ev);
        }
    }

    /// Paint a window's background over `region` (screen coordinates): its
    /// pixel or tile, or its parent's for ParentRelative; nothing for None.
    pub fn paint_background(&mut self, id: u32, region: &Region) {
        let buf = self.windows[&id].buf;
        let mut w = &self.windows[&id];
        while matches!(w.background, Background::ParentRelative) {
            match self.windows.get(&w.parent) {
                Some(p) => w = p,
                None => return,
            }
        }
        let fill = match &w.background {
            Background::None | Background::ParentRelative => return,
            Background::Pixel(p) => (Fill::Solid, *p),
            Background::Pixmap(p) => (Fill::tile(&p.borrow(), w.ax, w.ay), 0),
        };
        self.paint(buf, region, &fill.0, fill.1);
    }

    pub fn paint_border(&mut self, id: u32, region: &Region) {
        let w = &self.windows[&id];
        let fill = match &w.border {
            Border::Pixel(p) => (Fill::Solid, *p),
            Border::Pixmap(p) => (Fill::tile(&p.borrow(), w.ax, w.ay), 0),
        };
        self.paint(w.buf, region, &fill.0, fill.1);
    }

    /// Fill a region (screen coordinates) of a buffer, GXcopy; a tile's
    /// origin is in screen coordinates too.
    pub fn paint(&mut self, buf: u32, region: &Region, fill: &Fill, pixel: u32) {
        let mask = depth_mask(self.depth);
        let (px, stride, ox, oy) = self.buffer_mut(buf);
        let fill = match fill {
            Fill::Tiled { px, w, h, x, y } => Fill::Tiled { px: px.clone(), w: *w, h: *h, x: x - ox, y: y - oy },
            f => f.clone(),
        };
        let clip = region.translated(-ox, -oy);
        let mut c = Canvas { px, stride, dx: 0, dy: 0, clip: &clip, function: 3, plane_mask: mask, depth_mask: mask, fill: &fill, bg: 0, touched: None };
        let e = clip.extents();
        use crate::mi::Sink;
        c.fill_rects(pixel & mask, &[crate::mi::Rect { x: e.x1, y: e.y1, w: e.width(), h: e.height() }]);
        for b in region.boxes() {
            self.touch_buffer(buf, *b);
        }
    }

    // The requests.

    pub fn create_window(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let depth = ctx.data;
        let id = self.new_id(ctx, r.u32()?)?;
        let parent = r.u32()?;
        let (x, y, width, height, bw) = (r.u16()? as i16, r.u16()? as i16, r.u16()?, r.u16()?, r.u16()?);
        let (mut class, visual) = (r.u16()?, r.u32()?);
        let p = self.win(parent)?;
        if class == 0 {
            class = p.class;
        }
        if class > 2 {
            return Err(XError(BAD_VALUE, class as u32));
        }
        if width == 0 || height == 0 {
            return Err(XError(BAD_VALUE, 0));
        }
        if class == INPUT_OUTPUT && p.input_only() {
            return Err(XError(BAD_MATCH, 0));
        }
        let mut w = Window::new(id, parent, x, y, width, height, bw);
        w.class = class;
        if class == INPUT_ONLY {
            if bw != 0 || depth != 0 || (visual != 0 && visual != VISUAL) {
                return Err(XError(BAD_MATCH, 0));
            }
            w.depth = 0;
        } else {
            let depth = if depth == 0 { p.depth } else { depth };
            let visual = if visual == 0 { p.visual } else { visual };
            if depth != ROOT_DEPTH || visual != VISUAL {
                return Err(XError(BAD_MATCH, 0));
            }
            w.border = p.border.clone();
        }
        w.colormap = p.colormap;
        let values = self.window_values(ctx, &w, r, true)?;
        self.windows.insert(id, w);
        self.apply_window_values(ctx, id, values);
        self.windows.get_mut(&parent).unwrap().children.push(id);
        self.validate();
        let w = &self.windows[&id];
        let ev = Builder::new(event::CREATE_NOTIFY, 0).u32(parent).u32(id).i16(x).i16(y).u16(width).u16(height).u16(bw).bool(w.override_redirect).done();
        self.deliver(parent, event::SUBSTRUCTURE_NOTIFY_MASK, &ev);
        Ok(None)
    }

    pub fn change_window_attributes(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        let w = self.win(id)?;
        let values = self.window_values(ctx, w, r, false)?;
        // Backing store turned on or off: the window draws elsewhere now.
        let (was, viewable) = (w.backing_store != 0, w.viewable);
        let redirects = viewable && values.iter().any(|(bit, v, _)| *bit == 6 && (*v != 0) != was);
        let snap = redirects.then(|| self.snapshot());
        self.apply_window_values(ctx, id, values);
        if let Some(snap) = snap {
            self.exposures(snap);
        }
        Ok(None)
    }

    /// A window's value list, checked.
    fn window_values(&self, ctx: &Ctx, w: &Window, r: &mut Reader, creating: bool) -> Result<Vec<(u32, u32, Option<Value>)>, XError> {
        let mask = r.u32()?;
        if mask >> 15 != 0 {
            return Err(XError(BAD_VALUE, mask));
        }
        let mut out = Vec::new();
        for bit in (0..15).filter(|b| mask >> b & 1 != 0) {
            let v = r.u32()?;
            // InputOnly windows have no colours, borders or backing store.
            if w.input_only() && matches!(bit, 0..=4 | 6..=8 | 10 | 13) {
                return Err(XError(BAD_MATCH, 0));
            }
            let extra = match bit {
                0 if v > 1 => Some(Value::Pixmap(self.pixmap_of_depth(v, w.depth)?)),
                2 if v == 0 => {
                    let parent = self.win(w.parent)?;
                    if parent.depth != w.depth {
                        return Err(XError(BAD_MATCH, 0));
                    }
                    Some(Value::Border(parent.border.clone()))
                }
                2 => Some(Value::Pixmap(self.pixmap_of_depth(v, w.depth)?)),
                4 | 5 if v > 10 => return Err(XError(BAD_VALUE, v)),
                6 if v > 2 => return Err(XError(BAD_VALUE, v)),
                9 | 10 if v > 1 => return Err(XError(BAD_VALUE, v)),
                11 => {
                    if v & !event::ALL_EVENTS != 0 {
                        return Err(XError(BAD_VALUE, v));
                    }
                    // Only one client at a time may redirect, or get button presses.
                    const EXCLUSIVE: u32 = event::SUBSTRUCTURE_REDIRECT_MASK | event::RESIZE_REDIRECT_MASK | event::BUTTON_PRESS_MASK;
                    if w.events.iter().any(|&(s, m)| s != ctx.slot && m & v & EXCLUSIVE != 0) {
                        return Err(XError(BAD_ACCESS, 0));
                    }
                    None
                }
                12 if v & !event::DEVICE_EVENTS != 0 => return Err(XError(BAD_VALUE, v)),
                13 => {
                    let cmap = if v == 0 { self.win(w.parent)?.colormap } else { v };
                    if cmap != 0 && !self.is_colormap(cmap) {
                        return Err(XError(BAD_COLOR, v));
                    }
                    Some(Value::Colormap(cmap))
                }
                14 if v != 0 => Some(Value::Cursor(self.cursor_ref(v)?)),
                _ => None,
            };
            out.push((bit, v, extra));
        }
        let _ = creating;
        Ok(out)
    }

    fn apply_window_values(&mut self, ctx: &Ctx, id: u32, values: Vec<(u32, u32, Option<Value>)>) {
        let mut repaint_border = false;
        let mut cursor = false;
        for (bit, v, extra) in values {
            let w = self.windows.get_mut(&id).unwrap();
            match (bit, extra) {
                (0, Some(Value::Pixmap(p))) => w.background = Background::Pixmap(p),
                (0, _) => w.background = if v == 1 { Background::ParentRelative } else { Background::None },
                (1, _) => w.background = Background::Pixel(v & depth_mask(w.depth)),
                (2, Some(Value::Pixmap(p))) => {
                    w.border = Border::Pixmap(p);
                    repaint_border = true;
                }
                (2, Some(Value::Border(b))) => {
                    w.border = b;
                    repaint_border = true;
                }
                (3, _) => {
                    w.border = Border::Pixel(v & depth_mask(w.depth));
                    repaint_border = true;
                }
                (4, _) => w.bit_gravity = v as u8,
                (5, _) => w.win_gravity = v as u8,
                (6, _) => w.backing_store = v as u8,
                (7, _) => w.backing_planes = v,
                (8, _) => w.backing_pixel = v,
                (9, _) => w.override_redirect = v != 0,
                (10, _) => w.save_under = v != 0,
                (11, _) => {
                    w.events.retain(|e| e.0 != ctx.slot);
                    if v != 0 {
                        w.events.push((ctx.slot, v));
                    }
                }
                (12, _) => w.do_not_propagate = v,
                (13, Some(Value::Colormap(c))) => {
                    if w.colormap != c {
                        w.colormap = c;
                        let installed = self.installed == c;
                        let ev = Builder::new(event::COLORMAP_NOTIFY, 0).u32(id).u32(c).bool(true).bool(installed).done();
                        self.deliver(id, event::COLORMAP_CHANGE_MASK, &ev);
                    }
                }
                (14, Some(Value::Cursor(c))) => {
                    w.cursor = Some(c);
                    cursor = true;
                }
                (14, _) => {
                    w.cursor = None;
                    cursor = true;
                }
                _ => {}
            }
        }
        if repaint_border {
            let w = &self.windows[&id];
            if w.viewable {
                let border = w.border_clip.subtract(&w.clip_shape().translated(w.ax, w.ay));
                self.paint_border(id, &border);
            }
        }
        if cursor {
            self.update_cursor();
        }
    }

    pub fn get_window_attributes(&self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let w = self.win(r.u32()?)?;
        let state = if w.viewable { 2 } else if w.mapped { 1 } else { 0 };
        let visual = if w.input_only() { 0 } else { w.visual };
        let mut reply = ctx.reply(w.backing_store);
        reply.u32(visual).u16(w.class).u8(w.bit_gravity).u8(w.win_gravity);
        reply.u32(w.backing_planes).u32(w.backing_pixel).u8(w.save_under as u8);
        reply.u8((w.colormap != 0 && self.installed == w.colormap) as u8).u8(state).u8(w.override_redirect as u8);
        reply.u32(w.colormap).u32(w.all_masks()).u32(w.mask_of(ctx.slot)).u16(w.do_not_propagate as u16).zeros(2);
        Ok(Some(reply.finish()))
    }

    pub fn destroy_window(&mut self, id: u32) -> Result<(), XError> {
        self.win(id)?;
        if id == ROOT {
            return Ok(());
        }
        // Unmapped first, its exposures and all, as X.org does.
        if self.windows[&id].mapped {
            self.unmap_window(id)?;
        }
        let snap = self.snapshot();
        // Inferiors' DestroyNotify before their parents'.
        let tree = self.subtree(id);
        for &w in tree.iter().rev() {
            let parent = self.windows[&w].parent;
            let ev = Builder::new(event::DESTROY_NOTIFY, 0).u32(w).u32(w).done();
            self.structure_notify(w, parent, ev);
        }
        let parent = self.windows[&id].parent;
        self.windows_going(&tree, parent);
        self.windows.get_mut(&parent).unwrap().children.retain(|&c| c != id);
        for &w in &tree {
            self.windows.remove(&w);
            self.window_gone(w);
        }
        self.exposures(snap);
        self.pointer_moved();
        Ok(())
    }

    pub fn destroy_subwindows(&mut self, id: u32) -> Result<(), XError> {
        let children = self.win(id)?.children.clone();
        // Top to bottom.
        for c in children.into_iter().rev() {
            self.destroy_window(c)?;
        }
        Ok(())
    }

    /// Someone other than the client making the request redirects the
    /// parent's structure: whom to send MapRequest and the like.
    fn redirector(&self, id: u32, ctx: &Ctx) -> Option<u32> {
        let w = &self.windows[&id];
        if w.override_redirect {
            return None;
        }
        let p = self.windows.get(&w.parent)?;
        p.events.iter().find(|e| e.1 & event::SUBSTRUCTURE_REDIRECT_MASK != 0 && e.0 != ctx.slot).map(|e| e.0)
    }

    pub fn map_window(&mut self, ctx: &Ctx, id: u32) -> Result<(), XError> {
        let w = self.win(id)?;
        if w.mapped {
            return Ok(());
        }
        let parent = w.parent;
        if let Some(slot) = self.redirector(id, ctx) {
            let ev = Builder::new(event::MAP_REQUEST, 0).u32(parent).u32(id).done();
            self.send(slot, &ev);
            return Ok(());
        }
        let snap = self.snapshot();
        let w = self.windows.get_mut(&id).unwrap();
        w.mapped = true;
        let ev = Builder::new(event::MAP_NOTIFY, 0).u32(id).u32(id).bool(w.override_redirect).done();
        self.structure_notify(id, parent, ev);
        self.exposures(snap);
        self.pointer_moved();
        Ok(())
    }

    pub fn map_subwindows(&mut self, ctx: &Ctx, id: u32) -> Result<(), XError> {
        let children = self.win(id)?.children.clone();
        let snap = self.snapshot();
        let mut any = false;
        for c in children.into_iter().rev() {
            let w = &self.windows[&c];
            if w.mapped {
                continue;
            }
            if let Some(slot) = self.redirector(c, ctx) {
                let ev = Builder::new(event::MAP_REQUEST, 0).u32(id).u32(c).done();
                self.send(slot, &ev);
                continue;
            }
            let w = self.windows.get_mut(&c).unwrap();
            w.mapped = true;
            let ev = Builder::new(event::MAP_NOTIFY, 0).u32(c).u32(c).bool(w.override_redirect).done();
            self.structure_notify(c, id, ev);
            any = true;
        }
        if any {
            self.exposures(snap);
            self.pointer_moved();
        }
        Ok(())
    }

    /// Unmap, with UnmapNotify; the caller exposes what's uncovered.
    fn unmap(&mut self, id: u32, from_configure: bool) {
        let w = self.windows.get_mut(&id).unwrap();
        w.mapped = false;
        let parent = w.parent;
        let ev = Builder::new(event::UNMAP_NOTIFY, 0).u32(id).u32(id).bool(from_configure).done();
        self.structure_notify(id, parent, ev);
    }

    pub fn unmap_window(&mut self, id: u32) -> Result<(), XError> {
        let w = self.win(id)?;
        if !w.mapped || id == ROOT {
            return Ok(());
        }
        let snap = self.snapshot();
        self.unmap(id, false);
        self.exposures(snap);
        self.unviewable(id);
        Ok(())
    }

    pub fn unmap_subwindows(&mut self, id: u32) -> Result<(), XError> {
        let children = self.win(id)?.children.clone();
        let snap = self.snapshot();
        let mut any = false;
        for c in children {
            if self.windows[&c].mapped {
                self.unmap(c, false);
                any = true;
            }
        }
        if any {
            self.exposures(snap);
            self.unviewable(id);
        }
        Ok(())
    }

    /// Windows that stopped being viewable lose the focus, the pointer and
    /// grabs on them.
    fn unviewable(&mut self, _id: u32) {
        self.check_focus();
        self.check_grabs();
        self.pointer_moved();
    }

    /// Does `a` (above) cover part of `b`? Both siblings.
    fn occludes(&self, a: u32, b: u32) -> bool {
        let (wa, wb) = (&self.windows[&a], &self.windows[&b]);
        if !wa.mapped || !wb.mapped || wa.input_only() {
            return false;
        }
        let ra = wa.bounding().translated(wa.x as i32 + wa.border_width as i32, wa.y as i32 + wa.border_width as i32);
        let rb = wb.bounding().translated(wb.x as i32 + wb.border_width as i32, wb.y as i32 + wb.border_width as i32);
        !ra.intersect(&rb).is_empty()
    }

    /// Siblings above `id` that cover it, or below that it covers.
    fn covered_by_any(&self, id: u32, siblings: &[u32], above: bool) -> bool {
        let pos = siblings.iter().position(|&s| s == id).unwrap();
        if above {
            siblings[pos + 1..].iter().any(|&s| self.occludes(s, id))
        } else {
            siblings[..pos].iter().any(|&s| self.occludes(id, s))
        }
    }

    pub fn configure_window(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        let mask = r.u16()?;
        r.skip(2)?;
        let w = self.win(id)?;
        if mask >> 7 != 0 {
            return Err(XError(BAD_VALUE, mask as u32));
        }
        let (mut x, mut y, mut width, mut height, mut bw) = (w.x, w.y, w.width, w.height, w.border_width);
        let (mut sibling, mut stack) = (0u32, None::<u8>);
        for bit in (0..7).filter(|b| mask >> b & 1 != 0) {
            let v = r.u32()?;
            match bit {
                0 => x = v as i16,
                1 => y = v as i16,
                2 => width = v as u16,
                3 => height = v as u16,
                4 => bw = v as u16,
                5 => sibling = v,
                _ => {
                    if v > 4 {
                        return Err(XError(BAD_VALUE, v));
                    }
                    stack = Some(v as u8)
                }
            }
        }
        if mask & 0xc != 0 && (width == 0 || height == 0) {
            return Err(XError(BAD_VALUE, 0));
        }
        if mask & 0x10 != 0 && w.input_only() && bw != 0 {
            return Err(XError(BAD_MATCH, 0));
        }
        if mask & 0x20 != 0 {
            if stack.is_none() {
                return Err(XError(BAD_MATCH, id));
            }
            let s = self.win(sibling)?;
            if s.parent != w.parent || sibling == id {
                return Err(XError(BAD_MATCH, 0));
            }
        }
        if id == ROOT {
            return Ok(None);
        }
        let parent = w.parent;
        if let Some(slot) = self.redirector(id, ctx) {
            let ev = Builder::new(event::CONFIGURE_REQUEST, stack.unwrap_or(0))
                .u32(parent)
                .u32(id)
                .u32(sibling)
                .i16(x)
                .i16(y)
                .u16(width)
                .u16(height)
                .u16(bw)
                .u16(mask)
                .done();
            self.send(slot, &ev);
            return Ok(None);
        }
        // A size change someone else redirects: ask them, and change the rest.
        if (width, height) != (w.width, w.height) {
            if let Some(&(slot, _)) = w.events.iter().find(|e| e.1 & event::RESIZE_REDIRECT_MASK != 0 && e.0 != ctx.slot) {
                let ev = Builder::new(event::RESIZE_REQUEST, 0).u32(id).u16(width).u16(height).done();
                self.send(slot, &ev);
                (width, height) = (w.width, w.height);
            }
        }
        // Where it goes in the stack.
        let siblings = self.windows[&parent].children.clone();
        let mut order = siblings.clone();
        if let Some(mode) = stack {
            let pos = order.iter().position(|&s| s == id).unwrap();
            let raise = match (mode, sibling) {
                (0, _) => Some(true),
                (1, _) => Some(false),
                (2, 0) => self.covered_by_any(id, &siblings, true).then_some(true),
                (3, 0) => self.covered_by_any(id, &siblings, false).then_some(false),
                (4, 0) => {
                    if self.covered_by_any(id, &siblings, true) {
                        Some(true)
                    } else if self.covered_by_any(id, &siblings, false) {
                        Some(false)
                    } else {
                        None
                    }
                }
                (2, s) => (order.iter().position(|&o| o == s).unwrap() > pos && self.occludes(s, id)).then_some(true),
                (3, s) => (order.iter().position(|&o| o == s).unwrap() < pos && self.occludes(id, s)).then_some(false),
                (_, s) => {
                    let sp = order.iter().position(|&o| o == s).unwrap();
                    if sp > pos && self.occludes(s, id) {
                        Some(true)
                    } else if sp < pos && self.occludes(id, s) {
                        Some(false)
                    } else {
                        None
                    }
                }
            };
            if let Some(raise) = raise {
                order.remove(pos);
                let to = match (sibling, mode, raise) {
                    (0, _, true) => order.len(),
                    (0, _, false) => 0,
                    // Above or below the sibling itself.
                    (s, 0 | 1, _) => {
                        let sp = order.iter().position(|&o| o == s).unwrap();
                        if mode == 0 { sp + 1 } else { sp }
                    }
                    (_, _, true) => order.len(),
                    (_, _, false) => 0,
                };
                order.insert(to, id);
            }
        }
        let snap = self.snapshot();
        let old = (w.width, w.height, w.x as i32 + w.border_width as i32, w.y as i32 + w.border_width as i32);
        let w = self.windows.get_mut(&id).unwrap();
        (w.x, w.y, w.width, w.height, w.border_width) = (x, y, width, height, bw);
        self.windows.get_mut(&parent).unwrap().children = order.clone();
        let pos = order.iter().position(|&s| s == id).unwrap();
        let above = if pos == 0 { 0 } else { order[pos - 1] };
        let w = &self.windows[&id];
        let ev = Builder::new(event::CONFIGURE_NOTIFY, 0).u32(id).u32(id).u32(above).i16(x).i16(y).u16(width).u16(height).u16(bw).bool(w.override_redirect).done();
        self.structure_notify(id, parent, ev);
        if (width, height) != (old.0, old.1) {
            let (mx, my) = (x as i32 + bw as i32 - old.2, y as i32 + bw as i32 - old.3);
            self.gravitate_children(id, width as i32 - old.0 as i32, height as i32 - old.1 as i32, mx, my);
        }
        self.exposures(snap);
        self.check_focus();
        self.pointer_moved();
        Ok(None)
    }

    /// The parent changed size by (dw, dh) and its inside moved by (mx, my):
    /// move children by their window gravity, or unmap them (UnmapGravity).
    fn gravitate_children(&mut self, id: u32, dw: i32, dh: i32, mx: i32, my: i32) {
        let children = self.windows[&id].children.clone();
        for c in children {
            let w = &self.windows[&c];
            let (x, y) = (w.x as i32, w.y as i32);
            let (nx, ny) = match w.win_gravity {
                0 => {
                    if w.mapped {
                        self.unmap(c, true);
                    }
                    continue;
                }
                // Static: stays where it is on the screen.
                10 => (x - mx, y - my),
                g => gravity_translate(x, y, x, y, dw, dh, g),
            };
            if (nx, ny) == (x, y) {
                continue;
            }
            let w = self.windows.get_mut(&c).unwrap();
            (w.x, w.y) = (nx as i16, ny as i16);
            let ev = Builder::new(event::GRAVITY_NOTIFY, 0).u32(c).u32(c).i16(nx as i16).i16(ny as i16).done();
            self.structure_notify(c, id, ev);
        }
    }

    pub fn circulate_window(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let direction = ctx.data;
        let id = r.u32()?;
        self.win(id)?;
        if direction > 1 {
            return Err(XError(BAD_VALUE, direction as u32));
        }
        let children = self.windows[&id].children.clone();
        // RaiseLowest: the lowest child something covers; LowerHighest: the
        // highest that covers something.
        let pick = if direction == 0 {
            children.iter().copied().find(|&c| self.windows[&c].mapped && self.covered_by_any(c, &children, true))
        } else {
            children.iter().rev().copied().find(|&c| self.windows[&c].mapped && self.covered_by_any(c, &children, false))
        };
        let Some(c) = pick else { return Ok(None) };
        let place = direction; // PlaceOnTop or PlaceOnBottom
        if let Some(slot) = self.redirector(c, ctx) {
            let ev = Builder::new(event::CIRCULATE_REQUEST, 0).u32(id).u32(c).u32(0).u8(place).done();
            self.send(slot, &ev);
            return Ok(None);
        }
        let snap = self.snapshot();
        let kids = &mut self.windows.get_mut(&id).unwrap().children;
        kids.retain(|&k| k != c);
        if place == 0 {
            kids.push(c);
        } else {
            kids.insert(0, c);
        }
        let ev = Builder::new(event::CIRCULATE_NOTIFY, 0).u32(c).u32(c).u32(0).u8(place).done();
        self.structure_notify(c, id, ev);
        self.exposures(snap);
        self.pointer_moved();
        Ok(None)
    }

    pub fn reparent_window(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let (id, parent) = (r.u32()?, r.u32()?);
        let (x, y) = (r.u16()? as i16, r.u16()? as i16);
        let w = self.win(id)?;
        let p = self.win(parent)?;
        if id == ROOT || parent == id || self.is_ancestor(id, parent) {
            return Err(XError(BAD_MATCH, 0));
        }
        if p.input_only() && !w.input_only() {
            return Err(XError(BAD_MATCH, 0));
        }
        if matches!(w.background, Background::ParentRelative) && p.depth != w.depth {
            return Err(XError(BAD_MATCH, 0));
        }
        let was_mapped = w.mapped;
        let old_parent = w.parent;
        if was_mapped {
            self.unmap_window(id)?;
        }
        let snap = self.snapshot();
        self.windows.get_mut(&old_parent).unwrap().children.retain(|&c| c != id);
        self.windows.get_mut(&parent).unwrap().children.push(id);
        let w = self.windows.get_mut(&id).unwrap();
        (w.parent, w.x, w.y) = (parent, x, y);
        let ev = Builder::new(event::REPARENT_NOTIFY, 0).u32(id).u32(id).u32(parent).i16(x).i16(y).bool(w.override_redirect).done();
        let to = |s: &Self, win: u32, mask: u32| {
            let mut e = ev;
            e.set_u32(4, win);
            s.deliver(win, mask, &e);
        };
        to(self, id, event::STRUCTURE_NOTIFY_MASK);
        to(self, old_parent, event::SUBSTRUCTURE_NOTIFY_MASK);
        if old_parent != parent {
            to(self, parent, event::SUBSTRUCTURE_NOTIFY_MASK);
        }
        self.exposures(snap);
        if was_mapped {
            self.map_window(ctx, id)?;
        }
        self.check_focus();
        self.pointer_moved();
        Ok(None)
    }

    pub fn change_save_set(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        self.win(id)?;
        if ctx.data > 1 {
            return Err(XError(BAD_VALUE, ctx.data as u32));
        }
        if id >> ID_BITS == ctx.slot || id == ROOT {
            return Err(XError(BAD_MATCH, id));
        }
        let c = self.clients.get_mut(&ctx.slot).unwrap();
        c.save_set.retain(|&w| w != id);
        if ctx.data == 0 {
            c.save_set.push(id);
        }
        Ok(None)
    }

    pub fn translate_coordinates(&self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let (src, dst) = (self.win(r.u32()?)?, self.win(r.u32()?)?);
        let (x, y) = (r.u16()? as i16 as i32, r.u16()? as i16 as i32);
        let (sx, sy) = (src.ax + x, src.ay + y);
        let (dx, dy) = (sx - dst.ax, sy - dst.ay);
        let child = dst
            .children
            .iter()
            .rev()
            .copied()
            .find(|c| {
                let w = &self.windows[c];
                w.mapped && w.bounding().contains(dx - w.x as i32 - w.border_width as i32, dy - w.y as i32 - w.border_width as i32)
            })
            .unwrap_or(0);
        let mut reply = ctx.reply(1);
        reply.u32(child).i16(dx as i16).i16(dy as i16);
        Ok(Some(reply.finish()))
    }

    pub fn query_tree(&self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let w = self.win(r.u32()?)?;
        let mut reply = ctx.reply(0);
        reply.u32(ROOT).u32(w.parent).u16(w.children.len() as u16).zeros(14);
        for &c in &w.children {
            reply.u32(c);
        }
        Ok(Some(reply.finish()))
    }

    // Properties.

    fn property_notify(&self, id: u32, name: u32, deleted: bool) {
        let ev = Builder::new(event::PROPERTY_NOTIFY, 0).u32(id).u32(name).u32(self.now()).bool(deleted).done();
        self.deliver(id, event::PROPERTY_CHANGE_MASK, &ev);
    }

    pub fn change_property(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let mode = ctx.data;
        let id = r.u32()?;
        self.win(id)?;
        let (name, kind) = (r.u32()?, r.u32()?);
        let format = r.u8()?;
        r.skip(3)?;
        let units = r.u32()?;
        if !matches!(format, 8 | 16 | 32) {
            return Err(XError(BAD_VALUE, format as u32));
        }
        if mode > 2 {
            return Err(XError(BAD_VALUE, mode as u32));
        }
        self.atom(name)?;
        self.atom(kind)?;
        let len = (units as usize).checked_mul(format as usize / 8).ok_or(XError(BAD_LENGTH, 0))?;
        let data = wire::convert(r.bytes(len)?, format, ctx.order);
        let props = &mut self.windows.get_mut(&id).unwrap().properties;
        match props.iter_mut().find(|p| p.name == name) {
            Some(p) if mode != 0 => {
                if p.kind != kind || p.format != format {
                    return Err(XError(BAD_MATCH, 0));
                }
                if mode == 1 {
                    p.data.splice(0..0, data);
                } else {
                    p.data.extend_from_slice(&data);
                }
            }
            Some(p) => *p = Property { name, kind, format, data },
            None => props.push(Property { name, kind, format, data }),
        }
        self.property_notify(id, name, false);
        Ok(None)
    }

    pub fn delete_property(&mut self, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        self.win(id)?;
        let name = self.atom(r.u32()?)?;
        let props = &mut self.windows.get_mut(&id).unwrap().properties;
        let n = props.len();
        props.retain(|p| p.name != name);
        if props.len() != n {
            self.property_notify(id, name, true);
        }
        Ok(None)
    }

    pub fn get_property(&mut self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        self.win(id)?;
        let name = self.atom(r.u32()?)?;
        let kind = r.u32()?;
        if kind != 0 {
            self.atom(kind)?;
        }
        let (offset, length) = (r.u32()?, r.u32()?);
        let props = &self.windows[&id].properties;
        let Some(i) = props.iter().position(|p| p.name == name) else {
            let mut reply = ctx.reply(0);
            reply.u32(0).u32(0).u32(0);
            return Ok(Some(reply.finish()));
        };
        let p = &props[i];
        let mut reply = ctx.reply(p.format);
        if kind != 0 && kind != p.kind {
            reply.u32(p.kind).u32(p.data.len() as u32).u32(0);
            return Ok(Some(reply.finish()));
        }
        let n = p.data.len() as u64;
        let start = 4 * offset as u64;
        if start > n {
            return Err(XError(BAD_VALUE, offset));
        }
        let take = (n - start).min(4 * length as u64);
        let after = n - start - take;
        let (start, take) = (start as usize, take as usize);
        let value = wire::convert(&p.data[start..start + take], p.format, ctx.order);
        reply.u32(p.kind).u32(after as u32).u32((take / (p.format as usize / 8)) as u32).zeros(12).bytes(&value);
        if ctx.data != 0 && after == 0 {
            self.windows.get_mut(&id).unwrap().properties.remove(i);
            self.property_notify(id, name, true);
        }
        Ok(Some(reply.finish()))
    }

    pub fn list_properties(&self, ctx: &Ctx, r: &mut Reader) -> Answer {
        let w = self.win(r.u32()?)?;
        let mut reply = ctx.reply(0);
        reply.u16(w.properties.len() as u16).zeros(22);
        for p in &w.properties {
            reply.u32(p.name);
        }
        Ok(Some(reply.finish()))
    }

    pub fn rotate_properties(&mut self, r: &mut Reader) -> Answer {
        let id = r.u32()?;
        self.win(id)?;
        let (n, delta) = (r.u16()? as usize, r.u16()? as i16 as i64);
        let mut names = Vec::with_capacity(n);
        for _ in 0..n {
            let a = self.atom(r.u32()?)?;
            if names.contains(&a) {
                return Err(XError(BAD_MATCH, a));
            }
            names.push(a);
        }
        let props = &self.windows[&id].properties;
        let mut idx = Vec::with_capacity(n);
        for &a in &names {
            idx.push(props.iter().position(|p| p.name == a).ok_or(XError(BAD_MATCH, a))?);
        }
        if n == 0 || delta.rem_euclid(n as i64) == 0 {
            return Ok(None);
        }
        // The value of names[i] goes to names[i + delta].
        let props = &mut self.windows.get_mut(&id).unwrap().properties;
        let values: Vec<(u32, u8, Vec<u8>)> = idx.iter().map(|&i| (props[i].kind, props[i].format, std::mem::take(&mut props[i].data))).collect();
        for (i, v) in values.into_iter().enumerate() {
            let to = idx[(i as i64 + delta).rem_euclid(n as i64) as usize];
            (props[to].kind, props[to].format, props[to].data) = v;
        }
        for a in names {
            self.property_notify(id, a, false);
        }
        Ok(None)
    }

    /// A window went: everything that pointed at it.
    fn window_gone(&mut self, id: u32) {
        self.selections.retain(|s| s.window != id);
        for c in self.clients.values_mut() {
            c.save_set.retain(|&w| w != id);
        }
        self.passive.retain(|g| g.window != id && g.confine_to != id);
        self.window_destroyed_input(id);
    }
}

/// A window value that refers to another resource.
pub enum Value {
    Pixmap(PixmapRef),
    Border(Border),
    Colormap(u32),
    Cursor(Rc<Cursor>),
}

impl State {
    /// StructureNotify on the window and SubstructureNotify on its parent,
    /// the event's first field naming each.
    pub fn structure_notify(&self, win: u32, parent: u32, ev: Event) {
        let mut e = ev;
        e.set_u32(4, win);
        self.deliver(win, event::STRUCTURE_NOTIFY_MASK, &e);
        if self.windows.contains_key(&parent) {
            e.set_u32(4, parent);
            self.deliver(parent, event::SUBSTRUCTURE_NOTIFY_MASK, &e);
        }
    }
}
