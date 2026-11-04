//! The connection manager's model, shared by every front-end: the form the
//! user edits, saved connections (a JSON file in the per-user settings
//! folder) and their passwords (the OS keychain, never the file).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use vintrdp_core::{Config, RdpVersion};

pub const DEPTHS: [u32; 5] = [8, 15, 16, 24, 32];

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
pub fn clean_dimension(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).take(5).collect()
}

/// Which RDP version to ask for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// RDP 5, falling back to RDP 4 if the server only speaks RDP 4.
    #[default]
    Auto,
    Rdp4,
    Rdp5,
}

impl Protocol {
    pub const ALL: [Protocol; 3] = [Protocol::Auto, Protocol::Rdp4, Protocol::Rdp5];

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Auto => "Automatic",
            Protocol::Rdp4 => "RDP 4 (Windows NT 4.0)",
            Protocol::Rdp5 => "RDP 5 (Windows 2000 / XP / 2003)",
        }
    }

    fn version(self) -> RdpVersion {
        match self {
            Protocol::Auto => RdpVersion::Auto,
            Protocol::Rdp4 => RdpVersion::V4,
            Protocol::Rdp5 => RdpVersion::V5,
        }
    }
}

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
    pub protocol: Protocol,
    /// Keep the password in the system keychain.
    pub save_password: bool,
    pub picture: Picture,
    /// Index into [`MONITORS`].
    pub color: usize,
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
            protocol: Protocol::Auto,
            save_password: true,
            picture: Picture::Screen,
            color: 0,
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
        let name = self.name.trim();
        Ok(Config {
            server,
            port,
            username: self.username.trim().to_string(),
            password: self.password.clone(),
            domain: self.domain.trim().to_string(),
            width: num(&self.width, "Width")?,
            height: num(&self.height, "Height")?,
            depth: DEPTHS[self.depth_index.min(DEPTHS.len() - 1)],
            version: self.protocol.version(),
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
    #[serde(default)]
    pub protocol: Protocol,
    pub save_password: bool,
    /// Seconds since the Unix epoch; the list is most-recent first.
    pub last_used: u64,
    #[serde(default)]
    pub picture: Picture,
    /// Index into [`MONITORS`].
    #[serde(default)]
    pub color: usize,
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
            protocol: Protocol::Auto,
            save_password: true,
            last_used: now(),
            picture: Picture::Screen,
            color,
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
            protocol: self.protocol,
            save_password: self.save_password,
            picture: self.picture,
            color: self.color % MONITORS.len(),
        }
    }

    /// Build from a validated form. `id` keeps an existing entry's identity.
    pub fn from_form(form: &Form, config: &Config, id: Option<String>) -> Self {
        Saved {
            picture: form.picture,
            color: form.color % MONITORS.len(),
            id: id.unwrap_or_else(new_id),
            name: form.name.trim().to_string(),
            computer: form.computer.trim().to_string(),
            username: config.username.clone(),
            domain: config.domain.clone(),
            width: config.width,
            height: config.height,
            depth: config.depth,
            protocol: form.protocol,
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
        self.connections.retain(|c| c.id != id);
        delete_password(id);
        self.save()
    }

    pub fn get(&self, id: &str) -> Option<&Saved> {
        self.connections.iter().find(|c| c.id == id)
    }

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
        if let Err(e) = keychain(&saved.id).and_then(|k| k.set_password(&form.password)) {
            eprintln!("vintrdp: could not save the password in the keychain: {e}");
        }
    } else {
        delete_password(&saved.id);
    }
    saved
}

fn keychain(id: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, id)
}

pub fn load_password(id: &str) -> Option<String> {
    keychain(id).and_then(|k| k.get_password()).ok()
}

pub fn delete_password(id: &str) {
    if let Ok(k) = keychain(id) {
        let _ = k.delete_credential();
    }
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
    fn form_to_config() {
        let f = Form { computer: "nt4:3390".into(), protocol: Protocol::Rdp4, ..Form::default() };
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

        let form = Form { name: "Lab NT4".into(), computer: "nt4".into(), protocol: Protocol::Rdp4, save_password: false, ..Form::default() };
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

        // Files written before the protocol menu existed still load.
        std::fs::write(&path, br#"{"connections":[{"id":"x","name":"","computer":"h","username":"u","domain":"","width":800,"height":600,"depth":8,"rdp4":true,"save_password":false,"last_used":1}]}"#).unwrap();
        assert_eq!(Store::load_from(path.clone()).connections[0].protocol, Protocol::Auto);
        let _ = std::fs::remove_dir_all(dir);
    }
}
