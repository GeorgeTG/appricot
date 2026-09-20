/**
 * Pure geometry for the demo's floating windows: drag clamping, resize math and the cascade
 * that places new toplevels. No DOM, no state — every function takes numbers in (logical
 * pixels, page coordinates) and returns numbers, so the tests can pin the math down.
 */

/** A point or a size, in logical pixels. */
export interface Point {
  x: number;
  y: number;
}

export interface Size {
  width: number;
  height: number;
}

/** The smallest window the chrome allows a resize grip to produce. */
export const MIN_WINDOW: Size = { width: 120, height: 80 };

/** How much of the title bar must stay reachable inside the viewport while dragging. */
export const DRAG_KEEP_PX = 48;

/** The demo title bar's height; a window may not be dragged below the viewport's bottom by
 * more than this, so it can always be grabbed again. */
export const TITLEBAR_HEIGHT = 28;

/** Where the first window lands, and the step between cascaded windows. */
const CASCADE_ORIGIN = 24;
const CASCADE_STEP = 28;

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), Math.max(min, max));
}

/**
 * Clamps a window position so the window stays grabbable: at least `DRAG_KEEP_PX` pixels of
 * its width are inside the viewport horizontally, and its title bar is fully inside it
 * vertically. A drag that runs thousands of pixels off-screen lands exactly on the bound.
 */
export function clampDrag(position: Point, size: Size, viewport: Size): Point {
  return {
    x: clamp(position.x, -(size.width - DRAG_KEEP_PX), viewport.width - DRAG_KEEP_PX),
    y: clamp(position.y, 0, viewport.height - TITLEBAR_HEIGHT),
  };
}

/**
 * One drag step: the window's origin follows the pointer from where the grab started.
 * `grabOffset` is pointer minus window-origin at pointerdown; the result is clamped.
 */
export function applyDrag(
  grabOffset: Point,
  pointer: Point,
  size: Size,
  viewport: Size,
): Point {
  return clampDrag({ x: pointer.x - grabOffset.x, y: pointer.y - grabOffset.y }, size, viewport);
}

/**
 * One resize step from the bottom-right grip: the size follows the pointer delta from where
 * the grip was grabbed, clamped between `min` and `max`. The max is the protocol's surface
 * bound (MAX_SURFACE_WIDTH/HEIGHT), so the host never proposes an impossible window.
 */
export function applyResize(
  startSize: Size,
  grabStart: Point,
  pointer: Point,
  max: Size,
  min: Size = MIN_WINDOW,
): Size {
  return {
    width: clamp(startSize.width + (pointer.x - grabStart.x), min.width, max.width),
    height: clamp(startSize.height + (pointer.y - grabStart.y), min.height, max.height),
  };
}

/**
 * The position for the n-th window of a cascade: diagonally offset, wrapped before it runs
 * off the viewport, and clamped so the title bar stays inside. Pure bookkeeping — the wm
 * passes the count of windows already placed.
 */
export function cascadePosition(index: number, size: Size, viewport: Size): Point {
  const step = CASCADE_STEP;
  const rawX = CASCADE_ORIGIN + (index % 8) * step;
  const rawY = CASCADE_ORIGIN + (index % 8) * step;
  return clampDrag({ x: rawX, y: rawY }, size, viewport);
}
