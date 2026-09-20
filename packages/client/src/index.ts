/**
 * @appricot/client: APPricot's embeddable browser client.
 *
 * The public surface, for a host that draws its own chrome (ADR-0003): a connection over a
 * transport, a surface registry fed by envelopes, a renderer that acks what it drew, input
 * mapping with keysyms, and cursor math. The codec mirror (`protocol.ts` and friends) is
 * re-exported too. The client draws only into canvases the host provides, and every byte
 * from the server is hostile input.
 */
export * from './protocol';
export { setTextOnly } from './text-only';

export { Emitter } from './events';
export type { Listener } from './events';

export { AppricotConnection, WebSocketTransport, connectAppricot } from './connection';
export type {
  ConnectionEvents,
  ConnectionOptions,
  ConnectionStatus,
  Transport,
} from './connection';

export { SurfaceRegistry, clampPopup, placePopup } from './registry';
export type { RegistryEvents, SurfaceRecord } from './registry';

export { SurfaceRenderer } from './render';
export type { AttachedSurface, SurfaceRendererDeps } from './render';

export { attachInput, keysymFor, modifiersFor } from './input';
export type { DetachInput, InputDeps } from './input';

export { cursorToImageData, drawCursor } from './cursor';
