// The node environment, not jsdom: this file reads vectors.json from the repository through
// `new URL(..., import.meta.url)`, and under the jsdom environment that URL resolves against
// jsdom's page instead of the file system.
import { existsSync, readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import type { Envelope, Positioner } from './wire';
import { ProtocolError, decodeEnvelope, encodeEnvelope } from './wire';
import { CODEC } from './limits';
import { decodeTile } from './tile';

/**
 * The node environment has no ImageData global either, so the test provides the constructor a
 * browser would, exactly as in tile.test.ts: the (width, height) constructor and a .data of
 * width*height*4 bytes.
 */
class ImageDataStub {
  readonly width: number;
  readonly height: number;
  readonly data: Uint8ClampedArray;

  constructor(width: number, height: number) {
    this.width = width;
    this.height = height;
    this.data = new Uint8ClampedArray(width * height * 4);
  }
}

{
  const globals = globalThis as unknown as { ImageData?: unknown };
  if (globals.ImageData === undefined) {
    globals.ImageData = ImageDataStub;
  }
}

/**
 * The shared test vectors: the Rust codec (crates/appricot-proto) writes
 * testdata/vectors.json and this file proves the TypeScript mirror decodes every vector and
 * re-encodes it byte-for-byte identically (docs/protocol/README.md rule 10), accepts every
 * lenient entry as the canonical message it names, and refuses every invalid entry. The format
 * is pinned: { format: 2, protocol_version: 0, vectors: [{ name, note, hex }], lenient: [{ name,
 * note, hex, canonical }], invalid: [{ name, note, hex, error, field }] } where each hex string
 * is one full Envelope-encoded message (docs/protocol/v0.md section 12).
 */

interface VectorEntry {
  name: string;
  note?: string;
  hex: string;
}

/** Bytes no encoder writes that both decoders accept, as the message `canonical` encodes. */
interface LenientEntry extends VectorEntry {
  canonical: string;
}

/** Bytes both decoders refuse; `error` and `field` are what the Rust decoder answers. */
interface InvalidEntry extends VectorEntry {
  error: 'LimitViolation' | 'ProtocolViolation' | 'UnknownMessage';
  field: string;
}

interface VectorFile {
  format: number;
  protocol_version: number;
  vectors: VectorEntry[];
  lenient: LenientEntry[];
  invalid: InvalidEntry[];
}

const vectorsUrl = new URL('../../../crates/appricot-proto/testdata/vectors.json', import.meta.url);

function loadVectors(): VectorFile {
  if (!existsSync(vectorsUrl)) {
    throw new Error('vectors.json missing - w1 must land first');
  }
  const parsed = JSON.parse(readFileSync(vectorsUrl, 'utf8')) as VectorFile;
  if (
    typeof parsed.format !== 'number' ||
    typeof parsed.protocol_version !== 'number' ||
    !Array.isArray(parsed.vectors) ||
    parsed.vectors.length === 0 ||
    !Array.isArray(parsed.lenient) ||
    !Array.isArray(parsed.invalid)
  ) {
    throw new Error('vectors.json has an unexpected shape');
  }
  return parsed;
}

const file = loadVectors();

function hexToBytes(hex: string): Uint8Array {
  const clean = hex.trim();
  // Empty is allowed: the `empty-envelope` invalid entry is zero bytes.
  if (clean.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(clean)) {
    throw new Error(`vector hex is malformed: ${clean.slice(0, 40)}`);
  }
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function bytesToHex(bytes: Uint8Array): string {
  let out = '';
  for (const byte of bytes) {
    out += byte.toString(16).padStart(2, '0');
  }
  return out;
}

function vectorsMatching(pattern: RegExp): VectorEntry[] {
  return file.vectors.filter((vector) => pattern.test(vector.name));
}

function decodeNamed(pattern: RegExp): { name: string; envelope: Envelope } {
  const matches = vectorsMatching(pattern);
  const first = matches[0];
  if (first === undefined) {
    throw new Error(`no vector in vectors.json matches /${pattern.source}/`);
  }
  return { name: first.name, envelope: decodeEnvelope(hexToBytes(first.hex)) };
}

/** Minimal varint encoding, for hand-building hostile inputs the encoder itself refuses. */
function varint(value: number): number[] {
  const out: number[] = [];
  let rest = value;
  while (rest > 0x7f) {
    out.push((rest % 128) | 0x80);
    rest = Math.floor(rest / 128);
  }
  out.push(rest);
  return out;
}

function collectStrings(value: unknown, into: string[] = []): string[] {
  if (typeof value === 'string') {
    into.push(value);
  } else if (Array.isArray(value)) {
    for (const element of value) {
      collectStrings(element, into);
    }
  } else if (typeof value === 'object' && value !== null) {
    for (const element of Object.values(value)) {
      collectStrings(element, into);
    }
  }
  return into;
}

function hasNegativeCoordinate(positioner: Positioner): boolean {
  const { anchorRect, offset } = positioner;
  return (
    (anchorRect?.x ?? 0) < 0 ||
    (anchorRect?.y ?? 0) < 0 ||
    (offset?.x ?? 0) < 0 ||
    (offset?.y ?? 0) < 0
  );
}

describe('the shared vector file', () => {
  it('declares format 2 and protocol version 0', () => {
    expect(file.format).toBe(2);
    expect(file.protocol_version).toBe(0);
  });

  it('carries at least one vector per message kind (24 oneof fields)', () => {
    expect(file.vectors.length).toBeGreaterThanOrEqual(24);
  });
});

describe('every vector round-trips byte-identically', () => {
  it.each(file.vectors)('$name', ({ hex }) => {
    const bytes = hexToBytes(hex);
    const decoded = decodeEnvelope(bytes);
    const reencoded = bytesToHex(encodeEnvelope(decoded));
    expect(reencoded).toBe(hex.trim().toLowerCase());
  });
});

describe('every lenient entry decodes to its canonical message', () => {
  it.each(file.lenient)('$name', ({ hex, canonical }) => {
    expect(hex).not.toBe(canonical);
    const decoded = decodeEnvelope(hexToBytes(hex));
    expect(decoded).toEqual(decodeEnvelope(hexToBytes(canonical)));
    expect(bytesToHex(encodeEnvelope(decoded))).toBe(canonical);
  });
});

describe('every invalid entry is refused', () => {
  it('covers the caps, the value rules, the enums, the grammar and unknown bodies', () => {
    const errors = new Set(file.invalid.map((entry) => entry.error));
    expect([...errors].sort()).toEqual(['LimitViolation', 'ProtocolViolation', 'UnknownMessage']);
    for (const entry of file.invalid) {
      expect(entry.field === '', entry.name).toBe(entry.error === 'UnknownMessage');
    }
  });

  it.each(file.invalid)('$name ($error $field)', ({ hex }) => {
    expect(() => decodeEnvelope(hexToBytes(hex))).toThrow(ProtocolError);
  });
});

describe('semantic spot-checks', () => {
  it('hello-minimal is a bare Hello for version 0', () => {
    const { envelope } = decodeNamed(/hello-minimal/);
    expect(envelope.kind).toBe('hello');
    if (envelope.kind !== 'hello') {
      return;
    }
    expect(envelope.hello.protocolVersion).toBe(0);
    expect(envelope.hello.codecs).toEqual([]);
    expect(envelope.hello.resumeSerial).toBeUndefined();
  });

  it('the Greek-title vector carries UTF-8 text in a string field', () => {
    const { envelope } = decodeNamed(/greek/i);
    const strings = collectStrings(envelope);
    expect(strings.some((text) => /\p{Script_Extensions=Greek}/u.test(text))).toBe(true);
  });

  it('the popup vector has a positioner with negative coordinates', () => {
    const { envelope } = decodeNamed(/popup/i);
    expect(envelope.kind).toBe('surfaceNew');
    if (envelope.kind !== 'surfaceNew') {
      return;
    }
    expect(envelope.surfaceNew.role).toBe(1);
    expect(envelope.surfaceNew.positioner).toBeDefined();
    if (envelope.surfaceNew.positioner !== undefined) {
      expect(hasNegativeCoordinate(envelope.surfaceNew.positioner)).toBe(true);
    }
  });

  it('the RAW-tile frame decodes into pixels', () => {
    const { envelope } = decodeNamed(/raw/i);
    expect(envelope.kind).toBe('frame');
    if (envelope.kind !== 'frame') {
      return;
    }
    const tile = envelope.frame.tiles[0];
    expect(tile).toBeDefined();
    if (tile === undefined || tile.rect === undefined) {
      return;
    }
    expect(tile.codec).toBe(CODEC.RAW);
    expect(tile.data.length).toBe(tile.rect.width * tile.rect.height * 4);
    const decoded = decodeTile(tile, {
      width: tile.rect.x + tile.rect.width,
      height: tile.rect.y + tile.rect.height,
    });
    expect(decoded.imageData.width).toBe(tile.rect.width);
    expect(decoded.imageData.height).toBe(tile.rect.height);
  });

  it('the AltGr key vector carries the altgr modifier bit and a Greek keysym', () => {
    const { envelope } = decodeNamed(/altgr/i);
    expect(envelope.kind).toBe('key');
    if (envelope.kind !== 'key') {
      return;
    }
    expect(envelope.key.modifiers & 32).toBe(32);
    expect(envelope.key.keysym).toBe(0x03b1);
  });
});

describe('decoder hardening beyond the vectors', () => {
  it('rejects an empty envelope', () => {
    expect(() => decodeEnvelope(new Uint8Array(0))).toThrowError('envelope names no message');
  });

  it('rejects an unknown oneof field number', () => {
    // Field 25, wire type 2, empty payload: 0xca 0x01 (key) 0x00 (length).
    expect(() => decodeEnvelope(new Uint8Array([0xca, 0x01, 0x00]))).toThrowError(
      'unknown oneof field 25',
    );
  });

  it('rejects a second body in one envelope', () => {
    const one = encodeEnvelope({ kind: 'cursorGone', cursorGone: {} });
    const two = new Uint8Array(one.length * 2);
    two.set(one, 0);
    two.set(one, one.length);
    expect(() => decodeEnvelope(two)).toThrowError('more than one body');
  });

  it('checks MAX_MESSAGE_BYTES before parsing anything else', () => {
    const oversized = new Uint8Array(16_777_216 + 1);
    expect(() => decodeEnvelope(oversized)).toThrowError('MAX_MESSAGE_BYTES');
  });

  it('rejects a title over MAX_TITLE_BYTES', () => {
    // Envelope field 5 (SurfaceNew), field 5 inside it (title) with 513 'a's. Hand-built: the
    // encoder refuses to emit this itself, which is exactly the point.
    const title = new Array<number>(513).fill(0x61);
    const surfaceNew = [0x2a, ...varint(title.length), ...title];
    const bytes = new Uint8Array([0x2a, ...varint(surfaceNew.length), ...surfaceNew]);
    expect(() => decodeEnvelope(bytes)).toThrowError('over the 512-byte cap');
  });

  it('rejects a Tile with codec 0', () => {
    // A Frame whose tile carries rect and data but no codec: proto3 defaults the absent field
    // to 0, and 0 is never a codec. Frame: surface 1, sequence 1, full_redraw, one tile.
    const tile = [0x0a, 0x04, 0x18, 0x01, 0x20, 0x01, 0x1a, 0x04, 0, 0, 0, 0];
    const frame = [0x08, 0x01, 0x10, 0x01, 0x18, 0x01, 0x22, ...varint(tile.length), ...tile];
    const bytes = new Uint8Array([0x62, ...varint(frame.length), ...frame]);
    expect(() => decodeEnvelope(bytes)).toThrowError('Tile.codec is zero');
  });

  it('rejects an unknown enum value, for every closed v0 enum', () => {
    // v0 enums are closed (wire.proto header, decided 2026-09-20): an unknown value is a
    // protocol violation in both codecs, not something to keep and ignore. Hand-built bytes;
    // the encoder refuses to emit any of these itself.
    // Bye with reason 999 (varint 0xe7 0x07), a value ByeReason does not name.
    expect(() => decodeEnvelope(new Uint8Array([0x1a, 0x03, 0x08, 0xe7, 0x07]))).toThrowError(
      'Bye.reason has unknown enum value 999',
    );
    // SurfaceNew with role 5, which Role does not name.
    expect(() => decodeEnvelope(new Uint8Array([0x2a, 0x02, 0x10, 0x05]))).toThrowError(
      'SurfaceNew.role has unknown enum value 5',
    );
    // SurfaceNew carrying a Positioner whose anchor is 9, which Anchor does not name.
    expect(() =>
      decodeEnvelope(new Uint8Array([0x2a, 0x04, 0x3a, 0x02, 0x10, 0x09])),
    ).toThrowError('Positioner.anchor has unknown enum value 9');
    // SurfaceGone with reason 7, which SurfaceGoneReason does not name.
    expect(() => decodeEnvelope(new Uint8Array([0x32, 0x02, 0x10, 0x07]))).toThrowError(
      'SurfaceGone.reason has unknown enum value 7',
    );
  });

  it('rejects invalid UTF-8 in a string field', () => {
    // Hello (field 1) -> client_name (field 2) with a lone 0xff byte.
    const hostile = new Uint8Array([0x0a, 0x03, 0x12, 0x01, 0xff]);
    expect(() => decodeEnvelope(hostile)).toThrowError('not valid UTF-8');
  });

  it('rejects a cursor image whose argb bytes are not width*height*4', () => {
    // CursorImage: width 2, height 2, argb 8 bytes (16 would be right). Hand-built.
    const cursor = [0x10, 0x02, 0x18, 0x02, 0x32, 0x08, 0, 0, 0, 0, 0, 0, 0, 0];
    const bytes = new Uint8Array([0x72, ...varint(cursor.length), ...cursor]);
    expect(() => decodeEnvelope(bytes)).toThrowError('width*height*4');
  });

  it('rejects more codecs than MAX_CODECS_OFFERED', () => {
    // Hello with a packed codecs field of nine 1s. Hand-built.
    const hello = [0x22, 0x09, 1, 1, 1, 1, 1, 1, 1, 1, 1];
    const bytes = new Uint8Array([0x0a, ...varint(hello.length), ...hello]);
    expect(() => decodeEnvelope(bytes)).toThrowError('MAX_CODECS_OFFERED');
  });

  it('round-trips a canonical client message through the encoder', () => {
    // The encoder must be canonical too (field order, packed codecs, zigzag), or the client
    // could not speak to the Rust peer at all; this covers C -> S ground the vectors may not.
    const sent = encodeEnvelope({
      kind: 'hello',
      hello: {
        protocolVersion: 0,
        clientName: 'test-client',
        streamToken: new Uint8Array([1, 2, 3]),
        codecs: [CODEC.QOI, CODEC.RAW],
        resumeSerial: 7,
      },
    });
    const decoded = decodeEnvelope(sent);
    expect(decoded.kind).toBe('hello');
    if (decoded.kind !== 'hello') {
      return;
    }
    expect(decoded.hello.clientName).toBe('test-client');
    expect(Array.from(decoded.hello.streamToken)).toEqual([1, 2, 3]);
    expect(decoded.hello.codecs).toEqual([CODEC.QOI, CODEC.RAW]);
    expect(decoded.hello.resumeSerial).toBe(7);
    expect(encodeEnvelope(decoded)).toEqual(sent);
  });

  it('zigzags negative pointer coordinates and keeps them on the round trip', () => {
    const sent = encodeEnvelope({
      kind: 'pointerMove',
      pointerMove: { surfaceId: 3, x: -1400, y: 2999 },
    });
    const decoded = decodeEnvelope(sent);
    expect(decoded.kind).toBe('pointerMove');
    if (decoded.kind !== 'pointerMove') {
      return;
    }
    expect(decoded.pointerMove.x).toBe(-1400);
    expect(decoded.pointerMove.y).toBe(2999);
    expect(encodeEnvelope(decoded)).toEqual(sent);
  });
});
