//! Tile encoders: they turn captured pixels into the payloads of draw messages.
//!
//! # Responsibility
//!
//! - Cut a damaged rectangle into tiles and encode each tile.
//! - Lossless first. Which codec is open: the M1 spike measures the candidates on the damage a
//!   real desktop application produces, not on synthetic frames (docs/roadmap.md, M1).
//!
//! # Not its responsibility
//!
//! - Capture (`appricot-x11`), pacing (`appricot-core`) and the wire format
//!   (`appricot-proto`).
//! - Any dependency outside the permissive allow list (docs/adr/0002-licence.md, enforced by
//!   `deny.toml`). Codecs are where copyleft hides, so a candidate crate is checked against
//!   that list before it is added, not after.
//!
//! At bootstrap this crate holds only the [`Encoding`] stub.

/// Edge length of a square tile, in pixels. Provisional, see l1-wire-spec-v0.
pub const TILE_SIZE: u32 = 64;

/// How a tile's payload is encoded. A stub: l1-wire-spec-v0 fixes the set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Encoding {
    /// Uncompressed pixels, row by row. For tests and debugging.
    Raw,
}

impl Encoding {
    /// True when decoding gives back exactly the captured pixels.
    pub const fn is_lossless(self) -> bool {
        match self {
            Self::Raw => true,
        }
    }
}
