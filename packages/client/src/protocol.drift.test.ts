// The node environment, not jsdom: this file reads wire.proto, limits.rs and vectors.json from
// the repository through `new URL(..., import.meta.url)` (see protocol.vector.test.ts).
import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import * as limits from './limits.js';
import { ProtocolError, decodeEnvelope, encodeEnvelope } from './wire.js';

/**
 * The drift detector. wire.ts is a hand-written mirror of wire.proto and limits.ts a
 * hand-written mirror of its limits table; the shared vectors catch a drift only where a vector
 * happens to look. This file reads the sources themselves and compares them:
 *
 *   - the limits block of wire.proto, limits.rs and limits.ts, row for row, and the codec ids;
 *   - every message, field number, type and label of wire.proto, through a generic decoder
 *     driven by the parsed schema, against wire.ts on every vector and lenient entry;
 *   - that the canonical vectors set every field, reach every message kind and carry every
 *     enum value at least once, so the comparison above leaves nothing untested;
 *   - the closed enum sets of wire.ts against wire.proto, value by value;
 *   - the grammar of wire.proto's ENCODING block, message by message: an unknown field of each
 *     skippable wire type is skipped, a group or an undefined wire type is refused, a singular
 *     field twice is refused, and a repeated one is not.
 *
 * Renumbering a field in wire.ts, a typo in an enum table, or a limit changed on one side only
 * turns this file red.
 */

function repoFile(path: string): string {
  return readFileSync(new URL(`../../../${path}`, import.meta.url), 'utf8');
}

// ---------------------------------------------------------------------------
// wire.proto, parsed
// ---------------------------------------------------------------------------

interface ProtoField {
  readonly name: string;
  readonly type: string;
  readonly number: number;
  readonly label: 'repeated' | 'optional' | 'plain';
}

interface ProtoSchema {
  readonly messages: ReadonlyMap<string, readonly ProtoField[]>;
  readonly enums: ReadonlyMap<string, ReadonlyMap<string, number>>;
  readonly limitRows: ReadonlyMap<string, number>;
  readonly codecs: ReadonlyMap<string, number>;
}

function parseProto(source: string): ProtoSchema {
  const messages = new Map<string, ProtoField[]>();
  const enums = new Map<string, Map<string, number>>();
  const limitRows = new Map<string, number>();
  const codecs = new Map<string, number>();
  let fields: ProtoField[] | undefined;
  let values: Map<string, number> | undefined;
  let inOneof = false;
  let inCodecs = false;
  for (const raw of source.split('\n')) {
    const row = /^\/\/\s+(MAX_[A-Z_]+|RESUME_GRACE_MS)\s+([\d,]+)\s/.exec(raw);
    if (row?.[1] !== undefined && row[2] !== undefined) {
      limitRows.set(row[1], Number(row[2].replaceAll(',', '')));
      continue;
    }
    if (raw.startsWith('// TILE CODECS')) {
      inCodecs = true;
      continue;
    }
    if (inCodecs) {
      const codec = /^\/\/\s+(\d+)\s+([A-Z]+)\s/.exec(raw);
      if (codec?.[1] !== undefined && codec[2] !== undefined) {
        codecs.set(codec[2], Number(codec[1]));
      } else if (raw.trim() === '//') {
        inCodecs = false;
      }
      continue;
    }
    const line = raw.replace(/\/\/.*$/, '').trim();
    let match: RegExpExecArray | null;
    if ((match = /^message (\w+) \{\}$/.exec(line)) !== null && match[1] !== undefined) {
      messages.set(match[1], []);
    } else if ((match = /^message (\w+) \{$/.exec(line)) !== null && match[1] !== undefined) {
      fields = [];
      messages.set(match[1], fields);
    } else if ((match = /^enum (\w+) \{$/.exec(line)) !== null && match[1] !== undefined) {
      values = new Map();
      enums.set(match[1], values);
    } else if (/^oneof \w+ \{$/.test(line)) {
      inOneof = true;
    } else if (line === '}') {
      if (inOneof) {
        inOneof = false;
      } else {
        fields = undefined;
        values = undefined;
      }
    } else if (
      (match = /^(repeated |optional )?(\w+) (\w+) = (\d+);$/.exec(line)) !== null &&
      fields !== undefined
    ) {
      const [, label, type, name, number] = match;
      if (type === undefined || name === undefined || number === undefined) {
        throw new Error(`unreadable field line in wire.proto: ${line}`);
      }
      fields.push({
        name,
        type,
        number: Number(number),
        label: label === undefined ? 'plain' : label.trim() === 'repeated' ? 'repeated' : 'optional',
      });
    } else if (
      (match = /^(\w+) = (0x[0-9a-fA-F]+|\d+);$/.exec(line)) !== null &&
      values !== undefined &&
      match[1] !== undefined &&
      match[2] !== undefined
    ) {
      values.set(match[1], Number(match[2]));
    }
  }
  return { messages, enums, limitRows, codecs };
}

const schema = parseProto(repoFile('crates/appricot-proto/proto/appricot/v0/wire.proto'));

function fieldsOf(type: string): readonly ProtoField[] {
  const fields = schema.messages.get(type);
  if (fields === undefined) {
    throw new Error(`wire.proto has no message ${type}`);
  }
  return fields;
}

function fieldNamed(type: string, name: string): ProtoField {
  const field = fieldsOf(type).find((f) => f.name === name);
  if (field === undefined) {
    throw new Error(`wire.proto has no field ${type}.${name}`);
  }
  return field;
}

const BODIES = fieldsOf('Envelope');

function isMessage(type: string): boolean {
  return schema.messages.has(type);
}

/** snake_case to the lowerCamel property names wire.ts uses: `scale_120ths` is `scale120ths`. */
function camel(name: string): string {
  return name.replace(/_([a-z0-9])/g, (_whole, next: string) => next.toUpperCase());
}

// ---------------------------------------------------------------------------
// A generic decoder, driven by the parsed schema and nothing else
// ---------------------------------------------------------------------------

interface At {
  pos: number;
}

function varintAt(bytes: Uint8Array, at: At): number {
  let value = 0;
  let scale = 1;
  for (;;) {
    const b = bytes[at.pos];
    if (b === undefined) {
      throw new Error('generic decoder: truncated varint');
    }
    at.pos += 1;
    value += (b & 0x7f) * scale;
    scale *= 128;
    if (b < 0x80) {
      return value;
    }
  }
}

function lengthDelimited(bytes: Uint8Array, at: At): Uint8Array {
  const length = varintAt(bytes, at);
  const out = bytes.subarray(at.pos, at.pos + length);
  at.pos += length;
  return out;
}

function skipGeneric(bytes: Uint8Array, at: At, wireType: number): void {
  if (wireType === 0) {
    varintAt(bytes, at);
  } else if (wireType === 1) {
    at.pos += 8;
  } else if (wireType === 2) {
    lengthDelimited(bytes, at);
  } else if (wireType === 5) {
    at.pos += 4;
  } else {
    throw new Error(`generic decoder: wire type ${wireType}`);
  }
}

function scalar(type: string, raw: number): number | boolean {
  if (type === 'bool') {
    return raw !== 0;
  }
  if (type === 'sint32') {
    return raw % 2 === 0 ? raw / 2 : -(raw + 1) / 2;
  }
  if (type === 'uint32' || schema.enums.has(type)) {
    return raw;
  }
  throw new Error(`generic decoder: no rule for type ${type}`);
}

function defaultOf(field: ProtoField): unknown {
  if (field.label === 'repeated') {
    return [];
  }
  if (field.label === 'optional' || isMessage(field.type)) {
    return undefined;
  }
  switch (field.type) {
    case 'string':
      return '';
    case 'bytes':
      return new Uint8Array(0);
    case 'bool':
      return false;
    default:
      return 0;
  }
}

/** What the vectors reached: `field Type.name` set, `enum Type=value` seen, `kind name` sent. */
const reached = new Set<string>();

function isSet(value: unknown): boolean {
  if (value === undefined || value === 0 || value === false || value === '') {
    return false;
  }
  if (Array.isArray(value) || value instanceof Uint8Array) {
    return value.length > 0;
  }
  return true;
}

function decodeGeneric(type: string, bytes: Uint8Array, track: boolean): Record<string, unknown> {
  const fields = fieldsOf(type);
  const out: Record<string, unknown> = {};
  for (const field of fields) {
    out[camel(field.name)] = defaultOf(field);
  }
  const at: At = { pos: 0 };
  while (at.pos < bytes.length) {
    const key = varintAt(bytes, at);
    const wireType = key % 8;
    const field = fields.find((f) => f.number === Math.floor(key / 8));
    if (field === undefined) {
      skipGeneric(bytes, at, wireType);
      continue;
    }
    const name = camel(field.name);
    if (field.label === 'repeated') {
      const list = out[name] as unknown[];
      if (isMessage(field.type)) {
        list.push(decodeGeneric(field.type, lengthDelimited(bytes, at), track));
      } else if (wireType === 2) {
        const packed = lengthDelimited(bytes, at);
        const inner: At = { pos: 0 };
        while (inner.pos < packed.length) {
          list.push(scalar(field.type, varintAt(packed, inner)));
        }
      } else {
        list.push(scalar(field.type, varintAt(bytes, at)));
      }
    } else if (isMessage(field.type)) {
      out[name] = decodeGeneric(field.type, lengthDelimited(bytes, at), track);
    } else if (field.type === 'string') {
      out[name] = new TextDecoder().decode(lengthDelimited(bytes, at));
    } else if (field.type === 'bytes') {
      out[name] = lengthDelimited(bytes, at).slice();
    } else {
      out[name] = scalar(field.type, varintAt(bytes, at));
    }
  }
  if (track) {
    for (const field of fields) {
      const value = out[camel(field.name)];
      if (isSet(value)) {
        reached.add(`field ${type}.${field.name}`);
      }
      if (schema.enums.has(field.type)) {
        reached.add(`enum ${field.type}=${String(value)}`);
      }
    }
  }
  return out;
}

function decodeEnvelopeGeneric(bytes: Uint8Array, track: boolean): Record<string, unknown> {
  const at: At = { pos: 0 };
  const key = varintAt(bytes, at);
  const body = BODIES.find((f) => f.number === Math.floor(key / 8));
  if (body === undefined || key % 8 !== 2) {
    throw new Error('generic decoder: not a body');
  }
  const payload = lengthDelimited(bytes, at);
  if (at.pos !== bytes.length) {
    throw new Error('generic decoder: more than one body');
  }
  if (track) {
    reached.add(`kind ${body.name}`);
  }
  const kind = camel(body.name);
  return { kind, [kind]: decodeGeneric(body.type, payload, track) };
}

// ---------------------------------------------------------------------------
// Hand-built protobuf
// ---------------------------------------------------------------------------

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

function key(number: number, wireType: number): number[] {
  return varint(number * 8 + wireType);
}

function lenField(number: number, payload: readonly number[]): number[] {
  return [...key(number, 2), ...varint(payload.length), ...payload];
}

/** For each message a body can reach, the (container, field) that holds it, found breadth first. */
const PARENT = new Map<string, { readonly container: string; readonly field: ProtoField }>();
{
  const queue = BODIES.map((body) => body.type);
  for (let type = queue.shift(); type !== undefined; type = queue.shift()) {
    for (const field of fieldsOf(type)) {
      const bodyType = BODIES.some((body) => body.type === field.type);
      if (isMessage(field.type) && !bodyType && !PARENT.has(field.type)) {
        PARENT.set(field.type, { container: type, field });
        queue.push(field.type);
      }
    }
  }
}

/** The smallest fields that make an otherwise empty message of each type valid on decode. */
function required(type: string): number[] {
  if (type === 'SurfaceNew') {
    return [...key(fieldNamed('SurfaceNew', 'scale_120ths').number, 0), 120];
  }
  return [];
}

/** Wraps the payload of a message of `type` into its containers and one envelope. */
function inEnvelope(type: string, payload: readonly number[]): Uint8Array {
  let current = type;
  let bytes = [...payload];
  for (let parent = PARENT.get(current); parent !== undefined; parent = PARENT.get(current)) {
    bytes = [...lenField(parent.field.number, bytes), ...required(parent.container)];
    current = parent.container;
  }
  const body = BODIES.find((f) => f.type === current);
  if (body === undefined) {
    throw new Error(`no envelope body holds ${type}`);
  }
  return Uint8Array.from(lenField(body.number, bytes));
}

/** A value of `field` that no rule of v0 refuses: 0, "A", one byte, or an empty message. */
function benign(field: ProtoField): number[] {
  if (field.type === 'string' || field.type === 'bytes') {
    return lenField(field.number, [0x41]);
  }
  if (isMessage(field.type)) {
    return lenField(field.number, []);
  }
  return [...key(field.number, 0), 0];
}

function errorOf(run: () => unknown): unknown {
  try {
    run();
  } catch (error) {
    return error;
  }
  return undefined;
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

interface VectorFile {
  vectors: { name: string; hex: string }[];
  lenient: { name: string; hex: string }[];
}

const vectorFile = JSON.parse(repoFile('crates/appricot-proto/testdata/vectors.json')) as VectorFile;

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

describe('the limits table has one set of numbers', () => {
  const rust = repoFile('crates/appricot-proto/src/limits.rs');

  function rustConsts(pattern: RegExp): Map<string, number> {
    const out = new Map<string, number>();
    for (const match of rust.matchAll(pattern)) {
      if (match[1] !== undefined && match[2] !== undefined) {
        out.set(match[1], Number(match[2].replaceAll('_', '')));
      }
    }
    return out;
  }

  it('wire.proto has the rows it always had, and the new wheel-step row', () => {
    expect(schema.limitRows.size).toBeGreaterThanOrEqual(25);
    expect(schema.limitRows.get('MAX_POINTER_AXIS_STEPS')).toBe(64);
  });

  it('limits.rs matches wire.proto, row for row', () => {
    const rows = rustConsts(/^pub const ([A-Z_]+): \w+ = ([\d_]+);$/gm);
    expect(Object.fromEntries(rows)).toEqual(Object.fromEntries(schema.limitRows));
  });

  it('limits.ts matches wire.proto, row for row', () => {
    const exported = Object.entries(limits).filter(([name]) => /^(MAX_|RESUME_GRACE_MS$)/.test(name));
    expect(Object.fromEntries(exported)).toEqual(Object.fromEntries(schema.limitRows));
  });

  it('the codec ids match in all three', () => {
    const rows = rustConsts(/^ {4}pub const ([A-Z]+): u32 = (\d+);$/gm);
    expect(schema.codecs.size).toBeGreaterThanOrEqual(2);
    expect(Object.fromEntries(rows)).toEqual(Object.fromEntries(schema.codecs));
    expect({ ...limits.CODEC }).toEqual(Object.fromEntries(schema.codecs));
  });
});

describe('wire.ts decodes exactly what wire.proto says', () => {
  it('parsed 25 bodies and the five nested messages', () => {
    expect(BODIES).toHaveLength(25);
    expect(BODIES.map((body) => body.number)).toEqual(Array.from({ length: 25 }, (_, i) => i + 1));
    expect([...PARENT.keys()].sort()).toEqual(['Point', 'Positioner', 'Rect', 'Size', 'Tile']);
  });

  it.each(vectorFile.vectors)('vector $name', ({ hex }) => {
    const bytes = hexToBytes(hex);
    expect(decodeEnvelope(bytes)).toEqual(decodeEnvelopeGeneric(bytes, true));
  });

  it.each(vectorFile.lenient)('lenient $name', ({ hex }) => {
    const bytes = hexToBytes(hex);
    expect(decodeEnvelope(bytes)).toEqual(decodeEnvelopeGeneric(bytes, false));
  });

  // Runs after the vector cases above, which fill `reached`.
  it('the vectors leave no field, kind or enum value untested', () => {
    const missing: string[] = [];
    for (const [type, fields] of schema.messages) {
      if (type === 'Envelope') {
        continue;
      }
      for (const field of fields) {
        if (!reached.has(`field ${type}.${field.name}`)) {
          missing.push(`field ${type}.${field.name}`);
        }
      }
    }
    for (const body of BODIES) {
      if (!reached.has(`kind ${body.name}`)) {
        missing.push(`kind ${body.name}`);
      }
    }
    for (const [type, values] of schema.enums) {
      for (const [name, value] of values) {
        if (!reached.has(`enum ${type}=${value}`)) {
          missing.push(`enum ${type}.${name}`);
        }
      }
    }
    expect(missing).toEqual([]);
  });
});

describe('the closed enum sets of wire.ts are the sets of wire.proto', () => {
  const probes: number[] = [...Array.from({ length: 0x110 }, (_, i) => i), 999, 0xffff, 0x7fffffff];
  const enumFields: { type: string; field: ProtoField }[] = [];
  for (const [type, fields] of schema.messages) {
    for (const field of fields) {
      if (schema.enums.has(field.type)) {
        enumFields.push({ type, field });
      }
    }
  }

  it('found every enum field', () => {
    expect(enumFields.map(({ type, field }) => `${type}.${field.name}`).sort()).toEqual([
      'Bye.reason',
      'Positioner.anchor',
      'Positioner.gravity',
      'SurfaceGone.reason',
      'SurfaceNew.role',
    ]);
  });

  const cases = enumFields.map(({ type, field }): [string, string, ProtoField] => [
    `${type}.${field.name}`,
    type,
    field,
  ]);

  it.each(cases)(
    '%s',
    (_label, type, field) => {
      const allowed = new Set(schema.enums.get(field.type)?.values());
      for (const value of probes) {
        const bytes = inEnvelope(type, [...key(field.number, 0), ...varint(value), ...required(type)]);
        const error = errorOf(() => decodeEnvelope(bytes));
        if (allowed.has(value)) {
          expect(error, `${type}.${field.name} = ${value}`).toBeUndefined();
        } else {
          expect(error, `${type}.${field.name} = ${value}`).toBeInstanceOf(ProtocolError);
        }
      }
    },
  );
});

describe('the grammar of wire.proto ENCODING, message by message', () => {
  const nested = [...PARENT.keys()];
  const reachable = [...BODIES.map((body) => body.type), ...nested];

  it.each(reachable)('%s: a singular field twice is refused, a repeated one is not', (type) => {
    for (const field of fieldsOf(type)) {
      const twice = inEnvelope(type, [...benign(field), ...benign(field)]);
      const error = errorOf(() => decodeEnvelope(twice));
      const duplicate = error instanceof ProtocolError && /appears more than once/.test(error.message);
      expect(duplicate, `${type}.${field.name} twice`).toBe(field.label !== 'repeated');
    }
  });

  // The first canonical vector of each kind, with unknown fields appended to its body.
  const firstOfKind = new Map<number, Uint8Array>();
  for (const vector of vectorFile.vectors) {
    const bytes = hexToBytes(vector.hex);
    const number = Math.floor(varintAt(bytes, { pos: 0 }) / 8);
    if (bytes.length < 4096 && !firstOfKind.has(number)) {
      firstOfKind.set(number, bytes);
    }
  }

  function withExtra(bytes: Uint8Array, extra: readonly number[]): Uint8Array {
    const at: At = { pos: 0 };
    const number = Math.floor(varintAt(bytes, at) / 8);
    const payload = lengthDelimited(bytes, at);
    return Uint8Array.from(lenField(number, [...payload, ...extra]));
  }

  it.each(BODIES.map((body): [string, ProtoField] => [body.name, body]))(
    '%s: an unknown field is skipped by its wire type; a group or wire type 6 or 7 is refused',
    (_name, body) => {
      const bytes = firstOfKind.get(body.number);
      expect(bytes, `a canonical vector for ${body.name}`).toBeDefined();
      if (bytes === undefined) {
        return;
      }
      const decoded = decodeEnvelope(bytes);
      const unknown = Math.max(0, ...fieldsOf(body.type).map((f) => f.number)) + 1;
      for (const number of [unknown, 1000, 536_870_911]) {
        const skippable = [
          [...key(number, 0), ...varint(2 ** 40)],
          [...key(number, 1), 1, 2, 3, 4, 5, 6, 7, 8],
          lenField(number, [0x3c, 0x62, 0x3e]),
          [...key(number, 5), 1, 2, 3, 4],
        ];
        for (const extra of skippable) {
          expect(decodeEnvelope(withExtra(bytes, extra)), `${body.name} + ${extra.join(' ')}`).toEqual(decoded);
        }
        for (const wireType of [3, 4, 6, 7]) {
          expect(() => decodeEnvelope(withExtra(bytes, key(number, wireType)))).toThrow(ProtocolError);
        }
      }
      // Field 0 is never a field.
      expect(() => decodeEnvelope(withExtra(bytes, [0x00, 0x00]))).toThrow(ProtocolError);
    },
  );
});

describe('the value rules, on encode as on decode', () => {
  it('refuses a Configure with serial 0, and keeps serial 0 for ConfigureAck', () => {
    const configure = { surfaceId: 2, serial: 0, size: { width: 800, height: 600 } };
    const refused = errorOf(() => encodeEnvelope({ kind: 'configure', configure }));
    expect(refused).toBeInstanceOf(ProtocolError);
    expect((refused as ProtocolError).field).toBe('Configure.serial');
    const ack = encodeEnvelope({ kind: 'configureAck', configureAck: configure });
    expect(decodeEnvelope(ack)).toEqual({ kind: 'configureAck', configureAck: configure });
  });

  it('refuses a Key with an empty code; "Unidentified" is the code for a key with no name', () => {
    const key = { keysym: 0xffff, code: '', pressed: true, modifiers: 0 };
    const refused = errorOf(() => encodeEnvelope({ kind: 'key', key }));
    expect(refused).toBeInstanceOf(ProtocolError);
    expect((refused as ProtocolError).field).toBe('Key.code');
    const named = { ...key, code: 'Unidentified' };
    expect(decodeEnvelope(encodeEnvelope({ kind: 'key', key: named }))).toEqual({ kind: 'key', key: named });
  });

  it('refuses a Key with no code field at all, as it refuses an empty one', () => {
    // Key: keysym 0x61, pressed; no field 2.
    const bytes = inEnvelope('Key', [0x08, 0x61, 0x18, 0x01]);
    expect(() => decodeEnvelope(bytes)).toThrow(ProtocolError);
  });

  it('holds PointerAxis to MAX_POINTER_AXIS_STEPS either way', () => {
    const cap = limits.MAX_POINTER_AXIS_STEPS;
    for (const [stepsX, stepsY] of [
      [cap, -cap],
      [-cap, cap],
    ] as const) {
      const pointerAxis = { surfaceId: 1, stepsX, stepsY };
      expect(decodeEnvelope(encodeEnvelope({ kind: 'pointerAxis', pointerAxis }))).toEqual({
        kind: 'pointerAxis',
        pointerAxis,
      });
    }
    for (const [stepsX, stepsY] of [
      [cap + 1, 0],
      [0, -(cap + 1)],
      [0, -0x80000000],
    ] as const) {
      const refused = errorOf(() =>
        encodeEnvelope({ kind: 'pointerAxis', pointerAxis: { surfaceId: 1, stepsX, stepsY } }),
      );
      expect(refused, `${stepsX}, ${stepsY}`).toBeInstanceOf(ProtocolError);
    }
  });
});
