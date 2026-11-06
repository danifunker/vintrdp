//! `vintrdp --control`: a session with no window, driven by text commands, for
//! scripts and automation.
//!
//!   vintrdp --control [--trust] [--sound] [rdesktop options] host
//!   vintrdp --control --listen SOCKET [...] host     (macOS, Linux, BSD)
//!   vintrdp --send SOCKET 'command' ['command'...]
//!
//! Commands come one per line, on stdin or over the socket, and each gets one
//! reply line: `ok`, optionally followed by a result, or `error` and a reason.
//! Text results (the clipboard) are JSON strings. Log messages go to stderr.
//! See HELP for the commands.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use vintrdp_core::scancode::{self, Scancode};
use vintrdp_core::{Config, Event, Frontend, MouseButton, Outcome, Session};

use crate::launch;

pub const HELP: &str = "\
Commands (one per line; replies are `ok [result]` or `error reason`):
  wait-idle [MS] [SECS]   wait until the screen hasn't changed for MS ms
                          (default 1000), giving up after SECS (default 30)
  wait SECS               just wait
  shot FILE [X Y W H]     save the screen, or part of it, as a PNG
  size                    the session's size, WxH
  type TEXT               type TEXT exactly as written (any characters)
  typeln TEXT             type TEXT, then Enter
  key COMBO...            press keys: Return, Tab, Escape, F4, ctrl+c,
                          alt+F4, win+r, ctrl+alt+Delete, shift+Tab, ...
  click X Y [right|middle]
  dclick X Y              double-click
  move X Y
  drag X1 Y1 X2 Y2
  scroll X Y up|down [N]
  clip                    the server's clipboard text
  clip-set TEXT           put TEXT on the server's clipboard
  paste-files PATH...     put local files on the server's clipboard (Server
                          2003); then `key ctrl+v` in a folder
  files                   files copied on the server, as local paths
  status                  connected, or how it ended
  quit                    disconnect (with --listen, also stops listening)
  help                    this list
Lines starting with # are ignored.";

/// What the session has told us, shared with the command loop.
#[derive(Default)]
struct State {
    connected: bool,
    updates: u64,
    last_update: Option<Instant>,
    clipboard: String,
    files: Vec<PathBuf>,
    ended: Option<Outcome>,
}

struct Control {
    state: Mutex<State>,
    changed: Condvar,
    trust: bool,
}

impl Frontend for Control {
    fn notify(&self, event: Event) {
        let mut s = self.state.lock().unwrap();
        match event {
            Event::Resized { width, height } => {
                s.connected = true;
                eprintln!("vintrdp: screen {width}x{height}");
            }
            Event::Updated => {
                s.updates += 1;
                s.last_update = Some(Instant::now());
            }
            Event::Cursor | Event::Bell => {}
            Event::Title(t) => eprintln!("vintrdp: title {t}"),
            Event::Message(m) => eprintln!("vintrdp: {m}"),
            Event::Clipboard(text) => s.clipboard = text,
            Event::ClipboardFiles(files) => s.files = files,
            Event::Ended(outcome) => {
                eprintln!("vintrdp: session ended ({}) {}", outcome.code, outcome.message);
                s.ended = Some(outcome);
            }
        }
        drop(s);
        self.changed.notify_all();
    }

    fn screen_size(&self) -> (u32, u32) {
        (1920, 1080)
    }

    fn ask(&self, title: &str, message: &str, accept: &str, _reject: &str) -> bool {
        eprintln!("vintrdp: {title}\n{message}");
        if self.trust {
            eprintln!("vintrdp: answering \"{accept}\" (--trust)");
        } else {
            eprintln!("vintrdp: refusing; pass --trust to accept");
        }
        self.trust
    }
}

/// The parts of `vintrdp --control ...` that aren't rdesktop options.
struct Options {
    trust: bool,
    sound: bool,
    listen: Option<PathBuf>,
}

fn take_options(args: &mut Vec<String>) -> Result<Options, String> {
    let mut flag = |name: &str| args.iter().position(|a| a == name).map(|i| args.remove(i)).is_some();
    let (trust, sound) = (flag("--trust"), flag("--sound"));
    let listen = match args.iter().position(|a| a == "--listen") {
        Some(i) if i + 1 < args.len() => {
            args.remove(i);
            Some(PathBuf::from(args.remove(i)))
        }
        Some(_) => return Err("--listen needs a socket path".into()),
        None => None,
    };
    Ok(Options { trust, sound, listen })
}

/// `vintrdp --control ...`: returns the exit code.
pub fn run(mut args: Vec<String>) -> i32 {
    let options = match take_options(&mut args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let mut config = match Config::from_args(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}\n\n{HELP}");
            return 2;
        }
    };
    // A transfer drive of our own, so a session in the app isn't disturbed.
    let transfer = std::env::temp_dir().join(format!("vintrdp-control-{}", std::process::id()));
    config.transfer = config.clipboard.then(|| transfer.clone());
    config.licence = Some(launch::imported_licence());
    config.sound &= options.sound;

    let control = Arc::new(Control { state: Mutex::new(State::default()), changed: Condvar::new(), trust: options.trust });
    let session = match Session::start(config, control.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vintrdp: could not start the session: {e}");
            return 1;
        }
    };
    // Predictable typing: Caps Lock off, Num Lock on.
    session.sync_locks(false, true, false);

    let code = match options.listen {
        #[cfg(unix)]
        Some(path) => listen(&path, &session, &control),
        #[cfg(not(unix))]
        Some(_) => {
            eprintln!("--listen needs Unix domain sockets; use stdin instead");
            2
        }
        None => {
            let stdin = std::io::stdin();
            let mut out = std::io::stdout();
            serve(stdin.lock(), &mut out, &session, &control);
            finish(&session, &control)
        }
    };
    let _ = std::fs::remove_dir_all(&transfer);
    code
}

/// Disconnect if still connected; the exit code is the session's.
fn finish(session: &Session, control: &Control) -> i32 {
    if control.state.lock().unwrap().ended.is_none() {
        session.quit();
    }
    session.wait(Duration::from_secs(5)).map_or(0, |o| if o.user_quit { 0 } else { o.code })
}

/// Answer commands from `input` until it ends, `quit`, or the session ends.
/// True if it was `quit`.
fn serve(input: impl BufRead, out: &mut impl Write, session: &Session, control: &Control) -> bool {
    for line in input.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "quit" {
            let _ = writeln!(out, "ok");
            let _ = out.flush();
            return true;
        }
        let reply = match command(line, session, control) {
            Ok(result) if result.is_empty() => "ok".to_string(),
            Ok(result) => format!("ok {result}"),
            Err(e) => format!("error {}", e.replace('\n', " ")),
        };
        if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
            break;
        }
        if control.state.lock().unwrap().ended.is_some() {
            break;
        }
    }
    false
}

#[cfg(unix)]
fn listen(path: &std::path::Path, session: &Session, control: &Control) -> i32 {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    let _ = std::fs::remove_file(path);
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            // Socket paths are limited to about 100 bytes.
            let hint = if path.as_os_str().len() > 100 { " (use a shorter path, or a relative one)" } else { "" };
            eprintln!("vintrdp: can't listen on {}: {e}{hint}", path.display());
            session.quit();
            return 1;
        }
    };
    // Only this user may connect: the socket is control of the session.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    eprintln!("vintrdp: listening on {}", path.display());
    // Stop accepting once the session is over.
    let _ = listener.set_nonblocking(true);
    loop {
        if control.state.lock().unwrap().ended.is_some() {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let Ok(reader) = stream.try_clone() else { continue };
                let mut writer = stream;
                if serve(std::io::BufReader::new(reader), &mut writer, session, control) {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                eprintln!("vintrdp: {e}");
                break;
            }
        }
    }
    let _ = std::fs::remove_file(path);
    finish(session, control)
}

/// `vintrdp --send SOCKET [command...]`: commands from the arguments, or
/// stdin if there are none. Prints the replies; exit code 1 if any failed.
pub fn send(args: Vec<String>) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let Some((path, commands)) = args.split_first() else {
            eprintln!("usage: vintrdp --send SOCKET [command...]");
            return 2;
        };
        let stream = match UnixStream::connect(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("vintrdp: can't connect to {path}: {e}");
                return 1;
            }
        };
        let commands: Vec<String> = if commands.is_empty() {
            std::io::stdin().lock().lines().map_while(Result::ok).collect()
        } else {
            commands.to_vec()
        };
        let mut writer = &stream;
        let mut reader = std::io::BufReader::new(&stream);
        let mut failed = false;
        for c in commands.iter().map(|c| c.trim()).filter(|c| !c.is_empty() && !c.starts_with('#')) {
            if writeln!(writer, "{c}").is_err() {
                eprintln!("vintrdp: the session has gone");
                return 1;
            }
            let mut reply = String::new();
            if reader.read_line(&mut reply).unwrap_or(0) == 0 {
                eprintln!("vintrdp: the session has gone");
                return 1;
            }
            print!("{reply}");
            failed |= reply.starts_with("error");
        }
        i32::from(failed)
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        eprintln!("--send needs Unix domain sockets");
        2
    }
}

// ---------------------------------------------------------------------------
// Commands

fn command(line: &str, session: &Session, control: &Control) -> Result<String, String> {
    let (name, rest) = line.split_once(char::is_whitespace).map_or((line, ""), |(n, r)| (n, r.trim_start()));
    let words: Vec<&str> = rest.split_whitespace().collect();
    let num = |i: usize, what: &str| -> Result<u16, String> {
        words.get(i).and_then(|w| w.parse().ok()).ok_or_else(|| format!("{name}: expected {what}"))
    };
    if let Some(o) = &control.state.lock().unwrap().ended {
        if !matches!(name, "status" | "help" | "clip" | "files") {
            return Err(format!("session ended ({}) {}", o.code, o.message));
        }
    }
    match name {
        "help" => Ok(serde_json::to_string(HELP).unwrap()),
        "status" => {
            let s = control.state.lock().unwrap();
            Ok(match (&s.ended, s.connected) {
                (Some(o), _) => format!("ended {} {}", o.code, o.message),
                (None, true) => "connected".to_string(),
                (None, false) => "connecting".to_string(),
            })
        }
        "wait" => {
            let secs: f64 = words.first().and_then(|w| w.parse().ok()).ok_or("wait: expected seconds")?;
            std::thread::sleep(Duration::from_secs_f64(secs.clamp(0.0, 3600.0)));
            Ok(String::new())
        }
        "wait-idle" => wait_idle(control, words.first().and_then(|w| w.parse().ok()).unwrap_or(1000), words.get(1).and_then(|w| w.parse().ok()).unwrap_or(30.0)),
        "size" => session.with_framebuffer(|w, h, _| format!("{w}x{h}")).ok_or_else(|| "not connected yet".to_string()),
        "shot" => {
            let file = words.first().ok_or("shot: expected a file name")?;
            let region = if words.len() >= 5 { Some((num(1, "X")?, num(2, "Y")?, num(3, "W")?, num(4, "H")?)) } else { None };
            shot(session, file, region)
        }
        "type" => {
            type_text(session, rest);
            Ok(String::new())
        }
        "typeln" => {
            type_text(session, rest);
            press(session, &[], (0x1c, false));
            Ok(String::new())
        }
        "key" => {
            if words.is_empty() {
                return Err("key: expected key names, like Return or ctrl+c".into());
            }
            for combo in &words {
                let (mods, key) = parse_combo(combo)?;
                press(session, &mods, key);
            }
            Ok(String::new())
        }
        "click" | "dclick" => {
            let (x, y) = (num(0, "X")?, num(1, "Y")?);
            let button = match words.get(2).copied() {
                None | Some("left") => MouseButton::Left,
                Some("right") => MouseButton::Right,
                Some("middle") => MouseButton::Middle,
                Some(b) => return Err(format!("{name}: unknown button {b}")),
            };
            session.mouse_move(x, y);
            for _ in 0..if name == "dclick" { 2 } else { 1 } {
                session.mouse_button(button, true, x, y);
                session.mouse_button(button, false, x, y);
            }
            Ok(String::new())
        }
        "move" => {
            session.mouse_move(num(0, "X")?, num(1, "Y")?);
            Ok(String::new())
        }
        "drag" => {
            let (x1, y1, x2, y2) = (num(0, "X1")?, num(1, "Y1")?, num(2, "X2")?, num(3, "Y2")?);
            session.mouse_move(x1, y1);
            session.mouse_button(MouseButton::Left, true, x1, y1);
            for i in 1..=10 {
                let at = |a: u16, b: u16| (a as i32 + (b as i32 - a as i32) * i / 10) as u16;
                std::thread::sleep(Duration::from_millis(20));
                session.mouse_move(at(x1, x2), at(y1, y2));
            }
            session.mouse_button(MouseButton::Left, false, x2, y2);
            Ok(String::new())
        }
        "scroll" => {
            let (x, y) = (num(0, "X")?, num(1, "Y")?);
            let up = match words.get(2).copied() {
                Some("up") => true,
                Some("down") => false,
                _ => return Err("scroll: expected up or down".into()),
            };
            let n: u16 = words.get(3).and_then(|w| w.parse().ok()).unwrap_or(1);
            session.mouse_move(x, y);
            for _ in 0..n.min(100) {
                session.wheel(up, x, y);
            }
            Ok(String::new())
        }
        "clip" => Ok(serde_json::to_string(&control.state.lock().unwrap().clipboard).unwrap()),
        "clip-set" => {
            session.set_clipboard(rest);
            Ok(String::new())
        }
        "paste-files" => {
            if !session.can_send_files() {
                return Err("this server can't take files through the clipboard (Server 2003 can)".into());
            }
            let paths: Vec<PathBuf> = words.iter().map(PathBuf::from).collect();
            if paths.is_empty() {
                return Err("paste-files: expected local paths".into());
            }
            let count = launch::stage_files(session, &paths).map_err(|e| e.to_string())?;
            Ok(count.to_string())
        }
        "files" => {
            let files = control.state.lock().unwrap().files.clone();
            Ok(serde_json::to_string(&files).unwrap())
        }
        _ => Err(format!("unknown command {name}; try help")),
    }
}

fn wait_idle(control: &Control, quiet_ms: u64, timeout_secs: f64) -> Result<String, String> {
    let start = Instant::now();
    let deadline = start + Duration::from_secs_f64(timeout_secs.clamp(0.0, 3600.0));
    let quiet = Duration::from_millis(quiet_ms);
    let mut s = control.state.lock().unwrap();
    loop {
        if let Some(o) = &s.ended {
            return Err(format!("session ended ({}) {}", o.code, o.message));
        }
        // Quiet since the later of the last change and the start of the wait,
        // so a wait straight after a keystroke sees the server react to it.
        let now = Instant::now();
        let since = now - s.last_update.map_or(start, |t| t.max(start));
        if s.connected && since >= quiet {
            return Ok(format!("{:.1}s", start.elapsed().as_secs_f64()));
        }
        if now >= deadline {
            return Err(if s.connected { "the screen kept changing".into() } else { "not connected yet".into() });
        }
        let wake = (deadline - now).min(Duration::from_millis(100));
        s = control.changed.wait_timeout(s, wake).unwrap().0;
    }
}

fn shot(session: &Session, file: &str, region: Option<(u16, u16, u16, u16)>) -> Result<String, String> {
    let (w, h, rgb) = session
        .with_framebuffer(|w, h, px| {
            let (x0, y0, cw, ch) = region.map_or((0, 0, w, h), |(x, y, cw, ch)| (x as usize, y as usize, cw as usize, ch as usize));
            let (x0, y0) = (x0.min(w), y0.min(h));
            let (cw, ch) = (cw.min(w - x0), ch.min(h - y0));
            let mut rgb = Vec::with_capacity(cw * ch * 3);
            for y in y0..y0 + ch {
                for &p in &px[y * w + x0..y * w + x0 + cw] {
                    rgb.extend([(p >> 16) as u8, (p >> 8) as u8, p as u8]);
                }
            }
            (cw, ch, rgb)
        })
        .ok_or("not connected yet")?;
    if w == 0 || h == 0 {
        return Err("empty region".into());
    }
    let out = std::fs::File::create(file).map_err(|e| format!("{file}: {e}"))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(out), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().and_then(|mut wr| wr.write_image_data(&rgb)).map_err(|e| e.to_string())?;
    Ok(format!("{w}x{h}"))
}

fn type_text(session: &Session, text: &str) {
    for c in text.chars() {
        match c {
            '\n' => press(session, &[], (0x1c, false)),
            '\t' => press(session, &[], (0x0f, false)),
            _ => match scancode::from_us_char(c) {
                Some((sc, shift)) => press(session, if shift { &[scancode::LSHIFT] } else { &[] }, sc),
                None => session.type_char(c),
            },
        }
        // Old servers drop keys that arrive in one burst.
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Hold `mods`, tap `key`, let go: spaced out like a real keyboard, since
/// Windows can miss a shortcut whose keys all arrive at once.
fn press(session: &Session, mods: &[Scancode], key: Scancode) {
    let pause = || std::thread::sleep(Duration::from_millis(if mods.is_empty() { 0 } else { 30 }));
    for m in mods {
        session.key(m.0, m.1, true);
        pause();
    }
    session.key(key.0, key.1, true);
    pause();
    session.key(key.0, key.1, false);
    pause();
    for m in mods.iter().rev() {
        session.key(m.0, m.1, false);
    }
}

/// "ctrl+alt+Delete" -> ([LCTRL, LALT], Delete).
fn parse_combo(combo: &str) -> Result<(Vec<Scancode>, Scancode), String> {
    let parts: Vec<&str> = combo.split('+').collect();
    // "ctrl++" is ctrl and the + key.
    let (mod_names, key_name) = match parts.as_slice() {
        [.., "", ""] => (&parts[..parts.len() - 2], "+"),
        [mods @ .., key] => (mods, *key),
        [] => return Err("empty key".into()),
    };
    let mut mods = Vec::new();
    for m in mod_names {
        mods.push(match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => scancode::LCTRL,
            "shift" => scancode::LSHIFT,
            "alt" => scancode::LALT,
            "win" | "super" | "meta" | "cmd" => scancode::LWIN,
            other => return Err(format!("unknown modifier {other} in {combo}")),
        });
    }
    let key = key_scancode(key_name).ok_or_else(|| format!("unknown key {key_name}"))?;
    if key.1 && !mods.contains(&scancode::LSHIFT) && key_name.chars().count() == 1 {
        // A character that needs Shift, like "?".
        mods.push(scancode::LSHIFT);
    }
    Ok((mods, key.0))
}

/// A key by name, and whether (as a character) it needs Shift.
fn key_scancode(name: &str) -> Option<(Scancode, bool)> {
    let ext = |c: u8| Some(((c, true), false));
    let plain = |c: u8| Some(((c, false), false));
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        // Letters by their key, so ctrl+C is ctrl+c.
        return scancode::from_us_char(c.to_ascii_lowercase()).map(|(sc, shift)| (sc, shift && !c.is_ascii_alphabetic()));
    }
    let lower = name.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return match n {
            1..=10 => plain(0x3a + n),
            11 => plain(0x57),
            12 => plain(0x58),
            _ => None,
        };
    }
    match lower.as_str() {
        "return" | "enter" => plain(0x1c),
        "tab" => plain(0x0f),
        "escape" | "esc" => plain(0x01),
        "backspace" => plain(0x0e),
        "space" => plain(0x39),
        "capslock" => plain(0x3a),
        "numlock" => plain(0x45),
        "scrolllock" => plain(0x46),
        "delete" | "del" => ext(0x53),
        "insert" | "ins" => ext(0x52),
        "home" => ext(0x47),
        "end" => ext(0x4f),
        "pageup" | "pgup" | "prior" => ext(0x49),
        "pagedown" | "pgdn" | "next" => ext(0x51),
        "up" => ext(0x48),
        "down" => ext(0x50),
        "left" => ext(0x4b),
        "right" => ext(0x4d),
        "win" | "super" => ext(0x5b),
        "menu" | "apps" => ext(0x5d),
        "printscreen" | "print" => ext(0x37),
        "ctrl" | "control" => plain(0x1d),
        "shift" => plain(0x2a),
        "alt" => plain(0x38),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_combos() {
        assert_eq!(parse_combo("ctrl+alt+Delete").unwrap(), (vec![scancode::LCTRL, scancode::LALT], (0x53, true)));
        assert_eq!(parse_combo("Return").unwrap(), (vec![], (0x1c, false)));
        assert_eq!(parse_combo("win+r").unwrap().0, vec![scancode::LWIN]);
        assert_eq!(parse_combo("ctrl+C").unwrap(), parse_combo("ctrl+c").unwrap());
        assert_eq!(parse_combo("F12").unwrap().1, (0x58, false));
        assert_eq!(parse_combo("?").unwrap().0, vec![scancode::LSHIFT]);
        assert_eq!(parse_combo("ctrl++").unwrap().1, scancode::from_us_char('+').unwrap().0);
        assert!(parse_combo("hyper+x").is_err());
        assert!(parse_combo("F13").is_err());
    }
}
