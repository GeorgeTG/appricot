/**
 * The TypeScript codec of the APPricot wire protocol, version 0.
 *
 * This file hand-mirrors the normative protobuf contract in
 * crates/appricot-proto/proto/appricot/v0/wire.proto: one interface per message, the same field
 * numbers, the same limits. The Rust twin (crates/appricot-proto, prost-based) and this file are
 * proven against each other by the shared vectors in crates/appricot-proto/testdata/vectors.json.
 *
 * Encoding rules (wire.proto, "ENCODING"), which both codecs follow so an
 * encode(decode(x)) round trip is byte-identical:
 *   - Fields are emitted in field-number order, with minimal varints.
 *   - proto3 default-value skipping: a plain scalar equal to its default (0, false, "", empty
 *     bytes) is not written; a field declared `optional` in the .proto is written whenever the
 *     property is present, even when the value equals the default.
 *   - v0 enums are closed: an unknown enum value is a protocol violation in both codecs and
 *     closes the connection (wire.proto header, decided 2026-09-20); new values ship only with
 *     a new protocol version.
 *   - Repeated scalars (`codecs`) are packed; repeated messages (`tiles`) are not packable and
 *     are written one key per entry.
 *   - sint32 fields travel zigzag-encoded.
 *
 * The server is untrusted (docs/adr/0003-untrusted-server-client.md): decodeEnvelope checks the
 * message cap first, then every per-field cap BEFORE a copy is allocated, rejects unknown field
 * numbers (there is no skip-by-length in v0), an envelope that names no known oneof field, and
 * bytes that are not valid UTF-8. Nothing here does I/O; the connection lives elsewhere
 * (docs/protocol/README.md rule 9).
 */

import {
  MAX_APP_ID_BYTES,
  MAX_BYE_TEXT_BYTES,
  MAX_CLIPBOARD_BYTES,
  MAX_CLIENT_NAME_BYTES,
  MAX_CODECS_OFFERED,
  MAX_CURSOR_BYTES,
  MAX_CURSOR_HEIGHT,
  MAX_CURSOR_WIDTH,
  MAX_ERROR_TEXT_BYTES,
  MAX_KEY_CODE_BYTES,
  MAX_MESSAGE_BYTES,
  MAX_SESSION_ID_BYTES,
  MAX_SURFACE_HEIGHT,
  MAX_SURFACE_WIDTH,
  MAX_TILE_BYTES,
  MAX_TILE_HEIGHT,
  MAX_TILES_PER_FRAME,
  MAX_TILE_WIDTH,
  MAX_TITLE_BYTES,
  MAX_TOKEN_BYTES,
} from './limits';

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/** A byte sequence broke the wire contract or the limits table. */
export class ProtocolError extends Error {
  /** The dotted field name whose cap or shape was broken, when one is known. */
  readonly field: string | undefined;

  constructor(message: string, field?: string) {
    super(message);
    this.name = 'ProtocolError';
    this.field = field;
  }
}

// ---------------------------------------------------------------------------
// Vocabulary types (one per .proto message; enums are union types)
// ---------------------------------------------------------------------------

/** A position in surface-local pixels. `x` grows right, `y` grows down. */
export interface Point {
  x: number;
  y: number;
}

/** A size in pixels. */
export interface Size {
  width: number;
  height: number;
}

/** A rectangle in surface-local pixels: top-left corner plus size. */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** What a surface is for (proto enum Role). 0 ROLE_TOPLEVEL, 1 ROLE_POPUP. */
export type Role = 0 | 1;

/**
 * An edge or corner of a rectangle (proto enum Anchor). 0 CENTER, 1 TOP, 2 BOTTOM, 3 LEFT,
 * 4 RIGHT, 5 TOP_LEFT, 6 BOTTOM_LEFT, 7 TOP_RIGHT, 8 BOTTOM_RIGHT.
 */
export type Anchor = 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;

/**
 * Why a side said goodbye (proto enum ByeReason). 0 PEER_CLOSED, 0x100 PROTOCOL_VERSION,
 * 0x101 LIMIT_VIOLATION, 0x102 AUTH_FAILED, 0x103 PROTOCOL_VIOLATION, 0x104 SESSION_GONE,
 * 0x105 SERVER_SHUTDOWN.
 */
export type ByeReason = 0 | 0x100 | 0x101 | 0x102 | 0x103 | 0x104 | 0x105;

/**
 * Why a surface went (proto enum SurfaceGoneReason). 0 APP_CLOSED, 1 PARENT_GONE,
 * 2 SESSION_END.
 */
export type SurfaceGoneReason = 0 | 1 | 2;

/** Where a popup goes, relative to its parent. Absent sub-messages mean the field was not sent. */
export interface Positioner {
  /** The rectangle in the parent's coordinates the popup is placed against. */
  anchorRect?: Rect | undefined;
  /** The point on anchor_rect the popup hangs from. */
  anchor: Anchor;
  /** The direction the popup grows from the anchor point. */
  gravity: Anchor;
  /** A last shift, applied after anchor and gravity. */
  offset?: Point | undefined;
  /** The popup's size. */
  size?: Size | undefined;
}

/** C -> S, and it must be the first message after the WebSocket is open. */
export interface Hello {
  protocolVersion: number;
  /** Informational, untrusted text. */
  clientName: string;
  /** Opaque per-session credential bytes. */
  streamToken: Uint8Array;
  /** Tile codec ids the client can decode, best first. Empty means: only codec 1 (RAW). */
  codecs: number[];
  /** Present to reattach to a session that lost its socket. */
  resumeSerial?: number | undefined;
}

/** S -> C, the answer to Hello. */
export interface HelloReply {
  protocolVersion: number;
  /** Untrusted text for display. */
  sessionId: string;
  maxFrameCredits: number;
  /** A subset of the client's offer. */
  codecs: number[];
  resumed: boolean;
  resumeSerial?: number | undefined;
  resumeGraceMs?: number | undefined;
}

/** Either side, before closing the socket. */
export interface Bye {
  reason: ByeReason;
  /** Untrusted text for display. */
  text: string;
}

/** S -> C, immediately before a close the client caused. Codes: see the .proto. */
export interface ServerError {
  code: number;
  /** Untrusted text for display. */
  text: string;
}

/** S -> C. A window appeared and becomes a surface the host shows. */
export interface SurfaceNew {
  surfaceId: number;
  role: Role;
  /** A dialog's toplevel parent, or a popup's parent surface. */
  parentId?: number | undefined;
  /** The size the surface starts with. */
  size?: Size | undefined;
  /** Untrusted text. */
  title: string;
  /** Untrusted text. */
  appId: string;
  /** Present for ROLE_POPUP: how the popup is placed inside its parent. */
  positioner?: Positioner | undefined;
  /** Device pixels per logical pixel in 120ths. Never zero. */
  scale120ths: number;
}

/** S -> C. The surface is gone; its id is never reused. */
export interface SurfaceGone {
  surfaceId: number;
  reason: SurfaceGoneReason;
}

/** S -> C. Title, app id or scale changed. Absent fields are unchanged. */
export interface SurfaceMetadata {
  surfaceId: number;
  title?: string | undefined;
  appId?: string | undefined;
  /** Never zero when present. */
  scale120ths?: number | undefined;
}

/** S -> C. The app asked for the keyboard focus; the host decides. */
export interface FocusAsk {
  surfaceId: number;
}

/** S -> C. The app asked to resize itself; the host decides. */
export interface ResizeAsk {
  surfaceId: number;
  size?: Size | undefined;
}

/** C -> S. The host proposes a size; the serial names this proposal. */
export interface Configure {
  surfaceId: number;
  serial: number;
  size?: Size | undefined;
}

/** S -> C. The app applied - or clamped - the configure named by the serial. */
export interface ConfigureAck {
  surfaceId: number;
  serial: number;
  size?: Size | undefined;
}

/** One encoded piece of one frame. rect is in surface coordinates, before scale. */
export interface Tile {
  rect?: Rect | undefined;
  /** A codec id (see limits.ts CODEC). 0 is a protocol violation. */
  codec: number;
  /** The encoded pixels. */
  data: Uint8Array;
}

/** S -> C. The changed pixels of one surface since the previous acknowledged frame. */
export interface Frame {
  surfaceId: number;
  sequence: number;
  fullRedraw: boolean;
  tiles: Tile[];
}

/** C -> S. The client has drawn this frame; the server frees the credit. */
export interface FrameAck {
  surfaceId: number;
  sequence: number;
}

/** S -> C. The cursor image changed. argb_premultiplied is width*height*4 bytes. */
export interface CursorImage {
  serial: number;
  width: number;
  height: number;
  hotspotX: number;
  hotspotY: number;
  argbPremultiplied: Uint8Array;
}

/** S -> C. The cursor left the visible set; draw the host's own cursor again. */
// eslint-disable-next-line @typescript-eslint/no-empty-object-type -- the proto message has no fields
export interface CursorGone {}

/** C -> S. The pointer moved, surface-local. */
export interface PointerMove {
  surfaceId: number;
  x: number;
  y: number;
}

/** C -> S. A button changed. button is the X button number: 1 left, 2 middle, 3 right. */
export interface PointerButton {
  surfaceId: number;
  button: number;
  pressed: boolean;
}

/** C -> S. The wheel moved by whole steps. */
export interface PointerAxis {
  surfaceId: number;
  stepsX: number;
  stepsY: number;
}

/** C -> S. A key changed. modifiers: 1 shift, 2 lock, 4 control, 8 alt, 16 meta, 32 altgr. */
export interface Key {
  keysym: number;
  /** The browser's physical key name (KeyboardEvent.code), ASCII. */
  code: string;
  pressed: boolean;
  modifiers: number;
}

/** C -> S. The host moved its focus to this surface. */
export interface FocusNotify {
  surfaceId: number;
}

/** C -> S. Focus left every APPricot surface; the server releases every key and button. */
// eslint-disable-next-line @typescript-eslint/no-empty-object-type -- the proto message has no fields
export interface BlurRelease {}

/** C -> S. The host allows a paste. */
export interface ClipboardSet {
  text: string;
}

/** S -> C. The app pasted and the streamer holds no clipboard text. */
// eslint-disable-next-line @typescript-eslint/no-empty-object-type -- the proto message has no fields
export interface ClipboardAsk {}

/** C -> S. The host user closed this window's chrome. It is a request. */
export interface CloseRequest {
  surfaceId: number;
}

/**
 * One protocol message, one per WebSocket binary message. The `kind` is the lowerCamel name of
 * the .proto oneof field, and each variant carries that message under the same name.
 */
export type Envelope =
  | { kind: 'hello'; hello: Hello }
  | { kind: 'helloReply'; helloReply: HelloReply }
  | { kind: 'bye'; bye: Bye }
  | { kind: 'serverError'; serverError: ServerError }
  | { kind: 'surfaceNew'; surfaceNew: SurfaceNew }
  | { kind: 'surfaceGone'; surfaceGone: SurfaceGone }
  | { kind: 'surfaceMetadata'; surfaceMetadata: SurfaceMetadata }
  | { kind: 'focusAsk'; focusAsk: FocusAsk }
  | { kind: 'resizeAsk'; resizeAsk: ResizeAsk }
  | { kind: 'configure'; configure: Configure }
  | { kind: 'configureAck'; configureAck: ConfigureAck }
  | { kind: 'frame'; frame: Frame }
  | { kind: 'frameAck'; frameAck: FrameAck }
  | { kind: 'cursorImage'; cursorImage: CursorImage }
  | { kind: 'cursorGone'; cursorGone: CursorGone }
  | { kind: 'pointerMove'; pointerMove: PointerMove }
  | { kind: 'pointerButton'; pointerButton: PointerButton }
  | { kind: 'pointerAxis'; pointerAxis: PointerAxis }
  | { kind: 'key'; key: Key }
  | { kind: 'focusNotify'; focusNotify: FocusNotify }
  | { kind: 'blurRelease'; blurRelease: BlurRelease }
  | { kind: 'clipboardSet'; clipboardSet: ClipboardSet }
  | { kind: 'clipboardAsk'; clipboardAsk: ClipboardAsk }
  | { kind: 'closeRequest'; closeRequest: CloseRequest };

// ---------------------------------------------------------------------------
// Wire primitives
// ---------------------------------------------------------------------------

const WT_VARINT = 0;
const WT_LEN = 2;

const EMPTY_BYTES = new Uint8Array(0);

const textDecoder = new TextDecoder('utf-8', { fatal: true });
const textEncoder = new TextEncoder();

function checkUint32(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value < 0 || value > 0xffffffff) {
    throw new ProtocolError(`${field} is not a uint32`, field);
  }
}

function checkInt32(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value < -0x80000000 || value > 0x7fffffff) {
    throw new ProtocolError(`${field} is not an int32`, field);
  }
}

// v0 enums are closed: an unknown value is a protocol violation and closes the connection
// (wire.proto header, decided 2026-09-20 when the Rust codec landed: prost rejects unknown
// values, and both codecs follow the strictest common rule). New values ship only with a new
// protocol version.
const ROLE_VALUES: readonly number[] = [0, 1];
const ANCHOR_VALUES: readonly number[] = [0, 1, 2, 3, 4, 5, 6, 7, 8];
const BYE_REASON_VALUES: readonly number[] = [0, 0x100, 0x101, 0x102, 0x103, 0x104, 0x105];
const SURFACE_GONE_REASON_VALUES: readonly number[] = [0, 1, 2];

function checkEnumValue(value: number, allowed: readonly number[], field: string): void {
  if (!allowed.includes(value)) {
    throw new ProtocolError(`${field} has unknown enum value ${value}`, field);
  }
}

function zigzag32(value: number): number {
  return ((value << 1) ^ (value >> 31)) >>> 0;
}

function unzigzag32(value: number): number {
  return (value >>> 1) ^ -(value & 1);
}

/** A growable output buffer. Nothing is bounds-checked here: lengths were validated upstream. */
class Writer {
  private buf = new Uint8Array(64);
  private len = 0;

  finish(): Uint8Array {
    return this.buf.slice(0, this.len);
  }

  private room(extra: number): void {
    if (this.len + extra <= this.buf.length) {
      return;
    }
    let capacity = this.buf.length;
    while (capacity < this.len + extra) {
      capacity *= 2;
    }
    const next = new Uint8Array(capacity);
    next.set(this.buf.subarray(0, this.len));
    this.buf = next;
  }

  varint(value: number): void {
    let rest = value;
    while (rest > 0x7f) {
      this.room(1);
      this.buf[this.len] = (rest % 128) | 0x80;
      this.len += 1;
      rest = Math.floor(rest / 128);
    }
    this.room(1);
    this.buf[this.len] = rest;
    this.len += 1;
  }

  tag(field: number, wireType: number): void {
    this.varint((field << 3) | wireType);
  }

  raw(bytes: Uint8Array): void {
    this.room(bytes.length);
    this.buf.set(bytes, this.len);
    this.len += bytes.length;
  }
}

/** A cursor over a checked byte range. Every read throws ProtocolError past its end. */
class Reader {
  private readonly buf: Uint8Array;
  private readonly end: number;
  private pos = 0;

  constructor(buf: Uint8Array, end?: number) {
    this.buf = buf;
    this.end = end ?? buf.length;
  }

  done(): boolean {
    return this.pos === this.end;
  }

  byte(name: string): number {
    if (this.pos >= this.end) {
      throw new ProtocolError(`${name} is truncated`, name);
    }
    const b = this.buf[this.pos];
    if (b === undefined) {
      throw new ProtocolError(`${name} is truncated`, name);
    }
    this.pos += 1;
    return b;
  }

  /** Reads one varint and bounds it to 32 bits; the .proto carries no wider field. */
  u32(name: string): number {
    let value = 0;
    let scale = 1;
    for (let count = 0; ; count += 1) {
      if (count === 10) {
        throw new ProtocolError(`${name}: varint longer than 10 bytes`, name);
      }
      const b = this.byte(name);
      value += (b % 128) * scale;
      scale *= 128;
      if (b < 0x80) {
        if (!Number.isSafeInteger(value) || value > 0xffffffff) {
          throw new ProtocolError(`${name} overflows 32 bits`, name);
        }
        return value;
      }
    }
  }

  /** Bounds-checks a length-delimited range and returns a sub-reader over it (a view, no copy). */
  span(length: number, name: string): Reader {
    if (length > this.end - this.pos) {
      throw new ProtocolError(`${name} runs past the message`, name);
    }
    const sub = new Reader(this.buf.subarray(this.pos, this.pos + length));
    this.pos += length;
    return sub;
  }

  /** Copies `length` bytes out, after the cap and the bounds check and only then. */
  takeBytes(length: number, cap: number, name: string): Uint8Array {
    if (length > cap) {
      throw new ProtocolError(`${name} is ${length} bytes, over the ${cap}-byte cap`, name);
    }
    if (length > this.end - this.pos) {
      throw new ProtocolError(`${name} runs past the message`, name);
    }
    const copy = new Uint8Array(length);
    copy.set(this.buf.subarray(this.pos, this.pos + length));
    this.pos += length;
    return copy;
  }

  /** Decodes a UTF-8 string, after the cap and the bounds check and only then. */
  takeString(length: number, cap: number, name: string): string {
    if (length > cap) {
      throw new ProtocolError(`${name} is ${length} bytes, over the ${cap}-byte cap`, name);
    }
    if (length > this.end - this.pos) {
      throw new ProtocolError(`${name} runs past the message`, name);
    }
    const view = this.buf.subarray(this.pos, this.pos + length);
    this.pos += length;
    try {
      return textDecoder.decode(view);
    } catch {
      throw new ProtocolError(`${name} is not valid UTF-8`, name);
    }
  }
}

function fieldKey(r: Reader, name: string): [field: number, wireType: number] {
  const key = r.u32(name);
  return [key >>> 3, key & 7];
}

function wantWireType(actual: number, expected: number, name: string): void {
  if (actual !== expected) {
    throw new ProtocolError(`${name} has wire type ${actual}, expected ${expected}`, name);
  }
}

// ---------------------------------------------------------------------------
// Shared validation (used by both encode and decode, so neither can emit or accept a violation)
// ---------------------------------------------------------------------------

function checkSurfaceSize(size: Size | undefined, field: string): void {
  if (size === undefined) {
    return;
  }
  if (size.width > MAX_SURFACE_WIDTH) {
    throw new ProtocolError(`${field}.width is over MAX_SURFACE_WIDTH`, field);
  }
  if (size.height > MAX_SURFACE_HEIGHT) {
    throw new ProtocolError(`${field}.height is over MAX_SURFACE_HEIGHT`, field);
  }
}

function checkScale(scale: number | undefined, field: string): void {
  if (scale === 0) {
    throw new ProtocolError(`${field} is zero`, field);
  }
}

function checkKeyCode(code: string): void {
  // Key.code is a physical key name: ASCII (the .proto cites KeyboardEvent.code).
  for (const char of code) {
    const point = char.codePointAt(0);
    if (point === undefined || point > 0x7f) {
      throw new ProtocolError('Key.code is not ASCII', 'Key.code');
    }
  }
}

function checkTile(tile: Tile): void {
  if (tile.codec === 0) {
    throw new ProtocolError('Tile.codec is zero', 'Tile.codec');
  }
  checkUint32(tile.codec, 'Tile.codec');
  if (tile.data.length > MAX_TILE_BYTES) {
    throw new ProtocolError('Tile.data is over MAX_TILE_BYTES', 'Tile.data');
  }
  const rect = tile.rect;
  if (rect !== undefined) {
    if (rect.width > MAX_TILE_WIDTH) {
      throw new ProtocolError('Tile.rect.width is over MAX_TILE_WIDTH', 'Tile.rect.width');
    }
    if (rect.height > MAX_TILE_HEIGHT) {
      throw new ProtocolError('Tile.rect.height is over MAX_TILE_HEIGHT', 'Tile.rect.height');
    }
  }
}

function checkCursorImage(cursor: CursorImage): void {
  if (cursor.width > MAX_CURSOR_WIDTH) {
    throw new ProtocolError('CursorImage.width is over MAX_CURSOR_WIDTH', 'CursorImage.width');
  }
  if (cursor.height > MAX_CURSOR_HEIGHT) {
    throw new ProtocolError('CursorImage.height is over MAX_CURSOR_HEIGHT', 'CursorImage.height');
  }
  if (cursor.argbPremultiplied.length > MAX_CURSOR_BYTES) {
    throw new ProtocolError(
      'CursorImage.argb_premultiplied is over MAX_CURSOR_BYTES',
      'CursorImage.argb_premultiplied',
    );
  }
  if (cursor.argbPremultiplied.length !== cursor.width * cursor.height * 4) {
    throw new ProtocolError(
      'CursorImage.argb_premultiplied is not width*height*4 bytes',
      'CursorImage.argb_premultiplied',
    );
  }
}

// ---------------------------------------------------------------------------
// Field writers (proto3: plain scalars skip their default; `optional` skips only absence)
// ---------------------------------------------------------------------------

function writeUint32(w: Writer, field: number, value: number, name: string): void {
  if (value === 0) {
    return;
  }
  checkUint32(value, name);
  w.tag(field, WT_VARINT);
  w.varint(value);
}

function writeOptUint32(w: Writer, field: number, value: number | undefined, name: string): void {
  if (value === undefined) {
    return;
  }
  checkUint32(value, name);
  w.tag(field, WT_VARINT);
  w.varint(value);
}

function writeEnum(
  w: Writer,
  field: number,
  value: number,
  allowed: readonly number[],
  name: string,
): void {
  // Enums travel as int32 varints, and v0 enums are closed (see checkEnumValue): neither codec
  // ever emits a value outside the set the protocol version defines.
  checkEnumValue(value, allowed, name);
  writeUint32(w, field, value, name);
}

function writeBool(w: Writer, field: number, value: boolean): void {
  if (!value) {
    return;
  }
  w.tag(field, WT_VARINT);
  w.varint(1);
}

function writeSint32(w: Writer, field: number, value: number, name: string): void {
  if (value === 0) {
    return;
  }
  checkInt32(value, name);
  w.tag(field, WT_VARINT);
  w.varint(zigzag32(value));
}

function writeStringAlways(w: Writer, field: number, value: string, cap: number, name: string): void {
  const encoded = textEncoder.encode(value);
  if (encoded.length > cap) {
    throw new ProtocolError(`${name} is ${encoded.length} bytes, over the ${cap}-byte cap`, name);
  }
  w.tag(field, WT_LEN);
  w.varint(encoded.length);
  w.raw(encoded);
}

function writeString(w: Writer, field: number, value: string, cap: number, name: string): void {
  if (value === '') {
    return;
  }
  writeStringAlways(w, field, value, cap, name);
}

function writeOptString(
  w: Writer,
  field: number,
  value: string | undefined,
  cap: number,
  name: string,
): void {
  if (value === undefined) {
    return;
  }
  writeStringAlways(w, field, value, cap, name);
}

function writeBytes(w: Writer, field: number, value: Uint8Array, cap: number, name: string): void {
  if (value.length === 0) {
    return;
  }
  if (value.length > cap) {
    throw new ProtocolError(`${name} is ${value.length} bytes, over the ${cap}-byte cap`, name);
  }
  w.tag(field, WT_LEN);
  w.varint(value.length);
  w.raw(value);
}

/** Writes a sub-message: field tag, length prefix, then the payload in field-number order. */
function writeMessage<T>(
  w: Writer,
  field: number,
  value: T | undefined,
  encode: (w: Writer, value: T) => void,
): void {
  if (value === undefined) {
    return;
  }
  const inner = new Writer();
  encode(inner, value);
  const payload = inner.finish();
  w.tag(field, WT_LEN);
  w.varint(payload.length);
  w.raw(payload);
}

function writePackedUint32(
  w: Writer,
  field: number,
  values: readonly number[],
  cap: number,
  name: string,
): void {
  if (values.length === 0) {
    return;
  }
  if (values.length > cap) {
    throw new ProtocolError(`${name} has ${values.length} entries, over the cap of ${cap}`, name);
  }
  const inner = new Writer();
  for (const value of values) {
    checkUint32(value, name);
    inner.varint(value);
  }
  const payload = inner.finish();
  w.tag(field, WT_LEN);
  w.varint(payload.length);
  w.raw(payload);
}

// ---------------------------------------------------------------------------
// Message encoders
// ---------------------------------------------------------------------------

function encodePoint(w: Writer, m: Point): void {
  writeSint32(w, 1, m.x, 'Point.x');
  writeSint32(w, 2, m.y, 'Point.y');
}

function encodeSize(w: Writer, m: Size): void {
  writeUint32(w, 1, m.width, 'Size.width');
  writeUint32(w, 2, m.height, 'Size.height');
}

function encodeRect(w: Writer, m: Rect): void {
  writeSint32(w, 1, m.x, 'Rect.x');
  writeSint32(w, 2, m.y, 'Rect.y');
  writeUint32(w, 3, m.width, 'Rect.width');
  writeUint32(w, 4, m.height, 'Rect.height');
}

function encodePositioner(w: Writer, m: Positioner): void {
  checkSurfaceSize(m.size, 'Positioner.size');
  writeMessage(w, 1, m.anchorRect, encodeRect);
  writeEnum(w, 2, m.anchor, ANCHOR_VALUES, 'Positioner.anchor');
  writeEnum(w, 3, m.gravity, ANCHOR_VALUES, 'Positioner.gravity');
  writeMessage(w, 4, m.offset, encodePoint);
  writeMessage(w, 5, m.size, encodeSize);
}

function encodeHello(w: Writer, m: Hello): void {
  writeUint32(w, 1, m.protocolVersion, 'Hello.protocol_version');
  writeString(w, 2, m.clientName, MAX_CLIENT_NAME_BYTES, 'Hello.client_name');
  writeBytes(w, 3, m.streamToken, MAX_TOKEN_BYTES, 'Hello.stream_token');
  writePackedUint32(w, 4, m.codecs, MAX_CODECS_OFFERED, 'Hello.codecs');
  writeOptUint32(w, 5, m.resumeSerial, 'Hello.resume_serial');
}

function encodeHelloReply(w: Writer, m: HelloReply): void {
  writeUint32(w, 1, m.protocolVersion, 'HelloReply.protocol_version');
  writeString(w, 2, m.sessionId, MAX_SESSION_ID_BYTES, 'HelloReply.session_id');
  writeUint32(w, 3, m.maxFrameCredits, 'HelloReply.max_frame_credits');
  writePackedUint32(w, 4, m.codecs, MAX_CODECS_OFFERED, 'HelloReply.codecs');
  writeBool(w, 5, m.resumed);
  writeOptUint32(w, 6, m.resumeSerial, 'HelloReply.resume_serial');
  writeOptUint32(w, 7, m.resumeGraceMs, 'HelloReply.resume_grace_ms');
}

function encodeBye(w: Writer, m: Bye): void {
  writeEnum(w, 1, m.reason, BYE_REASON_VALUES, 'Bye.reason');
  writeString(w, 2, m.text, MAX_BYE_TEXT_BYTES, 'Bye.text');
}

function encodeServerError(w: Writer, m: ServerError): void {
  writeUint32(w, 1, m.code, 'ServerError.code');
  writeString(w, 2, m.text, MAX_ERROR_TEXT_BYTES, 'ServerError.text');
}

function encodeSurfaceNew(w: Writer, m: SurfaceNew): void {
  checkSurfaceSize(m.size, 'SurfaceNew.size');
  checkScale(m.scale120ths, 'SurfaceNew.scale_120ths');
  writeUint32(w, 1, m.surfaceId, 'SurfaceNew.surface_id');
  writeEnum(w, 2, m.role, ROLE_VALUES, 'SurfaceNew.role');
  writeOptUint32(w, 3, m.parentId, 'SurfaceNew.parent_id');
  writeMessage(w, 4, m.size, encodeSize);
  writeString(w, 5, m.title, MAX_TITLE_BYTES, 'SurfaceNew.title');
  writeString(w, 6, m.appId, MAX_APP_ID_BYTES, 'SurfaceNew.app_id');
  writeMessage(w, 7, m.positioner, encodePositioner);
  writeUint32(w, 8, m.scale120ths, 'SurfaceNew.scale_120ths');
}

function encodeSurfaceGone(w: Writer, m: SurfaceGone): void {
  writeUint32(w, 1, m.surfaceId, 'SurfaceGone.surface_id');
  writeEnum(w, 2, m.reason, SURFACE_GONE_REASON_VALUES, 'SurfaceGone.reason');
}

function encodeSurfaceMetadata(w: Writer, m: SurfaceMetadata): void {
  if (m.scale120ths !== undefined) {
    checkScale(m.scale120ths, 'SurfaceMetadata.scale_120ths');
  }
  writeUint32(w, 1, m.surfaceId, 'SurfaceMetadata.surface_id');
  writeOptString(w, 2, m.title, MAX_TITLE_BYTES, 'SurfaceMetadata.title');
  writeOptString(w, 3, m.appId, MAX_APP_ID_BYTES, 'SurfaceMetadata.app_id');
  writeOptUint32(w, 4, m.scale120ths, 'SurfaceMetadata.scale_120ths');
}

function encodeFocusAsk(w: Writer, m: FocusAsk): void {
  writeUint32(w, 1, m.surfaceId, 'FocusAsk.surface_id');
}

function encodeResizeAsk(w: Writer, m: ResizeAsk): void {
  checkSurfaceSize(m.size, 'ResizeAsk.size');
  writeUint32(w, 1, m.surfaceId, 'ResizeAsk.surface_id');
  writeMessage(w, 2, m.size, encodeSize);
}

function encodeConfigure(w: Writer, m: Configure): void {
  checkSurfaceSize(m.size, 'Configure.size');
  writeUint32(w, 1, m.surfaceId, 'Configure.surface_id');
  writeUint32(w, 2, m.serial, 'Configure.serial');
  writeMessage(w, 3, m.size, encodeSize);
}

function encodeConfigureAck(w: Writer, m: ConfigureAck): void {
  checkSurfaceSize(m.size, 'ConfigureAck.size');
  writeUint32(w, 1, m.surfaceId, 'ConfigureAck.surface_id');
  writeUint32(w, 2, m.serial, 'ConfigureAck.serial');
  writeMessage(w, 3, m.size, encodeSize);
}

function encodeTileMessage(w: Writer, m: Tile): void {
  checkTile(m);
  writeMessage(w, 1, m.rect, encodeRect);
  writeUint32(w, 2, m.codec, 'Tile.codec');
  writeBytes(w, 3, m.data, MAX_TILE_BYTES, 'Tile.data');
}

function encodeFrame(w: Writer, m: Frame): void {
  if (m.tiles.length > MAX_TILES_PER_FRAME) {
    throw new ProtocolError(
      `Frame.tiles has ${m.tiles.length} entries, over MAX_TILES_PER_FRAME`,
      'Frame.tiles',
    );
  }
  for (const tile of m.tiles) {
    checkTile(tile);
  }
  writeUint32(w, 1, m.surfaceId, 'Frame.surface_id');
  writeUint32(w, 2, m.sequence, 'Frame.sequence');
  writeBool(w, 3, m.fullRedraw);
  for (const tile of m.tiles) {
    writeMessage(w, 4, tile, encodeTileMessage);
  }
}

function encodeFrameAck(w: Writer, m: FrameAck): void {
  writeUint32(w, 1, m.surfaceId, 'FrameAck.surface_id');
  writeUint32(w, 2, m.sequence, 'FrameAck.sequence');
}

function encodeCursorImage(w: Writer, m: CursorImage): void {
  checkCursorImage(m);
  writeUint32(w, 1, m.serial, 'CursorImage.serial');
  writeUint32(w, 2, m.width, 'CursorImage.width');
  writeUint32(w, 3, m.height, 'CursorImage.height');
  writeUint32(w, 4, m.hotspotX, 'CursorImage.hotspot_x');
  writeUint32(w, 5, m.hotspotY, 'CursorImage.hotspot_y');
  writeBytes(w, 6, m.argbPremultiplied, MAX_CURSOR_BYTES, 'CursorImage.argb_premultiplied');
}

function encodePointerMove(w: Writer, m: PointerMove): void {
  writeUint32(w, 1, m.surfaceId, 'PointerMove.surface_id');
  writeSint32(w, 2, m.x, 'PointerMove.x');
  writeSint32(w, 3, m.y, 'PointerMove.y');
}

function encodePointerButton(w: Writer, m: PointerButton): void {
  writeUint32(w, 1, m.surfaceId, 'PointerButton.surface_id');
  writeUint32(w, 2, m.button, 'PointerButton.button');
  writeBool(w, 3, m.pressed);
}

function encodePointerAxis(w: Writer, m: PointerAxis): void {
  writeUint32(w, 1, m.surfaceId, 'PointerAxis.surface_id');
  writeSint32(w, 2, m.stepsX, 'PointerAxis.steps_x');
  writeSint32(w, 3, m.stepsY, 'PointerAxis.steps_y');
}

function encodeKey(w: Writer, m: Key): void {
  checkKeyCode(m.code);
  writeUint32(w, 1, m.keysym, 'Key.keysym');
  writeString(w, 2, m.code, MAX_KEY_CODE_BYTES, 'Key.code');
  writeBool(w, 3, m.pressed);
  writeUint32(w, 4, m.modifiers, 'Key.modifiers');
}

function encodeFocusNotify(w: Writer, m: FocusNotify): void {
  writeUint32(w, 1, m.surfaceId, 'FocusNotify.surface_id');
}

/** CursorGone, BlurRelease and ClipboardAsk carry no fields: nothing to encode. */
function encodeNoFields(): void {
  return undefined;
}

function encodeClipboardSet(w: Writer, m: ClipboardSet): void {
  writeString(w, 1, m.text, MAX_CLIPBOARD_BYTES, 'ClipboardSet.text');
}

function encodeCloseRequest(w: Writer, m: CloseRequest): void {
  writeUint32(w, 1, m.surfaceId, 'CloseRequest.surface_id');
}

// ---------------------------------------------------------------------------
// Message decoders
// ---------------------------------------------------------------------------

function decodePoint(r: Reader): Point {
  const m: Point = { x: 0, y: 0 };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'Point field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'Point.x');
        m.x = unzigzag32(r.u32('Point.x'));
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'Point.y');
        m.y = unzigzag32(r.u32('Point.y'));
        break;
      }
      default:
        throw new ProtocolError(`Point has unknown field ${field}`, 'Point');
    }
  }
  return m;
}

function decodeSize(r: Reader, field: string): Size {
  const m: Size = { width: 0, height: 0 };
  while (!r.done()) {
    const [fieldNumber, wireType] = fieldKey(r, `${field} field key`);
    switch (fieldNumber) {
      case 1: {
        wantWireType(wireType, WT_VARINT, `${field}.width`);
        m.width = r.u32(`${field}.width`);
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, `${field}.height`);
        m.height = r.u32(`${field}.height`);
        break;
      }
      default:
        throw new ProtocolError(`${field} has unknown field ${fieldNumber}`, field);
    }
  }
  return m;
}

function decodeRect(r: Reader): Rect {
  const m: Rect = { x: 0, y: 0, width: 0, height: 0 };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'Rect field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'Rect.x');
        m.x = unzigzag32(r.u32('Rect.x'));
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'Rect.y');
        m.y = unzigzag32(r.u32('Rect.y'));
        break;
      }
      case 3: {
        wantWireType(wireType, WT_VARINT, 'Rect.width');
        m.width = r.u32('Rect.width');
        break;
      }
      case 4: {
        wantWireType(wireType, WT_VARINT, 'Rect.height');
        m.height = r.u32('Rect.height');
        break;
      }
      default:
        throw new ProtocolError(`Rect has unknown field ${field}`, 'Rect');
    }
  }
  return m;
}

function decodeSubMessage<T>(r: Reader, wireType: number, name: string, decode: (r: Reader) => T): T {
  wantWireType(wireType, WT_LEN, name);
  const length = r.u32(`${name} length`);
  return decode(r.span(length, name));
}

function decodePositioner(r: Reader): Positioner {
  const m: Positioner = { anchorRect: undefined, anchor: 0, gravity: 0, offset: undefined, size: undefined };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'Positioner field key');
    switch (field) {
      case 1: {
        m.anchorRect = decodeSubMessage(r, wireType, 'Positioner.anchor_rect', decodeRect);
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'Positioner.anchor');
        const anchor = r.u32('Positioner.anchor');
        checkEnumValue(anchor, ANCHOR_VALUES, 'Positioner.anchor');
        m.anchor = anchor as Anchor;
        break;
      }
      case 3: {
        wantWireType(wireType, WT_VARINT, 'Positioner.gravity');
        const gravity = r.u32('Positioner.gravity');
        checkEnumValue(gravity, ANCHOR_VALUES, 'Positioner.gravity');
        m.gravity = gravity as Anchor;
        break;
      }
      case 4: {
        m.offset = decodeSubMessage(r, wireType, 'Positioner.offset', decodePoint);
        break;
      }
      case 5: {
        m.size = decodeSubMessage(r, wireType, 'Positioner.size', (sub) => decodeSize(sub, 'Positioner.size'));
        break;
      }
      default:
        throw new ProtocolError(`Positioner has unknown field ${field}`, 'Positioner');
    }
  }
  checkSurfaceSize(m.size, 'Positioner.size');
  return m;
}

/** Reads one `repeated uint32 codecs` field; packed (proto3 default) and unpacked are both legal. */
function readCodecs(r: Reader, wireType: number, into: number[], field: string): void {
  const push = (value: number): void => {
    into.push(value);
    if (into.length > MAX_CODECS_OFFERED) {
      throw new ProtocolError(`${field} has more than MAX_CODECS_OFFERED entries`, field);
    }
  };
  if (wireType === WT_LEN) {
    const length = r.u32(`${field} length`);
    const sub = r.span(length, field);
    while (!sub.done()) {
      push(sub.u32(`${field} entry`));
    }
  } else if (wireType === WT_VARINT) {
    push(r.u32(`${field} entry`));
  } else {
    throw new ProtocolError(`${field} has wire type ${wireType}, expected 0 or 2`, field);
  }
}

function decodeHello(r: Reader): Hello {
  const m: Hello = {
    protocolVersion: 0,
    clientName: '',
    streamToken: EMPTY_BYTES,
    codecs: [],
    resumeSerial: undefined,
  };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'Hello field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'Hello.protocol_version');
        m.protocolVersion = r.u32('Hello.protocol_version');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_LEN, 'Hello.client_name');
        const length = r.u32('Hello.client_name length');
        m.clientName = r.takeString(length, MAX_CLIENT_NAME_BYTES, 'Hello.client_name');
        break;
      }
      case 3: {
        wantWireType(wireType, WT_LEN, 'Hello.stream_token');
        const length = r.u32('Hello.stream_token length');
        m.streamToken = r.takeBytes(length, MAX_TOKEN_BYTES, 'Hello.stream_token');
        break;
      }
      case 4: {
        readCodecs(r, wireType, m.codecs, 'Hello.codecs');
        break;
      }
      case 5: {
        wantWireType(wireType, WT_VARINT, 'Hello.resume_serial');
        m.resumeSerial = r.u32('Hello.resume_serial');
        break;
      }
      default:
        throw new ProtocolError(`Hello has unknown field ${field}`, 'Hello');
    }
  }
  return m;
}

function decodeHelloReply(r: Reader): HelloReply {
  const m: HelloReply = {
    protocolVersion: 0,
    sessionId: '',
    maxFrameCredits: 0,
    codecs: [],
    resumed: false,
    resumeSerial: undefined,
    resumeGraceMs: undefined,
  };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'HelloReply field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'HelloReply.protocol_version');
        m.protocolVersion = r.u32('HelloReply.protocol_version');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_LEN, 'HelloReply.session_id');
        const length = r.u32('HelloReply.session_id length');
        m.sessionId = r.takeString(length, MAX_SESSION_ID_BYTES, 'HelloReply.session_id');
        break;
      }
      case 3: {
        wantWireType(wireType, WT_VARINT, 'HelloReply.max_frame_credits');
        m.maxFrameCredits = r.u32('HelloReply.max_frame_credits');
        break;
      }
      case 4: {
        readCodecs(r, wireType, m.codecs, 'HelloReply.codecs');
        break;
      }
      case 5: {
        wantWireType(wireType, WT_VARINT, 'HelloReply.resumed');
        m.resumed = r.u32('HelloReply.resumed') !== 0;
        break;
      }
      case 6: {
        wantWireType(wireType, WT_VARINT, 'HelloReply.resume_serial');
        m.resumeSerial = r.u32('HelloReply.resume_serial');
        break;
      }
      case 7: {
        wantWireType(wireType, WT_VARINT, 'HelloReply.resume_grace_ms');
        m.resumeGraceMs = r.u32('HelloReply.resume_grace_ms');
        break;
      }
      default:
        throw new ProtocolError(`HelloReply has unknown field ${field}`, 'HelloReply');
    }
  }
  return m;
}

function decodeBye(r: Reader): Bye {
  const m: Bye = { reason: 0, text: '' };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'Bye field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'Bye.reason');
        const reason = r.u32('Bye.reason');
        checkEnumValue(reason, BYE_REASON_VALUES, 'Bye.reason');
        m.reason = reason as ByeReason;
        break;
      }
      case 2: {
        wantWireType(wireType, WT_LEN, 'Bye.text');
        const length = r.u32('Bye.text length');
        m.text = r.takeString(length, MAX_BYE_TEXT_BYTES, 'Bye.text');
        break;
      }
      default:
        throw new ProtocolError(`Bye has unknown field ${field}`, 'Bye');
    }
  }
  return m;
}

function decodeServerError(r: Reader): ServerError {
  const m: ServerError = { code: 0, text: '' };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'ServerError field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'ServerError.code');
        m.code = r.u32('ServerError.code');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_LEN, 'ServerError.text');
        const length = r.u32('ServerError.text length');
        m.text = r.takeString(length, MAX_ERROR_TEXT_BYTES, 'ServerError.text');
        break;
      }
      default:
        throw new ProtocolError(`ServerError has unknown field ${field}`, 'ServerError');
    }
  }
  return m;
}

function decodeSurfaceNew(r: Reader): SurfaceNew {
  const m: SurfaceNew = {
    surfaceId: 0,
    role: 0,
    parentId: undefined,
    size: undefined,
    title: '',
    appId: '',
    positioner: undefined,
    scale120ths: 0,
  };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'SurfaceNew field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'SurfaceNew.surface_id');
        m.surfaceId = r.u32('SurfaceNew.surface_id');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'SurfaceNew.role');
        const role = r.u32('SurfaceNew.role');
        checkEnumValue(role, ROLE_VALUES, 'SurfaceNew.role');
        m.role = role as Role;
        break;
      }
      case 3: {
        wantWireType(wireType, WT_VARINT, 'SurfaceNew.parent_id');
        m.parentId = r.u32('SurfaceNew.parent_id');
        break;
      }
      case 4: {
        m.size = decodeSubMessage(r, wireType, 'SurfaceNew.size', (sub) => decodeSize(sub, 'SurfaceNew.size'));
        break;
      }
      case 5: {
        wantWireType(wireType, WT_LEN, 'SurfaceNew.title');
        const length = r.u32('SurfaceNew.title length');
        m.title = r.takeString(length, MAX_TITLE_BYTES, 'SurfaceNew.title');
        break;
      }
      case 6: {
        wantWireType(wireType, WT_LEN, 'SurfaceNew.app_id');
        const length = r.u32('SurfaceNew.app_id length');
        m.appId = r.takeString(length, MAX_APP_ID_BYTES, 'SurfaceNew.app_id');
        break;
      }
      case 7: {
        m.positioner = decodeSubMessage(r, wireType, 'SurfaceNew.positioner', decodePositioner);
        break;
      }
      case 8: {
        wantWireType(wireType, WT_VARINT, 'SurfaceNew.scale_120ths');
        m.scale120ths = r.u32('SurfaceNew.scale_120ths');
        break;
      }
      default:
        throw new ProtocolError(`SurfaceNew has unknown field ${field}`, 'SurfaceNew');
    }
  }
  checkSurfaceSize(m.size, 'SurfaceNew.size');
  checkScale(m.scale120ths, 'SurfaceNew.scale_120ths');
  return m;
}

function decodeSurfaceGone(r: Reader): SurfaceGone {
  const m: SurfaceGone = { surfaceId: 0, reason: 0 };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'SurfaceGone field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'SurfaceGone.surface_id');
        m.surfaceId = r.u32('SurfaceGone.surface_id');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'SurfaceGone.reason');
        const reason = r.u32('SurfaceGone.reason');
        checkEnumValue(reason, SURFACE_GONE_REASON_VALUES, 'SurfaceGone.reason');
        m.reason = reason as SurfaceGoneReason;
        break;
      }
      default:
        throw new ProtocolError(`SurfaceGone has unknown field ${field}`, 'SurfaceGone');
    }
  }
  return m;
}

function decodeFocusAsk(r: Reader): FocusAsk {
  const m: FocusAsk = { surfaceId: 0 };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'FocusAsk field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'FocusAsk.surface_id');
        m.surfaceId = r.u32('FocusAsk.surface_id');
        break;
      }
      default:
        throw new ProtocolError(`FocusAsk has unknown field ${field}`, 'FocusAsk');
    }
  }
  return m;
}

function decodeSurfaceMetadata(r: Reader): SurfaceMetadata {
  const m: SurfaceMetadata = { surfaceId: 0, title: undefined, appId: undefined, scale120ths: undefined };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'SurfaceMetadata field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'SurfaceMetadata.surface_id');
        m.surfaceId = r.u32('SurfaceMetadata.surface_id');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_LEN, 'SurfaceMetadata.title');
        const length = r.u32('SurfaceMetadata.title length');
        m.title = r.takeString(length, MAX_TITLE_BYTES, 'SurfaceMetadata.title');
        break;
      }
      case 3: {
        wantWireType(wireType, WT_LEN, 'SurfaceMetadata.app_id');
        const length = r.u32('SurfaceMetadata.app_id length');
        m.appId = r.takeString(length, MAX_APP_ID_BYTES, 'SurfaceMetadata.app_id');
        break;
      }
      case 4: {
        wantWireType(wireType, WT_VARINT, 'SurfaceMetadata.scale_120ths');
        m.scale120ths = r.u32('SurfaceMetadata.scale_120ths');
        break;
      }
      default:
        throw new ProtocolError(`SurfaceMetadata has unknown field ${field}`, 'SurfaceMetadata');
    }
  }
  checkScale(m.scale120ths, 'SurfaceMetadata.scale_120ths');
  return m;
}

function decodeResizeAsk(r: Reader): ResizeAsk {
  const m: ResizeAsk = { surfaceId: 0, size: undefined };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'ResizeAsk field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'ResizeAsk.surface_id');
        m.surfaceId = r.u32('ResizeAsk.surface_id');
        break;
      }
      case 2: {
        m.size = decodeSubMessage(r, wireType, 'ResizeAsk.size', (sub) => decodeSize(sub, 'ResizeAsk.size'));
        break;
      }
      default:
        throw new ProtocolError(`ResizeAsk has unknown field ${field}`, 'ResizeAsk');
    }
  }
  checkSurfaceSize(m.size, 'ResizeAsk.size');
  return m;
}

function decodeConfigure(r: Reader, kind: 'Configure' | 'ConfigureAck'): Configure {
  const m: Configure = { surfaceId: 0, serial: 0, size: undefined };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, `${kind} field key`);
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, `${kind}.surface_id`);
        m.surfaceId = r.u32(`${kind}.surface_id`);
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, `${kind}.serial`);
        m.serial = r.u32(`${kind}.serial`);
        break;
      }
      case 3: {
        m.size = decodeSubMessage(r, wireType, `${kind}.size`, (sub) => decodeSize(sub, `${kind}.size`));
        break;
      }
      default:
        throw new ProtocolError(`${kind} has unknown field ${field}`, kind);
    }
  }
  checkSurfaceSize(m.size, `${kind}.size`);
  return m;
}

function decodeTileMessage(r: Reader): Tile {
  const m: Tile = { rect: undefined, codec: 0, data: EMPTY_BYTES };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'Tile field key');
    switch (field) {
      case 1: {
        m.rect = decodeSubMessage(r, wireType, 'Tile.rect', decodeRect);
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'Tile.codec');
        m.codec = r.u32('Tile.codec');
        break;
      }
      case 3: {
        wantWireType(wireType, WT_LEN, 'Tile.data');
        const length = r.u32('Tile.data length');
        m.data = r.takeBytes(length, MAX_TILE_BYTES, 'Tile.data');
        break;
      }
      default:
        throw new ProtocolError(`Tile has unknown field ${field}`, 'Tile');
    }
  }
  checkTile(m);
  return m;
}

function decodeFrame(r: Reader): Frame {
  const m: Frame = { surfaceId: 0, sequence: 0, fullRedraw: false, tiles: [] };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'Frame field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'Frame.surface_id');
        m.surfaceId = r.u32('Frame.surface_id');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'Frame.sequence');
        m.sequence = r.u32('Frame.sequence');
        break;
      }
      case 3: {
        wantWireType(wireType, WT_VARINT, 'Frame.full_redraw');
        m.fullRedraw = r.u32('Frame.full_redraw') !== 0;
        break;
      }
      case 4: {
        const tile = decodeSubMessage(r, wireType, 'Frame.tiles', decodeTileMessage);
        m.tiles.push(tile);
        if (m.tiles.length > MAX_TILES_PER_FRAME) {
          throw new ProtocolError('Frame.tiles has more than MAX_TILES_PER_FRAME entries', 'Frame.tiles');
        }
        break;
      }
      default:
        throw new ProtocolError(`Frame has unknown field ${field}`, 'Frame');
    }
  }
  return m;
}

function decodeFrameAck(r: Reader): FrameAck {
  const m: FrameAck = { surfaceId: 0, sequence: 0 };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'FrameAck field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'FrameAck.surface_id');
        m.surfaceId = r.u32('FrameAck.surface_id');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'FrameAck.sequence');
        m.sequence = r.u32('FrameAck.sequence');
        break;
      }
      default:
        throw new ProtocolError(`FrameAck has unknown field ${field}`, 'FrameAck');
    }
  }
  return m;
}

function decodeCursorImage(r: Reader): CursorImage {
  const m: CursorImage = {
    serial: 0,
    width: 0,
    height: 0,
    hotspotX: 0,
    hotspotY: 0,
    argbPremultiplied: EMPTY_BYTES,
  };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'CursorImage field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'CursorImage.serial');
        m.serial = r.u32('CursorImage.serial');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'CursorImage.width');
        m.width = r.u32('CursorImage.width');
        break;
      }
      case 3: {
        wantWireType(wireType, WT_VARINT, 'CursorImage.height');
        m.height = r.u32('CursorImage.height');
        break;
      }
      case 4: {
        wantWireType(wireType, WT_VARINT, 'CursorImage.hotspot_x');
        m.hotspotX = r.u32('CursorImage.hotspot_x');
        break;
      }
      case 5: {
        wantWireType(wireType, WT_VARINT, 'CursorImage.hotspot_y');
        m.hotspotY = r.u32('CursorImage.hotspot_y');
        break;
      }
      case 6: {
        wantWireType(wireType, WT_LEN, 'CursorImage.argb_premultiplied');
        const length = r.u32('CursorImage.argb_premultiplied length');
        m.argbPremultiplied = r.takeBytes(
          length,
          MAX_CURSOR_BYTES,
          'CursorImage.argb_premultiplied',
        );
        break;
      }
      default:
        throw new ProtocolError(`CursorImage has unknown field ${field}`, 'CursorImage');
    }
  }
  checkCursorImage(m);
  return m;
}

function decodePointerMove(r: Reader): PointerMove {
  const m: PointerMove = { surfaceId: 0, x: 0, y: 0 };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'PointerMove field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'PointerMove.surface_id');
        m.surfaceId = r.u32('PointerMove.surface_id');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'PointerMove.x');
        m.x = unzigzag32(r.u32('PointerMove.x'));
        break;
      }
      case 3: {
        wantWireType(wireType, WT_VARINT, 'PointerMove.y');
        m.y = unzigzag32(r.u32('PointerMove.y'));
        break;
      }
      default:
        throw new ProtocolError(`PointerMove has unknown field ${field}`, 'PointerMove');
    }
  }
  return m;
}

function decodePointerButton(r: Reader): PointerButton {
  const m: PointerButton = { surfaceId: 0, button: 0, pressed: false };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'PointerButton field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'PointerButton.surface_id');
        m.surfaceId = r.u32('PointerButton.surface_id');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'PointerButton.button');
        m.button = r.u32('PointerButton.button');
        break;
      }
      case 3: {
        wantWireType(wireType, WT_VARINT, 'PointerButton.pressed');
        m.pressed = r.u32('PointerButton.pressed') !== 0;
        break;
      }
      default:
        throw new ProtocolError(`PointerButton has unknown field ${field}`, 'PointerButton');
    }
  }
  return m;
}

function decodePointerAxis(r: Reader): PointerAxis {
  const m: PointerAxis = { surfaceId: 0, stepsX: 0, stepsY: 0 };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'PointerAxis field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'PointerAxis.surface_id');
        m.surfaceId = r.u32('PointerAxis.surface_id');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_VARINT, 'PointerAxis.steps_x');
        m.stepsX = unzigzag32(r.u32('PointerAxis.steps_x'));
        break;
      }
      case 3: {
        wantWireType(wireType, WT_VARINT, 'PointerAxis.steps_y');
        m.stepsY = unzigzag32(r.u32('PointerAxis.steps_y'));
        break;
      }
      default:
        throw new ProtocolError(`PointerAxis has unknown field ${field}`, 'PointerAxis');
    }
  }
  return m;
}

function decodeKey(r: Reader): Key {
  const m: Key = { keysym: 0, code: '', pressed: false, modifiers: 0 };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'Key field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, 'Key.keysym');
        m.keysym = r.u32('Key.keysym');
        break;
      }
      case 2: {
        wantWireType(wireType, WT_LEN, 'Key.code');
        const length = r.u32('Key.code length');
        m.code = r.takeString(length, MAX_KEY_CODE_BYTES, 'Key.code');
        checkKeyCode(m.code);
        break;
      }
      case 3: {
        wantWireType(wireType, WT_VARINT, 'Key.pressed');
        m.pressed = r.u32('Key.pressed') !== 0;
        break;
      }
      case 4: {
        wantWireType(wireType, WT_VARINT, 'Key.modifiers');
        m.modifiers = r.u32('Key.modifiers');
        break;
      }
      default:
        throw new ProtocolError(`Key has unknown field ${field}`, 'Key');
    }
  }
  return m;
}

function decodeSurfaceIdOnly(r: Reader, kind: string): number {
  let surfaceId = 0;
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, `${kind} field key`);
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_VARINT, `${kind}.surface_id`);
        surfaceId = r.u32(`${kind}.surface_id`);
        break;
      }
      default:
        throw new ProtocolError(`${kind} has unknown field ${field}`, kind);
    }
  }
  return surfaceId;
}

function decodeClipboardSet(r: Reader): ClipboardSet {
  const m: ClipboardSet = { text: '' };
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'ClipboardSet field key');
    switch (field) {
      case 1: {
        wantWireType(wireType, WT_LEN, 'ClipboardSet.text');
        const length = r.u32('ClipboardSet.text length');
        m.text = r.takeString(length, MAX_CLIPBOARD_BYTES, 'ClipboardSet.text');
        break;
      }
      default:
        throw new ProtocolError(`ClipboardSet has unknown field ${field}`, 'ClipboardSet');
    }
  }
  return m;
}

function decodeEmpty(r: Reader, kind: string): void {
  while (!r.done()) {
    const [field] = fieldKey(r, `${kind} field key`);
    throw new ProtocolError(`${kind} has unknown field ${field}`, kind);
  }
}

// ---------------------------------------------------------------------------
// The envelope
// ---------------------------------------------------------------------------

/** Encodes one Envelope exactly as the Rust peer does: one body, fields in field-number order. */
export function encodeEnvelope(e: Envelope): Uint8Array {
  const w = new Writer();
  switch (e.kind) {
    case 'hello':
      writeMessage(w, 1, e.hello, encodeHello);
      break;
    case 'helloReply':
      writeMessage(w, 2, e.helloReply, encodeHelloReply);
      break;
    case 'bye':
      writeMessage(w, 3, e.bye, encodeBye);
      break;
    case 'serverError':
      writeMessage(w, 4, e.serverError, encodeServerError);
      break;
    case 'surfaceNew':
      writeMessage(w, 5, e.surfaceNew, encodeSurfaceNew);
      break;
    case 'surfaceGone':
      writeMessage(w, 6, e.surfaceGone, encodeSurfaceGone);
      break;
    case 'surfaceMetadata':
      writeMessage(w, 7, e.surfaceMetadata, encodeSurfaceMetadata);
      break;
    case 'focusAsk':
      writeMessage(w, 8, e.focusAsk, encodeFocusAsk);
      break;
    case 'resizeAsk':
      writeMessage(w, 9, e.resizeAsk, encodeResizeAsk);
      break;
    case 'configure':
      writeMessage(w, 10, e.configure, encodeConfigure);
      break;
    case 'configureAck':
      writeMessage(w, 11, e.configureAck, encodeConfigureAck);
      break;
    case 'frame':
      writeMessage(w, 12, e.frame, encodeFrame);
      break;
    case 'frameAck':
      writeMessage(w, 13, e.frameAck, encodeFrameAck);
      break;
    case 'cursorImage':
      writeMessage(w, 14, e.cursorImage, encodeCursorImage);
      break;
    case 'cursorGone':
      writeMessage(w, 15, e.cursorGone, encodeNoFields);
      break;
    case 'pointerMove':
      writeMessage(w, 16, e.pointerMove, encodePointerMove);
      break;
    case 'pointerButton':
      writeMessage(w, 17, e.pointerButton, encodePointerButton);
      break;
    case 'pointerAxis':
      writeMessage(w, 18, e.pointerAxis, encodePointerAxis);
      break;
    case 'key':
      writeMessage(w, 19, e.key, encodeKey);
      break;
    case 'focusNotify':
      writeMessage(w, 20, e.focusNotify, encodeFocusNotify);
      break;
    case 'blurRelease':
      writeMessage(w, 21, e.blurRelease, encodeNoFields);
      break;
    case 'clipboardSet':
      writeMessage(w, 22, e.clipboardSet, encodeClipboardSet);
      break;
    case 'clipboardAsk':
      writeMessage(w, 23, e.clipboardAsk, encodeNoFields);
      break;
    case 'closeRequest':
      writeMessage(w, 24, e.closeRequest, encodeCloseRequest);
      break;
  }
  return w.finish();
}

function decodeBody(field: number, r: Reader): Envelope {
  switch (field) {
    case 1:
      return { kind: 'hello', hello: decodeHello(r) };
    case 2:
      return { kind: 'helloReply', helloReply: decodeHelloReply(r) };
    case 3:
      return { kind: 'bye', bye: decodeBye(r) };
    case 4:
      return { kind: 'serverError', serverError: decodeServerError(r) };
    case 5:
      return { kind: 'surfaceNew', surfaceNew: decodeSurfaceNew(r) };
    case 6:
      return { kind: 'surfaceGone', surfaceGone: decodeSurfaceGone(r) };
    case 7:
      return { kind: 'surfaceMetadata', surfaceMetadata: decodeSurfaceMetadata(r) };
    case 8:
      return { kind: 'focusAsk', focusAsk: decodeFocusAsk(r) };
    case 9:
      return { kind: 'resizeAsk', resizeAsk: decodeResizeAsk(r) };
    case 10:
      return { kind: 'configure', configure: decodeConfigure(r, 'Configure') };
    case 11:
      return { kind: 'configureAck', configureAck: decodeConfigure(r, 'ConfigureAck') };
    case 12:
      return { kind: 'frame', frame: decodeFrame(r) };
    case 13:
      return { kind: 'frameAck', frameAck: decodeFrameAck(r) };
    case 14:
      return { kind: 'cursorImage', cursorImage: decodeCursorImage(r) };
    case 15:
      decodeEmpty(r, 'CursorGone');
      return { kind: 'cursorGone', cursorGone: {} };
    case 16:
      return { kind: 'pointerMove', pointerMove: decodePointerMove(r) };
    case 17:
      return { kind: 'pointerButton', pointerButton: decodePointerButton(r) };
    case 18:
      return { kind: 'pointerAxis', pointerAxis: decodePointerAxis(r) };
    case 19:
      return { kind: 'key', key: decodeKey(r) };
    case 20:
      return { kind: 'focusNotify', focusNotify: { surfaceId: decodeSurfaceIdOnly(r, 'FocusNotify') } };
    case 21:
      decodeEmpty(r, 'BlurRelease');
      return { kind: 'blurRelease', blurRelease: {} };
    case 22:
      return { kind: 'clipboardSet', clipboardSet: decodeClipboardSet(r) };
    case 23:
      decodeEmpty(r, 'ClipboardAsk');
      return { kind: 'clipboardAsk', clipboardAsk: {} };
    case 24:
      return { kind: 'closeRequest', closeRequest: { surfaceId: decodeSurfaceIdOnly(r, 'CloseRequest') } };
    default:
      throw new ProtocolError(
        `envelope names unknown oneof field ${field}`,
        'envelope',
      );
  }
}

/**
 * Decodes one Envelope from the bytes of a single WebSocket binary message.
 *
 * The input cap is checked first, then every per-field cap is checked before a copy is
 * allocated (docs/protocol/README.md rule 3). An envelope that names no known oneof field, a
 * field number no v0 message defines, or bytes that are not valid UTF-8 are all protocol
 * violations and throw ProtocolError; the caller closes the connection (BYE_PROTOCOL_VIOLATION).
 */
export function decodeEnvelope(bytes: Uint8Array): Envelope {
  if (bytes.length > MAX_MESSAGE_BYTES) {
    throw new ProtocolError(
      `message is ${bytes.length} bytes, over MAX_MESSAGE_BYTES`,
      'envelope',
    );
  }
  const r = new Reader(bytes);
  let out: Envelope | undefined;
  while (!r.done()) {
    const [field, wireType] = fieldKey(r, 'envelope field key');
    if (wireType !== WT_LEN) {
      throw new ProtocolError(
        `envelope field ${field} has wire type ${wireType}, expected 2`,
        'envelope',
      );
    }
    const length = r.u32(`envelope field ${field} length`);
    const sub = r.span(length, `envelope field ${field}`);
    if (out !== undefined) {
      throw new ProtocolError('envelope carries more than one body', 'envelope');
    }
    out = decodeBody(field, sub);
  }
  if (out === undefined) {
    throw new ProtocolError('envelope names no message', 'envelope');
  }
  return out;
}
