/**
 * Cursor pixels: the wire's premultiplied ARGB turned into an `ImageData`.
 *
 * The server is untrusted (ADR-0003 §5): we hand the host pixels and math, and the host owns
 * every DOM element. The overlay is positioned by the host; `drawCursor` only offsets by the
 * hotspot so the hotspot lands where the host put the pointer.
 */
import { ProtocolError } from './protocol';
import type { CursorImage } from './protocol';

/** Un-premultiplies one channel: 0 stays 0, otherwise round(p * 255 / a), capped at 255. */
function unpremultiplyChannel(premultiplied: number, alpha: number): number {
  if (alpha === 0) {
    return 0;
  }
  return Math.min(255, Math.round((premultiplied * 255) / alpha));
}

/**
 * Builds an `ImageData` without assuming the constructor exists: plain Node (the test runner)
 * has no `ImageData` global, so fall back to a structurally identical object. Every consumer in
 * this package reads only `width`, `height` and `data`.
 */
function makeImageData(
  data: Uint8ClampedArray<ArrayBuffer>,
  width: number,
  height: number,
): ImageData {
  if (typeof ImageData === 'function') {
    return new ImageData(data, width, height);
  }
  return { width, height, data, colorSpace: 'srgb' } as ImageData;
}

/**
 * Converts one `CursorImage` to straight-alpha RGBA, the byte order `ImageData` wants.
 *
 * The wire sends ARGB with premultiplied alpha, row-major from the top. Fully transparent
 * pixels become zeroed RGBA, which is what a transparent cursor row should be. The byte
 * length is checked against `width * height * 4` before anything is allocated, and a
 * mismatch is a `ProtocolError`, never a short read (ADR-0003 §4).
 */
export function cursorToImageData(image: CursorImage): ImageData {
  const { width, height, argbPremultiplied } = image;
  if (!Number.isInteger(width) || !Number.isInteger(height) || width <= 0 || height <= 0) {
    throw new ProtocolError('CursorImage width/height must be positive integers');
  }
  const bytes = width * height * 4;
  if (argbPremultiplied.length !== bytes) {
    throw new ProtocolError(
      `CursorImage.argb_premultiplied must be exactly ${bytes} bytes for ${width}x${height}`,
    );
  }
  // An explicit ArrayBuffer keeps the TS 5.7+ buffer-generic types happy for ImageData.
  const rgba = new Uint8ClampedArray(new ArrayBuffer(bytes));
  for (let i = 0; i < bytes; i += 4) {
    const a = argbPremultiplied[i] ?? 0;
    rgba[i] = unpremultiplyChannel(argbPremultiplied[i + 1] ?? 0, a);
    rgba[i + 1] = unpremultiplyChannel(argbPremultiplied[i + 2] ?? 0, a);
    rgba[i + 2] = unpremultiplyChannel(argbPremultiplied[i + 3] ?? 0, a);
    rgba[i + 3] = a;
  }
  return makeImageData(rgba, width, height);
}

/**
 * Draws the cursor onto `ctx` so that its hotspot sits at `(x, y)`. The host positions the
 * overlay canvas (usually its origin at the pointer) and calls this with `x = y = 0`;
 * passing the pointer position directly works too. `putImageData` ignores transforms, so the
 * placement is exact whatever else the host drew.
 */
export function drawCursor(
  ctx: CanvasRenderingContext2D,
  image: CursorImage,
  x = 0,
  y = 0,
): void {
  ctx.putImageData(cursorToImageData(image), x - image.hotspotX, y - image.hotspotY);
}
