// @vitest-environment jsdom
/**
 * Keyboard paste, the SDK's one path from the user's clipboard to a session (ADR-0003 §7,
 * docs/protocol/v0.md §8).
 *
 * - The paste chord's keydown is never cancelled, so the browser can fire its `paste` event.
 * - Without a host policy nothing reads the clipboard, and the chord goes to the app at once.
 * - With a policy, the chord is held back until the paste event: its text goes out as exactly
 *   one ClipboardSet (if the policy allows it and it fits the cap), and then the chord, so the
 *   app pastes that text. No paste event: the chord still reaches the app, at its keyup.
 * - Nothing ever reads `navigator` for a clipboard: a poison getter proves it.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { attachInput } from './input.js';
import type { DetachInput } from './input.js';
import { isPasteChord, pastedText, sendPaste } from './paste.js';
import type { ChordKey } from './paste.js';
import { MAX_CLIPBOARD_BYTES } from './protocol.js';
import type { Envelope } from './protocol.js';

let clipboardReads = 0;
const attached: DetachInput[] = [];

beforeEach(() => {
  clipboardReads = 0;
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    get() {
      clipboardReads += 1;
      throw new Error('the asynchronous Clipboard API must never be used');
    },
  });
});

afterEach(() => {
  for (const detach of attached.splice(0)) {
    detach();
  }
  delete (navigator as { clipboard?: unknown }).clipboard;
  vi.restoreAllMocks();
});

/** A `paste` event as the browser fires it, carrying `text` as text/plain. */
function pasteEvent(text: string): { event: ClipboardEvent; getData: ReturnType<typeof vi.fn> } {
  const event = new Event('paste', { bubbles: true, cancelable: true });
  const getData = vi.fn((type: string) => (type === 'text/plain' ? text : ''));
  Object.defineProperty(event, 'clipboardData', { value: { getData } });
  return { event: event as ClipboardEvent, getData };
}

function key(type: 'keydown' | 'keyup', init: KeyboardEventInit): KeyboardEvent {
  return new KeyboardEvent(type, { bubbles: true, cancelable: true, ...init });
}

/** One surface, attached with or without a paste policy; every envelope lands in `sent`. */
function makeSurface(policy?: (text: string) => boolean) {
  const element = document.createElement('div');
  const sent: Envelope[] = [];
  let focused = true;
  attached.push(
    attachInput(element, 7, {
      conn: { send: (e) => sent.push(e) },
      isFocused: () => focused,
      ...(policy === undefined ? {} : { paste: policy }),
    }),
  );
  return {
    element,
    sent,
    unfocus: () => {
      focused = false;
    },
  };
}

const CTRL_DOWN = { key: 'Control', code: 'ControlLeft', ctrlKey: true };
const CTRL_UP = { key: 'Control', code: 'ControlLeft' };
const V_DOWN = { key: 'v', code: 'KeyV', ctrlKey: true };
const V_UP = { key: 'v', code: 'KeyV', ctrlKey: true };

const ctrlPress = { kind: 'key', key: { keysym: 0xffe3, code: 'ControlLeft', pressed: true, modifiers: 4 } };
const ctrlRelease = { kind: 'key', key: { keysym: 0xffe3, code: 'ControlLeft', pressed: false, modifiers: 0 } };
const vPress = { kind: 'key', key: { keysym: 0x76, code: 'KeyV', pressed: true, modifiers: 4 } };
const vRelease = { kind: 'key', key: { keysym: 0x76, code: 'KeyV', pressed: false, modifiers: 4 } };

describe('isPasteChord', () => {
  const chord = (init: Partial<ChordKey>): ChordKey => ({
    key: 'v',
    code: 'KeyV',
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    ...init,
  });

  it('knows Ctrl+V, Ctrl+Shift+V, Cmd+V and Shift+Insert', () => {
    expect(isPasteChord(chord({ ctrlKey: true }))).toBe(true);
    expect(isPasteChord(chord({ key: 'V', ctrlKey: true, shiftKey: true }))).toBe(true);
    expect(isPasteChord(chord({ metaKey: true }))).toBe(true);
    expect(isPasteChord(chord({ key: 'Insert', code: 'Insert', shiftKey: true }))).toBe(true);
  });

  it('follows the layout for Latin letters and the physical key for other scripts', () => {
    // Dvorak types 'v' on the Period position, and 'k' on the KeyV position.
    expect(isPasteChord(chord({ key: 'v', code: 'Period', ctrlKey: true }))).toBe(true);
    expect(isPasteChord(chord({ key: 'k', code: 'KeyV', ctrlKey: true }))).toBe(false);
    // A Greek layout types 'ω' on the KeyV position.
    expect(isPasteChord(chord({ key: 'ω', code: 'KeyV', ctrlKey: true }))).toBe(true);
  });

  it('is not a plain v, an AltGr or Alt chord, Ctrl+Cmd+V, or Ctrl+Insert', () => {
    expect(isPasteChord(chord({}))).toBe(false);
    expect(isPasteChord(chord({ ctrlKey: true, altKey: true }))).toBe(false); // AltGr
    expect(isPasteChord(chord({ metaKey: true, altKey: true }))).toBe(false);
    expect(isPasteChord(chord({ ctrlKey: true, metaKey: true }))).toBe(false);
    expect(isPasteChord(chord({ key: 'Insert', code: 'Insert', ctrlKey: true }))).toBe(false);
    expect(isPasteChord(chord({ key: 'Insert', code: 'Insert' }))).toBe(false);
  });
});

describe('pastedText and sendPaste', () => {
  it('reads text/plain from the paste event, and nothing from an empty one', () => {
    expect(pastedText(pasteEvent('Γειά σου').event)).toBe('Γειά σου');
    expect(pastedText(pasteEvent('').event)).toBeNull();
    expect(pastedText(new Event('paste') as ClipboardEvent)).toBeNull(); // no clipboardData
  });

  it('refuses an over-cap paste quietly, before the policy sees it', () => {
    const sent: Envelope[] = [];
    const policy = vi.fn(() => true);

    // Over by two in UTF-8 bytes, though only half the cap in UTF-16 code units.
    expect(sendPaste({ send: (e) => sent.push(e) }, 'α'.repeat(MAX_CLIPBOARD_BYTES / 2 + 1), policy)).toBe(false);
    expect(sendPaste({ send: (e) => sent.push(e) }, 'A'.repeat(MAX_CLIPBOARD_BYTES + 1), policy)).toBe(false);
    expect(policy).not.toHaveBeenCalled();
    expect(sent).toEqual([]);

    // Exactly at the cap is legal.
    expect(sendPaste({ send: (e) => sent.push(e) }, 'α'.repeat(MAX_CLIPBOARD_BYTES / 2), policy)).toBe(true);
    expect(sent.map((e) => e.kind)).toEqual(['clipboardSet']);
  });

  it('sends nothing the policy refuses', () => {
    const sent: Envelope[] = [];
    expect(sendPaste({ send: (e) => sent.push(e) }, 'secret', () => false)).toBe(false);
    expect(sent).toEqual([]);
  });
});

describe('keyboard paste through attachInput, with a host policy', () => {
  it('holds Ctrl+V back until the paste event, then sends one ClipboardSet and the chord', () => {
    const policy = vi.fn(() => true);
    const { element, sent } = makeSurface(policy);

    const ctrl = key('keydown', CTRL_DOWN);
    element.dispatchEvent(ctrl);
    const v = key('keydown', V_DOWN);
    element.dispatchEvent(v);

    expect(ctrl.defaultPrevented).toBe(true); // an ordinary key: it belongs to the app
    expect(v.defaultPrevented).toBe(false); // left alone, so the browser fires `paste`
    expect(sent).toEqual([ctrlPress]); // the chord waits for the text

    const { event: paste } = pasteEvent('Γειά σου, πρόχειρο');
    element.dispatchEvent(paste);
    element.dispatchEvent(key('keyup', V_UP));
    element.dispatchEvent(key('keyup', CTRL_UP));

    expect(paste.defaultPrevented).toBe(true);
    expect(policy).toHaveBeenCalledWith('Γειά σου, πρόχειρο');
    expect(sent).toEqual([
      ctrlPress,
      { kind: 'clipboardSet', clipboardSet: { text: 'Γειά σου, πρόχειρο' } },
      vPress,
      vRelease,
      ctrlRelease,
    ]);
    expect(clipboardReads).toBe(0);
  });

  it('delivers the chord without the text when the policy refuses it', () => {
    const { element, sent } = makeSurface(() => false);

    element.dispatchEvent(key('keydown', CTRL_DOWN));
    element.dispatchEvent(key('keydown', V_DOWN));
    element.dispatchEvent(pasteEvent('not for the app').event);

    expect(sent).toEqual([ctrlPress, vPress]);
  });

  it('refuses an over-cap paste quietly and still delivers the chord', () => {
    const policy = vi.fn(() => true);
    const { element, sent } = makeSurface(policy);

    element.dispatchEvent(key('keydown', CTRL_DOWN));
    element.dispatchEvent(key('keydown', V_DOWN));
    expect(() => element.dispatchEvent(pasteEvent('A'.repeat(MAX_CLIPBOARD_BYTES + 1)).event)).not.toThrow();

    expect(policy).not.toHaveBeenCalled();
    expect(sent).toEqual([ctrlPress, vPress]);
  });

  it('delivers the chord at its keyup when no paste event came', () => {
    const { element, sent } = makeSurface(() => true);

    element.dispatchEvent(key('keydown', CTRL_DOWN));
    element.dispatchEvent(key('keydown', V_DOWN));
    element.dispatchEvent(key('keyup', V_UP));

    expect(sent).toEqual([ctrlPress, vPress, vRelease]);
  });

  it('treats Cmd+V and Shift+Insert the same way', () => {
    const { element, sent } = makeSurface(() => true);

    const cmdV = key('keydown', { key: 'v', code: 'KeyV', metaKey: true });
    element.dispatchEvent(cmdV);
    element.dispatchEvent(pasteEvent('one').event);
    element.dispatchEvent(key('keyup', { key: 'v', code: 'KeyV', metaKey: true }));
    const shiftInsert = key('keydown', { key: 'Insert', code: 'Insert', shiftKey: true });
    element.dispatchEvent(shiftInsert);
    element.dispatchEvent(pasteEvent('two').event);

    expect(cmdV.defaultPrevented).toBe(false);
    expect(shiftInsert.defaultPrevented).toBe(false);
    expect(sent).toEqual([
      { kind: 'clipboardSet', clipboardSet: { text: 'one' } },
      { kind: 'key', key: { keysym: 0x76, code: 'KeyV', pressed: true, modifiers: 16 } },
      { kind: 'key', key: { keysym: 0x76, code: 'KeyV', pressed: false, modifiers: 16 } },
      { kind: 'clipboardSet', clipboardSet: { text: 'two' } },
      { kind: 'key', key: { keysym: 0xff63, code: 'Insert', pressed: true, modifiers: 1 } },
    ]);
  });

  it('ignores a paste event while the surface is not focused', () => {
    const policy = vi.fn(() => true);
    const { element, sent, unfocus } = makeSurface(policy);
    unfocus();

    const { event, getData } = pasteEvent('typed for the host');
    element.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(false);
    expect(getData).not.toHaveBeenCalled();
    expect(policy).not.toHaveBeenCalled();
    expect(sent).toEqual([]);
  });
});

describe('keyboard paste through attachInput, without a host policy', () => {
  it('sends the chord at once, uncancelled, and never reads the paste event', () => {
    const { element, sent } = makeSurface();

    element.dispatchEvent(key('keydown', CTRL_DOWN));
    const v = key('keydown', V_DOWN);
    element.dispatchEvent(v);
    const { event: paste, getData } = pasteEvent('the host may take this itself');
    element.dispatchEvent(paste);

    expect(v.defaultPrevented).toBe(false);
    expect(paste.defaultPrevented).toBe(false); // the host's own paste listener still works
    expect(getData).not.toHaveBeenCalled();
    expect(sent).toEqual([ctrlPress, vPress]);
    expect(clipboardReads).toBe(0);
  });
});
