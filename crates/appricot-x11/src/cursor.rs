//! The session cursor image, read through XFixes.

use appricot_core::CursorImage;
use appricot_core::{Point, Size};
use x11rb::protocol::ErrorKind;

/// The largest cursor image handed upstream, in each dimension. The wire caps `CursorImage`
/// at 128x128; larger images (none are expected: servers cap cursors far below this) are
/// cropped, not scaled.
pub(crate) const MAX_CURSOR_SIDE: u32 = 128;

/// The errors a cursor-image grab may answer while the display itself is fine, so the
/// one update is skipped and the session lives on: the Cursor error the request is
/// specified to carry (the cursor hidden, the pointer off the screens), and an Access
/// refusal — observed from an X server when a cursor another client has just set becomes
/// the displayed one (reproduced with xsetroot, 2026-09-29). Anything else still fails
/// the backend.
pub(crate) fn is_grab_refusal(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::Cursor | ErrorKind::Access)
}

/// Turns a XFixes `GetCursorImage` reply into the model's cursor image.
///
/// XFixes reports pixels as native-order `CARD32` of the form `0xAARRGGBB`, already
/// premultiplied; the model wants bytes `A, R, G, B` per pixel, row by row from the top,
/// which is what this produces.
pub(crate) fn cursor_image(
    serial: u32,
    width: u16,
    height: u16,
    xhot: u16,
    yhot: u16,
    pixels: &[u32],
) -> CursorImage {
    let full = Size::new(u32::from(width), u32::from(height));
    let size = Size::new(
        full.width.min(MAX_CURSOR_SIDE),
        full.height.min(MAX_CURSOR_SIDE),
    );
    let mut argb = Vec::with_capacity(CursorImage::byte_len(size).unwrap_or(0));
    for y in 0..size.height {
        for x in 0..size.width {
            let packed = pixels[(y * full.width + x) as usize].to_be_bytes();
            argb.extend_from_slice(&packed);
        }
    }
    CursorImage {
        serial,
        size,
        hotspot: Point::new(i32::from(xhot), i32::from(yhot)),
        argb,
    }
}

#[cfg(test)]
mod tests {
    use super::{cursor_image, is_grab_refusal};
    use x11rb::protocol::ErrorKind;

    #[test]
    fn the_grabs_documented_refusals_skip_the_update_and_everything_else_fails() {
        assert!(is_grab_refusal(ErrorKind::Cursor));
        assert!(is_grab_refusal(ErrorKind::Access));
        // Not refusals: a window gone mid-grab is a different path's business, and an
        // unknown error must still fail the backend rather than hide behind the cursor.
        assert!(!is_grab_refusal(ErrorKind::Window));
        assert!(!is_grab_refusal(ErrorKind::Implementation));
    }

    #[test]
    fn argb_words_become_argb_bytes() {
        // 2x1 cursor: opaque white, then half-transparent red (premultiplied: 0x80 red).
        let image = cursor_image(7, 2, 1, 1, 0, &[0xff_ff_ff_ff, 0x80_80_00_00]);
        assert_eq!(image.serial, 7);
        assert_eq!(
            image.argb,
            vec![0xff, 0xff, 0xff, 0xff, 0x80, 0x80, 0x00, 0x00]
        );
    }

    #[test]
    fn the_hotspot_travels_unchanged() {
        let image = cursor_image(1, 1, 1, 3, 4, &[0]);
        assert_eq!(image.hotspot.x, 3);
        assert_eq!(image.hotspot.y, 4);
    }

    #[test]
    fn oversized_images_are_cropped_to_the_cap() {
        // A 200x1 cursor is cropped to 128x1; the first 128 pixels survive in order.
        let pixels: Vec<u32> = (0..200u32).map(|i| 0xff_00_00_00 | i).collect();
        let image = cursor_image(2, 200, 1, 0, 0, &pixels);
        assert_eq!(image.size.width, 128);
        assert_eq!(image.argb.len(), 128 * 4);
        // Packed 0xff_00_00_7f is A=ff R=00 G=00 B=7f.
        assert_eq!(&image.argb[4 * 127..4 * 128], &[0xff, 0x00, 0x00, 0x7f]);
    }
}
