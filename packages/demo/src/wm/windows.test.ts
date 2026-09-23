import { describe, expect, it } from 'vitest';

import { WindowManager } from './windows';

const VIEWPORT = { width: 1280, height: 800 };

function wmWithThree(): WindowManager {
  const wm = new WindowManager();
  wm.addToplevel({ surfaceId: 1, appId: 'a', title: 'one', size: { width: 300, height: 200 } }, VIEWPORT);
  wm.addToplevel({ surfaceId: 2, appId: 'b', title: 'two', size: { width: 300, height: 200 } }, VIEWPORT);
  wm.addToplevel({ surfaceId: 3, appId: 'c', title: 'three', size: { width: 300, height: 200 } }, VIEWPORT);
  return wm;
}

describe('WindowManager add and order', () => {
  it('stacks in arrival order, bottom to top', () => {
    expect(wmWithThree().order()).toEqual([1, 2, 3]);
  });

  it('ignores a repeated surface id (ids are never reused; a repeat is a replay)', () => {
    const wm = new WindowManager();
    wm.addToplevel({ surfaceId: 7, appId: 'a', title: 'one', size: { width: 100, height: 80 } }, VIEWPORT);
    expect(wm.addToplevel({ surfaceId: 7, appId: 'a', title: 'again', size: { width: 1, height: 1 } }, VIEWPORT)).toBeUndefined();
    expect(wm.order()).toEqual([7]);
    expect(wm.get(7)?.title).toBe('one');
  });
});

describe('WindowManager focus and z-order', () => {
  it('focusing a bottom window moves it to the top of the stack', () => {
    const wm = wmWithThree();
    expect(wm.focus(1)).toBe(3);
    expect(wm.order()).toEqual([2, 3, 1]);
    expect(wm.focusedId).toBe(1);
  });

  it('re-focusing the top window keeps the order and updates focus', () => {
    const wm = wmWithThree();
    wm.focus(3);
    expect(wm.order()).toEqual([1, 2, 3]);
    expect(wm.focusedId).toBe(3);
  });

  it('focusing an unknown id changes nothing', () => {
    const wm = wmWithThree();
    expect(wm.focus(99)).toBeUndefined();
    expect(wm.order()).toEqual([1, 2, 3]);
  });

  it('blur clears focus', () => {
    const wm = wmWithThree();
    wm.focus(2);
    wm.blur();
    expect(wm.focusedId).toBeNull();
  });
});

describe('WindowManager remove', () => {
  it('removing the focused top window refocuses the new top', () => {
    const wm = wmWithThree();
    wm.focus(3);
    const removal = wm.removeToplevel(3);
    expect(removal?.removed.surfaceId).toBe(3);
    expect(removal?.refocus).toBe(2);
    expect(wm.focusedId).toBe(2);
    expect(wm.order()).toEqual([1, 2]);
  });

  it('removing a non-focused window leaves focus alone', () => {
    const wm = wmWithThree();
    wm.focus(3);
    const removal = wm.removeToplevel(1);
    expect(removal?.refocus).toBe(3);
    expect(wm.focusedId).toBe(3);
  });

  it('removing the last window reports null refocus (focus left every surface)', () => {
    const wm = new WindowManager();
    wm.addToplevel({ surfaceId: 5, appId: 'a', title: 'solo', size: { width: 100, height: 80 } }, VIEWPORT);
    wm.focus(5);
    expect(wm.removeToplevel(5)?.refocus).toBeNull();
    expect(wm.focusedId).toBeNull();
  });

  it('removing an unknown id reports undefined', () => {
    expect(wmWithThree().removeToplevel(42)).toBeUndefined();
  });
});

describe('WindowManager popups', () => {
  it('tracks popups per parent and cascades them on parent removal', () => {
    const wm = wmWithThree();
    wm.addPopup(11, 1);
    wm.addPopup(12, 1);
    wm.addPopup(21, 2);
    expect(wm.popupsOf(1)).toEqual([11, 12]);
    const removal = wm.removeToplevel(1);
    expect(removal?.popups).toEqual([11, 12]);
    expect(wm.popupsOf(1)).toEqual([]);
    expect(wm.popupsOf(2)).toEqual([21]);
  });

  it('drops a popup whose parent is not a tracked window', () => {
    const wm = wmWithThree();
    wm.addPopup(31, 99);
    expect(wm.popupsOf(99)).toEqual([]);
  });

  it('removePopup forgetts exactly one popup', () => {
    const wm = wmWithThree();
    wm.addPopup(11, 1);
    expect(wm.removePopup(11, 1)).toBe(true);
    expect(wm.removePopup(11, 1)).toBe(false);
    expect(wm.popupsOf(1)).toEqual([]);
  });
});

describe('WindowManager minimize', () => {
  it('takes the window out of the visible order but keeps it tracked', () => {
    const wm = wmWithThree();
    wm.focus(3);
    expect(wm.minimize(3)).toMatchObject({ minimized: { surfaceId: 3 } });
    expect(wm.order()).toEqual([1, 2]);
    expect(wm.get(3)).toMatchObject({ title: 'three' });
    expect(wm.minimizedIds()).toEqual([3]);
    expect(wm.isMinimized(3)).toBe(true);
  });

  it('minimizing the focused window moves focus to the top-most remaining window', () => {
    const wm = wmWithThree();
    wm.focus(3);
    expect(wm.minimize(3)?.refocus).toBe(2);
    expect(wm.focusedId).toBe(2);
  });

  it('minimizing the only visible window reports null refocus (the caller sends BlurRelease)', () => {
    const wm = new WindowManager();
    wm.addToplevel({ surfaceId: 5, appId: 'a', title: 'solo', size: { width: 100, height: 80 } }, VIEWPORT);
    wm.focus(5);
    expect(wm.minimize(5)?.refocus).toBeNull();
    expect(wm.focusedId).toBeNull();
  });

  it('minimizing a window while the rest are already minimized also reports null refocus', () => {
    const wm = wmWithThree();
    wm.focus(3);
    wm.minimize(1);
    wm.minimize(2);
    // 3 is the last visible window; the other two exist but are hidden and cannot take focus.
    expect(wm.minimize(3)?.refocus).toBeNull();
    expect(wm.focusedId).toBeNull();
  });

  it('minimizing a non-focused window leaves focus alone', () => {
    const wm = wmWithThree();
    wm.focus(3);
    const result = wm.minimize(1);
    expect(result?.refocus).toBe(3);
    expect(wm.focusedId).toBe(3);
  });

  it('reports the popups that hide with the parent', () => {
    const wm = wmWithThree();
    wm.addPopup(11, 1);
    expect(wm.minimize(1)?.popups).toEqual([11]);
    // Still parented while the parent is hidden: restore brings them back with it.
    expect(wm.popupsOf(1)).toEqual([11]);
  });

  it('ignores unknown ids and double minimization', () => {
    const wm = wmWithThree();
    expect(wm.minimize(42)).toBeUndefined();
    wm.minimize(1);
    expect(wm.minimize(1)).toBeUndefined();
    expect(wm.minimizedIds()).toEqual([1]);
  });

  it('lists minimized windows in minimize order', () => {
    const wm = wmWithThree();
    wm.minimize(3);
    wm.minimize(1);
    expect(wm.minimizedIds()).toEqual([3, 1]);
  });

  it('focus cannot resurrect a minimized window', () => {
    const wm = wmWithThree();
    wm.minimize(2);
    expect(wm.focus(2)).toBeUndefined();
    expect(wm.order()).toEqual([1, 3]);
    expect(wm.focusedId).toBeNull();
    expect(wm.isMinimized(2)).toBe(true);
  });
});

describe('WindowManager restore', () => {
  it('returns a minimized window to the top, focuses it, and preserves everything else', () => {
    const wm = wmWithThree();
    wm.focus(3);
    wm.move(2, 321, 234);
    wm.minimize(2);
    const zIndex = wm.restore(2);
    expect(zIndex).toBe(3);
    expect(wm.order()).toEqual([1, 3, 2]);
    expect(wm.focusedId).toBe(2);
    expect(wm.get(2)).toMatchObject({ x: 321, y: 234, title: 'two' });
    expect(wm.minimizedIds()).toEqual([]);
    expect(wm.isMinimized(2)).toBe(false);
  });

  it('restore is refused for windows that are not minimized (unknown included)', () => {
    const wm = wmWithThree();
    expect(wm.restore(1)).toBeUndefined();
    expect(wm.restore(42)).toBeUndefined();
  });

  it('a popup parented to a minimized window rides along on restore', () => {
    const wm = wmWithThree();
    wm.addPopup(11, 1);
    wm.minimize(1);
    wm.restore(1);
    expect(wm.popupsOf(1)).toEqual([11]);
  });

  it('a window removed while minimized is gone for good', () => {
    const wm = wmWithThree();
    wm.minimize(2);
    expect(wm.removeToplevel(2)?.removed.surfaceId).toBe(2);
    expect(wm.restore(2)).toBeUndefined();
    expect(wm.get(2)).toBeUndefined();
  });

  it('removing the last visible window while another is minimized reports null refocus', () => {
    const wm = new WindowManager();
    wm.addToplevel({ surfaceId: 1, appId: 'a', title: 'one', size: { width: 100, height: 80 } }, VIEWPORT);
    wm.addToplevel({ surfaceId: 2, appId: 'b', title: 'two', size: { width: 100, height: 80 } }, VIEWPORT);
    wm.minimize(1);
    wm.focus(2);
    // Only the minimized 1 survives; a hidden window cannot take focus.
    expect(wm.removeToplevel(2)?.refocus).toBeNull();
    expect(wm.focusedId).toBeNull();
  });
});

describe('WindowManager move and resize', () => {
  it('move updates the position of a known window only', () => {
    const wm = wmWithThree();
    wm.move(1, 321, 234);
    expect(wm.get(1)).toMatchObject({ x: 321, y: 234 });
    wm.move(99, 1, 1);
    expect(wm.get(99)).toBeUndefined();
  });

  it('resize replaces the size copy, not the caller object', () => {
    const wm = wmWithThree();
    const size = { width: 500, height: 400 };
    wm.resize(2, size);
    size.width = 1;
    expect(wm.get(2)?.size).toEqual({ width: 500, height: 400 });
  });
});

describe('WindowManager metadata', () => {
  it('setMetadata replaces the title and app id the wm reports', () => {
    const wm = wmWithThree();
    wm.minimize(2);
    wm.setMetadata(2, { title: '<b>renamed</b>', appId: 'b2' });
    expect(wm.get(2)).toMatchObject({ title: '<b>renamed</b>', appId: 'b2' });
    // Unknown ids are ignored.
    wm.setMetadata(99, { title: 'x', appId: 'y' });
    expect(wm.get(99)).toBeUndefined();
  });
});
