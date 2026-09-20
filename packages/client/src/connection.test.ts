import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { PROTOCOL_VERSION, decodeEnvelope, encodeEnvelope } from './protocol';
import type { ByeReason, Envelope, HelloReply } from './protocol';
import { AppricotConnection, connectAppricot } from './connection';
import type { Transport } from './connection';

/** A scripted transport: the test drives open, bytes and closes by hand. */
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

const TOKEN = new Uint8Array([1, 2, 3, 4]);

function makeConn(reconnect = false): AppricotConnection {
  return new AppricotConnection(() => new FakeTransport(), { token: TOKEN, reconnect });
}

function helloReplyBytes(
  opts: {
    version?: number | undefined;
    resumeSerial?: number | undefined;
    resumed?: boolean | undefined;
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
    expect(helloEnvelope.hello.clientName).toBe('@appricot/client');
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
    expect(conn.status).toBe('closed');

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

    conn.connect();
    let expected = 1;
    for (const wait of waits) {
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
