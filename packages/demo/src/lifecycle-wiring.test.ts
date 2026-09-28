// @vitest-environment jsdom
import { beforeAll, describe, expect, it, vi } from 'vitest';

import type { AppricotConnection } from '@app-ricot/client';

/**
 * Main-wiring tests for the connection lifecycle: the real `src/main.ts` on the real page DOM
 * and the real `SurfaceRegistry`, with a FRESH fake connection for every connectAppricot()
 * call, so a stale connection can be told from the live one.
 *
 * The scenario runs in order, one step per test: a session with a window; its socket drops
 * with Reconnect checked (the SDK says 'reconnecting' during its backoff); the SDK resumes and
 * the server re-announces the window; it drops again and the user clicks Connect during the
 * backoff; the old connection keeps emitting; a fatal Bye ('closed', then `ended`); an `ended`
 * alone; and Close.
 */

const mocks = vi.hoisted(() => {
  class FakeEmitter {
    readonly #handlers = new Map<string, Array<(e: unknown) => void>>();
    on(kind: string, fn: (e: unknown) => void): () => void {
      const list = this.#handlers.get(kind) ?? [];
      list.push(fn);
      this.#handlers.set(kind, list);
      return () => {
        const current = this.#handlers.get(kind);
        const at = current?.indexOf(fn) ?? -1;
        if (at >= 0) {
          current?.splice(at, 1);
        }
      };
    }
    emit(kind: string, e: unknown): void {
      for (const fn of [...(this.#handlers.get(kind) ?? [])]) {
        fn(e);
      }
    }
  }

  interface FakeConn {
    events: FakeEmitter;
    status: string;
    sessionId: string | undefined;
    sent: unknown[];
    closed: number;
    send(e: unknown): void;
    close(): void;
  }

  const conns: FakeConn[] = [];
  const renderers: Array<{ surfaceId: number; conn: unknown; detached: boolean }> = [];

  function makeConn(): FakeConn {
    const conn: FakeConn = {
      events: new FakeEmitter(),
      status: 'connecting',
      sessionId: undefined,
      sent: [],
      closed: 0,
      send(e: unknown): void {
        if (conn.status === 'open') {
          conn.sent.push(e);
        }
      },
      close(): void {
        conn.closed += 1;
      },
    };
    conns.push(conn);
    return conn;
  }

  return { conns, renderers, makeConn };
});

vi.mock('@app-ricot/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@app-ricot/client')>();
  return {
    ...actual,
    SurfaceRenderer: {
      attach: (_canvas: HTMLCanvasElement, surfaceId: number, deps: { conn: unknown }) => {
        const handle = { surfaceId, conn: deps.conn, detached: false };
        mocks.renderers.push(handle);
        return {
          detach: (): void => {
            handle.detached = true;
          },
        };
      },
    },
    attachInput: () => (): void => {},
    connectAppricot: (): AppricotConnection => mocks.makeConn() as unknown as AppricotConnection,
  };
});

// --- the page, by hand ----------------------------------------------------------------------------

interface Page {
  tokenField: HTMLInputElement;
  connectButton: HTMLButtonElement;
  closeButton: HTMLButtonElement;
  reconnect: HTMLInputElement;
  status: HTMLElement;
  windows: HTMLElement;
}

function buildPage(): Page {
  const toolbar = document.createElement('header');
  const url = document.createElement('span');
  url.id = 'session-url';
  const tokenField = document.createElement('input');
  tokenField.id = 'token';
  const connectButton = document.createElement('button');
  connectButton.id = 'connect';
  const closeButton = document.createElement('button');
  closeButton.id = 'close';
  closeButton.disabled = true;
  const reconnect = document.createElement('input');
  reconnect.id = 'reconnect';
  reconnect.type = 'checkbox';
  const strip = document.createElement('span');
  strip.id = 'minimized';
  const status = document.createElement('span');
  status.id = 'status';
  const sessionIdText = document.createElement('span');
  sessionIdText.id = 'session-id';
  const windowsCountText = document.createElement('span');
  windowsCountText.id = 'windows-count';
  toolbar.append(url, tokenField, connectButton, closeButton, reconnect, strip, status);
  toolbar.append(sessionIdText, windowsCountText);
  const windows = document.createElement('main');
  windows.id = 'windows';
  document.body.append(toolbar, windows);
  return { tokenField, connectButton, closeButton, reconnect, status, windows };
}

let page: Page;

type Conn = (typeof mocks.conns)[number];

function conn(index: number): Conn {
  const found = mocks.conns[index];
  if (found === undefined) {
    throw new Error(`no connection #${index}`);
  }
  return found;
}

function setStatus(target: Conn, status: string): void {
  target.status = status;
  target.events.emit('status', status);
}

function end(target: Conn, cause: string): void {
  target.status = 'closed';
  target.events.emit('status', 'closed');
  target.events.emit('ended', { cause });
}

function helloReply(target: Conn, resumed: boolean): void {
  target.events.emit('message', {
    kind: 'helloReply',
    helloReply: {
      protocolVersion: 0,
      sessionId: 'lifecycle',
      maxFrameCredits: 4,
      codecs: [1],
      resumed,
      resumeSerial: 1,
      resumeGraceMs: 10_000,
    },
  });
}

function surfaceNew(target: Conn, surfaceId: number, title: string): void {
  target.events.emit('message', {
    kind: 'surfaceNew',
    surfaceNew: {
      surfaceId,
      role: 0,
      size: { width: 320, height: 200 },
      title,
      appId: 'app',
      scale120ths: 120,
    },
  });
}

function titles(): string[] {
  return [...page.windows.querySelectorAll('.window .titlebar .title')].map(
    (node) => node.textContent ?? '',
  );
}

beforeAll(async () => {
  page = buildPage();
  page.tokenField.value = 'lifecycle-token';
  page.reconnect.checked = true;
  await import('./main');
});

describe('the demo connection lifecycle', () => {
  it('connects, and shows the first session', () => {
    page.connectButton.click();
    expect(mocks.conns).toHaveLength(1);
    expect(page.connectButton.disabled).toBe(true);
    setStatus(conn(0), 'open');
    surfaceNew(conn(0), 1, 'first session');
    expect(titles()).toEqual(['first session']);
    expect(page.status.textContent).toBe('open');
  });

  it("keeps the windows through 'reconnecting': a retry is pending, not an end", () => {
    setStatus(conn(0), 'reconnecting');
    expect(titles()).toEqual(['first session']);
    expect(mocks.renderers.find((r) => r.surfaceId === 1)?.detached).toBe(false);
    expect(page.status.textContent).toBe('reconnecting');
    // Connect starts over, and Close stops the retry.
    expect(page.connectButton.disabled).toBe(false);
    expect(page.closeButton.disabled).toBe(false);
    expect(conn(0).closed).toBe(0);
  });

  it('updates the windows in place when the SDK resumes and the server re-announces them', () => {
    const window = page.windows.querySelector('.window');
    setStatus(conn(0), 'connecting');
    expect(page.connectButton.disabled).toBe(true);
    setStatus(conn(0), 'open');
    helloReply(conn(0), true);
    surfaceNew(conn(0), 1, 'first session, resumed');
    // The first message after the re-announcement ends it (v0 §7).
    conn(0).events.emit('message', { kind: 'cursorGone', cursorGone: {} });
    expect(titles()).toEqual(['first session, resumed']);
    // The same window, never torn down and built again.
    expect(page.windows.querySelector('.window')).toBe(window);
    expect(mocks.renderers.filter((r) => r.surfaceId === 1)).toHaveLength(1);
  });

  it('closes the old connection for good when the user clicks Connect during a backoff', () => {
    setStatus(conn(0), 'reconnecting');
    page.connectButton.click();
    expect(mocks.conns).toHaveLength(2);
    // The old connection is closed, which cancels its pending reconnect.
    expect(conn(0).closed).toBe(1);
    setStatus(conn(1), 'open');
    // The new session numbers its windows from 1 too, and they appear.
    surfaceNew(conn(1), 1, 'second session');
    expect(titles()).toEqual(['second session']);
    const renderer = mocks.renderers.at(-1);
    expect(renderer?.conn).toBe(conn(1));
  });

  it('ignores everything the replaced connection still emits', () => {
    conn(0).status = 'open';
    surfaceNew(conn(0), 5, 'ghost');
    end(conn(0), 'grace-expired');
    expect(titles()).toEqual(['second session']);
    expect(page.status.textContent).toBe('open');
    expect(page.connectButton.disabled).toBe(true);
  });

  it('tears down after a fatal Bye: no frozen windows, whatever the reconnect box says', () => {
    conn(1).events.emit('message', { kind: 'bye', bye: { reason: 0x104, message: 'gone' } });
    end(conn(1), 'bye');
    expect(titles()).toEqual([]);
    expect(mocks.renderers.every((r) => r.detached)).toBe(true);
    // The SDK's own cause, as text; nothing will retry, so there is nothing to Close.
    expect(page.status.textContent).toBe('closed: bye');
    expect(page.connectButton.disabled).toBe(false);
    expect(page.closeButton.disabled).toBe(true);
  });

  it('tears down on the ended event alone', () => {
    page.connectButton.click();
    expect(mocks.conns).toHaveLength(3);
    setStatus(conn(2), 'open');
    surfaceNew(conn(2), 1, 'third session');
    expect(titles()).toEqual(['third session']);

    // The status moves without its event reaching the page: `ended` alone must tear down.
    conn(2).status = 'closed';
    conn(2).events.emit('ended', { cause: 'grace-expired' });
    expect(titles()).toEqual([]);
    expect(page.status.textContent).toBe('closed: grace-expired');
    expect(page.connectButton.disabled).toBe(false);
  });

  it('Close closes the connection, clears the page and goes idle', () => {
    page.connectButton.click();
    expect(mocks.conns).toHaveLength(4);
    expect(conn(2).closed).toBe(1);
    setStatus(conn(3), 'open');
    surfaceNew(conn(3), 1, 'fourth session');
    expect(titles()).toEqual(['fourth session']);

    page.closeButton.click();
    expect(conn(3).closed).toBe(1);
    expect(titles()).toEqual([]);
    expect(page.status.textContent).toBe('idle');
    expect(page.closeButton.disabled).toBe(true);
    expect(page.connectButton.disabled).toBe(false);
    // A late 'closed' and ended from the connection the user closed change nothing.
    end(conn(3), 'user');
    expect(page.status.textContent).toBe('idle');
  });
});
