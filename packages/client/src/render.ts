/**
 * The renderer: a client-owned backing store per surface, and canvases that show it.
 *
 * The backing store decodes each frame's tiles into an offscreen canvas per surface and acks
 * what it has drawn (rule 5, docs/protocol/README.md) after the next paint — whether or not a
 * host canvas is attached. `attach()` only sizes the host's canvas, blits the store into it and
 * follows it. So a frame that arrives before the host attaches (a React effect runs after the
 * paint), or while a window is unmounted, is still drawn and acked: no credit leaks, and a
 * canvas attached later shows the current pixels at once.
 *
 * It touches the host-provided canvases plus its own offscreen canvases, and nothing else in
 * the DOM (ADR-0003 §5). A frame with a tile that fails to decode is dropped whole — nothing
 * of it reaches the backing store — and never acked; the server's credits stall for that
 * surface instead of the client pretending it drew hostile bytes (ADR-0003 §4).
 */
import type { Envelope, Frame } from './protocol.js';
import { decodeTile } from './tile.js';
import type { DecodedTile } from './tile.js';
import type { AppricotConnection } from './connection.js';
import { Emitter } from './events.js';
import { bindFrameSink } from './registry.js';
import type { SurfaceRecord, SurfaceRegistry } from './registry.js';

export interface SurfaceRendererDeps {
  registry: SurfaceRegistry;
  conn: AppricotConnection;
}

/** The handle `SurfaceRenderer.attach` returns; `detach()` stops showing the surface. */
export interface AttachedSurface {
  detach(): void;
}

/** A canvas's size in device pixels: the surface size scaled by `scale/120`, at least 1x1. */
function deviceSize(record: SurfaceRecord): { width: number; height: number } {
  const k = record.scale / 120;
  return {
    width: Math.max(1, Math.round(record.size.width * k)),
    height: Math.max(1, Math.round(record.size.height * k)),
  };
}

/** One surface's pixels as the client holds them, attached or not. */
interface Backing {
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
  /** The scale the canvas was last sized for, in 120ths. */
  scale: number;
  /** The newest sequence drawn; a resume restarts it at 0. */
  lastDrawn: number;
  /** True once a frame has been drawn: only then is there anything to show. */
  painted: boolean;
}

interface StoreEvents {
  /** New pixels landed in this surface's backing store. */
  drawn: number;
  /** This surface's size or scale may have changed; attached canvases re-check theirs. */
  resized: number;
}

/**
 * The backing store of one connection and its registry. It takes frames from the connection's
 * 'message' event and from the registry (which holds frames that arrived before any store
 * existed), drawing each Frame object once whichever path delivers it first.
 */
class BackingStore {
  readonly events = new Emitter<StoreEvents>();
  readonly #registry: SurfaceRegistry;
  readonly #conn: AppricotConnection;
  readonly #surfaces = new Map<number, Backing>();
  readonly #taken = new WeakSet<Frame>();
  #scratch: { canvas: HTMLCanvasElement; ctx: CanvasRenderingContext2D } | undefined;
  /**
   * Acks scheduled before the socket went, or before a resume, name a sequence space that is
   * gone; they are not sent.
   */
  #epoch = 0;

  constructor(registry: SurfaceRegistry, conn: AppricotConnection) {
    this.#registry = registry;
    this.#conn = conn;
    conn.events.on('message', (envelope: Envelope) => {
      if (envelope.kind === 'frame') {
        this.claim(); // the registry may hold earlier frames; they go first
        this.#take(envelope.frame);
      }
    });
    conn.events.on('status', (status) => {
      if (status !== 'open') {
        this.#epoch += 1; // the socket went: an ack still waiting for its paint is void
      }
    });
    conn.events.on('resumed', () => {
      this.#epoch += 1;
      for (const backing of this.#surfaces.values()) {
        backing.lastDrawn = 0; // sequences restart; the full_redraw that follows re-syncs
      }
    });
    registry.events.on('window-added', (e) => {
      this.events.emit('resized', e.surface.id);
    });
    registry.events.on('metadata', (e) => {
      this.#refit(e.surface.id);
    });
    registry.events.on('configure-acked', (e) => {
      this.#refit(e.surfaceId);
    });
    registry.events.on('window-removed', (e) => {
      this.#surfaces.delete(e.surfaceId);
    });
  }

  /** Claims the registry's frames: the ones it held are drawn now, later ones come here. */
  claim(): void {
    for (const frame of bindFrameSink(this.#registry, this.#take)) {
      this.#take(frame);
    }
  }

  /** The surface's backing canvas, once it holds pixels. */
  pixels(surfaceId: number): HTMLCanvasElement | undefined {
    const backing = this.#surfaces.get(surfaceId);
    return backing?.painted === true ? backing.canvas : undefined;
  }

  readonly #take = (frame: Frame): void => {
    if (this.#taken.has(frame)) {
      return; // the same Frame object, delivered by the other path
    }
    this.#taken.add(frame);
    this.#draw(frame);
  };

  #draw(frame: Frame): void {
    const record = this.#registry.get(frame.surfaceId);
    if (record === undefined) {
      return; // unknown or gone: nothing to draw, nothing to ack
    }
    const backing = this.#backingFor(record);
    if (backing === undefined) {
      return;
    }
    // Old or repeated sequences are dropped; a full_redraw always draws — it is a sync of
    // the whole surface, cheap to honour and the one path that repairs a stale surface.
    if (!frame.fullRedraw && frame.sequence <= backing.lastDrawn) {
      return;
    }
    // Every tile is decoded before anything is drawn, so a frame is dropped whole: a hostile
    // tile after a legal one leaves the backing store untouched (ADR-0003 §4).
    let tiles: DecodedTile[];
    try {
      tiles = frame.tiles.map((tile) => decodeTile(tile, record.size));
    } catch {
      return; // hostile tile bytes: draw nothing, ack nothing, throw nowhere
    }
    const k = record.scale / 120;
    const { canvas, ctx } = backing;
    if (frame.fullRedraw) {
      ctx.clearRect(0, 0, canvas.width, canvas.height);
    }
    for (const { rect, imageData } of tiles) {
      if (k === 1) {
        ctx.putImageData(imageData, rect.x, rect.y);
        continue;
      }
      // putImageData ignores scale, so tiles ride a scratch canvas through drawImage, which
      // does not. The scratch is resized to the tile, never grown past it.
      const scratch = this.#scratchCanvas();
      if (scratch === undefined) {
        continue;
      }
      scratch.canvas.width = imageData.width;
      scratch.canvas.height = imageData.height;
      scratch.ctx.putImageData(imageData, 0, 0);
      ctx.drawImage(scratch.canvas, rect.x * k, rect.y * k, rect.width * k, rect.height * k);
    }
    backing.lastDrawn = frame.sequence;
    backing.painted = true;
    this.events.emit('drawn', frame.surfaceId);
    this.#ackAfterPaint(frame.surfaceId, frame.sequence);
  }

  #ackAfterPaint(surfaceId: number, sequence: number): void {
    const epoch = this.#epoch;
    const ack = (): void => {
      if (epoch === this.#epoch) {
        this.#conn.send({ kind: 'frameAck', frameAck: { surfaceId, sequence } });
      }
    };
    if (typeof requestAnimationFrame === 'function') {
      requestAnimationFrame(ack);
    } else {
      setTimeout(ack, 0); // no rAF (jsdom, and any host without one): still after this turn
    }
  }

  /** The surface's backing, created on its first frame and kept at its current size. */
  #backingFor(record: SurfaceRecord): Backing | undefined {
    const existing = this.#surfaces.get(record.id);
    if (existing !== undefined) {
      this.#fit(existing, record);
      return existing;
    }
    const size = deviceSize(record);
    const created = makeCanvas(size.width, size.height);
    if (created === undefined) {
      return undefined;
    }
    const backing: Backing = {
      ...created,
      scale: record.scale,
      lastDrawn: 0,
      painted: false,
    };
    this.#surfaces.set(record.id, backing);
    return backing;
  }

  #refit(surfaceId: number): void {
    const record = this.#registry.get(surfaceId);
    const backing = this.#surfaces.get(surfaceId);
    if (record !== undefined && backing !== undefined) {
      this.#fit(backing, record);
    }
    this.events.emit('resized', surfaceId);
  }

  /**
   * Sizes a backing canvas to its record. An unchanged size is left alone: writing a canvas
   * dimension clears it even when the value does not change. A real change keeps the pixels
   * already drawn, at the new scale and anchored top-left, until the app repaints.
   */
  #fit(backing: Backing, record: SurfaceRecord): void {
    const size = deviceSize(record);
    if (backing.canvas.width === size.width && backing.canvas.height === size.height) {
      backing.scale = record.scale;
      return;
    }
    const next = makeCanvas(size.width, size.height);
    if (next === undefined) {
      return;
    }
    if (backing.painted) {
      const k = record.scale / backing.scale;
      next.ctx.drawImage(
        backing.canvas,
        0,
        0,
        backing.canvas.width * k,
        backing.canvas.height * k,
      );
    }
    backing.canvas = next.canvas;
    backing.ctx = next.ctx;
    backing.scale = record.scale;
  }

  #scratchCanvas(): { canvas: HTMLCanvasElement; ctx: CanvasRenderingContext2D } | undefined {
    this.#scratch ??= makeCanvas(1, 1);
    return this.#scratch;
  }
}

function makeCanvas(
  width: number,
  height: number,
): { canvas: HTMLCanvasElement; ctx: CanvasRenderingContext2D } | undefined {
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext('2d');
  return ctx === null ? undefined : { canvas, ctx };
}

/** One backing store per connection and registry, made on the first attach. */
const stores = new WeakMap<AppricotConnection, WeakMap<SurfaceRegistry, BackingStore>>();

function storeFor({ registry, conn }: SurfaceRendererDeps): BackingStore {
  let byRegistry = stores.get(conn);
  if (byRegistry === undefined) {
    byRegistry = new WeakMap();
    stores.set(conn, byRegistry);
  }
  let store = byRegistry.get(registry);
  if (store === undefined) {
    store = new BackingStore(registry, conn);
    byRegistry.set(registry, store);
  }
  store.claim();
  return store;
}

export const SurfaceRenderer = {
  /**
   * Shows `surfaceId` in `canvas`. The canvas's backing store is set to the surface size
   * scaled by `scale/120`, and written only when that size really changes; layout is the
   * host's — only `canvas.width`/`canvas.height` are touched. The surface's pixels live in the
   * client, not in the canvas: attaching later, detaching, or attaching again all show the
   * current pixels, and frames are drawn and acked in between.
   */
  attach(
    canvas: HTMLCanvasElement,
    surfaceId: number,
    deps: SurfaceRendererDeps,
  ): AttachedSurface {
    const visibleCtx = canvas.getContext('2d');
    if (visibleCtx === null) {
      throw new Error('A 2D canvas context is unavailable; cannot attach a renderer.');
    }
    const { registry } = deps;
    const store = storeFor(deps);
    let detached = false;

    /** Sizes the canvas to the surface; true when its size (and so its bitmap) changed. */
    const fit = (): boolean => {
      const record = registry.get(surfaceId);
      if (record === undefined) {
        return false;
      }
      const size = deviceSize(record);
      let changed = false;
      if (canvas.width !== size.width) {
        canvas.width = size.width;
        changed = true;
      }
      if (canvas.height !== size.height) {
        canvas.height = size.height;
        changed = true;
      }
      return changed;
    };

    const blit = (): void => {
      const pixels = store.pixels(surfaceId);
      if (pixels !== undefined) {
        visibleCtx.drawImage(pixels, 0, 0);
      }
    };

    const offDrawn = store.events.on('drawn', (id) => {
      if (!detached && id === surfaceId) {
        fit();
        blit();
      }
    });
    const offResized = store.events.on('resized', (id) => {
      // A title-only change or another surface's resize leaves this canvas alone.
      if (!detached && id === surfaceId && fit()) {
        blit();
      }
    });
    const offRemoved = registry.events.on('window-removed', (e) => {
      if (!detached && e.surfaceId === surfaceId) {
        detach();
      }
    });

    fit();
    blit();

    function detach(): void {
      if (detached) {
        return;
      }
      detached = true;
      offDrawn();
      offResized();
      offRemoved();
    }

    return { detach };
  },
};
