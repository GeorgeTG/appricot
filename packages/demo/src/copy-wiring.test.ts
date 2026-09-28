// @vitest-environment jsdom
import { beforeAll, describe, expect, it, vi } from 'vitest';

import type { AppricotConnection } from '@app-ricot/client';

/**
 * Main-wiring tests for the app-to-host clipboard direction (ADR-0003 §7): the real
 * `src/main.ts` on a real page DOM with the real `SurfaceRegistry`, only the canvas seams
 * of `@app-ricot/client` swapped out (as in minimize-wiring.test.ts). The browser's
 * `navigator.clipboard` is jsdom's — absent — so the test installs a recording stub before
 * the module boots.
 *
 * What is proved here, in the demo's own chrome:
 * - a `clipboardText` envelope holds its untrusted text as data: the button arms, the
 *   status line reports a count in the host's own words, and the text itself is rendered
 *   nowhere in the DOM;
 * - the one clipboard write is the button's click: nothing is written before it, and the
 *   write receives exactly the held string, hostile markup included, as data;
 * - a write the browser refuses is reported, not thrown;
 * - the session's end disarms the button and forgets the text.
 */

/** The same hostile corpus shape as the other wiring tests: markup in every spelling. */
const HOSTILE_COPY =
  '<img src=x onerror="fetch(`/leak?c=${document.cookie}`)"><script>alert(1)</script>' +
  '<svg onload=alert(2)"></svg>"onmouseover="alert(4)';

const writes: string[] = [];

/** Lets one test make the next write fail, then restores the resolving stub. */
const clipboardStub = vi.hoisted(() => {
  let failNext = false;
  return {
    writeText: (text: string): Promise<void> => {
      writes.push(text);
      if (failNext) {
        failNext = false;
        return Promise.reject(new Error('refused'));
      }
      return Promise.resolve();
    },
    failNextWrite: (): void => {
      failNext = true;
    },
  };
});

const mocks = vi.hoisted(() => {
  /** The fake connection's event bus: `on` returns its own unsubscribe, like the real one. */
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
      for (const fn of [...this.#handlers.get(kind) ?? []]) {
        fn(e);
      }
    }
  }

  const conn = {
    events: new FakeEmitter(),
    status: 'open',
    sessionId: 'copy-wiring-session',
    send: (): void => {},
    close: (): void => {},
  };
  return { conn };
});

vi.mock('@app-ricot/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@app-ricot/client')>();
  return {
    ...actual,
    SurfaceRenderer: {
      attach: () => ({ detach: (): void => {} }),
    },
    attachInput: () => (): void => {},
    connectAppricot: (): AppricotConnection => mocks.conn as unknown as AppricotConnection,
  };
});

// --- the page and the wire, by hand -----------------------------------------------------------

interface Page {
  connectButton: HTMLButtonElement;
  closeButton: HTMLButtonElement;
  copyButton: HTMLButtonElement;
  statusText: HTMLElement;
  windows: HTMLElement;
}

function buildPage(): Page {
  const toolbar = document.createElement('header');
  const url = document.createElement('span');
  url.id = 'session-url';
  const tokenField = document.createElement('input');
  tokenField.id = 'token';
  tokenField.type = 'password';
  const connectButton = document.createElement('button');
  connectButton.id = 'connect';
  connectButton.type = 'button';
  const closeButton = document.createElement('button');
  closeButton.id = 'close';
  closeButton.type = 'button';
  closeButton.disabled = true;
  const reconnect = document.createElement('input');
  reconnect.id = 'reconnect';
  reconnect.type = 'checkbox';
  const strip = document.createElement('span');
  strip.id = 'minimized';
  const statusText = document.createElement('span');
  statusText.id = 'status';
  const sessionIdText = document.createElement('span');
  sessionIdText.id = 'session-id';
  const windowsCountText = document.createElement('span');
  windowsCountText.id = 'windows-count';
  // The button under test: disabled until the app copies, like the page ships it.
  const copyButton = document.createElement('button');
  copyButton.id = 'copy-app-text';
  copyButton.type = 'button';
  copyButton.disabled = true;
  toolbar.append(url, tokenField, connectButton, closeButton, reconnect, copyButton, strip);
  toolbar.append(statusText, sessionIdText, windowsCountText);
  const windows = document.createElement('main');
  windows.id = 'windows';
  document.body.append(toolbar, windows);
  return { connectButton, closeButton, copyButton, statusText, windows };
}

let page: Page;

/** Feeds one envelope to the page the way the connection does. */
function emit(envelope: unknown): void {
  mocks.conn.events.emit('message', envelope);
}

function appCopied(text: string): void {
  emit({ kind: 'clipboardText', clipboardText: { text } });
}

beforeAll(async () => {
  page = buildPage();
  // jsdom ships no navigator.clipboard; the page checks for it at click time.
  Object.defineProperty(window.navigator, 'clipboard', {
    value: clipboardStub,
    configurable: true,
  });
  const tokenField = document.getElementById('token');
  if (tokenField instanceof HTMLInputElement) {
    tokenField.value = 'copy-wiring-token';
  }
  await import('./main');
  page.connectButton.click();
});

// --- the held copy is data, never markup ------------------------------------------------------

describe('the app-to-host copy: held as data, written only by the gesture', () => {
  it('disarms and writes nothing before the app has copied anything', () => {
    expect(page.copyButton.disabled).toBe(true);
    page.copyButton.click();
    expect(writes).toHaveLength(0);
  });

  it('arms the button on the event and renders the untrusted text nowhere', () => {
    appCopied(HOSTILE_COPY);
    expect(page.copyButton.disabled).toBe(false);
    // The host's own words on the status line — a count, not the text.
    expect(page.statusText.textContent).toBe(`app copied ${HOSTILE_COPY.length} characters`);
    // And the text itself is in no text node of the page.
    expect(document.body.textContent?.includes('<img')).toBe(false);
    expect(document.body.textContent?.includes('onerror')).toBe(false);
    expect(document.body.textContent?.includes(HOSTILE_COPY)).toBe(false);
  });

  it('writes the held string, exactly and only, from the click gesture', async () => {
    page.copyButton.click();
    expect(writes).toEqual([HOSTILE_COPY]);
    // The success report is the status line's own words.
    await vi.waitFor(() => {
      expect(page.statusText.textContent).toBe('copied to your clipboard');
    });
  });

  it('reports a refused write instead of throwing', async () => {
    clipboardStub.failNextWrite();
    page.copyButton.click();
    await vi.waitFor(() => {
      expect(page.statusText.textContent).toBe('clipboard write refused');
    });
  });

  it('forgets the held copy when the session ends', () => {
    page.closeButton.click();
    expect(page.copyButton.disabled).toBe(true);
    const before = writes.length;
    page.copyButton.click();
    expect(writes).toHaveLength(before);
  });
});
