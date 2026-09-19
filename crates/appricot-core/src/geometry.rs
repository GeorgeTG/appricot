//! Integer geometry: points, sizes and rectangles, in pixels.

/// A position, in pixels. `x` grows to the right and `y` grows downwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Point {
    /// Horizontal position.
    pub x: i32,
    /// Vertical position.
    pub y: i32,
}

impl Point {
    /// The point at `(x, y)`.
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A width and a height, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Size {
    /// Width, in pixels.
    pub width: u32,
    /// Height, in pixels.
    pub height: u32,
}

impl Size {
    /// A size of `width` by `height`.
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// True when the size covers no pixel.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// An axis-aligned rectangle: a top-left corner and a size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// The top-left corner.
    pub origin: Point,
    /// The width and height.
    pub size: Size,
}

impl Rect {
    /// The rectangle whose top-left corner is `(x, y)`.
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            origin: Point::new(x, y),
            size: Size::new(width, height),
        }
    }

    /// True when the rectangle covers no pixel.
    pub const fn is_empty(self) -> bool {
        self.size.is_empty()
    }

    /// The part two rectangles share, or `None` when they do not overlap.
    pub fn intersection(self, other: Self) -> Option<Self> {
        let left = self.left().max(other.left());
        let top = self.top().max(other.top());
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= left || bottom <= top {
            return None;
        }
        Some(Self::from_edges(left, top, right, bottom))
    }

    /// The smallest rectangle that holds both. An empty rectangle adds nothing.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let left = self.left().min(other.left());
        let top = self.top().min(other.top());
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self::from_edges(left, top, right, bottom)
    }

    /// The left edge. Edges are `i64`, so `x + width` cannot overflow.
    pub(crate) fn left(self) -> i64 {
        i64::from(self.origin.x)
    }

    /// The top edge.
    pub(crate) fn top(self) -> i64 {
        i64::from(self.origin.y)
    }

    /// The right edge, one past the last column.
    pub(crate) fn right(self) -> i64 {
        self.left() + i64::from(self.size.width)
    }

    /// The bottom edge, one past the last row.
    pub(crate) fn bottom(self) -> i64 {
        self.top() + i64::from(self.size.height)
    }

    /// Builds a rectangle from its edges, saturating whatever does not fit.
    fn from_edges(left: i64, top: i64, right: i64, bottom: i64) -> Self {
        Self::new(
            saturate_i32(left),
            saturate_i32(top),
            saturate_u32(right - left),
            saturate_u32(bottom - top),
        )
    }
}

/// Converts to `i32`, clamping at its bounds instead of wrapping.
pub(crate) fn saturate_i32(v: i64) -> i32 {
    let clamped = v.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    i32::try_from(clamped).unwrap_or_default()
}

/// Converts to `u32`, clamping at its bounds instead of wrapping.
pub(crate) fn saturate_u32(v: i64) -> u32 {
    let clamped = v.clamp(0, i64::from(u32::MAX));
    u32::try_from(clamped).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::Rect;

    #[test]
    fn intersection_of_overlapping_rectangles() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersection(b), Some(Rect::new(5, 5, 5, 5)));
    }

    #[test]
    fn rectangles_that_only_touch_do_not_intersect() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(10, 0, 10, 10);
        assert_eq!(a.intersection(b), None);
    }

    #[test]
    fn union_covers_both_and_ignores_empty() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 5, 10, 10);
        assert_eq!(a.union(b), Rect::new(0, 0, 30, 15));
        assert_eq!(a.union(Rect::default()), a);
    }

    #[test]
    fn edges_do_not_overflow_at_the_limits() {
        let far = Rect::new(i32::MAX, i32::MAX, u32::MAX, u32::MAX);
        let near = Rect::new(i32::MIN, i32::MIN, 1, 1);
        let both = far.union(near);
        assert_eq!(both.origin.x, i32::MIN);
        assert_eq!(both.size.width, u32::MAX);
    }
}
