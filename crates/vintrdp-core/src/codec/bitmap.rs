//! Bitmap decompression: RDP's interleaved RLE (8, 15, 16 and 24 bpp) and
//! RDP 6 planar (32 bpp). Port of rdesktop's bitmap.c.

/// Decompress `input` into a `width` x `height` bitmap of `bpp` bits per
/// pixel (8, 15, 16, 24 or 32), in the server's own pixel format, rows top
/// first (the wire's bottom-up order already flipped), each row `width *
/// bytes_per_pixel` bytes with no padding. `None` if the data is corrupt.
pub fn decompress(input: &[u8], width: usize, height: usize, bpp: u32) -> Option<Vec<u8>> {
    let _ = (input, width, height, bpp);
    todo!("codec::bitmap::decompress")
}
