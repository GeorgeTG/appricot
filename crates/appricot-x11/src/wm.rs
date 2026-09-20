//! The backend's window-manager bookkeeping: tracking, placement and size hints.
//!
//! # Wave 1 layout decision: toplevels kept apart in a large root (revisit after the spike)
//!
//! `docs/architecture.md` §4.2 leaves open whether the X root follows the host's viewport or
//! the streamer keeps X toplevels apart in a large root. This crate resolves it for wave 1
//! in the second way, because of a measured X11 fact (§3.2): an XTEST click lands on
//! whichever window is on top at that point. Windows that never overlap need no stacking
//! management for input to be correct, so pointer injection stays a plain coordinate
//! computation. The cost is that popups which flip at screen edges (the first option's
//! argument) keep flipping at the X root's edges, which no user sees directly — the host
//! clamps what it shows. **Revisit after `l1-spike-x11-capture`**, which logs how the pilot
//! application's override-redirect windows actually place.
//!
//! [`Layout`] implements the policy: toplevels are placed left to right in rows inside the
//! root, separated by [`GAP`] pixels, wrapping to a new row, and starting over at the origin
//! when the root runs out. Removals are not reclaimed; a session that maps hundreds of
//! toplevels eventually overlaps, which is accepted for wave 1.

use std::collections::HashMap;

use appricot_core::{Rect, Role, Size, SurfaceId};

/// Pixels left between neighbouring toplevels, so "apart" is visible and popups at window
/// edges do not fight each other.
pub(crate) const GAP: i32 = 16;

/// Where the next toplevel goes: a fill cursor over the root, left to right, top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layout {
    next_x: i32,
    next_y: i32,
    row_height: u32,
}

impl Layout {
    /// A layout that starts placing at the root's origin.
    pub(crate) fn new() -> Self {
        Self {
            next_x: 0,
            next_y: 0,
            row_height: 0,
        }
    }

    /// Clamps `requested` into `root` and returns where the window goes and the size it
    /// gets.
    pub(crate) fn place(&mut self, requested: Size, root: Size) -> Rect {
        let size = Size::new(
            requested.width.min(root.width).max(1),
            requested.height.min(root.height).max(1),
        );

        // Does it still fit in this row? If not, wrap. Does it fit in the next row? If not,
        // the root is exhausted and the layout starts over at the origin.
        if self
            .next_x
            .saturating_add(i32::try_from(size.width).unwrap_or(i32::MAX))
            > i32::try_from(root.width).unwrap_or(i32::MAX)
        {
            self.next_x = 0;
            self.next_y = self
                .next_y
                .saturating_add(i32::try_from(self.row_height).unwrap_or(0))
                .saturating_add(GAP);
            self.row_height = 0;
        }
        if self
            .next_y
            .saturating_add(i32::try_from(size.height).unwrap_or(i32::MAX))
            > i32::try_from(root.height).unwrap_or(i32::MAX)
        {
            self.next_x = 0;
            self.next_y = 0;
            self.row_height = 0;
        }

        let placed = Rect::new(self.next_x, self.next_y, size.width, size.height);
        self.next_x = self
            .next_x
            .saturating_add(i32::try_from(size.width).unwrap_or(0))
            .saturating_add(GAP);
        self.row_height = self.row_height.max(size.height);
        placed
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::new()
    }
}

/// The size parts of `WM_NORMAL_HINTS` the backend clamps with.
///
/// Parsed from the raw property bytes: `[flags, x, y, width, height, min_w, min_h, max_w,
/// max_h, ...]` as 32-bit values, with `flags` bits `PMinSize = 0x10` and `PMaxSize = 0x20`
/// saying which fields are real (ICCCM). The byte order is the requesting client's native
/// order; X11 has no wire conversion for format-32 properties, and both the dev container
/// and every deployment target of this crate are little-endian.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SizeHints {
    /// The window's minimum size, when `PMinSize` is set.
    pub min: Option<(u32, u32)>,
    /// The window's maximum size, when `PMaxSize` is set.
    pub max: Option<(u32, u32)>,
}

impl SizeHints {
    /// Reads the hints out of the property's raw bytes. Anything malformed becomes "no
    /// hints"; a window's size hints are advice, never a reason to fail.
    pub(crate) fn parse(raw: &[u8]) -> Self {
        if raw.len() < 4 || !raw.len().is_multiple_of(4) {
            return Self::default();
        }
        let words: Vec<u32> = raw
            .chunks_exact(4)
            .map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let Some(&flags) = words.first() else {
            return Self::default();
        };
        let field = |i: usize| -> Option<u32> { words.get(i).copied().filter(|v| *v > 0) };
        Self {
            min: (flags & 0x10 != 0)
                .then(|| field(5).zip(field(6)))
                .flatten(),
            max: (flags & 0x20 != 0)
                .then(|| field(7).zip(field(8)))
                .flatten(),
        }
    }

    /// Clamps a proposed size against the hints, then against the root.
    pub(crate) fn clamp(self, requested: Size, root: Size) -> Size {
        let (mut width, mut height) = (requested.width, requested.height);
        if let Some((min_w, min_h)) = self.min {
            width = width.max(min_w);
            height = height.max(min_h);
        }
        if let Some((max_w, max_h)) = self.max {
            width = width.min(max_w);
            height = height.min(max_h);
        }
        // The root always wins: no configure the backend applies may exceed it.
        width = width.min(root.width).max(1);
        height = height.min(root.height).max(1);
        Size::new(width, height)
    }
}

/// How a window came under the backend's eye.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowKind {
    /// A toplevel the backend manages: it maps through `MapRequest`, and the backend places
    /// and configures it.
    Managed,
    /// An override-redirect window the backend never manages, only watches: a popup.
    OverrideRedirect,
}

/// Everything the backend remembers about one X window.
#[derive(Debug, Clone)]
pub(crate) struct TrackedWindow {
    /// The surface the window is shown as.
    pub id: SurfaceId,
    /// The X window.
    pub window: u32,
    /// How it is managed.
    pub kind: WindowKind,
    /// The role the surface was created with.
    pub role: Role,
    /// The window's geometry in root coordinates, as last seen or applied.
    pub geometry: Rect,
    /// The size hints, as last read.
    pub hints: SizeHints,
    /// Whether `WM_PROTOCOLS` lists `WM_DELETE_WINDOW`.
    pub has_delete: bool,
    /// Whether `WM_PROTOCOLS` lists `WM_TAKE_FOCUS`.
    pub take_focus: bool,
    /// `_NET_WM_NAME`, the preferred (UTF-8) title.
    pub title_net: Option<String>,
    /// `WM_NAME`, the Latin-1 fallback title.
    pub title_name: Option<String>,
    /// `WM_CLASS`'s second field (`res_class`), the app id.
    pub app_id: String,
    /// The Damage object watching this window, created at map.
    pub damage: Option<u32>,
    /// The named Composite pixmap of this window, and the size it was named at.
    pub pixmap: Option<(u32, Size)>,
}

impl TrackedWindow {
    /// The title metadata carries: `_NET_WM_NAME` when set, else `WM_NAME`.
    pub(crate) fn title(&self) -> String {
        self.title_net
            .clone()
            .or_else(|| self.title_name.clone())
            .unwrap_or_default()
    }

    /// True when the two sizes differ, which is what makes a `Resized` event.
    pub(crate) fn size_changed(&self, size: Size) -> bool {
        self.geometry.size != size
    }
}

/// The backend's window table: every tracked window by X window and by surface id.
#[derive(Debug, Default)]
pub(crate) struct WindowTable {
    by_window: HashMap<u32, TrackedWindow>,
    next_surface: u32,
}

impl WindowTable {
    /// An empty table; surface ids start at 1 and are never reused.
    pub(crate) fn new() -> Self {
        Self {
            by_window: HashMap::new(),
            next_surface: 1,
        }
    }

    /// Registers a window and mints its surface id.
    pub(crate) fn insert(&mut self, window: u32, tracked: TrackedWindow) -> SurfaceId {
        let id = SurfaceId::new(self.next_surface);
        self.next_surface += 1;
        self.by_window.insert(window, tracked);
        id
    }

    /// Mints the id the next [`WindowTable::insert`] will hand out. The caller fills
    /// `TrackedWindow.id` with it before inserting.
    pub(crate) fn peek_next_surface(&self) -> SurfaceId {
        SurfaceId::new(self.next_surface)
    }

    /// The window tracked under `window`, if any.
    pub(crate) fn get(&self, window: u32) -> Option<&TrackedWindow> {
        self.by_window.get(&window)
    }

    /// Mutable lookup.
    pub(crate) fn get_mut(&mut self, window: u32) -> Option<&mut TrackedWindow> {
        self.by_window.get_mut(&window)
    }

    /// The tracked window that surface `id` names, if any.
    pub(crate) fn by_surface(&self, id: SurfaceId) -> Option<&TrackedWindow> {
        self.by_window.values().find(|t| t.id == id)
    }

    /// Mutable lookup by surface id.
    pub(crate) fn by_surface_mut(&mut self, id: SurfaceId) -> Option<&mut TrackedWindow> {
        self.by_window.values_mut().find(|t| t.id == id)
    }

    /// Forgets the window and hands its bookkeeping back, so the caller can free its Damage
    /// object and named pixmap.
    pub(crate) fn remove(&mut self, window: u32) -> Option<TrackedWindow> {
        self.by_window.remove(&window)
    }

    /// True while `window` is tracked.
    pub(crate) fn contains(&self, window: u32) -> bool {
        self.by_window.contains_key(&window)
    }
}

#[cfg(test)]
mod tests {
    use appricot_core::{Point, Rect, Size};

    use super::{GAP, Layout, SizeHints};

    fn root() -> Size {
        Size::new(1400, 900)
    }

    #[test]
    fn the_first_window_gets_the_origin() {
        let mut layout = Layout::new();
        assert_eq!(
            layout.place(Size::new(200, 150), root()),
            Rect::new(0, 0, 200, 150)
        );
    }

    #[test]
    fn the_second_window_sits_beside_the_first() {
        let mut layout = Layout::new();
        layout.place(Size::new(200, 150), root());
        let second = layout.place(Size::new(100, 50), root());
        assert_eq!(
            second,
            Rect::new(200 + GAP, 0, 100, 50),
            "windows never overlap: {second:?}"
        );
        assert_ne!(second.origin, Point::default());
    }

    #[test]
    fn a_window_that_does_not_fit_wraps_to_the_next_row() {
        let mut layout = Layout::new();
        let first = layout.place(Size::new(1300, 100), root());
        let second = layout.place(Size::new(200, 50), root());
        assert_eq!(first, Rect::new(0, 0, 1300, 100));
        assert_eq!(second, Rect::new(0, 100 + GAP, 200, 50));
    }

    #[test]
    fn a_window_taller_than_the_root_is_clamped() {
        let mut layout = Layout::new();
        assert_eq!(
            layout.place(Size::new(400, 5000), root()),
            Rect::new(0, 0, 400, 900)
        );
    }

    #[test]
    fn when_the_root_runs_out_the_layout_starts_over() {
        let mut layout = Layout::new();
        // Two 600x800 windows fit side by side in 1400x900; the third fits neither the
        // row (1232 + 600 > 1400) nor a new one (816 + 800 > 900).
        layout.place(Size::new(600, 800), root());
        let second = layout.place(Size::new(600, 800), root());
        assert_eq!(second, Rect::new(616, 0, 600, 800));
        let third = layout.place(Size::new(600, 800), root());
        assert_eq!(
            third,
            Rect::new(0, 0, 600, 800),
            "exhausted root overlaps rather than failing"
        );
    }

    #[test]
    fn zero_sized_requests_get_one_pixel() {
        let mut layout = Layout::new();
        assert_eq!(layout.place(Size::new(0, 0), root()), Rect::new(0, 0, 1, 1));
    }

    fn hints(raw: &[u32]) -> SizeHints {
        let bytes: Vec<u8> = raw.iter().flat_map(|w| w.to_ne_bytes()).collect();
        SizeHints::parse(&bytes)
    }

    #[test]
    fn min_and_max_hints_are_read_from_their_flagged_fields() {
        // flags | x | y | w | h | min_w | min_h | max_w | max_h
        let parsed = hints(&[0x30, 0, 0, 0, 0, 100, 60, 800, 600]);
        assert_eq!(parsed.min, Some((100, 60)));
        assert_eq!(parsed.max, Some((800, 600)));
    }

    #[test]
    fn unflagged_hints_are_ignored() {
        let parsed = hints(&[0x08, 0, 0, 640, 480, 100, 60, 800, 600]);
        assert_eq!(parsed.min, None);
        assert_eq!(parsed.max, None);
    }

    #[test]
    fn malformed_hints_mean_no_hints() {
        assert_eq!(SizeHints::parse(&[]), SizeHints::default());
        assert_eq!(SizeHints::parse(&[0; 3]), SizeHints::default());
    }

    #[test]
    fn clamping_honours_min_then_root() {
        let h = SizeHints {
            min: Some((300, 200)),
            max: Some((2000, 2000)),
        };
        assert_eq!(h.clamp(Size::new(100, 50), root()), Size::new(300, 200));
        // The root wins over a minimum that does not fit it.
        assert_eq!(
            h.clamp(Size::new(10, 10), Size::new(150, 900)),
            Size::new(150, 200)
        );
        assert_eq!(h.clamp(Size::new(5000, 10), root()), Size::new(1400, 200));
    }
}
