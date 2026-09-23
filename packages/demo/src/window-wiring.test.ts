// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { beforeAll, describe, expect, it, vi } from 'vitest';

import type { AppricotConnection } from '@appricot/client';

/**
 * Main-wiring tests for one window's behaviour: the real `src/main.ts` on the real page DOM
 * and the real `SurfaceRegistry`, with only the canvas-bound seams of `@appricot/client`
 * swapped out (as in minimize-wiring.test.ts). Covered here:
 *
 * - keyboard focus (C11): the canvas is focusable and takes DOM focus when the user focuses
 *   its window, while a session-driven focus change never pulls DOM focus out of a host field;
 * - ResizeAsk (C1): answered with a Configure, clamped to the desktop;
 * - a serial-0 ConfigureAck (the app resized itself) sizes the chrome, for a popup too;
 * - a title change while minimized reaches the restore strip;
 * - one pointer gesture sends one Configure, however it ends;
 * - the popup layer sits outside the clipping content box.
 */

const HOSTILE_TITLE = '<img src=x onerror="alert(1)"><script>alert(2)</script> Τίτλος';
const RENAMED_HOSTILE = '<svg onload=alert(3)></svg><b>renamed</b> Νέος τίτλος';

const mocks = vi.hoisted(() => {
  const sent: unknown[] = [];

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
    sessionId: 'window-wiring-session',
    send: (e: unknown): void => {
      sent.push(e);
    },
    close: (): void => {},
  };
  return { sent, conn };
});

vi.mock('@appricot/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@appricot/client')>();
  return {
    ...actual,
    SurfaceRenderer: { attach: () => ({ detach: (): void => {} }) },
    attachInput: () => (): void => {},
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
  const url = document.createElement('span');
  url.id = 'session-url';
  const tokenField = document.createElement('input');
  tokenField.id = 'token';
  tokenField.type = 'password';
  const connectButton = document.createElement('button');
  connectButton.id = 'connect';
  const closeBtn = document.createElement('button');
  closeBtn.id = 'close';
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
  toolbar.append(url, tokenField, connectButton, closeBtn, reconnect, strip, statusText);
  toolbar.append(sessionIdText, windowsCountText);
  const windows = document.createElement('main');
  windows.id = 'windows';
  document.body.append(toolbar, windows);
  return { tokenField, connectButton, windows, strip };
}

let page: Page;

function emit(envelope: unknown): void {
  mocks.conn.events.emit('message', envelope);
}

function surfaceNew(surfaceId: number, title: string, role: 0 | 1 = 0, parentId?: number): void {
  emit({
    kind: 'surfaceNew',
    surfaceNew: {
      surfaceId,
      role,
      parentId,
      size: role === 1 ? { width: 100, height: 50 } : { width: 320, height: 200 },
      title,
      appId: `app.${surfaceId}`,
      scale120ths: 120,
      positioner:
        role === 1
          ? {
              anchorRect: { x: 10, y: 10, width: 1, height: 1 },
              anchor: 5,
              gravity: 8,
              size: { width: 100, height: 50 },
            }
          : undefined,
    },
  });
}

function winRoot(surfaceId: number): HTMLElement {
  const root = page.windows.querySelector<HTMLElement>(`.window[data-surface-id="${surfaceId}"]`);
  if (root === null) {
    throw new Error(`window ${surfaceId} is not on the page`);
  }
  return root;
}

function canvasOf(surfaceId: number): HTMLCanvasElement {
  const canvas = winRoot(surfaceId).querySelector<HTMLCanvasElement>('.content > canvas.surface');
  if (canvas === null) {
    throw new Error(`window ${surfaceId} has no canvas`);
  }
  return canvas;
}

function pointer(target: Element, type: string, pointerId = 1): void {
  target.dispatchEvent(
    new PointerEvent(type, { bubbles: true, cancelable: true, button: 0, pointerId }),
  );
}

type Configure = { kind: 'configure'; configure: { surfaceId: number; serial: number; size: unknown } };

function configures(): Configure[] {
  return mocks.sent.filter(
    (e): e is Configure =>
      typeof e === 'object' && e !== null && (e as { kind?: unknown }).kind === 'configure',
  );
}

function lastFocusNotify(): number | undefined {
  const notifies = mocks.sent.filter(
    (e): e is { focusNotify: { surfaceId: number } } =>
      typeof e === 'object' && e !== null && (e as { kind?: unknown }).kind === 'focusNotify',
  );
  return notifies.at(-1)?.focusNotify.surfaceId;
}

beforeAll(async () => {
  // jsdom implements neither pointer capture nor layout.
  Element.prototype.setPointerCapture = function setPointerCapture(): void {};
  page = buildPage();
  Object.defineProperty(page.windows, 'clientWidth', { value: 1000, configurable: true });
  Object.defineProperty(page.windows, 'clientHeight', { value: 700, configurable: true });
  page.tokenField.value = 'window-wiring-token';
  await import('./main');
  page.connectButton.click();
});

// --- keyboard focus (C11) -------------------------------------------------------------------------

describe('keyboard focus follows the focused window (C11)', () => {
  it('makes the canvas focusable and gives a new window DOM focus when nothing else holds it', () => {
    surfaceNew(1, 'notes');
    expect(canvasOf(1).tabIndex).toBe(0);
    expect(document.activeElement).toBe(canvasOf(1));
    expect(lastFocusNotify()).toBe(1);
  });

  it('never pulls DOM focus out of a host field for a session-driven focus change', () => {
    page.tokenField.focus();
    expect(document.activeElement).toBe(page.tokenField);
    // A new window and a focus-ask both come from the session.
    surfaceNew(2, HOSTILE_TITLE);
    emit({ kind: 'focusAsk', focusAsk: { surfaceId: 1 } });
    expect(document.activeElement).toBe(page.tokenField);
  });

  it('moves DOM focus to the canvas the user clicks', () => {
    pointer(canvasOf(2), 'pointerdown');
    expect(document.activeElement).toBe(canvasOf(2));
    expect(winRoot(2).classList.contains('focused')).toBe(true);
    expect(lastFocusNotify()).toBe(2);
  });

  it('moves DOM focus to the window canvas when the user grabs the title bar', () => {
    const titlebar = winRoot(1).querySelector('.titlebar');
    if (titlebar === null) throw new Error('no title bar');
    pointer(titlebar, 'pointerdown');
    pointer(titlebar, 'pointerup');
    expect(document.activeElement).toBe(canvasOf(1));
    // The mousedown that follows must not hand focus to the body.
    const down = new MouseEvent('mousedown', { bubbles: true, cancelable: true });
    titlebar.dispatchEvent(down);
    expect(down.defaultPrevented).toBe(true);
    // The window controls keep their default.
    const onClose = new MouseEvent('mousedown', { bubbles: true, cancelable: true });
    winRoot(1).querySelector('.close')?.dispatchEvent(onClose);
    expect(onClose.defaultPrevented).toBe(false);
  });

  it('gives a clicked popup its own DOM focus and its parent the window focus', () => {
    surfaceNew(11, 'menu', 1, 1);
    const popupCanvas = winRoot(1).querySelector<HTMLCanvasElement>('.popup > canvas.surface');
    if (popupCanvas === null) throw new Error('no popup canvas');
    expect(popupCanvas.tabIndex).toBe(0);
    pointer(canvasOf(2), 'pointerdown'); // focus elsewhere first
    pointer(popupCanvas, 'pointerdown');
    expect(document.activeElement).toBe(popupCanvas);
    expect(winRoot(1).classList.contains('focused')).toBe(true);
    expect(lastFocusNotify()).toBe(1);
  });

  it('focuses the window whose canvas gets DOM focus some other way (Tab)', () => {
    canvasOf(2).focus();
    expect(winRoot(2).classList.contains('focused')).toBe(true);
    expect(lastFocusNotify()).toBe(2);
  });
});

// --- ResizeAsk and ConfigureAck (C1) -----------------------------------------------------------------

describe('size: ResizeAsk is honoured and every ack sizes the chrome (C1)', () => {
  it('answers a ResizeAsk with a Configure of the asked size', () => {
    const before = configures().length;
    emit({ kind: 'resizeAsk', resizeAsk: { surfaceId: 1, size: { width: 500, height: 400 } } });
    const sent = configures().slice(before);
    expect(sent).toHaveLength(1);
    expect(sent[0]?.configure.surfaceId).toBe(1);
    expect(sent[0]?.configure.size).toEqual({ width: 500, height: 400 });
  });

  it('clamps the grant to the desktop (the viewport less the title bar), with rising serials', () => {
    const before = configures();
    emit({ kind: 'resizeAsk', resizeAsk: { surfaceId: 1, size: { width: 5000, height: 5000 } } });
    const sent = configures().slice(before.length);
    expect(sent[0]?.configure.size).toEqual({ width: 1000, height: 672 });
    expect(sent[0]?.configure.serial).toBe((before.at(-1)?.configure.serial ?? 0) + 1);
  });

  it('ignores an ask for an unknown surface, for a popup, or for no area', () => {
    const before = configures().length;
    emit({ kind: 'resizeAsk', resizeAsk: { surfaceId: 99, size: { width: 500, height: 400 } } });
    emit({ kind: 'resizeAsk', resizeAsk: { surfaceId: 11, size: { width: 500, height: 400 } } });
    emit({ kind: 'resizeAsk', resizeAsk: { surfaceId: 1, size: { width: 0, height: 0 } } });
    expect(configures()).toHaveLength(before);
  });

  it('sizes the chrome to a serial-0 ack: the app resized itself', () => {
    emit({
      kind: 'configureAck',
      configureAck: { surfaceId: 1, serial: 0, size: { width: 480, height: 360 } },
    });
    expect(canvasOf(1).style.width).toBe('480px');
    expect(canvasOf(1).style.height).toBe('360px');
  });

  it('re-places a popup at the size a serial-0 ack reports', () => {
    const popup = winRoot(1).querySelector<HTMLElement>('.popup');
    expect(popup?.style.width).toBe('100px');
    emit({
      kind: 'configureAck',
      configureAck: { surfaceId: 11, serial: 0, size: { width: 160, height: 90 } },
    });
    expect(popup?.style.width).toBe('160px');
    expect(popup?.style.height).toBe('90px');
  });
});

// --- metadata while minimized ----------------------------------------------------------------------

describe('a title change while minimized', () => {
  it('reaches the restore strip and the title bar, as text', () => {
    winRoot(2).querySelector<HTMLButtonElement>('.minimize')?.click();
    const label = (): Element | undefined => page.strip.children[0];
    expect(label()?.textContent).toBe(HOSTILE_TITLE);

    emit({ kind: 'surfaceMetadata', surfaceMetadata: { surfaceId: 2, title: RENAMED_HOSTILE } });
    expect(label()?.textContent).toBe(RENAMED_HOSTILE);
    expect(label()?.childElementCount).toBe(0);
    expect(label()?.firstChild?.nodeType).toBe(Node.TEXT_NODE);
    const titleText = winRoot(2).querySelector('.titlebar .title');
    expect(titleText?.textContent).toBe(RENAMED_HOSTILE);
    expect(titleText?.childElementCount).toBe(0);
  });
});

// --- pointer gestures ----------------------------------------------------------------------------------

describe('a resize gesture', () => {
  it('sends one Configure however it ends, and leaves no listener behind', () => {
    const grip = winRoot(1).querySelector('.resize-grip');
    if (grip === null) throw new Error('no grip');
    const before = configures().length;
    for (let gesture = 0; gesture < 3; gesture += 1) {
      pointer(grip, 'pointerdown');
      pointer(grip, 'pointerup');
    }
    expect(configures()).toHaveLength(before + 3);
    // A stray cancel (or up) after the gestures ended fires nothing that was left over.
    pointer(grip, 'pointercancel');
    pointer(grip, 'pointerup');
    expect(configures()).toHaveLength(before + 3);
    // A gesture that ends in a cancel sends its one Configure too.
    pointer(grip, 'pointerdown');
    pointer(grip, 'pointercancel');
    pointer(grip, 'pointerup');
    expect(configures()).toHaveLength(before + 4);
  });
});

// --- the popup layer --------------------------------------------------------------------------------------

describe('the popup layer', () => {
  it('sits beside the clipping content box, not inside it', () => {
    const layer = winRoot(1).querySelector('.popup-layer');
    const content = winRoot(1).querySelector('.content');
    expect(layer).not.toBeNull();
    expect(content?.contains(layer ?? null)).toBe(false);
    expect(layer?.parentElement?.classList.contains('content-frame')).toBe(true);
  });

  it('is styled to show its margin band, below the host chrome', () => {
    const testPath = expect.getState().testPath;
    if (testPath === undefined) {
      throw new Error('vitest did not report this test file path');
    }
    const css = readFileSync(join(dirname(testPath), '..', 'styles.css'), 'utf8');
    const rule = (selector: string): string => {
      const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      const match = new RegExp(`(?:^|\\n)${escaped}\\s*\\{([^}]*)\\}`).exec(css);
      if (match?.[1] === undefined) {
        throw new Error(`no rule for ${selector}`);
      }
      return match[1];
    };
    // The frame the layer hangs off clips nothing; the content box still clips the canvas.
    expect(rule('.window .content-frame')).not.toMatch(/overflow/);
    expect(rule('.window .content')).toMatch(/overflow:\s*hidden/);
    // The layer clips at its own edge, and the host chrome stacks above it.
    expect(rule('.window .popup-layer')).toMatch(/overflow:\s*hidden/);
    expect(rule('.window .popup-layer')).toMatch(/z-index:\s*1/);
    expect(rule('.window .titlebar')).toMatch(/z-index:\s*2/);
    expect(rule('.window .resize-grip')).toMatch(/z-index:\s*2/);
  });
});
