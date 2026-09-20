/**
 * The surface registry: what the host knows about the streamed windows, fed envelope by
 * envelope from the connection.
 *
 * The registry is a mirror with a spine (ADR-0003): every string it holds is untrusted text
 * that the host must show through `setTextOnly()` or as children, a popup whose parent is not
 * in the registry is dropped, and the per-session surface count is capped at MAX_SURFACES.
 * Server requests that would move, raise or focus a window become events for the host to
 * decide on; nothing here touches the DOM.
 */
import { MAX_SURFACES } from './protocol';
import type { Anchor, CursorImage, Envelope, Positioner, Rect, Role, Size } from './protocol';
import { Emitter } from './events';

/** One tracked surface. `title` and `appId` are untrusted text (ADR-0003 §1). */
export interface SurfaceRecord {
  /** Server-assigned id, unique within the session, never reused. */
  id: number;
  role: Role;
  /** The popup's or dialog's parent surface; undefined for a plain toplevel. */
  parent: number | undefined;
  /** Untrusted text. */
  title: string;
  /** Untrusted text. */
  appId: string;
  /** Device pixels per logical pixel in 120ths: 120 is 1x, 240 is 2x (wire `scale_120ths`). */
  scale: number;
  /** The size the surface currently has, in logical pixels. */
  size: Size;
  /** How a ROLE_POPUP asked to be placed, before `clampPopup` gets it. */
  positioner: Positioner | undefined;
  /** The highest frame sequence seen for this surface. */
  lastSequence: number;
}

/** Everything the registry tells the host. Payloads copy their numbers; records are shared. */
export interface RegistryEvents {
  'window-added': { surface: SurfaceRecord };
  'window-removed': { surfaceId: number; reason: number };
  /** Title, app id or scale changed; `surface` is the updated record. */
  metadata: { surface: SurfaceRecord };
  /** The new cursor pixels, or null when the cursor left the visible set. */
  'cursor-changed': { image: CursorImage | null };
  'focus-ask': { surfaceId: number };
  'resize-ask': { surfaceId: number; size: Size };
  /** The app applied a configure; `size` is the size it really took. */
  'configure-acked': { surfaceId: number; serial: number; size: Size };
  'clipboard-ask': undefined;
}

export class SurfaceRegistry {
  readonly events = new Emitter<RegistryEvents>();
  readonly #surfaces = new Map<number, SurfaceRecord>();
  #lastCursorSerial = 0;

  /** The record for one surface id, or undefined when it is not (or no longer) tracked. */
  get(surfaceId: number): SurfaceRecord | undefined {
    return this.#surfaces.get(surfaceId);
  }

  /** Every tracked surface, in insertion order. */
  list(): SurfaceRecord[] {
    return [...this.#surfaces.values()];
  }

  /** Feeds one decoded envelope. Envelopes that are not the registry's business are ignored. */
  apply(e: Envelope): void {
    switch (e.kind) {
      case 'surfaceNew': {
        const m = e.surfaceNew;
        if (this.#surfaces.has(m.surfaceId)) {
          return; // ids are never reused; a repeat is a hostile replay, not a new window
        }
        if (this.#surfaces.size >= MAX_SURFACES) {
          return; // session-level bound from the limits table (ADR-0003 §4)
        }
        // 1 = ROLE_POPUP per wire.proto: a popup whose parent is not there is dropped
        // (ADR-0003 §5).
        if (m.role === 1 && m.parentId !== undefined && !this.#surfaces.has(m.parentId)) {
          return;
        }
        const record: SurfaceRecord = {
          id: m.surfaceId,
          role: m.role,
          parent: m.parentId,
          title: m.title,
          appId: m.appId,
          scale: m.scale120ths,
          size: m.size ?? { width: 0, height: 0 }, // an absent size stays a zero box
          positioner: m.positioner,
          lastSequence: 0,
        };
        this.#surfaces.set(record.id, record);
        this.events.emit('window-added', { surface: record });
        return;
      }
      case 'surfaceGone': {
        const m = e.surfaceGone;
        if (!this.#surfaces.delete(m.surfaceId)) {
          return;
        }
        this.events.emit('window-removed', { surfaceId: m.surfaceId, reason: m.reason });
        return;
      }
      case 'surfaceMetadata': {
        const m = e.surfaceMetadata;
        const record = this.#surfaces.get(m.surfaceId);
        if (record === undefined) {
          return;
        }
        if (m.title !== undefined) {
          record.title = m.title;
        }
        if (m.appId !== undefined) {
          record.appId = m.appId;
        }
        if (m.scale120ths !== undefined) {
          record.scale = m.scale120ths;
        }
        this.events.emit('metadata', { surface: record });
        return;
      }
      case 'frame': {
        const m = e.frame;
        const record = this.#surfaces.get(m.surfaceId);
        if (record !== undefined && m.sequence > record.lastSequence) {
          record.lastSequence = m.sequence;
        }
        return;
      }
      case 'cursorImage': {
        const m = e.cursorImage;
        // wire.proto CursorImage: serial rises monotonically; one already drawn is dropped.
        if (m.serial <= this.#lastCursorSerial) {
          return;
        }
        this.#lastCursorSerial = m.serial;
        this.events.emit('cursor-changed', { image: m });
        return;
      }
      case 'cursorGone': {
        this.events.emit('cursor-changed', { image: null });
        return;
      }
      case 'focusAsk': {
        this.events.emit('focus-ask', { surfaceId: e.focusAsk.surfaceId });
        return;
      }
      case 'resizeAsk': {
        this.events.emit('resize-ask', {
          surfaceId: e.resizeAsk.surfaceId,
          size: e.resizeAsk.size ?? { width: 0, height: 0 }, // absent: a zero box
        });
        return;
      }
      case 'configureAck': {
        const m = e.configureAck;
        const record = this.#surfaces.get(m.surfaceId);
        if (record !== undefined && m.size !== undefined) {
          record.size = m.size; // the size the app really took is the size it now has
        }
        this.events.emit('configure-acked', {
          surfaceId: m.surfaceId,
          serial: m.serial,
          size: m.size ?? record?.size ?? { width: 0, height: 0 },
        });
        return;
      }
      case 'clipboardAsk': {
        this.events.emit('clipboard-ask', undefined);
        return;
      }
      default:
        return;
    }
  }
}

/** The point of `rect` that `anchor` names: an edge midpoint, a corner, or the center. */
function anchorPoint(rect: Rect, anchor: Anchor): { x: number; y: number } {
  const x1 = rect.x + rect.width;
  const y1 = rect.y + rect.height;
  switch (anchor) {
    case 1: // ANCHOR_TOP
      return { x: Math.floor((rect.x + x1) / 2), y: rect.y };
    case 2: // ANCHOR_BOTTOM
      return { x: Math.floor((rect.x + x1) / 2), y: y1 };
    case 3: // ANCHOR_LEFT
      return { x: rect.x, y: Math.floor((rect.y + y1) / 2) };
    case 4: // ANCHOR_RIGHT
      return { x: x1, y: Math.floor((rect.y + y1) / 2) };
    case 5: // ANCHOR_TOP_LEFT
      return { x: rect.x, y: rect.y };
    case 6: // ANCHOR_BOTTOM_LEFT
      return { x: rect.x, y: y1 };
    case 7: // ANCHOR_TOP_RIGHT
      return { x: x1, y: rect.y };
    case 8: // ANCHOR_BOTTOM_RIGHT
      return { x: x1, y: y1 };
    default: // ANCHOR_CENTER
      return { x: Math.floor((rect.x + x1) / 2), y: Math.floor((rect.y + y1) / 2) };
  }
}

/**
 * Places a popup from its `Positioner`, in the parent's surface-local coordinates: the
 * gravity-named point of the popup lands on the anchor-named point of the anchor rect, then
 * the offset shifts it. Absent sub-messages count as zero: no anchor rect anchors at the
 * parent's origin, no offset does not shift. Pure math; the result still needs `clampPopup`
 * before it is shown.
 */
export function placePopup(positioner: Positioner): Rect {
  const anchorRect = positioner.anchorRect ?? { x: 0, y: 0, width: 0, height: 0 };
  const offset = positioner.offset ?? { x: 0, y: 0 };
  const size = positioner.size ?? { width: 0, height: 0 };
  const onAnchor = anchorPoint(anchorRect, positioner.anchor);
  const onPopup = anchorPoint({ x: 0, y: 0, ...size }, positioner.gravity);
  return {
    x: onAnchor.x - onPopup.x + offset.x,
    y: onAnchor.y - onPopup.y + offset.y,
    width: size.width,
    height: size.height,
  };
}

/**
 * Clamps a placed popup rectangle into its parent's box plus `margin` pixels on every side
 * (ADR-0003 §5, the client-side enforcement). A popup larger than the parent box plus twice
 * the margin has its size capped to that, so a server cannot ask for a popup the size of the
 * screen: whatever it sends, what comes back is no bigger than the parent's neighbourhood and
 * its origin sits inside it. The size is never grown.
 */
export function clampPopup(placed: Rect, parentBox: Rect, margin = 32): Rect {
  const maxWidth = parentBox.width + margin * 2;
  const maxHeight = parentBox.height + margin * 2;
  const width = Math.max(0, Math.min(placed.width, maxWidth));
  const height = Math.max(0, Math.min(placed.height, maxHeight));
  const xMin = parentBox.x - margin;
  const yMin = parentBox.y - margin;
  const x = Math.min(
    Math.max(placed.x, xMin),
    parentBox.x + parentBox.width + margin - width,
  );
  const y = Math.min(
    Math.max(placed.y, yMin),
    parentBox.y + parentBox.height + margin - height,
  );
  return { x, y, width, height };
}
