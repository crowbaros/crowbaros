//! Pixels in memory, and the CPU renderer that composes them.
//!
//! An image holds premultiplied ARGB, one `u32` per pixel, row by row. The CPU
//! renderer draws with the "over" operator, which is what Wayland's
//! premultiplied formats ask for. It draws for the tests, which check real
//! pixels, and it is the fallback where no GPU renders.

use crate::geometry::Rect;

/// The pixel formats Fulcrum takes from shared memory. Every compositor must
/// take both (`wl_shm`).
pub mod format {
    pub const ARGB8888: u32 = 0;
    pub const XRGB8888: u32 = 1;
}

#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u32>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({}x{})", self.width, self.height)
    }
}

/// One channel of `s` over `d`, premultiplied: `s + d × (1 − αs)`.
fn over(source: u32, destination: u32) -> u32 {
    let inverse = 255 - (source >> 24);
    if inverse == 0 {
        return source;
    }
    let mut out = 0;
    for shift in [0, 8, 16, 24] {
        let s = (source >> shift) & 0xff;
        let d = (destination >> shift) & 0xff;
        let value = (s + (d * inverse + 127) / 255).min(255);
        out |= value << shift;
    }
    out
}

impl Image {
    /// A transparent image.
    pub fn new(width: i32, height: i32) -> Self {
        let (width, height) = (width.max(0), height.max(0));
        Image {
            width,
            height,
            pixels: vec![0; width as usize * height as usize],
        }
    }

    pub fn pixel(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        self.pixels.get(y as usize * self.width as usize + x as usize).copied()
    }

    /// Copies pixels out of a client's buffer: `bytes` is its pool, and the
    /// buffer is at `offset`, `stride` bytes a row. The caller has checked
    /// that the buffer lies in the pool and that the format is one of
    /// [`format`]'s.
    pub fn from_shm(bytes: &[u8], offset: usize, width: i32, height: i32, stride: i32, format: u32) -> Self {
        let mut image = Image::new(width, height);
        let opaque = format == format::XRGB8888;
        for y in 0..height as usize {
            let row = offset + y * stride as usize;
            for x in 0..width as usize {
                let at = row + x * 4;
                let Some(chunk) = bytes.get(at..at + 4) else {
                    continue;
                };
                let mut pixel = u32::from_le_bytes(chunk.try_into().expect("four bytes"));
                if opaque {
                    pixel |= 0xff00_0000;
                }
                image.pixels[y * width as usize + x] = pixel;
            }
        }
        image
    }

    /// Draws `source` over this image with its top-left corner at `at`,
    /// clipped to this image.
    pub fn draw(&mut self, source: &Image, at: (i32, i32)) {
        for sy in 0..source.height {
            let y = at.1.saturating_add(sy);
            if y < 0 || y >= self.height {
                continue;
            }
            for sx in 0..source.width {
                let x = at.0.saturating_add(sx);
                if x < 0 || x >= self.width {
                    continue;
                }
                let s = source.pixels[(sy * source.width + sx) as usize];
                let d = &mut self.pixels[(y * self.width + x) as usize];
                *d = over(s, *d);
            }
        }
    }

    /// Fills a rectangle with one premultiplied colour, drawn over.
    pub fn fill(&mut self, rect: Rect, argb: u32) {
        for y in rect.y.max(0)..(rect.y.saturating_add(rect.height)).min(self.height) {
            for x in rect.x.max(0)..(rect.x.saturating_add(rect.width)).min(self.width) {
                let d = &mut self.pixels[(y * self.width + x) as usize];
                *d = over(argb, *d);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opaque_pixel_replaces_and_a_transparent_one_keeps() {
        assert_eq!(over(0xff10_2030, 0xff40_5060), 0xff10_2030);
        assert_eq!(over(0x0000_0000, 0xff40_5060), 0xff40_5060);
    }

    #[test]
    fn half_alpha_blends_premultiplied() {
        // 50% white over opaque black is opaque middle grey.
        assert_eq!(over(0x8080_8080, 0xff00_0000), 0xff80_8080);
    }

    #[test]
    fn xrgb_is_drawn_opaque_whatever_its_unused_byte_says() {
        let bytes = 0x0011_2233u32.to_le_bytes();
        let image = Image::from_shm(&bytes, 0, 1, 1, 4, format::XRGB8888);
        assert_eq!(image.pixel(0, 0), Some(0xff11_2233));
    }

    #[test]
    fn drawing_is_clipped_to_the_destination() {
        let mut destination = Image::new(4, 4);
        let mut source = Image::new(3, 3);
        source.fill(Rect::new(0, 0, 3, 3), 0xffff_0000);
        destination.draw(&source, (2, -1));
        assert_eq!(destination.pixel(3, 0), Some(0xffff_0000));
        assert_eq!(destination.pixel(3, 1), Some(0xffff_0000));
        assert_eq!(destination.pixel(3, 2), Some(0));
        assert_eq!(destination.pixel(1, 0), Some(0));
    }
}
