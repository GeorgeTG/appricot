// The node environment, not jsdom: this file reads placement.json from the repository through
// `new URL(..., import.meta.url)`, and under the jsdom environment that URL resolves against
// jsdom's page instead of the file system.
import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import type { Anchor, Positioner, Rect } from './protocol.js';
import { placePopup } from './registry.js';

/**
 * The shared popup placement vectors (contract C10): appricot-core's `Positioner::place`
 * writes crates/appricot-core/testdata/placement.json (tests/placement_vectors.rs), and this
 * file proves `placePopup` puts every case exactly where core does. Gravity is the direction
 * the popup grows from the anchor point on both sides; a drift turns this red.
 *
 * Format: { format: 1, cases: [{ name, anchor_rect: [x, y, w, h], anchor, gravity,
 * offset: [x, y], size: [w, h], placed: [x, y, w, h] }] }, anchors as wire numbers.
 */
interface PlacementCase {
  name: string;
  anchor_rect: [number, number, number, number];
  anchor: number;
  gravity: number;
  offset: [number, number];
  size: [number, number];
  placed: [number, number, number, number];
}

interface PlacementFile {
  format: number;
  cases: PlacementCase[];
}

const placementUrl = new URL(
  '../../../crates/appricot-core/testdata/placement.json',
  import.meta.url,
);

function loadCases(): PlacementCase[] {
  const parsed = JSON.parse(readFileSync(placementUrl, 'utf8')) as PlacementFile;
  if (parsed.format !== 1 || !Array.isArray(parsed.cases)) {
    throw new Error('placement.json has an unexpected shape');
  }
  return parsed.cases;
}

function asAnchor(value: number): Anchor {
  if (!Number.isInteger(value) || value < 0 || value > 8) {
    throw new Error(`placement.json names anchor ${String(value)}, not a wire Anchor`);
  }
  return value as Anchor;
}

function toRect([x, y, width, height]: [number, number, number, number]): Rect {
  return { x, y, width, height };
}

const cases = loadCases();

describe('placePopup against the core placement vectors', () => {
  it('covers every anchor x gravity pair twice, plus the X11 and edge cases', () => {
    expect(cases).toHaveLength(2 * 81 + 6);
    const pairs = new Set(cases.map((c) => `${String(c.anchor)}/${String(c.gravity)}`));
    expect(pairs.size).toBe(81);
  });

  it.each(cases)('$name', (c) => {
    const positioner: Positioner = {
      anchorRect: toRect(c.anchor_rect),
      anchor: asAnchor(c.anchor),
      gravity: asAnchor(c.gravity),
      offset: { x: c.offset[0], y: c.offset[1] },
      size: { width: c.size[0], height: c.size[1] },
    };
    expect(placePopup(positioner)).toEqual(toRect(c.placed));
  });
});
