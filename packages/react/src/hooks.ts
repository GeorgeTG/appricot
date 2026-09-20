import { useCallback, useEffect, useRef, useState } from 'react';

import type { ConnectionStatus, RegistryEvents, SurfaceRecord, SurfaceRegistry } from './client';
import { ROLE_TOPLEVEL } from './client';
import { useAppricot } from './provider';

/** The registry events that change what the hooks below read back. */
const RECORD_EVENTS = ['window-added', 'window-removed', 'metadata', 'configure-acked'] as const;

/**
 * Re-renders the caller on every record-changing registry event and returns the registry.
 * The registry fires one event per mutation, so reading it after every event cannot miss a
 * change; the extra bump right after subscribing catches anything that happened between
 * render and subscribe. Frame arrivals update only `lastSequence` and deliberately do not
 * re-render: they are the renderer's business, not the host tree's.
 */
function useRegistry(): SurfaceRegistry {
  const { registry } = useAppricot();
  const [, setTick] = useState(0);
  const bump = useCallback(() => {
    setTick((tick) => tick + 1);
  }, []);
  useEffect(() => {
    const stops = RECORD_EVENTS.map((event) => registry.events.on(event, bump));
    bump();
    return () => {
      for (const stop of stops) stop();
    };
  }, [registry, bump]);
  return registry;
}

/** The connection's current state, as the provider sees it. */
export function useConnectionStatus(): ConnectionStatus {
  return useAppricot().status;
}

/**
 * The toplevel windows, always current: id, title, app id, scale, size. Popups are not
 * listed; find a popup's parent through `useSurfaceMeta(record.parent)`. Every string is
 * untrusted server text (ADR-0003 §1).
 */
export function useWindows(): SurfaceRecord[] {
  return useRegistry()
    .list()
    .filter((surface) => surface.role === ROLE_TOPLEVEL);
}

/**
 * One surface's metadata (title, app id, scale, size, and a popup's parent), or undefined
 * while the surface is unknown — before its SurfaceNew arrived or after it is gone.
 */
export function useSurfaceMeta(surfaceId: number): SurfaceRecord | undefined {
  return useRegistry().get(surfaceId);
}

/** One registry event as `useSessionEvents` reports it: the event's name plus its payload. */
export type SessionEvent = {
  [K in keyof RegistryEvents]: RegistryEvents[K] extends undefined
    ? { readonly type: K }
    : { readonly type: K } & RegistryEvents[K];
}[keyof RegistryEvents];

/**
 * Reports every session event the registry sees: a surface added, removed or re-titled, the
 * cursor, the server's asks (focus, resize, clipboard) and the configure acks. The library
 * only reports; the host decides every answer (ADR-0003 §5). The handler is kept current
 * without resubscribing, so passing a new closure every render is safe.
 */
export function useSessionEvents(handler: (event: SessionEvent) => void): void {
  const { registry } = useAppricot();
  const latest = useRef(handler);
  useEffect(() => {
    latest.current = handler;
  });
  useEffect(() => {
    const stops = [
      registry.events.on('window-added', (e) => latest.current({ type: 'window-added', surface: e.surface })),
      registry.events.on('window-removed', (e) =>
        latest.current({ type: 'window-removed', surfaceId: e.surfaceId, reason: e.reason }),
      ),
      registry.events.on('metadata', (e) => latest.current({ type: 'metadata', surface: e.surface })),
      registry.events.on('cursor-changed', (e) =>
        latest.current({ type: 'cursor-changed', image: e.image }),
      ),
      registry.events.on('focus-ask', (e) => latest.current({ type: 'focus-ask', surfaceId: e.surfaceId })),
      registry.events.on('resize-ask', (e) =>
        latest.current({ type: 'resize-ask', surfaceId: e.surfaceId, size: e.size }),
      ),
      registry.events.on('configure-acked', (e) =>
        latest.current({ type: 'configure-acked', surfaceId: e.surfaceId, serial: e.serial, size: e.size }),
      ),
      registry.events.on('clipboard-ask', () => latest.current({ type: 'clipboard-ask' })),
    ];
    return () => {
      for (const stop of stops) stop();
    };
  }, [registry]);
}
