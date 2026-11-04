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
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBeep,
    NSButton, NSControlStateValueOff, NSControlStateValueOn, NSCursor, NSEvent, NSEventModifierFlags,
    NSBorderType, NSGraphicsContext, NSGridCellPlacement, NSGridRowAlignment, NSGridView, NSImage, NSImageSymbolConfiguration, NSMenu, NSMenuItem, NSPopUpButton, NSResponder, NSScreen,
    NSScrollView, NSSecureTextField, NSSlider, NSStackView, NSImageView, NSColor, NSBezierPath, NSTextAlignment, NSLineBreakMode, NSTextFieldDelegate, NSImageScaling, NSModalResponseOK, NSOpenPanel, NSSegmentSwitchTracking, NSSegmentedControl, NSControlTextEditingDelegate, NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate,
    NSTextField, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{CFData, CFRetained, CGFloat};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGContext, CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGInterpolationQuality,
};
use objc2_foundation::{NSArray, NSIndexSet, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, ns_string};

use vintrdp_core::scancode::{self, Scancode};
use vintrdp_core::{Config, CursorShape, Event, Frontend, MouseButton, Session};

use crate::launch::{self, DEPTHS, Form, Protocol, SIZES, Saved, Store, ViewMode, remember, size_index, size_label};

pub fn run(config: Option<Config>) -> i32 {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    install_menu(&app, mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    match config {
        Some(c) => start_session(mtm, c, None),
        None => show_manager(mtm),
    }
    app.run();
    0
}

/// Sessions started from the connection manager go back to it when they end.
static BACK_TO_MANAGER: AtomicBool = AtomicBool::new(false);

/// Leave the app (after a session), reopening the connection manager if the
/// session came from it. rdesktop's core is one session per process, so the
/// manager comes back as a fresh process.
fn finish(code: i32) -> ! {
    if BACK_TO_MANAGER.load(Ordering::Relaxed) {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
    }
    std::process::exit(code);
}

/// The saved connection a session belongs to, for its tile thumbnail.
static THUMBNAIL_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Save the current screen as the connection's tile picture. `background`
/// encodes on another thread (used while the session runs).
fn save_thumbnail(view: &RdpView, background: bool) {
    let (Some(id), Some(session)) = (THUMBNAIL_ID.get(), view.ivars().session.get()) else { return };
    let Some((w, h, px)) = session.with_framebuffer(|w, h, px| (w, h, px.to_vec())) else { return };
    let work = move || {
        if let Err(e) = launch::save_thumbnail(id, w, h, &px) {
            eprintln!("vintrdp: could not save thumbnail: {e}");
        }
    };
    if background {
        std::thread::spawn(work);
    } else {
        work();
    }
}

/// `saved_id` names the saved connection (for its thumbnail), if any.
fn start_session(mtm: MainThreadMarker, config: Config, saved_id: Option<String>) {
    BACK_TO_MANAGER.store(saved_id.is_some(), Ordering::Relaxed);
    if let Some(id) = saved_id {
        let _ = THUMBNAIL_ID.set(id);
    }
    let view = RdpView::new(mtm, NSSize::new(config.width as f64, config.height as f64));
    let window = make_window(mtm, &view, &config);
    quit_closes(&window, "Disconnect and Quit");
    // Coming from the manager, its window has just closed: make sure the
    // session window is the key window so it gets the keyboard.
    NSApplication::sharedApplication(mtm).activate();
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&view));
    UI.with(|ui| ui.set(Ui { window, view: view.clone() }).ok());

    let screen = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame().size).unwrap_or(NSSize::new(1440.0, 900.0));
    let frontend = Arc::new(MacFrontend { screen: (screen.width as u32, screen.height as u32) });
    match Session::start(config, frontend) {
        Ok(session) => {
            let _ = view.ivars().session.set(session);
        }
        Err(e) => {
            alert(mtm, "Could not start the session", &e);
            finish(1);
        }
    }
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
            // Refresh the tile picture now and then, so a dropped connection
            // still leaves a recent one.
            thread_local!(static LAST: Cell<Option<std::time::Instant>> = const { Cell::new(None) });
            if LAST.get().is_none_or(|t| t.elapsed() >= Duration::from_secs(15)) {
                LAST.set(Some(std::time::Instant::now()));
                save_thumbnail(&ui.view, true);
            }
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
            finish(outcome.code);
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
            save_thumbnail(self, false);
            let code = self.session().map_or(0, |s| {
                s.quit();
                s.wait(Duration::from_secs(3)).map_or(0, |o| if o.user_quit { 0 } else { o.code })
            });
            finish(code);
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

thread_local! {
    /// The "Quit" menu item; aimed at whichever window is current.
    static QUIT_ITEM: OnceCell<Retained<NSMenuItem>> = const { OnceCell::new() };
}

/// Point Cmd-Q at `window` (closing it is how we quit cleanly).
fn quit_closes(window: &NSWindow, title: &str) {
    QUIT_ITEM.with(|q| {
        if let Some(item) = q.get() {
            item.setTitle(&NSString::from_str(title));
            // SAFETY: the window outlives the menu item's use of it (both live until exit).
            unsafe { item.setTarget(Some(window)) };
        }
    });
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
    QUIT_ITEM.with(|q| {
        let _ = q.set(quit);
    });
    app_item.setSubmenu(Some(&app_menu));
    app.setMainMenu(Some(&menubar));
}

fn alert(mtm: MainThreadMarker, title: &str, message: &str) {
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(title));
    a.setInformativeText(&NSString::from_str(message));
    a.runModal();
}

// ---------------------------------------------------------------------------
// Connection manager: saved connections on the left (tiles or a list),
// details on the right.

/// The editable fields for one connection.
struct Fields {
    name: Retained<NSTextField>,
    computer: Retained<NSTextField>,
    user: Retained<NSTextField>,
    password: Retained<NSSecureTextField>,
    save_password: Retained<NSButton>,
    domain: Retained<NSTextField>,
    size_slider: Retained<NSSlider>,
    size_label: Retained<NSTextField>,
    width: Retained<NSTextField>,
    height: Retained<NSTextField>,
    depth: Retained<NSPopUpButton>,
    protocol: Retained<NSPopUpButton>,
}

impl Fields {
    /// `owner` receives the slider, icon-button and size-field events.
    fn build(mtm: MainThreadMarker, owner: &Manager) -> (Fields, Retained<NSGridView>) {
        let text = || NSTextField::textFieldWithString(ns_string!(""), mtm);
        let label = |s: &str| NSTextField::labelWithString(&NSString::from_str(s), mtm);
        let popup = || NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        // SAFETY (all three): the owner lives until exit and implements the actions.
        let checkbox = |s: &str| unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str(s), None, None, mtm) };
        let button = |s: &str, action| unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(s), Some(owner), Some(action), mtm) };
        let size_slider = unsafe {
            NSSlider::sliderWithValue_minValue_maxValue_target_action(2.0, 0.0, (SIZES.len() - 1) as f64, Some(owner), Some(sel!(sizeSlider:)), mtm)
        };
        size_slider.setNumberOfTickMarks(SIZES.len() as isize);
        size_slider.setAllowsTickMarkValuesOnly(true);

        let f = Fields {
            name: text(),
            computer: text(),
            user: text(),
            password: NSSecureTextField::new(mtm),
            save_password: checkbox("Save password in Keychain"),
            domain: text(),
            size_slider,
            size_label: label(""),
            width: text(),
            height: text(),
            depth: popup(),
            protocol: popup(),
        };
        f.computer.setPlaceholderString(Some(ns_string!("host or host:port")));
        // SAFETY: the owner lives until exit and implements controlTextDidChange:.
        unsafe {
            f.width.setDelegate(Some(ProtocolObject::from_ref(owner)));
            f.height.setDelegate(Some(ProtocolObject::from_ref(owner)));
        }
        let depths: Vec<Retained<NSString>> = DEPTHS.iter().map(|d| NSString::from_str(&format!("{d} bit"))).collect();
        f.depth.addItemsWithTitles(&NSArray::from_retained_slice(&depths));
        let protocols: Vec<Retained<NSString>> = Protocol::ALL.iter().map(|p| NSString::from_str(p.label())).collect();
        f.protocol.addItemsWithTitles(&NSArray::from_retained_slice(&protocols));

        let choose = button("Choose Picture…", sel!(chooseIcon:));
        let reset = button("Use Last Screen", sel!(resetIcon:));
        let icon_cells: [&NSView; 2] = [&choose, &reset];
        let icon_row = NSGridView::gridViewWithViews(&NSArray::from_slice(&[&*NSArray::from_slice(&icon_cells)]), mtm);

        // "W [1024]  H [768]  XGA" on one line under the slider.
        for field in [&f.width, &f.height] {
            field.widthAnchor().constraintEqualToConstant(64.0).setActive(true);
        }
        let (w_label, h_label) = (label("W"), label("H"));
        let size_cells: [&NSView; 5] = [&w_label, &f.width, &h_label, &f.height, &f.size_label];
        let size_row = NSStackView::stackViewWithViews(&NSArray::from_slice(&size_cells), mtm);
        size_row.setSpacing(6.0);

        let labels =
            ["Name", "Computer", "User name", "Password", "", "Domain", "Size", "", "Colours", "Protocol", "Tile"].map(label);
        let cells: [&NSView; 11] = [
            &f.name, &f.computer, &f.user, &f.password, &f.save_password, &f.domain, &f.size_slider, &size_row, &f.depth,
            &f.protocol, &icon_row,
        ];
        let rows: Vec<Retained<NSArray<NSView>>> =
            labels.iter().zip(cells).map(|(l, c)| NSArray::from_slice(&[&**l as &NSView, c])).collect();
        let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
        grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
        grid.columnAtIndex(1).setWidth(270.0);
        grid.setRowAlignment(NSGridRowAlignment::FirstBaseline);
        grid.setRowSpacing(10.0);
        grid.setColumnSpacing(10.0);
        (f, grid)
    }

    fn load(&self, form: &Form) {
        let set = |f: &NSTextField, v: &str| f.setStringValue(&NSString::from_str(v));
        set(&self.name, &form.name);
        set(&self.computer, &form.computer);
        set(&self.user, &form.username);
        set(&self.password, &form.password);
        set(&self.domain, &form.domain);
        set(&self.width, &form.width);
        set(&self.height, &form.height);
        self.depth.selectItemAtIndex(form.depth_index as isize);
        let protocol = Protocol::ALL.iter().position(|&p| p == form.protocol).unwrap_or(0);
        self.protocol.selectItemAtIndex(protocol as isize);
        self.save_password.setState(if form.save_password { NSControlStateValueOn } else { NSControlStateValueOff });
        self.size_changed();
    }

    fn read(&self) -> Form {
        Form {
            name: self.name.stringValue().to_string(),
            computer: self.computer.stringValue().to_string(),
            username: self.user.stringValue().to_string(),
            password: self.password.stringValue().to_string(),
            domain: self.domain.stringValue().to_string(),
            width: self.width.stringValue().to_string(),
            height: self.height.stringValue().to_string(),
            depth_index: self.depth.indexOfSelectedItem().max(0) as usize,
            protocol: Protocol::ALL[self.protocol.indexOfSelectedItem().clamp(0, 2) as usize],
            save_password: self.save_password.state() == NSControlStateValueOn,
        }
    }

    /// Width/height were edited: move the slider to match, or say "Custom".
    fn size_changed(&self) {
        for field in [&self.width, &self.height] {
            let (text, clean) = (field.stringValue().to_string(), launch::clean_dimension(&field.stringValue().to_string()));
            if text != clean {
                field.setStringValue(&NSString::from_str(&clean));
            }
        }
        let (w, h) = (self.width.stringValue().to_string(), self.height.stringValue().to_string());
        if let Some(i) = size_index(&w, &h) {
            self.size_slider.setDoubleValue(i as f64);
        }
        self.size_label.setStringValue(&NSString::from_str(&size_label(&w, &h)));
    }

    /// The slider moved: fill in width and height.
    fn slider_changed(&self) {
        let i = (self.size_slider.doubleValue().round() as usize).min(SIZES.len() - 1);
        let (w, h, _) = SIZES[i];
        self.width.setStringValue(&NSString::from_str(&w.to_string()));
        self.height.setStringValue(&NSString::from_str(&h.to_string()));
        self.size_changed();
    }
}

// A document view that lays out top-down, for the tile grid.
define_class!(
    // SAFETY: NSView has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

/// One saved connection in the tile grid: a picture centred above its name,
/// with a rounded highlight when selected. Clicks go to the manager.
struct TileIvars {
    index: Cell<usize>,
    selected: Cell<bool>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TileIvars]
    struct TileView;

    impl TileView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// Take every click inside the tile ourselves, not the picture or label.
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let f = self.frame();
            let inside = point.x >= f.origin.x && point.x < f.origin.x + f.size.width && point.y >= f.origin.y && point.y < f.origin.y + f.size.height;
            if inside { (self as *const Self).cast_mut().cast() } else { std::ptr::null_mut() }
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let color = if self.ivars().selected.get() { NSColor::selectedContentBackgroundColor() } else { NSColor::quaternaryLabelColor() };
            color.setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), 8.0, 8.0).fill();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let (index, clicks) = (self.ivars().index.get(), event.clickCount());
            MANAGER.with(|m| {
                if let Some(m) = m.get() {
                    m.tile_clicked(index, clicks);
                }
            });
        }
    }
);

impl TileView {
    fn new(mtm: MainThreadMarker, frame: NSRect, index: usize, image: Option<&NSImage>, title: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TileIvars { index: Cell::new(index), selected: Cell::new(false) });
        // SAFETY: NSView's designated initializer.
        let tile: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let (w, h) = (frame.size.width, frame.size.height);
        if let Some(image) = image {
            let picture = NSImageView::imageViewWithImage(image, mtm);
            picture.setImageScaling(NSImageScaling::ScaleProportionallyDown);
            picture.setFrame(NSRect::new(NSPoint::new(8.0, 8.0), NSSize::new(w - 16.0, h - 40.0)));
            tile.addSubview(&picture);
        }
        let label = NSTextField::labelWithString(&NSString::from_str(title), mtm);
        label.setAlignment(NSTextAlignment::Center);
        label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        label.setFrame(NSRect::new(NSPoint::new(6.0, h - 28.0), NSSize::new(w - 12.0, 20.0)));
        tile.addSubview(&label);
        tile
    }

    fn set_selected(&self, selected: bool) {
        self.ivars().selected.set(selected);
        self.setNeedsDisplay(true);
    }
}

thread_local! {
    static MANAGER: OnceCell<Retained<Manager>> = const { OnceCell::new() };
}

const TILE_W: f64 = 132.0;
const TILE_H: f64 = 112.0;
const TILE_GAP: f64 = 10.0;

struct ManagerIvars {
    store: RefCell<Store>,
    /// The saved connection being edited; None before the first one exists.
    selected: RefCell<Option<String>>,
    fields: OnceCell<Fields>,
    window: OnceCell<Retained<NSWindow>>,
    table: OnceCell<Retained<NSTableView>>,
    list_scroll: OnceCell<Retained<NSScrollView>>,
    tile_scroll: OnceCell<Retained<NSScrollView>>,
    tile_area: OnceCell<Retained<FlippedView>>,
    tiles: RefCell<Vec<Retained<TileView>>>,
    /// Set while we change the table selection ourselves.
    syncing: Cell<bool>,
    connecting: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ManagerIvars]
    struct Manager;

    impl Manager {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            self.ivars().store.borrow().connections.len() as isize
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn object_value(&self, _table: &NSTableView, _column: Option<&NSTableColumn>, row: isize) -> Option<Retained<NSString>> {
            let store = self.ivars().store.borrow();
            store.connections.get(row as usize).map(|c| NSString::from_str(c.display_name()))
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_changed(&self, _n: &NSNotification) {
            if self.ivars().syncing.get() {
                return;
            }
            let row = self.table().selectedRow();
            let id = (row >= 0).then(|| self.ivars().store.borrow().connections.get(row as usize).map(|c| c.id.clone())).flatten();
            if let Some(id) = id {
                self.select(Some(id));
            }
        }

        #[unsafe(method(viewMode:))]
        fn view_mode(&self, sender: &NSSegmentedControl) {
            let mode = if sender.selectedSegment() == 1 { ViewMode::List } else { ViewMode::Tiles };
            self.ivars().store.borrow_mut().view = mode;
            self.save_store();
            self.show_mode();
        }

        #[unsafe(method(connect:))]
        fn connect(&self, _sender: Option<&AnyObject>) {
            self.do_connect();
        }

        #[unsafe(method(save:))]
        fn save(&self, _sender: Option<&AnyObject>) {
            self.do_save();
        }

        #[unsafe(method(newConnection:))]
        fn new_connection(&self, _sender: Option<&AnyObject>) {
            self.add_new();
        }

        #[unsafe(method(deleteConnection:))]
        fn delete_connection(&self, _sender: Option<&AnyObject>) {
            self.do_delete();
        }

        #[unsafe(method(chooseIcon:))]
        fn choose_icon(&self, _sender: Option<&AnyObject>) {
            self.do_choose_icon();
        }

        #[unsafe(method(resetIcon:))]
        fn reset_icon(&self, _sender: Option<&AnyObject>) {
            if let Some(id) = self.ivars().selected.borrow().clone() {
                let r = self.ivars().store.borrow_mut().clear_icon(&id);
                self.report(r);
                self.refresh();
            }
        }

        #[unsafe(method(sizeSlider:))]
        fn size_slider(&self, _sender: Option<&AnyObject>) {
            self.fields().slider_changed();
        }

        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _n: &NSNotification) {
            self.fields().size_changed();
        }
    }

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for Manager {}
    // SAFETY: the data source methods above have the protocol's signatures.
    unsafe impl NSTableViewDataSource for Manager {}
    // SAFETY: as above, for the delegate (and its text-editing parent).
    unsafe impl NSControlTextEditingDelegate for Manager {}
    // SAFETY: as above.
    unsafe impl NSTableViewDelegate for Manager {}
    // SAFETY: controlTextDidChange: above has the protocol's signature.
    unsafe impl NSTextFieldDelegate for Manager {}

    // SAFETY: NSWindowDelegate has no safety requirements.
    unsafe impl NSWindowDelegate for Manager {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            if !self.ivars().connecting.get() {
                std::process::exit(0);
            }
        }
    }
);

impl Manager {
    fn table(&self) -> &NSTableView {
        self.ivars().table.get().unwrap()
    }
    fn fields(&self) -> &Fields {
        self.ivars().fields.get().unwrap()
    }
    fn window(&self) -> &NSWindow {
        self.ivars().window.get().unwrap()
    }

    fn tile_clicked(&self, index: usize, clicks: isize) {
        let id = self.ivars().store.borrow().connections.get(index).map(|c| c.id.clone());
        self.select(id);
        if clicks >= 2 {
            self.do_connect();
        }
    }

    fn report(&self, r: std::io::Result<()>) {
        if let Err(e) = r {
            alert(self.mtm(), "Could not update saved connections", &e.to_string());
        }
    }

    fn save_store(&self) {
        let r = self.ivars().store.borrow().save();
        self.report(r);
    }

    /// Show `id` in the details pane and highlight it in both views.
    fn select(&self, id: Option<String>) {
        let saved = id.as_deref().and_then(|id| self.ivars().store.borrow().get(id).cloned());
        self.ivars().selected.replace(saved.as_ref().map(|s| s.id.clone()));
        self.fields().load(&saved.map(|s| s.to_form()).unwrap_or_default());
        self.sync_selection();
    }

    fn sync_selection(&self) {
        let selected = self.ivars().selected.borrow().clone();
        let row = selected.as_deref().and_then(|id| self.ivars().store.borrow().position(id));
        self.ivars().syncing.set(true);
        let rows = row.map_or_else(NSIndexSet::new, NSIndexSet::indexSetWithIndex);
        self.table().selectRowIndexes_byExtendingSelection(&rows, false);
        self.ivars().syncing.set(false);
        for (i, tile) in self.ivars().tiles.borrow().iter().enumerate() {
            tile.set_selected(Some(i) == row);
        }
    }

    /// Rebuild both views from the store.
    fn refresh(&self) {
        self.table().reloadData();
        self.rebuild_tiles();
        self.sync_selection();
    }

    fn rebuild_tiles(&self) {
        let mtm = self.mtm();
        let area = self.ivars().tile_area.get().unwrap();
        for old in self.ivars().tiles.borrow_mut().drain(..) {
            old.removeFromSuperview();
        }
        let width = self.ivars().tile_scroll.get().unwrap().contentSize().width;
        let columns = (((width - TILE_GAP) / (TILE_W + TILE_GAP)).floor() as usize).max(1);
        let store = self.ivars().store.borrow();
        let mut tiles = Vec::new();
        for (i, c) in store.connections.iter().enumerate() {
            let image = c
                .tile_image()
                .and_then(|p| NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&p.to_string_lossy())))
                .or_else(|| {
                    // Symbols size by point size, not setSize.
                    let symbol = NSImage::imageWithSystemSymbolName_accessibilityDescription(ns_string!("desktopcomputer"), None)?;
                    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(44.0, 0.0);
                    symbol.imageWithSymbolConfiguration(&config)
                });
            let (col, row) = (i % columns, i / columns);
            let origin = NSPoint::new(TILE_GAP + col as f64 * (TILE_W + TILE_GAP), TILE_GAP + row as f64 * (TILE_H + TILE_GAP));
            let frame = NSRect::new(origin, NSSize::new(TILE_W, TILE_H));
            let tile = TileView::new(mtm, frame, i, image.as_deref(), c.display_name());
            area.addSubview(&tile);
            tiles.push(tile);
        }
        let rows = store.connections.len().div_ceil(columns);
        let height = (TILE_GAP + rows as f64 * (TILE_H + TILE_GAP)).max(self.ivars().tile_scroll.get().unwrap().contentSize().height);
        area.setFrameSize(NSSize::new(width, height));
        *self.ivars().tiles.borrow_mut() = tiles;
    }

    fn show_mode(&self) {
        let tiles = self.ivars().store.borrow().view == ViewMode::Tiles;
        self.ivars().tile_scroll.get().unwrap().setHidden(!tiles);
        self.ivars().list_scroll.get().unwrap().setHidden(tiles);
    }

    /// "New": add a tile/row straight away and start editing it.
    fn add_new(&self) {
        let blank = Saved::blank();
        let id = blank.id.clone();
        let r = self.ivars().store.borrow_mut().upsert(blank);
        self.report(r);
        self.refresh();
        self.select(Some(id));
        self.window().makeFirstResponder(Some(&self.fields().name));
    }

    /// Validate and save the form. Returns the config to connect with.
    fn do_save(&self) -> Option<(Config, String)> {
        let form = self.fields().read();
        let config = match form.to_config() {
            Ok(c) => c,
            Err(e) => {
                alert(self.mtm(), "Check the connection settings", &e);
                return None;
            }
        };
        let id = self.ivars().selected.borrow().clone();
        let saved = remember(&mut self.ivars().store.borrow_mut(), &form, &config, id);
        self.ivars().selected.replace(Some(saved.id.clone()));
        self.refresh();
        Some((config, saved.id))
    }

    fn do_connect(&self) {
        let Some((config, id)) = self.do_save() else { return };
        self.ivars().connecting.set(true);
        self.window().close();
        start_session(self.mtm(), config, Some(id));
    }

    fn do_delete(&self) {
        let Some(id) = self.ivars().selected.borrow().clone() else { return };
        let shown = self.ivars().store.borrow().get(&id).map(|c| c.display_name().to_string()).unwrap_or_default();
        let a = NSAlert::new(self.mtm());
        a.setMessageText(&NSString::from_str(&format!("Delete \u{201c}{shown}\u{201d}?")));
        a.setInformativeText(ns_string!("Its settings, picture and saved password will be removed."));
        a.addButtonWithTitle(ns_string!("Delete"));
        a.addButtonWithTitle(ns_string!("Cancel"));
        if a.runModal() != NSAlertFirstButtonReturn {
            return;
        }
        let r = self.ivars().store.borrow_mut().remove(&id);
        self.report(r);
        let first = self.ivars().store.borrow().connections.first().map(|c| c.id.clone());
        self.refresh();
        self.select(first);
    }

    fn do_choose_icon(&self) {
        let Some(id) = self.ivars().selected.borrow().clone() else { return };
        let panel = NSOpenPanel::openPanel(self.mtm());
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        panel.setMessage(Some(ns_string!("Choose a picture for this connection's tile")));
        if panel.runModal() != NSModalResponseOK {
            return;
        }
        let Some(path) = panel.URL().and_then(|u| u.path()) else { return };
        let path = std::path::PathBuf::from(path.to_string());
        if NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&path.to_string_lossy())).is_none() {
            return alert(self.mtm(), "That file isn't a picture", &path.display().to_string());
        }
        let r = self.ivars().store.borrow_mut().set_icon(&id, &path);
        self.report(r);
        self.refresh();
    }
}

fn show_manager(mtm: MainThreadMarker) {
    const W: f64 = 820.0;
    const H: f64 = 620.0;
    const LEFT_W: f64 = 3.0 * (TILE_W + TILE_GAP) + TILE_GAP + 16.0;

    let store = Store::load();
    let view = store.view;
    let manager: Retained<Manager> = {
        let this = Manager::alloc(mtm).set_ivars(ManagerIvars {
            store: RefCell::new(store),
            selected: RefCell::new(None),
            fields: OnceCell::new(),
            window: OnceCell::new(),
            table: OnceCell::new(),
            list_scroll: OnceCell::new(),
            tile_scroll: OnceCell::new(),
            tile_area: OnceCell::new(),
            tiles: RefCell::new(Vec::new()),
            syncing: Cell::new(false),
            connecting: Cell::new(false),
        });
        // SAFETY: NSObject's init.
        unsafe { msg_send![super(this), init] }
    };

    let rect = |x: f64, y: f64, w: f64, h: f64| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
    // SAFETY: standard initializer; the manager keeps a reference.
    let window = unsafe {
        let w = NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), rect(0.0, 0.0, W, H), style, NSBackingStoreType::Buffered, false);
        w.setReleasedWhenClosed(false);
        w
    };
    window.setTitle(ns_string!("vintrdp"));
    let content = window.contentView().unwrap();
    let list_frame = rect(20.0, 60.0, LEFT_W, H - 112.0);

    // Tiles | List switch
    let labels = NSArray::from_retained_slice(&[NSString::from_str("Tiles"), NSString::from_str("List")]);
    // SAFETY: the manager (target) lives until exit and implements viewMode:.
    let switch = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(&labels, NSSegmentSwitchTracking::SelectOne, Some(&manager), Some(sel!(viewMode:)), mtm)
    };
    switch.setSelectedSegment(if view == ViewMode::List { 1 } else { 0 });
    switch.setFrame(rect(20.0, H - 40.0, 140.0, 24.0));
    content.addSubview(&switch);

    // Tiles
    let tile_scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), list_frame);
    tile_scroll.setHasVerticalScroller(true);
    tile_scroll.setBorderType(NSBorderType::BezelBorder);
    let tile_area = {
        let this = FlippedView::alloc(mtm);
        // SAFETY: NSView's designated initializer.
        let v: Retained<FlippedView> = unsafe { msg_send![this, initWithFrame: rect(0.0, 0.0, LEFT_W, H - 112.0)] };
        v
    };
    tile_scroll.setDocumentView(Some(&tile_area));
    content.addSubview(&tile_scroll);

    // List
    let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), ns_string!("name"));
    column.setTitle(ns_string!("Saved connections"));
    column.setWidth(LEFT_W - 20.0);
    column.setEditable(false);
    let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, LEFT_W, H - 112.0));
    table.addTableColumn(&column);
    table.setAllowsEmptySelection(true);
    // SAFETY: the manager outlives the table (both live until exit) and
    // implements the data source / delegate methods with the right signatures.
    unsafe {
        table.setDataSource(Some(ProtocolObject::from_ref(&*manager)));
        table.setDelegate(Some(ProtocolObject::from_ref(&*manager)));
        table.setTarget(Some(&manager));
        table.setDoubleAction(Some(sel!(connect:)));
    }
    let list_scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), list_frame);
    list_scroll.setDocumentView(Some(&table));
    list_scroll.setHasVerticalScroller(true);
    list_scroll.setBorderType(NSBorderType::BezelBorder);
    content.addSubview(&list_scroll);

    // SAFETY: buttons whose target (the manager) lives until exit.
    let button = |title: &str, action, x: f64, w: f64| unsafe {
        let b = NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(&manager), Some(action), mtm);
        b.setFrame(rect(x, 16.0, w, 32.0));
        content.addSubview(&b);
        b
    };
    button("New", sel!(newConnection:), 20.0, 80.0);
    button("Delete", sel!(deleteConnection:), 104.0, 80.0);
    button("Save", sel!(save:), W - 228.0, 100.0);
    let connect = button("Connect", sel!(connect:), W - 120.0, 100.0);
    connect.setKeyEquivalent(ns_string!("\r"));

    // Details
    let (fields, grid) = Fields::build(mtm, &manager);
    let size = grid.fittingSize();
    grid.setFrame(rect(W - 20.0 - size.width, H - 20.0 - size.height, size.width, size.height));
    content.addSubview(&grid);

    window.setDelegate(Some(ProtocolObject::from_ref(&*manager)));
    let _ = manager.ivars().fields.set(fields);
    let _ = manager.ivars().table.set(table);
    let _ = manager.ivars().list_scroll.set(list_scroll);
    let _ = manager.ivars().tile_scroll.set(tile_scroll);
    let _ = manager.ivars().tile_area.set(tile_area);
    let _ = manager.ivars().window.set(window.clone());

    manager.show_mode();
    manager.refresh();
    let first = manager.ivars().store.borrow().connections.first().map(|c| c.id.clone());
    match first {
        Some(id) => manager.select(Some(id)),
        None => manager.add_new(),
    }
    quit_closes(&window, "Quit vintrdp");
    window.center();
    window.makeKeyAndOrderFront(None);
    // Tiles find the manager here; this also keeps it alive.
    MANAGER.with(|m| {
        let _ = m.set(manager);
    });
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
