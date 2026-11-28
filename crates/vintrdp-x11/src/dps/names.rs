//! User names: a DPS client numbers the names it uses often
//! (`index /name defineusername`) and then sends the numbers, in binary
//! object sequences (a name object of length 0) and binary tokens (147,
//! 148). Ghostscript has no user name table, so the numbers are turned
//! back into the names here, on the way to it: a lexer that follows the
//! input across GiveInput requests (strings, comments, binary tokens), and
//! learns each defineusername as it goes by.

use std::collections::HashMap;

#[derive(Default)]
pub struct Names {
    names: HashMap<u32, Vec<u8>>,
    /// Text being scanned.
    state: State,
    /// An unfinished binary token or sequence, waiting for the rest.
    held: Vec<u8>,
    /// The last two tokens, to see `index /name defineusername`.
    last: [Token; 2],
    /// A text token being read (a number or a name).
    word: Vec<u8>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Text,
    /// In a (string), this deep.
    Str(u32),
    StrEscape(u32),
    Hex,
    Ascii85,
    Comment,
}

#[derive(Default, Clone, PartialEq, Eq, Debug)]
enum Token {
    #[default]
    Other,
    Int(i64),
    Literal(Vec<u8>),
}

/// A binary object sequence's or token's length, if all of its header is there.
fn binary_len(b: &[u8]) -> Option<Result<usize, ()>> {
    let t = *b.first()?;
    let hi = |x: u8| x & 1 == 0;
    Some(Ok(match t {
        128..=131 => {
            let big = t == 128 || t == 130;
            let u16_at = |i: usize| -> Option<usize> {
                let v = [*b.get(i)?, *b.get(i + 1)?];
                Some(if big { u16::from_be_bytes(v) } else { u16::from_le_bytes(v) } as usize)
            };
            if *b.get(1)? == 0 {
                let v = [*b.get(4)?, *b.get(5)?, *b.get(6)?, *b.get(7)?];
                (if big { u32::from_be_bytes(v) } else { u32::from_le_bytes(v) }) as usize
            } else {
                u16_at(2)?
            }
        }
        132 | 133 | 138 | 139 | 140 => 5,
        134 | 135 => 3,
        136 | 141 | 145..=148 => 2,
        137 => {
            let r = *b.get(1)? & 0x7f;
            2 + if r < 32 { 4 } else { 2 }
        }
        142 => 2 + *b.get(1)? as usize,
        143 | 144 => {
            let v = [*b.get(1)?, *b.get(2)?];
            3 + if hi(t) { u16::from_be_bytes(v) } else { u16::from_le_bytes(v) } as usize
        }
        149 => {
            let r = *b.get(1)?;
            let v = [*b.get(2)?, *b.get(3)?];
            let n = if r < 128 { u16::from_be_bytes(v) } else { u16::from_le_bytes(v) } as usize;
            let size = if (32..48).contains(&(r & 0x7f)) { 2 } else { 4 };
            4 + n * size
        }
        _ => return Some(Err(())),
    }))
}

fn is_delim(c: u8) -> bool {
    matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | 0)
}

impl Names {
    /// The input, with user names spelt out.
    pub fn rewrite(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            if !self.held.is_empty() {
                // Finish a binary token or sequence.
                self.held.push(data[i]);
                i += 1;
                match binary_len(&self.held) {
                    None => continue,
                    Some(Err(())) => {
                        out.append(&mut self.held);
                    }
                    Some(Ok(n)) if self.held.len() >= n => {
                        let b = std::mem::take(&mut self.held);
                        self.binary(&b, &mut out);
                    }
                    Some(Ok(_)) => {}
                }
                continue;
            }
            let c = data[i];
            match self.state {
                State::Text => {
                    if c >= 128 && c <= 149 {
                        self.end_word();
                        // Take the whole of it at once if it's here.
                        self.held.push(c);
                        i += 1;
                        if let Some(Ok(n)) = binary_len(&data[i - 1..]) {
                            if data.len() - (i - 1) >= n {
                                self.held.clear();
                                let b = data[i - 1..i - 1 + n].to_vec();
                                self.binary(&b, &mut out);
                                i += n - 1;
                            }
                        }
                        continue;
                    }
                    if is_space(c) || is_delim(c) {
                        self.end_word();
                        match c {
                            b'(' => self.state = State::Str(1),
                            b'%' => self.state = State::Comment,
                            b'<' => {
                                if data.get(i + 1) == Some(&b'~') {
                                    self.state = State::Ascii85;
                                } else if data.get(i + 1) != Some(&b'<') {
                                    self.state = State::Hex;
                                }
                            }
                            b'/' => self.word.push(b'/'),
                            _ => {}
                        }
                        if !matches!(c, b'/') && !is_space(c) {
                            self.push(Token::Other);
                        }
                    } else {
                        self.word.push(c);
                    }
                }
                State::Str(n) => match c {
                    b'\\' => self.state = State::StrEscape(n),
                    b'(' => self.state = State::Str(n + 1),
                    b')' if n == 1 => {
                        self.state = State::Text;
                        self.push(Token::Other);
                    }
                    b')' => self.state = State::Str(n - 1),
                    _ => {}
                },
                State::StrEscape(n) => self.state = State::Str(n),
                State::Hex => {
                    if c == b'>' {
                        self.state = State::Text;
                        self.push(Token::Other);
                    }
                }
                State::Ascii85 => {
                    if c == b'>' && out.last() == Some(&b'~') {
                        self.state = State::Text;
                        self.push(Token::Other);
                    }
                }
                State::Comment => {
                    if c == b'\n' || c == b'\r' {
                        self.state = State::Text;
                    }
                }
            }
            out.push(c);
            i += 1;
        }
        out
    }

    /// A text word ended: a number, a literal name, or an executable name.
    fn end_word(&mut self) {
        if self.word.is_empty() {
            return;
        }
        let w = std::mem::take(&mut self.word);
        if let Some(name) = w.strip_prefix(b"/") {
            self.push(Token::Literal(name.to_vec()));
        } else if let Some(n) = std::str::from_utf8(&w).ok().and_then(|s| s.parse::<i64>().ok()) {
            self.push(Token::Int(n));
        } else {
            self.exec(&w);
        }
    }

    fn push(&mut self, t: Token) {
        self.last = [self.last[1].clone(), t];
    }

    /// An executable name: defineusername learns from the two before it.
    fn exec(&mut self, name: &[u8]) {
        if name == b"defineusername" {
            if let [Token::Int(i), Token::Literal(n)] = &self.last {
                if *i >= 0 {
                    self.names.insert(*i as u32, n.clone());
                }
            }
        }
        self.push(Token::Other);
    }

    /// A binary token or object sequence, whole.
    fn binary(&mut self, b: &[u8], out: &mut Vec<u8>) {
        match b[0] {
            128..=131 => self.sequence(b, out),
            136 => {
                out.extend_from_slice(b);
                self.push(Token::Int(b[1] as i8 as i64));
            }
            132..=135 => {
                out.extend_from_slice(b);
                let v = match b[0] {
                    132 => i32::from_be_bytes([b[1], b[2], b[3], b[4]]) as i64,
                    133 => i32::from_le_bytes([b[1], b[2], b[3], b[4]]) as i64,
                    134 => i16::from_be_bytes([b[1], b[2]]) as i64,
                    _ => i16::from_le_bytes([b[1], b[2]]) as i64,
                };
                self.push(Token::Int(v));
            }
            147 | 148 => {
                // A user name: spelt out, as a text name between spaces.
                match self.names.get(&(b[1] as u32)) {
                    Some(n) => {
                        out.push(b' ');
                        if b[0] == 147 {
                            out.push(b'/');
                        }
                        out.extend_from_slice(n);
                        out.push(b' ');
                        if b[0] == 147 {
                            self.push(Token::Literal(n.clone()));
                        } else {
                            let n = n.clone();
                            self.exec(&n);
                        }
                    }
                    None => {
                        out.extend_from_slice(b);
                        self.push(Token::Other);
                    }
                }
            }
            _ => {
                out.extend_from_slice(b);
                self.push(Token::Other);
            }
        }
    }

    /// A binary object sequence: user name objects get their text, added
    /// at the end; `index /name defineusername` in it is learnt.
    fn sequence(&mut self, b: &[u8], out: &mut Vec<u8>) {
        let big = b[0] == 128 || b[0] == 130;
        let (head, top) = if b[1] == 0 { (8, u16_at(b, 2, big) as usize) } else { (4, b[1] as usize) };
        let body = &b[head..];
        let u16_of = |o: &[u8], i: usize| if big { u16::from_be_bytes([o[i], o[i + 1]]) } else { u16::from_le_bytes([o[i], o[i + 1]]) };
        let u32_of = |o: &[u8], i: usize| if big { u32::from_be_bytes([o[i], o[i + 1], o[i + 2], o[i + 3]]) } else { u32::from_le_bytes([o[i], o[i + 1], o[i + 2], o[i + 3]]) };
        // The objects: the top level and the arrays' elements, all 8 bytes,
        // before any text. Arrays (type 9) point at their elements.
        let mut objects = top;
        let mut k = 0;
        while k < objects && (k + 1) * 8 <= body.len() {
            let o = &body[k * 8..];
            if o[0] & 0x7f == 9 {
                let (len, off) = (u16_of(o, 2) as usize, u32_of(o, 4) as usize);
                objects = objects.max(off / 8 + len);
            }
            k += 1;
        }
        let objects = objects.min(body.len() / 8);
        let mut body = body.to_vec();
        let mut extra = Vec::new();
        let mut changed = false;
        // A top-level `int /name defineusername` is learnt first.
        if top == 3 && objects >= 3 {
            let ty = |k: usize| body[k * 8] & 0x7f;
            let text = |k: usize| {
                let o = &body[k * 8..];
                let (len, off) = (u16_of(o, 2) as usize, u32_of(o, 4) as usize);
                (len != 0 && len != 0xffff).then(|| body.get(off..off + len).map(|t| t.to_vec())).flatten()
            };
            if ty(0) == 1 && ty(1) == 3 && body[8] & 0x80 == 0 && ty(2) == 3 && text(2).as_deref() == Some(b"defineusername") {
                if let Some(name) = text(1) {
                    self.names.insert(u32_of(&body, 4), name);
                }
            }
        }
        for k in 0..objects {
            let at = k * 8;
            if body[at] & 0x7f != 3 || u16_of(&body, at + 2) != 0 {
                continue;
            }
            let index = u32_of(&body, at + 4);
            let Some(name) = self.names.get(&index) else { continue };
            let off = (body.len() + extra.len()) as u32;
            let len = name.len() as u16;
            extra.extend_from_slice(name);
            let (l, o) = if big { (len.to_be_bytes(), off.to_be_bytes()) } else { (len.to_le_bytes(), off.to_le_bytes()) };
            body[at + 2..at + 4].copy_from_slice(&l);
            body[at + 4..at + 8].copy_from_slice(&o);
            changed = true;
        }
        self.push(Token::Other);
        if !changed {
            out.extend_from_slice(b);
            return;
        }
        body.extend_from_slice(&extra);
        // The header: short while the whole fits in 16 bits.
        let total4 = 4 + body.len();
        if head == 4 && total4 <= 0xffff {
            out.push(b[0]);
            out.push(b[1]);
            out.extend_from_slice(&if big { (total4 as u16).to_be_bytes() } else { (total4 as u16).to_le_bytes() });
        } else {
            let total = 8 + body.len();
            out.push(b[0]);
            out.push(0);
            out.extend_from_slice(&if big { (top as u16).to_be_bytes() } else { (top as u16).to_le_bytes() });
            out.extend_from_slice(&if big { (total as u32).to_be_bytes() } else { (total as u32).to_le_bytes() });
        }
        out.extend_from_slice(&body);
    }
}

fn u16_at(b: &[u8], i: usize, big: bool) -> u16 {
    if big { u16::from_be_bytes([b[i], b[i + 1]]) } else { u16::from_le_bytes([b[i], b[i + 1]]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary object sequence, big-endian: objects, then their text.
    fn seq(objects: &[(u8, u16, u32)], text: &[u8]) -> Vec<u8> {
        let mut b = vec![128, objects.len() as u8, 0, 0];
        for &(t, len, v) in objects {
            b.push(t);
            b.push(0);
            b.extend_from_slice(&len.to_be_bytes());
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(text);
        let n = b.len() as u16;
        b[2..4].copy_from_slice(&n.to_be_bytes());
        b
    }

    #[test]
    fn text_definitions_and_binary_uses() {
        let mut n = Names::default();
        // Learnt from text, split across requests, with a string that
        // looks like a definition in the way.
        let a = n.rewrite(b"(12 /x defineusername) pop 7 /rect");
        assert_eq!(a, b"(12 /x defineusername) pop 7 /rect");
        n.rewrite(b"fill defineusername 8 /moveto defineusername\n");
        assert_eq!(n.names.get(&7).map(|v| v.as_slice()), Some(&b"rectfill"[..]));
        assert!(!n.names.contains_key(&12));
        // Binary tokens 148/147 for user names.
        assert_eq!(n.rewrite(&[b'1', b' ', 148, 7, b'2']), b"1  rectfill 2");
        assert_eq!(n.rewrite(&[147, 8]), b" /moveto ");
        // Unknown numbers pass as they are.
        assert_eq!(n.rewrite(&[148, 99]), [148, 99]);
    }

    #[test]
    fn sequences() {
        let mut n = Names::default();
        // [3 /foo defineusername] as a sequence: 3 objects, text after.
        let def = seq(&[(1, 0, 3), (3, 3, 24), (0x83, 14, 27)], b"foodefineusername");
        assert_eq!(n.rewrite(&def), def);
        assert_eq!(n.names.get(&3).map(|v| v.as_slice()), Some(&b"foo"[..]));
        // A use: 1 foo, with foo by number, split in two requests.
        let usage = seq(&[(1, 0, 1), (0x83, 0, 3)], b"");
        let mut got = n.rewrite(&usage[..5]);
        assert!(got.is_empty());
        got.extend(n.rewrite(&usage[5..]));
        assert_eq!(got, seq(&[(1, 0, 1), (0x83, 3, 16)], b"foo"));
        // System names (length -1) are Ghostscript's own.
        let sys = seq(&[(0x83, 0xffff, 107)], b"");
        assert_eq!(n.rewrite(&sys), sys);
        // Bytes past 127 inside a string aren't tokens.
        assert_eq!(n.rewrite(b"(\x80\x93) show"), b"(\x80\x93) show");
    }
}
