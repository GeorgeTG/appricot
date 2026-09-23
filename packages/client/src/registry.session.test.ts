import { describe, expect, it, vi } from 'vitest';

import { MAX_POPUPS_PER_PARENT, MAX_SURFACES } from './protocol';
import type { CursorImage, Envelope, Positioner, Size, SurfaceNew } from './protocol';
import { SurfaceRegistry } from './registry';
import type { RegistryEvents } from './registry';

/**
 * The registry's session rules: resume reconciliation (v0 §7, contract C4), a fresh session
 * replacing the old one, the cursor watermark (C3), and the popup rules of ADR-0003 §4-§5.
 */

type SurfaceNewOverrides = Partial<
  Pick<SurfaceNew, 'role' | 'parentId' | 'size' | 'title' | 'scale120ths' | 'positioner'>
>;

function surfaceNew(id: number, over: SurfaceNewOverrides = {}): Envelope {
  const msg: SurfaceNew = {
    surfaceId: id,
    role: over.role ?? 0,
    size: over.size ?? { width: 100, height: 80 },
    title: over.title ?? `window ${String(id)}`,
    appId: 'an.app',
    scale120ths: over.scale120ths ?? 120,
  };
  if (over.parentId !== undefined) {
    msg.parentId = over.parentId;
  }
  if (over.positioner !== undefined) {
    msg.positioner = over.positioner;
  }
  return { kind: 'surfaceNew', surfaceNew: msg };
}

function popupNew(id: number, parentId: number, positioner?: Positioner): Envelope {
  return surfaceNew(id, {
    role: 1,
    parentId,
    size: { width: 20, height: 10 },
    positioner: positioner ?? {
      anchorRect: { x: 1, y: 2, width: 0, height: 0 },
      anchor: 5,
      gravity: 8,
      size: { width: 20, height: 10 },
    },
  });
}

function helloReply(resumed: boolean): Envelope {
  return {
    kind: 'helloReply',
    helloReply: {
      protocolVersion: 0,
      sessionId: 'session',
      maxFrameCredits: 4,
      codecs: [1],
      resumed,
    },
  };
}

function cursorImage(serial: number): Envelope {
  const image: CursorImage = {
    serial,
    width: 1,
    height: 1,
    hotspotX: 0,
    hotspotY: 0,
    argbPremultiplied: new Uint8Array(4),
  };
  return { kind: 'cursorImage', cursorImage: image };
}

type Heard = {
  [K in keyof RegistryEvents]: { event: K; value: RegistryEvents[K] };
}[keyof RegistryEvents];

/** Records every registry event in order, payload numbers copied. */
function listen(registry: SurfaceRegistry): Heard[] {
  const heard: Heard[] = [];
  const events: (keyof RegistryEvents)[] = [
    'window-added',
    'window-removed',
    'metadata',
    'cursor-changed',
    'configure-acked',
  ];
  for (const event of events) {
    const record = (value: unknown): void => {
      heard.push({ event, value } as Heard);
    };
    registry.events.on(event, record as never);
  }
  return heard;
}

function names(heard: Heard[]): string[] {
  return heard.map((h) => {
    switch (h.event) {
      case 'window-added':
        return `added ${String(h.value.surface.id)}`;
      case 'window-removed':
        return `removed ${String(h.value.surfaceId)} (${String(h.value.reason)})`;
      case 'metadata':
        return `metadata ${String(h.value.surface.id)}`;
      case 'configure-acked':
        return `acked ${String(h.value.surfaceId)} serial ${String(h.value.serial)}`;
      case 'cursor-changed':
        return h.value.image === null ? 'cursor none' : `cursor ${String(h.value.image.serial)}`;
      default:
        return h.event;
    }
  });
}

/** A session with two toplevels and a popup of the first, then a dropped socket. */
function liveSession(): SurfaceRegistry {
  const registry = new SurfaceRegistry();
  registry.apply(helloReply(false));
  registry.apply(surfaceNew(1));
  registry.apply(surfaceNew(2));
  registry.apply(popupNew(3, 1));
  registry.apply(cursorImage(5));
  return registry;
}

describe('SurfaceRegistry resume reconciliation (C4)', () => {
  it('updates a surface resized and retitled during the grace in place', () => {
    const registry = liveSession();
    const record = registry.get(1);
    const heard = listen(registry);

    registry.apply(helloReply(true));
    registry.apply(surfaceNew(1, { size: { width: 300, height: 200 }, title: 'renamed' }));
    registry.apply(surfaceNew(2));
    registry.apply(popupNew(3, 1));
    registry.apply(cursorImage(6));

    // The same record, updated; never a remove plus an add for an id that survives.
    expect(registry.get(1)).toBe(record);
    expect(registry.get(1)?.title).toBe('renamed');
    expect(registry.get(1)?.size).toEqual({ width: 300, height: 200 });
    expect(registry.list().map((r) => r.id)).toEqual([1, 2, 3]);
    expect(names(heard)).toEqual(['metadata 1', 'acked 1 serial 0', 'cursor 6']);
  });

  it('removes a surface destroyed during the grace, and its popups with it', () => {
    const registry = liveSession();
    const heard = listen(registry);

    registry.apply(helloReply(true));
    registry.apply(surfaceNew(2)); // window 1 (and so its popup 3) is not re-announced
    registry.apply(cursorImage(6));

    expect(registry.list().map((r) => r.id)).toEqual([2]);
    expect(names(heard)).toEqual(['removed 1 (0)', 'removed 3 (1)', 'cursor 6']);
  });

  it('adds a surface created during the grace', () => {
    const registry = liveSession();
    const heard = listen(registry);

    registry.apply(helloReply(true));
    registry.apply(surfaceNew(1));
    registry.apply(surfaceNew(2));
    registry.apply(popupNew(3, 1));
    registry.apply(surfaceNew(9, { title: 'new while away' }));
    registry.apply({ kind: 'cursorGone', cursorGone: {} });

    expect(registry.list().map((r) => r.id)).toEqual([1, 2, 3, 9]);
    expect(names(heard)).toEqual(['added 9', 'cursor none']);
  });

  it('a zero-surface resume removes everything at the cursor message that ends it', () => {
    const registry = liveSession();
    const heard = listen(registry);

    registry.apply(helloReply(true));
    expect(registry.list()).toHaveLength(3); // stale, not gone, until the announcement ends
    registry.apply(cursorImage(6));

    expect(registry.list()).toEqual([]);
    expect(names(heard)).toEqual(['removed 1 (0)', 'removed 3 (1)', 'removed 2 (0)', 'cursor 6']);
  });

  it('reports a changed popup positioner as metadata', () => {
    const registry = liveSession();
    const heard = listen(registry);
    const moved: Positioner = {
      anchorRect: { x: 40, y: 2, width: 0, height: 0 },
      anchor: 5,
      gravity: 8,
      size: { width: 20, height: 10 },
    };

    registry.apply(helloReply(true));
    registry.apply(surfaceNew(1));
    registry.apply(surfaceNew(2));
    registry.apply(popupNew(3, 1, moved));
    registry.apply(cursorImage(6));

    expect(registry.get(3)?.positioner).toEqual(moved);
    expect(names(heard)).toEqual(['metadata 3', 'cursor 6']);
  });

  it('still ignores a repeated SurfaceNew outside a resume, and a second one inside it', () => {
    const registry = liveSession();
    const heard = listen(registry);

    registry.apply(surfaceNew(1, { title: 'replayed' }));
    expect(registry.get(1)?.title).toBe('window 1');

    registry.apply(helloReply(true));
    registry.apply(surfaceNew(1, { title: 'as it stands' }));
    registry.apply(surfaceNew(1, { title: 'replayed again' }));
    registry.apply(surfaceNew(2));
    registry.apply(popupNew(3, 1));
    registry.apply(cursorImage(6));

    expect(registry.get(1)?.title).toBe('as it stands');
    expect(names(heard)).toEqual(['metadata 1', 'cursor 6']);
  });

  it('holds MAX_SURFACES across a resume: a new surface cannot push a live one past it', () => {
    const registry = new SurfaceRegistry();
    registry.apply(helloReply(false));
    for (let id = 1; id <= MAX_SURFACES; id++) {
      registry.apply(surfaceNew(id));
    }

    registry.apply(helloReply(true));
    registry.apply(surfaceNew(1000)); // a new one takes the first slot of the re-announcement
    for (let id = 1; id <= MAX_SURFACES; id++) {
      registry.apply(surfaceNew(id));
    }
    registry.apply(cursorImage(1));

    expect(registry.list()).toHaveLength(MAX_SURFACES);
    expect(registry.get(1000)).toBeDefined();
  });
});

describe('SurfaceRegistry fresh session', () => {
  it('a non-resumed HelloReply removes every surface of the old session', () => {
    const registry = liveSession();
    const heard = listen(registry);

    registry.apply(helloReply(false));

    expect(registry.list()).toEqual([]);
    expect(names(heard)).toEqual([
      'removed 1 (2)',
      'removed 2 (2)',
      'removed 3 (2)',
      'cursor none',
    ]);
  });

  it('restarts the cursor watermark with the session (C3)', () => {
    const registry = liveSession(); // cursor serial 5 drawn
    const changed = vi.fn();
    registry.events.on('cursor-changed', changed);

    registry.apply(cursorImage(2)); // same session: stale
    expect(changed).not.toHaveBeenCalled();

    registry.apply(helloReply(false));
    changed.mockClear();
    registry.apply(cursorImage(1)); // a fresh session counts from its own start

    expect(changed).toHaveBeenCalledTimes(1);
  });

  it('keeps the cursor watermark across a resume', () => {
    const registry = liveSession();
    const changed = vi.fn();
    registry.events.on('cursor-changed', changed);

    registry.apply(helloReply(true));
    registry.apply(cursorImage(5)); // already drawn: ends the announcement, draws nothing

    expect(changed).not.toHaveBeenCalled();
  });
});

describe('SurfaceRegistry popup rules (ADR-0003 §4-§5)', () => {
  it('drops a popup that names no parent', () => {
    const registry = new SurfaceRegistry();
    const added = vi.fn();
    registry.events.on('window-added', added);

    registry.apply(surfaceNew(5, { role: 1, size: { width: 1920, height: 1200 } }));

    expect(registry.get(5)).toBeUndefined();
    expect(added).not.toHaveBeenCalled();
  });

  it('removes popups, and their popups, when their parent goes', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNew(1));
    registry.apply(popupNew(2, 1));
    registry.apply(popupNew(3, 2)); // a popup of the popup
    registry.apply(surfaceNew(4, { parentId: 1 })); // a dialog: its parent is advice only
    const heard = listen(registry);
    const seenDuringRemoval: number[][] = [];
    registry.events.on('window-removed', () => {
      seenDuringRemoval.push(registry.list().map((r) => r.id));
    });

    registry.apply({ kind: 'surfaceGone', surfaceGone: { surfaceId: 1, reason: 0 } });
    // The server's own cascade that follows names ids already gone: ignored.
    registry.apply({ kind: 'surfaceGone', surfaceGone: { surfaceId: 2, reason: 1 } });

    expect(registry.list().map((r) => r.id)).toEqual([4]);
    expect(names(heard)).toEqual(['removed 1 (0)', 'removed 2 (1)', 'removed 3 (1)']);
    // Every listener already saw the final state.
    expect(seenDuringRemoval).toEqual([[4], [4], [4]]);
  });

  it('keeps at most MAX_POPUPS_PER_PARENT popups per parent', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNew(1));
    registry.apply(surfaceNew(2));
    for (let i = 0; i < MAX_POPUPS_PER_PARENT + 5; i++) {
      registry.apply(popupNew(100 + i, 1));
    }
    registry.apply(popupNew(500, 2)); // another parent has its own budget

    const popupsOf = (parent: number): number =>
      registry.list().filter((r) => r.role === 1 && r.parent === parent).length;
    expect(popupsOf(1)).toBe(MAX_POPUPS_PER_PARENT);
    expect(popupsOf(2)).toBe(1);

    // A popup that goes frees its slot.
    registry.apply({ kind: 'surfaceGone', surfaceGone: { surfaceId: 100, reason: 0 } });
    registry.apply(popupNew(999, 1));
    expect(popupsOf(1)).toBe(MAX_POPUPS_PER_PARENT);
    expect(registry.get(999)).toBeDefined();
  });
});

describe('SurfaceRegistry size reporting', () => {
  it('reports a size as it stands with serial 0 on resume, never another size', () => {
    const registry = liveSession();
    const acked: Size[] = [];
    registry.events.on('configure-acked', (e) => {
      if (e.serial === 0) {
        acked.push(e.size);
      }
    });

    registry.apply(helloReply(true));
    registry.apply(surfaceNew(2, { size: { width: 640, height: 480 } }));
    registry.apply(cursorImage(6));

    expect(acked).toEqual([{ width: 640, height: 480 }]);
  });
});
