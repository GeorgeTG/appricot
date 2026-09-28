/**
 * The fake connection behaves as the real one where the provider and a host can see it: its
 * statuses are the SDK's, and closing reaches 'closed' and then `ended`, once. The real
 * AppricotConnection runs next to the fake over an in-memory transport, so a change to the
 * real lifecycle turns this test red instead of leaving the fake behind.
 */
import { describe, expect, it, vi } from 'vitest';

import { clientMock, mockClientModule } from './client-mock.js';
import type { FakeConnection, FakeConnectionStatus } from './client-mock.js';
import type { ConnectionStatus } from './client.js';

type Client = typeof import('@app-ricot/client');

/** The status and ended events a connection emits, in order. */
function watch(events: {
  on(event: string, listener: (value: unknown) => void): () => void;
}): Array<[string, unknown]> {
  const seen: Array<[string, unknown]> = [];
  events.on('status', (value) => seen.push(['status', value]));
  events.on('ended', (value) => seen.push(['ended', value]));
  return seen;
}

/** A transport that opens nothing and closes once, as the Transport contract asks. */
function quietTransport() {
  let onClose: ((code: number) => void) | undefined;
  return {
    send: () => undefined,
    close: () => onClose?.(1000),
    onMessage: () => undefined,
    onClose: (cb: (code: number) => void) => {
      onClose = cb;
    },
    onOpen: () => undefined,
  };
}

describe('the fake connection', () => {
  it("knows every status the SDK's connection has, 'reconnecting' included", () => {
    const every: Record<ConnectionStatus, true> = {
      idle: true,
      connecting: true,
      open: true,
      reconnecting: true,
      closed: true,
    };
    const fake: FakeConnectionStatus[] = Object.keys(every) as ConnectionStatus[];
    expect(fake).toHaveLength(5);
  });

  it('closes like the real connection: the closed status, then ended, and only once', async () => {
    const { AppricotConnection } = await vi.importActual<Client>('@app-ricot/client');
    const real = new AppricotConnection(quietTransport, { token: new Uint8Array([1]) });
    real.connect();
    const realSeen = watch(real.events as unknown as FakeConnection['events']);
    real.close();
    real.close();

    mockClientModule();
    const fake = clientMock().connect() as FakeConnection;
    const fakeSeen = watch(fake.events);
    fake.close();
    fake.close();

    expect(realSeen).toEqual([
      ['status', 'closed'],
      ['ended', { cause: 'user' }],
    ]);
    expect(fakeSeen).toEqual(realSeen);
    expect(fake.status).toBe(real.status);
  });

  it('ends from the server side with the reason given, after the closed status', () => {
    mockClientModule();
    const m = clientMock();
    const fake = m.connect() as FakeConnection;
    const seen = watch(fake.events);
    m.setStatus('open');
    m.setStatus('reconnecting');
    m.end({ cause: 'grace-expired', code: 1006 });

    expect(seen).toEqual([
      ['status', 'open'],
      ['status', 'reconnecting'],
      ['status', 'closed'],
      ['ended', { cause: 'grace-expired', code: 1006 }],
    ]);
  });
});
