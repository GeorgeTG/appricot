/**
 * Tile pixel decoding: turns one wire Tile into an ImageData the host can draw into a canvas.
 *
 * The two codecs are defined by crates/appricot-proto/proto/appricot/v0/wire.proto:
 *   1  RAW  BGRX bytes, row-major, top to bottom, no stride padding (width * 4 per row).
 *   2  QOI  the QOI image format, implemented here from the published specification at
 *           https://qoiformat.org (checked 2026-09-23; the specification is CC0, public
 *           domain, and the reference implementation is MIT). No third-party code is used.
 *
 * Every tile is opaque (docs/protocol/v0.md §11). The fourth BGRX byte is unused, so RAW
 * ignores it, and the QOI alpha channel only feeds the decoder's own state (the index hash and
 * the carried alpha), never the pixels. A capture buffer therefore draws the same whichever
 * codec its tile got. The Rust decoder gives the same answers:
 * crates/appricot-encode/testdata/qoi-vectors.json pins both (tile.vector.test.ts).
 *
 * The server is untrusted (docs/adr/0003-untrusted-server-client.md): every bound - rect inside
 * the surface, tile caps, payload lengths, the QOI header matching the rect - is checked BEFORE
 * any pixel buffer is allocated, and a violation throws ProtocolError rather than reading or
 * writing out of range.
 */

import type { Rect, Tile } from './wire.js';
import { ProtocolError } from './wire.js';
import { CODEC, MAX_TILE_HEIGHT, MAX_TILE_WIDTH } from './limits.js';

/** One decoded tile: where it sits in the surface, and its pixels. */
export interface DecodedTile {
  readonly rect: Rect;
  readonly imageData: ImageData;
}

/**
 * Decodes one Tile from a Frame into pixels, bounded by the surface it belongs to.
 *
 * `surfaceBounds` is the surface's current size in surface coordinates (before scale). Every
 * bound that sizes the ImageData - the rect, the tile caps, the RAW length, the QOI header - is
 * checked before it is allocated. Anything that does not fit the contract of the tile codecs,
 * a malformed QOI chunk included, throws ProtocolError, and no ImageData is returned.
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

  // Each codec checks its payload against the rect, then decodes straight into the ImageData:
  // one allocation per tile, and no copy.
  const imageData =
    tile.codec === CODEC.RAW
      ? decodeRaw(tile.data, rect)
      : tile.codec === CODEC.QOI
        ? decodeQoi(tile.data, rect)
        : throwUnknownCodec(tile.codec);

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
function decodeRaw(data: Uint8Array, rect: Rect): ImageData {
  const expected = rect.width * rect.height * 4;
  if (data.length !== expected) {
    throw new ProtocolError(
      `RAW tile data is ${data.length} bytes, expected ${expected} (width*height*4)`,
      'Tile.data',
    );
  }
  const imageData = new ImageData(rect.width, rect.height);
  const out = imageData.data;
  for (let i = 0; i < expected; i += 4) {
    out[i] = byteAt(data, i + 2); // R <- the red byte (third in BGRX)
    out[i + 1] = byteAt(data, i + 1); // G
    out[i + 2] = byteAt(data, i); // B <- the blue byte (first in BGRX)
    out[i + 3] = 255; // X is unused; the pixel is opaque
  }
  return imageData;
}

// ---------------------------------------------------------------------------
// QOI, from the published specification (https://qoiformat.org, checked 2026-09-23)
// ---------------------------------------------------------------------------

const QOI_HEADER_BYTES = 14;
const QOI_END_MARKER = [0, 0, 0, 0, 0, 0, 0, 1];

/**
 * Codec 2: the QOI image format. The header's width and height must equal the tile rect's. The
 * 8-byte end marker is required, and nothing may follow it - a tile is one complete QOI stream,
 * not an open-ended one.
 *
 * Chunks decode as the specification says whatever the channels byte says (3 or 4): an RGBA
 * chunk is read in a 3-channel stream too, as the reference decoder reads it
 * (https://github.com/phoboslab/qoi/blob/master/qoi.h, checked 2026-09-23). The index stores the
 * current pixel after every chunk, a RUN chunk included, again as the reference decoder does.
 * Every pixel is drawn opaque: the stream's alpha feeds the index hash and the carried alpha,
 * never the output.
 */
function decodeQoi(data: Uint8Array, rect: Rect): ImageData {
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
  const imageData = new ImageData(rect.width, rect.height);
  const out = imageData.data;
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
      // QOI_OP_RUN: the previous pixel, 1 + the 6-bit count times. Stored in the index like
      // every other chunk's pixel; that only changes the index when the stream opens with a
      // RUN, whose pixel is the starting {0, 0, 0, 255}.
      const run = (byte & 0x3f) + 1;
      if (written + run > total) {
        throw new ProtocolError('QOI run writes past the tile', 'Tile.data');
      }
      storeInIndex(index, r, g, b, a);
      for (let i = 0; i < run; i += 1) {
        writePixel(out, written, r, g, b);
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
    writePixel(out, written, r, g, b);
    written += 1;
    storeInIndex(index, r, g, b, a);
  }

  if (data.length - pos !== QOI_END_MARKER.length) {
    throw new ProtocolError('QOI tile data does not end with the 8-byte end marker', 'Tile.data');
  }
  for (let i = 0; i < QOI_END_MARKER.length; i += 1) {
    if (data[pos + i] !== QOI_END_MARKER[i]) {
      throw new ProtocolError('QOI tile data does not end with the 8-byte end marker', 'Tile.data');
    }
  }

  return imageData;
}

/** Writes one opaque pixel: every tile is opaque, whatever alpha the stream carries. */
function writePixel(out: Uint8ClampedArray, pixel: number, r: number, g: number, b: number): void {
  const offset = pixel * 4;
  out[offset] = r;
  out[offset + 1] = g;
  out[offset + 2] = b;
  out[offset + 3] = 255;
}

/** Stores a pixel, with the stream's own alpha, at the slot the specification's hash names. */
function storeInIndex(index: Uint8Array, r: number, g: number, b: number, a: number): void {
  const slot = ((r * 3 + g * 5 + b * 7 + a * 11) & 63) * 4;
  index[slot] = r;
  index[slot + 1] = g;
  index[slot + 2] = b;
  index[slot + 3] = a;
}

function readU32Be(data: Uint8Array, offset: number): number {
  return (
    byteAt(data, offset) * 0x1000000 +
    (byteAt(data, offset + 1) << 16) +
    (byteAt(data, offset + 2) << 8) +
    byteAt(data, offset + 3)
  );
}
