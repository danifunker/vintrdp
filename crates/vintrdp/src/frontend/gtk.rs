//! Linux and BSD front-end: GTK 4.
//!
//! Plain `gtk::init` and a `glib::MainLoop` rather than `gtk::Application`,
//! which would claim our command line and allow only one instance. Events from
//! the rdesktop thread reach the main loop through `glib::idle_add_once`. The
//! connection manager mirrors the macOS one: tiles on the left, details on the
//! right.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib};
use gtk4 as gtk;

use vintrdp_core::scancode::{self, Scancode};
use vintrdp_core::{Config, CursorShape, Event, Frontend, MouseButton, Session};

use crate::launch::{self, DEPTHS, Form, MONITORS, Picture, ServerKind, SIZES, Store, TileImage, remember, size_index, size_label};

pub fn run(config: Option<Config>) -> i32 {
    glib::set_prgname(Some("vintrdp"));
    glib::set_application_name("vintrdp");
    if let Err(e) = gtk::init() {
        eprintln!("vintrdp: could not open a display: {e}");
        return 1;
    }
    gtk::Window::set_default_icon_name("vintrdp");
    install_css();
    match config {
        Some(c) => start_session(c, None),
        None => show_manager(),
    }
    glib::MainLoop::new(None, false).run();
    0
}

fn install_css() {
    let css = gtk::CssProvider::new();
    css.load_from_string(
        "flowboxchild.tile { border-radius: 10px; padding: 6px; background: alpha(currentColor, 0.08); }
         flowboxchild.tile:selected { background: @theme_selected_bg_color; color: @theme_selected_fg_color; }",
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

fn alert(parent: Option<&gtk::Window>, title: &str, message: &str) {
    gtk::AlertDialog::builder().modal(true).message(title).detail(message).build().show(parent);
}

fn texture(bytes: &'static [u8]) -> Option<gdk::Texture> {
    gdk::Texture::from_bytes(&glib::Bytes::from_static(bytes)).ok()
}

fn show_about(parent: &gtk::Window) {
    let about = gtk::AboutDialog::builder()
        .transient_for(parent)
        .modal(true)
        .program_name("vintrdp")
        .version(launch::VERSION)
        .comments(launch::DESCRIPTION)
        .website(launch::REPO_URL)
        .website_label("github.com/danifunker/vintrdp")
        .copyright(launch::FORK_CREDIT)
        .license_type(gtk::License::Gpl30)
        .build();
    if let Some(logo) = texture(launch::APP_ICON) {
        about.set_logo(Some(&logo));
    }
    about.present();
}

// ---------------------------------------------------------------------------
// The session window

/// Set while a redraw is queued, so a burst of updates costs one idle call.
static REDRAW_QUEUED: AtomicBool = AtomicBool::new(false);
/// Sessions started from the manager go back to it when they end.
static BACK_TO_MANAGER: AtomicBool = AtomicBool::new(false);
/// Set when the window closes, so a pending question can't block the exit.
static CLOSING: AtomicBool = AtomicBool::new(false);
/// The saved connection a session belongs to, for its tile thumbnail.
static THUMBNAIL_ID: OnceLock<String> = OnceLock::new();

struct Ui {
    window: gtk::Window,
    area: gtk::DrawingArea,
    session: OnceCell<Session>,
    /// The framebuffer as a cairo surface, reused between frames.
    surface: RefCell<Option<cairo::ImageSurface>>,
    /// Keys we've sent as pressed, released again when focus is lost.
    held: RefCell<Vec<Scancode>>,
    /// The last pointer position, in session pixels.
    pointer: Cell<(u16, u16)>,
    last_thumbnail: Cell<Option<Instant>>,
}

thread_local! {
    static UI: OnceCell<Rc<Ui>> = const { OnceCell::new() };
}

fn with_ui(f: impl FnOnce(&Rc<Ui>)) {
    UI.with(|ui| {
        if let Some(ui) = ui.get() {
            f(ui)
        }
    });
}

fn finish(code: i32) -> ! {
    if BACK_TO_MANAGER.load(Ordering::Relaxed) {
        launch::relaunch();
    }
    std::process::exit(code);
}

/// Put files on the local clipboard the way file managers paste them: GTK's
/// file list (also offered as text/uri-list) and GNOME's copied-files form.
fn set_clipboard_files(clipboard: &gdk::Clipboard, paths: &[std::path::PathBuf]) {
    let files: Vec<gio::File> = paths.iter().map(gio::File::for_path).collect();
    let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
    let gnome = glib::Bytes::from_owned(format!("copy\n{}", uris.join("\n")).into_bytes());
    let list = gdk::FileList::from_array(&files);
    let provider = gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_value(&list.to_value()),
        gdk::ContentProvider::for_bytes("x-special/gnome-copied-files", &gnome),
    ]);
    if let Err(e) = clipboard.set_content(Some(&provider)) {
        eprintln!("vintrdp: could not put files on the clipboard: {e}");
    }
}

fn save_thumbnail(ui: &Ui, background: bool) {
    let (Some(id), Some(session)) = (THUMBNAIL_ID.get(), ui.session.get()) else { return };
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

struct GtkFrontend {
    screen: (u32, u32),
}

impl Frontend for GtkFrontend {
    fn notify(&self, event: Event) {
        if matches!(event, Event::Updated) && REDRAW_QUEUED.swap(true, Ordering::AcqRel) {
            return;
        }
        glib::idle_add_once(move || handle_event(event));
    }

    fn screen_size(&self) -> (u32, u32) {
        self.screen
    }

    fn ask(&self, title: &str, message: &str, accept: &str, reject: &str) -> bool {
        let (tx, rx) = mpsc::channel();
        let (title, message, accept, reject) = (title.to_owned(), message.to_owned(), accept.to_owned(), reject.to_owned());
        glib::idle_add_once(move || {
            let dialog = gtk::AlertDialog::builder()
                .modal(true)
                .message(title)
                .detail(message)
                .buttons([accept, reject])
                .default_button(1)
                .cancel_button(1)
                .build();
            let mut parent = None;
            with_ui(|ui| parent = Some(ui.window.clone()));
            dialog.choose(parent.as_ref(), None::<&gio::Cancellable>, move |answer| {
                let _ = tx.send(answer == Ok(0));
            });
        });
        // Don't hang the rdesktop thread if the window closes meanwhile.
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(answer) => return answer,
                Err(mpsc::RecvTimeoutError::Disconnected) => return false,
                Err(mpsc::RecvTimeoutError::Timeout) if CLOSING.load(Ordering::Relaxed) => return false,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

fn handle_event(event: Event) {
    with_ui(|ui| match event {
        Event::Resized { width, height } => {
            ui.area.set_content_width(width as i32);
            ui.area.set_content_height(height as i32);
            ui.window.set_default_size(width as i32, height as i32);
            ui.area.queue_draw();
        }
        Event::Updated => {
            REDRAW_QUEUED.store(false, Ordering::Release);
            ui.area.queue_draw();
            // Refresh the tile picture now and then, so a dropped connection
            // still leaves a recent one.
            if ui.last_thumbnail.get().is_none_or(|t| t.elapsed() >= Duration::from_secs(15)) {
                ui.last_thumbnail.set(Some(Instant::now()));
                save_thumbnail(ui, true);
            }
        }
        Event::Cursor => {
            if let Some(session) = ui.session.get() {
                ui.area.set_cursor(make_cursor(&session.cursor()).as_ref());
            }
        }
        Event::Title(t) => ui.window.set_title(Some(&t)),
        Event::Message(m) => eprintln!("vintrdp: {m}"),
        Event::Bell => {
            if let Some(display) = gdk::Display::default() {
                display.beep();
            }
        }
        Event::Clipboard(text) => ui.window.clipboard().set_text(&text),
        Event::ClipboardFiles(paths) => set_clipboard_files(&ui.window.clipboard(), &paths),
        Event::Ended(outcome) => {
            if !outcome.user_quit && outcome.code != 0 {
                let dialog = gtk::AlertDialog::builder().modal(true).message("The remote session ended").detail(&outcome.message).build();
                let code = outcome.code;
                dialog.choose(Some(&ui.window), None::<&gio::Cancellable>, move |_| finish(code));
                return;
            }
            finish(outcome.code);
        }
    });
}

fn make_cursor(shape: &CursorShape) -> Option<gdk::Cursor> {
    match shape {
        CursorShape::Default => None,
        CursorShape::Hidden => gdk::Cursor::from_name("none", None),
        CursorShape::Image(c) => {
            let bytes = glib::Bytes::from(&c.rgba[..]);
            let texture = gdk::MemoryTexture::new(c.width as i32, c.height as i32, gdk::MemoryFormat::R8g8b8a8, &bytes, c.width * 4);
            Some(gdk::Cursor::from_texture(&texture, c.hot_x as i32, c.hot_y as i32, None))
        }
    }
}

/// Where the framebuffer goes inside the widget: (x, y, scale).
fn fit(area_w: f64, area_h: f64, fb_w: usize, fb_h: usize) -> (f64, f64, f64) {
    let scale = (area_w / fb_w as f64).min(area_h / fb_h as f64);
    ((area_w - fb_w as f64 * scale) / 2.0, (area_h - fb_h as f64 * scale) / 2.0, scale)
}

fn draw(ui: &Ui, cr: &cairo::Context, width: i32, height: i32) {
    cr.set_source_rgb(0.0, 0.0, 0.0);
    let _ = cr.paint();
    let Some(session) = ui.session.get() else { return };
    session.with_framebuffer(|fw, fh, px| {
        let mut slot = ui.surface.borrow_mut();
        if slot.as_ref().is_none_or(|s| s.width() as usize != fw || s.height() as usize != fh) {
            *slot = cairo::ImageSurface::create(cairo::Format::Rgb24, fw as i32, fh as i32).ok();
        }
        let Some(surface) = slot.as_mut() else { return };
        let stride = surface.stride() as usize;
        if let Ok(mut data) = surface.data() {
            // CAIRO_FORMAT_RGB24 is the same 0x00RRGGBB as the framebuffer.
            for (row, line) in data.chunks_exact_mut(stride).zip(px.chunks_exact(fw)) {
                for (dst, p) in row.chunks_exact_mut(4).zip(line) {
                    dst.copy_from_slice(&p.to_ne_bytes());
                }
            }
        }
        let (x, y, scale) = fit(width as f64, height as f64, fw, fh);
        let _ = cr.save();
        cr.translate(x, y);
        cr.scale(scale, scale);
        let _ = cr.set_source_surface(&*surface, 0.0, 0.0);
        cr.source().set_filter(if scale >= 1.0 { cairo::Filter::Nearest } else { cairo::Filter::Good });
        let _ = cr.paint();
        let _ = cr.restore();
    });
}

/// A widget position in session pixels.
fn to_session(ui: &Ui, x: f64, y: f64) -> Option<(u16, u16)> {
    let (aw, ah) = (ui.area.width() as f64, ui.area.height() as f64);
    ui.session.get()?.with_framebuffer(|fw, fh, _| {
        let (ox, oy, scale) = fit(aw, ah, fw, fh);
        (((x - ox) / scale).clamp(0.0, fw as f64 - 1.0) as u16, ((y - oy) / scale).clamp(0.0, fh as f64 - 1.0) as u16)
    })
}

fn send_key(ui: &Ui, sc: Scancode, down: bool) {
    let Some(session) = ui.session.get() else { return };
    let mut held = ui.held.borrow_mut();
    held.retain(|&k| k != sc);
    if down {
        held.push(sc);
    }
    session.key(sc.0, sc.1, down);
}

/// Type one character-derived key, with Shift held exactly if needed.
fn type_char(ui: &Ui, sc: Scancode, shift: bool) {
    let held = ui.held.borrow().iter().any(|&k| k == scancode::LSHIFT || k == scancode::RSHIFT);
    if shift != held {
        send_key(ui, scancode::LSHIFT, shift);
        if !shift {
            send_key(ui, scancode::RSHIFT, false);
        }
    }
    send_key(ui, sc, true);
    send_key(ui, sc, false);
    if shift != held {
        send_key(ui, scancode::LSHIFT, held);
    }
}

/// X11 and Wayland hardware keycodes are evdev codes plus 8.
fn key_from_hardware(keycode: u32) -> Option<Scancode> {
    keycode.checked_sub(8).and_then(scancode::from_evdev)
}

/// What to type instead of the hardware key.
enum Typed {
    /// A key on a US keyboard, and whether it needs Shift.
    Key(Scancode, bool),
    /// A character no US key makes; see `Session::type_char`.
    Char(char),
}

/// The key to type instead of the hardware key, when they disagree about the
/// character. Virtual keyboards (on-screen keyboards, remote-control tools,
/// Wayland's virtual-keyboard server) define their own keymaps, so their key
/// codes mean nothing; the same goes for a non-US layout on a US-layout server.
/// The character is what the user meant.
fn typed_instead(keyval: gdk::Key, state: gdk::ModifierType, hardware: Option<Scancode>) -> Option<Typed> {
    // Shortcuts are about the key, not the character it would type.
    let shortcut = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK;
    // Keypad keys stay keys; dead keys are half a character.
    if state.intersects(shortcut) || keyval.name().is_some_and(|n| n.starts_with("KP_") || n.starts_with("dead_")) {
        return None;
    }
    let c = keyval.to_unicode().filter(|c| !c.is_control())?;
    let Some((sc, shift)) = scancode::from_us_char(c) else { return Some(Typed::Char(c)) };
    // Letters are left out of the Shift comparison, so Caps Lock stays a real key.
    let same_shift = c.is_alphabetic() || shift == state.contains(gdk::ModifierType::SHIFT_MASK);
    (hardware != Some(sc) || !same_shift).then_some(Typed::Key(sc, shift))
}

/// Type a character no US key makes, with Shift up: Shift would turn the
/// keypad keys of an Alt+0nnn code into arrows.
fn type_text(ui: &Ui, c: char) {
    let Some(session) = ui.session.get() else { return };
    let shift: Vec<Scancode> = ui.held.borrow().iter().copied().filter(|&k| k == scancode::LSHIFT || k == scancode::RSHIFT).collect();
    for &k in &shift {
        send_key(ui, k, false);
    }
    session.type_char(c);
    for &k in &shift {
        send_key(ui, k, true);
    }
}

/// Give the server our Caps, Num and Scroll Lock; they may have changed while
/// another window had the keyboard.
fn sync_locks(ui: &Ui) {
    let keyboard = gdk::Display::default().and_then(|d| d.default_seat()).and_then(|s| s.keyboard());
    if let (Some(s), Some(k)) = (ui.session.get(), keyboard) {
        s.sync_locks(k.is_caps_locked(), k.is_num_locked(), k.is_scroll_locked());
    }
}

fn attach_input(ui: &Rc<Ui>) {
    // Capture phase: Tab, F10 and the like must reach the server, not GTK.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let u = ui.clone();
    keys.connect_key_pressed(move |_, keyval, keycode, state| {
        let hardware = key_from_hardware(keycode);
        match typed_instead(keyval, state, hardware) {
            Some(Typed::Key(sc, shift)) => type_char(&u, sc, shift),
            Some(Typed::Char(c)) => type_text(&u, c),
            None => {
                if let Some(sc) = hardware {
                    send_key(&u, sc, true);
                }
            }
        }
        glib::Propagation::Stop
    });
    let u = ui.clone();
    keys.connect_key_released(move |_, _, keycode, _| {
        // Keys sent as text are already up.
        if let Some(sc) = key_from_hardware(keycode).filter(|sc| u.held.borrow().contains(sc)) {
            send_key(&u, sc, false);
        }
    });
    ui.window.add_controller(keys);

    let motion = gtk::EventControllerMotion::new();
    let u = ui.clone();
    motion.connect_motion(move |_, x, y| {
        if let Some((x, y)) = to_session(&u, x, y) {
            u.pointer.set((x, y));
            if let Some(s) = u.session.get() {
                s.mouse_move(x, y);
            }
        }
    });
    ui.area.add_controller(motion);

    // GestureClick tracks one button at a time; this sees them all.
    let buttons = gtk::EventControllerLegacy::new();
    let u = ui.clone();
    buttons.connect_event(move |_, event| {
        let down = match event.event_type() {
            gdk::EventType::ButtonPress => true,
            gdk::EventType::ButtonRelease => false,
            _ => return glib::Propagation::Proceed,
        };
        let number = event.downcast_ref::<gdk::ButtonEvent>().map_or(0, |b| b.button());
        let button = match number {
            1 => MouseButton::Left,
            2 => MouseButton::Middle,
            3 => MouseButton::Right,
            _ => return glib::Propagation::Proceed,
        };
        let (x, y) = u.pointer.get();
        if let Some(s) = u.session.get() {
            s.mouse_button(button, down, x, y);
        }
        glib::Propagation::Proceed
    });
    ui.area.add_controller(buttons);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE);
    let u = ui.clone();
    scroll.connect_scroll(move |_, _, dy| {
        let (x, y) = u.pointer.get();
        if let Some(s) = u.session.get() {
            for _ in 0..dy.abs().round() as i32 {
                s.wheel(dy < 0.0, x, y);
            }
        }
        glib::Propagation::Stop
    });
    ui.area.add_controller(scroll);

    // Don't leave keys stuck down on the server.
    let u = ui.clone();
    ui.window.connect_is_active_notify(move |w| {
        if w.is_active() {
            sync_locks(&u);
            // Text copied elsewhere reaches the server when you come back here.
            let u = u.clone();
            let clipboard = w.clipboard();
            if clipboard.formats().contains_type(gdk::FileList::static_type()) {
                clipboard.read_value_async(gdk::FileList::static_type(), glib::Priority::DEFAULT, None::<&gio::Cancellable>, move |value| {
                    if let (Some(s), Ok(Ok(files))) = (u.session.get(), value.map(|v| v.get::<gdk::FileList>())) {
                        launch::offer_clipboard_files(s.clone(), files.files().iter().filter_map(|f| f.path()).collect());
                    }
                });
            } else {
                clipboard.read_text_async(None::<&gio::Cancellable>, move |text| {
                    if let (Some(s), Ok(Some(text))) = (u.session.get(), text) {
                        s.set_clipboard(&text);
                    }
                });
            }
        } else {
            let held: Vec<Scancode> = u.held.borrow_mut().drain(..).collect();
            if let Some(s) = u.session.get() {
                for (code, ext) in held {
                    s.key(code, ext, false);
                }
            }
        }
    });

    let drop_target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let u = ui.clone();
    drop_target.connect_drop(move |_, value, x, y| {
        let Some(session) = u.session.get() else { return false };
        let Ok(files) = value.get::<gdk::FileList>() else { return false };
        let paths = files.files().iter().filter_map(|f| f.path()).collect();
        let title = u.window.title().map(|t| t.to_string()).unwrap_or_default();
        let (x, y) = to_session(&u, x, y).unwrap_or((0, 0));
        launch::drop_files(session.clone(), paths, x, y, move |line| {
            let text = line.unwrap_or_else(|| title.clone());
            glib::idle_add_once(move || with_ui(|ui| ui.window.set_title(Some(&text))));
        });
        true
    });
    ui.area.add_controller(drop_target);

    let u = ui.clone();
    ui.window.connect_close_request(move |_| {
        CLOSING.store(true, Ordering::Relaxed);
        save_thumbnail(&u, false);
        let code = u.session.get().map_or(0, |s| {
            s.quit();
            s.wait(Duration::from_secs(5)).map_or(0, |o| if o.user_quit { 0 } else { o.code })
        });
        finish(code);
    });
}

fn start_session(config: Config, saved_id: Option<String>) {
    BACK_TO_MANAGER.store(saved_id.is_some(), Ordering::Relaxed);
    if let Some(id) = saved_id {
        let _ = THUMBNAIL_ID.set(id);
    }
    let area = gtk::DrawingArea::builder()
        .content_width(config.width as i32)
        .content_height(config.height as i32)
        .hexpand(true)
        .vexpand(true)
        .focusable(true)
        .build();
    let window = gtk::Window::builder().title(config.window_title()).child(&area).build();
    let ui = Rc::new(Ui {
        window: window.clone(),
        area: area.clone(),
        session: OnceCell::new(),
        surface: RefCell::new(None),
        held: RefCell::new(Vec::new()),
        pointer: Cell::new((0, 0)),
        last_thumbnail: Cell::new(None),
    });
    UI.with(|slot| {
        let _ = slot.set(ui.clone());
    });
    let u = ui.clone();
    area.set_draw_func(move |_, cr, w, h| draw(&u, cr, w, h));
    attach_input(&ui);
    window.present();
    area.grab_focus();

    let screen = gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_then(|m| m.downcast::<gdk::Monitor>().ok())
        .map_or((1440, 900), |m| (m.geometry().width() as u32, m.geometry().height() as u32));
    match Session::start(config, Arc::new(GtkFrontend { screen })) {
        Ok(session) => {
            let _ = ui.session.set(session);
            sync_locks(&ui);
        }
        Err(e) => {
            eprintln!("vintrdp: could not start the session: {e}");
            finish(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Connection manager

struct Fields {
    name: gtk::Entry,
    computer: gtk::Entry,
    user: gtk::Entry,
    password: gtk::PasswordEntry,
    save_password: gtk::CheckButton,
    domain: gtk::Entry,
    slider: gtk::Scale,
    width: gtk::Entry,
    height: gtk::Entry,
    size_label: gtk::Label,
    depth: gtk::DropDown,
    server: gtk::DropDown,
    /// "Last screen", then one item per built-in monitor colour.
    picture: gtk::DropDown,
    share: gtk::Entry,
    clipboard: gtk::CheckButton,
    sound: gtk::CheckButton,
    fallback: gtk::CheckButton,
    /// "None", then the local printers (launch::printer_choices).
    printer: gtk::DropDown,
    printer_choices: RefCell<Vec<String>>,
    ports: gtk::Entry,
    /// Shows this connection's licence source: Custom, Global or None.
    licence_label: gtk::Label,
    /// The labels and contents of the Options, Folder, Printer and Ports rows.
    optional_rows: OnceCell<[(gtk::Widget, gtk::Widget); 4]>,
    /// The connection's colour, kept while "Last screen" is chosen.
    color: Cell<usize>,
    /// Set while we change the size widgets ourselves.
    syncing: Cell<bool>,
}

impl Fields {
    fn load(&self, form: &Form) {
        self.syncing.set(true);
        self.name.set_text(&form.name);
        self.computer.set_text(&form.computer);
        self.user.set_text(&form.username);
        self.password.set_text(&form.password);
        self.domain.set_text(&form.domain);
        self.width.set_text(&form.width);
        self.height.set_text(&form.height);
        self.server.set_selected(ServerKind::ALL.iter().position(|&p| p == form.server).unwrap_or(0) as u32);
        self.apply_server();
        self.depth.set_selected(form.depth_index.min(form.server.depths() - 1) as u32);
        self.clipboard.set_active(form.clipboard);
        self.sound.set_active(form.sound);
        self.fallback.set_active(form.fallback_rdp4);
        self.save_password.set_active(form.save_password);
        self.share.set_text(&form.share);
        self.ports.set_text(&form.ports);
        let choices = launch::printer_choices(&form.printer);
        let titles: Vec<&str> = choices.iter().map(String::as_str).collect();
        self.printer.set_model(Some(&gtk::StringList::new(&titles)));
        self.printer.set_selected(choices.iter().position(|c| *c == form.printer).unwrap_or(0) as u32);
        *self.printer_choices.borrow_mut() = choices;
        self.color.set(form.color);
        self.picture.set_selected(match form.picture {
            Picture::Screen => 0,
            Picture::Monitor => (form.color % MONITORS.len() + 1) as u32,
        });
        self.syncing.set(false);
        self.size_changed();
    }

    fn read(&self) -> Form {
        let picture = self.picture.selected() as usize;
        Form {
            name: self.name.text().to_string(),
            computer: self.computer.text().to_string(),
            username: self.user.text().to_string(),
            password: self.password.text().to_string(),
            domain: self.domain.text().to_string(),
            width: self.width.text().to_string(),
            height: self.height.text().to_string(),
            depth_index: self.depth.selected() as usize,
            server: self.server_kind(),
            clipboard: self.clipboard.is_active(),
            sound: self.sound.is_active(),
            fallback_rdp4: self.fallback.is_active(),
            save_password: self.save_password.is_active(),
            picture: if picture == 0 { Picture::Screen } else { Picture::Monitor },
            color: if picture > 0 { picture - 1 } else { self.color.get() },
            share: self.share.text().to_string(),
            printer: self.printer_choices.borrow().get(self.printer.selected() as usize).map(|c| launch::printer_from_choice(c)).unwrap_or_default(),
            ports: self.ports.text().to_string(),
        }
    }

    fn server_kind(&self) -> ServerKind {
        ServerKind::ALL[(self.server.selected() as usize).min(3)]
    }

    /// Offer only what the chosen kind of server can do.
    fn apply_server(&self) {
        let kind = self.server_kind();
        let depth = self.depth.selected() as usize;
        let titles: Vec<String> = DEPTHS[..kind.depths()].iter().map(|d| format!("{d} bit")).collect();
        let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
        self.depth.set_model(Some(&gtk::StringList::new(&titles)));
        self.depth.set_selected(depth.min(kind.depths() - 1) as u32);
        self.sound.set_visible(kind.has_sound());
        if let Some([sharing, folder, printer, ports]) = self.optional_rows.get() {
            for ((label, widget), shown) in [(sharing, kind.has_clipboard()), (folder, kind.has_folder()), (printer, kind.has_devices()), (ports, kind.has_devices())] {
                label.set_visible(shown);
                widget.set_visible(shown);
            }
        }
    }

    /// Width/height were edited: move the slider to match, or say "Custom".
    fn size_changed(&self) {
        if self.syncing.replace(true) {
            return;
        }
        for field in [&self.width, &self.height] {
            let text = field.text();
            let clean = launch::clean_dimension(&text);
            if clean != text.as_str() {
                field.set_text(&clean);
            }
        }
        let (w, h) = (self.width.text(), self.height.text());
        if let Some(i) = size_index(&w, &h) {
            self.slider.set_value(i as f64);
        }
        self.size_label.set_text(&size_label(&w, &h));
        self.syncing.set(false);
    }

    /// The slider moved: fill in width and height.
    fn slider_changed(&self) {
        if self.syncing.get() {
            return;
        }
        let (w, h, _) = SIZES[(self.slider.value().round() as usize).min(SIZES.len() - 1)];
        self.syncing.set(true);
        self.width.set_text(&w.to_string());
        self.height.set_text(&h.to_string());
        self.syncing.set(false);
        self.size_changed();
    }
}

struct Manager {
    window: gtk::Window,
    tiles: gtk::FlowBox,
    fields: Fields,
    store: RefCell<Store>,
    /// The saved connection being edited.
    selected: RefCell<Option<String>>,
    /// Set while the tiles are rebuilt, when selection signals mean nothing.
    rebuilding: Cell<bool>,
    connecting: Cell<bool>,
}

impl Manager {
    fn report(&self, result: std::io::Result<()>) {
        if let Err(e) = result {
            alert(Some(&self.window), "Could not update saved connections", &e.to_string());
        }
    }

    /// Show `id` in the form and highlight its tile.
    fn select(&self, id: Option<String>) {
        let saved = id.as_deref().and_then(|id| self.store.borrow().get(id).cloned());
        *self.selected.borrow_mut() = saved.as_ref().map(|s| s.id.clone());
        self.fields.load(&saved.map(|s| s.to_form()).unwrap_or_default());
        self.update_licence_label();
        self.sync_selection();
    }

    /// Update the License row to show this connection's licence source.
    fn update_licence_label(&self) {
        let text = match self.selected.borrow().as_deref() {
            Some(id) if launch::connection_licence_path(id).exists() => "Custom",
            _ if launch::imported_licence().exists() => "Global",
            _ => "None",
        };
        self.fields.licence_label.set_text(text);
    }

    /// List the imported licences, with the option to remove the global one.
    fn show_licences(self: Rc<Self>) {
        let (global, per) = self.store.borrow().licences();
        let report = launch::licence_report(global, &per);
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message("Imported licenses")
            .detail(report)
            .build();
        if global.is_none() {
            dialog.show(Some(&self.window));
            return;
        }
        dialog.set_buttons(&["Close", "Remove Global License"]);
        dialog.set_default_button(0);
        dialog.set_cancel_button(0);
        let m = self.clone();
        dialog.choose(Some(&self.window), None::<&gio::Cancellable>, move |res| {
            if res == Ok(1) {
                if let Err(e) = launch::remove_global_licence() {
                    alert(Some(&m.window), "Could not remove the licence", &e.to_string());
                }
                m.update_licence_label();
            }
        });
    }

    fn sync_selection(&self) {
        let row = self.selected.borrow().as_deref().and_then(|id| self.store.borrow().position(id));
        let was = self.rebuilding.replace(true);
        match row.and_then(|r| self.tiles.child_at_index(r as i32)) {
            Some(child) => self.tiles.select_child(&child),
            None => self.tiles.unselect_all(),
        }
        self.rebuilding.set(was);
    }

    /// Rebuild the tiles from the store.
    fn refresh(&self) {
        self.rebuilding.set(true);
        self.tiles.remove_all();
        for c in &self.store.borrow().connections {
            let image = match c.tile_image() {
                TileImage::Screenshot(path) => gdk::Texture::from_filename(path).ok(),
                TileImage::Monitor(_) => None,
            }
            .or_else(|| texture(MONITORS[c.color % MONITORS.len()].1));

            let tile = gtk::Box::new(gtk::Orientation::Vertical, 4);
            // gtk::Image, not Picture: it holds its size whatever the source's.
            let picture = gtk::Image::builder().pixel_size(104).build();
            picture.set_paintable(image.as_ref());
            let label = gtk::Label::builder().label(c.display_name()).ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(14).build();
            tile.append(&picture);
            tile.append(&label);

            let child = gtk::FlowBoxChild::new();
            child.set_child(Some(&tile));
            child.add_css_class("tile");
            self.tiles.append(&child);
        }
        self.rebuilding.set(false);
        self.sync_selection();
    }

    /// "New": add a tile straight away and start editing it.
    fn add_new(&self) {
        let made = self.store.borrow_mut().new_blank();
        match made {
            Ok(blank) => {
                self.refresh();
                self.select(Some(blank.id));
            }
            Err(e) => self.report(Err(e)),
        }
        self.fields.name.grab_focus();
    }

    /// Validate and save the form. Returns the config to connect with.
    fn save(&self) -> Option<(Config, String)> {
        let form = self.fields.read();
        let id = self.selected.borrow().clone();
        let checked = self.store.borrow().validate(&form, id.as_deref());
        let mut config = match checked {
            Ok(c) => c,
            Err(e) => {
                alert(Some(&self.window), "Check the connection settings", &e);
                return None;
            }
        };
        let saved = remember(&mut self.store.borrow_mut(), &form, &config, id);
        config.licence = launch::effective_licence(&saved.id);
        *self.selected.borrow_mut() = Some(saved.id.clone());
        self.refresh();
        Some((config, saved.id))
    }

    fn connect(&self) {
        let Some((config, id)) = self.save() else { return };
        self.connecting.set(true);
        self.window.close();
        start_session(config, Some(id));
    }

    fn delete(self: &Rc<Self>) {
        let Some(id) = self.selected.borrow().clone() else { return };
        let shown = self.store.borrow().get(&id).map(|c| c.display_name().to_string()).unwrap_or_default();
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(format!("Delete \u{201c}{shown}\u{201d}?"))
            .detail("Its settings, picture and saved password will be removed.")
            .buttons(["Delete", "Cancel"])
            .default_button(1)
            .cancel_button(1)
            .build();
        let this = self.clone();
        dialog.choose(Some(&self.window), None::<&gio::Cancellable>, move |answer| {
            if answer != Ok(0) {
                return;
            }
            let result = this.store.borrow_mut().remove(&id);
            this.report(result);
            let first = this.store.borrow().connections.first().map(|c| c.id.clone());
            this.refresh();
            match first {
                Some(first) => this.select(Some(first)),
                None => this.add_new(),
            }
        });
    }
}

fn show_manager() {
    let entry = || gtk::Entry::builder().hexpand(true).build();
    let dimension = || gtk::Entry::builder().max_length(5).width_chars(6).max_width_chars(6).input_purpose(gtk::InputPurpose::Digits).build();
    let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, (SIZES.len() - 1) as f64, 1.0);
    slider.set_draw_value(false);
    slider.set_round_digits(0);
    for i in 0..SIZES.len() {
        slider.add_mark(i as f64, gtk::PositionType::Bottom, None);
    }
    let depths: Vec<String> = DEPTHS.iter().map(|d| format!("{d} bit")).collect();
    let protocols: Vec<&str> = ServerKind::ALL.iter().map(|p| p.label()).collect();
    let mut pictures = vec!["Last screen".to_string()];
    pictures.extend(MONITORS.iter().map(|(name, _)| format!("{name} monitor")));
    let strings = |items: Vec<&str>| gtk::DropDown::from_strings(&items);

    let fields = Fields {
        name: entry(),
        computer: entry(),
        user: entry(),
        password: gtk::PasswordEntry::builder().show_peek_icon(true).hexpand(true).build(),
        save_password: gtk::CheckButton::with_label("Save password"),
        domain: entry(),
        slider,
        width: dimension(),
        height: dimension(),
        size_label: gtk::Label::new(None),
        depth: strings(depths.iter().map(String::as_str).collect()),
        server: strings(protocols),
        picture: strings(pictures.iter().map(String::as_str).collect()),
        share: entry(),
        clipboard: gtk::CheckButton::with_label("Clipboard"),
        sound: gtk::CheckButton::with_label("Sound"),
        fallback: gtk::CheckButton::with_label("RDP 4 fallback"),
        printer: strings(vec![launch::NO_PRINTER]),
        printer_choices: RefCell::new(vec![launch::NO_PRINTER.to_string()]),
        ports: entry(),
        licence_label: gtk::Label::new(Some("None")),
        optional_rows: OnceCell::new(),
        color: Cell::new(0),
        syncing: Cell::new(false),
    };
    fields.computer.set_placeholder_text(Some("host or host:port"));
    fields.share.set_placeholder_text(Some("No folder selected"));
    fields.ports.set_placeholder_text(Some(launch::PORTS_HINT));
    let choose_folder = gtk::Button::with_label("Choose\u{2026}");
    let share_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    share_row.append(&fields.share);
    share_row.append(&choose_folder);

    let choose_licence = gtk::Button::with_label("Choose\u{2026}");
    let use_global = gtk::Button::with_label("Use Global");
    let licence_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    fields.licence_label.set_halign(gtk::Align::Start);
    licence_row.append(&fields.licence_label);
    licence_row.append(&choose_licence);
    licence_row.append(&use_global);

    let size_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    size_row.append(&gtk::Label::new(Some("W")));
    size_row.append(&fields.width);
    size_row.append(&gtk::Label::new(Some("H")));
    size_row.append(&fields.height);
    size_row.append(&fields.size_label);

    let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
    let options_row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    options_row.append(&fields.clipboard);
    options_row.append(&fields.sound);
    options_row.append(&fields.fallback);
    let rows: [(&str, &gtk::Widget); 16] = [
        ("Name", fields.name.upcast_ref()),
        ("Computer", fields.computer.upcast_ref()),
        ("User name", fields.user.upcast_ref()),
        ("Password", fields.password.upcast_ref()),
        ("", fields.save_password.upcast_ref()),
        ("Domain", fields.domain.upcast_ref()),
        ("Size", fields.slider.upcast_ref()),
        ("", size_row.upcast_ref()),
        ("Server", fields.server.upcast_ref()),
        ("Colours", fields.depth.upcast_ref()),
        ("Picture", fields.picture.upcast_ref()),
        ("Options", options_row.upcast_ref()),
        ("Folder", share_row.upcast_ref()),
        ("Printer", fields.printer.upcast_ref()),
        ("Ports", fields.ports.upcast_ref()),
        ("License", licence_row.upcast_ref()),
    ];
    let mut labels = Vec::new();
    for (i, (label, widget)) in rows.into_iter().enumerate() {
        let label = gtk::Label::builder().label(label).halign(gtk::Align::End).build();
        grid.attach(&label, 0, i as i32, 1, 1);
        grid.attach(widget, 1, i as i32, 1, 1);
        labels.push(label);
    }
    let _ = fields.optional_rows.set([
        (labels[11].clone().upcast(), options_row.clone().upcast()),
        (labels[12].clone().upcast(), share_row.clone().upcast()),
        (labels[13].clone().upcast(), fields.printer.clone().upcast()),
        (labels[14].clone().upcast(), fields.ports.clone().upcast()),
    ]);

    let tiles = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(false)
        .homogeneous(true)
        .valign(gtk::Align::Start)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .row_spacing(10)
        .column_spacing(10)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(10)
        .margin_end(10)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&tiles)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .has_frame(true)
        .width_request(450)
        .vexpand(true)
        .build();

    let button = |label: &str| gtk::Button::with_label(label);
    let (new, delete, save, connect) = (button("New"), button("Delete"), button("Save"), button("Connect"));
    let import = button("Import TS License\u{2026}");
    let licences = button("Licenses\u{2026}");
    connect.add_css_class("suggested-action");
    let spacer = gtk::Box::builder().hexpand(true).build();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for w in [new.upcast_ref::<gtk::Widget>(), delete.upcast_ref(), import.upcast_ref(), licences.upcast_ref(), spacer.upcast_ref(), save.upcast_ref(), connect.upcast_ref()] {
        actions.append(w);
    }

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 20);
    body.append(&scroller);
    body.append(&grid);
    let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(16).margin_bottom(16).margin_start(16).margin_end(16).build();
    page.append(&body);
    page.append(&actions);

    // A label: the AppImage's trimmed icon theme has no About icon.
    let about = gtk::Button::builder().label("About").tooltip_text("About vintrdp").build();
    let header = gtk::HeaderBar::new();
    header.pack_end(&about);
    let window = gtk::Window::builder().title("vintrdp").child(&page).default_width(900).default_height(600).build();
    window.set_titlebar(Some(&header));
    window.set_default_widget(Some(&connect));
    for e in [&fields.name, &fields.computer, &fields.user, &fields.domain, &fields.width, &fields.height] {
        e.set_activates_default(true);
    }
    fields.password.set_activates_default(true);

    let manager = Rc::new(Manager {
        window: window.clone(),
        tiles: tiles.clone(),
        fields,
        store: RefCell::new(Store::load()),
        selected: RefCell::new(None),
        rebuilding: Cell::new(false),
        connecting: Cell::new(false),
    });

    macro_rules! on {
        ($widget:expr, $signal:ident, $m:ident, $($args:pat),* => $body:expr) => {{
            let $m = manager.clone();
            $widget.$signal(move |$($args),*| $body);
        }};
    }
    on!(new, connect_clicked, m, _ => m.add_new());
    on!(delete, connect_clicked, m, _ => m.delete());
    on!(save, connect_clicked, m, _ => { m.save(); });
    on!(connect, connect_clicked, m, _ => m.connect());
    on!(about, connect_clicked, m, _ => show_about(&m.window));
    let m = manager.clone();
    manager.fields.server.connect_selected_notify(move |_| {
        if !m.fields.syncing.get() {
            m.fields.apply_server();
        }
    });
    on!(choose_folder, connect_clicked, m, _ => {
        let m2 = m.clone();
        gtk::FileDialog::builder().title("Choose a folder to share with the server").build().select_folder(Some(&m.window), None::<&gio::Cancellable>, move |folder| {
            if let Some(path) = folder.ok().and_then(|f| f.path()) {
                m2.fields.share.set_text(&path.to_string_lossy());
            }
        });
    });
    on!(import, connect_clicked, m, _ => {
        let m2 = m.clone();
        gtk::FileDialog::builder().title("Choose a Terminal Services licence file (.cal)").build().open(Some(&m.window), None::<&gio::Cancellable>, move |file| {
            if let Some(path) = file.ok().and_then(|f| f.path()) {
                match launch::import_licence(&path) {
                    Ok(()) => alert(Some(&m2.window), "Licence imported", "vintrdp will present it to every server you connect to."),
                    Err(e) => alert(Some(&m2.window), "Could not import the licence", &e.to_string()),
                }
            }
        });
    });
    on!(choose_licence, connect_clicked, m, _ => {
        let Some(id) = m.selected.borrow().clone() else { return };
        let m2 = m.clone();
        gtk::FileDialog::builder().title("Choose a licence file (.cal) for this connection").build().open(Some(&m.window), None::<&gio::Cancellable>, move |file| {
            if let Some(path) = file.ok().and_then(|f| f.path()) {
                let r = m2.store.borrow().set_connection_licence(&id, &path);
                m2.report(r);
                m2.update_licence_label();
            }
        });
    });
    on!(use_global, connect_clicked, m, _ => {
        if let Some(id) = m.selected.borrow().clone() {
            m.store.borrow().clear_connection_licence(&id);
            m.update_licence_label();
        }
    });
    on!(licences, connect_clicked, m, _ => m.clone().show_licences());
    on!(manager.fields.slider, connect_value_changed, m, _ => m.fields.slider_changed());
    on!(manager.fields.width, connect_changed, m, _ => m.fields.size_changed());
    on!(manager.fields.height, connect_changed, m, _ => m.fields.size_changed());
    on!(tiles, connect_child_activated, m, _, _ => m.connect());
    on!(tiles, connect_selected_children_changed, m, flow => {
        if m.rebuilding.get() {
            return;
        }
        let index = flow.selected_children().first().map(|c| c.index());
        let id = index.and_then(|i| m.store.borrow().connections.get(i as usize).map(|c| c.id.clone()));
        if id.is_some() && id != *m.selected.borrow() {
            m.select(id);
        }
    });
    let m = manager.clone();
    window.connect_close_request(move |_| {
        if m.connecting.get() {
            return glib::Propagation::Proceed;
        }
        std::process::exit(0);
    });

    manager.refresh();
    let first = manager.store.borrow().connections.first().map(|c| c.id.clone());
    match first {
        Some(id) => manager.select(Some(id)),
        None => manager.add_new(),
    }
    window.present();
}
