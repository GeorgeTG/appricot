// The node environment, not jsdom: this file reads qoi-vectors.json from the repository through
// `new URL(..., import.meta.url)`, and under the jsdom environment that URL resolves against
// jsdom's page instead of the file system.
import { existsSync, readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import type { Tile } from './wire.js';
import { ProtocolError } from './wire.js';
import { CODEC } from './limits.js';
import { decodeTile } from './tile.js';

/**
 * The node environment has no ImageData global, so the test provides the constructor a browser
 * would, exactly as in tile.test.ts: the (width, height) constructor and a writable .data of
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
 * The shared tile vectors: the Rust encoder (crates/appricot-encode, module tile_vectors) writes
 * testdata/qoi-vectors.json, and this file proves the TypeScript decoder draws every vector's
 * payload to the same pixels, and refuses the same payloads. The format is pinned:
 * { format: 1, vectors: [{ name, note, codec, width, height, payload, expect, rgba_fnv1a32?,
 * rgba? }] }, where payload and rgba are lowercase hex, expect is "pixels" or "reject", and
 * rgba_fnv1a32 is the 32-bit FNV-1a hash of the expected RGBA bytes in eight hex digits.
 */

interface TileVector {
  name: string;
  note: string;
  codec: number;
  width: number;
  height: number;
  payload: string;
  expect: 'pixels' | 'reject';
  rgba_fnv1a32?: string;
  rgba?: string;
}

interface TileVectorFile {
  format: number;
  vectors: TileVector[];
}

const vectorsUrl = new URL(
  '../../../crates/appricot-encode/testdata/qoi-vectors.json',
  import.meta.url,
);

function loadVectors(): TileVectorFile {
  if (!existsSync(vectorsUrl)) {
    throw new Error('qoi-vectors.json is missing; the Rust tile_vectors test writes it');
  }
  const parsed = JSON.parse(readFileSync(vectorsUrl, 'utf8')) as TileVectorFile;
  if (parsed.format !== 1 || !Array.isArray(parsed.vectors) || parsed.vectors.length === 0) {
    throw new Error('qoi-vectors.json has an unexpected shape');
  }
  return parsed;
}

const file = loadVectors();

function hexToBytes(hex: string): Uint8Array {
  if (hex.length % 2 !== 0 || !/^[0-9a-f]*$/.test(hex)) {
    throw new Error(`vector hex is malformed: ${hex.slice(0, 40)}`);
  }
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/** The 32-bit FNV-1a hash (offset basis 0x811c9dc5, prime 0x01000193), as eight hex digits. */
function fnv1a32(bytes: Uint8ClampedArray): string {
  let hash = 0x811c9dc5;
  for (const byte of bytes) {
    hash = Math.imul(hash ^ byte, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

function tileOf(vector: TileVector): Tile {
  return {
    rect: { x: 0, y: 0, width: vector.width, height: vector.height },
    codec: vector.codec,
    data: hexToBytes(vector.payload),
  };
}

function decodeVector(vector: TileVector): Uint8ClampedArray {
  return decodeTile(tileOf(vector), { width: vector.width, height: vector.height }).imageData.data;
}

function named(name: string): TileVector {
  const vector = file.vectors.find((candidate) => candidate.name === name);
  if (vector === undefined) {
    throw new Error(`no vector named ${name} in qoi-vectors.json`);
  }
  return vector;
}

describe('the Rust-generated tile vectors (appricot-encode testdata/qoi-vectors.json)', () => {
  it('the hash helper matches the Rust generator on a vector that carries its bytes', () => {
    const vector = named('qoi-1x1');
    expect(vector.rgba).toBe('112233ff');
    expect(fnv1a32(Uint8ClampedArray.from(hexToBytes('112233ff')))).toBe(vector.rgba_fnv1a32);
  });

  it('covers both codecs, the edge tile sizes and refusals', () => {
    const codecs = new Set(file.vectors.map((vector) => vector.codec));
    expect(codecs.has(CODEC.RAW)).toBe(true);
    expect(codecs.has(CODEC.QOI)).toBe(true);
    const sizes = new Set(file.vectors.map((vector) => `${vector.width}x${vector.height}`));
    for (const size of ['1x1', '1x256', '256x1', '120x132', '256x256']) {
      expect(sizes.has(size)).toBe(true);
    }
    expect(file.vectors.some((vector) => vector.expect === 'reject')).toBe(true);
  });

  for (const vector of file.vectors) {
    if (vector.expect === 'pixels') {
      it(`decodes ${vector.name}: ${vector.note}`, () => {
        const pixels = decodeVector(vector);
        expect(pixels.length).toBe(vector.width * vector.height * 4);
        if (vector.rgba !== undefined) {
          expect(Array.from(pixels)).toEqual(Array.from(hexToBytes(vector.rgba)));
        }
        expect(fnv1a32(pixels)).toBe(vector.rgba_fnv1a32);
        for (let i = 3; i < pixels.length; i += 4) {
          if (pixels[i] !== 255) {
            throw new Error(`${vector.name}: pixel ${(i - 3) / 4} is not opaque`);
          }
        }
      });
    } else {
      it(`refuses ${vector.name}: ${vector.note}`, () => {
        expect(() => decodeVector(vector)).toThrow(ProtocolError);
      });
    }
  }

  it('draws one capture buffer the same as RAW and as QOI, whatever its fourth byte', () => {
    const raw = named('raw-garbage-fourth-byte');
    const qoi = named('qoi-garbage-fourth-byte');
    expect(raw.codec).toBe(CODEC.RAW);
    expect(qoi.codec).toBe(CODEC.QOI);
    // The RAW payload really does carry unused bytes other than 0xFF.
    const rawPayload = hexToBytes(raw.payload);
    const fourth = new Set<number>();
    for (let i = 3; i < rawPayload.length; i += 4) {
      fourth.add(rawPayload[i] ?? 0);
    }
    expect(fourth.has(0)).toBe(true);
    expect(fourth.has(0x7f)).toBe(true);
    expect(Array.from(decodeVector(raw))).toEqual(Array.from(decodeVector(qoi)));
  });
});
