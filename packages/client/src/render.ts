/**
 * The renderer: decodes tiles into an offscreen canvas, blits once per frame, and acks only
 * what it has actually drawn (rule 5, docs/protocol/README.md) — after the next paint.
 *
 * It touches exactly one host-provided canvas per surface plus its own offscreen canvases,
 * and nothing else in the DOM (ADR-0003 §5). A frame whose tiles fail to decode is dropped
 * whole and never acked; the server's credits stall for that surface instead of the client
 * pretending it drew hostile bytes (ADR-0003 §4).
 */
import type { Envelope, Frame } from './protocol';
import { decodeTile } from './tile';
import type { AppricotConnection } from './connection';
import type { SurfaceRegistry } from './registry';

export interface SurfaceRendererDeps {
  registry: SurfaceRegistry;
  conn: AppricotConnection;
}

/** The handle `SurfaceRenderer.attach` returns; `detach()` stops drawing and acking. */
export interface AttachedSurface {
  detach(): void;
}

export const SurfaceRenderer = {
  /**
   * Draws `surfaceId` into `canvas`. The backing store is set to the surface size scaled by
   * `scale/120`; layout is the host's — only `canvas.width`/`canvas.height` are touched.
   */
  attach(
    canvas: HTMLCanvasElement,
    surfaceId: number,
    deps: SurfaceRendererDeps,
  ): AttachedSurface {
    const { registry, conn } = deps;

    const offscreen = document.createElement('canvas');
    const scratch = document.createElement('canvas');
    const offCtx = offscreen.getContext('2d');
    const scratchCtx = scratch.getContext('2d');
    const visibleCtx = canvas.getContext('2d');
    if (offCtx === null || scratchCtx === null || visibleCtx === null) {
      throw new Error('A 2D canvas context is unavailable; cannot attach a renderer.');
    }

    let lastDrawn = 0;
    let detached = false;

    const resize = (): void => {
      const record = registry.get(surfaceId);
      if (record === undefined) {
        return;
      }
      const k = record.scale / 120;
      canvas.width = Math.max(1, Math.round(record.size.width * k));
      canvas.height = Math.max(1, Math.round(record.size.height * k));
      offscreen.width = canvas.width;
      offscreen.height = canvas.height;
    };

    const drawFrame = (frame: Frame): void => {
      // Old or repeated sequences are dropped; a full_redraw always draws — it is a sync of
      // the whole surface, cheap to honour and the one path that repairs a stale surface.
      if (!frame.fullRedraw && frame.sequence <= lastDrawn) {
        return;
      }
      const record = registry.get(surfaceId);
      if (record === undefined) {
        return;
      }
      const k = record.scale / 120;
      try {
        if (frame.fullRedraw) {
          offCtx.clearRect(0, 0, offscreen.width, offscreen.height);
        }
        for (const tile of frame.tiles) {
          const { rect, imageData } = decodeTile(tile, record.size);
          if (k === 1) {
            offCtx.putImageData(imageData, rect.x, rect.y);
          } else {
            // putImageData ignores scale, so tiles ride a scratch canvas through drawImage,
            // which does not. The scratch is resized to the tile, never grown past it.
            scratch.width = imageData.width;
            scratch.height = imageData.height;
            scratchCtx.putImageData(imageData, 0, 0);
            offCtx.drawImage(
              scratch,
              rect.x * k,
              rect.y * k,
              rect.width * k,
              rect.height * k,
            );
          }
        }
      } catch {
        return; // hostile tile bytes: draw nothing further, ack nothing, throw nowhere
      }
      visibleCtx.drawImage(offscreen, 0, 0);
      lastDrawn = frame.sequence;
      ackAfterPaint(frame.sequence);
    };

    const ackAfterPaint = (sequence: number): void => {
      const ack = (): void => {
        conn.send({ kind: 'frameAck', frameAck: { surfaceId, sequence } });
      };
      if (typeof requestAnimationFrame === 'function') {
        requestAnimationFrame(ack);
      } else {
        setTimeout(ack, 0); // no rAF (jsdom, and any host without one): still after this turn
      }
    };

    const onMessage = (envelope: Envelope): void => {
      if (detached) {
        return;
      }
      if (envelope.kind === 'frame' && envelope.frame.surfaceId === surfaceId) {
        drawFrame(envelope.frame);
      }
    };
    const onWindowAdded = (e: { surface: { id: number } }): void => {
      if (!detached && e.surface.id === surfaceId) {
        resize();
      }
    };
    const onResized = (): void => {
      if (!detached) {
        resize(); // the registry record already carries the new size or scale
      }
    };
    const onRemoved = (e: { surfaceId: number }): void => {
      if (!detached && e.surfaceId === surfaceId) {
        detach();
      }
    };
    const onResumed = (): void => {
      if (!detached) {
        lastDrawn = 0; // sequences may restart after a resume; the full_redraw that follows
        // re-syncs every surface, so accept any sequence again.
      }
    };

    const offMessage = conn.events.on('message', onMessage);
    const offResumed = conn.events.on('resumed', onResumed);
    const offAdded = registry.events.on('window-added', onWindowAdded);
    const offMetadata = registry.events.on('metadata', onResized);
    const offAcked = registry.events.on('configure-acked', onResized);
    const offRemoved = registry.events.on('window-removed', onRemoved);

    resize();

    function detach(): void {
      if (detached) {
        return;
      }
      detached = true;
      offMessage();
      offResumed();
      offAdded();
      offMetadata();
      offAcked();
      offRemoved();
    }

    return { detach };
  },
};
