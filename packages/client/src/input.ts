/**
 * Input: pointer, wheel and keyboard events turned into envelopes, surface-local only.
 *
 * The host owns focus (ADR-0003 §6): keys are sent only while the host's `isFocused()` says
 * the surface has focus, a blur releases every held key and button, and every coordinate is
 * clamped into the surface box before it is sent (rule 7, docs/protocol/README.md) — the
 * server never learns a position outside the surface it is drawing.
 */
import type { Envelope, Size } from './protocol';

export interface InputDeps {
  /** Where envelopes go. `AppricotConnection` satisfies this structurally. */
  conn: { send(e: Envelope): void };
  /** The host's notion of focus. Keys flow only while this returns true. */
  isFocused: () => boolean;
  /**
   * The logical surface size used to clamp coordinates. Defaults to the element's client box
   * (falling back to its bounding rect), which is right when the host sizes the element at
   * one CSS pixel per logical pixel.
   */
  size?: () => Size;
}

/** The cleanup function `attachInput` returns. */
export type DetachInput = () => void;

/** Browser button numbers to X button numbers: 0/1/2 become 1/2/3; anything else is ignored. */
function toServerButton(button: number): number | undefined {
  if (button === 0) {
    return 1;
  }
  if (button === 1) {
    return 2;
  }
  if (button === 2) {
    return 3;
  }
  return undefined;
}

function clamp(v: number, min: number, max: number): number {
  return Math.min(Math.max(v, min), Math.max(min, max));
}

/**
 * Attaches input listeners to `element` for one surface and returns the detach function.
 * Pointer capture is taken on down so the matching up is seen even off the element; the
 * wheel's default is prevented so the streamed surface scrolls, not the host page.
 */
export function attachInput(
  element: HTMLElement,
  surfaceId: number,
  deps: InputDeps,
): DetachInput {
  const pressed = new Map<number, number>(); // pointerId -> the server button it holds

  const surfaceSize = (): Size => {
    const box = element.getBoundingClientRect();
    return deps.size?.() ?? {
      width: element.clientWidth || box.width,
      height: element.clientHeight || box.height,
    };
  };

  const surfacePoint = (e: { clientX: number; clientY: number }): { x: number; y: number } => {
    const box = element.getBoundingClientRect();
    const size = surfaceSize();
    return {
      x: Math.round(clamp(e.clientX - box.left, 0, size.width)),
      y: Math.round(clamp(e.clientY - box.top, 0, size.height)),
    };
  };

  const onPointerMove = (e: PointerEvent): void => {
    const { x, y } = surfacePoint(e);
    deps.conn.send({ kind: 'pointerMove', pointerMove: { surfaceId, x, y } });
  };

  const onPointerDown = (e: PointerEvent): void => {
    const button = toServerButton(e.button);
    if (button === undefined) {
      return;
    }
    if (typeof e.pointerId === 'number') {
      element.setPointerCapture?.(e.pointerId); // the up arrives even off the element
    }
    pressed.set(e.pointerId, button);
    // PointerButton carries no coordinates (wire.proto): the pointermove before it placed
    // the pointer, and the server keeps the two together.
    deps.conn.send({ kind: 'pointerButton', pointerButton: { surfaceId, button, pressed: true } });
  };

  const onPointerEnd = (e: PointerEvent): void => {
    const button = pressed.get(e.pointerId);
    if (button === undefined) {
      return;
    }
    pressed.delete(e.pointerId);
    deps.conn.send({
      kind: 'pointerButton',
      pointerButton: { surfaceId, button, pressed: false },
    });
  };

  const onWheel = (e: WheelEvent): void => {
    const stepsX = Math.sign(e.deltaX);
    const stepsY = Math.sign(e.deltaY);
    if (stepsX === 0 && stepsY === 0) {
      return;
    }
    e.preventDefault(); // the streamed surface owns this scroll, not the host page
    deps.conn.send({ kind: 'pointerAxis', pointerAxis: { surfaceId, stepsX, stepsY } });
  };

  const onKeydown = (e: KeyboardEvent): void => {
    if (!deps.isFocused() || e.repeat) {
      return; // repeats would double-press server-side; the app does its own repeat
    }
    const keysym = keysymFor(e);
    if (keysym === null) {
      return; // dead keys and keys we cannot name are not sent
    }
    e.preventDefault(); // the key goes to the streamed app, not to the host page
    deps.conn.send({
      kind: 'key',
      key: { keysym, code: e.code, pressed: true, modifiers: modifiersFor(e) },
    });
  };

  const onKeyup = (e: KeyboardEvent): void => {
    if (!deps.isFocused()) {
      return;
    }
    const keysym = keysymFor(e);
    if (keysym === null) {
      return;
    }
    deps.conn.send({
      kind: 'key',
      key: { keysym, code: e.code, pressed: false, modifiers: modifiersFor(e) },
    });
  };

  const onBlur = (): void => {
    pressed.clear(); // nothing stays held: the app cannot keep a key stuck down (ADR-0003 §6)
    deps.conn.send({ kind: 'blurRelease', blurRelease: {} });
  };

  element.addEventListener('pointermove', onPointerMove);
  element.addEventListener('pointerdown', onPointerDown);
  element.addEventListener('pointerup', onPointerEnd);
  element.addEventListener('pointercancel', onPointerEnd);
  element.addEventListener('wheel', onWheel, { passive: false });
  element.addEventListener('keydown', onKeydown);
  element.addEventListener('keyup', onKeyup);
  window.addEventListener('blur', onBlur, true); // capture: element blur reaches here too

  return () => {
    element.removeEventListener('pointermove', onPointerMove);
    element.removeEventListener('pointerdown', onPointerDown);
    element.removeEventListener('pointerup', onPointerEnd);
    element.removeEventListener('pointercancel', onPointerEnd);
    element.removeEventListener('wheel', onWheel);
    element.removeEventListener('keydown', onKeydown);
    element.removeEventListener('keyup', onKeyup);
    window.removeEventListener('blur', onBlur, true);
    pressed.clear();
  };
}

/**
 * Named-key keysyms from `KeyboardEvent.code`, literal table (wire.proto, KEYBOARD comment:
 * Key.keysym is an X11 keysym number). Arrows are 0xff51-0xff54; Home/Prior/Next/End are
 * 0xff50/0xff55/0xff56/0xff57; F1-F12 run 0xffbe-0xffc9; modifiers are the _L keysyms
 * (Shift 0xffe1, Control 0xffe3, Alt 0xffe9, Meta 0xffe7) plus their _R partners.
 */
const NAMED_KEYSYMS: Readonly<Record<string, number>> = {
  Enter: 0xff0d,
  NumpadEnter: 0xff8d,
  Tab: 0xff09,
  Backspace: 0xff08,
  Escape: 0xff1b,
  Home: 0xff50,
  ArrowLeft: 0xff51,
  ArrowUp: 0xff52,
  ArrowRight: 0xff53,
  ArrowDown: 0xff54,
  PageUp: 0xff55,
  PageDown: 0xff56,
  End: 0xff57,
  Insert: 0xff63,
  Delete: 0xffff,
  F1: 0xffbe,
  F2: 0xffbf,
  F3: 0xffc0,
  F4: 0xffc1,
  F5: 0xffc2,
  F6: 0xffc3,
  F7: 0xffc4,
  F8: 0xffc5,
  F9: 0xffc6,
  F10: 0xffc7,
  F11: 0xffc8,
  F12: 0xffc9,
  ShiftLeft: 0xffe1,
  ShiftRight: 0xffe2,
  ControlLeft: 0xffe3,
  ControlRight: 0xffe4,
  CapsLock: 0xffe5,
  AltLeft: 0xffe9,
  AltRight: 0xffea,
  MetaLeft: 0xffe7,
  MetaRight: 0xffe8,
};

/**
 * The keysym for one keyboard event, or null when there is none worth sending.
 *
 * Named keys come from the literal table above. A single-character `key` maps to its
 * codepoint, which is the X keysym for the Latin block and for Greek (0x0370-0x03ff: there
 * the X keysyms equal the Unicode codepoints — wire.proto header, KEYBOARD comment; the
 * human-readable spec lands as docs/protocol/v0.md). `key` 'Dead' has length 4, misses the
 * table, and returns null: dead keys are composing state, not text.
 */
export function keysymFor(e: { key: string; code: string }): number | null {
  const named = NAMED_KEYSYMS[e.code];
  if (named !== undefined) {
    return named;
  }
  if (e.key.length === 1) {
    const codePoint = e.key.codePointAt(0);
    if (codePoint !== undefined && codePoint > 0) {
      return codePoint;
    }
  }
  return null;
}

/**
 * The modifier bitmask for one keyboard event (wire.proto, KEYBOARD comment):
 * 1 shift, 2 lock (caps), 4 control, 8 alt, 16 meta, 32 altgr.
 */
export function modifiersFor(e: { getModifierState(key: string): boolean }): number {
  let mask = 0;
  if (e.getModifierState('Shift')) {
    mask |= 1;
  }
  if (e.getModifierState('CapsLock')) {
    mask |= 2;
  }
  if (e.getModifierState('Control')) {
    mask |= 4;
  }
  if (e.getModifierState('Alt')) {
    mask |= 8;
  }
  if (e.getModifierState('Meta')) {
    mask |= 16;
  }
  if (e.getModifierState('AltGraph')) {
    mask |= 32;
  }
  return mask;
}
