import { describe, expect, it } from 'vitest';

import { ProtocolError } from './protocol';
import type { CursorImage } from './protocol';
import { cursorToImageData, drawCursor } from './cursor';

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

  it('rejects a non-positive size', () => {
    expect(() => cursorToImageData({ ...image, width: 0 })).toThrow(ProtocolError);
    expect(() => cursorToImageData({ ...image, height: -2 })).toThrow(ProtocolError);
  });
});

describe('drawCursor', () => {
  it('offsets by the hotspot so the hotspot lands on the given point', () => {
    const calls: { dx: number; dy: number; width: number }[] = [];
    const ctx = {
      putImageData: (imgData: ImageData, dx: number, dy: number): void => {
        calls.push({ dx, dy, width: imgData.width });
      },
    } as unknown as CanvasRenderingContext2D;

    drawCursor(ctx, image); // default (0, 0)
    drawCursor(ctx, image, 100, 50);

    expect(calls).toEqual([
      { dx: -1, dy: 0, width: 2 },
      { dx: 99, dy: 50, width: 2 },
    ]);
  });
});
