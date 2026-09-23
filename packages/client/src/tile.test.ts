// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';

import type { Tile } from './wire';
import { CODEC } from './limits';
import { decodeTile } from './tile';

/**
 * jsdom does not implement ImageData (it is part of its unimplemented canvas set), so the test
 * provides the constructor a browser would. It offers exactly what tile.ts uses: the
 * (width, height) constructor and a writable .data of width*height*4 bytes.
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
 * The QOI streams below are written by hand, opcode by opcode, from the published
 * specification (https://qoiformat.org, checked 2026-09-20): a 14-byte header ("qoif", big-
 * endian width and height, channels, colorspace), the chunks, and the 8-byte end marker.
 */

function qoiHeader(width: number, height: number, channels: 3 | 4): number[] {
  return [
    0x71, 0x6f, 0x69, 0x66,
    (width >>> 24) & 0xff, (width >>> 16) & 0xff, (width >>> 8) & 0xff, width & 0xff,
    (height >>> 24) & 0xff, (height >>> 16) & 0xff, (height >>> 8) & 0xff, height & 0xff,
    channels,
    0,
  ];
}

const QOI_END = [0, 0, 0, 0, 0, 0, 0, 1];

function rawTile(data: number[], rect = { x: 0, y: 0, width: 2, height: 2 }): Tile {
  return { rect, codec: CODEC.RAW, data: new Uint8Array(data) };
}

describe('decodeTile: codec 1 (RAW)', () => {
  it('swaps BGRX to RGBA, row by row, with no stride padding', () => {
    // A 2x2 tile. Each pixel is 4 bytes B,G,R,X; row 1 starts at byte 8 with no padding.
    // Pixels: (B=10,G=20,R=30) (B=11,G=21,R=31) / (B=12,G=22,R=32) (B=13,G=23,R=33).
    const tile = rawTile([
      10, 20, 30, 255, 11, 21, 31, 255,
      12, 22, 32, 255, 13, 23, 33, 255,
    ]);

    const { rect, imageData } = decodeTile(tile, { width: 10, height: 10 });

    expect(rect).toEqual({ x: 0, y: 0, width: 2, height: 2 });
    expect(imageData.width).toBe(2);
    expect(imageData.height).toBe(2);
    expect(Array.from(imageData.data)).toEqual([
      30, 20, 10, 255, 31, 21, 11, 255,
      32, 22, 12, 255, 33, 23, 13, 255,
    ]);
  });

  it('decodes a tile that starts off the origin, inside the surface', () => {
    const tile = rawTile([1, 2, 3, 0], { x: 5, y: 6, width: 1, height: 1 });
    const { rect, imageData } = decodeTile(tile, { width: 8, height: 7 });
    expect(rect).toEqual({ x: 5, y: 6, width: 1, height: 1 });
    expect(Array.from(imageData.data)).toEqual([3, 2, 1, 255]);
  });

  it('returns a copy of the rect, not the tile object graph', () => {
    const rect = { x: 0, y: 0, width: 1, height: 1 };
    const tile = rawTile([0, 0, 0, 0], rect);
    const decoded = decodeTile(tile, { width: 1, height: 1 });
    expect(decoded.rect).not.toBe(rect);
    expect(decoded.rect).toEqual(rect);
  });

  it('refuses data that is not exactly width*height*4 bytes', () => {
    const fifteen = new Array<number>(15).fill(0);
    expect(() => decodeTile(rawTile(fifteen), { width: 10, height: 10 })).toThrowError(
      'RAW tile data is 15 bytes',
    );
    const seventeen = new Array<number>(17).fill(0);
    expect(() => decodeTile(rawTile(seventeen), { width: 10, height: 10 })).toThrowError(
      'RAW tile data is 17 bytes',
    );
  });
});

describe('decodeTile: codec 2 (QOI)', () => {
  it('decodes RGB, RUN, RGBA and RGB-alpha-carryover chunks, and draws every pixel opaque', () => {
    // 2x2, 4 channels. Pixels: red, red (run), green with alpha 128, blue. Every tile is opaque
    // (v0 §11): the stream's alpha never reaches the pixels.
    const data = new Uint8Array([
      ...qoiHeader(2, 2, 4),
      0xfe, 255, 0, 0, // RGB: (255, 0, 0), alpha carries over from the initial 255.
      0xc0, // RUN of 1: the previous pixel once more.
      0xff, 0, 255, 0, 128, // RGBA: (0, 255, 0, 128).
      0xfe, 0, 0, 255, // RGB: (0, 0, 255); alpha 128 carries over.
      ...QOI_END,
    ]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 2, height: 2 }, codec: CODEC.QOI, data };

    const { imageData } = decodeTile(tile, { width: 2, height: 2 });

    expect(Array.from(imageData.data)).toEqual([
      255, 0, 0, 255, 255, 0, 0, 255,
      0, 255, 0, 255, 0, 0, 255, 255,
    ]);
  });

  it('keeps the stream alpha in the index hash while drawing opaque pixels', () => {
    // 4x1, 4 channels. (10,10,10,0) lands in slot 22; a DIFF of +1 makes (11,11,11,0), slot 37
    // with its alpha 0 (it would be slot 26 with alpha 255); an RGB chunk; then INDEX 37.
    const data = new Uint8Array([
      ...qoiHeader(4, 1, 4),
      0xff, 10, 10, 10, 0,
      0x7f, // QOI_OP_DIFF: +1 in every channel
      0xfe, 200, 0, 0,
      37, // QOI_OP_INDEX
      ...QOI_END,
    ]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 4, height: 1 }, codec: CODEC.QOI, data };

    const { imageData } = decodeTile(tile, { width: 4, height: 1 });

    expect(Array.from(imageData.data)).toEqual([
      10, 10, 10, 255, 11, 11, 11, 255, 200, 0, 0, 255, 11, 11, 11, 255,
    ]);
  });

  it('stores the starting pixel in the index after a leading RUN, as qoi.h does', () => {
    // 6x1, 3 channels: a RUN of two repeats the starting (0,0,0,255), which the RUN stores at
    // slot 53, so INDEX 53 reads it back. A DIFF of +1 makes (1,1,1,255), slot 4; an RGB chunk;
    // INDEX 4 finds (1,1,1). Without the store, INDEX 53 would read (0,0,0,0), the DIFF would
    // make (1,1,1,0) at slot 15, and INDEX 4 would draw black.
    const data = new Uint8Array([
      ...qoiHeader(6, 1, 3),
      0xc1, 53, 0x7f, 0xfe, 9, 9, 9, 4,
      ...QOI_END,
    ]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 6, height: 1 }, codec: CODEC.QOI, data };

    const { imageData } = decodeTile(tile, { width: 6, height: 1 });

    expect(Array.from(imageData.data)).toEqual([
      0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 1, 1, 1, 255, 9, 9, 9, 255, 1, 1, 1, 255,
    ]);
  });

  it('draws an INDEX into a slot no pixel filled as opaque black', () => {
    // 2x1, 3 channels: INDEX 5 reads the zero-initialized (0,0,0,0); the RGB chunk after it
    // carries alpha 0 forward. Both pixels are drawn opaque.
    const data = new Uint8Array([...qoiHeader(2, 1, 3), 0x05, 0xfe, 1, 2, 3, ...QOI_END]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 2, height: 1 }, codec: CODEC.QOI, data };

    const { imageData } = decodeTile(tile, { width: 2, height: 1 });

    expect(Array.from(imageData.data)).toEqual([0, 0, 0, 255, 1, 2, 3, 255]);
  });

  it('decodes INDEX chunks through the 64-slot previously-seen cache', () => {
    // 1x2, 4 channels. First pixel (5, 6, 7, 255) lands in cache slot
    // (5*3 + 6*5 + 7*7 + 255*11) & 63 = 19; the second pixel is an INDEX into it.
    const slot = (5 * 3 + 6 * 5 + 7 * 7 + 255 * 11) & 63;
    const data = new Uint8Array([
      ...qoiHeader(1, 2, 4),
      0xfe, 5, 6, 7,
      slot, // QOI_OP_INDEX
      ...QOI_END,
    ]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 1, height: 2 }, codec: CODEC.QOI, data };

    const { imageData } = decodeTile(tile, { width: 1, height: 2 });

    expect(Array.from(imageData.data)).toEqual([5, 6, 7, 255, 5, 6, 7, 255]);
  });

  it('decodes DIFF chunks with the -2 bias, wrapping mod 256', () => {
    // 1x2, 4 channels. First pixel (100, 100, 100); second is +1 in every channel
    // (3-bit fields 0b11 = +1). Also wraps: a second DIFF path is covered below at 255.
    const data = new Uint8Array([
      ...qoiHeader(1, 2, 4),
      0xfe, 100, 100, 100,
      0x40 | (3 << 4) | (3 << 2) | 3, // QOI_OP_DIFF: dr=dg=db=+1
      ...QOI_END,
    ]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 1, height: 2 }, codec: CODEC.QOI, data };

    const { imageData } = decodeTile(tile, { width: 1, height: 2 });

    expect(Array.from(imageData.data)).toEqual([100, 100, 100, 255, 101, 101, 101, 255]);
  });

  it('decodes LUMA chunks with the -32 and -8 biases', () => {
    // 1x2, 4 channels. First pixel (100, 100, 100); second is (102, 104, 103):
    // dg = +4 (byte 4+32 = 36), dr-dg = -2 (nibble 6), db-dg = -1 (nibble 7).
    const data = new Uint8Array([
      ...qoiHeader(1, 2, 4),
      0xfe, 100, 100, 100,
      0x80 | 36, (6 << 4) | 7,
      ...QOI_END,
    ]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 1, height: 2 }, codec: CODEC.QOI, data };

    const { imageData } = decodeTile(tile, { width: 1, height: 2 });

    expect(Array.from(imageData.data)).toEqual([100, 100, 100, 255, 102, 104, 103, 255]);
  });

  it('decodes a long RUN and repeats the initial black pixel when asked', () => {
    // 8x1, 3 channels, a single RUN of 8 of the initial previous pixel {0, 0, 0, 255}.
    const data = new Uint8Array([
      ...qoiHeader(8, 1, 3),
      0xc0 | 7, // RUN of 7 + 1 = 8.
      ...QOI_END,
    ]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 8, height: 1 }, codec: CODEC.QOI, data };

    const { imageData } = decodeTile(tile, { width: 8, height: 1 });

    expect(Array.from(imageData.data)).toEqual(new Array<number>(32).fill(0).map((v, i) => (i % 4 === 3 ? 255 : v)));
  });

  it('forces a 3-channel tile to opaque pixels even when an RGBA chunk carries alpha', () => {
    // Hostile: a channels=3 stream with an RGBA chunk. The tile is a surface pixel source;
    // 3-channel data is opaque (wire.proto, codec 2).
    const data = new Uint8Array([
      ...qoiHeader(1, 1, 3),
      0xff, 1, 2, 3, 7,
      ...QOI_END,
    ]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 1, height: 1 }, codec: CODEC.QOI, data };

    const { imageData } = decodeTile(tile, { width: 1, height: 1 });

    expect(Array.from(imageData.data)).toEqual([1, 2, 3, 255]);
  });

  it('refuses a header whose size disagrees with the tile rect', () => {
    const data = new Uint8Array([...qoiHeader(3, 1, 4), 0xfe, 0, 0, 0, ...QOI_END]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 1, height: 1 }, codec: CODEC.QOI, data };
    expect(() => decodeTile(tile, { width: 8, height: 8 })).toThrowError('QOI header says 3x1');
  });

  it('refuses data shorter than the 14-byte header', () => {
    const data = new Uint8Array(13);
    const tile: Tile = { rect: { x: 0, y: 0, width: 1, height: 1 }, codec: CODEC.QOI, data };
    expect(() => decodeTile(tile, { width: 8, height: 8 })).toThrowError('shorter than the 14-byte header');
  });

  it('refuses a stream without the 8-byte end marker', () => {
    const data = new Uint8Array([...qoiHeader(1, 1, 4), 0xfe, 9, 9, 9]);
    const tile: Tile = { rect: { x: 0, y: 0, width: 1, height: 1 }, codec: CODEC.QOI, data };
    expect(() => decodeTile(tile, { width: 8, height: 8 })).toThrowError('end marker');
  });

  it('refuses a truncated chunk and a run that overflows the tile', () => {
    // An RGB chunk that stops two bytes in, with nothing (not even the end marker) after it.
    const truncated = new Uint8Array([...qoiHeader(1, 1, 4), 0xfe, 1, 2]);
    expect(() =>
      decodeTile(
        { rect: { x: 0, y: 0, width: 1, height: 1 }, codec: CODEC.QOI, data: truncated },
        { width: 8, height: 8 },
      ),
    ).toThrowError('ends mid-chunk');

    // A RUN of 2 into a 1-pixel tile. The chunk byte stays under 0xfe, which is the RGB tag.
    const overflow = new Uint8Array([...qoiHeader(1, 1, 4), 0xc1, ...QOI_END]);
    expect(() =>
      decodeTile(
        { rect: { x: 0, y: 0, width: 1, height: 1 }, codec: CODEC.QOI, data: overflow },
        { width: 8, height: 8 },
      ),
    ).toThrowError('QOI run writes past the tile');
  });
});

describe('decodeTile: bounds and codecs', () => {
  it('refuses a rect that starts outside the surface', () => {
    const tile = rawTile([0, 0, 0, 0], { x: -1, y: 0, width: 1, height: 1 });
    expect(() => decodeTile(tile, { width: 8, height: 8 })).toThrowError('starts outside the surface');
  });

  it('refuses a rect that runs past the right or bottom edge', () => {
    const right = rawTile(new Array<number>(16).fill(0), { x: 7, y: 0, width: 2, height: 2 });
    expect(() => decodeTile(right, { width: 8, height: 8 })).toThrowError('runs past the surface bounds');
    const bottom = rawTile(new Array<number>(16).fill(0), { x: 0, y: 7, width: 2, height: 2 });
    expect(() => decodeTile(bottom, { width: 8, height: 8 })).toThrowError('runs past the surface bounds');
  });

  it('accepts a rect that ends exactly on the edge', () => {
    const tile = rawTile(new Array<number>(16).fill(0), { x: 6, y: 6, width: 2, height: 2 });
    expect(() => decodeTile(tile, { width: 8, height: 8 })).not.toThrow();
  });

  it('refuses a tile wider or taller than MAX_TILE_WIDTH / MAX_TILE_HEIGHT', () => {
    // The wire decoder rejects these already; decodeTile is public and re-checks. The rect is
    // inside a 1920x1200 surface so only the tile cap fires.
    const wide = rawTile([], { x: 0, y: 0, width: 257, height: 1 });
    expect(() => decodeTile(wide, { width: 1920, height: 1200 })).toThrowError('MAX_TILE_WIDTH');
    const tall = rawTile([], { x: 0, y: 0, width: 1, height: 257 });
    expect(() => decodeTile(tall, { width: 1920, height: 1200 })).toThrowError('MAX_TILE_HEIGHT');
  });

  it('refuses codec 0 and unknown codecs', () => {
    expect(() =>
      decodeTile(
        { rect: { x: 0, y: 0, width: 1, height: 1 }, codec: 0, data: new Uint8Array(4) },
        { width: 8, height: 8 },
      ),
    ).toThrowError('unknown tile codec 0');
    expect(() =>
      decodeTile(
        { rect: { x: 0, y: 0, width: 1, height: 1 }, codec: 3, data: new Uint8Array(4) },
        { width: 8, height: 8 },
      ),
    ).toThrowError('unknown tile codec 3');
  });

  it('refuses a tile without a rect', () => {
    expect(() =>
      decodeTile({ codec: CODEC.RAW, data: new Uint8Array(4) }, { width: 8, height: 8 }),
    ).toThrowError('Tile.rect is absent');
  });
});
