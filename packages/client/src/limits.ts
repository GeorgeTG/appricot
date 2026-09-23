/**
 * The limits table of the wire protocol, version 0.
 *
 * Every constant mirrors one row of the table in
 * crates/appricot-proto/proto/appricot/v0/wire.proto (rule 3 of docs/protocol/README.md) and the
 * Rust twin in crates/appricot-proto/src/limits.rs. Both peers enforce every row before they
 * allocate or loop; the decoder in wire.ts checks each cap before it copies a single byte, so a
 * hostile server cannot make this client reserve or read past a bound.
 *
 * Change a number only together with the .proto header, the Rust table and the shared vectors.
 */

/** Most bytes in one WebSocket binary message, both directions. */
export const MAX_MESSAGE_BYTES = 16_777_216;

/** Frames sent and not yet acked, per surface. */
export const MAX_FRAME_CREDITS = 4;

/** Living surfaces in one session. */
export const MAX_SURFACES = 64;

/** Living popups of one parent surface. */
export const MAX_POPUPS_PER_PARENT = 16;

/** A surface's width, in pixels. */
export const MAX_SURFACE_WIDTH = 1920;

/** A surface's height, in pixels. */
export const MAX_SURFACE_HEIGHT = 1200;

/** A tile's width, in pixels. */
export const MAX_TILE_WIDTH = 256;

/** A tile's height, in pixels. */
export const MAX_TILE_HEIGHT = 256;

/** Tiles in one Frame. */
export const MAX_TILES_PER_FRAME = 48;

/** Bytes of one tile payload (256*256*4 raw). */
export const MAX_TILE_BYTES = 262_144;

/** SurfaceNew/SurfaceMetadata title, in UTF-8 bytes. */
export const MAX_TITLE_BYTES = 512;

/** SurfaceNew/SurfaceMetadata app id, in UTF-8 bytes. */
export const MAX_APP_ID_BYTES = 256;

/** Hello.client_name, in UTF-8 bytes. */
export const MAX_CLIENT_NAME_BYTES = 64;

/** HelloReply.session_id, in UTF-8 bytes. */
export const MAX_SESSION_ID_BYTES = 64;

/** Bye.text, in UTF-8 bytes. */
export const MAX_BYE_TEXT_BYTES = 256;

/** ServerError.text, in UTF-8 bytes. */
export const MAX_ERROR_TEXT_BYTES = 1024;

/** Hello.stream_token, in bytes. */
export const MAX_TOKEN_BYTES = 256;

/** Key.code, in bytes (a physical key name such as "KeyA", ASCII). */
export const MAX_KEY_CODE_BYTES = 16;

/** Entries in Hello.codecs / HelloReply.codecs. */
export const MAX_CODECS_OFFERED = 8;

/** ClipboardSet.text, in UTF-8 bytes. */
export const MAX_CLIPBOARD_BYTES = 65_536;

/** The cursor image's width, in pixels. */
export const MAX_CURSOR_WIDTH = 128;

/** The cursor image's height, in pixels. */
export const MAX_CURSOR_HEIGHT = 128;

/** Bytes of CursorImage.argb_premultiplied. */
export const MAX_CURSOR_BYTES = 65_536;

/** Discrete wheel steps in one PointerAxis, each way: |steps_x| and |steps_y|. */
export const MAX_POINTER_AXIS_STEPS = 64;

/** How long a session outlives its socket, in milliseconds. */
export const RESUME_GRACE_MS = 10_000;

/** Tile codec ids on the wire. 0 is never a codec; a value outside this set is not decodable. */
export const CODEC = {
  /** Uncompressed pixels: BGRX, row-major, exactly width * 4 bytes per row. */
  RAW: 1,
  /** The QOI image format, https://qoiformat.org. */
  QOI: 2,
} as const;
