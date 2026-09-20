//! Wire protocol v0 of APPricot: message types and their codec.
//!
//! # Responsibility
//!
//! - The types of every v0 message, and their encoding and decoding (see [`wire`]).
//! - The limits table: a cap for every length and count on the wire (see [`limits`]).
//! - Bounded decoding. A length read from the wire is checked against its cap before anything
//!   is allocated or sliced, so a hostile peer cannot make the decoder read or reserve more.
//!
//! # Not its responsibility
//!
//! - No I/O: no sockets, no files, no async runtime, no clock. Callers hand in bytes.
//! - No window model and no capture. Those are `appricot-core` and `appricot-x11`.
//!
//! The message set is generated from the frozen `proto/appricot/v0/wire.proto`, which is the
//! single source of truth of version 0; the spec in `docs/protocol/` is the human-readable
//! form. The TypeScript client in `packages/client` mirrors this crate, and the shared test
//! vectors in `crates/appricot-proto/testdata/vectors.json` prove the two agree byte for
//! byte.

mod bounded;
pub mod limits;
pub mod wire;

pub use bounded::{BoundError, BoundedString};

/// The wire protocol version this crate speaks.
///
/// `@appricot/client` mirrors it in `packages/client/src/protocol.ts`, and a test there reads
/// the line below, so change both together.
pub const PROTOCOL_VERSION: u16 = 0;
