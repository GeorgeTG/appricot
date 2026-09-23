//! appricot-encode repeats three facts of the wire's limits table: the tile edge, the tile
//! payload cap and the codec ids. It depends on no sibling but core, so it cannot name
//! appricot-proto's constants itself. The streamer sees both crates, so the pins live here:
//! a drift fails the build of this test, before any frame is cut.

use appricot_encode::{Encoding, MAX_TILE_BYTES, TILE_SIZE};
use appricot_proto::limits::{self, codec};

// The tile grid is the wire's largest tile, in both axes.
const _: () = assert!(TILE_SIZE == limits::MAX_TILE_WIDTH);
const _: () = assert!(TILE_SIZE == limits::MAX_TILE_HEIGHT);
// Encode's payload cap is the wire's.
const _: () = assert!(MAX_TILE_BYTES == limits::MAX_TILE_BYTES);
// The codec ids encode writes are the wire's.
const _: () = assert!(Encoding::Raw.id() == codec::RAW);
const _: () = assert!(Encoding::Qoi.id() == codec::QOI);

#[test]
fn every_wire_codec_id_names_the_matching_encoding() {
    assert_eq!(Encoding::from_id(codec::RAW), Some(Encoding::Raw));
    assert_eq!(Encoding::from_id(codec::QOI), Some(Encoding::Qoi));
    assert_eq!(Encoding::from_id(0), None, "0 is never a codec");
}

#[test]
fn a_full_tile_fits_the_frame_budget_of_the_largest_surface() {
    let columns = limits::MAX_SURFACE_WIDTH.div_ceil(TILE_SIZE);
    let rows = limits::MAX_SURFACE_HEIGHT.div_ceil(TILE_SIZE);
    let tiles = usize::try_from(columns * rows).expect("a small count");
    assert!(
        tiles <= limits::MAX_TILES_PER_FRAME,
        "a full redraw of the largest surface is {tiles} tiles"
    );
}
