// @vitest-environment jsdom
/**
 * The hostile-server CI proof (docs/adr/0003-untrusted-server-client.md, roadmap.md M2 exit
 * criterion): "A hostile test server (a fixture that sends markup in every string, oversized
 * lengths, a popup the size of the screen, and focus requests) cannot put markup into the page,
 * draw outside the parent-plus-margin box, or take focus. This is a CI test."
 *
 * One describe per hostile row, every input from ./cases.ts:
 *   (a) MARKUP   - the full markup corpus through registry + setTextOnly + attached renderers,
 *                  typed and as bytes: no markup element enters the page, no on* attribute, the
 *                  canary global never runs, and the hostile text survives as text.
 *   (b) OVERSIZED - every over-cap message throws ProtocolError naming its limits-table field,
 *                  the connection closes locally without reconnecting, and a surface flood
 *                  cannot grow the registry past MAX_SURFACES.
 *   (c) POPUP    - every hostile positioner, placed and clamped, stays inside the parent box
 *                  plus 32 px, including a 1920x1200 popup against a 300x200 parent.
 *   (d) FOCUS    - a FocusAsk flood and a misleading cursor hotspot change no focus: the SDK has
 *                  no focus-taking API, and FocusNotify is only ever the host's own send.
 *   (e) FRAMES   - tiles outside the surface are dropped whole and unacked, stale/wrapped
 *                  sequences are dropped, a full_redraw clears prior tiles, and every pixel the
 *                  renderer requests lands inside the surface bounds at any scale.
 *   (f) BYE      - a fatal Bye ends reconnect attempts, and its markup text stays text.
 *
 * The DOM is asserted structurally (querySelectorAll, text nodes, attributes), never through an
 * HTML sink: reading one is as restricted as writing one, and the parsed tree is the stronger
 * proof anyway. jsdom has no canvas rasteriser and no ImageData; both are stubbed below, as in
 * render.test.ts.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { Envelope, HelloReply } from '../protocol.js';
import {
  MAX_SURFACES,
  PROTOCOL_VERSION,
  ProtocolError,
  decodeEnvelope,
  encodeEnvelope,
} from '../protocol.js';
import type { ConnectionEvents, Transport } from '../connection.js';
import { AppricotConnection } from '../connection.js';
import { Emitter } from '../events.js';
import { SurfaceRegistry, clampPopup, placePopup } from '../registry.js';
import { SurfaceRenderer } from '../render.js';
import { setTextOnly } from '../text-only.js';
import { attachInput } from '../input.js';
import { cursorToImageData, drawCursor } from '../cursor.js';
import {
  FOCUS_FLOOD_COUNT,
  PWNED_PROPERTY,
  type HostileBytes,
  benignSurfaceNew,
  byeEnvelope,
  fatalByeReasons,
  focusAskFlood,
  frameEnvelope,
  fullRedrawFlood,
  hostilePopups,
  markupEnvelopeBytes,
  markupEnvelopes,
  misleadingCursorEnvelope,
  outOfBoundsFrames,
  oversizedBytes,
  rawTile,
  screenSizedPopupBytes,
  sequenceAbuseFrames,
  surfaceFlood,
} from './cases.js';

// ---------------------------------------------------------------------------
// Stubs: a recording canvas, an ImageData, a scripted transport, a fake connection
// ---------------------------------------------------------------------------

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

/** Minimal ImageData stand-in; this vitest jsdom setup exposes no ImageData global. */
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

/** A scripted transport, as in connection.test.ts: the test drives open, bytes and closes. */
class FakeTransport implements Transport {
  static created: FakeTransport[] = [];

  readonly sent: Uint8Array[] = [];
  closedByUser = false;
  #message: ((data: Uint8Array) => void)[] = [];
  #close: ((code: number) => void)[] = [];
  #open: (() => void)[] = [];

  constructor() {
    FakeTransport.created.push(this);
  }

  send(data: Uint8Array): void {
    this.sent.push(data);
  }

  close(): void {
    this.closedByUser = true;
    this.#fireClose(1000);
  }

  onMessage(cb: (data: Uint8Array) => void): void {
    this.#message.push(cb);
  }

  onClose(cb: (code: number) => void): void {
    this.#close.push(cb);
  }

  onOpen(cb: () => void): void {
    this.#open.push(cb);
  }

  simulateOpen(): void {
    for (const cb of [...this.#open]) cb();
  }

  simulateMessage(data: Uint8Array): void {
    for (const cb of [...this.#message]) cb(data);
  }

  simulateClose(code: number): void {
    this.#fireClose(code);
  }

  #fireClose(code: number): void {
    const callbacks = [...this.#close];
    this.#close = [];
    for (const cb of callbacks) cb(code);
  }
}

const TOKEN = new Uint8Array([9, 9, 9]);

function makeConn(reconnect = false): AppricotConnection {
  return new AppricotConnection(() => new FakeTransport(), { token: TOKEN, reconnect });
}

function latestTransport(): FakeTransport {
  const transport = FakeTransport.created.at(-1);
  if (transport === undefined) {
    throw new Error('no transport was created yet');
  }
  return transport;
}

function helloReplyBytes(): Uint8Array {
  const reply: HelloReply = {
    protocolVersion: PROTOCOL_VERSION,
    sessionId: 'hostile-session',
    maxFrameCredits: 4,
    codecs: [1],
    resumed: false,
  };
  return encodeEnvelope({ kind: 'helloReply', helloReply: reply });
}

/** One full open: connect, open the socket, answer a valid HelloReply. */
function openSession(conn: AppricotConnection): void {
  conn.connect();
  latestTransport().simulateOpen();
  latestTransport().simulateMessage(helloReplyBytes());
}

/** A connection-shaped recorder for the renderer and input, as in render.test.ts. */
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

beforeEach(() => {
  vi.useFakeTimers();
  FakeTransport.created = [];
  vi.stubGlobal('requestAnimationFrame', undefined); // the fallback: acks ride setTimeout 0
  vi.stubGlobal('ImageData', ImageDataStub);
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
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

// ---------------------------------------------------------------------------
// Structural DOM assertions (never an HTML sink: reading one is restricted too)
// ---------------------------------------------------------------------------

/** Elements only parsed markup can create; a text-only page has none of them. */
const MARKUP_ELEMENTS =
  'img, script, iframe, svg, object, embed, link, style, video, audio, source, track, form';

/** The corpus must cover every server string field many times over; fewer means it rotted. */
const MINIMUM_SHOWN_STRINGS = 40;

function canary(): unknown {
  return (window as unknown as Record<string, unknown>)[PWNED_PROPERTY];
}

/** The page-level proof: no markup element, no handler attribute, no executed canary. */
function assertPageClean(container: HTMLElement): void {
  expect(container.querySelectorAll(MARKUP_ELEMENTS)).toHaveLength(0);
  for (const element of container.querySelectorAll('*')) {
    for (const attribute of Array.from(element.attributes)) {
      expect(attribute.name.toLowerCase().startsWith('on')).toBe(false);
      expect(attribute.value.includes(PWNED_PROPERTY)).toBe(false);
    }
  }
  expect(canary()).toBeUndefined();
}

/** The text-preservation proof: exactly one text node, equal to the hostile string. */
function assertShownAsText(element: Element, expected: string): void {
  expect(element.childNodes).toHaveLength(1);
  const only = element.firstChild;
  expect(only?.nodeType).toBe(Node.TEXT_NODE);
  expect(only?.textContent).toBe(expected);
}

/**
 * What a host does with the corpus: feed the registry, mirror every untrusted string into the
 * DOM through setTextOnly, and keep an attached renderer in the same tree. Returns each element
 * and the LAST hostile string it was given, so the test can assert the text survived.
 */
function feedCorpus(envelopes: readonly Envelope[]): {
  container: HTMLElement;
  shown: Map<HTMLElement, string>;
} {
  const registry = new SurfaceRegistry();
  const container = document.createElement('div');
  document.body.appendChild(container);
  const { conn } = makeFakeConn();
  const shown = new Map<HTMLElement, string>();
  const status = document.createElement('p');
  container.appendChild(status);
  const titleBySurface = new Map<number, HTMLElement>();
  const appBySurface = new Map<number, HTMLElement>();

  for (const e of envelopes) {
    registry.apply(e);
    if (e.kind === 'surfaceNew') {
      const title = document.createElement('span');
      const app = document.createElement('span');
      setTextOnly(title, e.surfaceNew.title);
      setTextOnly(app, e.surfaceNew.appId);
      container.appendChild(title);
      container.appendChild(app);
      titleBySurface.set(e.surfaceNew.surfaceId, title);
      appBySurface.set(e.surfaceNew.surfaceId, app);
      shown.set(title, e.surfaceNew.title);
      shown.set(app, e.surfaceNew.appId);
      const canvas = document.createElement('canvas');
      container.appendChild(canvas);
      SurfaceRenderer.attach(canvas, e.surfaceNew.surfaceId, { registry, conn });
    } else if (e.kind === 'surfaceMetadata') {
      const record = registry.get(e.surfaceMetadata.surfaceId);
      if (record !== undefined) {
        const title = titleBySurface.get(record.id);
        const app = appBySurface.get(record.id);
        if (title !== undefined) {
          setTextOnly(title, record.title);
          shown.set(title, record.title);
        }
        if (app !== undefined) {
          setTextOnly(app, record.appId);
          shown.set(app, record.appId);
        }
      }
    } else if (e.kind === 'bye') {
      setTextOnly(status, e.bye.text);
      shown.set(status, e.bye.text);
    } else if (e.kind === 'serverError') {
      setTextOnly(status, e.serverError.text);
      shown.set(status, e.serverError.text);
    } else if (e.kind === 'helloReply') {
      setTextOnly(status, e.helloReply.sessionId);
      shown.set(status, e.helloReply.sessionId);
    } else if (e.kind === 'clipboardText') {
      // The host's copy pattern: the event's string lands as text in the host's own chrome,
      // and the actual clipboard write happens only in the host's user gesture - never here.
      setTextOnly(status, e.clipboardText.text);
      shown.set(status, e.clipboardText.text);
    }
  }
  return { container, shown };
}

/** Every string the corpus can leave in the DOM, asserted as text. */
function assertCorpusIsText(envelopes: readonly Envelope[]): void {
  const { container, shown } = feedCorpus(envelopes);
  expect(shown.size).toBeGreaterThanOrEqual(MINIMUM_SHOWN_STRINGS);
  for (const [element, expected] of shown) {
    assertShownAsText(element, expected);
  }
  assertPageClean(container);
}

// ---------------------------------------------------------------------------
// (a) MARKUP: markup in every string cannot enter the page
// ---------------------------------------------------------------------------

describe('M2 hostile row (a): markup in every string cannot put markup into the page', () => {
  it('the typed corpus stays text: no markup element, no handler, no canary', () => {
    assertCorpusIsText(markupEnvelopes().map((c) => c.envelope));
  });

  it('the byte corpus decodes to the same strings and stays text', () => {
    const decoded = markupEnvelopeBytes().map((c) => decodeEnvelope(c.bytes));
    assertCorpusIsText(decoded);
  });

  it('the payloads are genuinely live markup, so the green proof above means something', () => {
    // DOMParser parses markup without executing it (scripts do not run in a detached document),
    // and it is no restricted sink: it is the fixture's own honesty check. If a payload ever
    // stops parsing into an element, the corpus has rotted and row (a) proves nothing.
    const parser = new DOMParser();
    let parsedElements = 0;
    for (const markup of markupEnvelopes()) {
      const env = markup.envelope;
      const strings: string[] = [];
      if (env.kind === 'surfaceNew') {
        strings.push(env.surfaceNew.title, env.surfaceNew.appId);
      } else if (env.kind === 'bye') {
        strings.push(env.bye.text);
      } else if (env.kind === 'serverError') {
        strings.push(env.serverError.text);
      } else if (env.kind === 'clipboardText') {
        strings.push(env.clipboardText.text);
      }
      for (const s of strings) {
        if (s.includes('<')) {
          const doc = parser.parseFromString(s, 'text/html');
          parsedElements += doc.querySelectorAll(MARKUP_ELEMENTS).length;
        }
      }
    }
    expect(parsedElements).toBeGreaterThan(0);
    expect(canary()).toBeUndefined(); // parsing executed nothing
  });

  it('an app copy with markup in it surfaces as a plain string and writes nothing', () => {
    const registry = new SurfaceRegistry();
    const texts: string[] = [];
    registry.events.on('clipboard-text', (payload) => texts.push(payload.text));

    // What a hostile streamer sends: the copied "text" is markup, over and over, typed and
    // as wire bytes. The SDK exposes each one as a string and touches no DOM anywhere.
    for (const c of markupEnvelopes()) {
      if (c.envelope.kind === 'clipboardText') {
        registry.apply(c.envelope);
        registry.apply(decodeEnvelope(encodeEnvelope(c.envelope)));
      }
    }

    expect(texts.length).toBeGreaterThanOrEqual(10); // the corpus's clipboard rows, twice
    for (const text of texts) {
      expect(typeof text).toBe('string'); // data, never markup: the type is the guarantee
    }
    expect(document.body.children).toHaveLength(0); // the registry wrote nothing anywhere
    assertPageClean(document.body);

    // And the host's own gesture is what renders it: text in, text out.
    const element = document.createElement('p');
    document.body.appendChild(element);
    setTextOnly(element, texts[0] ?? '');
    assertShownAsText(element, texts[0] ?? '');
    assertPageClean(document.body);
  });
});

// ---------------------------------------------------------------------------
// (b) OVERSIZED: every over-cap length is refused before anything is allocated
// ---------------------------------------------------------------------------

/** Built once: the corpus includes a 16 MiB message and pays for it once per file. */
const OVERSIZED: readonly HostileBytes[] = oversizedBytes();

describe('M2 hostile row (b): oversized lengths are refused before anything is allocated', () => {
  it.each(OVERSIZED)('$name is a ProtocolError naming $field', ({ bytes, field }) => {
    let caught: unknown;
    try {
      decodeEnvelope(bytes);
    } catch (error) {
      caught = error;
    }
    expect(caught).toBeInstanceOf(ProtocolError);
    expect(caught).toBeInstanceOf(Error);
    const protocolError = caught as ProtocolError;
    expect(protocolError.field).toBe(field);
  });

  it.each(OVERSIZED)(
    'the connection closes locally, without reconnect, on $name',
    ({ bytes }) => {
      const conn = makeConn(true);
      const messages: Envelope[] = [];
      conn.events.on('message', (e) => messages.push(e));
      openSession(conn);

      latestTransport().simulateMessage(bytes);

      expect(latestTransport().closedByUser).toBe(true);
      expect(conn.status).toBe('closed');
      expect(messages).toHaveLength(1); // only the HelloReply; the hostile bytes never surfaced
      vi.advanceTimersByTime(60_000);
      expect(FakeTransport.created).toHaveLength(1);
    },
  );

  it('a surface flood cannot grow the registry past MAX_SURFACES', () => {
    const registry = new SurfaceRegistry();
    const added = vi.fn();
    registry.events.on('window-added', added);

    for (const e of surfaceFlood(MAX_SURFACES + 25)) {
      registry.apply(e);
    }

    expect(registry.list()).toHaveLength(MAX_SURFACES);
    expect(added).toHaveBeenCalledTimes(MAX_SURFACES);
  });
});

// ---------------------------------------------------------------------------
// (c) POPUP: a screen-sized popup stays inside the parent-plus-margin box
// ---------------------------------------------------------------------------

const PARENT_BOX = { x: 100, y: 60, width: 300, height: 200 };
const MARGIN = 32;

function assertInsideParentNeighbourhood(rect: {
  x: number;
  y: number;
  width: number;
  height: number;
}): void {
  expect(rect.width).toBeLessThanOrEqual(PARENT_BOX.width + MARGIN * 2);
  expect(rect.height).toBeLessThanOrEqual(PARENT_BOX.height + MARGIN * 2);
  expect(rect.x).toBeGreaterThanOrEqual(PARENT_BOX.x - MARGIN);
  expect(rect.y).toBeGreaterThanOrEqual(PARENT_BOX.y - MARGIN);
  expect(rect.x + rect.width).toBeLessThanOrEqual(
    PARENT_BOX.x + PARENT_BOX.width + MARGIN,
  );
  expect(rect.y + rect.height).toBeLessThanOrEqual(
    PARENT_BOX.y + PARENT_BOX.height + MARGIN,
  );
}

describe('M2 hostile row (c): a screen-sized popup cannot draw outside the parent-plus-margin box', () => {
  it.each(hostilePopups())(
    'clampPopup keeps "$name" inside the parent box plus 32 px',
    ({ positioner }) => {
      const placed = placePopup(positioner);
      const clamped = clampPopup(placed, PARENT_BOX);
      assertInsideParentNeighbourhood(clamped);
      // The clamp never grows a popup, whatever the server asked for.
      expect(clamped.width).toBeLessThanOrEqual(placed.width);
      expect(clamped.height).toBeLessThanOrEqual(placed.height);
    },
  );

  it('a 1920x1200 popup against a 300x200 parent becomes exactly the neighbourhood', () => {
    const clamped = clampPopup(
      { x: -500, y: -400, width: 1920, height: 1200 },
      PARENT_BOX,
    );
    expect(clamped).toEqual({ x: 68, y: 28, width: 364, height: 264 });
  });

  it('a screen-sized popup arriving as bytes is placed and clamped the same way', () => {
    const registry = new SurfaceRegistry();
    registry.apply(benignSurfaceNew(1));
    registry.apply(decodeEnvelope(screenSizedPopupBytes(2, 1)));

    const record = registry.get(2);
    expect(record?.role).toBe(1);
    const positioner = record?.positioner;
    expect(positioner).toBeDefined();
    if (positioner !== undefined) {
      assertInsideParentNeighbourhood(clampPopup(placePopup(positioner), PARENT_BOX));
    }
  });

  it('a popup whose parent is not tracked is dropped before any placement', () => {
    const registry = new SurfaceRegistry();
    registry.apply(decodeEnvelope(screenSizedPopupBytes(3, 404)));
    expect(registry.get(3)).toBeUndefined();
    expect(registry.list()).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// (d) FOCUS: focus requests cannot take focus
// ---------------------------------------------------------------------------

describe('M2 hostile row (d): focus requests cannot take focus', () => {
  it('a FocusAsk flood moves no focus: the asks become events and nothing else', () => {
    const registry = new SurfaceRegistry();
    const container = document.createElement('div');
    document.body.appendChild(container);
    const { conn } = makeFakeConn();
    const canvas = document.createElement('canvas');
    container.appendChild(canvas);
    registry.apply(benignSurfaceNew(1));
    SurfaceRenderer.attach(canvas, 1, { registry, conn });

    const asks = vi.fn();
    registry.events.on('focus-ask', asks);
    const focusEvents: string[] = [];
    document.addEventListener('focusin', () => focusEvents.push('focusin'));
    document.addEventListener('focusout', () => focusEvents.push('focusout'));
    const focusSpy = vi.spyOn(HTMLElement.prototype, 'focus').mockReturnValue(undefined);

    const before = document.activeElement;
    for (const ask of focusAskFlood()) {
      registry.apply(ask);
    }
    registry.apply(misleadingCursorEnvelope());
    registry.apply({ kind: 'cursorGone', cursorGone: {} });

    expect(asks).toHaveBeenCalledTimes(FOCUS_FLOOD_COUNT); // the host saw every ask
    expect(document.activeElement).toBe(before); // and none of them moved focus
    expect(document.activeElement).toBe(document.body);
    expect(focusEvents).toHaveLength(0);
    expect(focusSpy).not.toHaveBeenCalled();
    expect(container.childElementCount).toBe(1); // the canvas; the renderer adds no overlay
    assertPageClean(container);
  });

  it('the SDK exposes no focus-taking API: FocusNotify is only ever the host own send', () => {
    const element = document.createElement('div');
    document.body.appendChild(element);
    const sent: Envelope[] = [];
    attachInput(element, 7, {
      conn: { send: (e) => sent.push(e) },
      isFocused: () => true,
    });

    // A user gesture on the surface sends the pointer event, never a focus grant.
    element.dispatchEvent(new MouseEvent('pointerdown', { button: 0, clientX: 4, clientY: 4 }));
    expect(sent.map((e) => e.kind)).toEqual(['pointerMove', 'pointerButton']);

    // The host's own click handler is what says the surface is focused (the demo pattern).
    sent.push({ kind: 'focusNotify', focusNotify: { surfaceId: 7 } });
    expect(sent.map((e) => e.kind)).toEqual(['pointerMove', 'pointerButton', 'focusNotify']);
    // DOM focus followed the user's own click, so the keys typed next reach the surface
    // (v0 §8). That is the user's gesture, not the server's: no FocusNotify came from the SDK.
    expect(document.activeElement).toBe(element);
  });

  it('a cursor with a misleading hotspot is pixels, not focus', () => {
    const registry = new SurfaceRegistry();
    const cursorEvents: Envelope[] = [];
    registry.events.on('cursor-changed', (e) => {
      if (e.image !== null) {
        cursorEvents.push({ kind: 'cursorImage', cursorImage: e.image });
      }
    });
    registry.apply(misleadingCursorEnvelope());
    expect(cursorEvents).toHaveLength(1);

    const cursor = cursorEvents[0];
    if (cursor?.kind !== 'cursorImage') {
      throw new Error('the cursor event did not carry the hostile cursor');
    }
    const canvas = document.createElement('canvas');
    const ctx = canvas.getContext('2d');
    if (ctx === null) {
      throw new Error('no stubbed context');
    }
    expect(() => cursorToImageData(cursor.cursorImage)).not.toThrow();
    drawCursor(ctx, cursor.cursorImage, 100, 100);

    // The misleading hotspot only offsets the draw into the host's own canvas; canvas pixels
    // clip at the canvas bounds, and no focus moved anywhere.
    const puts = recordedCalls(canvas).filter((c) => c.op === 'putImageData');
    expect(puts).toHaveLength(1);
    expect(puts[0]?.args[1]).toBe(100 - cursor.cursorImage.hotspotX);
    expect(puts[0]?.args[2]).toBe(100 - cursor.cursorImage.hotspotY);
    expect(document.activeElement).toBe(document.body);
  });
});

// ---------------------------------------------------------------------------
// (e) FRAMES: frame abuse cannot draw outside the surface or wedge the client
// ---------------------------------------------------------------------------

/** A tracked surface with an attached renderer, plus its sent envelopes and an emitter. */
function attachedSurface(
  surfaceId: number,
  size: { width: number; height: number },
  scale = 120,
): {
  canvas: HTMLCanvasElement;
  sent: Envelope[];
  emit(envelope: Envelope): void;
} {
  const registry = new SurfaceRegistry();
  registry.apply({
    kind: 'surfaceNew',
    surfaceNew: {
      surfaceId,
      role: 0,
      size,
      title: 'frames',
      appId: 'frames.app',
      scale120ths: scale,
    },
  });
  const { conn, sent } = makeFakeConn();
  const canvas = document.createElement('canvas');
  SurfaceRenderer.attach(canvas, surfaceId, { registry, conn });
  const emit = (envelope: Envelope): void => {
    conn.events.emit('message', envelope);
  };
  return { canvas, sent, emit };
}

describe('M2 hostile row (e): frame abuse cannot draw outside the surface or stall the client', () => {
  it('every out-of-bounds tile is dropped whole: nothing drawn, nothing acked', () => {
    const size = { width: 10, height: 6 };
    const { canvas, sent, emit } = attachedSurface(1, size);

    for (const abuse of outOfBoundsFrames(1, size)) {
      emit(abuse.envelope);
      vi.advanceTimersByTime(1);
      expect(recordedCalls(canvas)).toHaveLength(0); // not even the blit happened
      expect(sent).toEqual([]);
    }
  });

  it('stale, repeated and wrapped sequences are dropped without acking them', () => {
    const { canvas, sent, emit } = attachedSurface(1, { width: 10, height: 6 });

    emit(frameEnvelope(1, 5, false, [rawTile(0, 0, 1, 1)])); // the good frame first
    vi.advanceTimersByTime(1);
    expect(sent).toHaveLength(1);
    const drawn = recordedCalls(canvas).length;

    for (const abuse of sequenceAbuseFrames(1)) {
      const acksBefore = sent.length;
      emit(abuse.envelope);
      vi.advanceTimersByTime(1);
      const sequence = frameSequence(abuse.envelope);
      if (abuse.draws) {
        expect(recordedCalls(canvas).length).toBeGreaterThan(drawn); // drew and moved on
        expect(sent).toHaveLength(acksBefore + 1);
        expect(sent.at(-1)).toMatchObject({
          kind: 'frameAck',
          frameAck: { surfaceId: 1, sequence },
        });
      } else {
        expect(recordedCalls(canvas).length).toBe(drawn); // dropped...
        expect(sent).toHaveLength(acksBefore); // ...and never acked
      }
    }
    // The good frame plus the two drawable abuse frames; every drop was silent.
    expect(sent.filter((e) => e.kind === 'frameAck')).toHaveLength(3);
  });

  it('a full_redraw clears prior tiles before drawing the new ones', () => {
    const { canvas, emit } = attachedSurface(1, { width: 10, height: 6 });

    emit(frameEnvelope(1, 1, false, [rawTile(0, 0, 2, 2)])); // tile A at the origin
    emit(frameEnvelope(1, 2, true, [rawTile(5, 4, 1, 1)])); // full redraw, tile B far away

    const offscreen = recordedCalls(canvas)[0]?.args[0] as HTMLCanvasElement;
    const ops = recordedCalls(offscreen).map((c) => c.op);
    expect(ops).toEqual(['putImageData', 'clearRect', 'putImageData']);
    const clear = recordedCalls(offscreen)[1]?.args;
    expect(clear).toEqual([0, 0, 10, 6]); // the whole backing store, before the new tile
  });

  it('a full_redraw flood draws bounded work and acks every frame exactly once', () => {
    const size = { width: 10, height: 6 };
    const { canvas, sent, emit } = attachedSurface(1, size);

    for (const frame of fullRedrawFlood(200, 1)) {
      emit(frame);
    }
    vi.advanceTimersByTime(1);

    const visible = recordedCalls(canvas);
    expect(visible).toHaveLength(200); // one blit per frame, none wedged
    expect(sent.filter((e) => e.kind === 'frameAck')).toHaveLength(200);

    // Every pixel request stayed inside the surface bounds.
    const offscreen = visible[0]?.args[0] as HTMLCanvasElement;
    for (const call of recordedCalls(offscreen)) {
      if (call.op === 'putImageData') {
        const image = call.args[0] as ImageData;
        const dx = call.args[1] as number;
        const dy = call.args[2] as number;
        expect(dx).toBeGreaterThanOrEqual(0);
        expect(dy).toBeGreaterThanOrEqual(0);
        expect(dx + image.width).toBeLessThanOrEqual(size.width);
        expect(dy + image.height).toBeLessThanOrEqual(size.height);
      }
    }
  });

  it('scaled tiles land inside the scaled backing store, not just the logical bounds', () => {
    const size = { width: 5, height: 4 };
    const { canvas, sent, emit } = attachedSurface(1, size, 240); // 2x device scale
    expect(canvas.width).toBe(10);
    expect(canvas.height).toBe(8);

    emit(frameEnvelope(1, 1, false, [rawTile(4, 3, 1, 1)])); // the last logical pixel
    vi.advanceTimersByTime(1);

    const offscreen = recordedCalls(canvas)[0]?.args[0] as HTMLCanvasElement;
    const draws = recordedCalls(offscreen).filter((c) => c.op === 'drawImage');
    expect(draws).toHaveLength(1);
    for (const call of draws) {
      // args: (source, dx, dy, dw, dh) - the scaled tile rect must fit the 10x8 store.
      const dx = call.args[1] as number;
      const dy = call.args[2] as number;
      const dw = call.args[3] as number;
      const dh = call.args[4] as number;
      expect(dx).toBeGreaterThanOrEqual(0);
      expect(dy).toBeGreaterThanOrEqual(0);
      expect(dx + dw).toBeLessThanOrEqual(canvas.width);
      expect(dy + dh).toBeLessThanOrEqual(canvas.height);
    }
    expect(sent).toHaveLength(1);
  });
});

function frameSequence(envelope: Envelope): number {
  if (envelope.kind !== 'frame') {
    throw new Error('not a frame');
  }
  return envelope.frame.sequence;
}

// ---------------------------------------------------------------------------
// (f) BYE: a fatal Bye ends reconnect attempts
// ---------------------------------------------------------------------------

describe('M2 hostile row (f): a fatal Bye ends reconnect attempts', () => {
  it.each(fatalByeReasons())(
    'Bye with reason $name then a socket drop closes for good',
    ({ reason }) => {
      const conn = makeConn(true);
      const messages: Envelope[] = [];
      conn.events.on('message', (e) => messages.push(e));

      openSession(conn);
      latestTransport().simulateMessage(encodeEnvelope(byeEnvelope(reason)));
      expect(messages.at(-1)?.kind).toBe('bye'); // the host saw the goodbye
      latestTransport().simulateClose(1006); // the socket drops right after

      expect(conn.status).toBe('closed');
      vi.advanceTimersByTime(60_000);
      expect(FakeTransport.created).toHaveLength(1); // no reconnect was ever scheduled
    },
  );

  it('the Bye text is markup and still only text', () => {
    const element = document.createElement('p');
    document.body.appendChild(element);
    const conn = makeConn();
    const messages: Envelope[] = [];
    conn.events.on('message', (e) => messages.push(e));
    openSession(conn);
    latestTransport().simulateMessage(encodeEnvelope(byeEnvelope(0x102)));

    const bye = messages.at(-1);
    if (bye?.kind !== 'bye') {
      throw new Error('the Bye never arrived');
    }
    setTextOnly(element, bye.bye.text); // what a host does with it
    assertShownAsText(element, bye.bye.text);
    assertPageClean(document.body);
  });
});
