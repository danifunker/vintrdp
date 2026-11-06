//! Connection parameters, and a parser for rdesktop-style command lines.

use std::path::PathBuf;

use crate::state::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RdpVersion {
    /// Try RDP 5; if the server rejects it (NT 4.0 TSE does), retry with RDP 4.
    #[default]
    Auto,
    V4,
    V5,
}

/// The `-x` "experience" setting: which visual effects the server may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Experience {
    Modem,
    Broadband,
    Lan,
    /// rdesktop's default: no full-window drag or menu animations.
    #[default]
    Default,
    /// Raw TS_PERFORMANCE flags.
    Flags(u32),
}

impl Experience {
    pub fn flags(self) -> u32 {
        match self {
            Experience::Modem => {
                PERF_DISABLE_CURSOR_SHADOW
                    | PERF_DISABLE_WALLPAPER
                    | PERF_DISABLE_FULLWINDOWDRAG
                    | PERF_DISABLE_MENUANIMATIONS
                    | PERF_DISABLE_THEMING
            }
            Experience::Broadband => PERF_DISABLE_WALLPAPER | PERF_ENABLE_FONT_SMOOTHING,
            Experience::Lan => PERF_ENABLE_FONT_SMOOTHING,
            Experience::Default => PERF_DISABLE_FULLWINDOWDRAG | PERF_DISABLE_MENUANIMATIONS | PERF_ENABLE_FONT_SMOOTHING,
            Experience::Flags(f) => f,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub server: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub domain: String,
    /// Program to start instead of the desktop (`-s`).
    pub shell: String,
    /// Working directory for `shell` (`-c`).
    pub directory: String,
    pub width: u32,
    pub height: u32,
    /// Colour depth: 8, 15, 16, 24 or 32. Forced to 8 for RDP 4.
    pub depth: u32,
    pub version: RdpVersion,
    pub encryption: bool,
    /// Encrypt only the logon packet (`-E`).
    pub encrypt_login_only: bool,
    pub console: bool,
    pub compression: bool,
    pub bitmap_cache: bool,
    pub persistent_bitmap_cache: bool,
    pub experience: Experience,
    /// Name the server sees for this machine (`-n`); defaults to the host name.
    pub hostname: Option<String>,
    pub keyboard_layout: u32,
    pub numlock_sync: bool,
    pub local_cursor: bool,
    /// Redirected folders (`-r disk:NAME=PATH`).
    pub disks: Vec<(String, PathBuf)>,
    pub title: Option<String>,
    pub tls_version: Option<String>,
    pub reconnect_on_network_error: bool,
    pub verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: String::new(),
            port: 3389,
            username: std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default(),
            password: String::new(),
            domain: String::new(),
            shell: String::new(),
            directory: String::new(),
            width: 1024,
            height: 768,
            depth: 16,
            version: RdpVersion::Auto,
            encryption: true,
            encrypt_login_only: false,
            console: false,
            compression: false,
            bitmap_cache: true,
            persistent_bitmap_cache: false,
            experience: Experience::Default,
            hostname: None,
            keyboard_layout: 0x409,
            numlock_sync: false,
            local_cursor: false,
            disks: Vec::new(),
            title: None,
            tls_version: None,
            reconnect_on_network_error: false,
            verbose: false,
        }
    }
}

pub const USAGE: &str = "\
Usage: vintrdp [options] server[:port]

  -u USER        user name            -p PASS     password (- reads stdin)
  -d DOMAIN      domain               -s SHELL    program to start
  -c DIR         working directory    -n NAME     client host name
  -g WxH         session size         -a DEPTH    colour depth (8/15/16/24/32)
  -4 / -5        force RDP 4 / RDP 5 (default: RDP 5, falling back to RDP 4)
  -k LAYOUT      keyboard layout id, e.g. 0x409
  -T NAME        connection name         -0          attach to console session
  -z             compression          -x m|b|l|HEX experience (modem/broadband/LAN)
  -b             no bitmap cache      -P          persistent bitmap cache
  -e             no encryption        -E          encrypt logon packet only
  -r disk:NAME=PATH                   redirect a local folder
  -N             sync Num Lock        -M          use local mouse cursor
  -V 1.0|1.1|1.2 TLS version          -R          reconnect on network errors
  -v             verbose logging      -h          this help
";

impl Config {
    /// Parse rdesktop-compatible arguments (without argv[0]).
    pub fn from_args<I, S>(args: I) -> Result<Config, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut c = Config::default();
        let mut server = None;
        let mut it = args.into_iter().map(Into::into).peekable();
        while let Some(arg) = it.next() {
            let Some(flag) = arg.strip_prefix('-').filter(|f| !f.is_empty()) else {
                if server.replace(arg).is_some() {
                    return Err("more than one server given".into());
                }
                continue;
            };
            let mut chars = flag.chars();
            let opt = chars.next().unwrap();
            let inline: String = chars.collect();
            let takes_value = "udscpngakTxrVwL".contains(opt);
            let value = if takes_value {
                if !inline.is_empty() {
                    inline
                } else {
                    it.next().ok_or_else(|| format!("-{opt} needs a value"))?
                }
            } else {
                if !inline.is_empty() {
                    return Err(format!("unknown option -{flag}"));
                }
                String::new()
            };
            match opt {
                'u' => c.username = value,
                'p' => c.password = if value == "-" { read_password()? } else { value },
                'd' => c.domain = value,
                's' => c.shell = value,
                'c' => c.directory = value,
                'n' => c.hostname = Some(value),
                'g' => (c.width, c.height) = parse_geometry(&value)?,
                'a' => {
                    c.depth = value.parse().map_err(|_| format!("bad colour depth {value}"))?;
                    if ![8, 15, 16, 24, 32].contains(&c.depth) {
                        return Err(format!("unsupported colour depth {value}"));
                    }
                }
                'k' => c.keyboard_layout = parse_int(&value)?,
                'T' => c.title = Some(value),
                'x' => {
                    c.experience = match value.chars().next() {
                        Some('m') => Experience::Modem,
                        Some('b') => Experience::Broadband,
                        Some('l') => Experience::Lan,
                        _ => Experience::Flags(u32::from_str_radix(value.trim_start_matches("0x"), 16).map_err(|_| format!("bad -x value {value}"))?),
                    }
                }
                'r' => {
                    let spec = value.strip_prefix("disk:").ok_or_else(|| format!("unsupported redirection {value}"))?;
                    for item in spec.split(',') {
                        let (name, path) = item.split_once('=').ok_or_else(|| format!("expected disk:NAME=PATH, got {item}"))?;
                        c.disks.push((name.to_string(), PathBuf::from(path)));
                    }
                }
                'V' => c.tls_version = Some(value),
                'w' | 'L' => {} // X11 WM_CLASS / codepage: no effect here
                '4' => c.version = RdpVersion::V4,
                '5' => c.version = RdpVersion::V5,
                '0' => c.console = true,
                'z' => c.compression = true,
                'b' => c.bitmap_cache = false,
                'P' => c.persistent_bitmap_cache = true,
                'e' => c.encryption = false,
                'E' => c.encrypt_login_only = true,
                'N' => c.numlock_sync = true,
                'M' => c.local_cursor = true,
                'R' => c.reconnect_on_network_error = true,
                'v' => c.verbose = true,
                'f' | 'K' | 'D' | 'm' | 'C' | 'B' => {} // X11 window behaviour
                'h' | '?' => return Err(USAGE.into()),
                _ => return Err(format!("unknown option -{opt}\n\n{USAGE}")),
            }
        }
        let server = server.ok_or_else(|| USAGE.to_string())?;
        (c.server, c.port) = parse_server(&server)?;
        Ok(c)
    }

    /// The short host name the server sees for this client.
    pub fn client_hostname(&self) -> String {
        if let Some(h) = &self.hostname {
            return h.clone();
        }
        let name = local_hostname().unwrap_or_else(|| "vintrdp".into());
        name.split('.').next().unwrap_or("vintrdp").to_string()
    }

    /// "name / host" (or just the host when there's no `-T` name).
    fn display_host(&self) -> String {
        match self.title.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() && name != self.server => format!("{name} / {}", self.server),
            _ => self.server.clone(),
        }
    }

    /// Window title before the protocol is known: "name / host - vintrdp".
    pub fn window_title(&self) -> String {
        format!("{} - vintrdp", self.display_host())
    }

    /// Window title once connected: "name / host (NT4) - vintrdp" for an
    /// RDP 4 session, "(RDP 5)" otherwise.
    pub fn session_title(&self, rdp4: bool) -> String {
        format!("{} ({}) - vintrdp", self.display_host(), if rdp4 { "NT4" } else { "RDP 5" })
    }
}

fn parse_int(s: &str) -> Result<u32, String> {
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => s.parse(),
    };
    r.map_err(|_| format!("bad number {s}"))
}

fn parse_geometry(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or_else(|| format!("expected WIDTHxHEIGHT, got {s}"))?;
    let w: u32 = w.parse().map_err(|_| format!("bad width in {s}"))?;
    let h: u32 = h.parse().map_err(|_| format!("bad height in {s}"))?;
    if !(200..=8192).contains(&w) || !(200..=8192).contains(&h) {
        return Err(format!("session size {s} out of range"));
    }
    Ok((w, h))
}

/// `host`, `host:port`, `[v6addr]` or `[v6addr]:port`.
pub fn parse_server(s: &str) -> Result<(String, u16), String> {
    let port = |p: &str| p.parse::<u16>().map_err(|_| format!("bad port in {s}"));
    if let Some(rest) = s.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or_else(|| format!("unterminated [ in {s}"))?;
        return match after.strip_prefix(':') {
            Some(p) => Ok((host.into(), port(p)?)),
            None if after.is_empty() => Ok((host.into(), 3389)),
            None => Err(format!("unexpected text after ] in {s}")),
        };
    }
    match s.rsplit_once(':') {
        // A bare IPv6 address has several colons and no port.
        Some((host, p)) if !host.contains(':') => Ok((host.into(), port(p)?)),
        _ if s.is_empty() => Err("empty server name".into()),
        _ => Ok((s.into(), 3389)),
    }
}

fn read_password() -> Result<String, String> {
    use std::io::BufRead;
    eprint!("Password: ");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

#[cfg(unix)]
fn local_hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for its length.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

#[cfg(windows)]
fn local_hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_options() {
        let c = Config::from_args(["-4", "-u", "admin", "-pSecret", "-g", "800x600", "nt4:3390"]).unwrap();
        assert_eq!(c.version, RdpVersion::V4);
        assert_eq!(c.username, "admin");
        assert_eq!(c.password, "Secret");
        assert_eq!((c.width, c.height), (800, 600));
        assert_eq!((c.server.as_str(), c.port), ("nt4", 3390));
        assert_eq!(c.session_title(true), "nt4 (NT4) - vintrdp");
        let named = Config { title: Some("Lab".into()), ..c };
        assert_eq!(named.session_title(false), "Lab / nt4 (RDP 5) - vintrdp");
        assert_eq!(named.window_title(), "Lab / nt4 - vintrdp");
    }

    #[test]
    fn server_forms() {
        assert_eq!(parse_server("host").unwrap(), ("host".into(), 3389));
        assert_eq!(parse_server("[::1]:4000").unwrap(), ("::1".into(), 4000));
        assert_eq!(parse_server("fe80::1").unwrap(), ("fe80::1".into(), 3389));
    }

    #[test]
    fn disks_and_errors() {
        let c = Config::from_args(["-r", "disk:home=/tmp,src=/usr/src", "h"]).unwrap();
        assert_eq!(c.disks.len(), 2);
        assert!(Config::from_args(["-a", "12", "h"]).is_err());
        assert!(Config::from_args(["-u"]).is_err());
        assert!(Config::from_args(Vec::<String>::new()).is_err());
    }
}
