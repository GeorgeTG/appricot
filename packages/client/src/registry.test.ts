import { describe, expect, it, vi } from 'vitest';

import { MAX_SURFACES } from './protocol';
import type {
  CursorImage,
  Envelope,
  Positioner,
  Rect,
  Role,
  Size,
  SurfaceGone,
  SurfaceNew,
} from './protocol';
import { SurfaceRegistry, clampPopup, placePopup } from './registry';
import type { SurfaceRecord } from './registry';

/** A toplevel SurfaceNew envelope with overridable pieces. */
function surfaceNew(
  id: number,
  over: { role?: Role; parentId?: number; size?: Size; title?: string } = {},
): Envelope {
  const msg: SurfaceNew = {
    surfaceId: id,
    role: 0,
    size: over.size ?? { width: 100, height: 80 },
    title: over.title ?? 'a title',
    appId: 'an.app',
    scale120ths: 120,
  };
  if (over.parentId !== undefined) {
    msg.parentId = over.parentId;
  }
  if (over.role !== undefined) {
    msg.role = over.role;
  }
  return { kind: 'surfaceNew', surfaceNew: msg };
}

function surfaceGone(id: number, reason: SurfaceGone['reason']): Envelope {
  return { kind: 'surfaceGone', surfaceGone: { surfaceId: id, reason } };
}

function cursorImage(serial: number): CursorImage {
  return {
    serial,
    width: 1,
    height: 1,
    hotspotX: 0,
    hotspotY: 0,
    argbPremultiplied: new Uint8Array(4),
  };
}

describe('SurfaceRegistry', () => {
  it('tracks a surface from SurfaceNew and forgets it on SurfaceGone', () => {
    const registry = new SurfaceRegistry();
    const added = vi.fn();
    const removed = vi.fn();
    registry.events.on('window-added', added);
    registry.events.on('window-removed', removed);

    registry.apply(surfaceNew(7));
    const record = registry.get(7);
    expect(record).toMatchObject({
      id: 7,
      role: 0,
      parent: undefined,
      title: 'a title',
      appId: 'an.app',
      scale: 120,
      size: { width: 100, height: 80 },
      lastSequence: 0,
    });
    expect(added).toHaveBeenCalledTimes(1);
    expect(registry.list()).toHaveLength(1);

    registry.apply(surfaceGone(7, 0));
    expect(registry.get(7)).toBeUndefined();
    expect(registry.list()).toHaveLength(0);
    expect(removed).toHaveBeenCalledWith({ surfaceId: 7, reason: 0 });
  });

  it('ignores a SurfaceNew whose id already lives', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNew(1, { title: 'first' }));

    registry.apply(surfaceNew(1, { title: 'replayed' }));

    expect(registry.list()).toHaveLength(1);
    expect(registry.get(1)?.title).toBe('first');
  });

  it('ignores a SurfaceGone for an unknown surface', () => {
    const registry = new SurfaceRegistry();
    const removed = vi.fn();
    registry.events.on('window-removed', removed);

    registry.apply(surfaceGone(99, 2));

    expect(removed).not.toHaveBeenCalled();
  });

  it('drops a popup whose parent is not tracked (ADR-0003 section 5)', () => {
    const registry = new SurfaceRegistry();
    const added = vi.fn();
    registry.events.on('window-added', added);

    registry.apply(surfaceNew(5, { role: 1, parentId: 404 }));

    expect(added).not.toHaveBeenCalled();
    expect(registry.get(5)).toBeUndefined();
  });

  it('keeps a popup whose parent lives, with the positioner on the record', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNew(1));
    const positioner: Positioner = {
      anchorRect: { x: 4, y: 6, width: 10, height: 8 },
      anchor: 8,
      gravity: 6,
      offset: { x: 0, y: 0 },
      size: { width: 20, height: 12 },
    };

    registry.apply({ kind: 'surfaceNew', surfaceNew: {
      surfaceId: 2,
      role: 1,
      parentId: 1,
      size: { width: 20, height: 12 },
      title: '',
      appId: 'an.app',
      scale120ths: 120,
      positioner,
    } });

    const record = registry.get(2);
    expect(record?.parent).toBe(1);
    expect(record?.positioner).toBe(positioner);
  });

  it('caps the session at MAX_SURFACES surfaces however many SurfaceNew arrive', () => {
    const registry = new SurfaceRegistry();
    for (let id = 1; id <= MAX_SURFACES; id++) {
      registry.apply(surfaceNew(id));
    }
    expect(registry.list()).toHaveLength(MAX_SURFACES);

    registry.apply(surfaceNew(MAX_SURFACES + 1));
    expect(registry.list()).toHaveLength(MAX_SURFACES);
    expect(registry.get(MAX_SURFACES + 1)).toBeUndefined();
  });

  it('applies only the fields a SurfaceMetadata carries', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNew(3));
    const metadata = vi.fn();
    registry.events.on('metadata', metadata);

    registry.apply({ kind: 'surfaceMetadata', surfaceMetadata: { surfaceId: 3, title: 'renamed' } });
    let record = registry.get(3);
    expect(record?.title).toBe('renamed');
    expect(record?.appId).toBe('an.app');
    expect(record?.scale).toBe(120);

    registry.apply({
      kind: 'surfaceMetadata',
      surfaceMetadata: { surfaceId: 3, appId: 'other.app', scale120ths: 240 },
    });
    record = registry.get(3);
    expect(record?.appId).toBe('other.app');
    expect(record?.scale).toBe(240);
    expect(metadata).toHaveBeenCalledTimes(2);
  });

  it('updates the size from ConfigureAck and echoes it as configure-acked', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNew(4, { size: { width: 100, height: 80 } }));
    const acked = vi.fn();
    registry.events.on('configure-acked', acked);

    registry.apply({
      kind: 'configureAck',
      configureAck: { surfaceId: 4, serial: 9, size: { width: 640, height: 480 } },
    });

    expect(registry.get(4)?.size).toEqual({ width: 640, height: 480 });
    expect(acked).toHaveBeenCalledWith({
      surfaceId: 4,
      serial: 9,
      size: { width: 640, height: 480 },
    });
  });

  it('keeps lastSequence at the highest sequence seen, never backwards', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNew(6));

    registry.apply({ kind: 'frame', frame: { surfaceId: 6, sequence: 2, fullRedraw: false, tiles: [] } });
    registry.apply({ kind: 'frame', frame: { surfaceId: 6, sequence: 1, fullRedraw: false, tiles: [] } });
    registry.apply({ kind: 'frame', frame: { surfaceId: 6, sequence: 5, fullRedraw: true, tiles: [] } });

    expect(registry.get(6)?.lastSequence).toBe(5);
  });

  it('drops a cursor image whose serial was already drawn, keeps monotonic ones', () => {
    const registry = new SurfaceRegistry();
    const changed = vi.fn();
    registry.events.on('cursor-changed', changed);

    registry.apply({ kind: 'cursorImage', cursorImage: cursorImage(5) });
    registry.apply({ kind: 'cursorImage', cursorImage: cursorImage(3) });
    registry.apply({ kind: 'cursorImage', cursorImage: cursorImage(5) });
    registry.apply({ kind: 'cursorImage', cursorImage: cursorImage(6) });

    expect(changed).toHaveBeenCalledTimes(2);
    expect(changed).toHaveBeenLastCalledWith({ image: cursorImage(6) });
  });

  it('reports CursorGone as a null cursor', () => {
    const registry = new SurfaceRegistry();
    const changed = vi.fn();
    registry.events.on('cursor-changed', changed);
    registry.apply({ kind: 'cursorImage', cursorImage: cursorImage(1) });

    registry.apply({ kind: 'cursorGone', cursorGone: {} });

    expect(changed).toHaveBeenLastCalledWith({ image: null });
  });

  it('surfaces focus, resize and clipboard asks as host decisions', () => {
    const registry = new SurfaceRegistry();
    const focusAsk = vi.fn();
    const resizeAsk = vi.fn();
    const clipboardAsk = vi.fn();
    registry.events.on('focus-ask', focusAsk);
    registry.events.on('resize-ask', resizeAsk);
    registry.events.on('clipboard-ask', clipboardAsk);

    registry.apply({ kind: 'focusAsk', focusAsk: { surfaceId: 2 } });
    registry.apply({
      kind: 'resizeAsk',
      resizeAsk: { surfaceId: 2, size: { width: 30, height: 20 } },
    });
    registry.apply({ kind: 'clipboardAsk', clipboardAsk: {} });

    expect(focusAsk).toHaveBeenCalledWith({ surfaceId: 2 });
    expect(resizeAsk).toHaveBeenCalledWith({ surfaceId: 2, size: { width: 30, height: 20 } });
    expect(clipboardAsk).toHaveBeenCalledWith(undefined);
  });
});

describe('placePopup', () => {
  const anchorRect: Rect = { x: 100, y: 50, width: 40, height: 20 };

  it('hangs a bottom-right-anchored popup below and right of the anchor rect', () => {
    const placed = placePopup({
      anchorRect,
      anchor: 8, // ANCHOR_BOTTOM_RIGHT of the parent rect: (140, 70)
      gravity: 6, // ANCHOR_BOTTOM_LEFT of the popup: its bottom-left corner goes there
      offset: { x: 2, y: 3 },
      size: { width: 10, height: 6 },
    });
    expect(placed).toEqual({ x: 142, y: 67, width: 10, height: 6 });
  });

  it('centers a popup on the anchor center by default', () => {
    const placed = placePopup({
      anchorRect,
      anchor: 0,
      gravity: 0,
      offset: { x: 0, y: 0 },
      size: { width: 20, height: 20 },
    });
    expect(placed).toEqual({ x: 110, y: 50, width: 20, height: 20 });
  });

  it('grows upward when gravity is the popup top', () => {
    const placed = placePopup({
      anchorRect: { x: 0, y: 100, width: 50, height: 10 },
      anchor: 1, // ANCHOR_TOP of the anchor rect: (25, 100)
      gravity: 1, // the popup's top-center sits there... its own top point is at its top edge
      offset: { x: 0, y: 0 },
      size: { width: 10, height: 4 },
    });
    // anchor point (25, 100); the popup's TOP-center is (5, 0) of a 10x4 popup, so the
    // popup hangs below. To hang above, gravity ANCHOR_BOTTOM (2) puts its bottom-center
    // at the anchor point; both directions are covered by this pair of assertions.
    expect(placed).toEqual({ x: 20, y: 100, width: 10, height: 4 });

    const above = placePopup({
      anchorRect: { x: 0, y: 100, width: 50, height: 10 },
      anchor: 1,
      gravity: 2, // popup bottom-center at (25, 100): the popup grows upward
      offset: { x: 0, y: 0 },
      size: { width: 10, height: 4 },
    });
    expect(above).toEqual({ x: 20, y: 96, width: 10, height: 4 });
  });
});

describe('clampPopup', () => {
  const parent: Rect = { x: 0, y: 0, width: 800, height: 600 };

  it('leaves a popup alone when it already fits the parent box plus margin', () => {
    const placed: Rect = { x: 100, y: 120, width: 90, height: 40 };
    expect(clampPopup(placed, parent)).toEqual(placed);
  });

  it('cannot place a popup the size of the screen outside the box', () => {
    // A hostile server asks for a 1920x1200 popup at (-5000, -5000). The result must be
    // small enough to fit the parent's neighbourhood and must sit inside it (ADR-0003 s5).
    const clamped = clampPopup(
      { x: -5000, y: -5000, width: 1920, height: 1200 },
      parent,
    );
    expect(clamped.width).toBe(800 + 64);
    expect(clamped.height).toBe(600 + 64);
    expect(clamped.x).toBe(-32);
    expect(clamped.y).toBe(-32);
    // It overlaps the parent box rather than hanging off it.
    expect(clamped.x + clamped.width).toBeGreaterThan(parent.x);
    expect(clamped.y + clamped.height).toBeGreaterThan(parent.y);
  });

  it('pulls a popup placed far past the right edge back inside the margin', () => {
    const clamped = clampPopup({ x: 10000, y: 10000, width: 100, height: 50 }, parent);
    expect(clamped).toEqual({ x: 800 + 32 - 100, y: 600 + 32 - 50, width: 100, height: 50 });
  });

  it('allows at most the margin outside each edge, negative positions included', () => {
    expect(clampPopup({ x: -40, y: -40, width: 10, height: 10 }, parent).x).toBe(-32);
    // x past the right margin clamps to 800+32-10; y is already inside its margin and stays.
    expect(clampPopup({ x: 836, y: 610, width: 10, height: 10 }, parent)).toEqual({
      x: 822,
      y: 610,
      width: 10,
      height: 10,
    });
  });

  it('honours margin 0: the popup cannot leave the parent box at all', () => {
    const clamped = clampPopup({ x: -5, y: 700, width: 20, height: 20 }, parent, 0);
    expect(clamped).toEqual({ x: 0, y: 580, width: 20, height: 20 });
  });

  it('never grows a popup, and never crashes on a zero size', () => {
    const clamped = clampPopup({ x: 10, y: 10, width: 0, height: 0 }, parent);
    expect(clamped).toEqual({ x: 10, y: 10, width: 0, height: 0 });
    expect(clampPopup({ x: 0, y: 0, width: 4, height: 4 }, parent).width).toBe(4);
  });
});

describe('SurfaceRecord shape', () => {
  it('is the shape the host was promised', () => {
    const registry = new SurfaceRegistry();
    registry.apply(surfaceNew(11));
    const record: SurfaceRecord | undefined = registry.get(11);
    expect(Object.keys(record ?? {}).sort()).toEqual(
      [
        'appId',
        'id',
        'lastSequence',
        'parent',
        'positioner',
        'role',
        'scale',
        'size',
        'title',
      ].sort(),
    );
  });
});
