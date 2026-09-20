// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest';

import type { Envelope } from './protocol';
import { attachInput, keysymFor, modifiersFor } from './input';

/**
 * One attached surface with a fixed on-screen box: the element covers (100, 50) to
 * (180, 90). Coordinates are asserted surface-local and clamped to the box, never global
 * (rule 7, docs/protocol/README.md).
 */
function makeAttached() {
  const element = document.createElement('div');
  const box = {
    x: 100,
    y: 50,
    width: 80,
    height: 40,
    top: 50,
    left: 100,
    right: 180,
    bottom: 90,
    toJSON: () => box,
  };
  vi.spyOn(element, 'getBoundingClientRect').mockReturnValue(box as DOMRect);
  const sent: Envelope[] = [];
  let focused = true;
  const detach = attachInput(element, 7, {
    conn: { send: (e) => sent.push(e) },
    isFocused: () => focused,
  });
  return {
    element,
    sent,
    detach,
    unfocus: () => {
      focused = false;
    },
  };
}

describe('attachInput pointer', () => {
  it('maps pointer position to surface-local coordinates', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(new MouseEvent('pointermove', { clientX: 140, clientY: 70 }));

    expect(sent).toEqual([
      { kind: 'pointerMove', pointerMove: { surfaceId: 7, x: 40, y: 20 } },
    ]);
  });

  it('clamps positions outside the surface box into it, on every edge', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(new MouseEvent('pointermove', { clientX: 999, clientY: 999 }));
    element.dispatchEvent(new MouseEvent('pointermove', { clientX: 0, clientY: 0 }));

    expect(sent).toEqual([
      { kind: 'pointerMove', pointerMove: { surfaceId: 7, x: 80, y: 40 } },
      { kind: 'pointerMove', pointerMove: { surfaceId: 7, x: 0, y: 0 } },
    ]);
  });

  it('maps browser buttons 0/1/2 to X buttons 1/2/3 and sees the up', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(new MouseEvent('pointerdown', { button: 0, clientX: 110, clientY: 60 }));
    element.dispatchEvent(new MouseEvent('pointerup', { button: 0 }));
    element.dispatchEvent(new MouseEvent('pointerdown', { button: 1 }));
    element.dispatchEvent(new MouseEvent('pointerup', { button: 1 }));
    element.dispatchEvent(new MouseEvent('pointerdown', { button: 2 }));
    element.dispatchEvent(new MouseEvent('pointerup', { button: 2 }));

    expect(sent).toEqual([
      { kind: 'pointerButton', pointerButton: { surfaceId: 7, button: 1, pressed: true } },
      { kind: 'pointerButton', pointerButton: { surfaceId: 7, button: 1, pressed: false } },
      { kind: 'pointerButton', pointerButton: { surfaceId: 7, button: 2, pressed: true } },
      { kind: 'pointerButton', pointerButton: { surfaceId: 7, button: 2, pressed: false } },
      { kind: 'pointerButton', pointerButton: { surfaceId: 7, button: 3, pressed: true } },
      { kind: 'pointerButton', pointerButton: { surfaceId: 7, button: 3, pressed: false } },
    ]);
  });

  it('ignores buttons with no X number (back, forward, ...)', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(new MouseEvent('pointerdown', { button: 4 }));

    expect(sent).toEqual([]);
  });

  it('turns the wheel into discrete steps and keeps the page from scrolling', () => {
    const { element, sent } = makeAttached();

    const down = new WheelEvent('wheel', { deltaY: 100, cancelable: true });
    element.dispatchEvent(down);
    const up = new WheelEvent('wheel', { deltaY: -100, cancelable: true });
    element.dispatchEvent(up);
    const sideways = new WheelEvent('wheel', { deltaX: 53.5, cancelable: true });
    element.dispatchEvent(sideways);

    expect(sent).toEqual([
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 0, stepsY: 1 } },
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 0, stepsY: -1 } },
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 1, stepsY: 0 } },
    ]);
    expect(down.defaultPrevented).toBe(true);
    expect(up.defaultPrevented).toBe(true);
    expect(sideways.defaultPrevented).toBe(true);
  });
});

describe('attachInput keyboard', () => {
  it('sends a key press and release with the keysym and the physical code', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'a', code: 'KeyA', cancelable: true }),
    );
    element.dispatchEvent(new KeyboardEvent('keyup', { key: 'a', code: 'KeyA' }));

    expect(sent).toEqual([
      { kind: 'key', key: { keysym: 0x61, code: 'KeyA', pressed: true, modifiers: 0 } },
      { kind: 'key', key: { keysym: 0x61, code: 'KeyA', pressed: false, modifiers: 0 } },
    ]);
  });

  it('sends the shifted keysym with the shift bit set', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'A', code: 'KeyA', shiftKey: true, cancelable: true }),
    );

    expect(sent).toEqual([
      { kind: 'key', key: { keysym: 0x41, code: 'KeyA', pressed: true, modifiers: 1 } },
    ]);
  });

  it('sends nothing for keys while the surface is not focused; the pointer still flows', () => {
    const { element, sent, unfocus } = makeAttached();

    element.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'a', code: 'KeyA', cancelable: true }),
    );
    unfocus();
    element.dispatchEvent(new KeyboardEvent('keydown', { key: 'b', code: 'KeyB' }));
    element.dispatchEvent(new MouseEvent('pointermove', { clientX: 120, clientY: 60 }));

    expect(sent).toEqual([
      { kind: 'key', key: { keysym: 0x61, code: 'KeyA', pressed: true, modifiers: 0 } },
      { kind: 'pointerMove', pointerMove: { surfaceId: 7, x: 20, y: 10 } },
    ]);
  });

  it("swallows the browser's own key repeats", () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'a', code: 'KeyA', repeat: true, cancelable: true }),
    );

    expect(sent).toEqual([]);
  });


  it('releases everything on blur', () => {
    const { sent } = makeAttached();

    window.dispatchEvent(new Event('blur'));

    expect(sent).toEqual([{ kind: 'blurRelease', blurRelease: {} }]);
  });

  it('stops sending once detached', () => {
    const { element, sent, detach } = makeAttached();
    detach();

    element.dispatchEvent(new MouseEvent('pointermove', { clientX: 120, clientY: 60 }));
    element.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', code: 'KeyA' }));

    expect(sent).toEqual([]);
  });
});

describe('keysymFor', () => {
  it('maps a printable key to its codepoint', () => {
    expect(keysymFor({ key: 'a', code: 'KeyA' })).toBe(0x61);
    expect(keysymFor({ key: ' ', code: 'Space' })).toBe(0x20);
  });

  it('maps the Greek block straight through: X keysyms 0x0370-0x03ff equal the codepoints', () => {
    expect(keysymFor({ key: 'α', code: 'KeyA' })).toBe(0x03b1);
    expect(keysymFor({ key: 'Ω', code: 'KeyW' })).toBe(0x03a9);
  });

  it('names the keys a single character cannot', () => {
    expect(keysymFor({ key: 'Enter', code: 'Enter' })).toBe(0xff0d);
    expect(keysymFor({ key: 'Tab', code: 'Tab' })).toBe(0xff09);
    expect(keysymFor({ key: 'Backspace', code: 'Backspace' })).toBe(0xff08);
    expect(keysymFor({ key: 'Escape', code: 'Escape' })).toBe(0xff1b);
    expect(keysymFor({ key: 'ArrowLeft', code: 'ArrowLeft' })).toBe(0xff51);
    expect(keysymFor({ key: 'ArrowDown', code: 'ArrowDown' })).toBe(0xff54);
    expect(keysymFor({ key: 'Home', code: 'Home' })).toBe(0xff50);
    expect(keysymFor({ key: 'PageUp', code: 'PageUp' })).toBe(0xff55);
    expect(keysymFor({ key: 'Delete', code: 'Delete' })).toBe(0xffff);
    expect(keysymFor({ key: 'F1', code: 'F1' })).toBe(0xffbe);
    expect(keysymFor({ key: 'F12', code: 'F12' })).toBe(0xffc9);
    expect(keysymFor({ key: 'Shift', code: 'ShiftLeft' })).toBe(0xffe1);
    expect(keysymFor({ key: 'Control', code: 'ControlLeft' })).toBe(0xffe3);
    expect(keysymFor({ key: 'Alt', code: 'AltLeft' })).toBe(0xffe9);
    expect(keysymFor({ key: 'Meta', code: 'MetaLeft' })).toBe(0xffe7);
  });

  it('returns null for a dead key, so it is never sent', () => {
    expect(keysymFor({ key: 'Dead', code: 'Quote' })).toBeNull();
  });

  it('keeps AltGr printable characters: the keysym is the codepoint, the bit is AltGraph', () => {
    // A Greek layout with AltGraph held: whatever printable character comes out (here '@'),
    // its codepoint is the keysym and modifiersFor sets the altgr bit, 32.
    const event = {
      key: '@',
      code: 'KeyQ',
      getModifierState: (name: string) => name === 'AltGraph',
    };
    expect(keysymFor(event)).toBe(0x40);
    expect(modifiersFor(event)).toBe(32);
  });
});

describe('modifiersFor', () => {
  it('builds the wire bitmask: shift, caps, ctrl, alt, meta, altgr', () => {
    const withState = (names: string[]) => ({
      getModifierState: (name: string) => names.includes(name),
    });

    expect(modifiersFor(withState([]))).toBe(0);
    expect(modifiersFor(withState(['Shift']))).toBe(1);
    expect(modifiersFor(withState(['CapsLock']))).toBe(2);
    expect(modifiersFor(withState(['Control']))).toBe(4);
    expect(modifiersFor(withState(['Alt']))).toBe(8);
    expect(modifiersFor(withState(['Meta']))).toBe(16);
    expect(modifiersFor(withState(['AltGraph']))).toBe(32);
    expect(modifiersFor(withState(['Shift', 'Control', 'AltGraph']))).toBe(1 | 4 | 32);
  });
});
