/**
 * The single seam between these React bindings and @app-ricot/client.
 *
 * No other file in this package imports '@app-ricot/client'; when the client SDK's shapes
 * move, this file is the only place to adapt. What crosses the seam:
 *
 *   connectAppricot(url, { token: Uint8Array, reconnect? }) -> AppricotConnection
 *   AppricotConnection: get status() ('idle' | 'connecting' | 'open' | 'reconnecting' |
 *                       'closed'), connect(), send(Envelope) (dropped while not open),
 *                       close(), events (an Emitter: 'message', 'close', 'status',
 *                       'resumed', and 'ended' with the CloseReason after 'closed')
 *   new SurfaceRegistry(): get(id), list(), apply(Envelope),
 *                          events (window-added/removed, metadata, cursor-changed,
 *                          focus-ask, resize-ask, configure-acked, clipboard-ask); a
 *                          resume's re-announcement reports a changed record through
 *                          metadata and configure-acked (serial 0), never remove plus add
 *   SurfaceRenderer.attach(canvas, surfaceId, { registry, conn }) -> { detach() }
 *   attachInput(element, surfaceId, { conn, isFocused(), size?(), paste? }) -> detach()
 *
 * Everything below is a re-export of those pins plus the two outbound messages these
 * bindings construct themselves.
 */
import {
  attachInput,
  connectAppricot,
  setTextOnly,
  SurfaceRegistry,
  SurfaceRenderer,
} from '@app-ricot/client';
import type { AppricotConnection, SurfaceRecord } from '@app-ricot/client';

export { attachInput, connectAppricot, setTextOnly, SurfaceRegistry, SurfaceRenderer };
export { MAX_SURFACE_HEIGHT, MAX_SURFACE_WIDTH } from '@app-ricot/client';
export type {
  AppricotConnection,
  ConnectionStatus,
  Envelope,
  PastePolicy,
  RegistryEvents,
  SurfaceRecord,
} from '@app-ricot/client';

/** ROLE_TOPLEVEL of the wire Role enum (wire.proto); popups are ROLE_POPUP, 1. */
export const ROLE_TOPLEVEL: SurfaceRecord['role'] = 0;

/**
 * Focus notification (wire.proto FocusNotify). Only this message moves focus.
 *
 * When focus LEAVES a surface these bindings send nothing: blur-release is session-wide (it
 * releases every held key and button for the whole session), and only the host knows whether
 * focus moved to another APPricot surface or left APPricot entirely. A host that wants to
 * release sends the SDK's blur-release envelope itself through `send`. (attachInput also
 * releases on its own when the whole window loses focus.)
 */
export function sendFocusNotify(conn: AppricotConnection, surfaceId: number): void {
  conn.send({ kind: 'focusNotify', focusNotify: { surfaceId } });
}

/**
 * Size proposal (wire.proto Configure). `serial` is this component's own counter, starting
 * at 1 and rising by one; the wire allows any u32 serial from the client, and the server
 * answers once with ConfigureAck carrying the same serial and the size the app really took.
 */
export function sendConfigure(
  conn: AppricotConnection,
  surfaceId: number,
  serial: number,
  width: number,
  height: number,
): void {
  conn.send({ kind: 'configure', configure: { surfaceId, serial, size: { width, height } } });
}
