//! What the client last received, per grid cell, so identical tiles need not be sent again.
//!
//! The M1 spike measured the pilot application damaging its whole window on every repaint —
//! two full-window Damage events a second for a blinking caret, about 1.1 Mbit/s and 15 tiles
//! per frame, while almost no pixel changed (docs/spike/findings-2026-09-23.md §2). This
//! cache is the
//! sender-side answer the spike proposed: before a tile goes out, it is compared with what the
//! surface last sent the client for that grid cell, and an identical one is omitted. The
//! client does not change — a client that draws only the tiles it receives is already correct,
//! because it applies frames strictly in sequence order, so a cell it is not told about still
//! shows the pixels of the last tile that named it.
//!
//! # What is compared
//!
//! The **encoded payload**, not the pixels: the capture was encoded anyway on the way out, the
//! encoders are deterministic (the same pixels under the same preferred codec produce the same
//! payload, `appricot_encode`), and a byte-identical payload under the same codec and the same
//! rectangle is the same pixels on the client's canvas. The cache therefore costs one full
//! frame's wire bytes per surface — on the pilot's flat UI, where QOI takes a tile to 2 % of
//! RAW (findings §4), about 68 KiB for a 1200x700 window (internal benchmark, September 2026;
//! the method was `bench.md` over every tile of a `startup.spike` run). The hard ceiling is
//! the RAW bytes of the surface, paid only when tiles do not compress at all — the cache never
//! exceeds what one complete frame of that surface costs on the wire. The rejected
//! alternatives: a cached RAW copy of every cell costs the full 3.4 MiB of a 1200x700 surface
//! before compression, on a streamer measured at 6-7 MiB RSS (findings §3); per-tile hashes
//! still read every pixel and add a collision risk a byte compare does not have.
//!
//! # When it is invalidated
//!
//! A tile is only ever *omitted* when the cached entry names the same grid cell, the same
//! rectangle, the same codec and the same payload. Anything else — a different damage shape
//! cutting the cell differently, a size change re-cutting the grid — makes the tile a miss and
//! it is sent. On top of that, [`SentTiles::begin_full_redraw`] drops every entry before a
//! `full_redraw` frame (the first frame of a surface, and the resynchronisation after a
//! resume, always carry every tile), and [`SentTiles::note_size`] drops every entry when the
//! surface's size changed, so the first frame at a new size is complete however the damage
//! fell. The cache lives and dies with the connection's pump: a parked session's resume is a
//! full redraw into an empty cache, so no state of the dead connection can leak into the new
//! one.

use std::collections::HashMap;

use appricot_core::{Rect, Size, SurfaceId};
use appricot_encode::TILE_SIZE;

/// The last tile this surface sent the client for one grid cell, exactly as it went out.
///
/// Storing the rectangle matters as much as storing the payload: a grid cell's tile is the
/// intersection of the damage with the cell ([`appricot_encode::cut_into_tiles`]), so the same
/// cell can be cut to different rectangles by different damage shapes, and a smaller piece of
/// a cell must not be compared against a bigger one. A codec change with identical bytes is
/// not a tile the client already holds either.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SentTile {
    /// The rectangle the payload was cut to, inside the cell.
    rect: Rect,
    /// The wire codec id of `data`.
    codec: u32,
    /// The encoded pixels exactly as they were sent.
    data: Vec<u8>,
}

/// The tiles one surface last sent the client, keyed by grid cell.
///
/// One of these lives per surface in the session's pump for as long as the connection lives;
/// see the [module documentation](self) for what is compared and when it is invalidated.
#[derive(Debug, Default, Clone)]
pub struct SentTiles {
    /// The surface size the entries were cut against; `None` until the first frame.
    size: Option<Size>,
    /// One entry per grid cell that received a tile, `(column, row)` of the tile grid.
    cells: HashMap<(i32, i32), SentTile>,
}

impl SentTiles {
    /// Drops every entry when the surface's size changed, so nothing cut against the old grid
    /// survives into a frame at the new size.
    pub fn note_size(&mut self, size: Size) {
        if self.size != Some(size) {
            self.cells.clear();
            self.size = Some(size);
        }
    }

    /// Starts a full redraw: every tile of the surface is about to be sent, so the comparison
    /// starts over from what this frame carries.
    pub fn begin_full_redraw(&mut self, size: Size) {
        self.cells.clear();
        self.size = Some(size);
    }

    /// Whether `rect` under `codec` carrying `data` is exactly what the client already holds
    /// for its grid cell, so sending it would change nothing.
    pub fn same_as_sent(&self, rect: Rect, codec: u32, data: &[u8]) -> bool {
        self.cell(rect)
            .is_some_and(|sent| sent.rect == rect && sent.codec == codec && sent.data == data)
    }

    /// Records `rect` under `codec` carrying `data` as what the client holds for its grid cell
    /// once this tile goes out.
    pub fn record(&mut self, rect: Rect, codec: u32, data: &[u8]) {
        self.cells.insert(
            cell_of(rect),
            SentTile {
                rect,
                codec,
                data: data.to_vec(),
            },
        );
    }

    /// The grid cell a rectangle belongs to, `(column, row)` on the [`TILE_SIZE`] grid.
    fn cell(&self, rect: Rect) -> Option<&SentTile> {
        self.cells.get(&cell_of(rect))
    }
}

/// The grid cell a rectangle belongs to: the tile grid never crosses a grid line
/// ([`appricot_encode::cut_into_tiles`]), so the origin alone names the cell.
fn cell_of(rect: Rect) -> (i32, i32) {
    let s = i32::try_from(TILE_SIZE).expect("the tile size fits an i32");
    (rect.origin.x.div_euclid(s), rect.origin.y.div_euclid(s))
}

/// Which surfaces' sent tiles the pump is holding, one map entry per living surface.
pub type SentTilesPerSurface = HashMap<SurfaceId, SentTiles>;

#[cfg(test)]
mod tests {
    use super::{SentTiles, cell_of};

    use appricot_core::{Rect, Size};

    fn tile(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn a_recorded_tile_is_the_same_only_as_itself() {
        let mut sent = SentTiles::default();
        sent.note_size(Size::new(300, 300));
        sent.record(tile(0, 0, 256, 256), 1, &[1, 2, 3]);

        assert!(sent.same_as_sent(tile(0, 0, 256, 256), 1, &[1, 2, 3]));

        // A different payload for the same cell: changed pixels.
        assert!(!sent.same_as_sent(tile(0, 0, 256, 256), 1, &[1, 2, 4]));
        // A different codec for byte-identical data: not what the client holds.
        assert!(!sent.same_as_sent(tile(0, 0, 256, 256), 2, &[1, 2, 3]));
        // A different rectangle of the same cell: a smaller piece is not the whole cell.
        assert!(!sent.same_as_sent(tile(0, 0, 10, 10), 1, &[1, 2, 3]));
        // A cell that never received a tile.
        assert!(!sent.same_as_sent(tile(256, 0, 44, 256), 1, &[1, 2, 3]));
    }

    #[test]
    fn recording_replaces_the_cells_entry() {
        let mut sent = SentTiles::default();
        sent.note_size(Size::new(300, 300));
        // The whole cell went out; then damage cut a piece of it, and the piece went out.
        sent.record(tile(0, 0, 256, 256), 1, &[9]);
        sent.record(tile(10, 10, 5, 5), 1, &[8]);
        assert!(sent.same_as_sent(tile(10, 10, 5, 5), 1, &[8]));
        // The whole-cell entry is gone: cutting it again must be a miss, not a stale hit.
        assert!(!sent.same_as_sent(tile(0, 0, 256, 256), 1, &[9]));
    }

    #[test]
    fn a_size_change_drops_every_entry() {
        let mut sent = SentTiles::default();
        sent.note_size(Size::new(300, 300));
        sent.record(tile(0, 0, 300, 300), 1, &[7]);
        assert!(sent.same_as_sent(tile(0, 0, 300, 300), 1, &[7]));

        sent.note_size(Size::new(600, 400));
        assert!(
            !sent.same_as_sent(tile(0, 0, 300, 300), 1, &[7]),
            "the old grid's entry must not answer for the new size"
        );
        // The same size again keeps what was recorded under it.
        sent.note_size(Size::new(600, 400));
        sent.record(tile(0, 0, 256, 256), 1, &[7]);
        sent.note_size(Size::new(600, 400));
        assert!(sent.same_as_sent(tile(0, 0, 256, 256), 1, &[7]));
    }

    #[test]
    fn a_full_redraw_starts_the_comparison_over() {
        let mut sent = SentTiles::default();
        sent.note_size(Size::new(300, 300));
        sent.record(tile(0, 0, 256, 256), 1, &[5]);
        assert!(sent.same_as_sent(tile(0, 0, 256, 256), 1, &[5]));

        sent.begin_full_redraw(Size::new(300, 300));
        assert!(
            !sent.same_as_sent(tile(0, 0, 256, 256), 1, &[5]),
            "every tile of a full redraw goes out, compared against nothing"
        );
    }

    #[test]
    fn origins_name_their_grid_cells() {
        assert_eq!(cell_of(tile(0, 0, 1, 1)), (0, 0));
        assert_eq!(cell_of(tile(255, 255, 1, 1)), (0, 0));
        assert_eq!(cell_of(tile(256, 0, 44, 256)), (1, 0));
        assert_eq!(cell_of(tile(1280, 768, 120, 132)), (5, 3));
        // An unknown caller's rectangle cannot sit left of the surface, but the division
        // still names a cell rather than panicking or wrapping.
        assert_eq!(cell_of(tile(-1, -1, 1, 1)), (-1, -1));
    }
}
