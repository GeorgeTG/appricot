//! Reading pixels out of a window the Composite extension holds for the backend.

use appricot_core::Size;
use appricot_core::{PixelBuffer, PixelFormat};

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
    use appricot_core::Size;

    use super::convert_zpixmap;
    use crate::BackendError;

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
