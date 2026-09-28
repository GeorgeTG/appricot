/**
 * The demo host page's DOM wiring (the browser entry, `/dist/main.js`).
 *
 * This is a host UI of its own: a toolbar (same-origin session URL shown as text, a token
 * INPUT field that is the token's only source — never the URL or its query — connect/close
 * buttons, a status line, a reconnect toggle, an opt-in toggle that sends pasted text to the
 * streamed app (off by default, ADR-0003 §7), a button that writes the app's last copied
 * text to the user's clipboard inside its click gesture — the only clipboard write, the held
 * text is rendered nowhere — and a strip of restore buttons for the
 * minimized windows, each titled with the window's own — untrusted — title through
 * `setTextOnly`) and, per streamed toplevel, one floating window of demo chrome: a title bar
 * whose title and app id go in through `setTextOnly` (untrusted text, ADR-0003 §1),
 * drag-to-move, a close button that sends CloseRequest, a minimize button that hides the
 * window without touching the session (the canvas, the renderer and the input stay attached;
 * a minimized parent hides its popups with it, since the popup layer is inside the window),
 * a focusable canvas the SDK's SurfaceRenderer attaches to, click-to-focus (FocusNotify, a
 * visual focus ring, and DOM focus on the canvas so the keyboard reaches it), a resize grip
 * that sends Configure on release, a ResizeAsk answered with a Configure clamped to the
 * desktop, chrome sized to every ConfigureAck (serial 0 included: the app resized itself), an
 * absolutely-positioned popup layer clamped to the parent (ADR-0003 §5), and a cursor
 * overlay drawn from the wire's cursor pixels, its hotspot on the pointer.
 *
 * The connection's lifecycle follows the SDK's statuses: 'reconnecting' is a retry still
 * pending, so the windows stay and a resume reconciles them in place (v0 §7); the `ended`
 * event (after the 'closed' status) is the end, and the windows go.
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
  cursorOrigin,
  drawCursor,
  setTextOnly,
} from '@app-ricot/client';
import type {
  AppricotConnection,
  AttachedSurface,
  CloseReason,
  CursorImage,
  DetachInput,
  SurfaceRecord,
} from '@app-ricot/client';
import { POPUP_MARGIN_PX, popupLocalRect } from './wm/popups';
import { applyDrag, applyResize, grantResize } from './wm/geometry';
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
  canvas: HTMLCanvasElement;
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
  /**
   * The streamed app's last copied text, as untrusted data (ADR-0003 §1/§7): held for the
   * user, rendered nowhere, and written to the user's clipboard only by the toolbar button —
   * a real user gesture, the only writer. Null while nothing is held.
   */
  appCopy: string | null;
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
/**
 * The paste opt-in (ADR-0003 §7). Unchecked, or missing from the page, pasted text stays in
 * the host; checked, a paste into a streamed window sends its text to the app.
 */
const pasteBox = document.getElementById('paste');

/**
 * The app-to-host copy button (ADR-0003 §7). Optional like the paste box: a page without it
 * simply holds nothing user-visible. Its click is the only writer of the user's clipboard.
 */
const copyAppTextButton = document.getElementById('copy-app-text');

/** The demo's paste policy: the user's own opt-in, read at every paste. */
function pastePolicy(): boolean {
  return pasteBox instanceof HTMLInputElement && pasteBox.checked;
}

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
  appCopy: null,
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

/**
 * How a focus change moves DOM focus:
 * - 'user': the user acted on the window (a click, minimize, restore): its canvas takes DOM
 *   focus.
 * - 'auto': the change came from the session (a new window, a focus-ask, a window going
 *   away): the canvas takes DOM focus only when the user is not typing into a host field, so
 *   a server can never pull the keyboard out of one (ADR-0003 §6: keys typed for the host
 *   never reach a session).
 * - 'none': a popup of the window takes the click, and DOM focus belongs on its canvas.
 */
type DomFocus = 'user' | 'auto' | 'none';

/** True when DOM focus is not in a host text field: no input (other than a button or a
 * checkbox), no textarea, no select, nothing editable. */
function domFocusIsFree(): boolean {
  const active = document.activeElement;
  if (active instanceof HTMLInputElement) {
    return ['button', 'checkbox', 'radio', 'reset', 'submit'].includes(active.type);
  }
  if (active instanceof HTMLTextAreaElement || active instanceof HTMLSelectElement) {
    return false;
  }
  return !(active instanceof HTMLElement && active.isContentEditable);
}

/**
 * Click-to-focus: the host raises the window and tells the server (FocusNotify). DOM focus
 * follows onto the window's canvas, because that is where attachInput listens for keys: a
 * canvas without DOM focus never sees one (C11). See DomFocus for when it follows.
 */
function focusSurface(surfaceId: number, domFocus: DomFocus = 'user'): void {
  app.wm.focus(surfaceId);
  syncStacking();
  if (app.wm.focusedId !== surfaceId) {
    return; // unknown or minimized: nothing was focused
  }
  if (app.conn !== null && app.conn.status === 'open') {
    app.conn.send({ kind: 'focusNotify', focusNotify: { surfaceId } });
  }
  if (domFocus === 'user' || (domFocus === 'auto' && domFocusIsFree())) {
    app.windows.get(surfaceId)?.canvas.focus({ preventScroll: true });
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
    const positioner = record.positioner ?? { anchor: 0, gravity: 0 };
    // Placed at the size the popup has now: one that resized itself (a serial-0
    // ConfigureAck, C1) is drawn at its new size, not the size its positioner first named.
    const sized = record.size.width > 0 && record.size.height > 0
      ? { ...positioner, size: record.size }
      : positioner;
    const local = popupLocalRect(sized, parent.size);
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
  // A menu that closes while it holds the keyboard hands it back to its window (C11): the
  // browser would drop DOM focus to the body, and the keys typed next would reach nothing.
  const hadFocus = document.activeElement === popup.canvas;
  app.popups.delete(surfaceId);
  app.wm.removePopup(surfaceId, popup.parentId);
  popup.renderer.detach();
  popup.detachInput();
  popup.el.remove();
  if (hadFocus) {
    app.windows.get(popup.parentId)?.canvas.focus({ preventScroll: true });
  }
}

// --- cursor overlay --------------------------------------------------------------------------

function drawCursorOverlay(surfaceId: number): void {
  const dom = app.windows.get(surfaceId);
  const pointer = app.pointerBySurface.get(surfaceId);
  const image = app.cursor;
  if (dom === undefined || pointer === undefined || image === null || image === undefined) {
    return;
  }
  // The overlay is the image's size, placed so the hotspot sits on the pointer, and the image
  // is drawn at its origin: drawing at the pointer would crop every cursor whose hotspot is not
  // its top-left corner.
  const origin = cursorOrigin(image, pointer.x, pointer.y);
  dom.overlay.width = image.width;
  dom.overlay.height = image.height;
  dom.overlay.style.left = `${origin.x}px`;
  dom.overlay.style.top = `${origin.y}px`;
  const ctx = dom.overlay.getContext('2d');
  if (ctx !== null) {
    drawCursor(ctx, image);
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
    // One controller per gesture: whichever of up or cancel ends it removes all three
    // listeners, so no stale handler outlives the gesture.
    const gesture = new AbortController();
    const onMove = (move: PointerEvent): void => {
      const placed = applyDrag(grab, { x: move.clientX, y: move.clientY }, state.size, viewportSize(windowsLayer));
      app.wm.move(state.surfaceId, placed.x, placed.y);
      applyWindowPosition(state.surfaceId);
    };
    const onUp = (): void => {
      gesture.abort();
    };
    titlebar.addEventListener('pointermove', onMove, { signal: gesture.signal });
    titlebar.addEventListener('pointerup', onUp, { signal: gesture.signal });
    titlebar.addEventListener('pointercancel', onUp, { signal: gesture.signal });
  });
}

/**
 * Keeps a press on non-focusable host chrome (the title bar, the resize grip) from moving DOM
 * focus: the browser's default for a mousedown there is to focus the body, which would take
 * the keyboard away from the canvas focusSurface just focused. The window controls and the
 * canvases keep their default.
 */
function keepFocusOnChrome(element: HTMLElement, controls: readonly Element[]): void {
  element.addEventListener('mousedown', (down) => {
    if (down.target instanceof Element && controls.includes(down.target)) {
      return;
    }
    down.preventDefault();
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
    // One controller per gesture: up or cancel ends it once and removes all three
    // listeners, so a later pointercancel cannot fire a stale handler and send a Configure.
    const gesture = new AbortController();
    const onMove = (move: PointerEvent): void => {
      const size = applyResize(startSize, grabStart, { x: move.clientX, y: move.clientY }, {
        width: MAX_SURFACE_WIDTH,
        height: MAX_SURFACE_HEIGHT,
      });
      app.wm.resize(state.surfaceId, size);
      applyWindowSize(state.surfaceId);
    };
    const onUp = (): void => {
      gesture.abort();
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
    grip.addEventListener('pointermove', onMove, { signal: gesture.signal });
    grip.addEventListener('pointerup', onUp, { signal: gesture.signal });
    grip.addEventListener('pointercancel', onUp, { signal: gesture.signal });
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

  // The popup layer is a sibling of .content, not a child: .content clips at its own box
  // (overflow: hidden), and the layer must reach --popup-margin past it (styles.css).
  const frame = el('div', 'content-frame');
  const content = el('div', 'content');
  const canvas = el('canvas', 'surface');
  // Focusable, so the keys attachInput listens for can reach it (C11).
  canvas.tabIndex = 0;
  const overlay = el('canvas', 'cursor-overlay');
  const popupLayer = el('div', 'popup-layer');
  content.append(canvas, overlay);
  frame.append(content, popupLayer);
  const grip = el('div', 'resize-grip');
  root.append(titlebar, frame, grip);

  const conn = app.conn;
  const renderer = SurfaceRenderer.attach(canvas, record.id, { registry: app.registry, conn });
  const detachInput = attachInput(canvas, record.id, {
    conn,
    isFocused: () => app.wm.focusedId === record.id,
    size: () => app.registry.get(record.id)?.size ?? { width: 0, height: 0 },
    paste: pastePolicy,
  });

  const dom: WindowDom = { root, titleText, appText, canvas, overlay, popupLayer, renderer, detachInput };
  app.windows.set(record.id, dom);
  windowsLayer.append(root);

  // Click anywhere on the window focuses it (capture, so it lands before input sends). A
  // press inside a popup is the popup's: its own capture handler focuses this window
  // without taking DOM focus from the popup's canvas.
  root.addEventListener('pointerdown', (down) => {
    if (down.target instanceof Node && popupLayer.contains(down.target)) {
      return;
    }
    focusSurface(record.id);
  }, true);
  keepFocusOnChrome(titlebar, [minimize, close]);
  keepFocusOnChrome(grip, []);
  // DOM focus arriving some other way (Tab, say) focuses the window too, so the wm and the
  // keyboard agree on which surface is focused.
  canvas.addEventListener('focus', () => {
    if (app.wm.focusedId !== record.id) {
      focusSurface(record.id, 'none');
    }
  });
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
  // On the frame, not the content: moves over a popup (a sibling of the content now) count too.
  frame.addEventListener('pointermove', (move) => {
    const box = canvas.getBoundingClientRect();
    app.pointerBySurface.set(record.id, { x: move.clientX - box.left, y: move.clientY - box.top });
    if (app.wm.focusedId === record.id) {
      drawCursorOverlay(record.id);
    }
  });

  applyWindowPosition(record.id);
  applyWindowSize(record.id);
  focusSurface(record.id, 'auto');
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
  const parentId = record.parent;
  const popupEl = el('div', 'popup');
  const canvas = el('canvas', 'surface');
  // Focusable: a menu takes the keyboard (arrows, Enter, Escape) while it is open (C11).
  canvas.tabIndex = 0;
  popupEl.append(canvas);
  parentDom.popupLayer.append(popupEl);
  const renderer = SurfaceRenderer.attach(canvas, record.id, { registry: app.registry, conn });
  // The size is the registry's: the popup's CSS box may be clamped smaller than the surface
  // (ADR-0003 §5), and a position is scaled from that box to the surface.
  const surfaceId = record.id;
  const detachInput = attachInput(canvas, surfaceId, {
    conn,
    isFocused: () => app.wm.focusedId === parentId,
    size: () => app.registry.get(surfaceId)?.size,
    paste: pastePolicy,
  });
  // A press on the popup focuses its parent window, and DOM focus goes to the popup's own
  // canvas: keys are not addressed to a surface on the wire, the server routes them to the
  // app's focus, so either canvas delivers them.
  popupEl.addEventListener('pointerdown', () => {
    focusSurface(parentId, 'none');
    canvas.focus({ preventScroll: true });
  }, true);
  canvas.addEventListener('focus', () => {
    if (app.wm.focusedId !== parentId) {
      focusSurface(parentId, 'none');
    }
  });
  app.wm.addPopup(record.id, record.parent);
  app.popups.set(record.id, { el: popupEl, canvas, parentId: record.parent, renderer, detachInput });
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
        focusSurface(removal.refocus, 'auto');
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
    // The wm keeps its own copy of the title, and the restore strip reads that copy: update
    // it too, or a title that changes while its window sits minimized leaves a stale label.
    app.wm.setMetadata(surface.id, { title: surface.title, appId: surface.appId });
    if (app.wm.isMinimized(surface.id)) {
      updateMinimizedStrip();
    }
  });

  registry.events.on('cursor-changed', ({ image }) => {
    setCursorImage(image);
  });

  registry.events.on('focus-ask', ({ surfaceId }) => {
    // The host decides (ADR-0003 §5); the demo honours the ask like a desktop would, but it
    // never pulls DOM focus out of a host text field for it ('auto').
    if (app.windows.has(surfaceId)) {
      focusSurface(surfaceId, 'auto');
    }
  });

  registry.events.on('clipboard-text', ({ text }) => {
    // The app copied (ADR-0003 §7): the text is untrusted data, so it is held and never
    // rendered — the status line says how much arrived, in the host's own words, and the
    // write happens only in the button's user gesture.
    app.appCopy = text;
    if (copyAppTextButton instanceof HTMLButtonElement) {
      copyAppTextButton.disabled = false;
    }
    setStatus(`app copied ${text.length} characters`);
  });

  registry.events.on('configure-acked', ({ surfaceId, size }) => {
    // Size the chrome to what the app actually took, not what we proposed. Serial 0 is no
    // answer to a Configure of ours: the app resized itself (C1), and the chrome follows it
    // all the same. A popup is re-placed at its new size.
    if (app.windows.has(surfaceId)) {
      app.wm.resize(surfaceId, size);
      applyWindowSize(surfaceId);
      return;
    }
    const popup = app.popups.get(surfaceId);
    if (popup !== undefined) {
      repositionPopupsOf(popup.parentId);
    }
  });

  registry.events.on('resize-ask', ({ surfaceId, size }) => {
    // The host decides size (ADR-0003 §5), and the demo grants what fits (C1): a Configure
    // of the asked size, clamped to its desktop and to the v0 surface bound. The ack then
    // sizes the chrome, as for a resize by the grip. A popup's ask is not a window's: its
    // size comes from its positioner, so it changes nothing here.
    const conn = app.conn;
    if (!app.windows.has(surfaceId) || conn === null || conn.status !== 'open') {
      return;
    }
    const granted = grantResize(size, viewportSize(windowsLayer), {
      width: MAX_SURFACE_WIDTH,
      height: MAX_SURFACE_HEIGHT,
    });
    if (granted === null) {
      return;
    }
    app.configureSerial += 1;
    conn.send({
      kind: 'configure',
      configure: { surfaceId, serial: app.configureSerial, size: granted },
    });
  });
}

// --- connection lifecycle -----------------------------------------------------------------------

/** A fresh, wired registry for the next session. Surface ids are per session, so a registry
 * never outlives the session it was fed from: the old one is dropped, not cleared. */
function resetRegistry(): void {
  const registry = new SurfaceRegistry();
  wireRegistry(registry);
  app.registry = registry;
}

/** Drops every window, popup and cursor from the page, and starts a fresh registry: after
 * this, a re-announced surface (a resume on the same connection) is a new window again. */
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
  resetRegistry();
  setCursorImage(null);
  // The app's held copy belongs to the session that produced it; the next session starts
  // with nothing to write, and the button says so.
  app.appCopy = null;
  if (copyAppTextButton instanceof HTMLButtonElement) {
    copyAppTextButton.disabled = true;
  }
  updateMinimizedStrip();
  updateWindowCount();
}

/** Closes the current connection for good (a pending reconnect with it) and clears the page.
 * The connection's later events are ignored: it is no longer `app.conn`. */
function endSession(): void {
  const conn = app.conn;
  app.conn = null;
  app.userOwned = false;
  conn?.close();
  teardownSession();
  setTextOnly(sessionIdText, '');
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
  // A connection waiting out its backoff ('reconnecting') must not come back next to the new
  // session: close it for good, and drop what it left on the page, whose ids would collide
  // with the new session's.
  endSession();
  app.reconnectWanted = reconnectBox.checked;
  const conn = connectAppricot(sessionUrl(), {
    token: state.token,
    reconnect: app.reconnectWanted,
  });
  app.conn = conn;
  app.userOwned = true;
  app.configureSerial = 0;
  // Every handler checks that this connection is still the page's: a replaced one can keep
  // emitting (a late close, a status change), and it must not touch the new session.
  conn.events.on('message', (envelope) => {
    if (app.conn === conn) {
      app.registry.apply(envelope);
    }
  });
  conn.events.on('status', (status) => {
    if (app.conn !== conn) {
      return;
    }
    if (status === 'open') {
      setStatus('open', 'open');
      // sessionId is untrusted text from HelloReply: text only, never markup.
      setTextOnly(sessionIdText, conn.sessionId === undefined ? '' : `session ${conn.sessionId}`);
      connectButton.disabled = true;
      closeButton.disabled = false;
      return;
    }
    setStatus(status);
    if (status === 'connecting') {
      connectButton.disabled = true;
      closeButton.disabled = false;
      return;
    }
    if (status === 'reconnecting') {
      // A retry is pending, not an end: the windows stay. A resume re-announces them and the
      // registry updates each in place (v0 §7); a fresh session instead removes them through
      // the registry. Connect starts over, and Close stops the retry.
      connectButton.disabled = false;
      closeButton.disabled = false;
      return;
    }
    if (status === 'closed') {
      // Terminal: the SDK retries nothing after 'closed'. The `ended` event follows.
      closed(conn);
    }
  });
  conn.events.on('ended', (reason) => {
    closed(conn, reason);
  });
  connectButton.disabled = true;
  closeButton.disabled = false;
  setStatus('connecting');
}

/**
 * The connection is over for good (the 'closed' status, then `ended` with its reason): the
 * windows go, and Connect starts over. Idempotent, and a no-op for a replaced connection.
 */
function closed(conn: AppricotConnection, reason?: CloseReason): void {
  if (app.conn !== conn) {
    return;
  }
  teardownSession();
  setTextOnly(sessionIdText, '');
  // The cause is the SDK's own word, never server text; a Bye's text is not shown.
  if (reason !== undefined) {
    setStatus(`closed: ${reason.cause}`, reason.cause === 'user' ? '' : 'error');
  }
  connectButton.disabled = false;
  closeButton.disabled = true;
}

function disconnect(): void {
  if (app.conn === null) {
    return;
  }
  endSession();
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
// The one clipboard write of the app-to-host direction (ADR-0003 §7): a click is the user
// gesture the browser requires, and the held text leaves as data through writeText — the
// page never renders it, so nothing untrusted reaches the DOM on this path either.
copyAppTextButton?.addEventListener('click', () => {
  const text = app.appCopy;
  if (text === null) {
    return; // nothing held: the button is disabled, this is only the belt to the braces
  }
  if (navigator.clipboard === undefined) {
    // A non-secure context (or an old engine): the demo serves loopback-only, which is a
    // secure context in every current browser, so this is the diagnostic, not the norm.
    setStatus('no clipboard API in this context', 'error');
    return;
  }
  void navigator.clipboard
    .writeText(text)
    .then(() => {
      setStatus('copied to your clipboard');
    })
    .catch(() => {
      setStatus('clipboard write refused', 'error');
    });
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
