//! Atoms: the 68 the protocol predefines, then whatever clients intern.

use std::collections::HashMap;

const PREDEFINED: [&str; 68] = [
    "PRIMARY", "SECONDARY", "ARC", "ATOM", "BITMAP", "CARDINAL", "COLORMAP", "CURSOR",
    "CUT_BUFFER0", "CUT_BUFFER1", "CUT_BUFFER2", "CUT_BUFFER3", "CUT_BUFFER4", "CUT_BUFFER5", "CUT_BUFFER6", "CUT_BUFFER7",
    "DRAWABLE", "FONT", "INTEGER", "PIXMAP", "POINT", "RECTANGLE", "RESOURCE_MANAGER",
    "RGB_COLOR_MAP", "RGB_BEST_MAP", "RGB_BLUE_MAP", "RGB_DEFAULT_MAP", "RGB_GRAY_MAP", "RGB_GREEN_MAP", "RGB_RED_MAP",
    "STRING", "VISUALID", "WINDOW", "WM_COMMAND", "WM_HINTS", "WM_CLIENT_MACHINE", "WM_ICON_NAME", "WM_ICON_SIZE",
    "WM_NAME", "WM_NORMAL_HINTS", "WM_SIZE_HINTS", "WM_ZOOM_HINTS", "MIN_SPACE", "NORM_SPACE", "MAX_SPACE", "END_SPACE",
    "SUPERSCRIPT_X", "SUPERSCRIPT_Y", "SUBSCRIPT_X", "SUBSCRIPT_Y", "UNDERLINE_POSITION", "UNDERLINE_THICKNESS",
    "STRIKEOUT_ASCENT", "STRIKEOUT_DESCENT", "ITALIC_ANGLE", "X_HEIGHT", "QUAD_WIDTH", "WEIGHT", "POINT_SIZE",
    "RESOLUTION", "COPYRIGHT", "NOTICE", "FONT_NAME", "FAMILY_NAME", "FULL_NAME", "CAP_HEIGHT", "WM_CLASS",
    "WM_TRANSIENT_FOR",
];

/// At most this many atoms, and this many bytes of their names: past
/// either, InternAtom answers BadAlloc, so a client can't use up the
/// server's memory with them (atoms are never freed). X.org has no limit
/// but its memory; these are far beyond what any session interns.
pub const MAX_ATOMS: usize = 1 << 18;
pub const MAX_NAME_BYTES: usize = 32 << 20;

/// Atom names are bytes (Latin-1), compared exactly.
pub struct Atoms {
    names: Vec<Vec<u8>>,
    ids: HashMap<Vec<u8>, u32>,
    bytes: usize,
    max: (usize, usize),
}

impl Default for Atoms {
    fn default() -> Self {
        let mut atoms = Atoms { names: vec![Vec::new()], ids: HashMap::new(), bytes: 0, max: (MAX_ATOMS, MAX_NAME_BYTES) };
        for name in PREDEFINED {
            atoms.intern(name.as_bytes());
        }
        atoms
    }
}

impl Atoms {
    /// The atom for a name, made if it's new; None past the limits.
    pub fn try_intern(&mut self, name: &[u8]) -> Option<u32> {
        if let Some(&id) = self.ids.get(name) {
            return Some(id);
        }
        if self.names.len() > self.max.0 || self.bytes + name.len() > self.max.1 {
            return None;
        }
        let id = self.names.len() as u32;
        self.bytes += name.len();
        self.names.push(name.to_vec());
        self.ids.insert(name.to_vec(), id);
        Some(id)
    }

    /// The server's own atoms (a handful of names): made even past the
    /// limits, which only clients' names count against.
    pub fn intern(&mut self, name: &[u8]) -> u32 {
        if let Some(id) = self.try_intern(name) {
            return id;
        }
        let id = self.names.len() as u32;
        self.names.push(name.to_vec());
        self.ids.insert(name.to_vec(), id);
        id
    }

    pub fn find(&self, name: &[u8]) -> Option<u32> {
        self.ids.get(name).copied()
    }

    pub fn name(&self, atom: u32) -> Option<&[u8]> {
        self.names.get(atom as usize).filter(|_| atom != 0).map(Vec::as_slice)
    }

    pub fn exists(&self, atom: u32) -> bool {
        self.name(atom).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb_protocol::protocol::xproto::AtomEnum;

    #[test]
    fn predefined() {
        let mut a = Atoms::default();
        assert_eq!(a.find(b"PRIMARY"), Some(AtomEnum::PRIMARY.into()));
        assert_eq!(a.find(b"STRING"), Some(AtomEnum::STRING.into()));
        assert_eq!(a.find(b"WM_TRANSIENT_FOR"), Some(AtomEnum::WM_TRANSIENT_FOR.into()));
        assert_eq!(a.name(0), None);
        assert_eq!(a.intern(b"_NET_WM_NAME"), 69);
        assert_eq!(a.intern(b"_NET_WM_NAME"), 69);
        assert_eq!(a.name(69), Some(&b"_NET_WM_NAME"[..]));
        assert!(!a.exists(70));
    }

    #[test]
    fn limits() {
        let mut a = Atoms::default();
        a.max = (a.names.len() + 1, usize::MAX);
        assert!(a.try_intern(b"ONE").is_some());
        assert!(a.try_intern(b"TWO").is_some());
        assert_eq!(a.try_intern(b"THREE"), None);
        assert_eq!(a.try_intern(b"ONE"), a.find(b"ONE"));
        let mut b = Atoms::default();
        b.max = (usize::MAX, b.bytes + 4);
        assert!(b.try_intern(b"ABCD").is_some());
        assert_eq!(b.try_intern(b"E"), None);
    }
}
