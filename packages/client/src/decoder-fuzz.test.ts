// The node environment, not jsdom: this file reads vectors.json from the repository through
// `new URL(..., import.meta.url)`, which under jsdom resolves against jsdom's page instead of
// the file system (see protocol.vector.test.ts for the measured gotcha).
import { existsSync, readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import { markupEnvelopeBytes, oversizedBytes, rawTile, screenSizedPopupBytes } from './hostile/cases';
import { CODEC, MAX_MESSAGE_BYTES } from './limits';
import { decodeTile } from './tile';
import type { Envelope, Tile } from './wire';
import { ProtocolError, decodeEnvelope, encodeEnvelope } from './wire';

/**
 * The bounded decoder fuzzer (docs/protocol/README.md rule 10, "Both decoders are fuzzed";
 * docs/protocol/v0.md section 12). The Rust twins are the 10,000-mutation loop in
 * crates/appricot-proto/src/wire.rs (`mutated_encodings_never_panic`) and the structure-aware,
 * seeded fuzzer in crates/appricot-proto/tests/fuzz.rs; this is their TypeScript mirror against
 * `decodeEnvelope` and `decodeTile`, hand-rolled and deterministic:
 *
 *   - PRNG: xorshift32 with the fixed seed `FUZZ_SEED` below, no Math.random anywhere. Every
 *     draw is specified 32-bit integer arithmetic, so the run is identical on every machine and
 *     every repetition; the seed and both iteration counts are constants, not options.
 *   - Corpus: every committed vector, lenient entry and invalid entry of vectors.json, a
 *     handful of hand-built valid envelopes (popup with negative coordinates, Greek title, AltGr
 *     key, full tiles in both codecs, a tile at every cap), and the hostile factories of
 *     src/hostile/cases.ts used as data.
 *   - Mutations: bit flips, byte splices between corpus members, truncation (a systematic sweep
 *     at every boundary plus random cuts), value-preserving varint extension, 2-byte
 *     length-field inflation, and duplicated chunks appended.
 *   - Invariants: `decodeEnvelope` returns an Envelope or throws ProtocolError - never another
 *     error, never a non-Error throw, never a hang (every input is bounded to
 *     MAX_MESSAGE_BYTES + 64 before it is handed over). A successful decode re-encodes without
 *     throwing (the Rust twin's rule: decoded means valid) and reaches a byte-stable fixed
 *     point under decode(encode(.)), which is the stronger property and holds because the
 *     corpus and both encoders are canonical. `decodeTile` likewise returns ProtocolError or
 *     pixels whose width and height match the rect and whose byte length is exactly
 *     width*height*4.
 *   - Coverage: the run counts the outcomes (accepted, rejected-by-cap, rejected-by-grammar)
 *     and every mutation family's fire count, and fails loudly if any bucket or family is
 *     empty - a family that never fires means the fuzzer is not exercising what it claims.
 */

/**
 * The node environment has no ImageData global either, so the test provides the constructor a
 * browser would, exactly as in protocol.vector.test.ts: the (width, height) constructor and a
 * .data of width*height*4 bytes. decodeTile allocates nothing through it before its checks.
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

// ---------------------------------------------------------------------------
// Tuning: documented, deterministic, cheap
// ---------------------------------------------------------------------------

/** The fixed xorshift32 seed. Change it and the entire run changes; that is the point. */
const FUZZ_SEED = 0x1234_5678;

/** Random envelope mutations, plus a 4,000-iteration tile loop and the truncation sweep. */
const ENVELOPE_ITERATIONS = 12_000;
const TILE_ITERATIONS = 4_000;

/**
 * Corpus members over this size are decoded once in the corpus pass but neither swept nor
 * mutated: the three hostile members above it (a 64 KiB clipboard text, a 256 KiB tile, a
 * 16 MiB message) teach their caps deterministically, and cloning any of them per iteration
 * would cost more than every other member together.
 */
const POOL_LIMIT = 4096;

/**
 * The fuzzer's promise to the decoder: no input handed to decodeEnvelope is longer than
 * MAX_MESSAGE_BYTES + 64, so nothing below can turn into an unbounded scan. With POOL_LIMIT
 * and the bounded mutation ops the bound cannot actually trigger; it is enforced anyway.
 */
const HARD_BOUND = MAX_MESSAGE_BYTES + 64;

/** Splice and duplication chunks are short on purpose: shape changes, not bulk. */
const CHUNK_LIMIT = 64;

/** Tile seeds with more payload than this are excluded from the tile loop for the same reason. */
const TILE_SEED_DATA_LIMIT = 4096;

/** Generous surface bounds for decodeTile, so rect rejection is about the rect, not the bound. */
const TILE_SURFACE_BOUNDS = { width: 8192, height: 8192 } as const;

// ---------------------------------------------------------------------------
// Determinism: the PRNG
// ---------------------------------------------------------------------------

/**
 * xorshift32 (Marsaglia): the whole point is that the sequence is fixed. Every draw is plain
 * 32-bit integer arithmetic (`^`, `<<`, `>>>`), whose semantics JavaScript specifies exactly,
 * so the run is identical on every machine and at every repetition.
 */
class Rng {
  private state: number;

  constructor(seed: number) {
    this.state = seed >>> 0;
  }

  next(): number {
    let x = this.state;
    x = (x ^ (x << 13)) >>> 0;
    x = (x ^ (x >>> 17)) >>> 0;
    x = (x ^ (x << 5)) >>> 0;
    this.state = x;
    return x;
  }

  byte(): number {
    return this.next() & 0xff;
  }

  below(n: number): number {
    return this.next() % n;
  }
}

// ---------------------------------------------------------------------------
// Byte helpers
// ---------------------------------------------------------------------------

function byteAt(bytes: Uint8Array, index: number): number {
  const value = bytes[index];
  if (value === undefined) {
    throw new Error(`byte ${index} is out of range in a ${bytes.length}-byte buffer`);
  }
  return value;
}

function hexToBytes(hex: string): Uint8Array {
  const clean = hex.trim();
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

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) {
    return false;
  }
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) {
      return false;
    }
  }
  return true;
}

/** Enough hex to reproduce a failure by hand, without dumping a 4 KiB member into the log. */
function hexSnippet(bytes: Uint8Array): string {
  const prefix = bytesToHex(bytes.subarray(0, 48));
  return bytes.length <= 48 ? prefix : `${prefix}.. (${bytes.length} bytes)`;
}

// ---------------------------------------------------------------------------
// The corpus
// ---------------------------------------------------------------------------

interface CorpusMember {
  readonly name: string;
  readonly bytes: Uint8Array;
}

interface VectorEntry {
  name: string;
  note?: string;
  hex: string;
}

interface VectorFile {
  format: number;
  protocol_version: number;
  vectors: VectorEntry[];
  /** Bytes no encoder writes that both decoders accept (format 2). */
  lenient?: VectorEntry[];
  /** Bytes both decoders refuse (format 2). */
  invalid?: VectorEntry[];
}

function loadVectors(): VectorFile {
  const url = new URL('../../../crates/appricot-proto/testdata/vectors.json', import.meta.url);
  if (!existsSync(url)) {
    throw new Error('vectors.json missing - the Rust codec generates it');
  }
  const parsed = JSON.parse(readFileSync(url, 'utf8')) as VectorFile;
  if (
    typeof parsed.format !== 'number' ||
    typeof parsed.protocol_version !== 'number' ||
    !Array.isArray(parsed.vectors) ||
    parsed.vectors.length === 0
  ) {
    throw new Error('vectors.json has an unexpected shape');
  }
  return parsed;
}

/**
 * A minimal valid 2x2 3-channel QOI stream, hand-built from the published specification
 * (https://qoiformat.org, checked 2026-09-20 - the same source tile.ts implements): header,
 * two QOI_OP_RGB pixels, a two-pixel QOI_OP_RUN, the 8-byte end marker. 31 bytes total.
 */
function qoiTileData(): Uint8Array {
  return Uint8Array.from([
    0x71, 0x6f, 0x69, 0x66, // "qoif" magic
    0x00, 0x00, 0x00, 0x02, // width 2, big-endian
    0x00, 0x00, 0x00, 0x02, // height 2
    0x03, 0x00, // 3 channels, sRGB with linear alpha
    0xfe, 0xe1, 0x00, 0x00, // QOI_OP_RGB: (0xe1, 0, 0); alpha carries over as 255
    0xfe, 0x00, 0xe1, 0x00, // QOI_OP_RGB: (0, 0xe1, 0)
    0xc1, // QOI_OP_RUN: the previous pixel, twice
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // the end marker
  ]);
}

function fill(count: number, value: number): Uint8Array {
  const out = new Uint8Array(count);
  out.fill(value);
  return out;
}

/** Valid envelopes the committed vectors do not carry, built through the encoder itself. */
function handBuiltCorpus(): CorpusMember[] {
  const member = (name: string, envelope: Envelope): CorpusMember => ({
    name,
    bytes: encodeEnvelope(envelope),
  });
  return [
    member('hand: popup with negative coordinates', {
      kind: 'surfaceNew',
      surfaceNew: {
        surfaceId: 4,
        role: 1,
        parentId: 2,
        size: { width: 96, height: 28 },
        title: 'menu',
        appId: 'menu.app',
        positioner: {
          anchorRect: { x: -8, y: -12, width: 40, height: 8 },
          anchor: 6,
          gravity: 7,
          offset: { x: -3, y: -29 },
          size: { width: 96, height: 28 },
        },
        scale120ths: 120,
      },
    }),
    member('hand: greek title', {
      kind: 'surfaceNew',
      surfaceNew: {
        surfaceId: 5,
        role: 0,
        size: { width: 400, height: 300 },
        title: 'Ρύθμιση παραθύρου',
        appId: 'app.el',
        scale120ths: 120,
      },
    }),
    member('hand: altgr greek alpha key', {
      kind: 'key',
      key: { keysym: 0x03b1, code: 'KeyA', pressed: true, modifiers: 32 },
    }),
    member('hand: frame with a full small RAW tile', {
      kind: 'frame',
      frame: {
        surfaceId: 3,
        sequence: 9,
        fullRedraw: true,
        tiles: [{ rect: { x: 4, y: 6, width: 8, height: 8 }, codec: CODEC.RAW, data: fill(8 * 8 * 4, 0x5a) }],
      },
    }),
    member('hand: frame with a full QOI tile', {
      kind: 'frame',
      frame: {
        surfaceId: 3,
        sequence: 10,
        fullRedraw: false,
        tiles: [{ rect: { x: 1, y: 1, width: 2, height: 2 }, codec: CODEC.QOI, data: qoiTileData() }],
      },
    }),
    member('hand: frame with a tile at every cap (256x256 RAW = MAX_TILE_BYTES)', {
      kind: 'frame',
      frame: {
        surfaceId: 6,
        sequence: 11,
        fullRedraw: true,
        tiles: [
          { rect: { x: 0, y: 0, width: 256, height: 256 }, codec: CODEC.RAW, data: fill(256 * 256 * 4, 0x80) },
        ],
      },
    }),
  ];
}

/** The hostile factories of src/hostile/cases.ts, used as plain data. */
function hostileCorpus(): CorpusMember[] {
  const members: CorpusMember[] = markupEnvelopeBytes().map((entry) => ({
    name: `hostile markup: ${entry.name}`,
    bytes: entry.bytes,
  }));
  for (const row of oversizedBytes()) {
    members.push({ name: `hostile oversized: ${row.name}`, bytes: row.bytes });
  }
  members.push({ name: 'hostile: screen-sized popup', bytes: screenSizedPopupBytes(7, 3) });
  return members;
}

function buildCorpus(vectorFile: VectorFile): CorpusMember[] {
  const corpus: CorpusMember[] = vectorFile.vectors.map((vector) => ({
    name: `vector: ${vector.name}`,
    bytes: hexToBytes(vector.hex),
  }));
  for (const entry of vectorFile.lenient ?? []) {
    corpus.push({ name: `lenient: ${entry.name}`, bytes: hexToBytes(entry.hex) });
  }
  for (const entry of vectorFile.invalid ?? []) {
    corpus.push({ name: `invalid: ${entry.name}`, bytes: hexToBytes(entry.hex) });
  }
  corpus.push(...handBuiltCorpus());
  corpus.push(...hostileCorpus());
  return corpus;
}

// ---------------------------------------------------------------------------
// The invariant
// ---------------------------------------------------------------------------

type Outcome = 'accepted' | 'cap' | 'grammar';

interface Buckets {
  accepted: number;
  cap: number;
  grammar: number;
}

/**
 * A cap rejection names a limit from the limits table (or an "N-byte cap" in the message text);
 * every other refusal - a singular field twice, wrong wire types, truncation, spans running past
 * the message, invalid UTF-8, closed enums, zero scale, more than one body - is a grammar
 * rejection. An unknown field is not a refusal: it is skipped.
 */
const CAP_RE = /over MAX_|over the \d+-byte cap|more than MAX_/;

function classifyRejection(message: string): 'cap' | 'grammar' {
  return CAP_RE.test(message) ? 'cap' : 'grammar';
}

function describeError(error: unknown): string {
  if (error instanceof Error) {
    return `${error.name}: ${error.message}`;
  }
  return `a non-Error throw: ${String(error)}`;
}

/** Every invariant violation found anywhere in the run; the test fails if this is not empty. */
const failures: string[] = [];

/** Cheap coverage signal for the report: how many distinct fields the rejections named. */
const distinctRejectionFields = new Set<string>();

/**
 * Runs the envelope invariant on one input and returns its outcome. Any deviation from
 * "ProtocolError, or a decode that re-encodes to a byte-stable fixed point" is recorded in
 * `failures` with the input bytes, so the first divergence fails the run reproducibly.
 */
function checkEnvelope(bytes: Uint8Array, label: string): Outcome {
  let envelope: Envelope;
  try {
    envelope = decodeEnvelope(bytes);
  } catch (error) {
    if (error instanceof ProtocolError) {
      distinctRejectionFields.add(error.field ?? '(no field)');
      return classifyRejection(error.message);
    }
    failures.push(`${label}: decodeEnvelope threw ${describeError(error)} (hex ${hexSnippet(bytes)})`);
    return 'grammar';
  }
  try {
    // The Rust twin's rule: decoded means valid, so encoding it back must work. And because the
    // corpus and both encoders are canonical (fields in field-number order, minimal varints), a
    // second round must be byte-identical - the stronger round-trip property the vectors pin.
    const once = encodeEnvelope(envelope);
    const twice = encodeEnvelope(decodeEnvelope(once));
    if (!bytesEqual(twice, once)) {
      failures.push(`${label}: the round trip is not byte-stable (hex ${hexSnippet(bytes)})`);
    }
  } catch (error) {
    failures.push(
      `${label}: a decoded envelope failed to re-encode: ${describeError(error)} (hex ${hexSnippet(bytes)})`,
    );
  }
  return 'accepted';
}

// ---------------------------------------------------------------------------
// The tile loop's seeds and knobs
// ---------------------------------------------------------------------------

interface TileSeed {
  readonly name: string;
  readonly tile: Tile;
}

/** Rect and codec draws: negatives, small legal values, cap-crossers, int32 extremes. */
const RECT_VALUE_POOL: readonly number[] = [
  -2147483648, -9, -1, 0, 1, 2, 3, 15, 255, 256, 257, 511, 1023, 4095, 4096, 65535, 2147483647,
];
const CODEC_VALUE_POOL: readonly number[] = [0, 1, 2, 3, 7, 255, 0xffffffff];

function tileSeeds(corpus: readonly CorpusMember[]): TileSeed[] {
  const seeds: TileSeed[] = [];
  for (const member of corpus) {
    let envelope: Envelope;
    try {
      envelope = decodeEnvelope(member.bytes);
    } catch {
      continue; // hostile corpus members are supposed to be refused
    }
    if (envelope.kind !== 'frame') {
      continue;
    }
    for (const tile of envelope.frame.tiles) {
      if (tile.data.length <= TILE_SEED_DATA_LIMIT) {
        seeds.push({ name: `${member.name} tile`, tile });
      }
    }
  }
  // The hostile factories' valid RAW tile, and the no-rect shape no corpus frame carries.
  seeds.push({ name: 'hostile rawTile', tile: rawTile(10, 12, 3, 2) });
  seeds.push({ name: 'rectless tile', tile: { codec: CODEC.RAW, data: Uint8Array.of(1, 2, 3, 4) } });
  return seeds;
}

// ---------------------------------------------------------------------------
// The one test
// ---------------------------------------------------------------------------

describe('the bounded decoder fuzzer', () => {
  // One test on purpose: the whole run is one deterministic stream, and its wall time belongs
  // to the run, not to vitest's per-file overhead. The timeout is headroom, not a budget - the
  // run itself stays well inside a second (see the summary it prints).
  it(
    'mutated input is always a ProtocolError or a byte-stable decode',
    { timeout: 30_000 },
    () => {
      const startedAt = Date.now(); // reporting only; the fuzz stream itself never reads a clock

      const vectorFile = loadVectors();
      const corpus = buildCorpus(vectorFile);

      // --- The corpus itself: every member honours the invariant, and the committed vectors
      // --- are all accepted (they are canonical by construction, proved by the vector test).
      const corpusBuckets: Buckets = { accepted: 0, cap: 0, grammar: 0 };
      let vectorsAccepted = 0;
      for (const member of corpus) {
        const outcome = checkEnvelope(member.bytes, `corpus ${member.name}`);
        corpusBuckets[outcome] += 1;
        if (member.name.startsWith('vector: ') && outcome === 'accepted') {
          vectorsAccepted += 1;
        }
      }
      expect(vectorsAccepted, 'every committed vector must decode').toBe(vectorFile.vectors.length);
      expect(corpusBuckets.cap, 'the hostile corpus must produce cap rejections').toBeGreaterThan(0);

      // --- Truncation at EVERY boundary: each member small enough to afford it, decoded at
      // --- every prefix length. This family is exhaustive rather than random because
      // --- truncation bugs hide at exact field boundaries; it is a few thousand tiny decodes.
      const sweepBuckets: Buckets = { accepted: 0, cap: 0, grammar: 0 };
      let sweptCuts = 0;
      for (const member of corpus) {
        const length = member.bytes.length;
        if (length === 0 || length > POOL_LIMIT) {
          continue;
        }
        for (let cut = 0; cut < length; cut += 1) {
          const outcome = checkEnvelope(member.bytes.subarray(0, cut), `sweep ${member.name} cut ${cut}`);
          sweepBuckets[outcome] += 1;
          sweptCuts += 1;
        }
      }

      // --- The random envelope loop.
      const pool = corpus.filter((member) => member.bytes.length > 0 && member.bytes.length <= POOL_LIMIT);
      expect(pool.length, 'the mutation pool is unexpectedly small').toBeGreaterThan(20);

      const rng = new Rng(FUZZ_SEED);
      const families = {
        bitFlip: 0,
        splice: 0,
        truncate: 0,
        varintExtend: 0,
        inflateLength: 0,
        duplicateChunk: 0,
      };
      const randomBuckets: Buckets = { accepted: 0, cap: 0, grammar: 0 };

      for (let iteration = 0; iteration < ENVELOPE_ITERATIONS; iteration += 1) {
        const base = pool[rng.below(pool.length)];
        if (base === undefined) {
          throw new Error('the mutation pool drew an absent member');
        }
        let bytes = base.bytes.slice();
        const ops = 1 + rng.below(3);
        for (let op = 0; op < ops; op += 1) {
          if (bytes.length === 0) {
            break; // a truncation to zero leaves nothing to mutate; the empty decode still runs
          }
          switch (rng.below(6)) {
            case 0: {
              // Bit flip: the cheapest structural corruption there is.
              const at = rng.below(bytes.length);
              bytes[at] = byteAt(bytes, at) ^ (1 << rng.below(8));
              families.bitFlip += 1;
              break;
            }
            case 1: {
              // Byte splice: a range from another corpus member overwrites (or extends) this
              // one - foreign lengths, foreign payloads, mixed messages.
              const other = pool[rng.below(pool.length)];
              if (other === undefined || other.bytes.length === 0) {
                break;
              }
              const srcStart = rng.below(other.bytes.length);
              const srcLen = 1 + rng.below(Math.min(CHUNK_LIMIT, other.bytes.length - srcStart));
              const dstStart = rng.below(bytes.length);
              const spliced = new Uint8Array(Math.max(bytes.length, dstStart + srcLen));
              spliced.set(bytes);
              spliced.set(other.bytes.subarray(srcStart, srcStart + srcLen), dstStart);
              bytes = spliced;
              families.splice += 1;
              break;
            }
            case 2: {
              // Random truncation; the exhaustive sweep above is the same family, systematic.
              bytes = bytes.subarray(0, rng.below(bytes.length));
              families.truncate += 1;
              break;
            }
            case 3: {
              // Varint extension: a byte under 0x80 terminates a varint wherever it sits in a
              // tag, length or value position. Rewriting it to `byte | 0x80` with an inserted
              // 0x00 after it keeps that varint's VALUE identical while making it non-minimal -
              // the one encode/decode asymmetry protobuf allows, so a successful decode must
              // still reach a byte-stable fixed point after one canonical re-encode.
              const heads: number[] = [];
              for (let p = 0; p < bytes.length; p += 1) {
                if (byteAt(bytes, p) < 0x80) {
                  heads.push(p);
                }
              }
              if (heads.length === 0) {
                break;
              }
              const head = heads[rng.below(heads.length)];
              if (head === undefined) {
                break;
              }
              const extended = new Uint8Array(bytes.length + 1);
              extended.set(bytes.subarray(0, head + 1));
              extended[head] = byteAt(extended, head) | 0x80;
              extended[head + 1] = 0x00;
              extended.set(bytes.subarray(head + 1), head + 2);
              bytes = extended;
              families.varintExtend += 1;
              break;
            }
            case 4: {
              // Length-field inflation: a 2-byte varint (>= 0x80 then < 0x80, the shape every
              // length in [128, 16383] takes) rewritten to 0xff 0xff - a length of 16383 that
              // no bounded input can hold, so the decoder must refuse by span arithmetic, never
              // by trying to allocate. A base with no 2-byte varint skips the op; a family that
              // never fires anywhere fails the coverage assert below.
              const heads: number[] = [];
              for (let p = 0; p + 1 < bytes.length; p += 1) {
                if (byteAt(bytes, p) >= 0x80 && byteAt(bytes, p + 1) < 0x80) {
                  heads.push(p);
                }
              }
              if (heads.length === 0) {
                break;
              }
              const head = heads[rng.below(heads.length)];
              if (head === undefined) {
                break;
              }
              bytes[head] = 0xff;
              bytes[head + 1] = 0xff;
              families.inflateLength += 1;
              break;
            }
            default: {
              // Duplicated chunk appended: repeated fields, doubled messages, trailing
              // garbage - the shapes a hostile server splices in by hand.
              const chunkStart = rng.below(bytes.length);
              const chunkLen = 1 + rng.below(Math.min(CHUNK_LIMIT, bytes.length - chunkStart));
              const duplicated = new Uint8Array(bytes.length + chunkLen);
              duplicated.set(bytes);
              duplicated.set(bytes.subarray(chunkStart, chunkStart + chunkLen), bytes.length);
              bytes = duplicated;
              families.duplicateChunk += 1;
              break;
            }
          }
        }
        if (bytes.length > HARD_BOUND) {
          bytes = bytes.subarray(0, HARD_BOUND);
        }
        const outcome = checkEnvelope(bytes, `random iteration ${iteration} (base ${base.name})`);
        randomBuckets[outcome] += 1;
      }

      // --- The tile loop: the same contract one layer down, on decoded Tile values.
      const seeds = tileSeeds(corpus);
      expect(seeds.length, 'the tile seed pool is unexpectedly small').toBeGreaterThan(4);

      const tileBuckets: Buckets = { accepted: 0, cap: 0, grammar: 0 };
      const tileFamilies = { dataBitFlip: 0, dataResize: 0, rectField: 0, codecValue: 0 };

      for (let iteration = 0; iteration < TILE_ITERATIONS; iteration += 1) {
        const seed = seeds[rng.below(seeds.length)];
        if (seed === undefined) {
          throw new Error('the tile seed pool drew an absent seed');
        }
        const tile: Tile = {
          rect: seed.tile.rect === undefined ? undefined : { ...seed.tile.rect },
          codec: seed.tile.codec,
          data: seed.tile.data.slice(),
        };
        const draw = (): number =>
          rng.below(2) === 0 ? RECT_VALUE_POOL[rng.below(RECT_VALUE_POOL.length)] ?? 0 : rng.below(8192);
        const ops = 1 + rng.below(2);
        for (let op = 0; op < ops; op += 1) {
          switch (rng.below(4)) {
            case 0: {
              if (tile.data.length === 0) {
                break;
              }
              const at = rng.below(tile.data.length);
              tile.data[at] = byteAt(tile.data, at) ^ (1 << rng.below(8));
              tileFamilies.dataBitFlip += 1;
              break;
            }
            case 1: {
              if (rng.below(2) === 0) {
                tile.data = tile.data.slice(0, rng.below(tile.data.length + 1));
              } else {
                const grown = new Uint8Array(tile.data.length + 1 + rng.below(16));
                grown.set(tile.data);
                for (let i = tile.data.length; i < grown.length; i += 1) {
                  grown[i] = rng.byte();
                }
                tile.data = grown;
              }
              tileFamilies.dataResize += 1;
              break;
            }
            case 2: {
              if (tile.rect === undefined) {
                tile.rect = { x: draw(), y: draw(), width: draw(), height: draw() };
              } else {
                switch (rng.below(4)) {
                  case 0:
                    tile.rect.x = draw();
                    break;
                  case 1:
                    tile.rect.y = draw();
                    break;
                  case 2:
                    tile.rect.width = draw();
                    break;
                  default:
                    tile.rect.height = draw();
                    break;
                }
              }
              tileFamilies.rectField += 1;
              break;
            }
            default: {
              tile.codec = CODEC_VALUE_POOL[rng.below(CODEC_VALUE_POOL.length)] ?? 1;
              tileFamilies.codecValue += 1;
              break;
            }
          }
        }
        const label = `tile iteration ${iteration} (${seed.name})`;
        try {
          const decoded = decodeTile(tile, TILE_SURFACE_BOUNDS);
          tileBuckets.accepted += 1;
          const rect = tile.rect;
          if (rect === undefined) {
            failures.push(`${label}: decodeTile accepted a tile without a rect`);
            continue;
          }
          if (
            decoded.rect.x !== rect.x ||
            decoded.rect.y !== rect.y ||
            decoded.rect.width !== rect.width ||
            decoded.rect.height !== rect.height
          ) {
            failures.push(`${label}: the decoded rect does not match the tile's rect`);
          }
          if (decoded.imageData.width !== rect.width || decoded.imageData.height !== rect.height) {
            failures.push(`${label}: pixel dimensions do not match the rect`);
          }
          if (decoded.imageData.data.length !== rect.width * rect.height * 4) {
            failures.push(`${label}: pixel bytes are not exactly width*height*4`);
          }
        } catch (error) {
          if (error instanceof ProtocolError) {
            distinctRejectionFields.add(error.field ?? '(no field)');
            tileBuckets[classifyRejection(error.message)] += 1;
          } else {
            failures.push(`${label}: decodeTile threw ${describeError(error)}`);
          }
        }
      }

      // --- The fuzzer must exercise what it claims. The corpus and sweep asserts above are
      // --- structural; these prove the mutations themselves earn the coverage.
      for (const [family, fired] of Object.entries(families)) {
        expect(fired, `the mutation family ${family} never fired`).toBeGreaterThan(0);
      }
      for (const [family, fired] of Object.entries(tileFamilies)) {
        expect(fired, `the tile mutation family ${family} never fired`).toBeGreaterThan(0);
      }
      expect(randomBuckets.accepted, 'the random loop never accepted a mutation').toBeGreaterThan(0);
      expect(randomBuckets.cap, 'the random loop never reached a cap rejection').toBeGreaterThan(0);
      expect(randomBuckets.grammar, 'the random loop never reached a grammar rejection').toBeGreaterThan(0);
      expect(tileBuckets.accepted, 'the tile loop never accepted a mutation').toBeGreaterThan(0);
      expect(tileBuckets.cap, 'the tile loop never reached a cap rejection').toBeGreaterThan(0);
      expect(tileBuckets.grammar, 'the tile loop never reached a grammar rejection').toBeGreaterThan(0);

      if (failures.length > 0) {
        const shown = failures.slice(0, 5).join('\n  ');
        throw new Error(
          `decoder fuzz: ${failures.length} invariant violation(s); first ${Math.min(5, failures.length)}:\n  ${shown}`,
        );
      }

      // One summary line: the numbers the implementation report cites (iterations, buckets,
      // families, distinct rejection fields, wall time) without a second run.
      console.log(
        `decoder fuzz: ${ENVELOPE_ITERATIONS} envelope + ${TILE_ITERATIONS} tile iterations ` +
          `+ ${sweptCuts} truncation cuts (xorshift32 seed 0x${FUZZ_SEED.toString(16)}) | ` +
          `corpus a/c/g ${corpusBuckets.accepted}/${corpusBuckets.cap}/${corpusBuckets.grammar}, ` +
          `sweep a/c/g ${sweepBuckets.accepted}/${sweepBuckets.cap}/${sweepBuckets.grammar}, ` +
          `random a/c/g ${randomBuckets.accepted}/${randomBuckets.cap}/${randomBuckets.grammar}, ` +
          `tiles a/c/g ${tileBuckets.accepted}/${tileBuckets.cap}/${tileBuckets.grammar} | ` +
          `families ${Object.values(families).join('/')} + ${Object.values(tileFamilies).join('/')} | ` +
          `${distinctRejectionFields.size} distinct rejection fields | ${Date.now() - startedAt} ms`,
      );
    },
  );
});
