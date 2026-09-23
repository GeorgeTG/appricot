// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { Envelope, SurfaceNew } from './protocol.js';
import { Emitter } from './events.js';
import type { AppricotConnection, ConnectionEvents } from './connection.js';
import { SurfaceRegistry } from './registry.js';
import { SurfaceRenderer } from './render.js';

/**
 * jsdom has no canvas rasteriser: getContext returns null. Every canvas in this file gets a
 * recording stub instead, so the tests observe exactly what was drawn and when — which is the
 * renderer's whole contract: draw, then ack after the paint.
 */
interface RecordedCall {
  /** A 2D context call, or `reset`: a width/height write, which clears the canvas bitmap. */
  op: 'clearRect' | 'putImageData' | 'drawImage' | 'reset';
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

/** The calls recorded for `canvas`, created on first use. */
function callsOf(canvas: HTMLCanvasElement): RecordedCall[] {
  let calls = callsByCanvas.get(canvas);
  if (calls === undefined) {
    calls = [];
    callsByCanvas.set(canvas, calls);
  }
  return calls;
}

/**
 * Models the HTML canvas rule the renderer must respect: writing `width` or `height` clears
 * the bitmap, even when the value does not change. Each write is recorded as a `reset`, in
 * order with the drawing calls, and then really applied.
 */
function recordDimensionWrites(): void {
  for (const dimension of ['width', 'height'] as const) {
    const original = Object.getOwnPropertyDescriptor(HTMLCanvasElement.prototype, dimension);
    const set = original?.set;
    if (set === undefined) {
      throw new Error(`HTMLCanvasElement.${dimension} has no setter to spy on`);
    }
    vi.spyOn(HTMLCanvasElement.prototype, dimension, 'set').mockImplementation(function (
      this: HTMLCanvasElement,
      value: number,
    ) {
      callsOf(this).push({ op: 'reset', args: [dimension, value] });
      set.call(this, value);
    });
  }
}

/** What landed on a canvas since its last reset: the pixels it shows now. */
function sinceLastReset(canvas: HTMLCanvasElement): RecordedCall[] {
  const calls = recordedCalls(canvas);
  const last = calls.map((c) => c.op).lastIndexOf('reset');
  return calls.slice(last + 1);
}

function resets(canvas: HTMLCanvasElement): number {
  return recordedCalls(canvas).filter((c) => c.op === 'reset').length;
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('requestAnimationFrame', undefined); // the jsdom-less fallback: setTimeout 0
  vi.stubGlobal('ImageData', ImageDataStub); // absent in this jsdom setup (see above)
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(function (
    this: HTMLCanvasElement,
  ) {
    const calls = callsOf(this);
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

    expect(recordedCalls(canvas).filter((c) => c.op !== 'reset')).toHaveLength(0);
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

    expect(recordedCalls(canvas).filter((c) => c.op !== 'reset')).toHaveLength(0);
    expect(sent).toEqual([]);
  });

  it('shows no other surface in a canvas, yet draws and acks that surface in the client', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    registry.apply(surfaceNewEnvelope(2, { width: 10, height: 6 }, 120));
    const { conn, sent } = makeFakeConn();
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });

    conn.events.emit('message', frameEnvelope(2, 1, false, [rawTile(0, 0)]));
    vi.advanceTimersByTime(1);

    expect(recordedCalls(canvas).filter((c) => c.op !== 'reset')).toHaveLength(0);
    // Surface 2 has no canvas, but its pixels live in the client: drawn, so acked (xc-8).
    expect(sent).toEqual([{ kind: 'frameAck', frameAck: { surfaceId: 2, sequence: 1 } }]);
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

    // After the detach the canvas gets nothing more; the client still draws and acks the
    // surface, so an unmounted window does not burn its credits (xc-8).
    const onCanvas = recordedCalls(canvas).length;
    conn.events.emit('message', frameEnvelope(1, 2, false, [rawTile(1, 1)]));
    vi.advanceTimersByTime(1);
    expect(recordedCalls(canvas)).toHaveLength(onCanvas);
    expect(sent).toHaveLength(2);
  });
});

/** A host's wiring: every envelope the connection delivers feeds the registry first. */
function hostWiring(): {
  registry: SurfaceRegistry;
  conn: AppricotConnection;
  sent: Envelope[];
  deliver(envelope: Envelope): void;
} {
  const registry = new SurfaceRegistry();
  const { conn, sent } = makeFakeConn();
  conn.events.on('message', (envelope) => {
    registry.apply(envelope);
  });
  return {
    registry,
    conn,
    sent,
    deliver: (envelope) => {
      conn.events.emit('message', envelope);
    },
  };
}

function acks(sent: Envelope[]): { surfaceId: number; sequence: number }[] {
  return sent.flatMap((e) => (e.kind === 'frameAck' ? [e.frameAck] : []));
}

describe('SurfaceRenderer canvas resets (render-1)', () => {
  it('never resets a canvas for another surface, or for a title-only change', () => {
    recordDimensionWrites();
    const { registry, conn, deliver } = hostWiring();
    deliver(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    deliver(surfaceNewEnvelope(2, { width: 10, height: 6 }, 120));
    const one = document.createElement('canvas');
    const two = document.createElement('canvas');
    SurfaceRenderer.attach(one, 1, { registry, conn });
    SurfaceRenderer.attach(two, 2, { registry, conn });
    deliver(frameEnvelope(1, 1, true, [rawTile(0, 0)]));
    deliver(frameEnvelope(2, 1, true, [rawTile(0, 0)]));
    const resetsOne = resets(one);
    const resetsTwo = resets(two);

    // Window 1 is retitled, then acked at the size it already has.
    deliver({ kind: 'surfaceMetadata', surfaceMetadata: { surfaceId: 1, title: 'renamed' } });
    deliver({
      kind: 'configureAck',
      configureAck: { surfaceId: 1, serial: 3, size: { width: 10, height: 6 } },
    });
    // Window 2 really resizes: that is window 2's business only.
    deliver({
      kind: 'configureAck',
      configureAck: { surfaceId: 2, serial: 4, size: { width: 20, height: 12 } },
    });

    expect(resets(one)).toBe(resetsOne); // canvas 1 kept its bitmap throughout
    expect(sinceLastReset(one).map((c) => c.op)).toEqual(['drawImage']);
    expect(resets(two)).toBeGreaterThan(resetsTwo);
  });

  it('keeps the pixels across a real resize: the canvas is repainted after its reset', () => {
    recordDimensionWrites();
    const { registry, conn, deliver } = hostWiring();
    deliver(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });
    deliver(frameEnvelope(1, 1, true, [rawTile(2, 3)]));
    const before = recordedCalls(canvas).at(-1)?.args[0] as HTMLCanvasElement;

    deliver({
      kind: 'configureAck',
      configureAck: { surfaceId: 1, serial: 5, size: { width: 20, height: 12 } },
    });

    expect(canvas.width).toBe(20);
    expect(canvas.height).toBe(12);
    // The canvas was cleared by the resize, then shown the backing store again at once...
    const shown = sinceLastReset(canvas);
    expect(shown.map((c) => c.op)).toEqual(['drawImage']);
    // ...and that backing store carries the old pixels, copied across, not a blank one.
    const after = shown[0]?.args[0] as HTMLCanvasElement;
    expect(after).not.toBe(before);
    const copied = recordedCalls(after).filter((c) => c.op !== 'reset');
    expect(copied).toEqual([{ op: 'drawImage', args: [before, 0, 0, 10, 6] }]);
  });
});

describe('SurfaceRenderer backing store (render-2, xc-8)', () => {
  it('draws and acks a full redraw that arrived before attach, and attach shows it', () => {
    const { registry, conn, sent, deliver } = hostWiring();
    deliver(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    deliver(frameEnvelope(1, 1, true, [rawTile(2, 3)])); // no canvas attached yet
    vi.advanceTimersByTime(1);

    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });
    vi.advanceTimersByTime(1);

    expect(acks(sent)).toEqual([{ surfaceId: 1, sequence: 1 }]);
    const shown = recordedCalls(canvas);
    expect(shown.map((c) => c.op)).toEqual(['drawImage']);
    const backing = shown[0]?.args[0] as HTMLCanvasElement;
    expect(recordedCalls(backing).map((c) => c.op)).toEqual(['clearRect', 'putImageData']);

    // A later frame is drawn and acked once, although both paths deliver it.
    deliver(frameEnvelope(1, 2, false, [rawTile(0, 0)]));
    vi.advanceTimersByTime(1);
    expect(acks(sent)).toEqual([
      { surfaceId: 1, sequence: 1 },
      { surfaceId: 1, sequence: 2 },
    ]);
    expect(recordedCalls(canvas).map((c) => c.op)).toEqual(['drawImage', 'drawImage']);
  });

  it('holds at most MAX_FRAME_CREDITS frames per surface before anything attaches', () => {
    const { registry, conn, sent, deliver } = hostWiring();
    deliver(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    for (let sequence = 1; sequence <= 6; sequence++) {
      deliver(frameEnvelope(1, sequence, false, [rawTile(0, 0)]));
    }

    SurfaceRenderer.attach(document.createElement('canvas'), 1, { registry, conn });
    vi.advanceTimersByTime(1);

    // The server may keep only four frames unacked; the fifth and sixth broke that rule.
    expect(acks(sent).map((a) => a.sequence)).toEqual([1, 2, 3, 4]);
  });

  it('keeps drawing and acking while detached, and a remount shows the current pixels', () => {
    const { registry, conn, sent, deliver } = hostWiring();
    deliver(surfaceNewEnvelope(7, { width: 10, height: 6 }, 120));
    const first = document.createElement('canvas');
    const attached = SurfaceRenderer.attach(first, 7, { registry, conn });
    deliver(frameEnvelope(7, 1, true, [rawTile(0, 0)]));
    attached.detach(); // the host hides the window: conditional rendering unmounts it

    for (let sequence = 2; sequence <= 6; sequence++) {
      deliver(frameEnvelope(7, sequence, false, [rawTile(1, 1)]));
    }
    vi.advanceTimersByTime(1);
    // Every frame was acked: no credit is stuck on the hidden window.
    expect(acks(sent).map((a) => a.sequence)).toEqual([1, 2, 3, 4, 5, 6]);
    expect(recordedCalls(first).map((c) => c.op)).toEqual(['drawImage']);

    const second = document.createElement('canvas');
    SurfaceRenderer.attach(second, 7, { registry, conn });
    const shown = recordedCalls(second);
    expect(shown.map((c) => c.op)).toEqual(['drawImage']); // the current pixels, at once
    const backing = shown[0]?.args[0] as HTMLCanvasElement;
    expect(recordedCalls(backing).filter((c) => c.op === 'putImageData')).toHaveLength(6);
  });
});

describe('SurfaceRenderer frames are dropped whole (render-3)', () => {
  it('a legal tile before a hostile one leaves the backing store untouched', () => {
    const { registry, conn, sent, deliver } = hostWiring();
    deliver(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });
    deliver(frameEnvelope(1, 1, false, [rawTile(0, 0)]));
    vi.advanceTimersByTime(1);
    const backing = recordedCalls(canvas)[0]?.args[0] as HTMLCanvasElement;
    const backingOps = recordedCalls(backing).length;
    const shown = recordedCalls(canvas).length;

    // full_redraw, a legal tile, then a RAW 1x1 tile with 3 bytes.
    deliver(frameEnvelope(1, 2, true, [rawTile(4, 4), rawTile(5, 5, [1, 2, 3])]));
    vi.advanceTimersByTime(1);

    expect(recordedCalls(backing)).toHaveLength(backingOps); // no clear, no legal tile
    expect(recordedCalls(canvas)).toHaveLength(shown); // no blit
    expect(acks(sent)).toEqual([{ surfaceId: 1, sequence: 1 }]); // no ack
  });
});

describe('SurfaceRenderer across a resume (client-resume-1)', () => {
  it('repaints a surface resized during the grace at its new size, and acks the redraw', () => {
    const { registry, conn, sent, deliver } = hostWiring();
    deliver(surfaceNewEnvelope(1, { width: 10, height: 6 }, 120));
    deliver(surfaceNewEnvelope(2, { width: 10, height: 6 }, 120));
    const canvas = document.createElement('canvas');
    SurfaceRenderer.attach(canvas, 1, { registry, conn });
    deliver(frameEnvelope(1, 3, true, [rawTile(0, 0)]));
    vi.advanceTimersByTime(1);

    // The socket dropped; during the grace the app grew window 1 and closed window 2.
    conn.events.emit('resumed', undefined);
    deliver({
      kind: 'helloReply',
      helloReply: {
        protocolVersion: 0,
        sessionId: 's',
        maxFrameCredits: 4,
        codecs: [1],
        resumed: true,
      },
    });
    deliver(surfaceNewEnvelope(1, { width: 20, height: 12 }, 120));
    deliver({ kind: 'cursorGone', cursorGone: {} });
    // The resume's full redraw: sequence 1 again, with a tile only the new size can hold.
    deliver(frameEnvelope(1, 1, true, [rawTile(15, 10)]));
    vi.advanceTimersByTime(1);

    expect(registry.list().map((r) => r.id)).toEqual([1]);
    expect(registry.get(1)?.size).toEqual({ width: 20, height: 12 });
    expect(canvas.width).toBe(20);
    expect(acks(sent)).toEqual([
      { surfaceId: 1, sequence: 3 },
      { surfaceId: 1, sequence: 1 },
    ]);
  });
});
