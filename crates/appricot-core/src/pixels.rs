//! Pixels copied out of a surface, and the cursor image.

use crate::geometry::{Point, Size};

/// How the bytes of one pixel are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
/// not per surface. `serial` rises monotonically and lets both sides drop a stale image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    /// Rises monotonically; a client drops an image it has already drawn.
    pub serial: u32,
    /// The image's size, at most 128x128 on the wire.
    pub size: Size,
    /// The active point, relative to the image's top-left.
    pub hotspot: Point,
    /// The pixels: `size.width * size.height * 4` bytes.
    pub argb: Vec<u8>,
}

impl CursorImage {
    /// The byte length a cursor image of `size` carries.
    pub fn byte_len(size: Size) -> usize {
        4 * (size.width as usize) * (size.height as usize)
    }
}
