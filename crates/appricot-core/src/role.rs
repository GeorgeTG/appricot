//! Roles, and how a popup is placed relative to its parent.

use crate::geometry::{Point, Rect, Size, saturate_i32};
use crate::surface::SurfaceId;

/// What a surface is for. It is set when the surface is created and never changes.
///
/// Shaped after xdg-shell's `xdg_toplevel` and `xdg_popup`
/// (<https://wayland.app/protocols/xdg-shell>).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// A window the host shows as one of its own, inside host-drawn chrome.
    ///
    /// A toplevel may name another toplevel as its parent (a dialog); see
    /// [`Surface::set_toplevel_parent`](crate::Surface::set_toplevel_parent).
    Toplevel,
    /// A short-lived surface tied to a parent: a menu, a combo list, a tooltip. No chrome.
    Popup {
        /// The surface the popup belongs to. The popup goes when its parent goes.
        parent: SurfaceId,
        /// Where the popup goes, relative to its parent.
        positioner: Positioner,
    },
}

/// An edge or a corner of a rectangle, named as `xdg_positioner` names them.
///
/// A [`Positioner`] uses it twice: as the anchor, the point on the anchor rectangle the popup
/// hangs from, and as the gravity, the direction the popup grows from that point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Anchor {
    /// The centre. `xdg_positioner` calls it `none`.
    #[default]
    Center,
    /// The middle of the top edge.
    Top,
    /// The middle of the bottom edge.
    Bottom,
    /// The middle of the left edge.
    Left,
    /// The middle of the right edge.
    Right,
    /// The top-left corner.
    TopLeft,
    /// The bottom-left corner.
    BottomLeft,
    /// The top-right corner.
    TopRight,
    /// The bottom-right corner.
    BottomRight,
}

impl Anchor {
    /// The point this anchor names on `rect`, as `(x, y)`.
    fn point_on(self, rect: Rect) -> (i64, i64) {
        let x = match self {
            Self::Left | Self::TopLeft | Self::BottomLeft => rect.left(),
            Self::Right | Self::TopRight | Self::BottomRight => rect.right(),
            // `midpoint` instead of `(a + b) / 2`: same value, and it cannot overflow.
            Self::Center | Self::Top | Self::Bottom => i64::midpoint(rect.left(), rect.right()),
        };
        let y = match self {
            Self::Top | Self::TopLeft | Self::TopRight => rect.top(),
            Self::Bottom | Self::BottomLeft | Self::BottomRight => rect.bottom(),
            Self::Center | Self::Left | Self::Right => i64::midpoint(rect.top(), rect.bottom()),
        };
        (x, y)
    }

    /// Used as a gravity: the top-left corner of a popup of `size` that grows from `point`.
    fn grow_from(self, point: (i64, i64), size: Size) -> (i64, i64) {
        let (width, height) = (i64::from(size.width), i64::from(size.height));
        let x = match self {
            Self::Left | Self::TopLeft | Self::BottomLeft => point.0 - width,
            Self::Right | Self::TopRight | Self::BottomRight => point.0,
            Self::Center | Self::Top | Self::Bottom => point.0 - width / 2,
        };
        let y = match self {
            Self::Top | Self::TopLeft | Self::TopRight => point.1 - height,
            Self::Bottom | Self::BottomLeft | Self::BottomRight => point.1,
            Self::Center | Self::Left | Self::Right => point.1 - height / 2,
        };
        (x, y)
    }
}

/// Where a popup goes, relative to its parent: `xdg_positioner` in miniature.
///
/// Constraint adjustment (slide, flip or resize when the popup would leave the visible area)
/// is not modelled yet. The client clamps every popup to its parent's box plus a fixed margin
/// (docs/adr/0003, §5), whatever the positioner says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Positioner {
    /// The popup's size.
    pub size: Size,
    /// A rectangle in the parent's coordinates that the popup is placed against.
    pub anchor_rect: Rect,
    /// The point on `anchor_rect` the popup hangs from.
    pub anchor: Anchor,
    /// The direction the popup grows from the anchor point.
    pub gravity: Anchor,
    /// A last shift, applied after anchor and gravity.
    pub offset: Point,
}

impl Positioner {
    /// A popup whose top-left corner is at `at`, in its parent's coordinates.
    ///
    /// That is all an X11 override-redirect window tells a backend: its root position minus
    /// its parent's.
    pub fn at(at: Point, size: Size) -> Self {
        Self {
            size,
            anchor_rect: Rect::new(at.x, at.y, 0, 0),
            anchor: Anchor::TopLeft,
            gravity: Anchor::BottomRight,
            offset: Point::default(),
        }
    }

    /// The popup's rectangle in its parent's coordinates, before any constraint adjustment.
    pub fn place(&self) -> Rect {
        let hang = self.anchor.point_on(self.anchor_rect);
        let (left, top) = self.gravity.grow_from(hang, self.size);
        let origin = Point::new(
            saturate_i32(left + i64::from(self.offset.x)),
            saturate_i32(top + i64::from(self.offset.y)),
        );
        let size = self.size;
        Rect { origin, size }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Anchor, Point, Positioner, Rect, Size};

    #[test]
    fn a_popup_at_an_offset_starts_there() {
        let p = Positioner::at(Point::new(30, 40), Size::new(100, 20));
        assert_eq!(p.place(), Rect::new(30, 40, 100, 20));
    }

    #[test]
    fn a_menu_hangs_below_its_button() {
        // A 200x300 menu under a 50x20 button at (10, 5), flush with the button's left edge.
        let p = Positioner {
            size: Size::new(200, 300),
            anchor_rect: Rect::new(10, 5, 50, 20),
            anchor: Anchor::BottomLeft,
            gravity: Anchor::BottomRight,
            offset: Point::default(),
        };
        assert_eq!(p.place(), Rect::new(10, 25, 200, 300));
    }

    #[test]
    fn a_centred_tooltip_grows_upwards() {
        // A 40x10 tooltip centred above a 20x20 icon at (100, 100), 2 pixels higher still.
        let p = Positioner {
            size: Size::new(40, 10),
            anchor_rect: Rect::new(100, 100, 20, 20),
            anchor: Anchor::Top,
            gravity: Anchor::Top,
            offset: Point::new(0, -2),
        };
        assert_eq!(p.place(), Rect::new(90, 88, 40, 10));
    }
}
