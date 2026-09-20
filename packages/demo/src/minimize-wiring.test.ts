// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { beforeAll, describe, expect, it, vi } from 'vitest';

import type { AppricotConnection } from '@appricot/client';

/**
 * The one main-wiring test: the real `src/main.ts` booting against the real page DOM and the
 * real `SurfaceRegistry`, with only the canvas-bound seams of `@appricot/client` swapped out
 * (`SurfaceRenderer.attach` — jsdom has no canvas rasteriser — and `attachInput`), plus
 * `connectAppricot` for a fake connection whose messages the test emits by hand.
 *
 * Everything else is the shipped wiring: surfaceNew envelopes flow through the registry into
 * buildWindow/buildPopup, the minimize button hides a window by class, focus moves as the wm
 * decided (FocusNotify, or BlurRelease when no window is left), the toolbar's restore strip
 * renders minimized windows' titles through the real `setTextOnly`, and no minimize or
 * restore ever detaches a renderer or an input — the canvas the renderer owns stays in the
 * DOM the whole time, which is the difference between hiding a window and killing its
 * stream.
 */

/** The same hostile title as hostile-title.test.ts: markup in every shape a server can send. */
const HOSTILE_TITLE =
  '<img src=x onerror="fetch(`/leak?c=${document.cookie}`)"><script>alert(1)</script>' +
  '<svg onload=alert(2)></svg><iframe srcdoc="<script>alert(3)</script>">"onmouseover="alert(4)';

/** One recorded `SurfaceRenderer.attach` call, as the assertions read it: the surface and
 * whether detach was ever called (the test's stand-in for "the renderer still has its
 * effects on the canvas" — the wiring must never call it for a hide). */
interface RendererHandle {
  surfaceId: number;
  detached: boolean;
}

const mocks = vi.hoisted(() => {
  const renderers: Array<{ surfaceId: number; canvas: unknown; detached: boolean }> = [];
  const inputs: Array<{ surfaceId: number; detached: boolean }> = [];
  const sent: unknown[] = [];

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
      for (const fn of [...(this.#handlers.get(kind) ?? [])]) {
        fn(e);
      }
    }
  }

  const conn = {
    events: new FakeEmitter(),
    status: 'open',
    sessionId: 'wiring-test-session',
    send: (e: unknown): void => {
      sent.push(e);
    },
    close: (): void => {},
  };
  return { renderers, inputs, sent, conn };
});

vi.mock('@appricot/client', async (importOriginal) => {
  // Everything real except the three canvas/transport seams; setTextOnly, SurfaceRegistry,
  // the geometry and the limits stay the shipped code.
  const actual = await importOriginal<typeof import('@appricot/client')>();
  return {
    ...actual,
    SurfaceRenderer: {
      attach: (canvas: HTMLCanvasElement, surfaceId: number) => {
        const handle = { surfaceId, canvas, detached: false };
        mocks.renderers.push(handle);
        return {
          detach: (): void => {
            handle.detached = true;
          },
        };
      },
    },
    attachInput: (_canvas: HTMLCanvasElement, surfaceId: number) => {
      const handle = { surfaceId, detached: false };
      mocks.inputs.push(handle);
      return (): void => {
        handle.detached = true;
      };
    },
    connectAppricot: (): AppricotConnection => mocks.conn as unknown as AppricotConnection,
  };
});

// --- the page and the wire, by hand -----------------------------------------------------------

interface Page {
  tokenField: HTMLInputElement;
  connectButton: HTMLButtonElement;
  windows: HTMLElement;
  strip: HTMLElement;
}

function buildPage(): Page {
  const toolbar = document.createElement('header');
  const brand = document.createElement('span');
  brand.className = 'brand';
  const url = document.createElement('span');
  url.className = 'url';
  url.id = 'session-url';
  const field = document.createElement('label');
  field.className = 'field';
  field.append('Token');
  const tokenField = document.createElement('input');
  tokenField.id = 'token';
  tokenField.type = 'password';
  field.append(tokenField);
  const connectButton = document.createElement('button');
  connectButton.id = 'connect';
  connectButton.type = 'button';
  const closeBtn = document.createElement('button');
  closeBtn.id = 'close';
  closeBtn.type = 'button';
  closeBtn.disabled = true;
  const toggle = document.createElement('label');
  toggle.className = 'field toggle';
  const reconnect = document.createElement('input');
  reconnect.id = 'reconnect';
  reconnect.type = 'checkbox';
  toggle.append(reconnect, 'Reconnect');
  const strip = document.createElement('span');
  strip.id = 'minimized';
  strip.className = 'minimized-strip';
  const status = document.createElement('span');
  status.className = 'status';
  const statusText = document.createElement('span');
  statusText.id = 'status';
  const sessionIdText = document.createElement('span');
  sessionIdText.id = 'session-id';
  const windowsCountText = document.createElement('span');
  windowsCountText.id = 'windows-count';
  status.append(statusText, sessionIdText, windowsCountText);
  toolbar.append(brand, url, field, connectButton, closeBtn, toggle, strip, status);
  const windows = document.createElement('main');
  windows.id = 'windows';
  document.body.append(toolbar, windows);
  return { tokenField, connectButton, windows, strip };
}

let page: Page;

function surfaceNew(surfaceId: number, title: string, role: 0 | 1, parentId?: number): void {
  mocks.conn.events.emit('message', {
    kind: 'surfaceNew',
    surfaceNew: {
      surfaceId,
      role,
      parentId,
      size: { width: 320, height: 200 },
      title,
      appId: `app.${surfaceId}`,
      scale120ths: 120,
    },
  });
}

function surfaceGone(surfaceId: number): void {
  mocks.conn.events.emit('message', {
    kind: 'surfaceGone',
    surfaceGone: { surfaceId, reason: 0 },
  });
}

function winRoot(surfaceId: number): HTMLElement | null {
  return page.windows.querySelector<HTMLElement>(`.window[data-surface-id="${surfaceId}"]`);
}

function rendererOf(surfaceId: number): RendererHandle | undefined {
  return mocks.renderers.find((r) => r.surfaceId === surfaceId);
}

function lastFocusNotify(): number | undefined {
  const notifies = mocks.sent.filter(
    (e): e is { focusNotify: { surfaceId: number } } =>
      typeof e === 'object' && e !== null && (e as { kind?: unknown }).kind === 'focusNotify',
  );
  return notifies.at(-1)?.focusNotify.surfaceId;
}

function blurReleases(): number {
  return mocks.sent.filter(
    (e) => typeof e === 'object' && e !== null && (e as { kind?: unknown }).kind === 'blurRelease',
  ).length;
}

function stripButtons(): HTMLButtonElement[] {
  return [...page.strip.children].filter(
    (c): c is HTMLButtonElement => c instanceof HTMLButtonElement,
  );
}

// --- the scenario ------------------------------------------------------------------------------

beforeAll(async () => {
  page = buildPage();
  page.tokenField.value = 'wiring-test-token';
  await import('./main');
  page.connectButton.click();
  // Two toplevels — the second carries the hostile title — and one popup on the first.
  surfaceNew(1, 'notes', 0);
  surfaceNew(2, HOSTILE_TITLE, 0);
  surfaceNew(11, 'menu', 1, 1);
});

describe('the demo wiring: minimize and restore', () => {
  it('boots two windows and a popup, focus on the newest', () => {
    expect(winRoot(1)).not.toBeNull();
    expect(winRoot(2)).not.toBeNull();
    expect(winRoot(1)?.classList.contains('focused')).toBe(false);
    expect(winRoot(2)?.classList.contains('focused')).toBe(true);
    expect(winRoot(1)?.querySelectorAll('.popup')).toHaveLength(1);
    expect(lastFocusNotify()).toBe(2);
    expect(rendererOf(2)?.detached).toBe(false);
  });

  it('minimizing the focused window hides it, moves focus down, and detaches nothing', () => {
    const root2 = winRoot(2);
    const canvas2 = root2?.querySelector<HTMLElement>('.content > canvas.surface');
    const content2 = canvas2?.parentElement;
    root2?.querySelector<HTMLButtonElement>('.minimize')?.click();

    expect(root2?.classList.contains('minimized')).toBe(true);
    // Focus moved to the top-most remaining window, and the server was told.
    expect(root2?.classList.contains('focused')).toBe(false);
    expect(winRoot(1)?.classList.contains('focused')).toBe(true);
    expect(lastFocusNotify()).toBe(1);
    expect(blurReleases()).toBe(0);
    // The stream was not touched: canvas in place, renderer and input still attached.
    expect(canvas2?.isConnected).toBe(true);
    expect(canvas2?.parentElement).toBe(content2);
    expect(rendererOf(2)?.detached).toBe(false);
    expect(mocks.inputs.find((i) => i.surfaceId === 2)?.detached).toBe(false);
  });

  it('renders the minimized window on the strip as text, never markup', () => {
    const buttons = stripButtons();
    expect(buttons).toHaveLength(1);
    const label = buttons[0];
    expect(label?.childElementCount).toBe(0);
    expect(label?.childNodes.length).toBe(1);
    expect(label?.firstChild?.nodeType).toBe(Node.TEXT_NODE);
    expect(label?.textContent).toBe(HOSTILE_TITLE);
  });

  it('minimizing the last visible window sends BlurRelease and hides the popup with its parent', () => {
    const root1 = winRoot(1);
    const popup = root1?.querySelector('.popup');
    root1?.querySelector<HTMLButtonElement>('.minimize')?.click();

    expect(root1?.classList.contains('minimized')).toBe(true);
    expect(blurReleases()).toBe(1);
    // No window is focused any more.
    expect(page.windows.querySelector('.window.focused')).toBeNull();
    // The popup is hidden with its parent by construction: it lives inside the window root,
    // which display:none now hides (styles.css: .window.minimized { display: none }).
    expect(popup?.isConnected).toBe(true);
    expect(popup?.closest('.window.minimized')).toBe(root1);
    // The strip lists both, in minimize order, hostile title first.
    expect(stripButtons().map((b) => b.textContent)).toEqual([HOSTILE_TITLE, 'notes']);
  });

  it('restoring from the strip re-shows, raises and focuses the window', () => {
    const root2 = winRoot(2);
    const canvas2 = root2?.querySelector<HTMLElement>('.content > canvas.surface');
    const content2 = canvas2?.parentElement;
    stripButtons()[0]?.click(); // the first strip button is the hostile-titled window

    expect(root2?.classList.contains('minimized')).toBe(false);
    expect(root2?.classList.contains('focused')).toBe(true);
    expect(lastFocusNotify()).toBe(2);
    expect(stripButtons().map((b) => b.textContent)).toEqual(['notes']);
    // The round-trip kept the stream: same canvas, same parent, renderer never detached.
    expect(canvas2?.isConnected).toBe(true);
    expect(canvas2?.parentElement).toBe(content2);
    expect(rendererOf(2)?.detached).toBe(false);
    expect(mocks.inputs.find((i) => i.surfaceId === 2)?.detached).toBe(false);
  });

  it('a window the server closes while minimized leaves the strip and takes its popups', () => {
    surfaceGone(1);

    expect(winRoot(1)).toBeNull();
    expect(page.windows.querySelector('.popup')).toBeNull();
    expect(stripButtons()).toHaveLength(0);
    expect(rendererOf(1)?.detached).toBe(true);
    expect(rendererOf(11)?.detached).toBe(true);
    // The surviving window was refocused and its stream is still whole.
    expect(lastFocusNotify()).toBe(2);
    expect(winRoot(2)?.classList.contains('focused')).toBe(true);
    expect(rendererOf(2)?.detached).toBe(false);
  });

  it('hides minimized windows with a CSS class, and the rule exists in the stylesheet', () => {
    // Under the jsdom environment `import.meta.url` is not a file URL, so the stylesheet is
    // found from the path vitest itself reports for this file.
    const testPath = expect.getState().testPath;
    if (testPath === undefined) {
      throw new Error('vitest did not report this test file path');
    }
    const css = readFileSync(join(dirname(testPath), '..', 'styles.css'), 'utf8');
    expect(/\.window\.minimized\s*\{[^}]*display:\s*none/.test(css)).toBe(true);
  });
});
