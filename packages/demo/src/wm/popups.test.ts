import { describe, expect, it } from 'vitest';

import type { Positioner } from '@app-ricot/client';

import { POPUP_MARGIN_PX, popupLocalRect } from './popups';

/** A menu that drops from the parent's top-left corner: a sane anchor/gravity pair. */
const DROPDOWN: Positioner = {
  anchorRect: { x: 8, y: 8, width: 120, height: 24 },
  anchor: 6, // ANCHOR_BOTTOM_LEFT: the anchor point is the rect's bottom-left corner
  // ANCHOR_BOTTOM_RIGHT as a gravity: the popup grows right and down from the anchor point,
  // so its top-left corner lands there (xdg_positioner, v0 §10).
  gravity: 8,
  offset: { x: 0, y: 4 },
  size: { width: 160, height: 200 },
};

const PARENT = { width: 400, height: 300 };

describe('popupLocalRect', () => {
  it('places a dropdown under its anchor', () => {
    const rect = popupLocalRect(DROPDOWN, PARENT);
    expect(rect).toEqual({ x: 8, y: 36, width: 160, height: 200 });
  });

  it('puts an X11 menu where the app put it, not pinned to the corner', () => {
    // What the streamer sends for an override-redirect menu at (5, 5), 80x40: anchor
    // TOP_LEFT of a zero rect at its position, gravity BOTTOM_RIGHT.
    const menu: Positioner = {
      anchorRect: { x: 5, y: 5, width: 0, height: 0 },
      anchor: 5,
      gravity: 8,
      size: { width: 80, height: 40 },
    };
    expect(popupLocalRect(menu, PARENT)).toEqual({ x: 5, y: 5, width: 80, height: 40 });
  });

  it('clamps a hostile popup the size of the screen into the parent neighbourhood', () => {
    const hostile: Positioner = {
      anchorRect: { x: 0, y: 0, width: 0, height: 0 },
      anchor: 0,
      gravity: 0,
      size: { width: 1920, height: 1080 },
    };
    const rect = popupLocalRect(hostile, PARENT);
    // No bigger than the parent plus twice the margin, and its origin sits inside that box
    // (ADR-0003 §5: whatever the server sends, what is shown is parent-sized, not screen-sized).
    expect(rect.width).toBe(PARENT.width + POPUP_MARGIN_PX * 2);
    expect(rect.height).toBe(PARENT.height + POPUP_MARGIN_PX * 2);
    expect(rect.x).toBe(-POPUP_MARGIN_PX);
    expect(rect.y).toBe(-POPUP_MARGIN_PX);
  });

  it('keeps a popup placed far outside next to its parent', () => {
    const far: Positioner = {
      anchorRect: { x: 0, y: 0, width: 0, height: 0 },
      anchor: 0,
      gravity: 0,
      offset: { x: 5000, y: 5000 },
      size: { width: 100, height: 50 },
    };
    const rect = popupLocalRect(far, PARENT);
    expect(rect.x).toBe(PARENT.width + POPUP_MARGIN_PX - rect.width);
    expect(rect.y).toBe(PARENT.height + POPUP_MARGIN_PX - rect.height);
    expect(rect.width).toBe(100);
    expect(rect.height).toBe(50);
  });

  it('never grows a popup the positioner asked small', () => {
    const rect = popupLocalRect(DROPDOWN, PARENT);
    expect(rect.width).toBeLessThanOrEqual(POPUP_MARGIN_PX * 2 + PARENT.width);
    expect(popupLocalRect({ ...DROPDOWN, size: { width: 10, height: 10 } }, PARENT)).toMatchObject({
      width: 10,
      height: 10,
    });
  });
});
