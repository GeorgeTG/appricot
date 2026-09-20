/**
 * The wire protocol version this client speaks.
 *
 * It mirrors `PROTOCOL_VERSION` in crates/appricot-proto/src/lib.rs. protocol.test.ts reads
 * that file and fails when the two differ, so change both together.
 */
export const PROTOCOL_VERSION = 0;

/**
 * The codec mirror of appricot-proto, hand-written against the normative contract in
 * crates/appricot-proto/proto/appricot/v0/wire.proto: the message types, the bounded
 * encodeEnvelope/decodeEnvelope pair, the limits table and the tile codec ids. Tile pixel
 * decoding (decodeTile) lives in tile.ts.
 */
export * from './limits';
export * from './wire';
