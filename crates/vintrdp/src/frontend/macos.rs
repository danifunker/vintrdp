//! macOS front-end: AppKit via objc2.
//!
//! An `NSView` subclass draws the framebuffer with CoreGraphics and turns
//! key/mouse events into RDP input. Events from the rdesktop thread are
//! posted to the main queue with GCD. The view is also the window's delegate.

use std::cell::{Cell, OnceCell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBeep,
    NSButton, NSControlStateValueOff, NSControlStateValueOn, NSCursor, NSEvent, NSEventModifierFlags,
    NSGraphicsContext, NSGridView, NSImage, NSMenu, NSMenuItem, NSPopUpButton, NSResponder, NSScreen,
    NSSecureTextField, NSTextField, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{CFData, CFRetained, CGFloat};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGContext, CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGInterpolationQuality,
};
use objc2_foundation::{NSArray, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, ns_string};

use vintrdp_core::scancode::{self, Scancode};
use vintrdp_core::{Config, CursorShape, Event, Frontend, MouseButton, Session};

use crate::launch::{DEPTHS, Form};

pub fn run(config: Option<Config>) -> i32 {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    install_menu(&app, mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    let config = match config {
        Some(c) => c,
        None => match connect_dialog(mtm) {
            Some(c) => c,
            None => return 0,
        },
    };

    let view = RdpView::new(mtm, NSSize::new(config.width as f64, config.height as f64));
    let window = make_window(mtm, &view, &config);
    UI.with(|ui| ui.set(Ui { window, view: view.clone() }).ok());

    let screen = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame().size).unwrap_or(NSSize::new(1440.0, 900.0));
    let frontend = Arc::new(MacFrontend { screen: (screen.width as u32, screen.height as u32) });
    match Session::start(config, frontend) {
        Ok(session) => {
            let _ = view.ivars().session.set(session);
        }
        Err(e) => {
            alert(mtm, "Could not start the session", &e);
            return 1;
        }
    }

    app.run();
    0
}

// ---------------------------------------------------------------------------
// Main-thread state and event delivery

struct Ui {
    window: Retained<NSWindow>,
    view: Retained<RdpView>,
}

thread_local! {
    static UI: OnceCell<Ui> = const { OnceCell::new() };
}

fn with_ui(f: impl FnOnce(&Ui)) {
    UI.with(|ui| {
        if let Some(ui) = ui.get() {
            f(ui)
        }
    });
}

/// Set while a redraw is queued, so a burst of updates costs one dispatch.
static REDRAW_QUEUED: AtomicBool = AtomicBool::new(false);

struct MacFrontend {
    screen: (u32, u32),
}

impl Frontend for MacFrontend {
    fn notify(&self, event: Event) {
        if matches!(event, Event::Updated) && REDRAW_QUEUED.swap(true, Ordering::AcqRel) {
            return;
        }
        DispatchQueue::main().exec_async(move || handle_event(event));
    }

    fn screen_size(&self) -> (u32, u32) {
        self.screen
    }

    fn ask(&self, title: &str, message: &str, accept: &str, reject: &str) -> bool {
        let (tx, rx) = mpsc::channel();
        let (title, message, accept, reject) = (title.to_owned(), message.to_owned(), accept.to_owned(), reject.to_owned());
        DispatchQueue::main().exec_async(move || {
            let mtm = MainThreadMarker::new().unwrap();
            let a = NSAlert::new(mtm);
            a.setMessageText(&NSString::from_str(&title));
            a.setInformativeText(&NSString::from_str(&message));
            a.addButtonWithTitle(&NSString::from_str(&accept));
            a.addButtonWithTitle(&NSString::from_str(&reject));
            let _ = tx.send(a.runModal() == NSAlertFirstButtonReturn);
        });
        rx.recv().unwrap_or(false)
    }
}

fn handle_event(event: Event) {
    let mtm = MainThreadMarker::new().unwrap();
    with_ui(|ui| match event {
        Event::Resized { width, height } => {
            let size = NSSize::new(width as f64, height as f64);
            ui.window.setContentSize(size);
            ui.window.setContentAspectRatio(size);
            ui.window.center();
            ui.view.setNeedsDisplay(true);
        }
        Event::Updated => {
            REDRAW_QUEUED.store(false, Ordering::Release);
            ui.view.setNeedsDisplay(true);
        }
        Event::Cursor => {
            if let Some(session) = ui.view.ivars().session.get() {
                ui.view.ivars().cursor.replace(Some(make_cursor(&session.cursor())));
                ui.window.invalidateCursorRectsForView(&ui.view);
            }
        }
        Event::Title(t) => ui.window.setTitle(&NSString::from_str(&t)),
        Event::Message(m) => eprintln!("vintrdp: {m}"),
        Event::Bell => NSBeep(),
        Event::Ended(outcome) => {
            if !outcome.user_quit && outcome.code != 0 {
                alert(mtm, "The remote session ended", &outcome.message);
            }
            std::process::exit(outcome.code);
        }
    });
}

// ---------------------------------------------------------------------------
// The session view

#[derive(Default)]
struct ViewIvars {
    session: OnceCell<Session>,
    cursor: RefCell<Option<Retained<NSCursor>>>,
    /// Keys we've sent as pressed, released again when focus is lost.
    held: RefCell<Vec<Scancode>>,
    /// Fractional trackpad scrolling not yet sent as a wheel notch.
    wheel: Cell<f64>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements; RdpView has no Drop impl.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewIvars]
    struct RdpView;

    impl RdpView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            false
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            if let Some(cursor) = self.ivars().cursor.borrow().as_ref() {
                self.addCursorRect_cursor(self.bounds(), cursor);
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.press(event);
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            trace_key("up", event);
            if text_key(event).is_some() {
                return; // already sent as a complete keystroke on key-down
            }
            if let Some(sc) = scancode::from_mac_keycode(event.keyCode()) {
                self.send_key(sc, false);
            }
            self.sync_modifiers(event.modifierFlags());
        }

        /// Command-key combinations arrive here instead of keyDown:. Send
        /// them to the server (Command acts as Ctrl), except Cmd-Q.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            const KVK_Q: u16 = 0x0c;
            let cmd = event.modifierFlags().contains(NSEventModifierFlags::Command);
            let ours = cmd && event.keyCode() != KVK_Q;
            if ours {
                self.press(event);
            }
            ours
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            self.modifier_changed(event);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &NSEvent) {
            self.button(e, MouseButton::Left, true);
        }
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, e: &NSEvent) {
            self.button(e, MouseButton::Left, false);
        }
        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, e: &NSEvent) {
            self.button(e, MouseButton::Right, true);
        }
        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, e: &NSEvent) {
            self.button(e, MouseButton::Right, false);
        }
        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, e: &NSEvent) {
            if e.buttonNumber() == 2 {
                self.button(e, MouseButton::Middle, true);
            }
        }
        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, e: &NSEvent) {
            if e.buttonNumber() == 2 {
                self.button(e, MouseButton::Middle, false);
            }
        }
        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, e: &NSEvent) {
            self.motion(e);
        }
        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, e: &NSEvent) {
            self.motion(e);
        }
        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, e: &NSEvent) {
            self.motion(e);
        }
        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, e: &NSEvent) {
            self.motion(e);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, e: &NSEvent) {
            let Some((session, (x, y))) = self.session().zip(self.to_session(e)) else { return };
            // Trackpads report points; a wheel notch is roughly 10 of them.
            let delta = if e.hasPreciseScrollingDeltas() { e.scrollingDeltaY() / 10.0 } else { e.scrollingDeltaY() };
            let mut acc = self.ivars().wheel.get() + delta;
            while acc.abs() >= 1.0 {
                session.wheel(acc > 0.0, x, y);
                acc -= acc.signum();
            }
            self.ivars().wheel.set(acc);
        }
    }

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for RdpView {}

    // SAFETY: NSWindowDelegate has no safety requirements.
    unsafe impl NSWindowDelegate for RdpView {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            let code = self.session().map_or(0, |s| {
                s.quit();
                s.wait(Duration::from_secs(3)).map_or(0, |o| if o.user_quit { 0 } else { o.code })
            });
            std::process::exit(code);
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _n: &NSNotification) {
            // Don't leave keys stuck down on the server.
            let held: Vec<Scancode> = self.ivars().held.borrow_mut().drain(..).collect();
            if let Some(s) = self.session() {
                for (code, ext) in held {
                    s.key(code, ext, false);
                }
            }
        }

        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _n: &NSNotification) {
            self.setNeedsDisplay(true);
        }
    }
);

impl RdpView {
    fn new(mtm: MainThreadMarker, size: NSSize) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars::default());
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn session(&self) -> Option<&Session> {
        self.ivars().session.get()
    }

    /// Where the framebuffer is drawn inside the view, and its scale.
    fn fit(&self, fb_w: usize, fb_h: usize) -> (NSRect, f64) {
        let b = self.bounds();
        let scale = (b.size.width / fb_w as f64).min(b.size.height / fb_h as f64);
        let (w, h) = (fb_w as f64 * scale, fb_h as f64 * scale);
        let origin = NSPoint::new((b.size.width - w) / 2.0, (b.size.height - h) / 2.0);
        (NSRect::new(origin, NSSize::new(w, h)), scale)
    }

    fn draw(&self) {
        let Some(ctx) = NSGraphicsContext::currentContext() else { return };
        let cg = ctx.CGContext();
        CGContext::set_rgb_fill_color(Some(&cg), 0.0, 0.0, 0.0, 1.0);
        CGContext::fill_rect(Some(&cg), self.bounds());
        let Some(session) = self.session() else { return };
        session.with_framebuffer(|w, h, px| {
            let (dest, scale) = self.fit(w, h);
            // 0x00RRGGBB in a little-endian u32 is B,G,R,X in memory.
            // SAFETY: reinterpreting initialised u32s as bytes.
            let bytes = unsafe { std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), px.len() * 4) };
            let info = CGBitmapInfo(CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::NoneSkipFirst.0);
            if let Some(image) = make_cgimage(w, h, bytes, info) {
                let quality = if scale >= 1.0 { CGInterpolationQuality::None } else { CGInterpolationQuality::Default };
                CGContext::set_interpolation_quality(Some(&cg), quality);
                CGContext::draw_image(Some(&cg), dest, Some(&image));
            }
        });
    }

    /// The event's position in session pixels, if a framebuffer exists.
    fn to_session(&self, e: &NSEvent) -> Option<(u16, u16)> {
        let p = self.convertPoint_fromView(e.locationInWindow(), None);
        self.session()?.with_framebuffer(|w, h, _| {
            let (dest, scale) = self.fit(w, h);
            let x = ((p.x - dest.origin.x) / scale).clamp(0.0, w as f64 - 1.0);
            // The view isn't flipped: y grows upwards.
            let y = ((dest.origin.y + dest.size.height - p.y) / scale).clamp(0.0, h as f64 - 1.0);
            (x as u16, y as u16)
        })
    }

    fn motion(&self, e: &NSEvent) {
        if let Some((s, (x, y))) = self.session().zip(self.to_session(e)) {
            s.mouse_move(x, y);
        }
    }

    fn button(&self, e: &NSEvent, button: MouseButton, down: bool) {
        if let Some((s, (x, y))) = self.session().zip(self.to_session(e)) {
            s.mouse_button(button, down, x, y);
        }
    }

    fn press(&self, event: &NSEvent) {
        trace_key("down", event);
        self.sync_modifiers(event.modifierFlags());
        if let Some((sc, shift)) = text_key(event) {
            self.type_char(sc, shift);
            return;
        }
        if let Some(sc) = scancode::from_mac_keycode(event.keyCode()) {
            self.send_key(sc, true);
            // AppKit swallows key-up events while Command is held.
            if event.modifierFlags().contains(NSEventModifierFlags::Command) {
                self.send_key(sc, false);
            }
        }
    }

    /// Make the server's Shift/Ctrl/Alt state match the flags on a key
    /// event. Normally flagsChanged: has done this already; this catches
    /// events whose modifiers never produced one (e.g. synthesized input).
    fn sync_modifiers(&self, flags: NSEventModifierFlags) {
        let want = [
            (scancode::LSHIFT, scancode::RSHIFT, flags.contains(NSEventModifierFlags::Shift)),
            (scancode::LCTRL, scancode::RCTRL, flags.intersects(NSEventModifierFlags::Control | NSEventModifierFlags::Command)),
            (scancode::LALT, scancode::RALT, flags.contains(NSEventModifierFlags::Option)),
        ];
        for (left, right, down) in want {
            let held = self.ivars().held.borrow().iter().any(|&k| k == left || k == right);
            if down && !held {
                self.send_key(left, true);
            } else if !down && held {
                self.send_key(left, false);
                self.send_key(right, false);
            }
        }
    }

    /// Type one character-derived key, with Shift held exactly if needed.
    fn type_char(&self, sc: Scancode, shift: bool) {
        let shift_held = self.ivars().held.borrow().iter().any(|&k| k == scancode::LSHIFT || k == scancode::RSHIFT);
        if shift != shift_held {
            self.send_key(scancode::LSHIFT, shift);
            if !shift {
                self.send_key(scancode::RSHIFT, false);
            }
        }
        self.send_key(sc, true);
        self.send_key(sc, false);
        if shift != shift_held {
            self.send_key(scancode::LSHIFT, shift_held);
        }
    }

    fn send_key(&self, sc: Scancode, down: bool) {
        let Some(s) = self.session() else { return };
        let mut held = self.ivars().held.borrow_mut();
        held.retain(|&k| k != sc);
        if down {
            held.push(sc);
        }
        s.key(sc.0, sc.1, down);
    }

    /// Modifiers come as flagsChanged: with the key's code; the
    /// device-dependent bits say whether that particular key is now down.
    fn modifier_changed(&self, e: &NSEvent) {
        // NX_DEVICE*KEYMASK from IOKit's IOLLEvent.h
        const LCTL: usize = 0x0001;
        const LSHIFT: usize = 0x0002;
        const RSHIFT: usize = 0x0004;
        const LCMD: usize = 0x0008;
        const RCMD: usize = 0x0010;
        const LALT: usize = 0x0020;
        const RALT: usize = 0x0040;
        const RCTL: usize = 0x2000;

        let code = e.keyCode();
        let flags = e.modifierFlags().0;
        let Some(sc) = scancode::from_mac_keycode(code) else { return };
        if code == 0x39 {
            // Caps Lock reports its toggle state, not the key: tap it.
            self.send_key(sc, true);
            self.send_key(sc, false);
            return;
        }
        let mask = match code {
            0x38 => LSHIFT,
            0x3c => RSHIFT,
            0x3b => LCTL,
            0x3e => RCTL,
            0x3a => LALT,
            0x3d => RALT,
            0x37 => LCMD,
            0x36 => RCMD,
            _ => return,
        };
        self.send_key(sc, flags & mask != 0);
    }
}

// ---------------------------------------------------------------------------
// Window, menu, dialogs, cursors

fn make_window(mtm: MainThreadMarker, view: &RdpView, config: &Config) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(config.width as f64, config.height as f64));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    // SAFETY: standard initializer; we keep our own reference to the window,
    // so it must not release itself when closed.
    let window = unsafe {
        let w = NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), rect, style, NSBackingStoreType::Buffered, false);
        w.setReleasedWhenClosed(false);
        w
    };
    window.setTitle(&NSString::from_str(&config.window_title()));
    window.setContentView(Some(view));
    window.setDelegate(Some(ProtocolObject::from_ref(view)));
    window.setAcceptsMouseMovedEvents(true);
    window.makeFirstResponder(Some(view));
    window.center();
    window.makeKeyAndOrderFront(None);
    window
}

fn install_menu(app: &NSApplication, mtm: MainThreadMarker) {
    let menubar = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    menubar.addItem(&app_item);
    let app_menu = NSMenu::new(mtm);
    // Closing the window disconnects cleanly and quits (see windowWillClose:).
    // SAFETY: performClose: is an NSWindow action; a nil target reaches the key window.
    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("Disconnect and Quit"),
            Some(sel!(performClose:)),
            ns_string!("q"),
        )
    };
    app_menu.addItem(&quit);
    app_item.setSubmenu(Some(&app_menu));
    app.setMainMenu(Some(&menubar));
}

fn alert(mtm: MainThreadMarker, title: &str, message: &str) {
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(title));
    a.setInformativeText(&NSString::from_str(message));
    a.runModal();
}

/// The connection dialog: an alert with a grid of fields.
fn connect_dialog(mtm: MainThreadMarker) -> Option<Config> {
    let mut form = Form::default();
    loop {
        let text = |s: &str| NSTextField::textFieldWithString(&NSString::from_str(s), mtm);
        let label = |s: &str| NSTextField::labelWithString(&NSString::from_str(s), mtm);
        let computer = text(&form.computer);
        computer.setPlaceholderString(Some(ns_string!("host or host:port")));
        let user = text(&form.username);
        let password = NSSecureTextField::new(mtm);
        password.setStringValue(&NSString::from_str(&form.password));
        let domain = text(&form.domain);
        let width = text(&form.width);
        let height = text(&form.height);
        let depth = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        let titles: Vec<Retained<NSString>> = DEPTHS.iter().map(|d| NSString::from_str(&format!("{d} bit"))).collect();
        depth.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
        depth.selectItemAtIndex(form.depth_index as isize);
        // SAFETY: a checkbox with no target or action.
        let rdp4 = unsafe { NSButton::checkboxWithTitle_target_action(ns_string!("RDP 4 (NT 4.0 Terminal Server)"), None, None, mtm) };
        rdp4.setState(if form.rdp4 { NSControlStateValueOn } else { NSControlStateValueOff });

        let times = label("×");
        let size_cells: [&NSView; 3] = [&width, &times, &height];
        let size_row = NSGridView::gridViewWithViews(&NSArray::from_slice(&[&*NSArray::from_slice(&size_cells)]), mtm);

        let labels = ["Computer", "User name", "Password", "Domain", "Size", "Colours", "Protocol"].map(label);
        let fields: [&NSView; 7] = [&computer, &user, &password, &domain, &size_row, &depth, &rdp4];
        let rows: Vec<Retained<NSArray<NSView>>> =
            labels.iter().zip(fields).map(|(l, f)| NSArray::from_slice(&[&**l as &NSView, f])).collect();
        let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
        grid.columnAtIndex(1).setWidth(240.0);
        grid.setFrameSize(NSSize::new(340.0, 240.0));

        let a = NSAlert::new(mtm);
        a.setMessageText(ns_string!("Connect to a remote computer"));
        a.setInformativeText(ns_string!("RDP 4 and 5: Windows NT 4.0 Terminal Server through Windows Server 2003."));
        a.addButtonWithTitle(ns_string!("Connect"));
        a.addButtonWithTitle(ns_string!("Cancel"));
        a.setAccessoryView(Some(&grid));
        a.layout();
        a.window().setInitialFirstResponder(Some(&computer));
        if a.runModal() != NSAlertFirstButtonReturn {
            return None;
        }

        form = Form {
            computer: computer.stringValue().to_string(),
            username: user.stringValue().to_string(),
            password: password.stringValue().to_string(),
            domain: domain.stringValue().to_string(),
            width: width.stringValue().to_string(),
            height: height.stringValue().to_string(),
            depth_index: depth.indexOfSelectedItem().max(0) as usize,
            rdp4: rdp4.state() == NSControlStateValueOn,
        };
        match form.to_config() {
            Ok(c) => return Some(c),
            Err(e) => alert(mtm, "Check the connection settings", &e),
        }
    }
}

fn make_cgimage(w: usize, h: usize, bytes: &[u8], info: CGBitmapInfo) -> Option<CFRetained<CGImage>> {
    let data = CFData::from_bytes(bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data))?;
    let space = CGColorSpace::new_device_rgb()?;
    // SAFETY: bytes holds w*h 32-bit pixels laid out as `info` describes.
    unsafe {
        CGImage::new(
            w,
            h,
            8,
            32,
            w * 4,
            Some(&space),
            info,
            Some(&provider),
            std::ptr::null::<CGFloat>(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
}

fn make_cursor(shape: &CursorShape) -> Retained<NSCursor> {
    let rgba = CGBitmapInfo(CGImageAlphaInfo::Last.0);
    let image = match shape {
        CursorShape::Default => return NSCursor::arrowCursor(),
        CursorShape::Hidden => make_cgimage(1, 1, &[0; 4], rgba).map(|i| (i, 0, 0)),
        CursorShape::Image(c) => make_cgimage(c.width, c.height, &c.rgba, rgba).map(|i| (i, c.hot_x, c.hot_y)),
    };
    let Some((cg, hx, hy)) = image else { return NSCursor::arrowCursor() };
    let size = NSSize::new(CGImage::width(Some(&cg)) as f64, CGImage::height(Some(&cg)) as f64);
    let ns = NSImage::initWithCGImage_size(NSImage::alloc(), &cg, size);
    // NSCursor hot spots are measured from the image's top-left corner.
    NSCursor::initWithImage_hotSpot(NSCursor::alloc(), &ns, NSPoint::new(hx as f64, hy as f64))
}

/// Key events with no hardware key code. Remote-control apps (e.g. RustDesk's
/// phone keyboard) post text with key code 0, which is also the A key, so
/// every key would arrive as "a". For those, use the character instead.
fn text_key(e: &NSEvent) -> Option<(Scancode, bool)> {
    if e.keyCode() != 0 {
        return None;
    }
    let c = e.characters()?.to_string().chars().next()?;
    if c.eq_ignore_ascii_case(&'a') {
        return None; // a real A key
    }
    scancode::from_us_char(c)
}

/// With VINTRDP_TRACE_INPUT set, log what AppKit reports for each key.
fn trace_key(what: &str, e: &NSEvent) {
    if std::env::var_os("VINTRDP_TRACE_INPUT").is_none() {
        return;
    }
    let chars = e.characters().map(|s| s.to_string()).unwrap_or_default();
    eprintln!("key {what}: keyCode={:#04x} chars={chars:?} flags={:#x}", e.keyCode(), e.modifierFlags().0);
}
