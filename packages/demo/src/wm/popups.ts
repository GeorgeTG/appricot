/**
 * Popup geometry for the demo host: where a popup layer sits inside its parent window.
 *
 * ADR-0003 §5: a popup is clamped to its parent's content box plus a small fixed margin, and
 * its size is capped, so a hostile server cannot ask for a popup the size of the screen and
 * cannot place one outside the parent's neighbourhood. The clamping math itself lives in
 * `@appricot/client` (`placePopup`, `clampPopup`); this module is the thin adapter that feeds
 * it the parent's content box in surface-local coordinates and adds nothing of its own.
 */
import { clampPopup, placePopup } from '@appricot/client';
import type { Positioner, Rect } from '@appricot/client';
import type { Size } from './geometry';

/** The client's default margin, restated so the demo's layer geometry and the clamp agree. */
export const POPUP_MARGIN_PX = 32;

/**
 * A popup's rectangle, in the PARENT's surface-local coordinates: the positioner is placed,
 * then clamped into the parent's content box grown by `POPUP_MARGIN_PX` on every side.
 *
 * The caller paints this inside a layer that extends exactly that margin past the parent's
 * content box and clips at its edges, so what the user can see is what the clamp allowed —
 * a hostile popup larger than the parent's neighbourhood is clipped by construction.
 */
export function popupLocalRect(positioner: Positioner, parentContentSize: Size): Rect {
  return clampPopup(
    placePopup(positioner),
    { x: 0, y: 0, width: parentContentSize.width, height: parentContentSize.height },
    POPUP_MARGIN_PX,
  );
}
