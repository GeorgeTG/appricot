/**
 * The wire protocol version this client speaks.
 *
 * It mirrors `PROTOCOL_VERSION` in crates/appricot-proto/src/lib.rs. protocol.test.ts reads
 * that file and fails when the two differ, so change both together.
 */
export const PROTOCOL_VERSION = 0;
