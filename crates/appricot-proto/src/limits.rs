//! The limits table of wire protocol v0: a cap for every length and count on the wire.
//!
//! The normative source is the header of `proto/appricot/v0/wire.proto`; those numbers win,
//! and this module mirrors them so a decoder can check them without reading the `.proto`.
//! The codec in [`crate::wire`] enforces every row on both encode and decode, before
//! anything is allocated or looped over. Session-level rows ([`MAX_SURFACES`],
//! [`MAX_POPUPS_PER_PARENT`], [`MAX_FRAME_CREDITS`]) bound living state, not one message,
//! so the streamer checks them; they live here so the table has one home.
//!
//! Every string bound by this table is untrusted text: the client renders it as text only,
//! never as markup (docs/adr/0003).

use crate::BoundedString;

/// Most bytes in one WebSocket binary message, in either direction. [`crate::wire`] refuses
/// longer input before parsing a single byte.
pub const MAX_MESSAGE_BYTES: usize = 16_777_216;

/// Frames sent and not yet acked, per surface.
pub const MAX_FRAME_CREDITS: usize = 4;

/// Living surfaces in one session.
pub const MAX_SURFACES: usize = 64;

/// Living popups of one parent surface.
pub const MAX_POPUPS_PER_PARENT: usize = 16;

/// A surface's widest width, in pixels.
pub const MAX_SURFACE_WIDTH: u32 = 1920;

/// A surface's tallest height, in pixels.
pub const MAX_SURFACE_HEIGHT: u32 = 1200;

/// A tile's widest width, in pixels.
pub const MAX_TILE_WIDTH: u32 = 256;

/// A tile's tallest height, in pixels.
pub const MAX_TILE_HEIGHT: u32 = 256;

/// Tiles in one Frame.
pub const MAX_TILES_PER_FRAME: usize = 48;

/// Bytes of one tile payload. 256 * 256 * 4 raw bytes.
pub const MAX_TILE_BYTES: usize = 262_144;

/// Most bytes in a surface title (`SurfaceNew.title`, `SurfaceMetadata.title`).
pub const MAX_TITLE_BYTES: usize = 512;

/// Most bytes in an app id (`SurfaceNew.app_id`, `SurfaceMetadata.app_id`).
pub const MAX_APP_ID_BYTES: usize = 256;

/// Most bytes in `Hello.client_name`.
pub const MAX_CLIENT_NAME_BYTES: usize = 64;

/// Most bytes in `HelloReply.session_id`.
pub const MAX_SESSION_ID_BYTES: usize = 64;

/// Most bytes in `Bye.text`.
pub const MAX_BYE_TEXT_BYTES: usize = 256;

/// Most bytes in `ServerError.text`.
pub const MAX_ERROR_TEXT_BYTES: usize = 1024;

/// Most bytes in `Hello.stream_token`.
pub const MAX_TOKEN_BYTES: usize = 256;

/// Most bytes in `Key.code`, the browser's physical key name.
pub const MAX_KEY_CODE_BYTES: usize = 16;

/// Entries in `Hello.codecs` (and the reply's subset of them).
pub const MAX_CODECS_OFFERED: usize = 8;

/// Most UTF-8 bytes in `ClipboardSet.text`.
pub const MAX_CLIPBOARD_BYTES: usize = 65_536;

/// A cursor image's widest width, in pixels.
pub const MAX_CURSOR_WIDTH: u32 = 128;

/// A cursor image's tallest height, in pixels.
pub const MAX_CURSOR_HEIGHT: u32 = 128;

/// Bytes of `CursorImage.argb_premultiplied`. 128 * 128 * 4 bytes.
pub const MAX_CURSOR_BYTES: usize = 65_536;

/// How long a session outlives its socket, in milliseconds.
pub const RESUME_GRACE_MS: u32 = 10_000;

/// A surface title.
pub type Title = BoundedString<MAX_TITLE_BYTES>;

/// An app id.
pub type AppId = BoundedString<MAX_APP_ID_BYTES>;

/// An error message from the server.
pub type ErrorText = BoundedString<MAX_ERROR_TEXT_BYTES>;

/// Tile codec ids on the wire. Stable across protocol versions; 0 is never a codec.
pub mod codec {
    /// Codec 1, RAW: uncompressed pixels, row-major, top to bottom, 4 bytes per pixel in the
    /// order blue, green, red, unused, no stride padding: exactly `width * 4` bytes per row.
    pub const RAW: u32 = 1;

    /// Codec 2, QOI: the QOI image format (<https://qoiformat.org>; the spec there is CC0 and
    /// the reference implementation MIT, checked 2026-09-20), implemented from the published
    /// spec with no third-party code. A 3-channel tile decodes to opaque pixels.
    pub const QOI: u32 = 2;
}
