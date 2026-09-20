/**
 * The demo host page's DOM wiring (the browser entry, `/dist/main.js`).
 *
 * This is a host UI of its own: a toolbar (same-origin session URL shown as text, a token
 * INPUT field that is the token's only source — never the URL or its query — connect/close
 * buttons, a status line, a reconnect toggle, and a strip of restore buttons for the
 * minimized windows, each titled with the window's own — untrusted — title through
 * `setTextOnly`) and, per streamed toplevel, one floating window of demo chrome: a title bar
 * whose title and app id go in through `setTextOnly` (untrusted text, ADR-0003 §1),
 * drag-to-move, a close button that sends CloseRequest, a minimize button that hides the
 * window without touching the session (the canvas, the renderer and the input stay attached;
 * a minimized parent hides its popups with it, since the popup layer is inside the window),
 * a canvas the SDK's SurfaceRenderer attaches to, click-to-focus (FocusNotify plus a visual
 * focus ring), a resize grip that sends Configure on release and sizes the chrome to the
 * ConfigureAck, an absolutely-positioned popup layer clamped to the parent (ADR-0003 §5),
 * and a cursor overlay drawn from the wire's cursor pixels.
 *
 * All decisions live in the pure modules under src/wm/ and src/token.ts; this file only
 * mirrors their results into the DOM and feeds them events. Nothing here reads the page's
 * URL beyond `document.baseURI` (to print the session URL), and no server string is ever
 * rendered as markup.
 */
import {
  MAX_SURFACE_HEIGHT,
  MAX_SURFACE_WIDTH,
  SurfaceRegistry,
  SurfaceRenderer,
  attachInput,
  connectAppricot,
  drawCursor,
  setTextOnly,
} from '@appricot/client';
import type {
  AppricotConnection,
  AttachedSurface,
  CursorImage,
  DetachInput,
  SurfaceRecord,
} from '@appricot/client';
import { POPUP_MARGIN_PX, popupLocalRect } from './wm/popups';
import { applyDrag, applyResize } from './wm/geometry';
import type { Point, Size } from './wm/geometry';
import { WindowManager } from './wm/windows';
import type { ToplevelState } from './wm/windows';
import { connectStateFromForm } from './token';

/** One floating window's DOM + detach handles. */
interface WindowDom {
  root: HTMLElement;
  titleText: HTMLElement;
  appText: HTMLElement;
  canvas: HTMLCanvasElement;
  overlay: HTMLCanvasElement;
  popupLayer: HTMLElement;
  renderer: AttachedSurface;
  detachInput: DetachInput;
}

/** One popup layer entry. */
interface PopupDom {
  el: HTMLElement;
  parentId: number;
  renderer: AttachedSurface;
  detachInput: DetachInput;
}

interface App {
  conn: AppricotConnection | null;
  /** True while the user asked for this connection (drives teardown on close). */
  userOwned: boolean;
  reconnectWanted: boolean;
  registry: SurfaceRegistry;
  wm: WindowManager;
  windows: Map<number, WindowDom>;
  popups: Map<number, PopupDom>;
  cursor: CursorImage | null;
  pointerBySurface: Map<number, Point>;
  configureSerial: number;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  node.className = className;
  return node;
}

/** The ws:// URL of the session endpoint on this origin. Same-origin by design: the demo
 * server proxies /session to the streamer's loopback bind, so connect-src 'self' holds and
 * the streamer is never reachable directly. `document.baseURI`, not `location` — the page
 * never consults its own URL for anything the server could influence. */
function sessionUrl(): string {
  const page = new URL('/', document.baseURI);
  const scheme = page.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${scheme}//${page.host}/session`;
}

function viewportSize(container: HTMLElement): Size {
  return { width: container.clientWidth, height: container.clientHeight };
}

// --- element handles -----------------------------------------------------------------------

/** Fetches one wired element; the page's script is useless without it, so a miss throws. */
function requireElement<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (node === null) {
    throw new Error(`the demo page is missing #${id}`);
  }
  return node as T;
}

const windowsLayer = requireElement<HTMLElement>('windows');
const tokenInput = requireElement<HTMLInputElement>('token');
const connectButton = requireElement<HTMLButtonElement>('connect');
const closeButton = requireElement<HTMLButtonElement>('close');
const reconnectBox = requireElement<HTMLInputElement>('reconnect');
const statusText = requireElement<HTMLElement>('status');
const sessionIdText = requireElement<HTMLElement>('session-id');
const windowsCountText = requireElement<HTMLElement>('windows-count');
const sessionUrlText = requireElement<HTMLElement>('session-url');
const minimizedStrip = requireElement<HTMLElement>('minimized');

const app: App = {
  conn: null,
  userOwned: false,
  reconnectWanted: false,
  registry: new SurfaceRegistry(),
  wm: new WindowManager(),
  windows: new Map(),
  popups: new Map(),
  cursor: null,
  pointerBySurface: new Map(),
  configureSerial: 0,
};

// --- status line ---------------------------------------------------------------------------

function setStatus(text: string, cls?: string): void {
  statusText.className = cls ?? '';
  statusText.textContent = text;
}

function updateWindowCount(): void {
  windowsCountText.textContent = `${app.windows.size} window${app.windows.size === 1 ? '' : 's'}`;
}

// --- focus ---------------------------------------------------------------------------------

/** Paints z-order and the focus ring from the wm's stack, bottom to top. */
function syncStacking(): void {
  app.wm.order().forEach((surfaceId, index) => {
    const dom = app.windows.get(surfaceId);
    if (dom !== undefined) {
      dom.root.style.zIndex = String(index + 1);
    }
  });
  for (const [surfaceId, dom] of app.windows) {
    dom.root.classList.toggle('focused', app.wm.focusedId === surfaceId);
  }
}

/** Click-to-focus: the host raises the window and tells the server (FocusNotify). */
function focusSurface(surfaceId: number): void {
  app.wm.focus(surfaceId);
  syncStacking();
  if (app.conn !== null && app.conn.status === 'open' && app.wm.focusedId === surfaceId) {
    app.conn.send({ kind: 'focusNotify', focusNotify: { surfaceId } });
  }
}

/** Focus left every streamed surface (the user clicked host chrome): BlurRelease. */
function blurAllSurfaces(): void {
  if (app.wm.focusedId === null) {
    return;
  }
  app.wm.blur();
  syncStacking();
  if (app.conn !== null && app.conn.status === 'open') {
    app.conn.send({ kind: 'blurRelease', blurRelease: {} });
  }
}

// --- minimize and restore ----------------------------------------------------------------------

/**
 * Rebuilds the toolbar's strip of restore buttons from the wm's minimized list. Every label
 * is a window title — untrusted server text — so it goes in through `setTextOnly`, exactly
 * like the title bar (ADR-0003 §1): the hostile-string case is one text node, never markup.
 */
function updateMinimizedStrip(): void {
  minimizedStrip.textContent = '';
  for (const surfaceId of app.wm.minimizedIds()) {
    const state = app.wm.get(surfaceId);
    if (state === undefined) {
      continue;
    }
    const restore = el('button', 'restore');
    restore.type = 'button';
    restore.title = 'Restore this window';
    setTextOnly(restore, state.title);
    restore.addEventListener('click', () => {
      restoreSurface(surfaceId);
    });
    minimizedStrip.append(restore);
  }
}

/**
 * Minimizes a window: hide the chrome (a CSS class — the canvas, the renderer and the input
 * stay attached, and the popups hide with the parent because their layer is inside the
 * window), move focus as the wm decided, and tell the server. Nothing on the wire pauses or
 * tears down: the session keeps streaming into the hidden canvas.
 */
function minimizeSurface(surfaceId: number): void {
  const result = app.wm.minimize(surfaceId);
  if (result === undefined) {
    return;
  }
  app.windows.get(surfaceId)?.root.classList.add('minimized');
  updateMinimizedStrip();
  syncStacking();
  if (result.refocus !== null) {
    focusSurface(result.refocus);
  } else if (app.wm.focusedId === null && app.conn !== null && app.conn.status === 'open') {
    app.conn.send({ kind: 'blurRelease', blurRelease: {} });
  }
}

/** Restores a minimized window: unhide it, raise it and focus it (a restore is an
 * activation, like clicking a taskbar entry). */
function restoreSurface(surfaceId: number): void {
  if (app.wm.restore(surfaceId) === undefined) {
    return;
  }
  app.windows.get(surfaceId)?.root.classList.remove('minimized');
  updateMinimizedStrip();
  focusSurface(surfaceId);
}

// --- window geometry -----------------------------------------------------------------------

function applyWindowPosition(surfaceId: number): void {
  const state = app.wm.get(surfaceId);
  const dom = app.windows.get(surfaceId);
  if (state === undefined || dom === undefined) {
    return;
  }
  dom.root.style.left = `${state.x}px`;
  dom.root.style.top = `${state.y}px`;
  repositionPopupsOf(surfaceId);
}

function applyWindowSize(surfaceId: number): void {
  const state = app.wm.get(surfaceId);
  const dom = app.windows.get(surfaceId);
  if (state === undefined || dom === undefined) {
    return;
  }
  dom.canvas.style.width = `${state.size.width}px`;
  dom.canvas.style.height = `${state.size.height}px`;
  repositionPopupsOf(surfaceId);
}

// --- popups ---------------------------------------------------------------------------------

/** Repositions (and resizes) every popup of one parent, from each positioner against the
 * parent's current content size. The clamp lives in the SDK; the layer's own edges do the
 * final visual enforcement. */
function repositionPopupsOf(parentId: number): void {
  const parent = app.wm.get(parentId);
  if (parent === undefined) {
    return;
  }
  for (const surfaceId of app.wm.popupsOf(parentId)) {
    const popup = app.popups.get(surfaceId);
    const record = app.registry.get(surfaceId);
    if (popup === undefined || record === undefined) {
      continue;
    }
    const local = popupLocalRect(record.positioner ?? { anchor: 0, gravity: 0 }, parent.size);
    // The layer sits POPUP_MARGIN_PX outside the content box, so layer coordinates are
    // parent-local coordinates shifted by that margin.
    popup.el.style.left = `${local.x + POPUP_MARGIN_PX}px`;
    popup.el.style.top = `${local.y + POPUP_MARGIN_PX}px`;
    popup.el.style.width = `${local.width}px`;
    popup.el.style.height = `${local.height}px`;
    const canvas = popup.el.firstElementChild;
    if (canvas instanceof HTMLCanvasElement) {
      canvas.style.width = `${local.width}px`;
      canvas.style.height = `${local.height}px`;
    }
  }
}

function removePopupDom(surfaceId: number): void {
  const popup = app.popups.get(surfaceId);
  if (popup === undefined) {
    return;
  }
  app.popups.delete(surfaceId);
  app.wm.removePopup(surfaceId, popup.parentId);
  popup.renderer.detach();
  popup.detachInput();
  popup.el.remove();
}

// --- cursor overlay --------------------------------------------------------------------------

function drawCursorOverlay(surfaceId: number): void {
  const dom = app.windows.get(surfaceId);
  const pointer = app.pointerBySurface.get(surfaceId);
  const image = app.cursor;
  if (dom === undefined || pointer === undefined || image === null || image === undefined) {
    return;
  }
  dom.overlay.width = image.width;
  dom.overlay.height = image.height;
  dom.overlay.style.left = `${pointer.x}px`;
  dom.overlay.style.top = `${pointer.y}px`;
  const ctx = dom.overlay.getContext('2d');
  if (ctx !== null) {
    drawCursor(ctx, image, 0, 0);
  }
}

function setCursorImage(image: CursorImage | null): void {
  app.cursor = image;
  for (const dom of app.windows.values()) {
    dom.root.classList.toggle('has-cursor', image !== null);
  }
  if (app.wm.focusedId !== null) {
    drawCursorOverlay(app.wm.focusedId);
  }
}

// --- drag and resize --------------------------------------------------------------------------

function wireDrag(dom: WindowDom, state: ToplevelState): void {
  const titlebar = dom.root.querySelector<HTMLElement>('.titlebar');
  const close = dom.root.querySelector<HTMLButtonElement>('.close');
  const minimize = dom.root.querySelector<HTMLButtonElement>('.minimize');
  if (titlebar === null || close === null || minimize === null) {
    return;
  }
  titlebar.addEventListener('pointerdown', (down) => {
    // The title-bar buttons are not drag handles.
    if (down.button !== 0 || down.target === close || down.target === minimize) {
      return;
    }
    focusSurface(state.surfaceId);
    const grab = { x: down.clientX - state.x, y: down.clientY - state.y };
    titlebar.setPointerCapture(down.pointerId);
    const onMove = (move: PointerEvent): void => {
      const placed = applyDrag(grab, { x: move.clientX, y: move.clientY }, state.size, viewportSize(windowsLayer));
      app.wm.move(state.surfaceId, placed.x, placed.y);
      applyWindowPosition(state.surfaceId);
    };
    const onUp = (): void => {
      titlebar.removeEventListener('pointermove', onMove);
    };
    titlebar.addEventListener('pointermove', onMove);
    titlebar.addEventListener('pointerup', onUp, { once: true });
    titlebar.addEventListener('pointercancel', onUp, { once: true });
  });
}

function wireResizeGrip(dom: WindowDom, state: ToplevelState): void {
  const grip = dom.root.querySelector<HTMLElement>('.resize-grip');
  if (grip === null) {
    return;
  }
  grip.addEventListener('pointerdown', (down) => {
    if (down.button !== 0) {
      return;
    }
    focusSurface(state.surfaceId);
    const startSize = { ...state.size };
    const grabStart = { x: down.clientX, y: down.clientY };
    grip.setPointerCapture(down.pointerId);
    const onMove = (move: PointerEvent): void => {
      const size = applyResize(startSize, grabStart, { x: move.clientX, y: move.clientY }, {
        width: MAX_SURFACE_WIDTH,
        height: MAX_SURFACE_HEIGHT,
      });
      app.wm.resize(state.surfaceId, size);
      applyWindowSize(state.surfaceId);
    };
    const onUp = (): void => {
      grip.removeEventListener('pointermove', onMove);
      if (app.conn !== null && app.conn.status === 'open') {
        app.configureSerial += 1;
        app.conn.send({
          kind: 'configure',
          configure: {
            surfaceId: state.surfaceId,
            serial: app.configureSerial,
            size: { ...state.size },
          },
        });
      }
    };
    grip.addEventListener('pointermove', onMove);
    grip.addEventListener('pointerup', onUp, { once: true });
    grip.addEventListener('pointercancel', onUp, { once: true });
  });
}

// --- toplevel chrome --------------------------------------------------------------------------

function buildWindow(record: SurfaceRecord): void {
  const state = app.wm.addToplevel(
    {
      surfaceId: record.id,
      appId: record.appId,
      title: record.title,
      size: record.size.width > 0 && record.size.height > 0
        ? record.size
        : { width: 640, height: 420 },
    },
    viewportSize(windowsLayer),
  );
  if (state === undefined || app.conn === null) {
    return; // a repeat id is a replay, not a new window (the wm's spine mirrors the registry)
  }

  // Host chrome first: the brand mark is a host-drawn label the streamed app cannot paint
  // over (ADR-0003 §5); the title and app id after it are untrusted text.
  const root = el('section', 'window');
  root.dataset['surfaceId'] = String(record.id);
  const titlebar = el('header', 'titlebar');
  const hostMark = el('span', 'host-mark');
  hostMark.textContent = 'APPricot';
  const appText = el('span', 'app');
  setTextOnly(appText, record.appId);
  const titleText = el('span', 'title');
  setTextOnly(titleText, record.title);
  const grow = el('span', 'grow');
  const minimize = el('button', 'winctl minimize');
  minimize.type = 'button';
  minimize.textContent = '–';
  minimize.title = 'Minimize (hides the window; the session keeps running)';
  const close = el('button', 'winctl close');
  close.type = 'button';
  close.textContent = '×';
  close.title = 'Close (sends CloseRequest)';
  titlebar.append(hostMark, appText, titleText, grow, minimize, close);

  const content = el('div', 'content');
  const canvas = el('canvas', 'surface');
  const overlay = el('canvas', 'cursor-overlay');
  const popupLayer = el('div', 'popup-layer');
  content.append(canvas, overlay, popupLayer);
  const grip = el('div', 'resize-grip');
  root.append(titlebar, content, grip);

  const conn = app.conn;
  const renderer = SurfaceRenderer.attach(canvas, record.id, { registry: app.registry, conn });
  const detachInput = attachInput(canvas, record.id, {
    conn,
    isFocused: () => app.wm.focusedId === record.id,
    size: () => app.registry.get(record.id)?.size ?? { width: 0, height: 0 },
  });

  const dom: WindowDom = { root, titleText, appText, canvas, overlay, popupLayer, renderer, detachInput };
  app.windows.set(record.id, dom);
  windowsLayer.append(root);

  // Click anywhere on the window focuses it (capture, so it lands before input sends).
  root.addEventListener('pointerdown', () => focusSurface(record.id), true);
  close.addEventListener('click', () => {
    if (conn.status === 'open') {
      conn.send({ kind: 'closeRequest', closeRequest: { surfaceId: record.id } });
    }
  });
  minimize.addEventListener('click', () => {
    minimizeSurface(record.id);
  });
  wireDrag(dom, state);
  wireResizeGrip(dom, state);
  content.addEventListener('pointermove', (move) => {
    const box = canvas.getBoundingClientRect();
    app.pointerBySurface.set(record.id, { x: move.clientX - box.left, y: move.clientY - box.top });
    if (app.wm.focusedId === record.id) {
      drawCursorOverlay(record.id);
    }
  });

  applyWindowPosition(record.id);
  applyWindowSize(record.id);
  focusSurface(record.id);
  updateWindowCount();
}

function buildPopup(record: SurfaceRecord): void {
  if (record.parent === undefined || app.conn === null) {
    return;
  }
  const parentDom = app.windows.get(record.parent);
  const parentState = app.wm.get(record.parent);
  if (parentDom === undefined || parentState === undefined) {
    return;
  }
  const conn = app.conn;
  const popupEl = el('div', 'popup');
  const canvas = el('canvas', 'surface');
  popupEl.append(canvas);
  parentDom.popupLayer.append(popupEl);
  const renderer = SurfaceRenderer.attach(canvas, record.id, { registry: app.registry, conn });
  const detachInput = attachInput(canvas, record.id, {
    conn,
    isFocused: () => app.wm.focusedId === record.parent,
    size: () => ({ width: canvas.clientWidth, height: canvas.clientHeight }),
  });
  popupEl.addEventListener('pointerdown', () => {
    if (record.parent !== undefined) {
      focusSurface(record.parent);
    }
  }, true);
  app.wm.addPopup(record.id, record.parent);
  app.popups.set(record.id, { el: popupEl, parentId: record.parent, renderer, detachInput });
  repositionPopupsOf(record.parent);
}

// --- registry events --------------------------------------------------------------------------

function wireRegistry(registry: SurfaceRegistry): void {
  registry.events.on('window-added', ({ surface }) => {
    if (surface.role === 1) {
      buildPopup(surface);
    } else {
      buildWindow(surface);
    }
    syncStacking();
  });

  registry.events.on('window-removed', ({ surfaceId }) => {
    if (app.popups.has(surfaceId)) {
      removePopupDom(surfaceId);
      return;
    }
    const dom = app.windows.get(surfaceId);
    if (dom === undefined) {
      return;
    }
    const removal = app.wm.removeToplevel(surfaceId);
    app.windows.delete(surfaceId);
    dom.renderer.detach();
    dom.detachInput();
    dom.root.remove();
    app.pointerBySurface.delete(surfaceId);
    for (const popupId of removal?.popups ?? []) {
      removePopupDom(popupId);
    }
    if (removal !== undefined) {
      if (removal.refocus !== null) {
        focusSurface(removal.refocus);
      } else if (app.wm.focusedId === null && app.conn?.status === 'open') {
        app.conn.send({ kind: 'blurRelease', blurRelease: {} });
      }
    }
    updateMinimizedStrip();
    updateWindowCount();
  });

  registry.events.on('metadata', ({ surface }) => {
    const dom = app.windows.get(surface.id);
    if (dom !== undefined) {
      setTextOnly(dom.titleText, surface.title);
      setTextOnly(dom.appText, surface.appId);
    }
    // A title that changes while its window sits minimized must not leave a stale label on
    // the restore strip.
    if (app.wm.isMinimized(surface.id)) {
      updateMinimizedStrip();
    }
  });

  registry.events.on('cursor-changed', ({ image }) => {
    setCursorImage(image);
  });

  registry.events.on('focus-ask', ({ surfaceId }) => {
    // The host decides (ADR-0003 §5); the demo honours the ask like a desktop would.
    if (app.windows.has(surfaceId)) {
      focusSurface(surfaceId);
    }
  });

  registry.events.on('configure-acked', ({ surfaceId, size }) => {
    // Size the chrome to what the app actually took, not what we proposed.
    if (app.windows.has(surfaceId)) {
      app.wm.resize(surfaceId, size);
      applyWindowSize(surfaceId);
    }
  });

  // resize-ask is deliberately not honoured: the host owns size (ADR-0003 §5). The demo
  // could offer a confirmation affordance later; for now the ask changes nothing.
}

// --- connection lifecycle -----------------------------------------------------------------------

function teardownSession(): void {
  for (const surfaceId of [...app.windows.keys()]) {
    const dom = app.windows.get(surfaceId);
    if (dom === undefined) {
      continue;
    }
    dom.renderer.detach();
    dom.detachInput();
    dom.root.remove();
    app.windows.delete(surfaceId);
    app.pointerBySurface.delete(surfaceId);
  }
  for (const surfaceId of [...app.popups.keys()]) {
    removePopupDom(surfaceId);
  }
  app.wm = new WindowManager();
  setCursorImage(null);
  updateMinimizedStrip();
  updateWindowCount();
}

function connect(): void {
  if (app.conn !== null && (app.conn.status === 'connecting' || app.conn.status === 'open')) {
    return;
  }
  // The empty string is the point: the page's query contributes nothing to the connect
  // state. The token comes from the input field alone (see src/token.ts).
  const state = connectStateFromForm(tokenInput, '');
  if ('error' in state) {
    setStatus('enter the token first', 'error');
    return;
  }
  app.reconnectWanted = reconnectBox.checked;
  const registry = new SurfaceRegistry();
  const conn = connectAppricot(sessionUrl(), {
    token: state.token,
    reconnect: app.reconnectWanted,
  });
  app.conn = conn;
  app.registry = registry;
  app.userOwned = true;
  app.configureSerial = 0;
  wireRegistry(registry);
  conn.events.on('message', (envelope) => {
    registry.apply(envelope);
  });
  conn.events.on('status', (status) => {
    if (status === 'open') {
      setStatus('open', 'open');
      // sessionId is untrusted text from HelloReply: text only, never markup.
      setTextOnly(sessionIdText, conn.sessionId === undefined ? '' : `session ${conn.sessionId}`);
      connectButton.disabled = true;
      closeButton.disabled = false;
      return;
    }
    setStatus(status);
    if (status === 'closed') {
      connectButton.disabled = false;
      closeButton.disabled = true;
    }
  });
  conn.events.on('close', (code) => {
    if (app.conn !== conn) {
      return; // a newer connect() (or a user close) owns the state now
    }
    // While a reconnect is pending (abnormal close, reconnect wanted), the connection
    // object, registry and chrome all stay: the SDK re-uses them and the server resumes
    // with one full redraw per surface. Everything else tears down.
    const willReconnect = app.reconnectWanted && code !== 1000;
    if (!willReconnect) {
      teardownSession();
      setTextOnly(sessionIdText, '');
      app.conn = null;
      app.userOwned = false;
      connectButton.disabled = false;
      closeButton.disabled = true;
    }
  });
  setStatus('connecting');
}

function disconnect(): void {
  if (app.conn === null) {
    return;
  }
  const conn = app.conn;
  app.conn = null;
  app.userOwned = false;
  conn.close();
  teardownSession();
  setTextOnly(sessionIdText, '');
  closeButton.disabled = true;
  connectButton.disabled = false;
  setStatus('idle');
}

// --- boot ---------------------------------------------------------------------------------------

setTextOnly(sessionUrlText, sessionUrl());
connectButton.addEventListener('click', () => {
  connect();
});
closeButton.addEventListener('click', () => {
  disconnect();
});
// Focus on host chrome leaves every streamed surface: the server releases held keys.
windowsLayer.parentElement?.addEventListener('pointerdown', (e) => {
  if (e.target === windowsLayer) {
    blurAllSurfaces();
  }
});
document.body.addEventListener('focusin', (e) => {
  if (tokenInput === e.target) {
    blurAllSurfaces();
  }
});
updateWindowCount();
