import { useEffect, useRef } from 'react';

import {
  attachInput,
  MAX_SURFACE_HEIGHT,
  MAX_SURFACE_WIDTH,
  sendConfigure,
  sendFocusNotify,
  SurfaceRenderer,
} from './client';
import { useSurfaceMeta } from './hooks';
import { useAppricot } from './provider';

// Without ResizeObserver (jsdom, older environments) the size is polled. Every current
// browser has ResizeObserver, so this exists for tests and exotic runtimes.
const RESIZE_POLL_MS = 200;

/** A server's ResizeAsk for one surface: the size the app would like to have. */
export interface AppricotResizeAsk {
  readonly surfaceId: number;
  readonly size: { readonly width: number; readonly height: number };
}

export interface AppricotSurfaceProps {
  /** The surface to draw, the registry's surface id. */
  readonly id: number;
  /**
   * When true (default false), the component watches the size the HOST gave its canvas and
   * proposes it to the app as a Configure. Serials are this component's own counter,
   * starting at 1 and rising by one; the server acks whatever serial it is sent with the
   * size the app really took, and the registry's record — and so the canvas backing store —
   * follows the ack, not the proposal. The proposal is re-sent when the connection (re)opens:
   * the SDK drops sends while the connection is not open.
   *
   * The host must give the canvas a CSS size. An unsized canvas's CSS box is its backing
   * store, which the renderer sets to the surface size times its scale; at any scale but 1x
   * proposing that box would grow the surface on every ack. So a box that equals the backing
   * store of a surface whose scale is not 1x is taken as unsized, and nothing is proposed.
   */
  readonly autoConfigure?: boolean;
  /**
   * Whether the host's focus is on this surface. Becoming focused moves DOM focus to the
   * canvas (so the keyboard reaches it) and sends FocusNotify — the only message that moves
   * focus — once the connection is open, and again after a resume. Losing focus sends
   * nothing: blur-release is session-wide and only the host knows whether focus moved to
   * another APPricot surface or left APPricot entirely, so releasing is the host's call
   * (through `send`). attachInput gates keys on this and releases by itself when the whole
   * window loses focus.
   */
  readonly focused?: boolean;
  /**
   * What to do when the app asks for a new size (a ResizeAsk). The host decides size
   * (ADR-0003 §5), and by default this component honours the ask: it answers with a
   * Configure of the asked size, clamped to the v0 surface limits. With `autoConfigure` on,
   * the host's canvas box decides instead, and the answer is a Configure of that box. Pass a
   * handler to decide yourself: the component then sends nothing, and the handler answers
   * (or not) through `send`. Either way the ConfigureAck sizes the registry's record.
   */
  readonly onResizeAsk?: (ask: AppricotResizeAsk) => void;
}

/**
 * One streamed surface: a single <canvas>, and that canvas is the component's entire DOM.
 *
 * The client's SurfaceRenderer draws the surface's frames into the canvas — and owns its
 * backing store: it sets canvas.width/height from the registry record scaled by the
 * surface's scale, so this component never sets them itself. Layout (CSS size) stays the
 * host's. attachInput turns host input inside the canvas into protocol messages; both
 * attach once the surface is known to the registry and detach on unmount. Sending for a
 * surface the registry does not know would be a protocol violation that closes the
 * session, so nothing is sent before the surface arrives. The host draws all chrome
 * around the canvas (ADR-0003 §5).
 *
 * The canvas is focusable (tabIndex 0) and has the `application` role: it is an interactive
 * surface that takes the keyboard, not an image.
 */
export function AppricotSurface({
  id,
  autoConfigure = false,
  focused = false,
  onResizeAsk,
}: AppricotSurfaceProps) {
  const { conn, registry } = useAppricot();
  const meta = useSurfaceMeta(id);
  const known = meta !== undefined;
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const serialRef = useRef(0);
  const focusedRef = useRef(false);
  const resizeAskRef = useRef(onResizeAsk);
  // Set by the autoConfigure effect: proposes the host's box again, even if unchanged.
  const reproposeRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    focusedRef.current = focused;
    resizeAskRef.current = onResizeAsk;
  });

  // DOM focus follows the host's focus: without it the keyboard never reaches the canvas's
  // listeners. preventScroll: focusing a window must not scroll the host page.
  useEffect(() => {
    if (!focused) return;
    canvasRef.current?.focus({ preventScroll: true });
  }, [focused]);

  // The renderer. Detached only when the connection, the surface id, or the surface's
  // existence changes — never on focus flips or resizes, so the canvas content survives.
  useEffect(() => {
    if (conn === null || !known) return;
    const canvas = canvasRef.current;
    if (canvas === null) return;
    const attached = SurfaceRenderer.attach(canvas, id, { registry, conn });
    return () => {
      attached.detach();
    };
  }, [conn, id, known, registry]);

  // Input capture. Attached once; attachInput asks isFocused() per event, so the focused
  // prop flows through the ref without re-attaching listeners.
  useEffect(() => {
    if (conn === null || !known) return;
    const canvas = canvasRef.current;
    if (canvas === null) return;
    return attachInput(canvas, id, {
      conn,
      isFocused: () => focusedRef.current,
    });
  }, [conn, id, known]);

  // Focus notification: sent when the surface is (or becomes) focused while the connection
  // is open, and re-sent when the connection reaches 'open' again — the SDK drops sends
  // while it is not open, and a resume forgets the app's focus.
  useEffect(() => {
    if (conn === null || !known) return;
    const notify = (): void => {
      if (focused && conn.status === 'open') {
        sendFocusNotify(conn, id);
      }
    };
    notify();
    return conn.events.on('status', notify);
  }, [conn, id, known, focused]);

  // ResizeAsk: the host's handler if it passed one, the default answer otherwise.
  useEffect(() => {
    if (conn === null || !known) return;
    return registry.events.on('resize-ask', ({ surfaceId, size }) => {
      if (surfaceId !== id) return;
      const handler = resizeAskRef.current;
      if (handler !== undefined) {
        handler({ surfaceId, size: { width: size.width, height: size.height } });
        return;
      }
      if (autoConfigure) {
        reproposeRef.current?.();
        return;
      }
      // An absent or empty size asks for nothing a Configure could carry.
      if (size.width < 1 || size.height < 1) return;
      serialRef.current += 1;
      sendConfigure(
        conn,
        id,
        serialRef.current,
        Math.min(size.width, MAX_SURFACE_WIDTH),
        Math.min(size.height, MAX_SURFACE_HEIGHT),
      );
    });
  }, [autoConfigure, conn, id, known, registry]);

  // autoConfigure: watch the canvas's CSS box and propose its size to the app.
  useEffect(() => {
    if (!autoConfigure || conn === null || !known) return;
    const canvas = canvasRef.current;
    if (canvas === null) return;
    let sentWidth = -1;
    let sentHeight = -1;
    const propose = (): void => {
      const observedWidth = canvas.clientWidth;
      const observedHeight = canvas.clientHeight;
      // Hidden or not laid out: proposing a zero size is meaningless.
      if (observedWidth < 1 || observedHeight < 1) return;
      // An unsized canvas: its box is the backing store the renderer set from the surface
      // size times a scale that is not 1x. Proposing it would ratchet the size up on every
      // ack, so nothing is proposed until the host sizes the canvas.
      const record = registry.get(id);
      if (
        record !== undefined &&
        record.scale !== 120 &&
        observedWidth === canvas.width &&
        observedHeight === canvas.height
      ) {
        return;
      }
      // The server closes the session on a Configure past the limits table, so clamp first.
      const width = Math.min(observedWidth, MAX_SURFACE_WIDTH);
      const height = Math.min(observedHeight, MAX_SURFACE_HEIGHT);
      // Identical proposals are dropped, so the ack-driven backing-store change on the
      // canvas cannot loop back into another Configure.
      if (width === sentWidth && height === sentHeight) return;
      sentWidth = width;
      sentHeight = height;
      serialRef.current += 1;
      sendConfigure(conn, id, serialRef.current, width, height);
    };
    // Forgets the last proposal and proposes again: a reopen, or the answer to a ResizeAsk.
    const repropose = (): void => {
      sentWidth = -1;
      sentHeight = -1;
      propose();
    };
    reproposeRef.current = repropose;
    // When the connection (re)opens, propose again: sends before 'open' were dropped, and
    // a resume restarts the app's size knowledge with one full redraw either way.
    const reopen = (): void => {
      if (conn.status !== 'open') return;
      repropose();
    };
    const stopReopen = conn.events.on('status', reopen);
    let stopWatching: () => void;
    if (typeof ResizeObserver === 'undefined') {
      const timer = setInterval(propose, RESIZE_POLL_MS);
      stopWatching = () => {
        clearInterval(timer);
      };
    } else {
      // ResizeObserver fires once on observe, so the initial size is proposed too.
      const observer = new ResizeObserver(propose);
      observer.observe(canvas);
      stopWatching = () => {
        observer.disconnect();
      };
    }
    return () => {
      if (reproposeRef.current === repropose) reproposeRef.current = null;
      stopReopen();
      stopWatching();
    };
  }, [autoConfigure, conn, id, known, registry]);

  // width/height are deliberately absent: the renderer owns the backing store. The title
  // is untrusted text, safe as an aria-label (never an HTML sink, ADR-0003 §1).
  return <canvas ref={canvasRef} tabIndex={0} role="application" aria-label={meta?.title} />;
}
