/**
 * Keyboard paste: the one sanctioned path from the user's clipboard to a session.
 *
 * ADR-0003 §7: the client sends pasted text only if the host's policy allows it, and it reads
 * the clipboard only from a `paste` event, never on its own. The browser fires that event only
 * when the keydown of the platform paste chord is left uncancelled, so `attachInput` never
 * cancels the chord. With a `paste` policy in its deps, `attachInput` also holds the chord back
 * from the app until the paste event has delivered the text, so the app asks for the clipboard
 * only after the text is there (docs/protocol/v0.md §8).
 *
 * Nothing here uses the asynchronous Clipboard API, and nothing here runs without a user
 * gesture.
 */
import { MAX_CLIPBOARD_BYTES } from './protocol.js';
import type { Envelope } from './protocol.js';

/**
 * The host's clipboard policy for one keyboard paste. It sees the text the user pasted and
 * returns true to let it reach the session as a `ClipboardSet`, false to keep it in the host.
 * It runs synchronously inside the browser's `paste` event.
 */
export type PastePolicy = (text: string) => boolean;

/** The key fields `isPasteChord` reads; a `KeyboardEvent` satisfies it. */
export interface ChordKey {
  key: string;
  code: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

/**
 * True for the platform paste chord: Ctrl+V (Ctrl+Shift+V too), Cmd+V, or Shift+Insert.
 *
 * The letter comes from `key` when the layout types a Latin letter there, so the chord follows
 * the layout (Dvorak's V is not on the KeyV position). On a layout whose letters are not Latin,
 * a Greek one for example, it falls back to the physical `KeyV`.
 */
export function isPasteChord(e: ChordKey): boolean {
  if (e.code === 'Insert') {
    return e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey;
  }
  if (e.altKey || e.ctrlKey === e.metaKey) {
    return false; // exactly one of Ctrl and Cmd, and no Alt: AltGr arrives as Ctrl+Alt
  }
  const latin = /^[a-z]$/i.test(e.key);
  return latin ? e.key.toLowerCase() === 'v' : e.code === 'KeyV';
}

/**
 * The plain text a `paste` event carries, or null when it carries none. This is the SDK's only
 * read of the user's clipboard, and it happens only inside the event the browser fired for the
 * user's own paste gesture.
 */
export function pastedText(e: ClipboardEvent): string | null {
  const text = e.clipboardData?.getData('text/plain') ?? '';
  return text === '' ? null : text;
}

const utf8Encoder = new TextEncoder();

/** True when `text` fits `ClipboardSet.text`: at most `MAX_CLIPBOARD_BYTES` UTF-8 bytes. */
function fitsClipboardCap(text: string): boolean {
  // Every UTF-16 code unit costs at least one UTF-8 byte, so a string longer than the cap in
  // code units is over it in bytes, and is refused before anything encodes it.
  if (text.length > MAX_CLIPBOARD_BYTES) {
    return false;
  }
  return utf8Encoder.encode(text).length <= MAX_CLIPBOARD_BYTES;
}

/**
 * Sends `text` as one `ClipboardSet` when it fits the cap and the host's policy allows it.
 * Returns whether it was sent. An oversized paste is refused quietly, before the policy sees
 * it: nothing throws out of a DOM listener, and the session lives on.
 */
export function sendPaste(
  conn: { send(e: Envelope): void },
  text: string,
  policy: PastePolicy,
): boolean {
  if (!fitsClipboardCap(text) || !policy(text)) {
    return false;
  }
  conn.send({ kind: 'clipboardSet', clipboardSet: { text } });
  return true;
}
