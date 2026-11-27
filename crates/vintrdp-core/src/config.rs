//! Connection parameters, and a parser for rdesktop-style command lines.

use std::path::PathBuf;


// TS_EXTENDED_INFO_PACKET performanceFlags.
pub const PERF_DISABLE_WALLPAPER: u32 = 0x01;
pub const PERF_DISABLE_FULLWINDOWDRAG: u32 = 0x02;
pub const PERF_DISABLE_MENUANIMATIONS: u32 = 0x04;
pub const PERF_DISABLE_THEMING: u32 = 0x08;
pub const PERF_DISABLE_CURSOR_SHADOW: u32 = 0x20;
pub const PERF_ENABLE_FONT_SMOOTHING: u32 = 0x80;

/// What to speak to the server: RDP, VNC (RFB) for an address `vnc://host`,
/// or be an X server for `x11://[bind][:display]` (and `xdmcp://host`, an X
/// server that asks host's display manager for a login screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    Rdp,
    Vnc,
    X11,
}

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
    pub protocol: Protocol,
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
    /// Full screen (`-f`): the session the size of the screen, in a window without a
    /// frame over everything. Only the Windows front-end has it so far.
    pub fullscreen: bool,
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
    /// Redirected printers (`-r printer:NAME[=DRIVER]`).
    pub printers: Vec<Printer>,
    /// Redirected serial ports, as ("COM1", local device) (`-r comport:`).
    pub serial_ports: Vec<(String, String)>,
    /// Redirected parallel ports, as ("LPT1", local device) (`-r lptport:`).
    pub parallel_ports: Vec<(String, String)>,
    /// Where files on their way to the server are staged. None means files
    /// can't go through the clipboard.
    pub transfer: Option<PathBuf>,
    /// A licence file the user imported, offered to every server before the
    /// per-server cache. rdesktop `.cal` format.
    pub licence: Option<PathBuf>,
    /// Play the server's sound here.
    pub sound: bool,
    /// Share text through the clipboard.
    pub clipboard: bool,
    /// If RDP 5 is refused (old server, or licensing it can't satisfy), connect
    /// with RDP 4 instead of failing. Ignored when RDP 4 was requested outright.
    pub fallback_rdp4: bool,
    pub title: Option<String>,
    pub tls_version: Option<String>,
    pub reconnect_on_network_error: bool,
    /// X11: other machines that may connect (`x11://...?allow=host,host`;
    /// `*` for any). This one always may.
    pub x11_allow: Vec<String>,
    /// X11: font folders on this machine (`?fontpath=dir,dir`).
    pub x11_font_path: Vec<String>,
    /// `xdmcp://host`: the display manager to ask for a session.
    pub xdmcp: Option<Xdmcp>,
    pub verbose: bool,
}

/// A printer to redirect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Printer {
    /// The local printer; None for the default one.
    pub name: Option<String>,
    /// The driver the server should use; None picks one (see
    /// `printing::default_driver`).
    pub driver: Option<String>,
}

/// "com1" -> "COM1": a port name the server will accept (and that fits
/// rdpdr's 8-byte device name).
fn parse_port(port: &str, prefix: &str) -> Result<String, String> {
    let upper = port.trim().to_ascii_uppercase();
    let number = upper.strip_prefix(prefix).and_then(|n| n.parse::<u16>().ok()).filter(|n| (1..=256).contains(n));
    number.map(|n| format!("{prefix}{n}")).ok_or_else(|| format!("{port} isn't a {prefix} port name, like {prefix}1"))
}

/// Serial and parallel ports as the connection manager takes them:
/// `COM1=/dev/ttyUSB0, LPT1=/dev/usb/lp0` (on Windows, `COM1=COM3`).
/// Returns (serial, parallel), each as (server port, local device).
pub type PortList = Vec<(String, String)>;

pub fn parse_ports(text: &str) -> Result<(PortList, PortList), String> {
    let (mut serial, mut parallel) = (Vec::new(), Vec::new());
    for item in text.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        let (port, device) = item.split_once('=').map(|(p, d)| (p.trim(), d.trim())).filter(|(_, d)| !d.is_empty()).ok_or_else(|| format!("Ports are written COM1=DEVICE or LPT1=DEVICE; \u{201c}{item}\u{201d} isn't."))?;
        if port.to_ascii_uppercase().starts_with("COM") {
            serial.push((parse_port(port, "COM")?, device.to_string()));
        } else {
            parallel.push((parse_port(port, "LPT")?, device.to_string()));
        }
    }
    Ok((serial, parallel))
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: String::new(),
            protocol: Protocol::Rdp,
            port: 3389,
            username: std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default(),
            password: String::new(),
            domain: String::new(),
            shell: String::new(),
            directory: String::new(),
            width: 1024,
            height: 768,
            fullscreen: false,
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
            printers: Vec::new(),
            serial_ports: Vec::new(),
            parallel_ports: Vec::new(),
            transfer: None,
            licence: None,
            sound: true,
            clipboard: true,
            fallback_rdp4: true,
            title: None,
            tls_version: None,
            reconnect_on_network_error: false,
            x11_allow: Vec::new(),
            x11_font_path: Vec::new(),
            xdmcp: None,
            verbose: false,
        }
    }
}

pub const USAGE: &str = "\
Usage: vintrdp [options] server[:port]
       vintrdp [options] vnc://server[:display | ::port]
       vintrdp [options] x11://[address][:display][?allow=host,...]
                                      be an X server (display :1 by default)
       vintrdp [options] xdmcp://host[:port][?indirect|broadcast][&display=N]
                         [&address=IP]  an X server showing host's XDM login
                                      (xdmcp://* broadcasts; address= is the one
                                      the host connects back to, behind NAT)

  -u USER        user name            -p PASS     password (- reads stdin)
  -d DOMAIN      domain               -s SHELL    program to start
  -c DIR         working directory    -n NAME     client host name
  -g WxH         session size         -a DEPTH    colour depth (8/15/16/24/32)
  -f             full screen (Windows; Ctrl+Alt+Break switches)
  -4 / -5        force RDP 4 / RDP 5 (default: RDP 5, falling back to RDP 4)
  -k LAYOUT      keyboard layout id, e.g. 0x409
  -T NAME        connection name         -0          attach to console session
  -z             compression          -x m|b|l|HEX experience (modem/broadband/LAN)
  -b             no bitmap cache      -P          persistent bitmap cache
  -e             no encryption        -E          encrypt logon packet only
  -r disk:NAME=PATH                   redirect a local folder
  -r printer[:NAME[=DRIVER]]          redirect a printer (default: the default one)
  -r comport:COM1=DEVICE              redirect a serial port (/dev/ttyUSB0, COM3)
  -r lptport:LPT1=DEVICE              redirect a parallel port (/dev/usb/lp0, LPT1)
  -N             lock sync (default)  -M          use local mouse cursor
  -V 1.0|1.1|1.2 TLS version          -R          reconnect on network errors
  -v             verbose logging      -h, --help  this help
  --version                           print the version
  --import-licence FILE               keep a TS licence file for every connection
  --control [--listen SOCKET] [options] server
                                      a session with no window, driven by text
                                      commands (--send SOCKET COMMAND...)
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
        let mut depth_given = false;
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
                    depth_given = true;
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
                'r' if value == "sound:off" => c.sound = false,
                'r' if value == "clipboard:off" => c.clipboard = false,
                'r' if value == "printer" || value.starts_with("printer:") => {
                    let spec = value.strip_prefix("printer").unwrap().trim_start_matches(':');
                    if spec.is_empty() {
                        c.printers.push(Printer { name: None, driver: None });
                    }
                    for item in spec.split(',').filter(|i| !i.is_empty()) {
                        let (name, driver) = item.split_once('=').map_or((item, None), |(n, d)| (n, Some(d.to_string())));
                        c.printers.push(Printer { name: Some(name.to_string()).filter(|n| !n.is_empty()), driver });
                    }
                }
                'r' if value.starts_with("comport:") || value.starts_with("lptport:") => {
                    let (kind, spec) = value.split_once(':').unwrap();
                    let (prefix, list) = if kind == "comport" { ("COM", &mut c.serial_ports) } else { ("LPT", &mut c.parallel_ports) };
                    for item in spec.split(',') {
                        let (port, device) = item.split_once('=').ok_or_else(|| format!("expected {kind}:{prefix}1=DEVICE, got {item}"))?;
                        list.push((parse_port(port, prefix)?, device.to_string()));
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
                'f' => c.fullscreen = true,
                'R' => c.reconnect_on_network_error = true,
                'v' => c.verbose = true,
                'K' | 'D' | 'm' | 'C' | 'B' => {} // X11 window behaviour
                'h' | '?' => return Err(USAGE.into()),
                _ => return Err(format!("unknown option -{opt}\n\n{USAGE}")),
            }
        }
        let server = server.ok_or_else(|| format!("no server given\n\n{USAGE}"))?;
        (c.protocol, c.server, c.port) = parse_address(&server)?;
        if c.protocol == Protocol::X11 {
            c.x11_allow = parse_x11_allow(&server);
            c.x11_font_path = parse_x11_font_path(&server);
            c.xdmcp = parse_xdmcp(&server)?;
        }
        // VNC servers work in full colour; RDP's 16-bit default would halve it.
        if c.protocol == Protocol::Vnc && !depth_given {
            c.depth = 32;
        }
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

    /// Window title of a VNC session: "name / host (VNC) - vintrdp".
    pub fn vnc_title(&self) -> String {
        format!("{} (VNC) - vintrdp", self.display_host())
    }

    /// Window title of an X server: "name / X display :1 - vintrdp", or
    /// "name / host (XDMCP) - vintrdp".
    pub fn x11_title(&self) -> String {
        let display = match &self.xdmcp {
            Some(x) if x.host.is_empty() => "XDMCP".to_string(),
            Some(x) => format!("{} (XDMCP)", x.host),
            None => format!("X display :{}", self.port.wrapping_sub(X11_PORT)),
        };
        match self.title.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => format!("{name} / {display} - vintrdp"),
            _ => format!("{display} - vintrdp"),
        }
    }

    /// Window title once connected: "name / host (NT4) - vintrdp" for an
    /// RDP 4 session, "(RDP 5)" otherwise.
    pub fn session_title(&self, rdp4: bool) -> String {
        format!("{} ({}) - vintrdp", self.display_host(), if rdp4 { "RDP 4" } else { "RDP 5" })
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
/// An address as typed: `host[:port]` or `rdp://host[:port]` for RDP (see
/// [`parse_server`]), `vnc://host[:display]` or `vnc://host::port` for VNC.
/// As VNC viewers have it, a number after one colon below 100 is a display
/// (port 5900 + display); 100 and up is a port.
pub fn parse_address(s: &str) -> Result<(Protocol, String, u16), String> {
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("x11://") {
        return parse_x11(s);
    }
    if lower.starts_with("xdmcp://") {
        // The server to be: everywhere (the host connects back), display
        // :N, or 0 for the first that's free.
        let display = query_list(s, "display").first().map(|d| d.parse::<u16>().ok().filter(|&d| d < 1000).ok_or_else(|| format!("bad display number in {s}"))).transpose()?;
        parse_xdmcp(s)?;
        return Ok((Protocol::X11, String::new(), display.map_or(0, |d| X11_PORT + d)));
    }
    if lower.starts_with("rdp://") {
        let (host, port) = parse_server(s[6..].trim_end_matches('/'))?;
        return Ok((Protocol::Rdp, host, port));
    }
    if !lower.starts_with("vnc://") {
        let (host, port) = parse_server(s)?;
        return Ok((Protocol::Rdp, host, port));
    }
    let rest = s[6..].trim_end_matches('/');
    // An IPv6 address goes in brackets, as in a URL.
    let (host, after) = match rest.strip_prefix('[') {
        Some(r) => r.split_once(']').ok_or_else(|| format!("unterminated [ in {s}"))?,
        None => rest.split_at(rest.find(':').unwrap_or(rest.len())),
    };
    if host.is_empty() {
        return Err("empty server name".into());
    }
    let number = |n: &str| n.parse::<u16>().map_err(|_| format!("bad port or display in {s}"));
    let port = if after.is_empty() {
        5900
    } else if let Some(p) = after.strip_prefix("::") {
        number(p)?
    } else if let Some(n) = after.strip_prefix(':') {
        match number(n)? {
            n if n < 100 => 5900 + n,
            n => n,
        }
    } else {
        return Err(format!("unexpected text after ] in {s}"));
    };
    Ok((Protocol::Vnc, host.into(), port))
}

/// X display :N is TCP port 6000 + N.
pub const X11_PORT: u16 = 6000;

/// `x11://[address][:display][?allow=...]`: where to listen. No address
/// means this machine only (or everywhere, when others are allowed: see
/// [`parse_x11_allow`]).
fn parse_x11(s: &str) -> Result<(Protocol, String, u16), String> {
    let rest = s[6..].split('?').next().unwrap_or("").trim_end_matches('/');
    let (host, after) = match rest.strip_prefix('[') {
        Some(r) => r.split_once(']').ok_or_else(|| format!("unterminated [ in {s}"))?,
        None => rest.split_at(rest.rfind(':').unwrap_or(rest.len())),
    };
    let display = match after.strip_prefix(':') {
        None if after.is_empty() => 1,
        // "x11://:1.0": the screen number is always 0.
        Some(d) => d.split('.').next().unwrap_or("").parse::<u16>().ok().filter(|&d| d < 1000).ok_or_else(|| format!("bad display number in {s}"))?,
        None => return Err(format!("unexpected text after ] in {s}")),
    };
    Ok((Protocol::X11, host.to_string(), X11_PORT + display))
}

/// The hosts after `?allow=` in an `x11://` address.
pub fn parse_x11_allow(s: &str) -> Vec<String> {
    query_list(s, "allow")
}

/// The folders after `?fontpath=` in an `x11://` or `xdmcp://` address.
pub fn parse_x11_font_path(s: &str) -> Vec<String> {
    query_list(s, "fontpath")
}

/// A query parameter's values, comma-separated: `?key=a,b&key=c`.
fn query_list(s: &str, key: &str) -> Vec<String> {
    let query = s.split_once('?').map_or("", |q| q.1);
    query
        .split('&')
        .filter_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
        .flat_map(|v| v.split(','))
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdmcpMode {
    Query,
    Broadcast,
    Indirect,
}

/// `xdmcp://host[:port]?...`: the display manager to ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Xdmcp {
    /// Empty to broadcast to every host.
    pub host: String,
    pub port: u16,
    pub mode: XdmcpMode,
    /// The address the host connects back to (`?address=`).
    pub address: Option<std::net::IpAddr>,
}

/// The XDMCP port.
pub const XDMCP_PORT: u16 = 177;

/// `xdmcp://host[:port][?indirect|broadcast][&display=N][&address=IP]`;
/// `xdmcp://*` (or no host) broadcasts. None for any other address.
pub fn parse_xdmcp(s: &str) -> Result<Option<Xdmcp>, String> {
    if !s.to_ascii_lowercase().starts_with("xdmcp://") {
        return Ok(None);
    }
    let rest = s[8..].split('?').next().unwrap_or("").trim_end_matches('/');
    let (host, port) = if rest.is_empty() || rest == "*" {
        (String::new(), XDMCP_PORT)
    } else if let Some(r) = rest.strip_prefix("*:") {
        (String::new(), r.parse().map_err(|_| format!("bad port in {s}"))?)
    } else if rest.contains(':') && !rest.starts_with('[') && rest.matches(':').count() > 1 {
        // A bare IPv6 address.
        (rest.to_string(), XDMCP_PORT)
    } else {
        let (h, p) = parse_server(rest)?;
        // parse_server's default is RDP's port.
        let given = rest.rsplit_once(':').is_some_and(|(_, p)| !p.contains(']'));
        (h, if given { p } else { XDMCP_PORT })
    };
    let query = s.split_once('?').map_or("", |q| q.1);
    let flag = |f: &str| query.split('&').any(|kv| kv.eq_ignore_ascii_case(f));
    let mode = if host.is_empty() || flag("broadcast") {
        XdmcpMode::Broadcast
    } else if flag("indirect") {
        XdmcpMode::Indirect
    } else {
        XdmcpMode::Query
    };
    let address = match query_list(s, "address").first() {
        Some(a) => Some(a.trim_matches(['[', ']']).parse().map_err(|_| format!("bad address {a} in {s}: give an IP address"))?),
        None => None,
    };
    let host = if mode == XdmcpMode::Broadcast && host == "255.255.255.255" { String::new() } else { host };
    Ok(Some(Xdmcp { host, port, mode, address }))
}

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
        assert_eq!(c.session_title(true), "nt4 (RDP 4) - vintrdp");
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
    #[test]
    fn fullscreen() {
        assert!(Config::from_args(["-f", "h"]).unwrap().fullscreen);
        assert!(!Config::from_args(["h"]).unwrap().fullscreen);
    }
    #[test]
    fn addresses() {
        let a = |s: &str| parse_address(s).unwrap();
        assert_eq!(a("nt4"), (Protocol::Rdp, "nt4".into(), 3389));
        assert_eq!(a("rdp://nt4:3390/"), (Protocol::Rdp, "nt4".into(), 3390));
        assert_eq!(a("vnc://pi"), (Protocol::Vnc, "pi".into(), 5900));
        assert_eq!(a("VNC://pi:1"), (Protocol::Vnc, "pi".into(), 5901));
        assert_eq!(a("vnc://pi:5902"), (Protocol::Vnc, "pi".into(), 5902));
        assert_eq!(a("vnc://pi::99"), (Protocol::Vnc, "pi".into(), 99));
        assert_eq!(a("vnc://pi:3389"), (Protocol::Vnc, "pi".into(), 3389));
        assert_eq!(a("vnc://[::1]:2"), (Protocol::Vnc, "::1".into(), 5902));
        assert_eq!(a("vnc://[fe80::1]::5999"), (Protocol::Vnc, "fe80::1".into(), 5999));
        assert!(parse_address("vnc://").is_err());
        assert_eq!(a("x11://"), (Protocol::X11, "".into(), 6001));
        assert_eq!(a("x11://:0"), (Protocol::X11, "".into(), 6000));
        assert_eq!(a("X11://0.0.0.0:2.0"), (Protocol::X11, "0.0.0.0".into(), 6002));
        assert_eq!(a("x11://[::]:3?allow=m900"), (Protocol::X11, "::".into(), 6003));
        assert_eq!(a("x11://localhost"), (Protocol::X11, "localhost".into(), 6001));
        assert!(parse_address("x11://:x").is_err());
        let c = Config::from_args(["x11://:4?allow=m900,10.0.0.5&x=y"]).unwrap();
        assert_eq!((c.protocol, c.port, c.x11_allow.clone()), (Protocol::X11, 6004, vec!["m900".to_string(), "10.0.0.5".to_string()]));
        assert_eq!(c.x11_title(), "X display :4 - vintrdp");
        assert_eq!(a("xdmcp://aix43"), (Protocol::X11, "".into(), 0));
        assert_eq!(a("xdmcp://aix43?display=3"), (Protocol::X11, "".into(), 6003));
        let x = |s: &str| parse_xdmcp(s).unwrap().unwrap();
        assert_eq!(x("xdmcp://192.168.99.153:17702"), Xdmcp { host: "192.168.99.153".into(), port: 17702, mode: XdmcpMode::Query, address: None });
        assert_eq!(x("XDMCP://m900?indirect&address=10.0.0.2"), Xdmcp { host: "m900".into(), port: 177, mode: XdmcpMode::Indirect, address: Some("10.0.0.2".parse().unwrap()) });
        assert_eq!(x("xdmcp://*").mode, XdmcpMode::Broadcast);
        assert_eq!(x("xdmcp://10.0.0.255?broadcast"), Xdmcp { host: "10.0.0.255".into(), port: 177, mode: XdmcpMode::Broadcast, address: None });
        assert_eq!(x("xdmcp://[fe80::1]:1777").port, 1777);
        assert!(parse_xdmcp("xdmcp://h?address=nowhere").is_err());
        assert!(parse_xdmcp("x11://:1").unwrap().is_none());
        let c = Config::from_args(["xdmcp://aix43:17702?allow=m900"]).unwrap();
        assert_eq!((c.x11_title(), c.x11_allow.clone(), c.xdmcp.as_ref().unwrap().port), ("aix43 (XDMCP) - vintrdp".to_string(), vec!["m900".to_string()], 17702));
        let c = Config::from_args(["-p", "x", "vnc://pi:1"]).unwrap();
        assert_eq!((c.protocol, c.port, c.depth), (Protocol::Vnc, 5901, 32));
        assert_eq!(Config::from_args(["-a", "8", "vnc://pi"]).unwrap().depth, 8);
        assert_eq!(Config::from_args(["pi"]).unwrap().depth, 16);
    }
    #[test]
    fn devices() {
        let c = Config::from_args(["-r", "printer", "-r", "printer:Office Laser=HP LaserJet 4,Home", "-r", "comport:com1=/dev/ttyUSB0", "-r", "lptport:LPT2=/dev/usb/lp0", "h"]).unwrap();
        assert_eq!(c.printers.len(), 3);
        assert_eq!((c.printers[0].name.as_deref(), c.printers[0].driver.as_deref()), (None, None));
        assert_eq!((c.printers[1].name.as_deref(), c.printers[1].driver.as_deref()), (Some("Office Laser"), Some("HP LaserJet 4")));
        assert_eq!(c.printers[2].name.as_deref(), Some("Home"));
        assert_eq!(c.serial_ports, [("COM1".to_string(), "/dev/ttyUSB0".to_string())]);
        assert_eq!(c.parallel_ports, [("LPT2".to_string(), "/dev/usb/lp0".to_string())]);
        assert!(Config::from_args(["-r", "comport:COM1", "h"]).is_err());
        assert!(Config::from_args(["-r", "comport:LPT1=/dev/x", "h"]).is_err());
        assert!(Config::from_args(["-r", "comport:COM12345=/dev/x", "h"]).is_err());
        let (serial, parallel) = parse_ports(" com1 = /dev/ttyUSB0, LPT2=/dev/usb/lp0 ,").unwrap();
        assert_eq!(serial, [("COM1".to_string(), "/dev/ttyUSB0".to_string())]);
        assert_eq!(parallel, [("LPT2".to_string(), "/dev/usb/lp0".to_string())]);
        assert_eq!(parse_ports("").unwrap(), (vec![], vec![]));
        assert!(parse_ports("COM1").is_err());
        assert!(parse_ports("PRN=/dev/x").is_err());
    }
}

