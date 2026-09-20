/**
 * Tile pixel decoding: turns one wire Tile into an ImageData the host can draw into a canvas.
 *
 * The two codecs are defined by crates/appricot-proto/proto/appricot/v0/wire.proto:
 *   1  RAW  BGRX bytes, row-major, top to bottom, no stride padding (width * 4 per row).
 *   2  QOI  the QOI image format, implemented here from the published specification at
 *           https://qoiformat.org (checked 2026-09-20; the format is BSD-3-Clause). No
 *           third-party code is used.
 *
 * The server is untrusted (docs/adr/0003-untrusted-server-client.md): every bound - rect inside
 * the surface, tile caps, payload lengths, the QOI header matching the rect - is checked BEFORE
 * any pixel buffer is allocated, and a violation throws ProtocolError rather than reading or
 * writing out of range.
 */

import type { Rect, Tile } from './wire';
import { ProtocolError } from './wire';
import { CODEC, MAX_TILE_HEIGHT, MAX_TILE_WIDTH } from './limits';

/** One decoded tile: where it sits in the surface, and its pixels. */
export interface DecodedTile {
  readonly rect: Rect;
  readonly imageData: ImageData;
}

/**
 * Decodes one Tile from a Frame into pixels, bounded by the surface it belongs to.
 *
 * `surfaceBounds` is the surface's current size in surface coordinates (before scale). Every
 * check happens before the ImageData is allocated; anything that does not fit the contract of
 * the tile codecs throws ProtocolError.
 */
export function decodeTile(tile: Tile, surfaceBounds: { width: number; height: number }): DecodedTile {
  if (
    !Number.isSafeInteger(surfaceBounds.width) ||
    !Number.isSafeInteger(surfaceBounds.height) ||
    surfaceBounds.width < 0 ||
    surfaceBounds.height < 0
  ) {
    throw new ProtocolError('surfaceBounds is not a non-negative integer size', 'surfaceBounds');
  }
  const rect = tile.rect;
  if (rect === undefined) {
    throw new ProtocolError('Tile.rect is absent', 'Tile.rect');
  }
  if (rect.width > MAX_TILE_WIDTH) {
    throw new ProtocolError('Tile.rect.width is over MAX_TILE_WIDTH', 'Tile.rect.width');
  }
  if (rect.height > MAX_TILE_HEIGHT) {
    throw new ProtocolError('Tile.rect.height is over MAX_TILE_HEIGHT', 'Tile.rect.height');
  }
  if (rect.x < 0 || rect.y < 0) {
    throw new ProtocolError('tile rect starts outside the surface', 'Tile.rect');
  }
  if (rect.x + rect.width > surfaceBounds.width || rect.y + rect.height > surfaceBounds.height) {
    throw new ProtocolError('tile rect runs past the surface bounds', 'Tile.rect');
  }
  if (rect.width === 0 || rect.height === 0) {
    throw new ProtocolError('tile rect is empty', 'Tile.rect');
  }

  const pixels =
    tile.codec === CODEC.RAW
      ? decodeRaw(tile.data, rect)
      : tile.codec === CODEC.QOI
        ? decodeQoi(tile.data, rect)
        : throwUnknownCodec(tile.codec);

  const imageData = new ImageData(rect.width, rect.height);
  imageData.data.set(pixels);
  return {
    rect: { ...rect },
    imageData,
  };
}

function throwUnknownCodec(codec: number): never {
  throw new ProtocolError(`unknown tile codec ${codec}`, 'Tile.codec');
}

/** Checked byte read: the bounds were validated before the loop, this keeps the compiler honest. */
function byteAt(data: Uint8Array, offset: number): number {
  const value = data[offset];
  if (value === undefined) {
    throw new ProtocolError('tile data is shorter than validated', 'Tile.data');
  }
  return value;
}

/** Codec 1: BGRX bytes, exactly width*4 per row, become RGBA with a forced opaque alpha. */
function decodeRaw(data: Uint8Array, rect: Rect): Uint8ClampedArray {
  const expected = rect.width * rect.height * 4;
  if (data.length !== expected) {
    throw new ProtocolError(
      `RAW tile data is ${data.length} bytes, expected ${expected} (width*height*4)`,
      'Tile.data',
    );
  }
  const out = new Uint8ClampedArray(expected);
  for (let i = 0; i < expected; i += 4) {
    out[i] = byteAt(data, i + 2); // R <- the red byte (third in BGRX)
    out[i + 1] = byteAt(data, i + 1); // G
    out[i + 2] = byteAt(data, i); // B <- the blue byte (first in BGRX)
    out[i + 3] = 255; // X is unused; the pixel is opaque
  }
  return out;
}

// ---------------------------------------------------------------------------
// QOI, from the published specification (https://qoiformat.org, checked 2026-09-20)
// ---------------------------------------------------------------------------

const QOI_HEADER_BYTES = 14;
const QOI_END_MARKER = [0, 0, 0, 0, 0, 0, 0, 1];

/**
 * Codec 2: the QOI image format. The header's width and height must equal the tile rect's;
 * 3-channel data decodes to opaque pixels. The 8-byte end marker is required, and nothing may
 * follow it - a tile is one complete QOI stream, not an open-ended one.
 */
function decodeQoi(data: Uint8Array, rect: Rect): Uint8ClampedArray {
  if (data.length < QOI_HEADER_BYTES) {
    throw new ProtocolError(
      `QOI tile data is ${data.length} bytes, shorter than the ${QOI_HEADER_BYTES}-byte header`,
      'Tile.data',
    );
  }
  if (data[0] !== 0x71 || data[1] !== 0x6f || data[2] !== 0x69 || data[3] !== 0x66) {
    throw new ProtocolError('QOI tile data has no "qoif" magic', 'Tile.data');
  }
  const headerWidth = readU32Be(data, 4);
  const headerHeight = readU32Be(data, 8);
  if (headerWidth !== rect.width || headerHeight !== rect.height) {
    throw new ProtocolError(
      `QOI header says ${headerWidth}x${headerHeight}, the tile rect says ${rect.width}x${rect.height}`,
      'Tile.data',
    );
  }
  const channels = data[12];
  if (channels !== 3 && channels !== 4) {
    throw new ProtocolError(`QOI header names ${channels} channels, expected 3 or 4`, 'Tile.data');
  }
  const colorspace = data[13];
  if (colorspace !== undefined && colorspace > 1) {
    throw new ProtocolError(`QOI header names colorspace ${colorspace}, expected 0 or 1`, 'Tile.data');
  }

  const total = rect.width * rect.height;
  const out = new Uint8ClampedArray(total * 4);
  // The 64-slot previously-seen-pixel cache, zero-initialized, and the running pixel, which the
  // specification starts at {0, 0, 0, 255}.
  const index = new Uint8Array(64 * 4);
  let r = 0;
  let g = 0;
  let b = 0;
  let a = 255;
  let pos = QOI_HEADER_BYTES;
  let written = 0;

  const at = (offset: number): number => {
    const value = data[pos + offset];
    if (value === undefined) {
      throw new ProtocolError('QOI tile data ends mid-chunk', 'Tile.data');
    }
    return value;
  };

  while (written < total) {
    const byte = at(0);
    pos += 1;
    if (byte === 0xff) {
      // QOI_OP_RGBA: a full pixel follows.
      r = at(0);
      g = at(1);
      b = at(2);
      a = at(3);
      pos += 4;
    } else if (byte === 0xfe) {
      // QOI_OP_RGB: red, green, blue follow; alpha carries over.
      r = at(0);
      g = at(1);
      b = at(2);
      pos += 3;
    } else if ((byte & 0xc0) === 0xc0) {
      // QOI_OP_RUN: the previous pixel, 1 + the 6-bit count times.
      const run = (byte & 0x3f) + 1;
      if (written + run > total) {
        throw new ProtocolError('QOI run writes past the tile', 'Tile.data');
      }
      for (let i = 0; i < run; i += 1) {
        writePixel(out, written, r, g, b, a);
        written += 1;
      }
      continue;
    } else if ((byte & 0xc0) === 0x00) {
      // QOI_OP_INDEX: a pixel from the cache.
      const slot = (byte & 0x3f) * 4;
      r = byteAt(index, slot);
      g = byteAt(index, slot + 1);
      b = byteAt(index, slot + 2);
      a = byteAt(index, slot + 3);
    } else if ((byte & 0xc0) === 0x40) {
      // QOI_OP_DIFF: 2-bit deltas, each biased by -2, wrapping mod 256.
      r = (r + ((byte >> 4) & 0x03) - 2) & 0xff;
      g = (g + ((byte >> 2) & 0x03) - 2) & 0xff;
      b = (b + (byte & 0x03) - 2) & 0xff;
    } else {
      // QOI_OP_LUMA: a 6-bit green delta (bias -32), then a byte of red and blue deltas
      // relative to green (4-bit each, bias -8), all wrapping mod 256.
      const vg = (byte & 0x3f) - 32;
      const next = at(0);
      pos += 1;
      r = (r + vg + ((next >> 4) & 0x0f) - 8) & 0xff;
      g = (g + vg) & 0xff;
      b = (b + vg + (next & 0x0f) - 8) & 0xff;
    }
    writePixel(out, written, r, g, b, a);
    written += 1;
    const slot = (r * 3 + g * 5 + b * 7 + a * 11) & 63;
    index[slot * 4] = r;
    index[slot * 4 + 1] = g;
    index[slot * 4 + 2] = b;
    index[slot * 4 + 3] = a;
  }

  if (data.length - pos !== QOI_END_MARKER.length) {
    throw new ProtocolError('QOI tile data does not end with the 8-byte end marker', 'Tile.data');
  }
  for (let i = 0; i < QOI_END_MARKER.length; i += 1) {
    if (data[pos + i] !== QOI_END_MARKER[i]) {
      throw new ProtocolError('QOI tile data does not end with the 8-byte end marker', 'Tile.data');
    }
  }

  if (channels === 3) {
    // A 3-channel tile is opaque whatever a hostile encoder stuffed into alpha slots.
    for (let i = 3; i < out.length; i += 4) {
      out[i] = 255;
    }
  }
  return out;
}

function writePixel(out: Uint8ClampedArray, pixel: number, r: number, g: number, b: number, a: number): void {
  const offset = pixel * 4;
  out[offset] = r;
  out[offset + 1] = g;
  out[offset + 2] = b;
  out[offset + 3] = a;
}

function readU32Be(data: Uint8Array, offset: number): number {
  return (
    byteAt(data, offset) * 0x1000000 +
    (byteAt(data, offset + 1) << 16) +
    (byteAt(data, offset + 2) << 8) +
    byteAt(data, offset + 3)
  );
}
