/**
 * The surface registry: what the host knows about the streamed windows, fed envelope by
 * envelope from the connection.
 *
 * The registry is a mirror with a spine (ADR-0003): every string it holds is untrusted text
 * that the host must show through `setTextOnly()` or as children; a popup needs a living
 * parent, goes when its parent goes, and one parent keeps at most MAX_POPUPS_PER_PARENT of
 * them; the per-session surface count is capped at MAX_SURFACES. Server requests that would
 * move, raise or focus a window become events for the host to decide on; nothing here touches
 * the DOM.
 *
 * Resume (v0 §7): after a HelloReply with `resumed` set, the server re-sends every living
 * surface as SurfaceNew, as it stands now. The registry marks every record stale, updates each
 * re-announced one in place (never a remove plus an add), and at the first message that is not
 * a SurfaceNew removes every record still stale. A HelloReply without `resumed` starts a fresh
 * session: every record of the old one is removed.
 */
import { MAX_FRAME_CREDITS, MAX_POPUPS_PER_PARENT, MAX_SURFACES } from './protocol.js';
import type {
  Anchor,
  CursorImage,
  Envelope,
  Frame,
  HelloReply,
  Point,
  Positioner,
  Rect,
  Role,
  Size,
  SurfaceGoneReason,
  SurfaceNew,
} from './protocol.js';
import { Emitter } from './events.js';

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
  /** The highest frame sequence seen for this surface; a resume restarts it at 0. */
  lastSequence: number;
}

/** Everything the registry tells the host. Payloads copy their numbers; records are shared. */
export interface RegistryEvents {
  'window-added': { surface: SurfaceRecord };
  /**
   * A surface went. `reason` is the wire SurfaceGoneReason: 0 the app closed it (also a
   * surface that did not come back from a resume), 1 its parent went first (a popup never
   * outlives its parent), 2 the session ended (a fresh session replaced the old one).
   */
  'window-removed': { surfaceId: number; reason: number };
  /**
   * Title, app id, scale or a popup's positioner changed; `surface` is the updated record.
   * A resume's re-announcement reports what changed while the socket was down this way.
   */
  metadata: { surface: SurfaceRecord };
  /** The new cursor pixels, or null when the cursor left the visible set. */
  'cursor-changed': { image: CursorImage | null };
  'focus-ask': { surfaceId: number };
  'resize-ask': { surfaceId: number; size: Size };
  /**
   * The surface took a new size; `size` is the size it really has now. `serial` names the
   * host's own Configure being answered. Serial 0 is never a host serial: it means the app
   * resized itself, unprompted (a popup that grew, or a size change reported by a resume), and
   * the host sizes its chrome to it.
   */
  'configure-acked': { surfaceId: number; serial: number; size: Size };
  'clipboard-ask': undefined;
}

/** ROLE_POPUP of the wire Role enum. */
const ROLE_POPUP = 1;
/** The wire SurfaceGoneReason values. */
const GONE_APP_CLOSED = 0;
const GONE_PARENT_GONE = 1;
const GONE_SESSION_END = 2;

/**
 * The most tile bytes the registry holds for frames nobody has claimed yet (see
 * `bindFrameSink`). A compliant server cannot come near it; past it a frame is dropped
 * unacked, like any other frame the client refuses.
 */
const MAX_HELD_FRAME_BYTES = 64 * 1024 * 1024;

/** Where a registry hands the frames it is fed once a backing store has claimed them. */
export type FrameSink = (frame: Frame) => void;

let bindSink: (registry: SurfaceRegistry, sink: FrameSink) => Frame[];

export class SurfaceRegistry {
  readonly events = new Emitter<RegistryEvents>();
  readonly #surfaces = new Map<number, SurfaceRecord>();
  #lastCursorSerial = 0;
  #cursorShown = false;
  /** While a resume's re-announcement runs: the ids not re-announced yet. */
  #stale: Set<number> | undefined;
  /** The backing store's intake, once one has claimed this registry's frames. */
  #sink: FrameSink | undefined;
  /** Frames that arrived while nothing had claimed them, by surface, in arrival order. */
  readonly #held = new Map<number, Frame[]>();
  #heldBytes = 0;

  static {
    bindSink = (registry, sink) => registry.#bindSink(sink);
  }

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
    // A resume's re-announcement ends at the first message that is not a SurfaceNew (C4).
    if (this.#stale !== undefined && e.kind !== 'surfaceNew') {
      this.#sweepStale();
    }
    switch (e.kind) {
      case 'helloReply': {
        this.#onHelloReply(e.helloReply);
        return;
      }
      case 'surfaceNew': {
        this.#onSurfaceNew(e.surfaceNew);
        return;
      }
      case 'surfaceGone': {
        this.#remove(e.surfaceGone.surfaceId, e.surfaceGone.reason);
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
        if (record === undefined) {
          return;
        }
        if (m.sequence > record.lastSequence) {
          record.lastSequence = m.sequence;
        }
        this.#handOff(m);
        return;
      }
      case 'cursorImage': {
        const m = e.cursorImage;
        // wire.proto CursorImage: serial rises monotonically; one already drawn is dropped.
        if (m.serial <= this.#lastCursorSerial) {
          return;
        }
        this.#lastCursorSerial = m.serial;
        this.#cursorShown = true;
        this.events.emit('cursor-changed', { image: m });
        return;
      }
      case 'cursorGone': {
        this.#cursorShown = false;
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

  #onHelloReply(reply: HelloReply): void {
    // Held frames belong to the sequence space that just ended; the server redraws in full.
    this.#dropHeldFrames();
    if (reply.resumed) {
      // Every record is stale until the server re-announces it (v0 §7, C4).
      this.#stale = new Set(this.#surfaces.keys());
      for (const record of this.#surfaces.values()) {
        record.lastSequence = 0; // frame sequences restart on resume
      }
      return;
    }
    // A fresh session: nothing of an earlier one survives it (v0 §2, HelloReply.resumed).
    this.#sink = undefined; // the next backing store to attach claims the new session's frames
    this.#lastCursorSerial = 0; // cursor serials restart with the session (C3)
    this.#removeAll(GONE_SESSION_END);
    if (this.#cursorShown) {
      this.#cursorShown = false;
      this.events.emit('cursor-changed', { image: null });
    }
  }

  #onSurfaceNew(m: SurfaceNew): void {
    const existing = this.#surfaces.get(m.surfaceId);
    if (existing !== undefined) {
      // Ids are never reused. Outside a resume a repeat is a hostile replay, not a new window;
      // inside one it is the surface as it stands now.
      if (this.#stale?.has(m.surfaceId) === true) {
        this.#reannounce(existing, m);
      }
      return;
    }
    if (this.#liveCount() >= MAX_SURFACES) {
      return; // session-level bound from the limits table (ADR-0003 §4)
    }
    if (m.role === ROLE_POPUP) {
      // A popup needs a living parent (ADR-0003 §5), and one parent keeps at most
      // MAX_POPUPS_PER_PARENT popups (ADR-0003 §4).
      if (m.parentId === undefined || !this.#surfaces.has(m.parentId)) {
        return;
      }
      if (this.#popupCount(m.parentId) >= MAX_POPUPS_PER_PARENT) {
        return;
      }
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
  }

  /** A stale record came back in a resume: update it in place, report what changed. */
  #reannounce(record: SurfaceRecord, m: SurfaceNew): void {
    // The caps hold across a resume too: a record that would break one stays stale and goes
    // at the sweep, like a surface the server did not re-announce.
    if (this.#liveCount() >= MAX_SURFACES) {
      return;
    }
    if (
      record.role === ROLE_POPUP &&
      record.parent !== undefined &&
      this.#popupCount(record.parent) >= MAX_POPUPS_PER_PARENT
    ) {
      return;
    }
    this.#stale?.delete(record.id);
    // The role and the parent are set once (v0 §4.1); everything else is as it stands now.
    const size = m.size ?? { width: 0, height: 0 };
    const sizeChanged = size.width !== record.size.width || size.height !== record.size.height;
    const metadataChanged =
      m.title !== record.title ||
      m.appId !== record.appId ||
      m.scale120ths !== record.scale ||
      !samePositioner(m.positioner, record.positioner);
    record.title = m.title;
    record.appId = m.appId;
    record.scale = m.scale120ths;
    record.positioner = m.positioner;
    if (sizeChanged) {
      record.size = size;
    }
    if (metadataChanged) {
      this.events.emit('metadata', { surface: record });
    }
    if (sizeChanged) {
      // Serial 0: the host did not ask for this size; the app took it while the socket was down.
      this.events.emit('configure-acked', { surfaceId: record.id, serial: 0, size: record.size });
    }
  }

  /** Ends a resume's re-announcement: whatever did not come back is gone. */
  #sweepStale(): void {
    const stale = this.#stale;
    this.#stale = undefined;
    for (const id of stale ?? []) {
      this.#remove(id, GONE_APP_CLOSED);
    }
  }

  /**
   * Removes one surface and every popup below it, the popups reported PARENT_GONE (ADR-0003
   * §5: a popup whose parent is gone is dropped). Every record goes before the first event, so
   * each listener already sees the final state. An unknown id removes nothing.
   */
  #remove(surfaceId: number, reason: SurfaceGoneReason): void {
    if (!this.#surfaces.has(surfaceId)) {
      return;
    }
    const gone: { surfaceId: number; reason: SurfaceGoneReason }[] = [{ surfaceId, reason }];
    for (let i = 0; i < gone.length; i++) {
      const parent = gone[i]?.surfaceId;
      for (const record of this.#surfaces.values()) {
        if (record.role === ROLE_POPUP && record.parent === parent) {
          gone.push({ surfaceId: record.id, reason: GONE_PARENT_GONE });
        }
      }
    }
    for (const entry of gone) {
      this.#forget(entry.surfaceId);
    }
    for (const entry of gone) {
      this.events.emit('window-removed', entry);
    }
  }

  /** Removes every record, each reported with `reason`. */
  #removeAll(reason: SurfaceGoneReason): void {
    const ids = [...this.#surfaces.keys()];
    for (const id of ids) {
      this.#forget(id);
    }
    for (const id of ids) {
      this.events.emit('window-removed', { surfaceId: id, reason });
    }
  }

  #forget(surfaceId: number): void {
    this.#surfaces.delete(surfaceId);
    this.#stale?.delete(surfaceId);
    const held = this.#held.get(surfaceId);
    if (held !== undefined) {
      this.#held.delete(surfaceId);
      this.#heldBytes -= held.reduce((sum, frame) => sum + frameBytes(frame), 0);
    }
  }

  /** Surfaces that count toward the caps: every record not waiting for its re-announcement. */
  #liveCount(): number {
    return this.#surfaces.size - (this.#stale?.size ?? 0);
  }

  /** Living popups of `parent`, stale ones excluded. */
  #popupCount(parent: number): number {
    let count = 0;
    for (const record of this.#surfaces.values()) {
      if (
        record.role === ROLE_POPUP &&
        record.parent === parent &&
        this.#stale?.has(record.id) !== true
      ) {
        count += 1;
      }
    }
    return count;
  }

  /**
   * Passes a frame of a tracked surface to the backing store, or holds it until one claims
   * this registry. A host may attach its first canvas only after a frame has arrived (a React
   * effect runs after the paint); the held frame is then drawn and acked on attach instead of
   * being lost with its credit. The server keeps at most MAX_FRAME_CREDITS frames unacked per
   * surface, so a surface never needs more held than that.
   */
  #handOff(frame: Frame): void {
    if (this.#sink !== undefined) {
      this.#sink(frame);
      return;
    }
    const held = this.#held.get(frame.surfaceId) ?? [];
    const bytes = frameBytes(frame);
    if (held.length >= MAX_FRAME_CREDITS || this.#heldBytes + bytes > MAX_HELD_FRAME_BYTES) {
      return;
    }
    held.push(frame);
    this.#held.set(frame.surfaceId, held);
    this.#heldBytes += bytes;
  }

  #dropHeldFrames(): void {
    this.#held.clear();
    this.#heldBytes = 0;
  }

  #bindSink(sink: FrameSink): Frame[] {
    if (this.#sink === sink) {
      return [];
    }
    this.#sink = sink;
    const held = [...this.#held.values()].flat();
    this.#dropHeldFrames();
    return held;
  }
}

/**
 * Makes `sink` the consumer of every frame `registry` is fed from now on, and returns the
 * frames it held while nothing was bound, in arrival order per surface. Binding the sink that
 * is already bound returns nothing. Internal to the package: the renderer's backing store
 * (render.ts) is the only caller, and index.ts does not export it.
 */
export function bindFrameSink(registry: SurfaceRegistry, sink: FrameSink): Frame[] {
  return bindSink(registry, sink);
}

function frameBytes(frame: Frame): number {
  return frame.tiles.reduce((sum, tile) => sum + tile.data.length, 0);
}

function samePoint(a: Point | undefined, b: Point | undefined): boolean {
  return a === undefined || b === undefined ? a === b : a.x === b.x && a.y === b.y;
}

function sameSize(a: Size | undefined, b: Size | undefined): boolean {
  return a === undefined || b === undefined
    ? a === b
    : a.width === b.width && a.height === b.height;
}

function sameRect(a: Rect | undefined, b: Rect | undefined): boolean {
  return a === undefined || b === undefined
    ? a === b
    : a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height;
}

function samePositioner(a: Positioner | undefined, b: Positioner | undefined): boolean {
  if (a === undefined || b === undefined) {
    return a === b;
  }
  return (
    a.anchor === b.anchor &&
    a.gravity === b.gravity &&
    sameRect(a.anchorRect, b.anchorRect) &&
    samePoint(a.offset, b.offset) &&
    sameSize(a.size, b.size)
  );
}

/** Rust's `saturate_i32` in appricot-core: every placed coordinate fits a sint32. */
function saturateI32(value: number): number {
  return Math.min(Math.max(value, -0x80000000), 0x7fffffff);
}

/**
 * The middle of `a` and `b`, rounded towards zero: Rust's `i64::midpoint`, which core uses, so
 * both sides agree on odd spans at negative coordinates too.
 */
function midpoint(a: number, b: number): number {
  return Math.trunc((a + b) / 2);
}

/** The point of `rect` that `anchor` names: an edge midpoint, a corner, or the center. */
function anchorPoint(rect: Rect, anchor: Anchor): Point {
  const left = rect.x;
  const top = rect.y;
  const right = rect.x + rect.width;
  const bottom = rect.y + rect.height;
  switch (anchor) {
    case 1: // ANCHOR_TOP
      return { x: midpoint(left, right), y: top };
    case 2: // ANCHOR_BOTTOM
      return { x: midpoint(left, right), y: bottom };
    case 3: // ANCHOR_LEFT
      return { x: left, y: midpoint(top, bottom) };
    case 4: // ANCHOR_RIGHT
      return { x: right, y: midpoint(top, bottom) };
    case 5: // ANCHOR_TOP_LEFT
      return { x: left, y: top };
    case 6: // ANCHOR_BOTTOM_LEFT
      return { x: left, y: bottom };
    case 7: // ANCHOR_TOP_RIGHT
      return { x: right, y: top };
    case 8: // ANCHOR_BOTTOM_RIGHT
      return { x: right, y: bottom };
    default: // ANCHOR_CENTER
      return { x: midpoint(left, right), y: midpoint(top, bottom) };
  }
}

/**
 * The top-left corner of a popup of `size` that grows from `point` in the direction `gravity`
 * names (xdg_positioner): BOTTOM_RIGHT extends right and down, so the popup's top-left corner
 * sits on the point; TOP_LEFT extends up and left; the centre and the edge midpoints centre
 * the popup on the point along the axes they do not name.
 */
function growFrom(gravity: Anchor, point: Point, size: Size): Point {
  let x: number;
  switch (gravity) {
    case 3: // ANCHOR_LEFT
    case 5: // ANCHOR_TOP_LEFT
    case 6: // ANCHOR_BOTTOM_LEFT
      x = point.x - size.width;
      break;
    case 4: // ANCHOR_RIGHT
    case 7: // ANCHOR_TOP_RIGHT
    case 8: // ANCHOR_BOTTOM_RIGHT
      x = point.x;
      break;
    default: // ANCHOR_CENTER, ANCHOR_TOP, ANCHOR_BOTTOM
      x = point.x - Math.floor(size.width / 2);
  }
  let y: number;
  switch (gravity) {
    case 1: // ANCHOR_TOP
    case 5: // ANCHOR_TOP_LEFT
    case 7: // ANCHOR_TOP_RIGHT
      y = point.y - size.height;
      break;
    case 2: // ANCHOR_BOTTOM
    case 6: // ANCHOR_BOTTOM_LEFT
    case 8: // ANCHOR_BOTTOM_RIGHT
      y = point.y;
      break;
    default: // ANCHOR_CENTER, ANCHOR_LEFT, ANCHOR_RIGHT
      y = point.y - Math.floor(size.height / 2);
  }
  return { x, y };
}

/**
 * Places a popup from its `Positioner`, in the parent's surface-local coordinates, exactly as
 * appricot-core's `Positioner::place` does (xdg_positioner semantics, v0 §10): take the
 * anchor-named point of the anchor rect, grow the popup from it in the direction the gravity
 * names, then shift by the offset. An X11 menu arrives as anchor TOP_LEFT of a zero-size rect
 * at its position plus gravity BOTTOM_RIGHT, so it lands where the app put it. Absent
 * sub-messages count as zero: no anchor rect anchors at the parent's origin, no offset does
 * not shift. Pure math; the result still needs `clampPopup` before it is shown. The shared
 * vectors in crates/appricot-core/testdata/placement.json pin it to the Rust side.
 */
export function placePopup(positioner: Positioner): Rect {
  const anchorRect = positioner.anchorRect ?? { x: 0, y: 0, width: 0, height: 0 };
  const offset = positioner.offset ?? { x: 0, y: 0 };
  const size = positioner.size ?? { width: 0, height: 0 };
  const hang = anchorPoint(anchorRect, positioner.anchor);
  const corner = growFrom(positioner.gravity, hang, size);
  return {
    x: saturateI32(corner.x + offset.x),
    y: saturateI32(corner.y + offset.y),
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
