/**
 * @appricot/client: APPricot's embeddable browser client.
 *
 * The public surface, for a host that draws its own chrome (ADR-0003): a connection over a
 * transport, a surface registry fed by envelopes, a renderer that acks what it drew, input
 * mapping with keysyms, and cursor math. The codec mirror (`protocol.ts` and friends) is
 * re-exported too. The client draws only into canvases the host provides, and every byte
 * from the server is hostile input.
 */
export * from './protocol.js';
export { setTextOnly } from './text-only.js';

export { Emitter } from './events.js';
export type { Listener } from './events.js';

export { AppricotConnection, WebSocketTransport, connectAppricot } from './connection.js';
export type {
  CloseReason,
  ConnectionEvents,
  ConnectionOptions,
  ConnectionStatus,
  Transport,
} from './connection.js';

export { SurfaceRegistry, clampPopup, placePopup } from './registry.js';
export type { RegistryEvents, SurfaceRecord } from './registry.js';

export { SurfaceRenderer } from './render.js';
export type { AttachedSurface, SurfaceRendererDeps } from './render.js';

export { attachInput, keysymFor, modifiersFor } from './input.js';
export type { DetachInput, InputDeps } from './input.js';
export { isPasteChord } from './paste.js';
export type { ChordKey, PastePolicy } from './paste.js';

export { cursorOrigin, cursorToImageData, drawCursor } from './cursor.js';
