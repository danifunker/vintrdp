//! The server's colours to ours (`0x00RRGGBB`): the palette at 8 bpp, 5-5-5
//! and 5-6-5 packed pixels, and BGR at 24 and 32 bpp. Port of rdesktop's
//! X.c colour handling, as the old shim.rs did it.

/// Bytes per pixel for a colour depth (15 bpp takes two).
pub fn bytes_per_pixel(bpp: u32) -> usize {
    (bpp as usize).div_ceil(8).clamp(1, 4)
}

/// Widen 5 bits to 8, filling the low bits from the high ones.
fn widen5(v: u32) -> u32 {
    ((v << 3) & 0xf8) | ((v >> 2) & 0x7)
}

pub fn rgb15(c: u32) -> u32 {
    widen5(c >> 10) << 16 | widen5(c >> 5) << 8 | widen5(c)
}

pub fn rgb16(c: u32) -> u32 {
    let g = ((c >> 3) & 0xfc) | ((c >> 9) & 0x3);
    widen5(c >> 11) << 16 | g << 8 | widen5(c)
}

/// The session's depth and, at 8 bpp, its palette.
pub struct Colours {
    pub depth: u32,
    pub palette: [u32; 256],
}

impl Colours {
    pub fn new(depth: u32) -> Self {
        Colours { depth, palette: [0; 256] }
    }

    /// An order's colour (three bytes, low first) in the session's depth.
    pub fn colour(&self, c: u32) -> u32 {
        match self.depth {
            8 => self.palette[(c & 0xff) as usize],
            15 => rgb15(c),
            16 => rgb16(c),
            // Orders carry red first: the opposite of bitmaps.
            _ => (c & 0xff) << 16 | (c & 0xff00) | (c >> 16) & 0xff,
        }
    }

    /// The depth of server-format pixel data `bytes` wide: the session's when
    /// it's that size, else the usual depth for the size.
    pub fn depth_for(&self, bytes: usize) -> u32 {
        if bytes == bytes_per_pixel(self.depth) {
            self.depth
        } else {
            match bytes {
                1 => 8,
                2 => 16,
                3 => 24,
                _ => 32,
            }
        }
    }

    /// Convert `width` x `height` pixels of `bpp` server-format data (rows
    /// top first, packed) to ours. Missing data reads as black.
    pub fn convert(&self, bpp: u32, width: usize, height: usize, data: &[u8]) -> Vec<u32> {
        let n = width.saturating_mul(height);
        let bytes = bytes_per_pixel(bpp);
        let mut out: Vec<u32> = match bpp {
            8 => data.iter().take(n).map(|&i| self.palette[i as usize]).collect(),
            15 => data.chunks_exact(2).take(n).map(|p| rgb15(u16::from_le_bytes([p[0], p[1]]) as u32)).collect(),
            16 => data.chunks_exact(2).take(n).map(|p| rgb16(u16::from_le_bytes([p[0], p[1]]) as u32)).collect(),
            _ => data.chunks_exact(bytes).take(n).map(|p| (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32).collect(),
        };
        out.resize(n, 0);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_colours_per_depth() {
        let mut c = Colours::new(8);
        c.palette[3] = 0x123456;
        assert_eq!(c.colour(0x103), 0x123456);
        c.depth = 15;
        assert_eq!(c.colour(0x7c00), 0xff0000);
        assert_eq!(c.colour(0x03e0), 0x00ff00);
        assert_eq!(c.colour(0x001f), 0x0000ff);
        c.depth = 16;
        assert_eq!(c.colour(0xf800), 0xff0000);
        assert_eq!(c.colour(0x07e0), 0x00ff00);
        assert_eq!(c.colour(0x001f), 0x0000ff);
        assert_eq!(c.colour(0x8410), 0x848284);
        c.depth = 24;
        assert_eq!(c.colour(0x332211), 0x112233);
        c.depth = 32;
        assert_eq!(c.colour(0x332211), 0x112233);
    }

    #[test]
    fn bitmap_pixels_per_depth() {
        let mut c = Colours::new(8);
        c.palette[1] = 0xabcdef;
        assert_eq!(c.convert(8, 2, 1, &[1, 0]), [0xabcdef, 0]);
        assert_eq!(c.convert(15, 1, 1, &[0x00, 0x7c]), [0xff0000]);
        assert_eq!(c.convert(16, 2, 1, &[0xe0, 0x07, 0x1f, 0x00]), [0x00ff00, 0x0000ff]);
        // Bitmaps are BGR, BGRX.
        assert_eq!(c.convert(24, 1, 1, &[0x33, 0x22, 0x11]), [0x112233]);
        assert_eq!(c.convert(32, 1, 1, &[0x33, 0x22, 0x11, 0xff]), [0x112233]);
        // Short data is padded with black rather than read past.
        assert_eq!(c.convert(24, 2, 1, &[1, 2, 3, 4]), [0x030201, 0]);
    }

    #[test]
    fn depth_for_sizes() {
        let c = Colours::new(15);
        assert_eq!(c.depth_for(2), 15);
        assert_eq!(c.depth_for(3), 24);
        assert_eq!(bytes_per_pixel(15), 2);
        assert_eq!(bytes_per_pixel(32), 4);
    }
}
