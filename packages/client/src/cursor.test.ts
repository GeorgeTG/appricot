import { describe, expect, it } from 'vitest';

import { ProtocolError } from './protocol';
import type { CursorImage } from './protocol';
import { cursorOrigin, cursorToImageData, drawCursor } from './cursor';

/** 2x1 cursor: one opaque white pixel, one fully transparent pixel. */
const image: CursorImage = {
  serial: 1,
  width: 2,
  height: 1,
  hotspotX: 1,
  hotspotY: 0,
  // ARGB, premultiplied: white kept white; the transparent pixel is all zeroes.
  argbPremultiplied: new Uint8Array([255, 255, 255, 255, 0, 0, 0, 0]),
};

describe('cursorToImageData', () => {
  it('converts premultiplied ARGB to straight-alpha RGBA in ImageData order', () => {
    const out = cursorToImageData(image);

    expect(out.width).toBe(2);
    expect(out.height).toBe(1);
    expect([...out.data]).toEqual([255, 255, 255, 255, 0, 0, 0, 0]);
  });

  it('un-premultiplies half-alpha pixels', () => {
    // ARGB premultiplied: alpha 128, channels stored at half intensity.
    const half: CursorImage = {
      serial: 2,
      width: 1,
      height: 1,
      hotspotX: 0,
      hotspotY: 0,
      argbPremultiplied: new Uint8Array([128, 64, 64, 64]),
    };

    const out = cursorToImageData(half);

    // round(64 * 255 / 128) = 128: the straight colour is twice the stored one.
    expect([...out.data]).toEqual([128, 128, 128, 128]);
  });

  it('accepts a fully transparent row without dividing by zero', () => {
    const transparent: CursorImage = {
      serial: 3,
      width: 4,
      height: 1,
      hotspotX: 0,
      hotspotY: 0,
      argbPremultiplied: new Uint8Array(16), // alpha 0 everywhere, colour bytes 0
    };

    const out = cursorToImageData(transparent);

    expect([...out.data]).toEqual(new Array(16).fill(0));
  });

  it('rejects a byte length that does not match width*height*4', () => {
    const broken: CursorImage = { ...image, argbPremultiplied: new Uint8Array(7) };
    expect(() => cursorToImageData(broken)).toThrow(ProtocolError);
  });

  it('rejects a negative size, and a zero size that still carries bytes', () => {
    expect(() => cursorToImageData({ ...image, width: 0 })).toThrow(ProtocolError);
    expect(() => cursorToImageData({ ...image, height: -2 })).toThrow(ProtocolError);
  });

  it('turns an empty cursor, which the decoder accepts, into one transparent pixel', () => {
    const empty: CursorImage = {
      ...image,
      width: 0,
      height: 0,
      argbPremultiplied: new Uint8Array(0),
    };

    const out = cursorToImageData(empty);

    expect(out.width).toBe(1);
    expect(out.height).toBe(1);
    expect([...out.data]).toEqual([0, 0, 0, 0]);
  });
});

function recordingContext(): {
  ctx: CanvasRenderingContext2D;
  calls: { dx: number; dy: number; width: number }[];
} {
  const calls: { dx: number; dy: number; width: number }[] = [];
  const ctx = {
    putImageData: (imgData: ImageData, dx: number, dy: number): void => {
      calls.push({ dx, dy, width: imgData.width });
    },
  } as unknown as CanvasRenderingContext2D;
  return { ctx, calls };
}

describe('drawCursor', () => {
  it('offsets by the hotspot so the hotspot lands on the given point', () => {
    const { ctx, calls } = recordingContext();

    drawCursor(ctx, image, 100, 50);

    expect(calls).toEqual([{ dx: 99, dy: 50, width: 2 }]);
  });

  it('draws at the canvas origin by default, so an image-sized overlay crops nothing', () => {
    // A 16x16 I-beam with its hotspot in the middle: the documented overlay pattern puts the
    // overlay at cursorOrigin() and calls drawCursor(ctx, image). Nothing may land at a
    // negative offset, where putImageData would clip it.
    const ibeam: CursorImage = {
      serial: 4,
      width: 16,
      height: 16,
      hotspotX: 8,
      hotspotY: 8,
      argbPremultiplied: new Uint8Array(16 * 16 * 4),
    };
    const { ctx, calls } = recordingContext();

    drawCursor(ctx, ibeam);

    expect(calls).toEqual([{ dx: 0, dy: 0, width: 16 }]);
    expect(cursorOrigin(ibeam, 200, 120)).toEqual({ x: 192, y: 112 });
  });
});
