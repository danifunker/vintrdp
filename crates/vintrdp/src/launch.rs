//! The connection manager's model, shared by every front-end: the form the
//! user edits, saved connections (a JSON file in the per-user settings
//! folder) and their passwords (the OS keychain, never the file).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use vintrdp_core::{Config, RdpVersion};

pub const DEPTHS: [u32; 5] = [8, 15, 16, 24, 32];

/// The version shown to users: a release build stamps VINTRDP_VERSION.
pub const VERSION: &str = match option_env!("VINTRDP_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub const REPO_URL: &str = "https://github.com/danifunker/vintrdp";
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub const RDESKTOP_URL: &str = "https://github.com/marcofortina/rdesktop";
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub const GPL_URL: &str = "https://www.gnu.org/licenses/gpl-3.0.html";
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const DESCRIPTION: &str = "RDP 4 and 5 client for Windows NT 4.0 TSE to Server 2003.";
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const FORK_CREDIT: &str = "A fork of rdesktop, \u{a9} 1999\u{2013}2016 Matthew Chapman et al.";

/// Start a new copy of the app (the core is one session per process, so this
/// is how a finished session gets back to the connection manager). Inside an
/// AppImage the running binary is on a mount that goes away when we exit.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn relaunch() {
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok());
    if let Some(exe) = exe {
        let _ = std::process::Command::new(exe).spawn();
    }
}

/// An image as straight (not premultiplied) RGBA bytes.
#[cfg(windows)]
pub struct Rgba {
    pub width: usize,
    pub height: usize,
    pub bytes: Vec<u8>,
}

#[cfg(windows)]
pub fn decode_png(data: &[u8]) -> Option<Rgba> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (width, height) = (info.width as usize, info.height as usize);
    buf.truncate(info.buffer_size());
    let bytes = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some(Rgba { width, height, bytes })
}

/// Built-in tile pictures: a CRT monitor in eight colours (drawn by
/// `examples/make_icons.rs`).
pub const MONITORS: [(&str, &[u8]); 8] = [
    ("Beige", include_bytes!("../assets/monitors/0-beige.png")),
    ("Graphite", include_bytes!("../assets/monitors/1-graphite.png")),
    ("Red", include_bytes!("../assets/monitors/2-red.png")),
    ("Amber", include_bytes!("../assets/monitors/3-amber.png")),
    ("Green", include_bytes!("../assets/monitors/4-green.png")),
    ("Teal", include_bytes!("../assets/monitors/5-teal.png")),
    ("Blue", include_bytes!("../assets/monitors/6-blue.png")),
    ("Violet", include_bytes!("../assets/monitors/7-violet.png")),
];

/// The application icon (1024x1024 PNG).
#[cfg_attr(windows, allow(dead_code))]
pub const APP_ICON: &[u8] = include_bytes!("../assets/icon-1024.png");

/// What a connection's tile shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Picture {
    /// The screen from the last session (the monitor until there is one).
    #[default]
    Screen,
    /// Always the monitor in the connection's colour.
    Monitor,
}

/// What a tile should draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileImage {
    Screenshot(PathBuf),
    /// Index into [`MONITORS`].
    Monitor(usize),
}

/// Common session sizes, smallest first, for the size slider.
pub const SIZES: [(u32, u32, &str); 14] = [
    (640, 480, "VGA"),
    (800, 600, "SVGA"),
    (1024, 768, "XGA"),
    (1152, 864, "XGA+"),
    (1280, 720, "720p"),
    (1280, 800, "WXGA"),
    (1280, 1024, "SXGA"),
    (1366, 768, "HD"),
    (1440, 900, "WXGA+"),
    (1600, 900, "HD+"),
    (1600, 1200, "UXGA"),
    (1680, 1050, "WSXGA+"),
    (1920, 1080, "1080p"),
    (1920, 1200, "WUXGA"),
];

/// The slider position for a size, if it's one of [`SIZES`].
pub fn size_index(width: &str, height: &str) -> Option<usize> {
    let (w, h) = (width.trim().parse::<u32>().ok()?, height.trim().parse::<u32>().ok()?);
    SIZES.iter().position(|&(sw, sh, _)| (sw, sh) == (w, h))
}

/// The name shown after the W/H boxes: the size's name, or "Custom".
pub fn size_label(width: &str, height: &str) -> String {
    match size_index(width, height) {
        Some(i) => SIZES[i].2.to_string(),
        None => "Custom".to_string(),
    }
}

/// Keep a size box to at most 5 digits.
#[cfg_attr(windows, allow(dead_code))]
pub fn clean_dimension(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).take(5).collect()
}

/// What kind of server a connection is for. This decides which options the
/// form offers: there's no point showing what the server can't do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerKind {
    /// Detected on connecting; every option is shown.
    #[default]
    Auto,
    #[serde(alias = "rdp4")]
    Nt4,
    Win2000,
    #[serde(alias = "rdp5")]
    Win2003,
}

impl ServerKind {
    pub const ALL: [ServerKind; 4] = [ServerKind::Auto, ServerKind::Nt4, ServerKind::Win2000, ServerKind::Win2003];

    pub fn label(self) -> &'static str {
        match self {
            ServerKind::Auto => "Automatic",
            ServerKind::Nt4 => "Windows NT 4.0",
            ServerKind::Win2000 => "Windows 2000",
            ServerKind::Win2003 => "Windows Server 2003",
        }
    }

    fn version(self) -> RdpVersion {
        match self {
            ServerKind::Auto => RdpVersion::Auto,
            ServerKind::Nt4 => RdpVersion::V4,
            ServerKind::Win2000 | ServerKind::Win2003 => RdpVersion::V5,
        }
    }

    /// How many of [`DEPTHS`] this server can use, starting from the lowest.
    pub fn depths(self) -> usize {
        match self {
            ServerKind::Nt4 => 1,
            ServerKind::Win2000 => 3,
            ServerKind::Win2003 => 4,
            ServerKind::Auto => DEPTHS.len(),
        }
    }

    pub fn has_clipboard(self) -> bool {
        self != ServerKind::Nt4
    }

    pub fn has_sound(self) -> bool {
        matches!(self, ServerKind::Auto | ServerKind::Win2003)
    }

    pub fn has_folder(self) -> bool {
        matches!(self, ServerKind::Auto | ServerKind::Win2003)
    }

    /// Printers and serial/parallel ports: RDP 5 servers.
    pub fn has_devices(self) -> bool {
        self != ServerKind::Nt4
    }
}

/// The Printer menu: "None", then the local printers, the default first. A
/// saved printer that's gone (unplugged, say) stays on the menu so it isn't
/// lost by opening the connection.
pub fn printer_choices(current: &str) -> Vec<String> {
    static LOCAL: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let mut choices = vec![NO_PRINTER.to_string()];
    choices.extend(LOCAL.get_or_init(vintrdp_core::printing::printers).iter().cloned());
    let current = current.trim();
    if !current.is_empty() && !choices[1..].iter().any(|c| c == current) {
        choices.push(current.to_string());
    }
    choices
}

/// The Printer menu's first item.
pub const NO_PRINTER: &str = "None";

/// A Printer menu item as Form::printer.
pub fn printer_from_choice(choice: &str) -> String {
    if choice == NO_PRINTER { String::new() } else { choice.to_string() }
}

/// What the Ports field shows when empty.
pub const PORTS_HINT: &str = if cfg!(windows) {
    "COM1=COM3, LPT1=LPT1"
} else if cfg!(target_os = "macos") {
    "COM1=/dev/cu.usbserial"
} else {
    "COM1=/dev/ttyUSB0"
};

/// Keychain service name for saved passwords.
const KEYCHAIN_SERVICE: &str = "vintrdp";

/// What the user typed; every front-end's dialog edits one of these.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    /// Display name; empty means "use the computer name".
    pub name: String,
    /// `host` or `host:port`
    pub computer: String,
    pub username: String,
    pub password: String,
    pub domain: String,
    pub width: String,
    pub height: String,
    /// Index into [`DEPTHS`]
    pub depth_index: usize,
    pub server: ServerKind,
    /// Keep the password in the system keychain.
    pub save_password: bool,
    pub picture: Picture,
    /// Index into [`MONITORS`].
    pub color: usize,
    pub clipboard: bool,
    pub sound: bool,
    /// Connect with RDP 4 if the server refuses RDP 5.
    pub fallback_rdp4: bool,
    /// Ask for RDP's own encryption. Off reaches servers that have none
    /// (ntrdps); a server that needs it still gets it, after a retry.
    pub encryption: bool,
    /// A local folder to show the server as `\\\\tsclient\\Shared`; empty for none.
    pub share: String,
    /// A local printer to offer the server; empty for none.
    pub printer: String,
    /// Serial and parallel ports, e.g. `COM1=/dev/ttyUSB0`; see [`vintrdp_core::parse_ports`].
    pub ports: String,
}

impl Default for Form {
    fn default() -> Self {
        let c = Config::default();
        Self {
            name: String::new(),
            computer: String::new(),
            username: c.username,
            password: String::new(),
            domain: String::new(),
            width: c.width.to_string(),
            height: c.height.to_string(),
            depth_index: DEPTHS.iter().position(|&d| d == c.depth).unwrap_or(2),
            server: ServerKind::Auto,
            save_password: true,
            picture: Picture::Screen,
            color: 0,
            clipboard: true,
            sound: true,
            fallback_rdp4: true,
            encryption: true,
            share: String::new(),
            printer: String::new(),
            ports: String::new(),
        }
    }
}

impl Form {
    /// The name the connection will be listed under.
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() { self.computer.trim().to_string() } else { name.to_string() }
    }

    pub fn to_config(&self) -> Result<Config, String> {
        let computer = self.computer.trim();
        if computer.is_empty() {
            return Err("Enter a computer name or address.".into());
        }
        let num = |s: &str, what: &str| -> Result<u32, String> {
            s.trim()
                .parse::<u32>()
                .ok()
                .filter(|n| (200..=8192).contains(n))
                .ok_or_else(|| format!("{what} must be a number between 200 and 8192."))
        };
        let (server, port) = vintrdp_core::parse_server(computer)?;
        let share = if self.server.has_folder() { self.share.trim() } else { "" };
        if !share.is_empty() && !Path::new(share).is_dir() {
            return Err(format!("The shared folder {share} doesn't exist."));
        }
        let disks = if share.is_empty() { Vec::new() } else { vec![("Shared".to_string(), PathBuf::from(share))] };
        let devices = self.server.has_devices();
        let printer = self.printer.trim();
        let printers = if devices && !printer.is_empty() { vec![vintrdp_core::Printer { name: Some(printer.to_string()), driver: None }] } else { Vec::new() };
        let (serial_ports, parallel_ports) = if devices { vintrdp_core::parse_ports(&self.ports)? } else { Default::default() };
        let name = self.name.trim();
        Ok(Config {
            server,
            port,
            username: self.username.trim().to_string(),
            password: self.password.clone(),
            domain: self.domain.trim().to_string(),
            width: num(&self.width, "Width")?,
            height: num(&self.height, "Height")?,
            depth: DEPTHS[self.depth_index.min(self.server.depths() - 1)],
            clipboard: self.clipboard && self.server.has_clipboard(),
            transfer: (self.clipboard && self.server.has_folder()).then(transfer_dir),
            licence: Some(imported_licence()),
            sound: self.sound && self.server.has_sound(),
            fallback_rdp4: self.fallback_rdp4,
            encryption: self.encryption,
            version: self.server.version(),
            disks,
            printers,
            serial_ports,
            parallel_ports,
            title: (!name.is_empty()).then(|| name.to_string()),
            ..Config::default()
        })
    }
}

/// A saved connection. Everything but the password, which lives in the
/// keychain under [`Saved::id`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub id: String,
    pub name: String,
    pub computer: String,
    pub username: String,
    pub domain: String,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    #[serde(default, alias = "protocol")]
    pub server: ServerKind,
    pub save_password: bool,
    /// Seconds since the Unix epoch; the list is most-recent first.
    pub last_used: u64,
    #[serde(default)]
    pub picture: Picture,
    /// Index into [`MONITORS`].
    #[serde(default)]
    pub color: usize,
    #[serde(default)]
    pub share: String,
    #[serde(default = "enabled")]
    pub clipboard: bool,
    #[serde(default = "enabled")]
    pub sound: bool,
    #[serde(default = "enabled")]
    pub fallback_rdp4: bool,
    #[serde(default = "enabled")]
    pub encryption: bool,
    #[serde(default)]
    pub printer: String,
    #[serde(default)]
    pub ports: String,
}

fn enabled() -> bool {
    true
}

impl Saved {
    /// A fresh entry for the "New" button; see [`Store::new_blank`].
    fn blank(name: String, color: usize) -> Self {
        let f = Form::default();
        Saved {
            id: new_id(),
            name,
            computer: String::new(),
            username: f.username,
            domain: String::new(),
            width: 1024,
            height: 768,
            depth: DEPTHS[f.depth_index],
            server: ServerKind::Auto,
            save_password: true,
            last_used: now(),
            picture: Picture::Screen,
            color,
            share: String::new(),
            clipboard: true,
            sound: true,
            fallback_rdp4: true,
            encryption: true,
            printer: String::new(),
            ports: String::new(),
        }
    }

    pub fn display_name(&self) -> &str {
        if !self.name.trim().is_empty() {
            &self.name
        } else if !self.computer.trim().is_empty() {
            &self.computer
        } else {
            "Untitled"
        }
    }

    /// What this connection's tile shows.
    pub fn tile_image(&self) -> TileImage {
        let shot = thumbnail_path(&self.id);
        if self.picture == Picture::Screen && shot.exists() {
            TileImage::Screenshot(shot)
        } else {
            TileImage::Monitor(self.color % MONITORS.len())
        }
    }

    /// The form for this connection, with the password from the keychain.
    pub fn to_form(&self) -> Form {
        Form {
            name: self.name.clone(),
            computer: self.computer.clone(),
            username: self.username.clone(),
            password: if self.save_password { load_password(&self.id).unwrap_or_default() } else { String::new() },
            domain: self.domain.clone(),
            width: self.width.to_string(),
            height: self.height.to_string(),
            depth_index: DEPTHS.iter().position(|&d| d == self.depth).unwrap_or(2),
            server: self.server,
            save_password: self.save_password,
            picture: self.picture,
            color: self.color % MONITORS.len(),
            share: self.share.clone(),
            clipboard: self.clipboard,
            sound: self.sound,
            fallback_rdp4: self.fallback_rdp4,
            encryption: self.encryption,
            printer: self.printer.clone(),
            ports: self.ports.clone(),
        }
    }

    /// Build from a validated form. `id` keeps an existing entry's identity.
    pub fn from_form(form: &Form, config: &Config, id: Option<String>) -> Self {
        Saved {
            picture: form.picture,
            color: form.color % MONITORS.len(),
            share: form.share.trim().to_string(),
            clipboard: form.clipboard,
            sound: form.sound,
            fallback_rdp4: form.fallback_rdp4,
            encryption: form.encryption,
            printer: form.printer.trim().to_string(),
            ports: form.ports.trim().to_string(),
            id: id.unwrap_or_else(new_id),
            name: form.name.trim().to_string(),
            computer: form.computer.trim().to_string(),
            username: config.username.clone(),
            domain: config.domain.clone(),
            width: config.width,
            height: config.height,
            depth: config.depth,
            server: form.server,
            save_password: form.save_password,
            last_used: now(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    connections: Vec<Saved>,
}

/// The saved-connections file.
pub struct Store {
    path: PathBuf,
    pub connections: Vec<Saved>,
}

impl Store {
    /// Load from the per-user settings folder (an empty list if missing or
    /// unreadable; a corrupt file is kept aside as `.bak`).
    pub fn load() -> Store {
        Store::load_from(settings_dir().join("connections.json"))
    }

    pub fn load_from(path: PathBuf) -> Store {
        let file = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<File>(&bytes) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("vintrdp: {} is unreadable ({e}); starting with no saved connections", path.display());
                    let _ = std::fs::rename(&path, path.with_extension("json.bak"));
                    File::default()
                }
            },
            Err(_) => File::default(),
        };
        let mut store = Store { path, connections: file.connections };
        store.sort();
        store
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn sort(&mut self) {
        self.connections.sort_by(|a, b| b.last_used.cmp(&a.last_used));
    }

    /// Add or replace (by id), then write the file.
    pub fn upsert(&mut self, saved: Saved) -> std::io::Result<()> {
        match self.connections.iter_mut().find(|c| c.id == saved.id) {
            Some(c) => *c = saved,
            None => self.connections.push(saved),
        }
        self.sort();
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> std::io::Result<()> {
        let _ = std::fs::remove_file(thumbnail_path(id));
        let _ = std::fs::remove_file(connection_licence_path(id));
        self.connections.retain(|c| c.id != id);
        delete_password(id);
        self.save()
    }

    /// Give connection `id` its own licence, a copy of `source`.
    pub fn set_connection_licence(&self, id: &str, source: &Path) -> std::io::Result<()> {
        let dest = connection_licence_path(id);
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(source, dest).map(|_| ())
    }

    /// Drop connection `id`'s own licence; it falls back to the global one.
    pub fn clear_connection_licence(&self, id: &str) {
        let _ = std::fs::remove_file(connection_licence_path(id));
    }

    /// The global licence's size (if imported) and one entry per connection
    /// that has its own, for the Licenses list.
    pub fn licences(&self) -> (Option<u64>, Vec<LicenceEntry>) {
        let size = |p: PathBuf| std::fs::metadata(p).ok().map(|m| m.len());
        let per = self
            .connections
            .iter()
            .filter_map(|c| size(connection_licence_path(&c.id)).map(|bytes| LicenceEntry { label: c.display_name().to_string(), bytes }))
            .collect();
        (size(imported_licence()), per)
    }

    pub fn get(&self, id: &str) -> Option<&Saved> {
        self.connections.iter().find(|c| c.id == id)
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub fn position(&self, id: &str) -> Option<usize> {
        self.connections.iter().position(|c| c.id == id)
    }

    /// Is `name` used by a connection other than `except`? (Case-insensitive.)
    pub fn name_taken(&self, name: &str, except: Option<&str>) -> bool {
        let name = name.trim().to_lowercase();
        self.connections.iter().any(|c| Some(c.id.as_str()) != except && c.display_name().trim().to_lowercase() == name)
    }

    /// `base`, or `base (2)`, `base (3)`, ... whichever is free.
    pub fn unique_name(&self, base: &str) -> String {
        (1..)
            .map(|n| if n == 1 { base.to_string() } else { format!("{base} ({n})") })
            .find(|name| !self.name_taken(name, None))
            .unwrap()
    }

    /// Add a "New connection" entry with a free name and the next colour.
    pub fn new_blank(&mut self) -> std::io::Result<Saved> {
        let blank = Saved::blank(self.unique_name("New connection"), self.connections.len() % MONITORS.len());
        self.upsert(blank.clone())?;
        Ok(blank)
    }

    /// Check a form before saving it as connection `id` (None = new).
    pub fn validate(&self, form: &Form, id: Option<&str>) -> Result<Config, String> {
        let config = form.to_config()?;
        let name = form.display_name();
        if self.name_taken(&name, id) {
            return Err(format!("There is already a connection called \u{201c}{name}\u{201d}. Choose a different name."));
        }
        Ok(config)
    }

    pub fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(&File { connections: self.connections.clone() })?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, &self.path)
    }
}

/// Save a connection after a successful form submit: the settings go to the
/// file, the password to the keychain (or is removed if not wanted).
pub fn remember(store: &mut Store, form: &Form, config: &Config, id: Option<String>) -> Saved {
    let saved = Saved::from_form(form, config, id);
    if let Err(e) = store.upsert(saved.clone()) {
        eprintln!("vintrdp: could not save {}: {e}", store.path().display());
    }
    if form.save_password && !form.password.is_empty() {
        if let Err(e) = keychain::save(KEYCHAIN_SERVICE, &saved.id, &form.password) {
            eprintln!("vintrdp: could not save the password in the keychain: {e}");
        }
    } else {
        delete_password(&saved.id);
    }
    saved
}

/// Whether this system can keep passwords. Windows before XP has no
/// Credential Manager.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn can_save_passwords() -> bool {
    keychain::available()
}

pub fn load_password(id: &str) -> Option<String> {
    keychain::load(KEYCHAIN_SERVICE, id)
}

pub fn delete_password(id: &str) {
    keychain::delete(KEYCHAIN_SERVICE, id);
}

/// On Windows, our own Credential Manager calls, looked up at run time.
#[cfg(windows)]
use crate::win32::credential as keychain;

/// The `keyring` crate: Keychain on macOS, Secret Service elsewhere.
#[cfg(not(windows))]
mod keychain {
    fn entry(service: &str, id: &str) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(service, id)
    }

    pub fn available() -> bool {
        true
    }

    pub fn save(service: &str, id: &str, password: &str) -> keyring::Result<()> {
        entry(service, id).and_then(|k| k.set_password(password))
    }

    pub fn load(service: &str, id: &str) -> Option<String> {
        entry(service, id).and_then(|k| k.get_password()).ok()
    }

    pub fn delete(service: &str, id: &str) {
        if let Ok(k) = entry(service, id) {
            let _ = k.delete_credential();
        }
    }
}

/// `name` in `folder`, or `name (2)`, `name (3)`, ... if that's taken.
fn free_path(folder: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let plain = folder.join(name);
    if !plain.exists() {
        return plain;
    }
    let name = Path::new(name);
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let ext = name.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..).map(|n| folder.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap()
}

fn copy_tree(source: &Path, dest: &Path) -> std::io::Result<()> {
    if source.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(source, dest).map(|_| ())
    }
}

/// Copy files and folders into `folder`, never over something already there.
fn copy_into(folder: &Path, paths: &[PathBuf]) -> std::io::Result<usize> {
    for path in paths {
        let name = path.file_name().ok_or_else(|| std::io::Error::other("not a file"))?;
        copy_tree(path, &free_path(folder, name))?;
    }
    Ok(paths.len())
}

/// Copy `paths` to the transfer drive and put them on the server's clipboard.
/// Returns how many, and only then can they be pasted.
pub fn stage_files(session: &vintrdp_core::Session, paths: &[PathBuf]) -> std::io::Result<usize> {
    let transfer = session.config().transfer.as_ref().ok_or_else(|| std::io::Error::other("no transfer drive"))?;
    let id = new_id();
    let folder = transfer.join(&id);
    std::fs::create_dir_all(&folder)?;
    let count = copy_into(&folder, paths)?;
    let names = paths.iter().filter_map(|p| p.file_name()).map(|n| format!("C:\\{id}\\{}", n.to_string_lossy())).collect();
    session.set_clipboard_files(names);
    Ok(count)
}

/// Files dropped on the session window: paste them at (x, y) on the server.
/// Runs on another thread. `status` gets a line for the window title, then
/// `None` a few seconds later to put the title back; it has to hand over to
/// the UI thread itself.
pub fn drop_files(session: vintrdp_core::Session, paths: Vec<PathBuf>, x: u16, y: u16, status: impl Fn(Option<String>) + Send + 'static) {
    std::thread::spawn(move || {
        let line = if session.can_send_files() {
            status(Some("Copying\u{2026}".to_string()));
            match stage_files(&session, &paths) {
                Ok(count) => {
                    // The server's clipboard helper has to hear about them first.
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    session.paste_at(x, y);
                    if count == 1 { "Pasted 1 item on the server".to_string() } else { format!("Pasted {count} items on the server") }
                }
                Err(e) => format!("Couldn't copy: {e}"),
            }
        } else {
            "This server can't receive files".to_string()
        };
        status(Some(line));
        std::thread::sleep(std::time::Duration::from_secs(5));
        status(None);
    });
}

/// Files on the local clipboard, seen when the session window gets focus: make
/// them ready to paste on the server. The same list isn't copied twice.
pub fn offer_clipboard_files(session: vintrdp_core::Session, paths: Vec<PathBuf>) {
    static LAST: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    if !session.can_send_files() || std::mem::replace(&mut *LAST.lock().unwrap(), paths.clone()) == paths {
        return;
    }
    // Files copied on the server arrive on the transfer drive; they're already
    // on the server's clipboard, so don't send them back.
    let transfer = session.config().transfer.clone().unwrap_or_default();
    if paths.iter().all(|p| p.starts_with(&transfer)) {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = stage_files(&session, &paths) {
            eprintln!("vintrdp: could not stage files for the clipboard: {e}");
        }
    });
}

pub fn thumbnail_path(id: &str) -> PathBuf {
    settings_dir().join("thumbnails").join(format!("{id}.png"))
}

/// Largest thumbnail edge, in pixels.
const THUMB_MAX: usize = 320;

/// Save a small PNG of the session screen (`0x00RRGGBB` pixels) as `id`'s
/// tile picture. Box-filtered so text and window edges stay legible.
pub fn save_thumbnail(id: &str, width: usize, height: usize, px: &[u32]) -> std::io::Result<()> {
    if width == 0 || height == 0 || px.len() < width * height {
        return Ok(());
    }
    let scale = (width.max(height) as f64 / THUMB_MAX as f64).max(1.0);
    let (tw, th) = (((width as f64 / scale) as usize).max(1), ((height as f64 / scale) as usize).max(1));
    let mut rgb = Vec::with_capacity(tw * th * 3);
    for ty in 0..th {
        let (y0, y1) = (ty * height / th, ((ty + 1) * height / th).max(ty * height / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * width / tw, ((tx + 1) * width / tw).max(tx * width / tw + 1));
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in y0..y1.min(height) {
                for &p in &px[y * width + x0..y * width + x1.min(width)] {
                    r += (p >> 16) & 0xff;
                    g += (p >> 8) & 0xff;
                    b += p & 0xff;
                    n += 1;
                }
            }
            let n = n.max(1);
            rgb.extend([(r / n) as u8, (g / n) as u8, (b / n) as u8]);
        }
    }
    let path = thumbnail_path(id);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("png.tmp");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&tmp)?), tw as u32, th as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().and_then(|mut w| w.write_image_data(&rgb)).map_err(std::io::Error::other)?;
    std::fs::rename(tmp, path)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn new_id() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{t:x}-{:x}", std::process::id())
}

/// Where files on their way to the server are staged.
pub fn transfer_dir() -> PathBuf {
    settings_dir().join("Transfer")
}

/// The imported licence, offered to every server when it exists. One global
/// file, in rdesktop's `.cal` format.
pub fn imported_licence() -> PathBuf {
    settings_dir().join("licence.cal")
}

/// Copy a user-chosen licence file into the global location.
pub fn import_licence(source: &Path) -> std::io::Result<()> {
    let dest = imported_licence();
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::copy(source, &dest)?;
    Ok(())
}

/// Where a connection keeps its own licence, when it has one.
pub fn connection_licence_path(id: &str) -> PathBuf {
    settings_dir().join("licenses").join(format!("{id}.cal"))
}

/// The licence to present for connection `id`: its own if set, else the global
/// import. None means fall back to the per-server auto-cache.
pub fn effective_licence(id: &str) -> Option<PathBuf> {
    [connection_licence_path(id), imported_licence()].into_iter().find(|p| p.exists())
}

/// Forget the global licence.
pub fn remove_global_licence() -> std::io::Result<()> {
    match std::fs::remove_file(imported_licence()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// One row of the Licenses list.
pub struct LicenceEntry {
    pub label: String,
    pub bytes: u64,
}

/// Human-readable lines describing every imported licence, for a dialog.
pub fn licence_report(global: Option<u64>, per_connection: &[LicenceEntry]) -> String {
    let kb = |b: u64| format!("{:.1} KB", b as f64 / 1024.0);
    let mut lines = vec![match global {
        Some(b) => format!("Global licence: imported ({})", kb(b)),
        None => "Global licence: none".to_string(),
    }];
    if per_connection.is_empty() {
        lines.push("\nNo connection has its own licence.".to_string());
    } else {
        lines.push("\nConnections with their own licence:".to_string());
        for e in per_connection {
            lines.push(format!("  \u{2022} {} ({})", e.label, kb(e.bytes)));
        }
        lines.push("\nOther connections use the global licence. To remove a\nconnection's licence, select it and choose Use Global.".to_string());
    }
    lines.join("\n")
}

/// Per-user settings folder for vintrdp.
pub fn settings_dir() -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/vintrdp")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home).join("vintrdp")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("vintrdp")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn licence_report_lists_sources() {
        // Nothing imported.
        let none = licence_report(None, &[]);
        assert!(none.contains("Global licence: none"));
        assert!(none.contains("No connection has its own licence."));

        // A global and two per-connection licences.
        let per = [
            LicenceEntry { label: "Lab 2003".into(), bytes: 2048 },
            LicenceEntry { label: "NT4 TSE".into(), bytes: 1536 },
        ];
        let r = licence_report(Some(4096), &per);
        assert!(r.contains("Global licence: imported (4.0 KB)"));
        assert!(r.contains("\u{2022} Lab 2003 (2.0 KB)"));
        assert!(r.contains("\u{2022} NT4 TSE (1.5 KB)"));
        assert!(r.contains("Use Global"));
    }

    #[test]
    fn devices_reach_the_config() {
        let form = |server| Form { computer: "h".into(), server, printer: "Office Laser".into(), ports: "COM1=/dev/ttyUSB0, LPT1=/dev/usb/lp0".into(), ..Form::default() };
        let c = form(ServerKind::Win2000).to_config().unwrap();
        assert_eq!(c.printers, [vintrdp_core::Printer { name: Some("Office Laser".into()), driver: None }]);
        assert_eq!((c.serial_ports.len(), c.parallel_ports.len()), (1, 1));
        let nt4 = form(ServerKind::Nt4).to_config().unwrap();
        assert!(nt4.printers.is_empty() && nt4.serial_ports.is_empty() && nt4.parallel_ports.is_empty());
        assert!(Form { ports: "COM1".into(), ..form(ServerKind::Win2003) }.to_config().is_err());
        assert_eq!(printer_from_choice(NO_PRINTER), "");
        assert_eq!(printer_choices("Gone Printer").last().map(String::as_str), Some("Gone Printer"));
    }

    #[test]
    fn form_to_config() {
        let f = Form { computer: "nt4:3390".into(), server: ServerKind::Nt4, ..Form::default() };
        let c = f.to_config().unwrap();
        assert_eq!((c.server.as_str(), c.port, c.version), ("nt4", 3390, RdpVersion::V4));
        assert!(Form { width: "12".into(), computer: "x".into(), ..Form::default() }.to_config().is_err());
        assert!(Form::default().to_config().is_err());
    }

    #[test]
    fn unique_names() {
        let dir = std::env::temp_dir().join(format!("vintrdp-test-{}", new_id()));
        let mut store = Store::load_from(dir.join("connections.json"));
        let a = store.new_blank().unwrap();
        let b = store.new_blank().unwrap();
        let c = store.new_blank().unwrap();
        assert_eq!((a.name.as_str(), b.name.as_str(), c.name.as_str()), ("New connection", "New connection (2)", "New connection (3)"));
        assert_eq!((a.color, b.color, c.color), (0, 1, 2));

        // Renaming b to a's name (any case) is refused; keeping its own is fine.
        let form = |name: &str| Form { name: name.into(), computer: "host".into(), ..Form::default() };
        assert!(store.validate(&form("new CONNECTION"), Some(&b.id)).is_err());
        assert!(store.validate(&form("New connection (2)"), Some(&b.id)).is_ok());
        // An unnamed connection is listed by its computer name, which counts too.
        assert!(store.validate(&Form { name: String::new(), computer: "new connection".into(), ..Form::default() }, None).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn server_kinds_limit_the_options() {
        let form = |server| Form {
            computer: "host".into(),
            server,
            depth_index: 4,
            share: std::env::temp_dir().to_string_lossy().into_owned(),
            ..Form::default()
        };
        let nt4 = form(ServerKind::Nt4).to_config().unwrap();
        assert_eq!((nt4.depth, nt4.clipboard, nt4.sound, nt4.disks.len(), nt4.version), (8, false, false, 0, RdpVersion::V4));
        assert!(nt4.transfer.is_none());
        let w2k = form(ServerKind::Win2000).to_config().unwrap();
        assert_eq!((w2k.depth, w2k.clipboard, w2k.sound, w2k.disks.len()), (16, true, false, 0));
        let w2k3 = form(ServerKind::Win2003).to_config().unwrap();
        assert_eq!((w2k3.depth, w2k3.clipboard, w2k3.sound, w2k3.disks.len()), (24, true, true, 1));
        assert!(w2k3.transfer.is_some());
        assert!(form(ServerKind::Win2000).to_config().unwrap().transfer.is_none());
        assert_eq!(form(ServerKind::Auto).to_config().unwrap().depth, 32);
    }

    #[test]
    fn dropped_files_never_overwrite() {
        let root = std::env::temp_dir().join(format!("vintrdp-drop-{}", new_id()));
        let (from, to) = (root.join("from"), root.join("to"));
        std::fs::create_dir_all(from.join("dir/sub")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("a.txt"), "new").unwrap();
        std::fs::write(from.join("dir/sub/b.txt"), "deep").unwrap();
        std::fs::write(to.join("a.txt"), "old").unwrap();

        assert_eq!(copy_into(&to, &[from.join("a.txt"), from.join("dir")]).unwrap(), 2);
        assert_eq!(std::fs::read_to_string(to.join("a.txt")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(to.join("a (2).txt")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(to.join("dir/sub/b.txt")).unwrap(), "deep");
        copy_into(&to, &[from.join("dir")]).unwrap();
        assert!(to.join("dir (2)/sub/b.txt").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn old_saved_protocols_load() {
        let json = br#"{"connections":[{"id":"x","name":"","computer":"h","username":"u","domain":"","width":800,"height":600,"depth":8,"protocol":"rdp4","save_password":false,"last_used":1}]}"#;
        let path = std::env::temp_dir().join(format!("vintrdp-{}.json", new_id()));
        std::fs::write(&path, json).unwrap();
        let store = Store::load_from(path.clone());
        assert_eq!(store.connections[0].server, ServerKind::Nt4);
        assert!(store.connections[0].sound);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn builtin_pictures_are_pngs() {
        for (_, bytes) in MONITORS.iter().chain(std::iter::once(&("app", APP_ICON))) {
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        }
    }

    #[test]
    fn thumbnail_png() {
        let id = format!("test-{}", new_id());
        let px: Vec<u32> = (0..800 * 600).map(|i| if i % 800 < 400 { 0xff0000 } else { 0x0000ff }).collect();
        save_thumbnail(&id, 800, 600, &px).unwrap();
        let path = thumbnail_path(&id);
        let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let info = decoder.read_info().unwrap().info().clone();
        assert_eq!((info.width, info.height), (320, 240));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn sizes() {
        assert_eq!(size_index("1024", " 768 "), Some(2));
        assert_eq!(size_index("1000", "700"), None);
        assert_eq!(size_label("1920", "1080"), "1080p");
        assert_eq!(size_label("1000", "x"), "Custom");
        assert_eq!(clean_dimension("12a34567"), "12345");
        assert!(SIZES.windows(2).all(|w| w[0].0 * w[0].1 < w[1].0 * w[1].1 || w[0].0 < w[1].0));
    }

    #[test]
    fn store_round_trip_and_ordering() {
        let dir = std::env::temp_dir().join(format!("vintrdp-test-{}", new_id()));
        let path = dir.join("connections.json");
        let mut store = Store::load_from(path.clone());
        assert!(store.connections.is_empty());

        let form = Form { name: "Lab NT4".into(), computer: "nt4".into(), server: ServerKind::Nt4, save_password: false, ..Form::default() };
        let config = form.to_config().unwrap();
        let mut a = Saved::from_form(&form, &config, None);
        a.last_used = 10;
        let mut b = Saved { id: "b".into(), computer: "w2k".into(), name: String::new(), last_used: 20, ..a.clone() };
        assert_eq!(Saved::blank("New connection".into(), 0).display_name(), "New connection");
        store.upsert(a.clone()).unwrap();
        store.upsert(b.clone()).unwrap();

        let reloaded = Store::load_from(path.clone());
        assert_eq!(reloaded.connections, vec![b.clone(), a.clone()]);
        assert_eq!(reloaded.connections[0].display_name(), "w2k");
        assert_eq!(reloaded.connections[1].display_name(), "Lab NT4");
        assert_eq!(reloaded.connections[1].to_form().computer, "nt4");

        b.last_used = 5;
        store.upsert(b).unwrap();
        store.remove(&a.id).unwrap();
        assert_eq!(Store::load_from(path.clone()).connections.len(), 1);

        std::fs::write(&path, b"not json").unwrap();
        assert!(Store::load_from(path.clone()).connections.is_empty());
        assert!(path.with_extension("json.bak").exists());

        // Files written before the server menu existed still load.
        std::fs::write(&path, br#"{"connections":[{"id":"x","name":"","computer":"h","username":"u","domain":"","width":800,"height":600,"depth":8,"rdp4":true,"save_password":false,"last_used":1}]}"#).unwrap();
        assert_eq!(Store::load_from(path.clone()).connections[0].server, ServerKind::Auto);
        let _ = std::fs::remove_dir_all(dir);
    }
}
