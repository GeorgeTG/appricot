//! Popup placement, every anchor against every gravity: xdg_positioner semantics.
//!
//! The anchor names a point on the anchor rectangle; the gravity is the direction the popup
//! extends from that point (<https://wayland.app/protocols/xdg-shell>, `xdg_positioner`,
//! checked 2026-09-20). Gravity `bottom_right` puts the popup's top-left corner on the point;
//! gravity `top` puts the popup above the point, centred on it. The offset moves the result.

use appricot_core::{Anchor, Point, Positioner, Rect, Size};

/// Every anchor, with the point it names on `ANCHOR_RECT`.
const ANCHOR_POINTS: [(Anchor, (i32, i32)); 9] = [
    (Anchor::Center, (60, 50)),
    (Anchor::Top, (60, 20)),
    (Anchor::Bottom, (60, 80)),
    (Anchor::Left, (10, 50)),
    (Anchor::Right, (110, 50)),
    (Anchor::TopLeft, (10, 20)),
    (Anchor::BottomLeft, (10, 80)),
    (Anchor::TopRight, (110, 20)),
    (Anchor::BottomRight, (110, 80)),
];

/// Every gravity, with where it puts a `SIZE` popup's top-left corner relative to the point.
const GRAVITY_SHIFTS: [(Anchor, (i32, i32)); 9] = [
    (Anchor::Center, (-15, -5)),
    (Anchor::Top, (-15, -10)),
    (Anchor::Bottom, (-15, 0)),
    (Anchor::Left, (-30, -5)),
    (Anchor::Right, (0, -5)),
    (Anchor::TopLeft, (-30, -10)),
    (Anchor::BottomLeft, (-30, 0)),
    (Anchor::TopRight, (0, -10)),
    (Anchor::BottomRight, (0, 0)),
];

/// A 100x60 rectangle at (10, 20) in the parent.
const ANCHOR_RECT: Rect = Rect::new(10, 20, 100, 60);
/// The popup's size.
const SIZE: Size = Size::new(30, 10);
/// The last shift.
const OFFSET: Point = Point::new(3, -4);

#[test]
fn every_anchor_and_gravity_pair_places_the_popup_where_xdg_says() {
    for (anchor, (px, py)) in ANCHOR_POINTS {
        for (gravity, (gx, gy)) in GRAVITY_SHIFTS {
            let positioner = Positioner {
                size: SIZE,
                anchor_rect: ANCHOR_RECT,
                anchor,
                gravity,
                offset: OFFSET,
            };
            let want = Rect::new(
                px + gx + OFFSET.x,
                py + gy + OFFSET.y,
                SIZE.width,
                SIZE.height,
            );
            assert_eq!(
                positioner.place(),
                want,
                "anchor {anchor:?}, gravity {gravity:?}"
            );
        }
    }
}

#[test]
fn an_odd_size_centred_on_a_point_puts_its_extra_pixel_right_and_below() {
    let positioner = Positioner {
        size: Size::new(31, 11),
        anchor_rect: ANCHOR_RECT,
        anchor: Anchor::Center,
        gravity: Anchor::Center,
        offset: Point::default(),
    };
    assert_eq!(positioner.place(), Rect::new(45, 45, 31, 11));
}

#[test]
fn placement_saturates_instead_of_wrapping_at_the_edges_of_i32() {
    let positioner = Positioner {
        size: Size::new(10, 10),
        anchor_rect: Rect::new(i32::MAX, i32::MIN, 0, 0),
        anchor: Anchor::BottomRight,
        gravity: Anchor::TopLeft,
        offset: Point::new(i32::MAX, i32::MIN),
    };
    let placed = positioner.place();
    assert_eq!(placed.origin, Point::new(i32::MAX, i32::MIN));
}
