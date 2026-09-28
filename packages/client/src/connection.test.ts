import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  PROTOCOL_VERSION,
  ProtocolError,
  RESUME_GRACE_MS,
  decodeEnvelope,
  encodeEnvelope,
} from './protocol.js';
import type { ByeReason, Envelope, HelloReply } from './protocol.js';
import { AppricotConnection, connectAppricot } from './connection.js';
import type { CloseReason, Transport } from './connection.js';

/** A scripted transport: the test drives open, bytes and closes by hand. */
class FakeTransport implements Transport {
  static created: FakeTransport[] = [];

  readonly sent: Uint8Array[] = [];
  closedByUser = false;
  /** When true, close() only records the request; the test fires the close event later. */
  deferClose = false;
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
    if (!this.deferClose) {
      this.#fireClose(1000);
    }
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

const TOKEN = new Uint8Array([1, 2, 3, 4]);

function makeConn(reconnect = false): AppricotConnection {
  return new AppricotConnection(() => new FakeTransport(), { token: TOKEN, reconnect });
}

function helloReplyBytes(
  opts: {
    version?: number | undefined;
    resumeSerial?: number | undefined;
    resumed?: boolean | undefined;
    resumeGraceMs?: number | undefined;
  } = {},
): Uint8Array {
  const reply: HelloReply = {
    protocolVersion: opts.version ?? PROTOCOL_VERSION,
    sessionId: 'session-1',
    maxFrameCredits: 4,
    codecs: [2],
    resumed: opts.resumed ?? false,
  };
  if (opts.resumeSerial !== undefined) {
    reply.resumeSerial = opts.resumeSerial;
  }
  if (opts.resumeGraceMs !== undefined) {
    reply.resumeGraceMs = opts.resumeGraceMs;
  }
  return encodeEnvelope({ kind: 'helloReply', helloReply: reply });
}

function sentEnvelopes(transport: FakeTransport): Envelope[] {
  return transport.sent.map((bytes) => decodeEnvelope(bytes));
}

/** The newest transport the factory made. */
function latestTransport(): FakeTransport {
  const transport = FakeTransport.created.at(-1);
  if (transport === undefined) {
    throw new Error('no transport was created yet');
  }
  return transport;
}

/** One full open: connect, open the socket, answer a valid HelloReply. */
function openSession(conn: AppricotConnection, resumeSerial?: number): void {
  conn.connect();
  latestTransport().simulateOpen();
  latestTransport().simulateMessage(helloReplyBytes({ resumeSerial }));
}

beforeEach(() => {
  vi.useFakeTimers();
  FakeTransport.created = [];
});

afterEach(() => {
  vi.useRealTimers();
});

describe('AppricotConnection handshake', () => {
  it('sends Hello on open and turns open on a matching HelloReply', () => {
    const conn = makeConn();
    const statuses: string[] = [];
    conn.events.on('status', (s) => statuses.push(s));

    conn.connect();
    const transport = latestTransport();
    transport.simulateOpen();

    expect(transport.sent).toHaveLength(1);
    const [helloEnvelope] = sentEnvelopes(transport);
    if (helloEnvelope?.kind !== 'hello') {
      throw new Error('first message must be a hello');
    }
    expect(helloEnvelope.hello.protocolVersion).toBe(PROTOCOL_VERSION);
    expect(helloEnvelope.hello.clientName).toBe('@app-ricot/client');
    expect(helloEnvelope.hello.streamToken).toEqual(TOKEN);
    expect(helloEnvelope.hello.codecs).toEqual([2, 1]); // QOI best-first, RAW fallback
    expect(helloEnvelope.hello.resumeSerial).toBeUndefined();

    transport.simulateMessage(helloReplyBytes({ resumeSerial: 77 }));
    expect(conn.status).toBe('open');
    expect(conn.sessionId).toBe('session-1');
    expect(conn.maxFrameCredits).toBe(4);
    expect(statuses).toEqual(['connecting', 'open']);
  });

  it('fails without reconnecting when the version does not match', () => {
    const conn = makeConn(true);

    conn.connect();
    latestTransport().simulateOpen();
    latestTransport().simulateMessage(helloReplyBytes({ version: 99 }));

    expect(latestTransport().closedByUser).toBe(true);
    expect(conn.status).toBe('closed');
    vi.advanceTimersByTime(60_000);
    expect(FakeTransport.created).toHaveLength(1);
  });

  it('closes locally, without retry, on bytes that do not decode', () => {
    const conn = makeConn(true);
    const messages = vi.fn();
    conn.events.on('message', messages);
    openSession(conn);

    // An Envelope naming unknown oneof field 99 (tag 0x9a 0x06, empty body): refused.
    latestTransport().simulateMessage(new Uint8Array([0x9a, 0x06, 0x00]));

    expect(latestTransport().closedByUser).toBe(true);
    expect(conn.status).toBe('closed');
    expect(messages).toHaveBeenCalledTimes(1); // only the HelloReply got through
    vi.advanceTimersByTime(60_000);
    expect(FakeTransport.created).toHaveLength(1);
  });
});

describe('AppricotConnection reconnect', () => {
  it('reconnects after an abnormal close and resumes with the stashed serial', () => {
    const conn = makeConn(true);
    const onResumed = vi.fn();
    conn.events.on('resumed', onResumed);

    openSession(conn, 77);
    latestTransport().simulateClose(1006); // the drop
    expect(conn.status).toBe('reconnecting'); // not terminal: a retry is scheduled

    vi.advanceTimersByTime(499);
    expect(FakeTransport.created).toHaveLength(1); // backoff not over yet
    vi.advanceTimersByTime(1);
    expect(FakeTransport.created).toHaveLength(2);

    const second = latestTransport();
    second.simulateOpen();
    const [helloEnvelope] = sentEnvelopes(second);
    expect(helloEnvelope?.kind).toBe('hello');
    if (helloEnvelope?.kind === 'hello') {
      expect(helloEnvelope.hello.resumeSerial).toBe(77);
    }

    second.simulateMessage(helloReplyBytes({ resumeSerial: 77, resumed: true }));
    expect(conn.status).toBe('open');
    expect(onResumed).toHaveBeenCalledTimes(1);

    // A successful reply reset the backoff: the next drop waits 500 ms again, not 1000.
    second.simulateClose(1006);
    vi.advanceTimersByTime(500);
    expect(FakeTransport.created).toHaveLength(3);
  });

  it('doubles the backoff per failed attempt, capped at 8 s', () => {
    const conn = makeConn(true);
    const waits = [500, 1000, 2000, 4000, 8000, 8000];

    // A session whose grace outlasts the whole run, so only the backoff is measured.
    conn.connect();
    latestTransport().simulateOpen();
    latestTransport().simulateMessage(helloReplyBytes({ resumeGraceMs: 60_000 }));
    latestTransport().simulateClose(1006);
    vi.advanceTimersByTime(500);
    let expected = 2;
    for (const wait of waits.slice(1)) {
      const transport = latestTransport();
      transport.simulateOpen();
      transport.simulateClose(1006); // an attempt that never reached a HelloReply
      vi.advanceTimersByTime(wait - 1);
      expect(FakeTransport.created).toHaveLength(expected);
      vi.advanceTimersByTime(1);
      expected += 1;
      expect(FakeTransport.created).toHaveLength(expected);
    }
  });

  it.each([0x100, 0x102, 0x104])(
    'never reconnects after a fatal Bye (reason %d)',
    (reason) => {
      const conn = makeConn(true);
      openSession(conn);

      latestTransport().simulateMessage(
        encodeEnvelope({ kind: 'bye', bye: { reason: reason as ByeReason, text: '' } }),
      );
      latestTransport().simulateClose(1006);

      vi.advanceTimersByTime(60_000);
      expect(FakeTransport.created).toHaveLength(1);
      expect(conn.status).toBe('closed');
    },
  );

  it('does not reconnect after a user close', () => {
    const conn = makeConn(true);
    const closes: number[] = [];
    conn.events.on('close', (code) => closes.push(code));
    openSession(conn);

    conn.close();

    expect(closes).toEqual([1000]);
    expect(conn.status).toBe('closed');
    vi.advanceTimersByTime(60_000);
    expect(FakeTransport.created).toHaveLength(1);
  });

  it('does not reconnect when reconnect was not asked for', () => {
    const conn = makeConn(false);
    openSession(conn);

    latestTransport().simulateClose(1006);

    vi.advanceTimersByTime(60_000);
    expect(FakeTransport.created).toHaveLength(1);
  });
});

describe('AppricotConnection send', () => {
  it('drops sends while not open and encodes them when open', () => {
    const conn = makeConn();

    // Before connect() there is not even a transport; the send is dropped, never thrown.
    expect(() =>
      conn.send({ kind: 'closeRequest', closeRequest: { surfaceId: 1 } }),
    ).not.toThrow();

    openSession(conn);
    conn.send({ kind: 'closeRequest', closeRequest: { surfaceId: 1 } });

    const envelopes = sentEnvelopes(latestTransport());
    expect(envelopes[1]?.kind).toBe('closeRequest');
  });

  it('delivers every decoded envelope through the message event', () => {
    const conn = makeConn();
    const messages: Envelope[] = [];
    conn.events.on('message', (e) => messages.push(e));

    openSession(conn);
    latestTransport().simulateMessage(
      encodeEnvelope({ kind: 'focusAsk', focusAsk: { surfaceId: 3 } }),
    );

    expect(messages.map((e) => e.kind)).toEqual(['helloReply', 'focusAsk']);
  });
});

/** Every 'ended' reason the connection reports, in order. */
function endings(conn: AppricotConnection): CloseReason[] {
  const reasons: CloseReason[] = [];
  conn.events.on('ended', (reason) => reasons.push(reason));
  return reasons;
}

function byeBytes(reason: ByeReason): Uint8Array {
  return encodeEnvelope({ kind: 'bye', bye: { reason, text: '' } });
}

describe('AppricotConnection transports (conn-1)', () => {
  it('connect() during a pending reconnect cancels the timer: exactly one new transport', () => {
    const conn = makeConn(true);
    openSession(conn);
    latestTransport().simulateClose(1006);
    expect(conn.status).toBe('reconnecting');

    conn.connect(); // the host retries by hand before the backoff is over
    expect(FakeTransport.created).toHaveLength(2);
    latestTransport().simulateOpen();
    latestTransport().simulateMessage(helloReplyBytes());
    expect(conn.status).toBe('open');

    vi.advanceTimersByTime(60_000); // the old timer must not open a third transport
    expect(FakeTransport.created).toHaveLength(2);
    expect(conn.status).toBe('open');
  });

  it('close() is synchronous, and a stale transport cannot touch the new one', () => {
    const conn = makeConn(true);
    const statuses: string[] = [];
    const messages: Envelope[] = [];
    conn.events.on('status', (s) => statuses.push(s));
    conn.events.on('message', (e) => messages.push(e));
    openSession(conn);
    const old = latestTransport();
    old.deferClose = true; // like a real WebSocket, the close event comes later

    conn.close();
    expect(conn.status).toBe('closed');
    conn.connect(); // right after close(): not a no-op
    expect(FakeTransport.created).toHaveLength(2);
    const fresh = latestTransport();
    fresh.simulateOpen();
    fresh.simulateMessage(helloReplyBytes());

    // The old socket's late events change nothing.
    old.simulateMessage(encodeEnvelope({ kind: 'focusAsk', focusAsk: { surfaceId: 9 } }));
    old.simulateClose(1006);
    vi.advanceTimersByTime(60_000);

    expect(conn.status).toBe('open');
    expect(FakeTransport.created).toHaveLength(2);
    expect(messages.map((e) => e.kind)).toEqual(['helloReply', 'helloReply']);
    expect(statuses).toEqual(['connecting', 'open', 'closed', 'connecting', 'open']);
    conn.send({ kind: 'closeRequest', closeRequest: { surfaceId: 1 } });
    expect(sentEnvelopes(fresh).at(-1)?.kind).toBe('closeRequest');
  });
});

describe('AppricotConnection endings (conn-2)', () => {
  it('reports why it ended: a user close', () => {
    const conn = makeConn(true);
    const reasons = endings(conn);
    openSession(conn);

    conn.close();

    expect(reasons).toEqual([{ cause: 'user' }]);
    expect(conn.closeReason).toEqual({ cause: 'user' });
  });

  it('reports a refused decode, with the error, and never retries it', () => {
    const conn = makeConn(true);
    const reasons = endings(conn);
    openSession(conn);

    latestTransport().simulateMessage(new Uint8Array([0x9a, 0x06, 0x00]));

    expect(reasons).toHaveLength(1);
    expect(reasons[0]?.cause).toBe('refused');
    expect(reasons[0]?.error).toBeInstanceOf(ProtocolError);
    // The goodbye names the violation before the local close (v0 §5).
    const bye = sentEnvelopes(latestTransport()).at(-1);
    expect(bye).toEqual({ kind: 'bye', bye: { reason: 0x103, text: '' } });
  });

  it('reports a version it did not ask for with Bye(PROTOCOL_VERSION)', () => {
    const conn = makeConn(true);
    const reasons = endings(conn);
    conn.connect();
    latestTransport().simulateOpen();
    latestTransport().simulateMessage(helloReplyBytes({ version: 99 }));

    expect(reasons[0]?.cause).toBe('refused');
    expect(sentEnvelopes(latestTransport()).at(-1)).toEqual({
      kind: 'bye',
      bye: { reason: 0x100, text: '' },
    });
  });

  it('is terminal only when no retry follows: reconnecting first, closed at the end', () => {
    const conn = makeConn(false);
    const reasons = endings(conn);
    openSession(conn);

    latestTransport().simulateClose(1006);

    expect(conn.status).toBe('closed');
    expect(reasons).toEqual([{ cause: 'dropped', code: 1006 }]);
  });
});

describe('AppricotConnection gives up (conn-3)', () => {
  it('stops reconnecting once resume_grace_ms has passed since the drop', () => {
    const conn = makeConn(true);
    const reasons = endings(conn);
    conn.connect();
    latestTransport().simulateOpen();
    latestTransport().simulateMessage(helloReplyBytes({ resumeSerial: 3, resumeGraceMs: 3000 }));
    latestTransport().simulateClose(1006); // t = 0: the drop

    // Every attempt meets a refused upgrade (1006, no Bye), as after the grace (v0 §7).
    while (conn.status !== 'closed') {
      vi.advanceTimersByTime(100);
      const transport = latestTransport();
      if (conn.status === 'connecting') {
        transport.simulateClose(1006);
      }
    }

    // 500 ms, 1000 ms, then the last attempt clipped to the 3 s grace: t = 0.5, 1.5, 3.0.
    expect(FakeTransport.created).toHaveLength(4);
    expect(reasons).toEqual([{ cause: 'grace-expired', code: 1006 }]);
    vi.advanceTimersByTime(60_000);
    expect(FakeTransport.created).toHaveLength(4);
  });

  it('uses the default grace when the server did not state one', () => {
    const conn = makeConn(true);
    openSession(conn);
    latestTransport().simulateClose(1006);
    const droppedAt = Date.now();

    for (let t = 0; t < 30_000 && conn.status !== 'closed'; t += 100) {
      vi.advanceTimersByTime(100);
      if (conn.status === 'connecting') {
        latestTransport().simulateClose(1006);
      }
    }

    expect(conn.status).toBe('closed');
    expect(conn.closeReason?.cause).toBe('grace-expired');
    expect(Date.now() - droppedAt).toBe(RESUME_GRACE_MS);
  });

  it('never retries a transport factory that throws', () => {
    const failure = new SyntaxError('not a WebSocket URL');
    const conn = new AppricotConnection(
      () => {
        throw failure;
      },
      { token: TOKEN, reconnect: true },
    );
    const reasons = endings(conn);

    conn.connect();
    vi.advanceTimersByTime(60_000);

    expect(conn.status).toBe('closed');
    expect(reasons).toEqual([{ cause: 'transport-error', error: failure }]);
  });
});

describe('AppricotConnection order and direction (conn-4)', () => {
  it('refuses anything but the reply or a goodbye before the HelloReply', () => {
    const conn = makeConn(true);
    const messages: Envelope[] = [];
    conn.events.on('message', (e) => messages.push(e));
    conn.connect();
    latestTransport().simulateOpen();

    latestTransport().simulateMessage(
      encodeEnvelope({ kind: 'focusAsk', focusAsk: { surfaceId: 1 } }),
    );

    expect(messages).toEqual([]);
    expect(conn.status).toBe('closed');
    expect(conn.closeReason?.cause).toBe('refused');
    vi.advanceTimersByTime(60_000);
    expect(FakeTransport.created).toHaveLength(1);
  });

  it('lets a Bye through before the HelloReply', () => {
    const conn = makeConn(true);
    const messages: Envelope[] = [];
    conn.events.on('message', (e) => messages.push(e));
    conn.connect();
    latestTransport().simulateOpen();

    latestTransport().simulateMessage(byeBytes(0x102));
    latestTransport().simulateClose(1005);

    expect(messages.map((e) => e.kind)).toEqual(['bye']);
    expect(conn.closeReason).toEqual({
      cause: 'bye',
      code: 1005,
      bye: { reason: 0x102, text: '' },
    });
  });

  it('refuses a second HelloReply, which would overwrite the session', () => {
    const conn = makeConn(true);
    openSession(conn, 7);

    latestTransport().simulateMessage(helloReplyBytes({ resumeSerial: 99 }));

    expect(conn.status).toBe('closed');
    expect(conn.closeReason?.cause).toBe('refused');
  });

  it('refuses a message only a client sends', () => {
    const conn = makeConn(true);
    const messages: Envelope[] = [];
    conn.events.on('message', (e) => messages.push(e));
    openSession(conn);

    latestTransport().simulateMessage(
      encodeEnvelope({
        kind: 'key',
        key: { keysym: 0x61, code: 'KeyA', pressed: true, modifiers: 0 },
      }),
    );

    expect(messages.map((e) => e.kind)).toEqual(['helloReply']);
    expect(conn.status).toBe('closed');
    expect(conn.closeReason?.cause).toBe('refused');
  });
});

describe('AppricotConnection and a Bye (client-reconnect-1)', () => {
  it.each([
    [0x105, 1005],
    [0x105, 1006],
    [0x103, 1005],
    [0x101, 1006],
    [0x000, 1006],
  ])('never reconnects after Bye %i, whatever the close code (%i)', (reason, code) => {
    const conn = makeConn(true);
    openSession(conn);

    latestTransport().simulateMessage(byeBytes(reason as ByeReason));
    latestTransport().simulateClose(code);
    vi.advanceTimersByTime(60_000);

    expect(FakeTransport.created).toHaveLength(1);
    expect(conn.status).toBe('closed');
    expect(conn.closeReason?.cause).toBe('bye');
    expect(conn.closeReason?.bye?.reason).toBe(reason);
  });
});

describe('AppricotConnection and host listeners (events-1)', () => {
  it('is open before the host hears the HelloReply, even when a listener throws', () => {
    const conn = makeConn();
    const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const heard: string[] = [];
    conn.events.on('message', () => {
      throw new Error('a host bug');
    });
    conn.events.on('message', (e) => {
      heard.push(`${e.kind} while ${conn.status}`);
      conn.send({ kind: 'focusNotify', focusNotify: { surfaceId: 1 } });
    });

    try {
      openSession(conn);
    } finally {
      errors.mockRestore();
    }

    expect(conn.status).toBe('open');
    expect(heard).toEqual(['helloReply while open']);
    expect(sentEnvelopes(latestTransport()).map((e) => e.kind)).toEqual(['hello', 'focusNotify']);
  });
});

describe('connectAppricot', () => {
  it('refuses to run where there is no WebSocket global', () => {
    vi.stubGlobal('WebSocket', undefined);
    try {
      expect(() => connectAppricot('ws://localhost:1/', { token: TOKEN })).toThrow(
        /No WebSocket global/,
      );
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
