//! Text encodings: UTF-16LE (RDP's own) and Windows-1252, the ANSI code page
//! of Western-language servers (CF_TEXT, NT 4.0's licensing strings).

/// UTF-16LE bytes up to the first NUL (or the end), lossily.
pub fn from_utf16le(data: &[u8]) -> String {
    let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// `s` as UTF-16LE, with a terminating NUL when `nul` is set.
pub fn to_utf16le(s: &str, nul: bool) -> Vec<u8> {
    let mut out: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
    if nul {
        out.extend_from_slice(&[0, 0]);
    }
    out
}

/// Windows-1252's 0x80-0x9f; the rest of the code page is Latin-1. Unassigned
/// bytes (0x81, 0x8d, 0x8f, 0x90, 0x9d) map to the C1 control of that value,
/// as Windows' own MultiByteToWideChar does.
const CP1252_HIGH: [u16; 32] = [
    0x20ac, 0x0081, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039, 0x0152, 0x008d, 0x017d, 0x008f,
    0x0090, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0x009d, 0x017e, 0x0178,
];

/// A Windows-1252 byte as a character.
pub fn cp1252_char(b: u8) -> char {
    match b {
        0x80..=0x9f => char::from_u32(CP1252_HIGH[(b - 0x80) as usize] as u32).unwrap_or('?'),
        _ => b as char,
    }
}

/// A character's Windows-1252 byte, if it has one.
pub fn cp1252_byte(c: char) -> Option<u8> {
    let u = c as u32;
    match u {
        0..=0x7f | 0xa0..=0xff => Some(u as u8),
        _ => CP1252_HIGH.iter().position(|&h| h as u32 == u).map(|i| 0x80 + i as u8),
    }
}

/// Windows-1252 bytes up to the first NUL.
pub fn from_cp1252(data: &[u8]) -> String {
    data.iter().take_while(|&&b| b != 0).map(|&b| cp1252_char(b)).collect()
}

/// `s` in Windows-1252, with `?` for what it can't hold.
pub fn to_cp1252(s: &str) -> Vec<u8> {
    s.chars().map(|c| cp1252_byte(c).unwrap_or(b'?')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_round_trips() {
        let s = "h\u{e9}llo \u{1f600}";
        assert_eq!(from_utf16le(&to_utf16le(s, true)), s);
    }

    #[test]
    fn cp1252() {
        assert_eq!(to_cp1252("\u{20ac}\u{e9}\u{4e16}"), [0x80, 0xe9, b'?']);
        assert_eq!(from_cp1252(&[0x93, b'x', 0x94, 0, b'y']), "\u{201c}x\u{201d}");
        for b in 0..=255u8 {
            assert_eq!(cp1252_byte(cp1252_char(b)), Some(b));
        }
    }
}
