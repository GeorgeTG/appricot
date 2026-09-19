/**
 * @appricot/client: APPricot's embeddable browser client.
 *
 * It will hold the connection, the codec mirror of appricot-proto, a window registry with
 * events, tile decode and input capture. It draws only into canvases the host provides, and it
 * treats every byte from the server as hostile (docs/adr/0003-untrusted-server-client.md).
 *
 * At bootstrap it exports the protocol version and the text-only helper.
 */
export { PROTOCOL_VERSION } from './protocol';
export { setTextOnly } from './text-only';
