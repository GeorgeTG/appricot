// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { Envelope, SurfaceNew } from './protocol';
import { Emitter } from './events';
import type { AppricotConnection, ConnectionEvents } from './connection';
import { SurfaceRegistry } from './registry';
import { SurfaceRenderer } from './render';

/**
 * jsdom has no canvas rasteriser: getContext returns null. Every canvas in this file gets a
 * recording stub instead, so the tests observe exactly what was drawn and when — which is the
 * renderer's whole contract: draw, then ack after the paint.
 */
interface RecordedCall {
  op: 'clearRect' | 'putImageData' | 'drawImage';
  args: unknown[];
}

const callsByCanvas = new WeakMap<HTMLCanvasElement, RecordedCall[]>();

function recordedCalls(canvas: HTMLCanvasElement): RecordedCall[] {
  const calls = callsByCanvas.get(canvas);
  if (calls === undefined) {
    throw new Error('this canvas was never given a context');
  }
  return calls;
}

function makeFakeConn(): { conn: AppricotConnection; sent: Envelope[] } {
  const sent: Envelope[] = [];
  const conn = {
    events: new Emitter<ConnectionEvents>(),
    send: (e: Envelope): void => {
      sent.push(e);
    },
  };
  return { conn: conn as unknown as AppricotConnection, sent };
}

/** A 1x1 RAW tile: four bytes, blue/green/red/unused (wire.proto codec 1). */
function rawTile(x: number, y: number, bytes = [200, 100, 50, 255]): {
  rect: { x: number; y: number; width: number; height: number };
  codec: number;
  data: Uint8Array;
} {
  return { rect: { x, y, width: 1, height: 1 }, codec: 1, data: new Uint8Array(bytes) };
}

function surfaceNewEnvelope(id: number, size: { width: number; height: number }, scale: number): Envelope {
  const msg: SurfaceNew = {
    surfaceId: id,
    role: 0,
    size,
    title: 't',
    appId: 'a',
    scale120ths: scale,
  };
  return { kind: 'surfaceNew', surfaceNew: msg };
}

function frameEnvelope(
  surfaceId: number,
  sequence: number,
  fullRedraw: boolean,
  tiles: ReturnType<typeof rawTile>[],
): Envelope {
  return {
    kind: 'frame',
    frame: { surfaceId, sequence, fullRedraw, tiles },
  };
}

/**
 * A minimal `ImageData` stand-in: this vitest jsdom setup exposes no `ImageData` global
 * (measured 2026-09-20), and the tile decoder constructs one. It supports both constructor
 * shapes the DOM defines and is all any consumer in this file reads.
 */
class ImageDataStub {
  readonly width: number;
  readonly height: number;
  readonly data: Uint8ClampedArray<ArrayBuffer>;

  constructor(
    first: number | Uint8ClampedArray<ArrayBuffer>,
    second: number,
    third?: number,
  ) {
    if (typeof first === 'number') {
      this.width = first;
      this.height = second;
      this.data = new Uint8ClampedArray(new ArrayBuffer(first * second * 4));
    } else {
      this.width = second;
      this.height = third ?? second;
      this.data = first;
    }
  }
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('requestAnimationFrame', undefined); // the jsdom-less fallback: setTimeout 0
  vi.stubGlobal('ImageData', ImageDataStub); // absent in this jsdom setup (see above)
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(function (
    this: HTMLCanvasElement,
  ) {
    let calls = callsByCanvas.get(this);
    if (calls === undefined) {
      calls = [];
      callsByCanvas.set(this, calls);
    }
    const ctx = {
      clearRect: (...args: unknown[]): void => {
        calls.push({ op: 'clearRect', args });
      },
      putImageData: (...args: unknown[]): void => {
        calls.push({ op: 'putImageData', args });
      },
      drawImage: (...args: unknown[]): void => {
        calls.push({ op: 'drawImage', args });
      },
    };
    return ctx as unknown as CanvasRenderingContext2D;
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('SurfaceRenderer', () => {
  it('sizes the backing store to surface size * scale and stays out of layout', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 100, height: 80 }, 120));
    registry.apply(surfaceNewEnvelope(2, { width: 100, height: 80 }, 240));
    const { conn } = makeFakeConn();
    const one = document.createElement('canvas');
    const two = document.createElement('canvas');

    SurfaceRenderer.attach(one, 1, { registry, conn });
    SurfaceRenderer.attach(two, 2, { registry, conn });

    expect(one.width).toBe(100);
    expect(one.height).toBe(80);
    expect(two.width).toBe(200); // 2x device scale
    expect(two.height).toBe(160);
  });

  it('draws tiles, then acks the sequence only after the paint', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    const { conn, sent } = makeFakeConn();
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });

    conn.events.emit(
      'message',
      frameEnvelope(1, 1, true, [rawTile(2, 3)]),
    );

    // Drawn synchronously: the tile's ImageData was put at its rect, and the offscreen was
    // blitted to the visible canvas in the same turn.
    const visibleCalls = recordedCalls(canvas);
    expect(visibleCalls.map((c) => c.op)).toEqual(['drawImage']);
    const offscreen = visibleCalls[0]?.args[0] as HTMLCanvasElement;
    const offscreenCalls = recordedCalls(offscreen);
    expect(offscreenCalls.map((c) => c.op)).toEqual(['clearRect', 'putImageData']); // full_redraw cleared first
    const put = offscreenCalls[1]?.args;
    expect((put?.[0] as ImageData).width).toBe(1);
    expect(put?.[1]).toBe(2);
    expect(put?.[2]).toBe(3);

    // Not acked yet: the ack waits for the paint turn.
    expect(sent).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(sent).toEqual([
      { kind: 'frameAck', frameAck: { surfaceId: 1, sequence: 1 } },
    ]);
  });

  it('drops an old sequence without drawing or acking it', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    const { conn, sent } = makeFakeConn();
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });

    conn.events.emit('message', frameEnvelope(1, 5, false, [rawTile(0, 0)]));
    vi.advanceTimersByTime(1);
    expect(sent).toHaveLength(1);

    const drawn = recordedCalls(canvas).length;
    conn.events.emit('message', frameEnvelope(1, 5, false, [rawTile(1, 1)]));
    conn.events.emit('message', frameEnvelope(1, 2, false, [rawTile(1, 1)]));
    vi.advanceTimersByTime(1);

    expect(recordedCalls(canvas)).toHaveLength(drawn); // nothing new reached the canvas
    expect(sent).toHaveLength(1); // and nothing new was acked
  });

  it('draws and acks a full_redraw whose sequence went backwards after a resume', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    const { conn, sent } = makeFakeConn();
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });

    conn.events.emit('message', frameEnvelope(1, 9, false, [rawTile(0, 0)]));
    vi.advanceTimersByTime(1);
    expect(sent).toHaveLength(1);

    conn.events.emit('resumed', undefined); // a resume restarts the sequences
    conn.events.emit('message', frameEnvelope(1, 1, true, [rawTile(4, 4)]));
    vi.advanceTimersByTime(1);

    expect(sent).toEqual([
      { kind: 'frameAck', frameAck: { surfaceId: 1, sequence: 9 } },
      { kind: 'frameAck', frameAck: { surfaceId: 1, sequence: 1 } },
    ]);
  });

  it('scales tiles through a scratch canvas when the surface scale is not 1x', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 5, height: 4 }, 240));
    const { conn } = makeFakeConn();
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });
    expect(canvas.width).toBe(10);

    conn.events.emit('message', frameEnvelope(1, 1, false, [rawTile(2, 1)]));

    const offscreen = recordedCalls(canvas)[0]?.args[0] as HTMLCanvasElement;
    const drawCalls = recordedCalls(offscreen).filter((c) => c.op === 'drawImage');
    expect(drawCalls).toHaveLength(1);
    // The 1x1 tile at logical (2, 1) lands at device (4, 2) with 2x2 extent.
    expect(drawCalls[0]?.args.slice(1)).toEqual([4, 2, 2, 2]);
  });

  it('drops a whole frame whose tiles do not decode, and acks nothing', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    const { conn, sent } = makeFakeConn();
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });

    // A RAW 1x1 tile with 3 bytes: the decoder must refuse it.
    conn.events.emit('message', frameEnvelope(1, 1, false, [rawTile(0, 0, [1, 2, 3])]));
    vi.advanceTimersByTime(1);

    expect(recordedCalls(canvas)).toHaveLength(0);
    expect(sent).toEqual([]);
  });

  it('resizes the backing store when a configure is acked', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 100, height: 80 }, 120));
    const { conn } = makeFakeConn();
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });
    expect(canvas.width).toBe(100);

    registry.apply({
      kind: 'configureAck',
      configureAck: { surfaceId: 1, serial: 4, size: { width: 640, height: 480 } },
    });

    expect(canvas.width).toBe(640);
    expect(canvas.height).toBe(480);
  });

  it('stops drawing and acking once the surface is gone', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    const { conn, sent } = makeFakeConn();
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });

    registry.apply({ kind: 'surfaceGone', surfaceGone: { surfaceId: 1, reason: 0 } });
    conn.events.emit('message', frameEnvelope(1, 1, false, [rawTile(0, 0)]));
    vi.advanceTimersByTime(1);

    expect(recordedCalls(canvas)).toHaveLength(0);
    expect(sent).toEqual([]);
  });

  it('ignores frames addressed to other surfaces', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    registry.apply(surfaceNewEnvelope(2, { width: 10, height: 6 }, 120));
    const { conn, sent } = makeFakeConn();
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });

    conn.events.emit('message', frameEnvelope(2, 1, false, [rawTile(0, 0)]));
    vi.advanceTimersByTime(1);

    expect(recordedCalls(canvas)).toHaveLength(0);
    expect(sent).toEqual([]);
  });

  it('still acks a frame drawn just before detach: a stray ack is ignored, never fatal', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    const { conn, sent } = makeFakeConn();
    const canvas = document.createElement('canvas');
    const attached = SurfaceRenderer.attach(canvas, 1, { registry, conn });

    conn.events.emit('message', frameEnvelope(1, 1, false, [rawTile(0, 0)]));
    attached.detach();
    vi.advanceTimersByTime(1);

    // The rule is "ack only what was actually drawn" (wire.proto FrameAck), and this frame
    // was drawn; the server ignores an ack for a sequence it no longer holds.
    expect(sent).toEqual([
      { kind: 'frameAck', frameAck: { surfaceId: 1, sequence: 1 } },
    ]);

    // But nothing drawn or acked after the detach.
    conn.events.emit('message', frameEnvelope(1, 2, false, [rawTile(1, 1)]));
    vi.advanceTimersByTime(1);
    expect(sent).toHaveLength(1);
  });
});
