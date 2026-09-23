import { describe, expect, it } from 'vitest';

import {
  DRAG_KEEP_PX,
  MIN_WINDOW,
  TITLEBAR_HEIGHT,
  applyDrag,
  applyResize,
  cascadePosition,
  clampDrag,
  grantResize,
} from './geometry';

const VIEWPORT = { width: 1280, height: 800 };
const SIZE = { width: 400, height: 300 };

describe('clampDrag', () => {
  it('keeps a reasonable position unchanged', () => {
    expect(clampDrag({ x: 100, y: 120 }, SIZE, VIEWPORT)).toEqual({ x: 100, y: 120 });
  });

  it('clamps a drag thousands of pixels off the left onto the exact bound', () => {
    const placed = clampDrag({ x: -5000, y: 0 }, SIZE, VIEWPORT);
    expect(placed.x).toBe(-(SIZE.width - DRAG_KEEP_PX));
    expect(placed.y).toBe(0);
  });

  it('never lets the title bar leave the viewport vertically', () => {
    const below = clampDrag({ x: 0, y: 99999 }, SIZE, VIEWPORT);
    expect(below.y).toBe(VIEWPORT.height - TITLEBAR_HEIGHT);
    const above = clampDrag({ x: 0, y: -99999 }, SIZE, VIEWPORT);
    expect(above.y).toBe(0);
  });

  it('keeps the drag bounds inside a viewport smaller than the window', () => {
    const tiny = { width: 200, height: 100 };
    const placed = clampDrag({ x: 5000, y: 5000 }, SIZE, tiny);
    expect(placed.x).toBe(tiny.width - DRAG_KEEP_PX);
    expect(placed.y).toBe(Math.max(0, tiny.height - TITLEBAR_HEIGHT));
  });
});

describe('applyDrag', () => {
  it('follows the pointer from the grab point', () => {
    const grabOffset = { x: 10, y: 4 };
    expect(applyDrag(grabOffset, { x: 210, y: 104 }, SIZE, VIEWPORT)).toEqual({ x: 200, y: 100 });
  });

  it('clamps the followed pointer like clampDrag', () => {
    const placed = applyDrag({ x: 0, y: 0 }, { x: -9999, y: -9999 }, SIZE, VIEWPORT);
    expect(placed.x).toBe(-(SIZE.width - DRAG_KEEP_PX));
    expect(placed.y).toBe(0);
  });
});

describe('applyResize', () => {
  it('grows with the pointer delta', () => {
    expect(
      applyResize({ width: 400, height: 300 }, { x: 500, y: 500 }, { x: 560, y: 540 }, {
        width: 1920,
        height: 1200,
      }),
    ).toEqual({ width: 460, height: 340 });
  });

  it('never shrinks below the minimum the chrome allows', () => {
    const size = applyResize({ width: 400, height: 300 }, { x: 0, y: 0 }, { x: -5000, y: -5000 }, {
      width: 1920,
      height: 1200,
    });
    expect(size).toEqual({ width: MIN_WINDOW.width, height: MIN_WINDOW.height });
  });

  it('never proposes more than the protocol surface bound', () => {
    const size = applyResize({ width: 400, height: 300 }, { x: 0, y: 0 }, { x: 99999, y: 99999 }, {
      width: 1920,
      height: 1200,
    });
    expect(size).toEqual({ width: 1920, height: 1200 });
  });
});

describe('cascadePosition', () => {
  it('steps diagonally and stays inside the viewport', () => {
    expect(cascadePosition(0, SIZE, VIEWPORT)).toEqual({ x: 24, y: 24 });
    expect(cascadePosition(1, SIZE, VIEWPORT)).toEqual({ x: 52, y: 52 });
  });

  it('clamps a window bigger than the viewport to a grabbable slot', () => {
    const huge = { width: 2000, height: 1600 };
    const placed = cascadePosition(7, huge, VIEWPORT);
    expect(placed.x).toBeGreaterThanOrEqual(-(huge.width - DRAG_KEEP_PX));
    expect(placed.x).toBeLessThanOrEqual(VIEWPORT.width - DRAG_KEEP_PX);
    expect(placed.y).toBeGreaterThanOrEqual(0);
    expect(placed.y).toBeLessThanOrEqual(VIEWPORT.height - TITLEBAR_HEIGHT);
  });
});

describe('grantResize', () => {
  const MAX = { width: 1920, height: 1200 };

  it('grants an ask that fits as asked', () => {
    expect(grantResize({ width: 500, height: 400 }, VIEWPORT, MAX)).toEqual({ width: 500, height: 400 });
  });

  it('clamps to the desktop: the viewport, less the title bar in height', () => {
    expect(grantResize({ width: 5000, height: 5000 }, VIEWPORT, MAX)).toEqual({
      width: VIEWPORT.width,
      height: VIEWPORT.height - TITLEBAR_HEIGHT,
    });
  });

  it('clamps to the protocol bound when the desktop is larger', () => {
    expect(grantResize({ width: 5000, height: 5000 }, { width: 4000, height: 3000 }, MAX)).toEqual(MAX);
  });

  it('clamps to the protocol bound alone while the viewport has no size', () => {
    expect(grantResize({ width: 5000, height: 900 }, { width: 0, height: 0 }, MAX)).toEqual({
      width: 1920,
      height: 900,
    });
  });

  it('grants nothing for an ask with no area', () => {
    expect(grantResize({ width: 0, height: 400 }, VIEWPORT, MAX)).toBeNull();
    expect(grantResize({ width: 400, height: 0 }, VIEWPORT, MAX)).toBeNull();
  });
});
