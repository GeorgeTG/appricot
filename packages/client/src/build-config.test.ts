// The node environment: this file reads the package's own build config and sources.
import { readFileSync, readdirSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

/**
 * The hostile-server fixtures (src/hostile/) are test code: payload builders for markup,
 * oversized lengths and hostile popups. They must never be emitted into dist/, where they
 * would ship with the package and confuse a security review of it.
 */
const packageRoot = new URL('../', import.meta.url);

describe('the client build', () => {
  it('excludes the hostile fixtures from dist', () => {
    const config = JSON.parse(
      readFileSync(new URL('tsconfig.build.json', packageRoot), 'utf8'),
    ) as { exclude?: string[] };
    expect(config.exclude).toContain('src/hostile/**');
  });

  it('has no shipped module that imports the fixtures back into the build', () => {
    const sources = readdirSync(new URL('src/', packageRoot)).filter(
      (name) => name.endsWith('.ts') && !name.endsWith('.test.ts'),
    );
    expect(sources.length).toBeGreaterThan(0);
    for (const name of sources) {
      const text = readFileSync(new URL(`src/${name}`, packageRoot), 'utf8');
      expect(text, name).not.toMatch(/from '\.\/hostile/);
    }
  });
});
