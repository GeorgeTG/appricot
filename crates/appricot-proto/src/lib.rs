//! Wire protocol v0 of APPricot: message types and their codec.
//!
//! # Responsibility
//!
//! - The types of every v0 message, and their encoding and decoding.
//! - The limits table: a cap for every length and count on the wire (see [`limits`]).
//! - Bounded decoding. A length read from the wire is checked against its cap before anything
//!   is allocated or sliced, so a hostile peer cannot make the decoder read or reserve more.
//!
//! # Not its responsibility
//!
//! - No I/O: no sockets, no files, no async runtime, no clock. Callers hand in bytes.
//! - No window model and no capture. Those are `appricot-core` and `appricot-x11`.
//!
//! At bootstrap this crate holds the version and the bounded string. The messages arrive with
//! the task l1-wire-spec-v0. The spec in `docs/protocol/` is authoritative; this crate follows
//! it, and the TypeScript client in `packages/client` mirrors this crate.

mod bounded;
pub mod limits;

pub use bounded::{BoundError, BoundedString};

/// The wire protocol version this crate speaks.
///
/// `@appricot/client` mirrors it in `packages/client/src/protocol.ts`, and a test there reads
/// the line below, so change both together.
pub const PROTOCOL_VERSION: u16 = 0;
