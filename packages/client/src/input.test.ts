// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Envelope, Size } from './protocol';
import { attachInput, keysymFor, modifiersFor } from './input';
import type { DetachInput } from './input';

/** Every surface a test attached; detached after each test so no window listener leaks. */
const attached: DetachInput[] = [];

afterEach(() => {
  // Blur first, while the element is still focusable: jsdom answers the removal of a focused
  // element by firing a window blur on the next focus(), which a browser does not do.
  if (document.activeElement instanceof HTMLElement) {
    document.activeElement.blur();
  }
  for (const detach of attached.splice(0)) {
    detach();
  }
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

/**
 * One attached surface with a fixed on-screen box: the element covers (100, 50) to
 * (180, 90). Coordinates are asserted surface-local and clamped to the box, never global
 * (rule 7, docs/protocol/README.md). `size` is the logical surface size, when the host gives
 * one; `inDocument` puts the element in the page, where it can take focus.
 */
function makeAttached(options: { size?: Size; inDocument?: boolean } = {}) {
  const element = document.createElement('div');
  if (options.inDocument === true) {
    document.body.appendChild(element);
  }
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
  const logicalSize = options.size;
  const detach = attachInput(element, 7, {
    conn: { send: (e) => sent.push(e) },
    isFocused: () => focused,
    ...(logicalSize === undefined ? {} : { size: () => logicalSize }),
  });
  attached.push(detach);
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

    // The last pixel of an 80x40 surface is (79, 39): a position is a pixel inside it.
    expect(sent).toEqual([
      { kind: 'pointerMove', pointerMove: { surfaceId: 7, x: 79, y: 39 } },
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
    element.dispatchEvent(new WheelEvent('wheel', { deltaX: 53.5, cancelable: true }));

    expect(sent).toEqual([
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 0, stepsY: 1 } },
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 0, stepsY: -1 } },
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 1, stepsY: 0 } },
    ]);
    expect(down.defaultPrevented).toBe(true);
    expect(up.defaultPrevented).toBe(true);
    // Half a step sends nothing yet, and the host page still does not scroll under it.
    expect(sideways.defaultPrevented).toBe(true);
  });
});

describe('attachInput pointer scaling', () => {
  it('scales from the CSS box to the logical size the host gives', () => {
    // An 80x40 CSS box showing a 160x120 surface: 2x across, 3x down.
    const { element, sent } = makeAttached({ size: { width: 160, height: 120 } });

    element.dispatchEvent(new MouseEvent('pointermove', { clientX: 140, clientY: 70 }));
    element.dispatchEvent(new MouseEvent('pointermove', { clientX: 180, clientY: 90 }));

    expect(sent).toEqual([
      { kind: 'pointerMove', pointerMove: { surfaceId: 7, x: 80, y: 60 } },
      { kind: 'pointerMove', pointerMove: { surfaceId: 7, x: 159, y: 119 } },
    ]);
  });

  it('scales down when the host draws the surface larger than its logical size', () => {
    // A 1000 px wide panel showing an 800 px surface, here at a tenth: an 80x40 CSS box
    // showing a 64x32 surface, so CSS (50, 25) is logical (40, 20).
    const { element, sent } = makeAttached({ size: { width: 64, height: 32 } });

    element.dispatchEvent(new MouseEvent('pointermove', { clientX: 150, clientY: 75 }));

    expect(sent).toEqual([
      { kind: 'pointerMove', pointerMove: { surfaceId: 7, x: 40, y: 20 } },
    ]);
  });
});

describe('attachInput wheel (v0 §9)', () => {
  const wheel = (init: WheelEventInit): WheelEvent =>
    new WheelEvent('wheel', { cancelable: true, ...init });

  it("adds a trackpad's small deltas up into one step instead of one step each", () => {
    const { element, sent } = makeAttached();

    for (let i = 0; i < 12; i += 1) {
      element.dispatchEvent(wheel({ deltaY: 10 }));
    }

    expect(sent).toEqual([
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 0, stepsY: 1 } },
    ]);
  });

  it('counts a line-mode notch (3 lines) as one step, and a page as the surface height', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(wheel({ deltaY: 3, deltaMode: 1 })); // DOM_DELTA_LINE
    element.dispatchEvent(wheel({ deltaY: 6, deltaMode: 1 }));
    element.dispatchEvent(wheel({ deltaY: -5, deltaMode: 2 })); // DOM_DELTA_PAGE: 5 x 40 px

    expect(sent).toEqual([
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 0, stepsY: 1 } },
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 0, stepsY: 2 } },
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 0, stepsY: -2 } },
    ]);
  });

  it('starts afresh on a reversal, so a remainder never delays the other way', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(wheel({ deltaY: 90 })); // 0.9 of a step, held
    element.dispatchEvent(wheel({ deltaY: -100 })); // one step up, not 0.1 of one down

    expect(sent).toEqual([
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: 0, stepsY: -1 } },
    ]);
  });

  it('never sends more than 64 steps (MAX_POINTER_AXIS_STEPS), and drops the excess', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(wheel({ deltaY: 1_000_000, deltaX: -1_000_000 }));
    element.dispatchEvent(wheel({ deltaY: 50 })); // the excess did not carry over

    expect(sent).toEqual([
      { kind: 'pointerAxis', pointerAxis: { surfaceId: 7, stepsX: -64, stepsY: 64 } },
    ]);
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

  it('releases with the keysym the press sent, even when Shift went up first', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Shift', code: 'ShiftLeft', cancelable: true }),
    );
    element.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'A', code: 'KeyA', shiftKey: true, cancelable: true }),
    );
    element.dispatchEvent(new KeyboardEvent('keyup', { key: 'Shift', code: 'ShiftLeft' }));
    element.dispatchEvent(new KeyboardEvent('keyup', { key: 'a', code: 'KeyA' })); // not 'A'

    expect(sent).toEqual([
      { kind: 'key', key: { keysym: 0xffe1, code: 'ShiftLeft', pressed: true, modifiers: 0 } },
      { kind: 'key', key: { keysym: 0x41, code: 'KeyA', pressed: true, modifiers: 1 } },
      { kind: 'key', key: { keysym: 0xffe1, code: 'ShiftLeft', pressed: false, modifiers: 0 } },
      { kind: 'key', key: { keysym: 0x41, code: 'KeyA', pressed: false, modifiers: 0 } },
    ]);
  });

  it('drops a keyup whose press it never sent', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(new KeyboardEvent('keyup', { key: 'a', code: 'KeyA' }));

    expect(sent).toEqual([]);
  });

  it('sends "Unidentified" for an empty code, never an empty one (v0 §8)', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(new KeyboardEvent('keydown', { key: 'é', code: '', cancelable: true }));
    element.dispatchEvent(new KeyboardEvent('keyup', { key: 'é', code: '' }));

    expect(sent).toEqual([
      { kind: 'key', key: { keysym: 0xe9, code: 'Unidentified', pressed: true, modifiers: 0 } },
      { kind: 'key', key: { keysym: 0xe9, code: 'Unidentified', pressed: false, modifiers: 0 } },
    ]);
  });

  it('drops a key whose code the wire cannot carry: non-ASCII, or over 16 bytes', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', code: 'Κλειδί' }));
    element.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', code: 'K'.repeat(17) }));

    expect(sent).toEqual([]);
  });

  it('releases the first press when one code is pressed twice with no keyup between', () => {
    const { element, sent } = makeAttached();

    element.dispatchEvent(new KeyboardEvent('keydown', { key: 'é', code: '' }));
    element.dispatchEvent(new KeyboardEvent('keydown', { key: 'ü', code: '' }));

    expect(sent).toEqual([
      { kind: 'key', key: { keysym: 0xe9, code: 'Unidentified', pressed: true, modifiers: 0 } },
      { kind: 'key', key: { keysym: 0xe9, code: 'Unidentified', pressed: false, modifiers: 0 } },
      { kind: 'key', key: { keysym: 0xfc, code: 'Unidentified', pressed: true, modifiers: 0 } },
    ]);
  });
});

describe('attachInput focus', () => {
  it('makes the element focusable, and keeps a tabIndex the host set', () => {
    const { element } = makeAttached({ inDocument: true });
    expect(element.tabIndex).toBe(0);

    const own = document.createElement('div');
    own.tabIndex = -1;
    attached.push(attachInput(own, 8, { conn: { send: () => undefined }, isFocused: () => true }));
    expect(own.tabIndex).toBe(-1);
  });

  it('takes DOM focus on pointerdown, so a key typed next reaches the session', () => {
    const { element, sent } = makeAttached({ inDocument: true });
    const hostButton = document.createElement('button');
    document.body.appendChild(hostButton);
    hostButton.focus();
    expect(document.activeElement).toBe(hostButton);

    element.dispatchEvent(new MouseEvent('pointerdown', { button: 0 }));
    expect(document.activeElement).toBe(element);

    // The browser sends keys to the focused element; the test does the same.
    document.activeElement?.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'a', code: 'KeyA', bubbles: true, cancelable: true }),
    );
    expect(sent).toEqual([
      { kind: 'pointerButton', pointerButton: { surfaceId: 7, button: 1, pressed: true } },
      { kind: 'key', key: { keysym: 0x61, code: 'KeyA', pressed: true, modifiers: 0 } },
    ]);
  });

  it('focuses without scrolling the host page', () => {
    const { element } = makeAttached({ inDocument: true });
    const focus = vi.spyOn(element, 'focus');

    element.dispatchEvent(new MouseEvent('pointerdown', { button: 0 }));

    expect(focus).toHaveBeenCalledWith({ preventScroll: true });
  });

  it('removes the tabIndex it added when detached', () => {
    const { element, detach } = makeAttached({ inDocument: true });

    detach();

    expect(element.hasAttribute('tabindex')).toBe(false);
  });

  it('releases the keys it pressed when focus leaves the element, and nothing else', () => {
    const { element, sent } = makeAttached({ inDocument: true });
    element.focus();
    element.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', code: 'KeyA' }));

    const hostInput = document.createElement('input');
    document.body.appendChild(hostInput);
    hostInput.focus(); // the keyup of 'a' will go to the host input now, not to the surface

    expect(sent).toEqual([
      { kind: 'key', key: { keysym: 0x61, code: 'KeyA', pressed: true, modifiers: 0 } },
      { kind: 'key', key: { keysym: 0x61, code: 'KeyA', pressed: false, modifiers: 0 } },
    ]);
  });
});

describe('attachInput blur and detach', () => {
  it('keeps a button just pressed when a host control loses focus to the surface', () => {
    const { element, sent } = makeAttached({ inDocument: true });
    const connect = document.createElement('button');
    document.body.appendChild(connect);
    connect.focus(); // the demo's Connect button keeps focus after the click

    element.dispatchEvent(new MouseEvent('pointerdown', { button: 0 })); // blurs the button
    connect.dispatchEvent(new FocusEvent('blur')); // and a stray blur, for good measure
    element.dispatchEvent(new MouseEvent('pointerup', { button: 0 }));

    expect(sent).toEqual([
      { kind: 'pointerButton', pointerButton: { surfaceId: 7, button: 1, pressed: true } },
      { kind: 'pointerButton', pointerButton: { surfaceId: 7, button: 1, pressed: false } },
    ]);
  });

  it('sends one BlurRelease per connection when the window blurs, not one per surface', () => {
    const sent: Envelope[] = [];
    const conn = { send: (e: Envelope) => sent.push(e) };
    for (const id of [1, 2, 3]) {
      attached.push(attachInput(document.createElement('div'), id, { conn, isFocused: () => true }));
    }

    window.dispatchEvent(new Event('blur'));

    expect(sent).toEqual([{ kind: 'blurRelease', blurRelease: {} }]);
  });

  it('forgets what it held on a window blur: the later ups send nothing', () => {
    const { element, sent } = makeAttached();
    element.dispatchEvent(new MouseEvent('pointerdown', { button: 0 }));
    element.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', code: 'KeyA' }));

    window.dispatchEvent(new Event('blur'));
    element.dispatchEvent(new MouseEvent('pointerup', { button: 0 }));
    element.dispatchEvent(new KeyboardEvent('keyup', { key: 'a', code: 'KeyA' }));

    expect(sent.map((e) => e.kind)).toEqual(['pointerButton', 'key', 'blurRelease']);
  });

  it('stops listening for the window blur once every surface of the connection detached', () => {
    const { sent, detach } = makeAttached();
    detach();

    window.dispatchEvent(new Event('blur'));

    expect(sent).toEqual([]);
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

  it('maps printable Latin-1 to the codepoint itself, the X11 Latin-1 keysyms', () => {
    expect(keysymFor({ key: '~', code: 'Backquote' })).toBe(0x7e);
    expect(keysymFor({ key: ' ', code: 'Space' })).toBe(0xa0);
    expect(keysymFor({ key: 'é', code: 'KeyE' })).toBe(0xe9);
    expect(keysymFor({ key: 'ÿ', code: 'KeyY' })).toBe(0xff);
  });

  it('maps Greek to X11 Unicode keysyms, codepoint + 0x01000000 (v0 §8)', () => {
    expect(keysymFor({ key: 'α', code: 'KeyA' })).toBe(0x010003b1);
    expect(keysymFor({ key: 'Ω', code: 'KeyW' })).toBe(0x010003a9);
  });

  it('never sends a bare codepoint that is a legacy X11 keysym', () => {
    // Bare, these would be XK_BackSpace, XK_Return, XK_Escape, XK_Aogonek, XK_kana_fullstop.
    expect(keysymFor({ key: '（', code: 'Digit9' })).toBe(0x0100ff08);
    expect(keysymFor({ key: '－', code: 'Minus' })).toBe(0x0100ff0d);
    expect(keysymFor({ key: '；', code: 'Semicolon' })).toBe(0x0100ff1b);
    expect(keysymFor({ key: 'ơ', code: 'BracketRight' })).toBe(0x010001a1);
    expect(keysymFor({ key: 'ҡ', code: 'KeyQ' })).toBe(0x010004a1);
  });

  it('counts by code point: a character beyond U+FFFF is one character', () => {
    expect(keysymFor({ key: '😀', code: 'Unidentified' })).toBe(0x0101f600);
    expect(keysymFor({ key: '𝛼', code: 'KeyA' })).toBe(0x0101d6fc);
  });

  it('returns null for control characters, lone surrogates and more than one character', () => {
    expect(keysymFor({ key: '\u0000', code: 'KeyA' })).toBeNull();
    expect(keysymFor({ key: '\u001b', code: 'KeyA' })).toBeNull();
    expect(keysymFor({ key: '\u007f', code: 'KeyA' })).toBeNull();
    expect(keysymFor({ key: '\u0085', code: 'KeyA' })).toBeNull();
    expect(keysymFor({ key: '\ud83d', code: 'KeyA' })).toBeNull();
    expect(keysymFor({ key: 'ab', code: 'KeyA' })).toBeNull();
    expect(keysymFor({ key: 'é', code: 'KeyE' })).toBeNull(); // e + combining acute
    expect(keysymFor({ key: '', code: 'KeyA' })).toBeNull();
  });

  it('finds no named keysym on the prototype: a code such as "constructor" is not a key', () => {
    expect(keysymFor({ key: 'Unidentified', code: 'constructor' })).toBeNull();
    expect(keysymFor({ key: 'Unidentified', code: 'toString' })).toBeNull();
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
