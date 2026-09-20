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
   */
  readonly autoConfigure?: boolean;
  /**
   * Whether the host's focus is on this surface. Becoming focused sends FocusNotify — the
   * only message that moves focus — once the connection is open, and again after a resume.
   * Losing focus sends nothing: blur-release is session-wide and only the host knows whether
   * focus moved to another APPricot surface or left APPricot entirely, so releasing is the
   * host's call (through `send`). attachInput gates keys on this and releases by itself
   * when the whole window loses focus.
   */
  readonly focused?: boolean;
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
 */
export function AppricotSurface({ id, autoConfigure = false, focused = false }: AppricotSurfaceProps) {
  const { conn, registry } = useAppricot();
  const meta = useSurfaceMeta(id);
  const known = meta !== undefined;
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const serialRef = useRef(0);
  const focusedRef = useRef(false);

  useEffect(() => {
    focusedRef.current = focused;
  });

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
    // When the connection (re)opens, propose again: sends before 'open' were dropped, and
    // a resume restarts the app's size knowledge with one full redraw either way.
    const reopen = (): void => {
      if (conn.status !== 'open') return;
      sentWidth = -1;
      sentHeight = -1;
      propose();
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
      stopReopen();
      stopWatching();
    };
  }, [autoConfigure, conn, id, known]);

  // width/height are deliberately absent: the renderer owns the backing store. The title
  // is untrusted text, safe as an aria-label (never an HTML sink, ADR-0003 §1).
  return <canvas ref={canvasRef} role="img" aria-label={meta?.title} />;
}
