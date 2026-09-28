// The node environment: this file loads the package's built dist in a node process of its own.
import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

/**
 * The built package loads under Node's own ESM loader, with no bundler in between. Node
 * resolves a relative specifier exactly as written, so every `./x` in dist must be `./x.js`;
 * tsc under `NodeNext` enforces that in the sources (tsconfig.json), and this test proves it
 * on the output. It reads `dist/`, so it runs after `pnpm build`, as `just web-check` does.
 */
const dist = new URL('../dist/', import.meta.url);
const entry = new URL('index.js', dist);

describe('the built client', () => {
  it('exists: run `pnpm --filter @app-ricot/client build` first', () => {
    expect(existsSync(entry)).toBe(true);
  });

  it("loads under Node's ESM loader and exports the SDK", () => {
    const script =
      `const m = await import(${JSON.stringify(entry.href)});` +
      "if (typeof m.attachInput !== 'function' || typeof m.SurfaceRegistry !== 'function') " +
      "throw new Error('the entry point lacks its exports');";
    const run = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
      encoding: 'utf8',
      timeout: 30_000,
    });
    expect(run.stderr).toBe('');
    expect(run.status).toBe(0);
  });
});
