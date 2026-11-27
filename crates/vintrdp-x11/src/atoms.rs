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

/// Atom names are bytes (Latin-1), compared exactly.
pub struct Atoms {
    names: Vec<Vec<u8>>,
    ids: HashMap<Vec<u8>, u32>,
}

impl Default for Atoms {
    fn default() -> Self {
        let mut atoms = Atoms { names: vec![Vec::new()], ids: HashMap::new() };
        for name in PREDEFINED {
            atoms.intern(name.as_bytes());
        }
        atoms
    }
}

impl Atoms {
    pub fn intern(&mut self, name: &[u8]) -> u32 {
        if let Some(&id) = self.ids.get(name) {
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
}
