//! The screen's visuals, as Xvfb offers them: at depth 24, TrueColor (the
//! default) and DirectColor; at depth 8 ([`RootVisual::PseudoColor8`], an
//! 8-bit X terminal), PseudoColor (the default), GrayScale, StaticColor,
//! TrueColor, DirectColor and StaticGray. Pixels on the screen are the
//! visuals' own; the front-end is shown them through the installed
//! colormap, as a terminal's hardware does.

use crate::cmap::{Class, Visual};

/// The root window's depth and default visual.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RootVisual {
    #[default]
    TrueColor24,
    PseudoColor8,
}

#[derive(Clone, Copy, Debug)]
pub struct VisualDef {
    pub id: u32,
    pub depth: u8,
    pub v: Visual,
}

// STUB(x11): one depth for windows at a time: no 8-bit visual on the 24-bit screen, as Sun's and SGI's overlay planes gave (phase 4)
/// The visuals, the root's (the default) first.
pub fn visuals(root: RootVisual) -> Vec<VisualDef> {
    let v = |class, entries, (red_mask, green_mask, blue_mask)| Visual { class, bits_per_rgb: 8, entries, red_mask, green_mask, blue_mask };
    let id = |i: u32| crate::server::VISUAL + i;
    match root {
        RootVisual::TrueColor24 => {
            let rgb = (0xff_0000, 0xff00, 0xff);
            vec![VisualDef { id: id(0), depth: 24, v: v(Class::TrueColor, 256, rgb) }, VisualDef { id: id(1), depth: 24, v: v(Class::DirectColor, 256, rgb) }]
        }
        RootVisual::PseudoColor8 => {
            let (none, bgr) = ((0, 0, 0), (0x07, 0x38, 0xc0));
            [(Class::PseudoColor, 256, none), (Class::GrayScale, 256, none), (Class::StaticColor, 256, bgr), (Class::TrueColor, 8, bgr), (Class::DirectColor, 8, bgr), (Class::StaticGray, 256, none)]
                .into_iter()
                .enumerate()
                .map(|(i, (class, entries, masks))| VisualDef { id: id(i as u32), depth: 8, v: v(class, entries, masks) })
                .collect()
        }
    }
}

/// How the screen's pixels are shown: through the installed colormap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lut {
    /// TrueColor at depth 24: the pixel is the colour.
    Identity,
    /// Depth 8: a colour for each of the 256 pixels.
    Index(Vec<u32>),
    /// DirectColor at depth 24: each channel through its own table.
    Channels(Vec<u8>, Vec<u8>, Vec<u8>),
}

impl Lut {
    pub fn of(map: &crate::cmap::Cmap, depth: u8) -> Lut {
        let v = map.visual();
        if depth == 8 {
            return Lut::Index((0..256).map(|p| map.rgb(p)).collect());
        }
        match v.class {
            Class::DirectColor => {
                let ch = |shift: u32| (0..256u32).map(|i| (map.rgb(i << shift) >> shift) as u8).collect();
                Lut::Channels(ch(16), ch(8), ch(0))
            }
            _ => Lut::Identity,
        }
    }

    pub fn apply(&self, px: u32) -> u32 {
        match self {
            Lut::Identity => px & 0xff_ffff,
            Lut::Index(t) => t[(px & 0xff) as usize],
            Lut::Channels(r, g, b) => (r[(px >> 16 & 0xff) as usize] as u32) << 16 | (g[(px >> 8 & 0xff) as usize] as u32) << 8 | b[(px & 0xff) as usize] as u32,
        }
    }
}
