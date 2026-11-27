//! The local clipboard and X's selections: CLIPBOARD (and PRIMARY, if
//! [`crate::Settings::primary`] says so) both ways.
//!
//! Local text: the server owns the selections, through a window of its own,
//! and answers ConvertSelection as an ICCCM owner does: TARGETS, TIMESTAMP,
//! STRING and TEXT (Latin-1), COMPOUND_TEXT, UTF8_STRING; INCR for big text.
//! X text: when a client takes a selection, the server asks it for TARGETS,
//! then for the best text it has (UTF8_STRING, COMPOUND_TEXT, STRING), INCR
//! or not, and gives it to the [`crate::Host`].

use crate::event::{self, Builder, Event};
use crate::server::*;

/// The server's own window: owner of what it offers, requestor of what it
/// fetches. InputOnly, never mapped.
pub const SELECTION_WINDOW: u32 = 0x102;

/// How long a selection must stay put, buttons up, before it's fetched.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(300);

/// Text bigger than this goes INCR, in pieces this big.
const INCR_CHUNK: usize = 64 * 1024;

#[derive(Default)]
pub struct Clip {
    /// Bridge PRIMARY too.
    pub primary: bool,
    /// The local text the server offers, and when it took the selections.
    pub text: Option<String>,
    pub time: u32,
    /// Selections a client took, to fetch in turn, once they've settled:
    /// PRIMARY is taken when the button goes down, before anything's
    /// selected, so not while a button is down, nor until a moment after.
    queue: Vec<u32>,
    settled: Option<std::time::Instant>,
    fetch: Option<Fetch>,
    /// INCR transfers out: waiting for each requestor to delete its property.
    sends: Vec<Send>,
}

struct Fetch {
    selection: u32,
    /// The target asked for: TARGETS first, then text.
    target: u32,
    /// INCR: the type, and what's come so far.
    incr: Option<(u32, Vec<u8>)>,
}

struct Send {
    requestor: u32,
    property: u32,
    kind: u32,
    data: Vec<u8>,
    sent: usize,
}

impl State {
    fn bridged(&mut self, selection: u32) -> bool {
        selection == self.atoms.intern(b"CLIPBOARD") || (self.clip.primary && selection == 1)
    }

    pub fn create_selection_window(&mut self) {
        let mut w = crate::window::Window::new(SELECTION_WINDOW, ROOT, -1, -1, 1, 1, 0);
        w.class = crate::window::INPUT_ONLY;
        w.depth = 0;
        w.override_redirect = true;
        // Not among the root's children: no client sees it in the tree or
        // the stacking order, as X.org has no such window.
        self.windows.insert(SELECTION_WINDOW, w);
    }

    /// Local text, from the front-end: the server takes the selections.
    pub fn local_clipboard(&mut self, text: String) {
        self.clip.text = Some(text);
        self.clip.time = self.now();
        let mut sels = vec![self.atoms.intern(b"CLIPBOARD")];
        if self.clip.primary {
            sels.push(1);
        }
        for sel in sels {
            self.own(sel);
        }
    }

    /// Take a selection for the server, telling the owner it's lost it.
    fn own(&mut self, selection: u32) {
        let time = self.clip.time;
        if let Some(i) = self.selections.iter().position(|s| s.atom == selection) {
            let s = self.selections.remove(i);
            if s.slot != 0 {
                let ev = Builder::new(event::SELECTION_CLEAR, 0).u32(time).u32(s.window).u32(selection).done();
                self.send(s.slot, &ev);
            }
        }
        self.selections.push(Selection { atom: selection, window: SELECTION_WINDOW, slot: 0, time });
    }

    /// A client took a selection (deliver.rs): fetch it, if it's bridged.
    pub fn selection_taken(&mut self, selection: u32, slot: u32) {
        if slot == 0 || !self.bridged(selection) {
            return;
        }
        self.clip.queue.retain(|&s| s != selection);
        self.clip.queue.push(selection);
        self.clip.settled = Some(std::time::Instant::now() + SETTLE);
    }

    /// When the server should look at the selections again.
    pub fn clip_deadline(&self) -> Option<std::time::Instant> {
        self.clip.settled.filter(|_| !self.clip.queue.is_empty() && self.clip.fetch.is_none())
    }

    /// Time passed, or input came: fetch what's settled.
    pub fn clip_tick(&mut self) {
        let Some(at) = self.clip_deadline() else { return };
        if self.input.buttons != 0 {
            self.clip.settled = Some(std::time::Instant::now() + SETTLE);
            return;
        }
        if std::time::Instant::now() >= at {
            self.clip.settled = None;
            self.next_fetch();
        }
    }

    fn next_fetch(&mut self) {
        self.clip.fetch = None;
        while !self.clip.queue.is_empty() {
            let selection = self.clip.queue.remove(0);
            let Some(owner) = self.selections.iter().find(|s| s.atom == selection && s.slot != 0) else { continue };
            let (window, slot, time) = (owner.window, owner.slot, owner.time);
            let target = self.atoms.intern(b"TARGETS");
            self.clip.fetch = Some(Fetch { selection, target, incr: None });
            self.request_selection(slot, window, selection, target, time);
            return;
        }
    }

    fn request_selection(&mut self, slot: u32, owner: u32, selection: u32, target: u32, time: u32) {
        if self.verbose {
            let name = |s: &Self, a: u32| String::from_utf8_lossy(s.atoms.name(a).unwrap_or(b"?")).into_owned();
            eprintln!("vintrdp: X11: asking client {slot} for {} as {}", name(self, selection), name(self, target));
        }
        let property = self.atoms.intern(b"VINTRDP_SELECTION");
        let ev = Builder::new(event::SELECTION_REQUEST, 0).u32(time).u32(owner).u32(SELECTION_WINDOW).u32(selection).u32(target).u32(property).done();
        self.send(slot, &ev);
    }

    /// SelectionNotify sent to the server's window: what was asked for came,
    /// or didn't.
    pub fn selection_notified(&mut self, ev: &Event) {
        let le = |at: usize| u32::from_le_bytes(ev.0[at..at + 4].try_into().unwrap());
        let (selection, target, property) = (le(12), le(16), le(20));
        let Some(f) = &self.clip.fetch else { return };
        if f.selection != selection || f.target != target {
            return;
        }
        let targets = self.atoms.intern(b"TARGETS");
        let data = if property == 0 { None } else { self.take_property(property) };
        if target == targets {
            // The best text it offers; STRING if it gave no list.
            let offered: Vec<u32> = match &data {
                Some((4, 32, d)) => d.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect(),
                _ => Vec::new(),
            };
            let want = [&b"UTF8_STRING"[..], b"COMPOUND_TEXT", b"STRING", b"TEXT"].iter().map(|n| self.atoms.intern(n)).find(|t| offered.contains(t)).unwrap_or(31);
            let Some(owner) = self.selections.iter().find(|s| s.atom == selection && s.slot != 0) else { return self.next_fetch() };
            let (window, slot, time) = (owner.window, owner.slot, owner.time);
            self.clip.fetch.as_mut().unwrap().target = want;
            self.request_selection(slot, window, selection, want, time);
            return;
        }
        match data {
            Some((kind, _, d)) if kind == self.atoms.intern(b"INCR") => {
                // In pieces: each time the owner writes the property again.
                let _ = d;
                self.clip.fetch.as_mut().unwrap().incr = Some((0, Vec::new()));
            }
            Some((kind, 8, d)) => {
                self.fetched(kind, &d);
                self.next_fetch();
            }
            _ => {
                if target != 31 {
                    // Refused: try STRING, which every owner should have.
                    if let Some(owner) = self.selections.iter().find(|s| s.atom == selection && s.slot != 0) {
                        let (window, slot, time) = (owner.window, owner.slot, owner.time);
                        self.clip.fetch.as_mut().unwrap().target = 31;
                        self.request_selection(slot, window, selection, 31, time);
                        return;
                    }
                }
                self.next_fetch();
            }
        }
    }

    /// A property changed on the server's window: the next piece of INCR.
    pub fn selection_property(&mut self, name: u32) {
        if name != self.atoms.intern(b"VINTRDP_SELECTION") || !self.clip.fetch.as_ref().is_some_and(|f| f.incr.is_some()) {
            return;
        }
        let Some((kind, _, d)) = self.take_property(name) else { return };
        let f = self.clip.fetch.as_mut().unwrap();
        let (k, buf) = f.incr.as_mut().unwrap();
        if d.is_empty() {
            let (kind, buf) = (*k, std::mem::take(buf));
            self.fetched(kind, &buf);
            self.next_fetch();
        } else {
            *k = kind;
            buf.extend_from_slice(&d);
        }
    }

    /// A property of the server's window, deleted as it's read (which tells
    /// the owner, for INCR).
    fn take_property(&mut self, name: u32) -> Option<(u32, u8, Vec<u8>)> {
        let w = self.windows.get_mut(&SELECTION_WINDOW)?;
        let i = w.properties.iter().position(|p| p.name == name)?;
        let p = w.properties.remove(i);
        self.property_notify(SELECTION_WINDOW, name, true);
        Some((p.kind, p.format, p.data))
    }

    /// Text from an X client, for the local clipboard.
    fn fetched(&mut self, kind: u32, data: &[u8]) {
        let text = if kind == self.atoms.intern(b"UTF8_STRING") {
            String::from_utf8_lossy(data).into_owned()
        } else if kind == self.atoms.intern(b"COMPOUND_TEXT") {
            from_compound_text(data)
        } else {
            data.iter().map(|&b| b as char).collect()
        };
        let text = text.trim_end_matches('\0').to_string();
        if self.verbose {
            let name = String::from_utf8_lossy(self.atoms.name(kind).unwrap_or(b"?")).into_owned();
            eprintln!("vintrdp: X11: selection text, {} characters of {name}", text.chars().count());
        }
        // Nothing selected isn't worth emptying the local clipboard for.
        if !text.is_empty() {
            self.clip.text = None;
            self.host.clipboard(&text);
        }
    }

    /// ConvertSelection of something the server owns: answer as an owner.
    pub fn convert_own(&mut self, requestor: u32, selection: u32, target: u32, property: u32, time: u32) {
        let property = if property == 0 { target } else { property };
        let text = self.clip.text.clone().unwrap_or_default();
        let a = |s: &mut Self, n: &[u8]| s.atoms.intern(n);
        let (targets, timestamp, text_a, ctext, utf8, incr) = (a(self, b"TARGETS"), a(self, b"TIMESTAMP"), a(self, b"TEXT"), a(self, b"COMPOUND_TEXT"), a(self, b"UTF8_STRING"), a(self, b"INCR"));
        let answer: Option<(u32, u8, Vec<u8>)> = if target == targets {
            let list = [targets, timestamp, text_a, 31, ctext, utf8];
            Some((4, 32, list.iter().flat_map(|t| t.to_le_bytes()).collect()))
        } else if target == timestamp {
            Some((19, 32, self.clip.time.to_le_bytes().to_vec()))
        } else if target == 31 || target == text_a {
            Some((31, 8, to_latin1(&text)))
        } else if target == ctext {
            Some((ctext, 8, to_compound_text(&text)))
        } else if target == utf8 {
            Some((utf8, 8, text.into_bytes()))
        } else {
            // MULTIPLE and the rest: refused, as the ICCCM allows.
            None
        };
        let done = match answer {
            Some((kind, format, data)) if self.windows.contains_key(&requestor) => {
                if data.len() > INCR_CHUNK {
                    self.clip.sends.retain(|s| (s.requestor, s.property) != (requestor, property));
                    self.clip.sends.push(Send { requestor, property, kind, data: data.clone(), sent: 0 });
                    self.set_property(requestor, property, incr, 32, (data.len() as u32).to_le_bytes().to_vec());
                } else {
                    self.set_property(requestor, property, kind, format, data);
                }
                property
            }
            _ => 0,
        };
        let ev = Builder::new(event::SELECTION_NOTIFY, 0).u32(time).u32(requestor).u32(selection).u32(target).u32(done).done();
        let mut ev = ev;
        ev.0[0] |= 0x80;
        if let Some(_w) = self.windows.get(&requestor) {
            let slot = requestor >> ID_BITS;
            self.send(slot, &ev);
        }
    }

    /// A property was deleted: the next INCR piece, if one's waiting.
    pub fn property_deleted(&mut self, window: u32, name: u32) {
        let Some(i) = self.clip.sends.iter().position(|s| (s.requestor, s.property) == (window, name)) else { return };
        let s = &mut self.clip.sends[i];
        let piece = s.data[s.sent..(s.sent + INCR_CHUNK).min(s.data.len())].to_vec();
        s.sent += piece.len();
        let kind = s.kind;
        if piece.is_empty() {
            self.clip.sends.remove(i);
        }
        self.set_property(window, name, kind, 8, piece);
    }

    /// Replace a property, as ChangeProperty does (data little-endian).
    pub fn set_property(&mut self, window: u32, name: u32, kind: u32, format: u8, data: Vec<u8>) {
        let Some(w) = self.windows.get_mut(&window) else { return };
        let p = crate::window::Property { name, kind, format, data };
        match w.properties.iter_mut().find(|q| q.name == name) {
            Some(q) => *q = p,
            None => w.properties.push(p),
        }
        self.property_notify(window, name, false);
    }

    /// A window went: INCR transfers to it end.
    pub fn selection_window_gone(&mut self, window: u32) {
        self.clip.sends.retain(|s| s.requestor != window);
    }
}

fn to_latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect()
}

/// COMPOUND_TEXT of Latin-1 text: ISO 8859-1 is its starting state, so the
/// bytes are Latin-1's. Other control characters than tab and newline
/// aren't allowed.
fn to_compound_text(s: &str) -> Vec<u8> {
    to_latin1(s).into_iter().filter(|&b| b == b'\t' || b == b'\n' || (b >= 0x20 && b != 0x7f && !(0x80..0xa0).contains(&b))).collect()
}

/// COMPOUND_TEXT to text: Latin-1, as it starts; designations of other
/// character sets turn their characters into '?'.
fn from_compound_text(b: &[u8]) -> String {
    let mut out = String::new();
    let (mut gl_latin, mut gr_latin) = (true, true);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            0x1b => {
                // ESC, intermediates (0x20-0x2f), a final byte.
                let start = i + 1;
                let mut j = start;
                while j < b.len() && (0x20..0x30).contains(&b[j]) {
                    j += 1;
                }
                if j < b.len() {
                    let seq = &b[start..=j];
                    match seq {
                        [b'(', b'B'] => gl_latin = true,
                        [b'(', _] | [b'$', ..] => gl_latin = false,
                        [b'-', b'A'] => gr_latin = true,
                        [b'-', _] | [b')', _] => gr_latin = false,
                        _ => {}
                    }
                }
                i = j + 1;
            }
            // Direction and extended segments: skipped.
            0x9b => {
                i += 1;
                while i < b.len() && !(0x40..0x7f).contains(&b[i]) {
                    i += 1;
                }
                i += 1;
            }
            c if c < 0x80 => {
                out.push(if gl_latin || c < 0x20 { c as char } else { '?' });
                i += 1;
            }
            c => {
                out.push(if gr_latin { c as char } else { '?' });
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_text() {
        assert_eq!(from_compound_text(b"caf\xe9\n"), "café\n");
        assert_eq!(from_compound_text(b"a\x1b-Fb\xe1c\x1b-Ad\xe9"), "ab?cdé");
        assert_eq!(to_compound_text("café ✓\u{1}"), b"caf\xe9 ?");
    }
}
