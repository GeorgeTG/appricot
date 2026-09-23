// @vitest-environment jsdom
/**
 * The M2 clipboard row as the client sees it (docs/roadmap.md M2: "Text clipboard in both
 * directions, only as the host's policy allows, and a clipboard write to the user only
 * inside a user gesture").
 *
 * Four clauses, one describe each:
 *   (a) ASK      - a ClipboardAsk from the wire becomes exactly one `clipboard-ask` event
 *                  carrying data and nothing else: the message is fieldless, a hostile
 *                  server cannot smuggle a string onto it (the codec refuses unknown
 *                  fields), and hostile strings in the same stream stay text.
 *   (b) POLICY   - a host that ignores the ask sends nothing and touches no system
 *                  clipboard; the SDK never answers on its own.
 *   (c) GESTURE  - a ClipboardSet leaves only from a user gesture the host allowed. The
 *                  SDK's one paste path is keyboard paste in `attachInput`: opt-in through
 *                  its `paste` policy, it reads text only from the browser's `paste` event
 *                  (paste.ts), and attachInput never cancels the paste chord's keydown, so
 *                  that event can fire. Nothing touches navigator.clipboard (proved
 *                  behaviourally and by scanning the shipped sources). paste.test.ts proves
 *                  the keyboard path itself.
 *   (d) CAP      - a 64 KiB + 1 paste is refused client-side before any bytes reach a
 *                  transport. Through the raw `send` the refusal is a ProtocolError thrown by
 *                  the codec. Through `AppricotConnection.sendClipboardText`, the capped paste
 *                  helper a host calls from its own gesture, and through the keyboard paste
 *                  path, it is quiet: nothing is sent and nothing throws.
 *
 * The DOM is asserted structurally (querySelectorAll, text nodes), never through an HTML
 * sink, as in hostile/hostile.test.ts.
 */
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { cwd } from 'node:process';

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  MAX_CLIPBOARD_BYTES,
  PROTOCOL_VERSION,
  ProtocolError,
  decodeEnvelope,
  encodeEnvelope,
} from './protocol';
import type { Envelope, HelloReply } from './protocol';
import { AppricotConnection } from './connection';
import type { Transport } from './connection';
import { SurfaceRegistry } from './registry';
import { setTextOnly } from './text-only';
import { MARKUP_STRINGS, PWNED_PROPERTY } from './hostile/cases';

// ---------------------------------------------------------------------------
// Stubs: a scripted transport, a poisoned system clipboard, a host paste action
// ---------------------------------------------------------------------------

/** A scripted transport: the test drives open, bytes and closes by hand. */
class FakeTransport implements Transport {
  static created: FakeTransport[] = [];

  readonly sent: Uint8Array[] = [];
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

  #fireClose(code: number): void {
    const callbacks = [...this.#close];
    this.#close = [];
    for (const cb of callbacks) cb(code);
  }
}

const TOKEN = new Uint8Array([7, 7, 7]);

function makeConn(): AppricotConnection {
  return new AppricotConnection(() => new FakeTransport(), { token: TOKEN, reconnect: false });
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
    sessionId: 'clipboard-test',
    maxFrameCredits: 4,
    codecs: [1],
    resumed: false,
  };
  return encodeEnvelope({ kind: 'helloReply', helloReply: reply });
}

/** One full open: connect, open the socket, answer a valid HelloReply. */
function openSession(conn: AppricotConnection): FakeTransport {
  conn.connect();
  const transport = latestTransport();
  transport.simulateOpen();
  transport.simulateMessage(helloReplyBytes());
  return transport;
}

function sentEnvelopes(transport: FakeTransport): Envelope[] {
  return transport.sent.map((bytes) => decodeEnvelope(bytes));
}

function clipboardAskBytes(): Uint8Array {
  return encodeEnvelope({ kind: 'clipboardAsk', clipboardAsk: {} });
}

/** The corpus' first markup payload; an empty corpus means the fixture rotted. */
function firstMarkup(): string {
  const markup = MARKUP_STRINGS[0];
  if (markup === undefined) {
    throw new Error('the hostile markup corpus is empty; the fixture rotted');
  }
  return markup;
}

/** How many times anything read `navigator.clipboard`; armed fresh by `beforeEach`. */
let clipboardReads = 0;

/** Replaces `navigator.clipboard` with a poison getter: reading it is already the failure. */
function armPoisonClipboard(): void {
  clipboardReads = 0;
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    get() {
      clipboardReads += 1;
      return new Proxy(
        {},
        {
          get(): never {
            throw new Error('navigator.clipboard must never be used');
          },
        },
      );
    },
  });
}

/**
 * The host's own paste action, written the way a host page writes it: an explicit user
 * gesture (a menu item in the host chrome) calls it with text the host already holds. It
 * uses the raw `send`, the loud path; `sendClipboardText` is the quiet one.
 */
function hostPaste(conn: AppricotConnection, text: string): void {
  conn.send({ kind: 'clipboardSet', clipboardSet: { text } });
}

beforeEach(() => {
  vi.useFakeTimers();
  FakeTransport.created = [];
  armPoisonClipboard();
});

afterEach(() => {
  document.body.replaceChildren();
  delete (navigator as { clipboard?: unknown }).clipboard;
  vi.useRealTimers();
});

// ---------------------------------------------------------------------------
// Structural DOM assertions, as in hostile/hostile.test.ts (no HTML sink either way)
// ---------------------------------------------------------------------------

/** Elements only parsed markup can create; a text-only page has none of them. */
const MARKUP_ELEMENTS =
  'img, script, iframe, svg, object, embed, link, style, video, audio, source, track, form';

function canary(): unknown {
  return (window as unknown as Record<string, unknown>)[PWNED_PROPERTY];
}

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

// ---------------------------------------------------------------------------
// A minimal protobuf writer, for hostile bytes the SDK's encoder refuses to emit
// ---------------------------------------------------------------------------

function varint(value: number): Uint8Array {
  const out: number[] = [];
  let rest = value;
  while (rest > 0x7f) {
    out.push((rest % 128) | 0x80);
    rest = Math.floor(rest / 128);
  }
  out.push(rest);
  return Uint8Array.from(out);
}

function concat(parts: readonly Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const out = new Uint8Array(total);
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

/** A length-delimited field: `tag(field, 2)`, varint length, payload. */
function bytesField(field: number, payload: Uint8Array): Uint8Array {
  return concat([
    varint((field << 3) | 2),
    varint(payload.length),
    payload,
  ]);
}

// ---------------------------------------------------------------------------
// (a) ASK: the ask surfaces as data, never as markup
// ---------------------------------------------------------------------------

describe('M2 clipboard row (a): the ask surfaces as data, never as markup', () => {
  it('a ClipboardAsk decoded from real wire bytes becomes exactly one data event', () => {
    const registry = new SurfaceRegistry();
    const asks = vi.fn();
    registry.events.on('clipboard-ask', asks);

    const envelope = decodeEnvelope(clipboardAskBytes());
    registry.apply(envelope);

    expect(asks).toHaveBeenCalledTimes(1);
    expect(asks).toHaveBeenCalledWith(undefined); // the payload: there is nothing to render
    expect(document.body.children).toHaveLength(0); // the registry touched no DOM at all
  });

  it('a hostile server cannot smuggle a string onto the ask: an unknown field is dropped unread', () => {
    // Envelope.clipboard_ask is field 23; a real ClipboardAsk has no fields, so any field is
    // one this client does not know - here field 1 carrying markup, hand-encoded because
    // encodeEnvelope cannot write it. v0 skips an unknown field by its length without reading
    // it (docs/protocol/v0.md section 13; the `clipboard-ask-unknown-field` lenient vector), so
    // the ask decodes empty and the markup never becomes a value the host could render.
    const hostile = bytesField(
      23,
      bytesField(1, new TextEncoder().encode(firstMarkup())),
    );
    const registry = new SurfaceRegistry();
    const asks = vi.fn();
    registry.events.on('clipboard-ask', asks);

    const envelope = decodeEnvelope(hostile);
    expect(envelope).toEqual({ kind: 'clipboardAsk', clipboardAsk: {} });
    registry.apply(envelope);

    expect(asks).toHaveBeenCalledTimes(1);
    expect(asks).toHaveBeenCalledWith(undefined);
    expect(document.body.children).toHaveLength(0);
    expect(canary()).toBeUndefined();
  });

  it('hostile markup in the same stream stays text while the ask stays data', () => {
    const markup = firstMarkup();
    const registry = new SurfaceRegistry();
    const asks = vi.fn();
    registry.events.on('clipboard-ask', asks);

    // What a host does: mirror the untrusted title through setTextOnly, note the ask.
    const title = document.createElement('span');
    document.body.appendChild(title);
    registry.events.on('window-added', ({ surface }) => setTextOnly(title, surface.title));

    registry.apply(
      decodeEnvelope(
        encodeEnvelope({
          kind: 'surfaceNew',
          surfaceNew: {
            surfaceId: 1,
            role: 0,
            size: { width: 8, height: 6 },
            title: markup,
            appId: 'hostile.app',
            scale120ths: 120,
          },
        }),
      ),
    );
    registry.apply(decodeEnvelope(clipboardAskBytes()));

    assertShownAsText(title, markup);
    expect(asks).toHaveBeenCalledWith(undefined);
    assertPageClean(document.body);
  });
});

// ---------------------------------------------------------------------------
// (b) POLICY: a host that ignores the ask sends nothing
// ---------------------------------------------------------------------------

describe('M2 clipboard row (b): a host policy that ignores the ask sends nothing', () => {
  it('an ignored ask produces no ClipboardSet, no system clipboard use, no message', () => {
    const conn = makeConn();
    const registry = new SurfaceRegistry();
    const transport = openSession(conn);

    // The host wiring: envelopes feed the registry; the host simply registers no
    // clipboard-ask listener, which is the "ignore" policy.
    conn.events.on('message', (e) => registry.apply(e));

    transport.simulateMessage(clipboardAskBytes());

    const sent = sentEnvelopes(transport);
    expect(sent).toHaveLength(1); // the Hello, and nothing after it
    expect(sent[0]?.kind).toBe('hello');
    expect(clipboardReads).toBe(0); // nothing read navigator.clipboard
    expect(conn.status).toBe('open'); // the ask disturbed nothing
  });
});

// ---------------------------------------------------------------------------
// (c) GESTURE: a ClipboardSet leaves only from the host's own action
// ---------------------------------------------------------------------------

describe('M2 clipboard row (c): a paste is sent only from the host own action', () => {
  it('the ask alone sends nothing; the host paste action sends exactly one ClipboardSet', () => {
    const conn = makeConn();
    const registry = new SurfaceRegistry();
    const transport = openSession(conn);
    conn.events.on('message', (e) => registry.apply(e));

    // The app pasted: the host sees the ask. Without a user gesture, nothing is answered.
    transport.simulateMessage(clipboardAskBytes());
    expect(sentEnvelopes(transport)).toHaveLength(1); // still only the Hello

    // The explicit host action - a paste menu item the user clicked. The test calls the
    // host's own handler directly; the SDK contributed nothing to getting here.
    hostPaste(conn, 'Γειά σου, πρόχειρο');
    const sent = sentEnvelopes(transport);
    expect(sent).toHaveLength(2);
    const paste = sent[1];
    if (paste?.kind !== 'clipboardSet') {
      throw new Error(`expected a clipboardSet, got ${String(paste?.kind)}`);
    }
    expect(paste.clipboardSet.text).toBe('Γειά σου, πρόχειρο'); // UTF-8, byte for byte
    expect(clipboardReads).toBe(0); // the SDK never touched navigator.clipboard
  });

  it('the SDK sources never touch the system clipboard', () => {
    // The structural proof: whatever runs, nothing may even read navigator.clipboard; this
    // is the shipped-source proof of the same clause. Test files are the tests' own
    // business; everything else under src/ is what a host imports.
    // jsdom rewrites import.meta.url to a non-file scheme, so the scan starts from the
    // process working directory, which vitest sets to this package's root.
    const srcDir = join(cwd(), 'src');
    expect(existsSync(join(srcDir, 'wire.ts'))).toBe(true); // the client package, not some other cwd
    const files: string[] = [];
    const walk = (dir: string): void => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const path = join(dir, entry.name);
        if (entry.isDirectory()) {
          walk(path);
        } else if (entry.name.endsWith('.ts') && !entry.name.endsWith('.test.ts')) {
          files.push(path);
        }
      }
    };
    walk(srcDir);
    expect(files.length).toBeGreaterThan(10); // the scan saw the package, not nothing

    const forbidden = /navigator\s*\.\s*clipboard|execCommand\s*\(/;
    // A paste event's clipboardData is the one sanctioned read (ADR-0003 §7), and only the
    // keyboard paste module may make it.
    const pasteEventRead = /clipboardData/;
    const pasteModule = join(srcDir, 'paste.ts');
    expect(files).toContain(pasteModule);
    for (const file of files) {
      const source = readFileSync(file, 'utf8');
      expect(forbidden.test(source), `${file} touches the system clipboard`).toBe(false);
      if (file !== pasteModule) {
        expect(pasteEventRead.test(source), `${file} reads a paste event`).toBe(false);
      }
    }
  });
});

// ---------------------------------------------------------------------------
// (d) CAP: an oversized paste is refused client-side before any send
// ---------------------------------------------------------------------------

describe('M2 clipboard row (d): a 64 KiB + 1 paste is refused client-side before any send', () => {
  it('the over-cap paste reaches no transport; the session survives it', () => {
    const conn = makeConn();
    const transport = openSession(conn);
    expect(sentEnvelopes(transport)).toHaveLength(1); // the Hello

    // The raw `send` path: the codec's cap throws ProtocolError inside `send` before the
    // transport is touched — the documented loud path. The quiet, capped paste helper
    // (`sendClipboardText`, landed to close this task's source-level gap) is proven in the
    // describe below: it refuses silently where this throws.
    let threw: unknown;
    try {
      hostPaste(conn, 'A'.repeat(MAX_CLIPBOARD_BYTES + 1));
    } catch (error) {
      threw = error;
    }

    expect(threw).toBeInstanceOf(ProtocolError);
    expect((threw as ProtocolError).field).toBe('ClipboardSet.text');
    expect(transport.sent).toHaveLength(1); // nothing beyond the Hello ever left
    expect(conn.status).toBe('open'); // the refusal is not a session end

    // And the cap is inclusive, not off by one: a paste of exactly MAX_CLIPBOARD_BYTES
    // bytes is legal and sends.
    hostPaste(conn, 'α'.repeat(MAX_CLIPBOARD_BYTES / 2)); // two bytes per char
    const sent = sentEnvelopes(transport);
    expect(sent).toHaveLength(2);
    const paste = sent[1];
    if (paste?.kind !== 'clipboardSet') {
      throw new Error(`expected a clipboardSet, got ${String(paste?.kind)}`);
    }
    expect(paste.clipboardSet.text.length).toBe(MAX_CLIPBOARD_BYTES / 2);
    expect(clipboardReads).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// (d, quiet path): sendClipboardText - the capped paste helper a host calls
// ---------------------------------------------------------------------------

describe('M2 clipboard row (d): sendClipboardText refuses quietly where send throws', () => {
  it('an over-cap paste returns false, sends nothing, throws nothing, keeps the session', () => {
    const conn = makeConn();
    const transport = openSession(conn);
    expect(sentEnvelopes(transport)).toHaveLength(1); // the Hello

    expect(conn.sendClipboardText('A'.repeat(MAX_CLIPBOARD_BYTES + 1))).toBe(false);
    expect(transport.sent).toHaveLength(1); // nothing beyond the Hello ever left
    expect(conn.status).toBe('open'); // a refusal is not a session end
  });

  it('the cap counts UTF-8 bytes, not UTF-16 code units', () => {
    const conn = makeConn();
    const transport = openSession(conn);

    // 32,769 Greek characters: 32,769 code units (half the cap) but 65,538 UTF-8 bytes —
    // over by two. A .length check would wave it through; the byte count must not.
    expect(conn.sendClipboardText('α'.repeat(MAX_CLIPBOARD_BYTES / 2 + 1))).toBe(false);

    // Exactly at the cap in bytes: MAX_CLIPBOARD_BYTES / 2 Greek characters, 2 bytes each.
    expect(conn.sendClipboardText('α'.repeat(MAX_CLIPBOARD_BYTES / 2))).toBe(true);
    const sent = sentEnvelopes(transport);
    expect(sent).toHaveLength(2);
    expect(sent[1]?.kind).toBe('clipboardSet');
    expect(clipboardReads).toBe(0);
  });

  it('refuses while not open, without touching a transport', () => {
    const conn = makeConn(); // never connected
    expect(conn.sendClipboardText('x')).toBe(false);
  });
});
