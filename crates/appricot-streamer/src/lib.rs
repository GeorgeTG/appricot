//! appricot-streamer's server library: the protocol pump, the HTTP shell and the backend
//! driver, generic over the backend.
//!
//! The binary in `main.rs` wires this around the X11 backend; the integration tests wire it
//! around a mock. Everything here is backend-agnostic: no X11 or Wayland type crosses this
//! crate's modules (the window model lives in `appricot-core`, and the one backend that
//! exists lives in `appricot-x11`).
//!
//! # The pieces
//!
//! - [`auth`]: the token compare with no early exit.
//! - [`config`]: the `serve` configuration and its closed bind grammar.
//! - [`backend`]: the backend actor thread — one OS thread owns the display connection.
//! - [`server`]: routes (`/readyz`, `/session`), the one-session slot, its keeper, and the
//!   bind.
//! - [`session`]: the v0 protocol over one WebSocket: handshake, dispatch, frames, resume.
//! - [`sent_tiles`]: what the client last received per grid cell, so unchanged tiles are
//!   omitted from frames instead of sent.

pub mod auth;
pub mod backend;
pub mod config;
pub mod sent_tiles;
pub mod server;
pub mod session;
