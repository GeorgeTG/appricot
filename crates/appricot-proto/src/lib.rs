//! Wire protocol v0 of APPricot: message types and their codec.
//!
//! # Responsibility
//!
//! - The types of every v0 message, and their encoding and decoding (see [`wire`]).
//! - The limits table: a cap for every length and count on the wire (see [`limits`]).
//! - Bounded decoding. [`wire::decode_envelope`] refuses input over the message cap before it
//!   parses a byte, then walks the raw bytes once without allocating and compares every length
//!   and every repeated count with its cap before prost builds anything. A hostile peer can
//!   therefore not make the decoder reserve more than the limits table allows for one message.
//!   (`BoundedString`'s own decoders are a string helper; the wire path does not use them.)
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
mod prescan;
pub mod wire;

pub use bounded::{BoundError, BoundedString};

/// The wire protocol version this crate speaks.
///
/// `@app-ricot/client` mirrors it in `packages/client/src/protocol.ts`, and a test there reads
/// the line below, so change both together.
pub const PROTOCOL_VERSION: u16 = 0;
