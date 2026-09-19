//! The limits table: a cap for every length and count on the wire.
//!
//! The values here are provisional. The task l1-wire-spec-v0 fixes the table in
//! `docs/protocol/`, and this module then follows it. Every string type below is untrusted
//! text: the client renders it as text only, never as markup (docs/adr/0003).

use crate::BoundedString;

/// Most bytes in a surface title. 512 is roomy for any title a window manager would show, and
/// small enough that a title can never be used as a payload.
pub const MAX_TITLE_BYTES: usize = 512;

/// Most bytes in an app id. For X11 it comes from `WM_CLASS`.
pub const MAX_APP_ID_BYTES: usize = 256;

/// Most bytes in an error message the server sends.
pub const MAX_ERROR_BYTES: usize = 1024;

/// A surface title.
pub type Title = BoundedString<MAX_TITLE_BYTES>;

/// An app id.
pub type AppId = BoundedString<MAX_APP_ID_BYTES>;

/// An error message from the server.
pub type ErrorText = BoundedString<MAX_ERROR_BYTES>;
