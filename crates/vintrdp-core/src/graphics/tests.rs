//! Whole PDUs through [`Core`] into a framebuffer.

use super::*;
use crate::stream::Writer;

#[derive(Default)]
struct Recorder(Mutex<Vec<Event>>);

impl Frontend for Recorder {
    fn notify(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

impl Recorder {
    fn names(&self) -> Vec<&'static str> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|e| match e {
                Event::Resized { .. } => "resized",
                Event::Updated => "updated",
                Event::Cursor => "cursor",
                Event::Title(_) => "title",
                _ => "other",
            })
            .collect()
    }
}

struct Session {
    screen: Mutex<Screen>,
    fe: Recorder,
    config: Config,
    core: Core,
}

impl Session {
    fn new(depth: u32, width: u32, height: u32) -> Self {
        let config = Config::default();
        let mut p = Params::from_config(&config);
        (p.depth, p.width, p.height, p.persistent_cache) = (depth, width, height, false);
        let mut s = Session {
            screen: Mutex::new(Screen { surface: None, cursor: CursorShape::Default }),
            fe: Recorder::default(),
            config,
            core: Core::new(p.clone()),
        };
        let sink = Sink { screen: &s.screen, frontend: &s.fe, config: &s.config };
        s.core.activate(&p, &sink);
        s
    }

    fn call<R>(&mut self, f: impl FnOnce(&mut Core, &Sink) -> R) -> R {
        let sink = Sink { screen: &self.screen, frontend: &self.fe, config: &self.config };
        f(&mut self.core, &sink)
    }

    fn update(&mut self, kind: u16, bytes: &[u8]) -> Result<()> {
        let sink = Sink { screen: &self.screen, frontend: &self.fe, config: &self.config };
        self.core.update(kind, &mut Reader::new(bytes), &sink)
    }

    fn orders(&mut self, count: u16, bytes: &[u8]) -> Result<()> {
        let mut w = Writer::new();
        w.u16(0).u16(count).u16(0).bytes(bytes);
        self.update(UPDATE_ORDERS, w.as_slice())
    }

    fn px(&self, x: usize, y: usize) -> u32 {
        let s = lock(&self.screen);
        let s = s.surface.as_ref().unwrap();
        s.px[y * s.width + x]
    }

    fn count(&self, colour: u32) -> usize {
        lock(&self.screen).surface.as_ref().unwrap().px.iter().filter(|&&p| p == colour).count()
    }
}

/// A RECT order with every field: (x, y, cx, cy) and a 24 bpp colour.
fn rect_order(x: i16, y: i16, cx: i16, cy: i16, rgb: [u8; 3]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::RECT).u8(0x7f);
    w.i16(x).i16(y).i16(cx).i16(cy).bytes(&rgb);
    w.into_vec()
}

#[test]
fn activate_creates_and_resizes() {
    let mut s = Session::new(24, 8, 4);
    assert_eq!(s.fe.names(), ["title", "resized"]);
    let p = Params { width: 16, ..s.core.params.clone() };
    s.call(|c, k| c.activate(&p, k));
    assert_eq!(lock(&s.screen).surface.as_ref().unwrap().width, 16);
    // The same size again: nothing to say.
    s.call(|c, k| c.activate(&p, k));
    assert_eq!(s.fe.names(), ["title", "resized", "resized"]);
}

#[test]
fn rect_order_lands_in_framebuffer() {
    let mut s = Session::new(24, 8, 4);
    s.orders(1, &rect_order(1, 1, 2, 2, [0x11, 0x22, 0x33])).unwrap();
    // Order colours are red first.
    assert_eq!(s.px(1, 1), 0x112233);
    assert_eq!(s.px(2, 2), 0x112233);
    assert_eq!(s.px(3, 1), 0);
    assert_eq!(s.count(0x112233), 4);
    s.call(|c, k| c.flush(k));
    s.call(|c, k| c.flush(k));
    assert_eq!(s.fe.names(), ["title", "resized", "updated"]);
}

#[test]
fn delta_rect_reuses_state() {
    let mut s = Session::new(24, 8, 4);
    let mut bytes = rect_order(0, 0, 1, 1, [0xff, 0, 0]);
    // Same type, delta x +3, nothing else.
    bytes.extend([orders::STANDARD | orders::DELTA, 0x01, 3]);
    s.orders(2, &bytes).unwrap();
    assert_eq!(s.px(0, 0), 0xff0000);
    assert_eq!(s.px(3, 0), 0xff0000);
    assert_eq!(s.count(0xff0000), 2);
}

#[test]
fn bounds_clip_then_reset() {
    let mut s = Session::new(24, 8, 4);
    let mut w = Writer::new();
    // RECT over everything, clipped to (2,1)-(3,2).
    w.u8(orders::STANDARD | orders::CHANGE | orders::BOUNDS).u8(orders::RECT).u8(0x7f);
    w.u8(0x0f).i16(2).i16(1).i16(3).i16(2);
    w.i16(0).i16(0).i16(8).i16(4).bytes(&[0, 0xff, 0]);
    // Then an unclipped one-pixel RECT: the clip is gone.
    w.bytes(&rect_order(7, 3, 1, 1, [0, 0, 0xff]));
    // And one with the last bounds again, as a delta.
    w.u8(orders::STANDARD | orders::BOUNDS | orders::LASTBOUNDS).u8(0x7f).i16(0).i16(0).i16(8).i16(4).bytes(&[0xff; 3]);
    s.orders(3, w.as_slice()).unwrap();
    assert_eq!(s.count(0x00ff00), 0);
    assert_eq!(s.count(0xffffff), 4);
    assert_eq!(s.px(2, 1), 0xffffff);
    assert_eq!(s.px(7, 3), 0x0000ff);
    assert!(lock(&s.screen).surface.as_ref().unwrap().read(0, 0, 8, 4).iter().filter(|&&p| p == 0).count() == 8 * 4 - 5);
}

#[test]
fn uncompressed_bitmap_update() {
    let mut s = Session::new(24, 8, 4);
    let mut w = Writer::new();
    // One 2x2 rectangle at (3, 1), 24 bpp, rows bottom-up.
    w.u16(1).u16(3).u16(1).u16(4).u16(2).u16(2).u16(2).u16(24).u16(0).u16(12);
    w.bytes(&[0x01, 0x00, 0x00, 0x02, 0x00, 0x00]); // bottom: blue 1, blue 2
    w.bytes(&[0x00, 0x00, 0x03, 0x00, 0x00, 0x04]); // top: red 3, red 4
    s.update(UPDATE_BITMAP, w.as_slice()).unwrap();
    assert_eq!(s.px(3, 1), 0x030000);
    assert_eq!(s.px(4, 1), 0x040000);
    assert_eq!(s.px(3, 2), 0x000001);
    assert_eq!(s.px(4, 2), 0x000002);
}

#[test]
fn truncated_bitmap_update_is_an_error() {
    let mut s = Session::new(24, 8, 4);
    let mut w = Writer::new();
    w.u16(1).u16(0).u16(0).u16(65535).u16(65535).u16(65535).u16(65535).u16(32).u16(0).u16(0);
    assert!(s.update(UPDATE_BITMAP, w.as_slice()).is_err());
    let mut w = Writer::new();
    w.u16(1).u16(0).u16(0).u16(0).u16(0).u16(0).u16(1).u16(24).u16(0).u16(0);
    assert!(s.update(UPDATE_BITMAP, w.as_slice()).is_err());
}

#[test]
fn palette_and_8bpp_rect() {
    let mut s = Session::new(8, 4, 4);
    let mut w = Writer::new();
    w.u16(0).u16(2).u16(0).bytes(&[0, 0, 0, 0x12, 0x34, 0x56]);
    s.update(UPDATE_PALETTE, w.as_slice()).unwrap();
    s.orders(1, &rect_order(0, 0, 4, 4, [1, 0, 0])).unwrap();
    assert_eq!(s.count(0x123456), 16);
}

#[test]
fn memblt_from_raw_cache_and_screenblt() {
    let mut s = Session::new(24, 8, 4);
    let mut w = Writer::new();
    // Secondary: raw bitmap cache, id 1, idx 5, 2x1 at 24 bpp.
    let body = [1u8, 0, 2, 1, 24, 6, 0, 5, 0, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
    w.u8(orders::STANDARD | orders::SECONDARY).i16(body.len() as i16 - 7).u16(0).u8(0).bytes(&body);
    // MemBlt it at (1, 1), copy.
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::MEMBLT).u16(0x1ff);
    w.u8(1).u8(0).i16(1).i16(1).i16(2).i16(1).u8(0xcc).i16(0).i16(0).u16(5);
    // ScreenBlt it to (4, 2).
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::SCREENBLT).u8(0x7f);
    w.i16(4).i16(2).i16(2).i16(1).u8(0xcc).i16(1).i16(1);
    s.orders(3, w.as_slice()).unwrap();
    assert_eq!((s.px(1, 1), s.px(2, 1)), (0x302010, 0x605040));
    assert_eq!((s.px(4, 2), s.px(5, 2)), (0x302010, 0x605040));
    // A MemBlt from an empty cell draws nothing.
    let mut w = Writer::new();
    w.u8(orders::STANDARD).u16(0x100).u16(6);
    s.orders(1, w.as_slice()).unwrap();
}

#[test]
fn bitmap_cache_v2_uncompressed() {
    let mut s = Session::new(16, 8, 4);
    let mut w = Writer::new();
    // Cache 0, 2 bytes per pixel (mode 4), square 1x1, idx 0x0102 (long form).
    let flags: u16 = (4 << 3) | 0x80;
    let body = [1u8, 0x00, 0x02, 0x81, 0x02, 0x00, 0xf8];
    w.u8(orders::STANDARD | orders::SECONDARY).i16(body.len() as i16 - 7).u16(flags).u8(4).bytes(&body);
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::MEMBLT).u16(0x1ff);
    w.u8(0).u8(0).i16(0).i16(0).i16(1).i16(1).u8(0xcc).i16(0).i16(0).u16(0x102);
    s.orders(2, w.as_slice()).unwrap();
    assert_eq!(s.px(0, 0), 0xff0000);
}

#[test]
fn bad_secondary_order_is_skipped() {
    let mut s = Session::new(24, 8, 4);
    let mut w = Writer::new();
    // A glyph cache claiming more glyphs than it holds, then a RECT.
    let body = [0u8, 5, 0, 0];
    w.u8(orders::STANDARD | orders::SECONDARY).i16(body.len() as i16 - 7).u16(0).u8(3).bytes(&body);
    w.bytes(&rect_order(0, 0, 1, 1, [1, 2, 3]));
    s.orders(2, w.as_slice()).unwrap();
    assert_eq!(s.px(0, 0), 0x010203);
}

#[test]
fn glyphs_and_text_fragments() {
    let mut s = Session::new(24, 16, 8);
    let mut w = Writer::new();
    // Font 1: glyph 'A' (0x41) 2x2, all on, offset 0, baseline 0.
    let body = [1u8, 1, 0x41, 0, 0, 0, 0, 0, 2, 0, 2, 0, 0xc0, 0xc0, 0, 0];
    w.u8(orders::STANDARD | orders::SECONDARY).i16(body.len() as i16 - 7).u16(0).u8(3).bytes(&body);
    // Text2: font 1, explicit spacing, transparent, white on black, no box.
    // Text: A (+0), A (+3), then store both as fragment 7.
    let text = [0x41, 0, 0x41, 3, 0xff, 7, 4];
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::TEXT2).u8(0x3f).u8(0x00).u8(0x38);
    w.u8(1).u8(0).u8(0x0d).u8(0).bytes(&[0xff, 0xff, 0xff]).bytes(&[0, 0, 0]);
    w.i16(1).i16(1).u8(text.len() as u8).bytes(&text);
    // Replay fragment 7 at y = 5.
    let text = [0xfe, 7];
    w.u8(orders::STANDARD).u8(0x00).u8(0x00).u8(0x30);
    w.i16(5).u8(text.len() as u8).bytes(&text);
    s.orders(3, w.as_slice()).unwrap();
    assert_eq!(s.count(0xffffff), 16);
    for (x, y) in [(1, 1), (2, 2), (4, 1), (5, 2), (1, 5), (5, 6)] {
        assert_eq!(s.px(x, y), 0xffffff, "({x}, {y})");
    }
    assert_eq!(s.px(3, 1), 0);
}

#[test]
fn patblt_hatch_and_cached_brush() {
    let mut s = Session::new(24, 8, 8);
    let mut w = Writer::new();
    // Horizontal hatch, fg red, bg blue, opcode PATCOPY.
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::PATBLT).u16(0x0fff);
    w.i16(0).i16(0).i16(8).i16(8).u8(0xf0).bytes(&[0, 0, 0xff]).bytes(&[0xff, 0, 0]);
    w.u8(0).u8(0).u8(2).u8(0).bytes(&[0; 7]);
    s.orders(1, w.as_slice()).unwrap();
    assert_eq!(s.count(0xff0000), 8);
    assert_eq!(s.px(5, 3), 0xff0000);
    assert_eq!(s.px(5, 4), 0x0000ff);

    // A cached 1 bpp brush: top row set (sent bottom row first).
    let mut w = Writer::new();
    let body = [9u8, 1, 8, 8, 0x81, 8, 0, 0, 0, 0, 0, 0, 0, 0xff];
    w.u8(orders::STANDARD | orders::SECONDARY).i16(body.len() as i16 - 7).u16(0).u8(7).bytes(&body);
    // Brush style 0x81 (cached, colour code 1), index 9: set bits take bg.
    w.u8(orders::STANDARD).u16(0x0600).u8(0x81).u8(9);
    s.orders(2, w.as_slice()).unwrap();
    assert_eq!(s.count(0x0000ff), 8);
    assert_eq!(s.px(3, 0), 0x0000ff);
    assert_eq!(s.px(3, 1), 0xff0000);
}

#[test]
fn polyline_and_line() {
    let mut s = Session::new(24, 8, 8);
    let mut w = Writer::new();
    // Polyline from (0,0): +4 x, then +4 y. R2_COPYPEN is 13.
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::POLYLINE).u8(0x77);
    w.i16(0).i16(0).u8(13).bytes(&[0xff, 0xff, 0xff]).u8(2).u8(3).bytes(&[0x60, 4, 4]);
    s.orders(1, w.as_slice()).unwrap();
    // (0..4, 0) and (4, 0..4): the last point isn't drawn.
    assert_eq!(s.count(0xffffff), 8);
    assert_eq!(s.px(4, 3), 0xffffff);
    assert_eq!(s.px(4, 4), 0);
    let mut w = Writer::new();
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::LINE).u16(0x3fe);
    w.i16(0).i16(7).i16(3).i16(7).bytes(&[0; 3]).u8(13).u8(0).u8(1).bytes(&[0, 0xff, 0]);
    s.orders(1, w.as_slice()).unwrap();
    assert_eq!(s.count(0x00ff00), 3);
}

#[test]
fn desktop_save_and_restore() {
    let mut s = Session::new(24, 8, 4);
    s.orders(1, &rect_order(0, 0, 2, 2, [9, 9, 9])).unwrap();
    let mut w = Writer::new();
    // Save (0,0)-(1,1) at offset 100.
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::DESKSAVE).u8(0x3f);
    w.u32(100).i16(0).i16(0).i16(1).i16(1).u8(0);
    s.orders(1, w.as_slice()).unwrap();
    s.orders(1, &rect_order(0, 0, 8, 4, [0, 0, 0])).unwrap();
    // Restore it at (5,2).
    let mut w = Writer::new();
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::DESKSAVE).u8(0x3f).u32(100).i16(5).i16(2).i16(6).i16(3).u8(1);
    s.orders(1, w.as_slice()).unwrap();
    assert_eq!(s.count(0x090909), 4);
    assert_eq!(s.px(6, 3), 0x090909);
}

#[test]
fn unknown_order_stops_quietly() {
    let mut s = Session::new(24, 8, 4);
    let mut w = Writer::new();
    w.u8(orders::STANDARD | orders::CHANGE).u8(30).u8(0);
    w.bytes(&rect_order(0, 0, 1, 1, [1, 1, 1]));
    s.orders(2, w.as_slice()).unwrap();
    assert_eq!(s.px(0, 0), 0);
    // And a truncated order is an error, with the clip reset.
    let mut w = Writer::new();
    w.u8(orders::STANDARD | orders::CHANGE | orders::BOUNDS).u8(orders::RECT).u8(0x7f).u8(0x0f).i16(0).i16(0).i16(0).i16(0).u8(1);
    assert!(s.orders(1, w.as_slice()).is_err());
    s.orders(1, &rect_order(5, 2, 1, 1, [1, 1, 1])).unwrap();
    assert_eq!(s.px(5, 2), 0x010101);
}

#[test]
fn hostile_coordinates_dont_panic() {
    let mut s = Session::new(24, 8, 4);
    let mut w = Writer::new();
    // Raw cache bitmap 1x1, then MemBlt and ScreenBlt from far away.
    let body = [0u8, 0, 1, 1, 24, 3, 0, 0, 0, 1, 2, 3];
    w.u8(orders::STANDARD | orders::SECONDARY).i16(body.len() as i16 - 7).u16(0).u8(0).bytes(&body);
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::MEMBLT).u16(0x1ff);
    w.u8(0).u8(0).i16(-32768).i16(-32768).i16(32767).i16(32767).u8(0xcc).i16(-32768).i16(32767).u16(0);
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::SCREENBLT).u8(0x7f);
    w.i16(-5).i16(-5).i16(32767).i16(32767).u8(0xcc).i16(-32768).i16(32767);
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::DESKSAVE).u8(0x3f);
    w.u32(u32::MAX).i16(-32768).i16(-32768).i16(32767).i16(32767).u8(0);
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::ELLIPSE).u8(0x7f);
    w.i16(-32768).i16(-32768).i16(32767).i16(32767).u8(0).u8(1).bytes(&[1, 2, 3]);
    s.orders(4, w.as_slice()).unwrap();
}

#[test]
fn pointers() {
    let mut s = Session::new(24, 8, 4);
    let mut w = Writer::new();
    // Colour pointer, cache 3, 1x1 red, opaque.
    w.u16(0).u16(3).u16(0).u16(0).u16(1).u16(1).u16(2).u16(4).bytes(&[0, 0, 0xff, 0]).bytes(&[0, 0]);
    s.call(|c, k| c.pointer(POINTER_COLOR, &mut Reader::new(w.as_slice()), k)).unwrap();
    let CursorShape::Image(img) = lock(&s.screen).cursor.clone() else { panic!("no cursor") };
    assert_eq!(img.rgba, [0xff, 0, 0, 0xff]);
    // Hide it, then bring it back from the cache.
    s.call(|c, k| c.pointer(POINTER_SYSTEM, &mut Reader::new(&[0, 0, 0, 0, 0, 0]), k)).unwrap();
    assert!(matches!(lock(&s.screen).cursor, CursorShape::Hidden));
    s.call(|c, k| c.fastpath_update(FP_CACHED, &mut Reader::new(&[3, 0]), k)).unwrap();
    assert!(matches!(lock(&s.screen).cursor, CursorShape::Image(_)));
    // An empty slot hides it; the default comes back on request.
    s.call(|c, k| c.fastpath_update(FP_CACHED, &mut Reader::new(&[4, 0]), k)).unwrap();
    assert!(matches!(lock(&s.screen).cursor, CursorShape::Hidden));
    s.call(|c, k| c.fastpath_update(FP_PTR_DEFAULT, &mut Reader::new(&[]), k)).unwrap();
    assert!(matches!(lock(&s.screen).cursor, CursorShape::Default));
    // New pointer at 1 bpp, fast-path.
    let mut w = Writer::new();
    w.u16(1).u16(5).u16(0).u16(0).u16(1).u16(1).u16(2).u16(2).bytes(&[0x80, 0]).bytes(&[0, 0]);
    s.call(|c, k| c.fastpath_update(FP_POINTER, &mut Reader::new(w.as_slice()), k)).unwrap();
    let CursorShape::Image(img) = lock(&s.screen).cursor.clone() else { panic!("no cursor") };
    assert_eq!(img.rgba, [0xff, 0xff, 0xff, 0xff]);
    assert_eq!(s.fe.names().iter().filter(|&&n| n == "cursor").count(), 6);

    // With the local cursor, the server's shapes are ignored.
    s.core.params.local_cursor = true;
    s.call(|c, k| c.fastpath_update(FP_CACHED, &mut Reader::new(&[3, 0]), k)).unwrap();
    assert_eq!(s.fe.names().iter().filter(|&&n| n == "cursor").count(), 6);
}

#[test]
fn persistent_cache_round_trip() {
    let dir = std::env::temp_dir().join(format!("vintrdp-graphics-pst-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let new_session = || {
        let mut s = Session::new(16, 8, 4);
        s.core.pst.dir = Some(dir.clone());
        let p = Params { persistent_cache: true, bitmap_cache: true, cache_namespace: "t".into(), ..s.core.params.clone() };
        assert!(s.core.persistent_cache_ready(&p));
        s
    };
    let mut s = new_session();
    assert!(s.core.persistent_keys(2).is_empty());
    // A persistent uncompressed v2 bitmap: cache 2, idx 0, 1x1 green.
    let mut w = Writer::new();
    let flags: u16 = 2 | (4 << 3) | 0x80 | 0x100;
    let mut body = vec![7u8; 8];
    body.extend([1, 0x00, 0x02, 0x00, 0xe0, 0x07]);
    w.u8(orders::STANDARD | orders::SECONDARY).i16(body.len() as i16 - 7).u16(flags).u8(4).bytes(&body);
    s.orders(1, w.as_slice()).unwrap();
    s.core.save_state();
    drop(s);

    // Next session: the key is offered, the bitmap preloaded and drawable.
    let mut s = new_session();
    assert_eq!(s.core.persistent_keys(2), [[7u8; 8]]);
    // Only once per process.
    assert!(s.core.persistent_keys(2).is_empty());
    let mut w = Writer::new();
    w.u8(orders::STANDARD | orders::CHANGE).u8(orders::MEMBLT).u16(0x1ff);
    w.u8(2).u8(0).i16(2).i16(2).i16(1).i16(1).u8(0xcc).i16(0).i16(0).u16(0);
    s.orders(1, w.as_slice()).unwrap();
    assert_eq!(s.px(2, 2), 0x00ff00);
    let _ = std::fs::remove_dir_all(&dir);
}
