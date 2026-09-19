//! Per-surface damage: the rectangles that changed since the last frame.

use crate::geometry::Rect;

/// Most rectangles a [`Damage`] keeps before it merges them into one.
///
/// Past this, one bounding rectangle is cheaper to send than many small ones, and memory
/// stays bounded whatever the app does.
pub const MAX_DAMAGE_RECTS: usize = 16;

/// The changed rectangles of one surface, at most [`MAX_DAMAGE_RECTS`] of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Damage {
    rects: Vec<Rect>,
}

impl Damage {
    /// Adds a changed rectangle. An empty rectangle is ignored.
    ///
    /// When the list is full, everything held and `rect` collapse into one bounding rectangle.
    pub fn push(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        if self.rects.len() >= MAX_DAMAGE_RECTS {
            let merged = self.bounds().map_or(rect, |held| held.union(rect));
            self.rects.clear();
            self.rects.push(merged);
            return;
        }
        self.rects.push(rect);
    }

    /// The smallest rectangle that holds all the damage, or `None` when there is none.
    pub fn bounds(&self) -> Option<Rect> {
        self.rects.iter().copied().reduce(Rect::union)
    }

    /// The rectangles, oldest first.
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// How many rectangles are held.
    pub fn len(&self) -> usize {
        self.rects.len()
    }

    /// True when nothing changed.
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// Takes every rectangle, leaving none.
    pub fn take(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.rects)
    }

    /// Drops every rectangle.
    pub fn clear(&mut self) {
        self.rects.clear();
    }
}

#[cfg(test)]
mod tests {
    use crate::{Damage, MAX_DAMAGE_RECTS, Rect};

    #[test]
    fn empty_rectangles_are_not_damage() {
        let mut damage = Damage::default();
        damage.push(Rect::new(5, 5, 0, 10));
        assert!(damage.is_empty());
    }

    #[test]
    fn a_full_list_collapses_into_its_bounds() {
        let mut damage = Damage::default();
        for i in 0..MAX_DAMAGE_RECTS {
            let x = i32::try_from(i * 10).expect("small");
            damage.push(Rect::new(x, 0, 5, 5));
        }
        assert_eq!(damage.len(), MAX_DAMAGE_RECTS);
        damage.push(Rect::new(0, 100, 5, 5));
        assert_eq!(damage.rects(), &[Rect::new(0, 0, 155, 105)]);
    }
}
