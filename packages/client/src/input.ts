/**
 * Input: pointer, wheel and keyboard events turned into envelopes, surface-local only.
 *
 * The host owns focus (ADR-0003 §6): keys are sent only while the host's `isFocused()` says
 * the surface has focus, and when the browser window loses focus one `BlurRelease` per
 * connection releases every held key and button. Every coordinate is scaled from the element's
 * CSS box to the logical surface and clamped into it before it is sent (rule 7,
 * docs/protocol/README.md): the server never learns a position outside the surface it draws.
 *
 * The element takes DOM focus. `attachInput` makes it focusable and focuses it when the user
 * presses a pointer button on it, so the keys typed next reach its listeners. That is the
 * user's own gesture; nothing the server sends moves DOM focus.
 */
import { isPasteChord, pastedText, sendPaste } from './paste.js';
import type { PastePolicy } from './paste.js';
import { MAX_KEY_CODE_BYTES, MAX_POINTER_AXIS_STEPS } from './protocol.js';
import type { Envelope, Size } from './protocol.js';

export interface InputDeps {
  /** Where envelopes go. `AppricotConnection` satisfies this structurally. */
  conn: { send(e: Envelope): void };
  /** The host's notion of focus. Keys flow only while this returns true. */
  isFocused: () => boolean;
  /**
   * The logical surface size, in surface pixels. Pointer positions are scaled from the
   * element's CSS box to this size and clamped inside it, so the host may draw the surface at
   * any CSS size. Pass the registry's size for the surface:
   * `() => registry.get(surfaceId)?.size`. Without it, or while it returns undefined, the CSS
   * box is the logical size, which is right only at one CSS pixel per logical pixel.
   */
  size?: () => Size | undefined;
  /**
   * The host's clipboard policy for keyboard paste (ADR-0003 §7). Opt-in: without it the SDK
   * reads no clipboard at all. With it, the platform paste chord is held back from the app
   * until the browser's `paste` event arrives. The policy sees the pasted text; if it allows
   * it, the text goes out as one `ClipboardSet`, and the chord follows it.
   */
  paste?: PastePolicy;
}

/** The cleanup function `attachInput` returns. */
export type DetachInput = () => void;

/** CSS pixels of wheel travel per discrete step: one notch of a common mouse wheel. */
const WHEEL_STEP_PX = 100;

/** CSS pixels per line of a line-mode wheel delta. Browsers in line mode report 3 lines a notch. */
const WHEEL_LINE_PX = WHEEL_STEP_PX / 3;

/** One forgetter per attached surface of a connection, and the window listener they share. */
interface SharedBlur {
  readonly forgetters: Set<() => void>;
  readonly onBlur: (e: Event) => void;
}

const blurByConn = new WeakMap<object, SharedBlur>();

/**
 * Joins one surface to its connection's window-blur listener and returns the leave function.
 * When the browser window loses focus, every surface of the connection forgets what it holds
 * and the connection sends ONE `BlurRelease`, however many surfaces are attached.
 */
function joinWindowBlur(conn: InputDeps['conn'], forget: () => void): () => void {
  let shared = blurByConn.get(conn);
  if (shared === undefined) {
    const forgetters = new Set<() => void>();
    const onBlur = (e: Event): void => {
      if (e.target !== e.currentTarget) {
        return; // only the window's own blur, never an element's
      }
      for (const forgetOne of forgetters) {
        forgetOne();
      }
      // Nothing stays held: the app cannot keep a key stuck down (ADR-0003 §6).
      conn.send({ kind: 'blurRelease', blurRelease: {} });
    };
    shared = { forgetters, onBlur };
    blurByConn.set(conn, shared);
    // Bubble phase, not capture: a capture listener on window also sees every element's blur.
    window.addEventListener('blur', onBlur);
  }
  const joined = shared;
  joined.forgetters.add(forget);
  return () => {
    joined.forgetters.delete(forget);
    if (joined.forgetters.size === 0 && blurByConn.get(conn) === joined) {
      window.removeEventListener('blur', joined.onBlur);
      blurByConn.delete(conn);
    }
  };
}

/**
 * The code a key event carries on the wire, or null when the wire cannot carry it. The wire
 * needs a non-empty ASCII code of at most `MAX_KEY_CODE_BYTES` (v0 §4.4). An empty code (a
 * script's synthetic event, or a key the browser cannot place) is sent as "Unidentified", the
 * value UI Events reserves for a key code nobody can determine.
 */
function wireCode(code: string): string | null {
  const named = code === '' ? 'Unidentified' : code;
  // ASCII only, so the string length is the byte length.
  return /^[\x20-\x7e]+$/.test(named) && named.length <= MAX_KEY_CODE_BYTES ? named : null;
}

/** One axis of a pointer position: a CSS offset to a surface pixel in [0, extent - 1]. */
function toSurface(offset: number, cssExtent: number, extent: number): number {
  const scaled = cssExtent > 0 ? (offset * extent) / cssExtent : offset;
  return clamp(Math.floor(scaled), 0, Math.ceil(extent) - 1);
}

/** A wheel delta in CSS pixels, whatever its `deltaMode`; a page is the element's extent. */
function wheelPixels(delta: number, deltaMode: number, pageExtent: number): number {
  if (deltaMode === 1) {
    return delta * WHEEL_LINE_PX; // DOM_DELTA_LINE
  }
  if (deltaMode === 2) {
    return delta * (pageExtent > 0 ? pageExtent : WHEEL_STEP_PX); // DOM_DELTA_PAGE
  }
  return delta; // DOM_DELTA_PIXEL
}

/** Adds travel to one axis' accumulator. A reversal starts it afresh, so it answers at once. */
function accumulate(held: number, px: number): number {
  return held * px < 0 ? px : held + px;
}

/** The whole steps in an accumulator, at most `MAX_POINTER_AXIS_STEPS` either way. */
function wholeSteps(held: number): number {
  const steps = Math.trunc(held / WHEEL_STEP_PX) || 0; // `|| 0` turns -0 into 0
  return clamp(steps, -MAX_POINTER_AXIS_STEPS, MAX_POINTER_AXIS_STEPS);
}

/** What an accumulator keeps after its whole steps went out: the part of a step, never more. */
function carryOver(held: number): number {
  return held % WHEEL_STEP_PX;
}

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
 *
 * The element becomes focusable (`tabIndex` 0, unless the host set its own) and takes DOM
 * focus on pointerdown, without scrolling the page. Pointer capture is taken on down so the
 * matching up is seen even off the element; the wheel's default is prevented so the streamed
 * surface scrolls, not the host page.
 */
export function attachInput(
  element: HTMLElement,
  surfaceId: number,
  deps: InputDeps,
): DetachInput {
  const pressed = new Map<number, number>(); // pointerId -> the server button it holds
  const keys = new Map<string, number>(); // wire code -> the keysym its press sent
  // A paste chord held back until the paste event (or its own keyup) delivers it.
  let heldChord: { code: string; keysym: number; modifiers: number } | undefined;
  let wheelX = 0; // CSS pixels of wheel travel not yet sent as a step
  let wheelY = 0;

  // Focusable, so keys typed after a click on the surface reach the listeners below.
  const addedTabIndex = !element.hasAttribute('tabindex');
  if (addedTabIndex) {
    element.tabIndex = 0;
  }

  const sendKey = (code: string, keysym: number, pressedNow: boolean, modifiers: number): void => {
    deps.conn.send({ kind: 'key', key: { keysym, code, pressed: pressedNow, modifiers } });
  };

  /** Sends a press and remembers its keysym, so the release sends the same one. */
  const press = (code: string, keysym: number, modifiers: number): void => {
    const earlier = keys.get(code);
    if (earlier !== undefined) {
      // A second press of one code, with no keyup between: release the first, so it is never
      // left down (two keys the browser could not name both arrive as "Unidentified").
      sendKey(code, earlier, false, modifiers);
    }
    keys.set(code, keysym);
    sendKey(code, keysym, true, modifiers);
  };

  /** Releases every key this element pressed, one by one, and drops a held-back chord. */
  const releaseKeys = (): void => {
    heldChord = undefined;
    for (const [code, keysym] of keys) {
      sendKey(code, keysym, false, 0);
    }
    keys.clear();
  };

  /** Forgets every held key, button and wheel remainder: a BlurRelease has released them. */
  const forget = (): void => {
    pressed.clear();
    keys.clear();
    heldChord = undefined;
    wheelX = 0;
    wheelY = 0;
  };

  const surfacePoint = (e: { clientX: number; clientY: number }): { x: number; y: number } => {
    const box = element.getBoundingClientRect();
    const size = deps.size?.() ?? { width: box.width, height: box.height };
    return {
      x: toSurface(e.clientX - box.left, box.width, size.width),
      y: toSurface(e.clientY - box.top, box.height, size.height),
    };
  };

  const onPointerMove = (e: PointerEvent): void => {
    const { x, y } = surfacePoint(e);
    deps.conn.send({ kind: 'pointerMove', pointerMove: { surfaceId, x, y } });
  };

  const onPointerDown = (e: PointerEvent): void => {
    // Focus first. Whatever a focus change releases then happens before this press, never
    // after it.
    element.focus({ preventScroll: true });
    const button = toServerButton(e.button);
    if (button === undefined) {
      return;
    }
    if (typeof e.pointerId === 'number') {
      element.setPointerCapture?.(e.pointerId); // the up arrives even off the element
    }
    pressed.set(e.pointerId, button);
    // PointerButton carries no coordinates (wire.proto): the press lands wherever the last
    // PointerMove put the pointer. A touch or pen tap has no pointermove before its down, so
    // the position goes out first, and the press lands where it was tapped.
    onPointerMove(e);
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

  /**
   * The wheel, as whole steps (v0 §9). Deltas are summed in CSS pixels, whatever their
   * `deltaMode`, and one step goes out per `WHEEL_STEP_PX` of travel; the part of a step is
   * carried over. A trackpad's many small deltas thus add up to steps instead of each being
   * one. One event sends at most `MAX_POINTER_AXIS_STEPS` either way; travel past that is
   * dropped, so one flick cannot queue a long scroll.
   */
  const onWheel = (e: WheelEvent): void => {
    if (e.deltaX === 0 && e.deltaY === 0) {
      return;
    }
    e.preventDefault(); // the streamed surface owns this scroll, even below a whole step
    const box = element.getBoundingClientRect();
    wheelX = accumulate(wheelX, wheelPixels(e.deltaX, e.deltaMode, box.width));
    wheelY = accumulate(wheelY, wheelPixels(e.deltaY, e.deltaMode, box.height));
    const stepsX = wholeSteps(wheelX);
    const stepsY = wholeSteps(wheelY);
    wheelX = carryOver(wheelX);
    wheelY = carryOver(wheelY);
    if (stepsX === 0 && stepsY === 0) {
      return;
    }
    deps.conn.send({ kind: 'pointerAxis', pointerAxis: { surfaceId, stepsX, stepsY } });
  };

  const onKeydown = (e: KeyboardEvent): void => {
    if (!deps.isFocused() || e.repeat) {
      return; // repeats would double-press server-side; the app does its own repeat
    }
    const code = wireCode(e.code);
    const keysym = keysymFor(e);
    if (code === null || keysym === null) {
      return; // dead keys, keys we cannot name and codes the wire cannot carry are not sent
    }
    const modifiers = modifiersFor(e);
    if (isPasteChord(e)) {
      // Left uncancelled: its default action is the browser's paste event, the only way the
      // host may read the clipboard (ADR-0003 §7). With a paste policy, the app gets the chord
      // only after the pasted text went out, so it pastes that text.
      if (deps.paste !== undefined) {
        heldChord = { code, keysym, modifiers };
        return;
      }
    } else {
      e.preventDefault(); // the key goes to the streamed app, not to the host page
    }
    press(code, keysym, modifiers);
  };

  /** Sends the held-back paste chord's press, if there is one. */
  const deliverChord = (): void => {
    if (heldChord !== undefined) {
      const { code, keysym, modifiers } = heldChord;
      heldChord = undefined;
      press(code, keysym, modifiers);
    }
  };

  const onKeyup = (e: KeyboardEvent): void => {
    const code = wireCode(e.code);
    if (code === null) {
      return;
    }
    if (heldChord?.code === code) {
      deliverChord(); // no paste event came: the app still gets its chord, press then release
    }
    // The release sends the keysym the press sent, not one computed again from this event: a
    // modifier released first (Shift up before 'A' up) must not turn 'A' into 'a' here.
    const keysym = keys.get(code);
    if (keysym === undefined) {
      return; // no press went out for this key, so there is nothing to release
    }
    keys.delete(code);
    sendKey(code, keysym, false, modifiersFor(e));
  };

  const onPaste = (e: ClipboardEvent): void => {
    const policy = deps.paste;
    if (policy === undefined || !deps.isFocused()) {
      return;
    }
    e.preventDefault(); // this paste belongs to the session, not to the host page
    const text = pastedText(e);
    if (text !== null) {
      sendPaste(deps.conn, text, policy);
    }
    deliverChord();
  };

  // Focus left this element: its keyups will go elsewhere, so the keys it pressed are
  // released now. Buttons stay: pointer capture still delivers their ups.
  const onFocusOut = (e: FocusEvent): void => {
    if (e.relatedTarget instanceof Node && element.contains(e.relatedTarget)) {
      return; // focus only moved inside the element
    }
    releaseKeys();
  };

  const leaveWindowBlur = joinWindowBlur(deps.conn, forget);

  element.addEventListener('pointermove', onPointerMove);
  element.addEventListener('pointerdown', onPointerDown);
  element.addEventListener('pointerup', onPointerEnd);
  element.addEventListener('pointercancel', onPointerEnd);
  element.addEventListener('wheel', onWheel, { passive: false });
  element.addEventListener('keydown', onKeydown);
  element.addEventListener('keyup', onKeyup);
  element.addEventListener('focusout', onFocusOut);
  element.addEventListener('paste', onPaste);

  return () => {
    element.removeEventListener('pointermove', onPointerMove);
    element.removeEventListener('pointerdown', onPointerDown);
    element.removeEventListener('pointerup', onPointerEnd);
    element.removeEventListener('pointercancel', onPointerEnd);
    element.removeEventListener('wheel', onWheel);
    element.removeEventListener('keydown', onKeydown);
    element.removeEventListener('keyup', onKeyup);
    element.removeEventListener('focusout', onFocusOut);
    element.removeEventListener('paste', onPaste);
    leaveWindowBlur();
    releaseKeys(); // their keyups will never be seen here again
    pressed.clear();
    if (addedTabIndex) {
      element.removeAttribute('tabindex');
    }
  };
}

/**
 * Named-key keysyms from `KeyboardEvent.code`, literal table (wire.proto, KEYBOARD comment:
 * Key.keysym is an X11 keysym number). Arrows are 0xff51-0xff54; Home/Prior/Next/End are
 * 0xff50/0xff55/0xff56/0xff57; F1-F12 run 0xffbe-0xffc9; modifiers are the _L keysyms
 * (Shift 0xffe1, Control 0xffe3, Alt 0xffe9) plus their _R partners. The browser's Meta key
 * (the Windows or Command key) is X's Super (Super_L 0xffeb, Super_R 0xffec): X's Meta_L and
 * Meta_R are another key (docs/protocol/v0.md §8).
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
  MetaLeft: 0xffeb,
  MetaRight: 0xffec,
};

/** X11's Unicode keysyms: codepoint + 0x01000000 (docs/protocol/v0.md §8). */
const UNICODE_KEYSYM_OFFSET = 0x0100_0000;

/**
 * The keysym for one character, by the rule of docs/protocol/v0.md §8, or null for a
 * character no keysym stands for. Printable Latin-1 (U+0020-U+007E, U+00A0-U+00FF) is its
 * own keysym: X11's Latin-1 keysyms carry the same numbers. Every other character, Greek
 * included, is codepoint + 0x01000000, X11's Unicode keysym. A bare codepoint outside Latin-1
 * would collide with X11's legacy keysyms: U+FF08 is XK_BackSpace, U+01A1 is XK_Aogonek.
 * Control characters and lone surrogates are not text and have no keysym.
 */
function characterKeysym(codePoint: number): number | null {
  if ((codePoint >= 0x20 && codePoint <= 0x7e) || (codePoint >= 0xa0 && codePoint <= 0xff)) {
    return codePoint;
  }
  if (codePoint < 0x100 || (codePoint >= 0xd800 && codePoint <= 0xdfff)) {
    return null; // C0 and C1 controls, DEL, and a surrogate with no partner
  }
  return UNICODE_KEYSYM_OFFSET + codePoint;
}

/**
 * The keysym for one keyboard event, or null when there is none worth sending.
 *
 * Named keys come from the literal table above. A `key` of exactly one character, counted by
 * code point so that characters beyond U+FFFF count as one, maps through `characterKeysym`.
 * `key` 'Dead' is four characters, misses the table, and returns null: dead keys are
 * composing state, not text.
 */
export function keysymFor(e: { key: string; code: string }): number | null {
  // Own keys only: a code such as 'constructor' must not find Object.prototype's.
  const named = Object.hasOwn(NAMED_KEYSYMS, e.code) ? NAMED_KEYSYMS[e.code] : undefined;
  if (named !== undefined) {
    return named;
  }
  const codePoint = e.key.codePointAt(0);
  if (codePoint === undefined || e.key.length !== (codePoint > 0xffff ? 2 : 1)) {
    return null; // no character, or more than one
  }
  return characterKeysym(codePoint);
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
