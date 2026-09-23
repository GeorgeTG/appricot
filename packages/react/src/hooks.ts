import { useCallback, useEffect, useRef, useSyncExternalStore } from 'react';

import type { ConnectionStatus, RegistryEvents, SurfaceRecord, SurfaceRegistry } from './client';
import { ROLE_TOPLEVEL } from './client';
import { useAppricot } from './provider';

/** The registry events that change what the hooks below read back. */
const RECORD_EVENTS = ['window-added', 'window-removed', 'metadata', 'configure-acked'] as const;

/**
 * The last snapshot handed out per live registry record. The registry mutates its records
 * in place; the hooks hand out frozen copies instead, and a copy stays the same object until
 * a field the hooks report changes. That is what lets a host key React.memo or useMemo on
 * what a hook returned. Module-wide, so every component sees the same snapshot object.
 */
const snapshots = new WeakMap<SurfaceRecord, SurfaceRecord>();

/** The last `useWindows` list handed out per registry, for the same reason. */
const toplevelLists = new WeakMap<SurfaceRegistry, readonly SurfaceRecord[]>();

/**
 * The frozen snapshot of one live record: the cached one while every reported field is
 * unchanged, a new one otherwise. `lastSequence` is copied but not compared: frames do not
 * re-render the host tree, they are the renderer's business.
 */
function snapshotOf(record: SurfaceRecord): SurfaceRecord {
  const cached = snapshots.get(record);
  if (
    cached !== undefined &&
    cached.id === record.id &&
    cached.role === record.role &&
    cached.parent === record.parent &&
    cached.title === record.title &&
    cached.appId === record.appId &&
    cached.scale === record.scale &&
    cached.size.width === record.size.width &&
    cached.size.height === record.size.height &&
    cached.positioner === record.positioner
  ) {
    return cached;
  }
  const snapshot = Object.freeze({
    ...record,
    size: Object.freeze({ ...record.size }),
  });
  snapshots.set(record, snapshot);
  return snapshot;
}

/** The subscribe function useSyncExternalStore wants, stable per registry: every
 * record-changing registry event is a possible change. */
function useRecordSubscription(registry: SurfaceRegistry): (onChange: () => void) => () => void {
  return useCallback(
    (onChange: () => void) => {
      const stops = RECORD_EVENTS.map((event) => registry.events.on(event, onChange));
      return () => {
        for (const stop of stops) stop();
      };
    },
    [registry],
  );
}

/** The connection's current state, as the provider sees it. */
export function useConnectionStatus(): ConnectionStatus {
  return useAppricot().status;
}

/**
 * The toplevel windows, always current: id, title, app id, scale, size. Popups are not
 * listed; find a popup's parent through `useSurfaceMeta(record.parent)`. Every string is
 * untrusted server text (ADR-0003 §1).
 *
 * The list and its records are frozen snapshots: a record object changes identity exactly
 * when one of its fields does, and the list changes identity exactly when a window comes,
 * goes or changes. Mutating them is a TypeError in strict mode, and it would not reach the
 * registry anyway.
 */
export function useWindows(): readonly SurfaceRecord[] {
  const { registry } = useAppricot();
  const subscribe = useRecordSubscription(registry);
  const getSnapshot = useCallback((): readonly SurfaceRecord[] => {
    const next = registry
      .list()
      .filter((surface) => surface.role === ROLE_TOPLEVEL)
      .map(snapshotOf);
    const cached = toplevelLists.get(registry);
    if (
      cached !== undefined &&
      cached.length === next.length &&
      cached.every((surface, index) => surface === next[index])
    ) {
      return cached;
    }
    const list = Object.freeze(next);
    toplevelLists.set(registry, list);
    return list;
  }, [registry]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/**
 * One surface's metadata (title, app id, scale, size, and a popup's parent), or undefined
 * while the surface is unknown — before its SurfaceNew arrived or after it is gone.
 *
 * A frozen snapshot, as for `useWindows`: the object changes identity exactly when a field
 * changes, so it is safe as a memo key, and a change to another surface does not re-render
 * this caller.
 */
export function useSurfaceMeta(surfaceId: number): SurfaceRecord | undefined {
  const { registry } = useAppricot();
  const subscribe = useRecordSubscription(registry);
  const getSnapshot = useCallback((): SurfaceRecord | undefined => {
    const record = registry.get(surfaceId);
    return record === undefined ? undefined : snapshotOf(record);
  }, [registry, surfaceId]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
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
 *
 * A `configure-acked` with serial 0 is not an answer to any Configure: the app resized
 * itself (docs/protocol/v0.md), and the event's `size` is the size it now has. A host sizes
 * its chrome to it.
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
