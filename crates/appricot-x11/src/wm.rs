//! The backend's window-manager bookkeeping: tracking, placement and size hints.
//!
//! # Layout: toplevels overlap as little as the root allows, and nothing relies on it
//!
//! `docs/architecture.md` §4.2 leaves open whether the X root follows the host's viewport or
//! the streamer spreads X toplevels over the root. Wave 1 spreads them: [`place_toplevel`]
//! puts each new toplevel where it overlaps the live toplevels least, reading candidate
//! positions top to bottom, left to right, and leaving [`GAP`] pixels between neighbours when
//! there is room. A freed place is reused as soon as its window is gone.
//!
//! The root the repository configures (Xvfb at 1400x900, `docker/dev/entrypoint.sh`) is too
//! small to keep realistic windows apart: two 800x600 toplevels, or a 1200x700 main window and
//! a 400x300 dialog, cannot both fit without overlapping. Overlap is therefore the normal case
//! from the second window on, and **no correctness depends on the layout**. An XTEST click
//! lands on whichever window is on top at that point (§3.2), so the input path raises its
//! target before it presses. The layout only keeps the overlap, and so the restacking, small.
//! A larger root keeps more windows apart, but that is a deployment choice, not a requirement.
//!
//! Popups that flip at screen edges keep flipping at the X root's edges, which no user sees
//! directly: the host clamps what it shows.

use std::collections::HashMap;

use appricot_core::{Rect, Role, Size, SurfaceId};

/// Pixels left between neighbouring toplevels when the root has room for them, so popups at
/// window edges do not fight each other.
pub(crate) const GAP: i32 = 16;

/// The widest surface the wire carries.
///
/// This crate may not depend on `appricot-proto`, and `appricot-core` does not re-export the
/// row, so the value is mirrored here.
// mirrors appricot_proto::limits::MAX_SURFACE_WIDTH
pub(crate) const MAX_SURFACE_WIDTH: u32 = 1920;

/// The tallest surface the wire carries; mirrored like [`MAX_SURFACE_WIDTH`].
// mirrors appricot_proto::limits::MAX_SURFACE_HEIGHT
pub(crate) const MAX_SURFACE_HEIGHT: u32 = 1200;

/// How many live toplevels one placement looks at. It bounds the work per map; a session
/// past the core's surface cap has windows the host never sees anyway.
const MAX_PLACEMENT_NEIGHBOURS: usize = 64;

/// The largest size the backend applies or reports: the root, and never past the wire's
/// surface caps. Every size taken from the X server is cut to it before it leaves the
/// backend, because an over-cap size would make the streamer refuse the message.
pub(crate) fn size_bound(root: Size) -> Size {
    Size::new(
        root.width.clamp(1, MAX_SURFACE_WIDTH),
        root.height.clamp(1, MAX_SURFACE_HEIGHT),
    )
}

/// `size` cut to `bound`, and at least one pixel each way.
pub(crate) fn bounded_size(size: Size, bound: Size) -> Size {
    Size::new(
        size.width.min(bound.width).max(1),
        size.height.min(bound.height).max(1),
    )
}

/// The placement policy: clamps `requested` into `root` and returns where a new toplevel
/// goes and the size it gets, given the toplevels already on the root (`occupied`).
///
/// It keeps no state: every placement reads the live toplevels afresh, so the place of a
/// window that went away is free again at once.
///
/// The candidates are the root's corners and the places beside, below, left of and above
/// each live toplevel, [`GAP`] pixels away, pushed inside the root. The one with the least
/// overlap wins, the topmost and then the leftmost on a tie. The overlap is measured against
/// the live toplevels grown by [`GAP`], so a free place that also keeps the gap beats one
/// that touches a neighbour.
pub(crate) fn place_toplevel(requested: Size, root: Size, occupied: &[Rect]) -> Rect {
    let size = Size::new(
        requested.width.min(root.width).max(1),
        requested.height.min(root.height).max(1),
    );
    let occupied = &occupied[..occupied.len().min(MAX_PLACEMENT_NEIGHBOURS)];
    let width = i64::from(size.width);
    let height = i64::from(size.height);
    let max_x = (i64::from(root.width) - width).max(0);
    let max_y = (i64::from(root.height) - height).max(0);
    let gap = i64::from(GAP);

    let mut xs = vec![0, max_x];
    let mut ys = vec![0, max_y];
    for rect in occupied {
        let (left, top, right, bottom) = edges(*rect);
        xs.extend([left, right + gap, left - gap - width]);
        ys.extend([top, bottom + gap, top - gap - height]);
    }
    for v in &mut xs {
        *v = (*v).clamp(0, max_x);
    }
    for v in &mut ys {
        *v = (*v).clamp(0, max_y);
    }
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();

    let mut best: Option<(i64, i64, i64)> = None;
    for &y in &ys {
        for &x in &xs {
            let cost: i64 = occupied
                .iter()
                .map(|rect| {
                    let (left, top, right, bottom) = edges(*rect);
                    overlap(
                        (x, y, x + width, y + height),
                        (left - gap, top - gap, right + gap, bottom + gap),
                    )
                })
                .sum();
            // ys and xs are sorted, so the first candidate at a cost is the topmost, then
            // the leftmost, of that cost.
            if best.is_none_or(|(best_cost, _, _)| cost < best_cost) {
                best = Some((cost, x, y));
            }
        }
    }
    let (_, x, y) = best.unwrap_or((0, 0, 0));
    Rect::new(
        i32::try_from(x).unwrap_or(0),
        i32::try_from(y).unwrap_or(0),
        size.width,
        size.height,
    )
}

/// A rectangle's edges as `(left, top, right, bottom)`, wide enough never to overflow.
fn edges(rect: Rect) -> (i64, i64, i64, i64) {
    let left = i64::from(rect.origin.x);
    let top = i64::from(rect.origin.y);
    (
        left,
        top,
        left + i64::from(rect.size.width),
        top + i64::from(rect.size.height),
    )
}

/// The area two rectangles, given by their edges, share.
fn overlap(a: (i64, i64, i64, i64), b: (i64, i64, i64, i64)) -> i64 {
    let width = (a.2.min(b.2) - a.0.max(b.0)).max(0);
    let height = (a.3.min(b.3) - a.1.max(b.1)).max(0);
    width * height
}

/// Decodes a UTF-8 text property that a bounded read may have cut.
///
/// A property is read up to a byte count, and the cut can split the last character. That
/// incomplete tail is dropped, so a long title keeps everything before it. Invalid bytes
/// anywhere else become U+FFFD: a title is shown as text, and a bad byte must not blank it.
pub(crate) fn decode_utf8_cut(raw: &[u8]) -> String {
    String::from_utf8_lossy(without_cut_char(raw)).into_owned()
}

/// `raw` without a last character the end of the buffer cut short.
fn without_cut_char(raw: &[u8]) -> &[u8] {
    let len = raw.len();
    // The lead byte of the last character sits at most three continuation bytes back.
    for back in 1..=len.min(4) {
        let byte = raw[len - back];
        if byte & 0xc0 == 0x80 {
            continue; // a continuation byte: keep looking for the lead
        }
        let needed = match byte {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            // Not a lead byte at all: invalid, not cut. The lossy decode handles it.
            _ => return raw,
        };
        return if needed > back {
            &raw[..len - back]
        } else {
            raw
        };
    }
    raw
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

    /// Clamps a proposed size against the hints, then against `bound`: the root, capped to
    /// the wire (see [`size_bound`]).
    pub(crate) fn clamp(self, requested: Size, bound: Size) -> Size {
        let (mut width, mut height) = (requested.width, requested.height);
        if let Some((min_w, min_h)) = self.min {
            width = width.max(min_w);
            height = height.max(min_h);
        }
        if let Some((max_w, max_h)) = self.max {
            width = width.min(max_w);
            height = height.min(max_h);
        }
        // The bound always wins: no configure the backend applies may exceed it.
        width = width.min(bound.width).max(1);
        height = height.min(bound.height).max(1);
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
    /// The window's geometry in root coordinates, as last seen or applied: the origin is
    /// the inside corner, past any border, and the size is the inside size. It is the X
    /// server's size, which may exceed what the backend reports (see [`size_bound`]).
    pub geometry: Rect,
    /// The X border width. The named pixmap includes the border, so a capture reads past
    /// it. The backend sets it to zero on the toplevels it manages.
    pub border: u16,
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

    /// Registers a window, mints its surface id and writes it into `tracked.id`, so the
    /// table's id and the tracked window's id cannot disagree.
    pub(crate) fn insert(&mut self, window: u32, mut tracked: TrackedWindow) -> SurfaceId {
        let id = SurfaceId::new(self.next_surface);
        self.next_surface += 1;
        tracked.id = id;
        self.by_window.insert(window, tracked);
        id
    }

    /// The id the next [`WindowTable::insert`] will hand out. `insert` writes the id
    /// itself; this only fills the field before that.
    pub(crate) fn peek_next_surface(&self) -> SurfaceId {
        SurfaceId::new(self.next_surface)
    }

    /// The geometry of every managed toplevel: what a new toplevel is placed around.
    pub(crate) fn managed_geometries(&self) -> Vec<Rect> {
        self.by_window
            .values()
            .filter(|t| t.kind == WindowKind::Managed)
            .map(|t| t.geometry)
            .collect()
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

    use super::{
        GAP, MAX_SURFACE_HEIGHT, MAX_SURFACE_WIDTH, SizeHints, bounded_size, decode_utf8_cut,
        place_toplevel, size_bound,
    };

    fn root() -> Size {
        Size::new(1400, 900)
    }

    /// Places `sizes` one after another, each around the ones before it.
    fn place_all(sizes: &[Size]) -> Vec<Rect> {
        let mut placed: Vec<Rect> = Vec::new();
        for size in sizes {
            let rect = place_toplevel(*size, root(), &placed);
            placed.push(rect);
        }
        placed
    }

    fn overlap_area(a: Rect, b: Rect) -> u64 {
        a.intersection(b)
            .map_or(0, |r| u64::from(r.size.width) * u64::from(r.size.height))
    }

    #[test]
    fn the_first_window_gets_the_origin() {
        assert_eq!(
            place_toplevel(Size::new(200, 150), root(), &[]),
            Rect::new(0, 0, 200, 150)
        );
    }

    #[test]
    fn the_second_window_sits_beside_the_first() {
        let placed = place_all(&[Size::new(200, 150), Size::new(100, 50)]);
        assert_eq!(
            placed[1],
            Rect::new(200 + GAP, 0, 100, 50),
            "windows that fit never overlap: {placed:?}"
        );
        assert_ne!(placed[1].origin, Point::default());
    }

    #[test]
    fn a_window_that_does_not_fit_beside_goes_below() {
        let placed = place_all(&[Size::new(1300, 100), Size::new(200, 50)]);
        assert_eq!(placed[0], Rect::new(0, 0, 1300, 100));
        assert_eq!(placed[1], Rect::new(0, 100 + GAP, 200, 50));
    }

    #[test]
    fn a_window_taller_than_the_root_is_clamped() {
        assert_eq!(
            place_toplevel(Size::new(400, 5000), root(), &[]),
            Rect::new(0, 0, 400, 900)
        );
    }

    #[test]
    fn two_800x600_windows_on_the_default_root_overlap_as_little_as_it_allows() {
        // 800 + 16 + 800 > 1400 and 600 + 16 + 600 > 900: they cannot be kept apart. The
        // second goes to the far corner, never onto the first.
        let placed = place_all(&[Size::new(800, 600), Size::new(800, 600)]);
        assert_eq!(placed[0], Rect::new(0, 0, 800, 600));
        assert_eq!(placed[1], Rect::new(600, 300, 800, 600));
        assert_eq!(overlap_area(placed[0], placed[1]), 200 * 300);
    }

    #[test]
    fn a_dialog_beside_a_large_main_window_takes_the_least_covered_corner() {
        // A 1200x700 main window leaves strips 200 wide and 200 tall: a 400x300 dialog fits
        // in neither, so it overlaps the main window's corner and nothing more.
        let placed = place_all(&[Size::new(1200, 700), Size::new(400, 300)]);
        assert_eq!(placed[0], Rect::new(0, 0, 1200, 700));
        assert_eq!(placed[1], Rect::new(1000, 600, 400, 300));
        assert_eq!(overlap_area(placed[0], placed[1]), 200 * 100);
    }

    #[test]
    fn small_windows_fill_the_root_without_overlap() {
        let sizes = [Size::new(400, 300); 6];
        let placed = place_all(&sizes);
        for (i, a) in placed.iter().enumerate() {
            for b in &placed[i + 1..] {
                assert_eq!(overlap_area(*a, *b), 0, "{a:?} and {b:?} overlap");
            }
        }
    }

    #[test]
    fn the_place_of_a_window_that_went_away_is_reused() {
        let first = place_toplevel(Size::new(800, 600), root(), &[]);
        // The first window is gone: the next one takes the origin again.
        assert_eq!(place_toplevel(Size::new(800, 600), root(), &[]), first);
    }

    #[test]
    fn zero_sized_requests_get_one_pixel() {
        assert_eq!(
            place_toplevel(Size::new(0, 0), root(), &[]),
            Rect::new(0, 0, 1, 1)
        );
    }

    #[test]
    fn the_size_bound_is_the_root_capped_to_the_wire() {
        assert_eq!(size_bound(root()), root());
        assert_eq!(
            size_bound(Size::new(4000, 3000)),
            Size::new(MAX_SURFACE_WIDTH, MAX_SURFACE_HEIGHT)
        );
        let bound = size_bound(Size::new(4000, 3000));
        assert_eq!(
            bounded_size(Size::new(3000, 50), bound),
            Size::new(MAX_SURFACE_WIDTH, 50)
        );
        assert_eq!(bounded_size(Size::new(0, 0), bound), Size::new(1, 1));
    }

    #[test]
    fn a_title_cut_inside_a_character_keeps_what_came_before() {
        // "xα" is 78 CE B1; the read stopped after CE.
        assert_eq!(decode_utf8_cut(&[b'x', 0xce, 0xb1, b'y', 0xce]), "xαy");
        // A three-byte character cut after one or two bytes.
        assert_eq!(decode_utf8_cut(&[b'a', 0xe2, 0x82]), "a");
        assert_eq!(decode_utf8_cut(&[b'a', 0xe2]), "a");
        // A four-byte character cut after three.
        assert_eq!(decode_utf8_cut(&[b'a', 0xf0, 0x9f, 0x98]), "a");
        // Whole text is untouched.
        assert_eq!(decode_utf8_cut("Ελληνικά".as_bytes()), "Ελληνικά");
        assert_eq!(decode_utf8_cut(&[]), "");
    }

    #[test]
    fn invalid_bytes_inside_a_title_do_not_blank_it() {
        assert_eq!(decode_utf8_cut(&[b'a', 0xff, b'b']), "a\u{fffd}b");
        // A stray continuation byte at the end is invalid, not cut.
        assert_eq!(decode_utf8_cut(&[b'a', 0x80]), "a\u{fffd}");
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
