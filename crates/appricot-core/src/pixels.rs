//! Pixels copied out of a surface, and the cursor image.

use crate::geometry::{Point, Size};

/// How the bytes of one pixel are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Four bytes per pixel: blue, green, red, then one unused byte. This is the X11
    /// 32-bit ZPixmap byte order of a 24-bit depth window, and the layout of wire codec 1
    /// (RAW).
    Bgrx8888,
}

/// Pixels copied out of a surface, row by row from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelBuffer {
    /// Width and height of the copied area.
    pub size: Size,
    /// Bytes from the start of one row to the start of the next.
    pub stride: usize,
    /// The layout of each pixel.
    pub format: PixelFormat,
    /// The bytes: `stride` times `size.height` of them.
    pub data: Vec<u8>,
}

/// The cursor image of the session: ARGB with premultiplied alpha, 8 bits per channel,
/// row-major from the top, exactly `width * height * 4` bytes.
///
/// It mirrors what XFixes reports; on the wire it is `CursorImage`. One cursor per session,
/// not per surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    /// From a backend, its own serial, at most a cache key. The session replaces it with its
    /// own count, which rises by one for every image it forwards, so a client that drops an
    /// image it has already drawn never drops one the app returned to.
    pub serial: u32,
    /// The image's size, at most 128x128 on the wire.
    pub size: Size,
    /// The active point, relative to the image's top-left.
    pub hotspot: Point,
    /// The pixels: `size.width * size.height * 4` bytes.
    pub argb: Vec<u8>,
}

impl CursorImage {
    /// The byte length a cursor image of `size` carries, or `None` when it does not fit in a
    /// `usize`. The sides are the caller's, so the product is checked, never wrapped.
    pub fn byte_len(size: Size) -> Option<usize> {
        let width = usize::try_from(size.width).ok()?;
        let height = usize::try_from(size.height).ok()?;
        width.checked_mul(height)?.checked_mul(4)
    }
}

#[cfg(test)]
mod tests {
    use crate::{CursorImage, Size};

    #[test]
    fn byte_len_is_four_bytes_a_pixel() {
        assert_eq!(CursorImage::byte_len(Size::new(128, 128)), Some(65_536));
        assert_eq!(CursorImage::byte_len(Size::new(0, 7)), Some(0));
    }

    #[test]
    fn byte_len_refuses_a_product_that_does_not_fit() {
        // 2^32 * 2^32 * 4 bytes is past a 64-bit usize, and u32 sides are past a 32-bit one.
        assert_eq!(CursorImage::byte_len(Size::new(u32::MAX, u32::MAX)), None);
    }
}
