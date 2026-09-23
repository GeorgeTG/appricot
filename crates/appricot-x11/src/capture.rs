//! Reading pixels out of a window the Composite extension holds for the backend.

use appricot_core::{PixelBuffer, PixelFormat};
use appricot_core::{Point, Rect, Size};

use crate::BackendError;

/// The bytes per pixel of a 24-bit depth `ZPixmap` image: blue, green, red.
const DEPTH24_BPP: usize = 3;

/// The bytes per pixel of a 32-bit depth `ZPixmap` image: blue, green, red, then alpha or
/// an unused byte.
const DEPTH32_BPP: usize = 4;

/// Turns a `GetImage(ZPixmap)` reply into the model's pixel buffer.
///
/// `bytes_per_pixel` is how the SERVER stores a pixmap of this depth, read from its
/// pixmap-format table at connect: the depth alone does not fix the byte layout, because
/// most Xorg servers (Xvfb included) store depth 24 as 32 bits per pixel. Three bytes per
/// pixel (`B, G, R`, rows padded to 4 bytes) leave as `BGRX8888` with the unused byte set
/// to `0xff`. Four bytes per pixel are already `BGRX8888`; at depth 24 the unused fourth
/// byte is forced to `0xff`, because a server may leave anything there, while depth 32 is
/// copied untouched. Anything else is refused.
pub(crate) fn convert_zpixmap(
    depth: u8,
    data: &[u8],
    size: Size,
    bytes_per_pixel: usize,
) -> Result<PixelBuffer, BackendError> {
    if size.is_empty() {
        return Ok(PixelBuffer {
            size,
            stride: 0,
            format: PixelFormat::Bgrx8888,
            data: Vec::new(),
        });
    }
    match (depth, bytes_per_pixel) {
        (32, DEPTH32_BPP) => copy4(data, size),
        (24, DEPTH32_BPP) => normalize4(data, size),
        (24, DEPTH24_BPP) => convert3(data, size),
        (other, _) => Err(BackendError::UnsupportedDepth(other)),
    }
}

/// Copies a 4-byte-per-pixel depth-24 image, forcing the unused byte opaque.
fn normalize4(data: &[u8], size: Size) -> Result<PixelBuffer, BackendError> {
    let mut buffer = copy4(data, size)?;
    for pixel in buffer.data.chunks_exact_mut(DEPTH32_BPP) {
        pixel[3] = 0xff;
    }
    Ok(buffer)
}

/// A 3-byte-per-pixel image in 4-byte-per-pixel dress.
fn convert3(data: &[u8], size: Size) -> Result<PixelBuffer, BackendError> {
    let width = size.width as usize;
    let height = size.height as usize;
    // The server pads each scanline to 4 bytes; a short image without padding is accepted
    // too, so the stride is inferred from what actually arrived.
    let tight = width * DEPTH24_BPP;
    let padded = (tight + 3) & !3;
    let stride = if data.len() >= padded * height {
        padded
    } else if data.len() >= tight * height {
        tight
    } else {
        return Err(BackendError::ShortImage {
            got: data.len(),
            wanted: padded * height,
        });
    };

    let out_stride = width * DEPTH32_BPP;
    let mut out = Vec::with_capacity(out_stride * height);
    for y in 0..height {
        let row = &data[y * stride..y * stride + tight];
        for pixel in row.chunks_exact(DEPTH24_BPP) {
            out.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
        }
    }
    Ok(PixelBuffer {
        size,
        stride: out_stride,
        format: PixelFormat::Bgrx8888,
        data: out,
    })
}

/// A `BGRX8888` buffer of `size`, every byte zero: what a capture returns for the part of
/// a rectangle no window pixel covers.
pub(crate) fn zeroed(size: Size) -> PixelBuffer {
    let stride = size.width as usize * DEPTH32_BPP;
    PixelBuffer {
        size,
        stride,
        format: PixelFormat::Bgrx8888,
        data: vec![0; stride * size.height as usize],
    }
}

/// Puts `part`, whose top-left pixel is at `at`, into a buffer covering exactly `rect`,
/// zeros around it. `part` must lie inside `rect`; whatever does not is cut off.
///
/// This is how a capture keeps its contract (C2): the buffer always has the size of the
/// rectangle asked for, even when the window no longer covers all of it.
pub(crate) fn place_in(part: PixelBuffer, rect: Rect, at: Point) -> PixelBuffer {
    if part.size == rect.size && at == rect.origin {
        return part;
    }
    let mut out = zeroed(rect.size);
    let Some(shared) = Rect::new(at.x, at.y, part.size.width, part.size.height).intersection(rect)
    else {
        return out;
    };
    // Offsets are non-negative: `shared` lies inside both rectangles.
    let offset = |v: i32| usize::try_from(v).unwrap_or(0);
    let row_bytes = shared.size.width as usize * DEPTH32_BPP;
    for row in 0..shared.size.height as usize {
        let src_x = offset(shared.origin.x - at.x) * DEPTH32_BPP;
        let src_y = offset(shared.origin.y - at.y) + row;
        let dst_x = offset(shared.origin.x - rect.origin.x) * DEPTH32_BPP;
        let dst_y = offset(shared.origin.y - rect.origin.y) + row;
        let src = src_y * part.stride + src_x;
        let dst = dst_y * out.stride + dst_x;
        if let (Some(from), Some(to)) = (
            part.data.get(src..src + row_bytes),
            out.data.get_mut(dst..dst + row_bytes),
        ) {
            to.copy_from_slice(from);
        }
    }
    out
}

/// A 4-byte-per-pixel image copied row by row.
fn copy4(data: &[u8], size: Size) -> Result<PixelBuffer, BackendError> {
    let width = size.width as usize;
    let height = size.height as usize;
    let tight = width * DEPTH32_BPP;
    if data.len() < tight * height {
        return Err(BackendError::ShortImage {
            got: data.len(),
            wanted: tight * height,
        });
    }

    let mut out = Vec::with_capacity(tight * height);
    out.extend_from_slice(&data[..tight * height]);

    Ok(PixelBuffer {
        size,
        stride: tight,
        format: PixelFormat::Bgrx8888,
        data: out,
    })
}

#[cfg(test)]
mod tests {
    use appricot_core::{PixelBuffer, PixelFormat, Point, Rect, Size};

    use super::{convert_zpixmap, place_in, zeroed};
    use crate::BackendError;

    /// A `width` x `height` buffer whose pixel `(x, y)` is `[x, y, 0xaa, 0xff]`.
    fn pattern(width: u32, height: u32) -> PixelBuffer {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&[
                    u8::try_from(x).unwrap_or(0),
                    u8::try_from(y).unwrap_or(0),
                    0xaa,
                    0xff,
                ]);
            }
        }
        PixelBuffer {
            size: Size::new(width, height),
            stride: width as usize * 4,
            format: PixelFormat::Bgrx8888,
            data,
        }
    }

    fn pixel(buffer: &PixelBuffer, x: usize, y: usize) -> [u8; 4] {
        let at = y * buffer.stride + x * 4;
        buffer.data[at..at + 4].try_into().expect("four bytes")
    }

    #[test]
    fn a_zeroed_buffer_has_the_full_size_and_stride() {
        let buffer = zeroed(Size::new(3, 2));
        assert_eq!(buffer.size, Size::new(3, 2));
        assert_eq!(buffer.stride, 12);
        assert_eq!(buffer.data, vec![0; 24]);
        // An empty rectangle still reports its size, with no bytes.
        let empty = zeroed(Size::new(5, 0));
        assert_eq!(empty.size, Size::new(5, 0));
        assert!(empty.data.is_empty());
    }

    #[test]
    fn a_part_that_fills_the_rect_is_returned_as_is() {
        let part = pattern(4, 3);
        let placed = place_in(part.clone(), Rect::new(8, 8, 4, 3), Point::new(8, 8));
        assert_eq!(placed, part);
    }

    #[test]
    fn a_part_covering_the_top_left_is_padded_with_zeros() {
        // The window shrank to 2x2 inside a 4x3 tile at the surface origin.
        let placed = place_in(pattern(2, 2), Rect::new(0, 0, 4, 3), Point::new(0, 0));
        assert_eq!(placed.size, Size::new(4, 3));
        assert_eq!(placed.stride, 16);
        assert_eq!(placed.data.len(), 48);
        assert_eq!(pixel(&placed, 0, 0), [0, 0, 0xaa, 0xff]);
        assert_eq!(pixel(&placed, 1, 1), [1, 1, 0xaa, 0xff]);
        assert_eq!(pixel(&placed, 2, 0), [0; 4]);
        assert_eq!(pixel(&placed, 0, 2), [0; 4]);
        assert_eq!(pixel(&placed, 3, 2), [0; 4]);
    }

    #[test]
    fn a_part_inside_the_rect_lands_at_its_offset() {
        // The tile starts at (10, 20); the window's pixels cover (11, 21) to (12, 22).
        let placed = place_in(pattern(2, 2), Rect::new(10, 20, 4, 4), Point::new(11, 21));
        assert_eq!(pixel(&placed, 0, 0), [0; 4]);
        assert_eq!(pixel(&placed, 1, 1), [0, 0, 0xaa, 0xff]);
        assert_eq!(pixel(&placed, 2, 2), [1, 1, 0xaa, 0xff]);
        assert_eq!(pixel(&placed, 3, 3), [0; 4]);
    }

    #[test]
    fn a_part_outside_the_rect_leaves_only_zeros() {
        let placed = place_in(pattern(2, 2), Rect::new(0, 0, 2, 2), Point::new(50, 50));
        assert_eq!(placed, zeroed(Size::new(2, 2)));
    }

    #[test]
    fn depth_24_pixels_become_bgrx() {
        // 2x1: two pixels, B=0x11 G=0x22 R=0x33 and B=0xaa G=0xbb R=0xcc.
        let buffer = convert_zpixmap(
            24,
            &[0x11, 0x22, 0x33, 0xaa, 0xbb, 0xcc],
            Size::new(2, 1),
            3,
        )
        .expect("converts");
        assert_eq!(buffer.size, Size::new(2, 1));
        assert_eq!(buffer.stride, 8);
        assert_eq!(
            buffer.data,
            vec![0x11, 0x22, 0x33, 0xff, 0xaa, 0xbb, 0xcc, 0xff]
        );
    }

    #[test]
    fn depth_24_scanline_padding_is_skipped() {
        // Width 3 makes a 9-byte row padded to 12; the padding bytes are not pixels.
        let mut data = Vec::new();
        data.extend_from_slice(&[0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90]);
        data.extend_from_slice(&[0xff, 0xff, 0xff]); // padding, must not be read
        data.extend_from_slice(&[0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0, 0x01, 0x02, 0x03]);
        data.extend_from_slice(&[0xff, 0xff, 0xff]); // padding
        let buffer = convert_zpixmap(24, &data, Size::new(3, 2), 3).expect("converts");
        assert_eq!(buffer.stride, 12);
        assert_eq!(buffer.data.len(), 3 * 2 * 4);
        assert_eq!(&buffer.data[..4], &[0x10, 0x20, 0x30, 0xff]);
        assert_eq!(&buffer.data[3 * 4..3 * 4 + 4], &[0xa0, 0xb0, 0xc0, 0xff]);
        assert_eq!(&buffer.data[5 * 4..5 * 4 + 4], &[0x01, 0x02, 0x03, 0xff]);
    }

    #[test]
    fn depth_24_stored_as_4bpp_is_normalized_opaque() {
        // Xvfb stores depth 24 as 32bpp; the fourth byte may be garbage.
        let buffer = convert_zpixmap(
            24,
            &[0x11, 0x22, 0x33, 0x00, 0xaa, 0xbb, 0xcc, 0x77],
            Size::new(2, 1),
            4,
        )
        .expect("converts");
        assert_eq!(
            buffer.data,
            vec![0x11, 0x22, 0x33, 0xff, 0xaa, 0xbb, 0xcc, 0xff]
        );
    }

    #[test]
    fn depth_32_pixels_are_copied_as_bgrx() {
        let buffer =
            convert_zpixmap(32, &[1, 2, 3, 4, 5, 6, 7, 8], Size::new(2, 1), 4).expect("converts");
        assert_eq!(buffer.data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(buffer.stride, 8);
    }

    #[test]
    fn an_empty_rect_yields_an_empty_buffer() {
        let buffer = convert_zpixmap(24, &[], Size::new(0, 10), 4).expect("converts");
        assert!(buffer.data.is_empty());
    }

    #[test]
    fn a_short_image_is_an_error() {
        assert!(matches!(
            convert_zpixmap(24, &[1, 2, 3], Size::new(2, 2), 3),
            Err(BackendError::ShortImage { .. })
        ));
    }

    #[test]
    fn an_odd_depth_is_an_error() {
        assert!(matches!(
            convert_zpixmap(16, &[0; 4], Size::new(1, 1), 2),
            Err(BackendError::UnsupportedDepth(16))
        ));
    }
}
