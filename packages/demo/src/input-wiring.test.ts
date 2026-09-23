// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { beforeAll, describe, expect, it, vi } from 'vitest';

import type { AppricotConnection, CursorImage, InputDeps } from '@appricot/client';

/**
 * Main-wiring tests for what the demo hands the SDK's input and cursor helpers: the real
 * `src/main.ts` on a hand-built page and the real `SurfaceRegistry`, with `attachInput` and
 * `drawCursor` recorded instead of run. Covered here:
 *
 * - a popup's pointer is scaled to the registry's surface size, not to its CSS box;
 * - a popup that closes while its canvas holds the keyboard hands DOM focus to its window (C11);
 * - the cursor overlay puts the image's hotspot on the pointer, and the image is drawn whole;
 * - paste is an opt-in toolbar checkbox, off by default (ADR-0003 §7).
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

  const conn = {
    events: new FakeEmitter(),
    status: 'open',
    sessionId: 'input-wiring-session',
    send: (): void => {},
    close: (): void => {},
  };
  const inputs: Array<{ element: HTMLElement; surfaceId: number; deps: unknown }> = [];
  const draws: unknown[][] = [];
  return { conn, inputs, draws };
});

vi.mock('@appricot/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@appricot/client')>();
  return {
    ...actual,
    SurfaceRenderer: { attach: () => ({ detach: (): void => {} }) },
    attachInput: (element: HTMLElement, surfaceId: number, deps: unknown) => {
      mocks.inputs.push({ element, surfaceId, deps });
      return (): void => {};
    },
    drawCursor: (...args: unknown[]): void => {
      mocks.draws.push(args);
    },
    connectAppricot: (): AppricotConnection => mocks.conn as unknown as AppricotConnection,
  };
});

// --- the page and the wire, by hand -----------------------------------------------------------

interface Page {
  tokenField: HTMLInputElement;
  connectButton: HTMLButtonElement;
  paste: HTMLInputElement;
  windows: HTMLElement;
}

function buildPage(): Page {
  const toolbar = document.createElement('header');
  const ids = ['session-url', 'minimized', 'status', 'session-id', 'windows-count'];
  for (const id of ids) {
    const span = document.createElement('span');
    span.id = id;
    toolbar.append(span);
  }
  const tokenField = document.createElement('input');
  tokenField.id = 'token';
  tokenField.type = 'password';
  const connectButton = document.createElement('button');
  connectButton.id = 'connect';
  const closeButton = document.createElement('button');
  closeButton.id = 'close';
  const reconnect = document.createElement('input');
  reconnect.id = 'reconnect';
  reconnect.type = 'checkbox';
  const paste = document.createElement('input');
  paste.id = 'paste';
  paste.type = 'checkbox';
  toolbar.append(tokenField, connectButton, closeButton, reconnect, paste);
  const windows = document.createElement('main');
  windows.id = 'windows';
  document.body.append(toolbar, windows);
  return { tokenField, connectButton, paste, windows };
}

let page: Page;

function emit(envelope: unknown): void {
  mocks.conn.events.emit('message', envelope);
}

function toplevel(surfaceId: number): void {
  emit({
    kind: 'surfaceNew',
    surfaceNew: {
      surfaceId,
      role: 0,
      size: { width: 320, height: 200 },
      title: `window ${surfaceId}`,
      appId: 'app',
      scale120ths: 120,
    },
  });
}

function popup(surfaceId: number, parentId: number, size: { width: number; height: number }): void {
  emit({
    kind: 'surfaceNew',
    surfaceNew: {
      surfaceId,
      role: 1,
      parentId,
      size,
      title: '',
      appId: 'app',
      scale120ths: 120,
      positioner: {
        anchorRect: { x: 10, y: 10, width: 1, height: 1 },
        anchor: 5,
        gravity: 8,
        size,
      },
    },
  });
}

function inputOf(surfaceId: number): { element: HTMLElement; deps: InputDeps } {
  const found = mocks.inputs.findLast((call) => call.surfaceId === surfaceId);
  if (found === undefined) {
    throw new Error(`no attachInput for surface ${surfaceId}`);
  }
  return { element: found.element, deps: found.deps as InputDeps };
}

function windowCanvas(surfaceId: number): HTMLCanvasElement {
  const canvas = page.windows.querySelector<HTMLCanvasElement>(
    `.window[data-surface-id="${surfaceId}"] .content > canvas.surface`,
  );
  if (canvas === null) {
    throw new Error(`window ${surfaceId} has no canvas`);
  }
  return canvas;
}

beforeAll(async () => {
  Element.prototype.setPointerCapture = function setPointerCapture(): void {};
  // jsdom draws nothing; the overlay only needs a context to hand to drawCursor.
  HTMLCanvasElement.prototype.getContext = function getContext() {
    return {} as CanvasRenderingContext2D;
  } as unknown as typeof HTMLCanvasElement.prototype.getContext;
  page = buildPage();
  Object.defineProperty(page.windows, 'clientWidth', { value: 1000, configurable: true });
  Object.defineProperty(page.windows, 'clientHeight', { value: 700, configurable: true });
  page.tokenField.value = 'input-wiring-token';
  await import('./main');
  page.connectButton.click();
});

describe('a popup', () => {
  it("scales its pointer to the registry's surface size, not to its CSS box", () => {
    toplevel(1);
    popup(2, 1, { width: 150, height: 90 });
    const { deps } = inputOf(2);
    // jsdom lays nothing out: the popup's CSS box is 0x0, and the old wiring answered that.
    expect(deps.size?.()).toEqual({ width: 150, height: 90 });

    // The popup resized itself (a serial-0 ack): the next position is scaled to the new size.
    emit({
      kind: 'configureAck',
      configureAck: { surfaceId: 2, serial: 0, size: { width: 200, height: 120 } },
    });
    expect(deps.size?.()).toEqual({ width: 200, height: 120 });
  });

  it('hands DOM focus back to its window when it closes while holding it (C11)', () => {
    const { element } = inputOf(2);
    element.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, button: 0 }));
    expect(document.activeElement).toBe(element);

    emit({ kind: 'surfaceGone', surfaceGone: { surfaceId: 2, reason: 0 } });
    expect(element.isConnected).toBe(false);
    expect(document.activeElement).toBe(windowCanvas(1));
  });

  it('leaves DOM focus alone when it closes without holding it', () => {
    popup(3, 1, { width: 80, height: 40 });
    const hostField = document.createElement('input');
    document.body.append(hostField);
    hostField.focus();

    emit({ kind: 'surfaceGone', surfaceGone: { surfaceId: 3, reason: 0 } });
    expect(document.activeElement).toBe(hostField);
    hostField.remove();
  });
});

describe('the cursor overlay', () => {
  it("puts the image's hotspot on the pointer and draws the whole image", () => {
    const image: CursorImage = {
      serial: 1,
      width: 16,
      height: 24,
      hotspotX: 5,
      hotspotY: 7,
      argbPremultiplied: new Uint8Array(16 * 24 * 4),
    };
    windowCanvas(1).focus();
    const frame = page.windows.querySelector('.window[data-surface-id="1"] .content-frame');
    frame?.dispatchEvent(new PointerEvent('pointermove', { clientX: 50, clientY: 40 }));
    emit({ kind: 'cursorImage', cursorImage: image });

    const overlay = page.windows.querySelector<HTMLCanvasElement>(
      '.window[data-surface-id="1"] canvas.cursor-overlay',
    );
    // jsdom's canvas box is at (0, 0): the pointer is (50, 40), the hotspot (5, 7).
    expect(overlay?.style.left).toBe('45px');
    expect(overlay?.style.top).toBe('33px');
    expect(overlay?.width).toBe(16);
    expect(overlay?.height).toBe(24);
    // Drawn at the overlay's origin: no coordinates, so nothing is cropped.
    const last = mocks.draws.at(-1);
    expect(last).toHaveLength(2);
    expect(last?.[1]).toEqual(image);
  });
});

describe('paste (ADR-0003 §7)', () => {
  it('passes a paste policy that allows nothing until the user opts in', () => {
    const { deps } = inputOf(1);
    expect(deps.paste).toBeTypeOf('function');
    expect(page.paste.checked).toBe(false);
    expect(deps.paste?.('text from the host')).toBe(false);

    page.paste.checked = true;
    expect(deps.paste?.('text from the host')).toBe(true);
    page.paste.checked = false;
    expect(deps.paste?.('text from the host')).toBe(false);
  });

  it('ships the opt-in unchecked, labelled as sending the text to the app', () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const html = readFileSync(join(here, '..', 'index.html'), 'utf8');
    const doc = new DOMParser().parseFromString(html, 'text/html');
    const box = doc.getElementById('paste');
    expect(box?.tagName).toBe('INPUT');
    expect(box?.getAttribute('type')).toBe('checkbox');
    expect(box?.hasAttribute('checked')).toBe(false);
    expect(box?.closest('label')?.textContent).toMatch(/pasted text to the app/);
  });
});
