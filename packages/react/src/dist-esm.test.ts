// The node environment: this file loads the package's built dist in a node process of its own.
import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

/**
 * The built bindings load under Node's own ESM loader, with no bundler in between: every
 * relative specifier in dist names its `.js` file (tsc under `NodeNext` enforces that in the
 * sources), and `react` and `@appricot/client` resolve from the workspace install. It reads
 * `dist/`, so it runs after `pnpm build`, as `just web-check` does.
 */
const dist = new URL('../dist/', import.meta.url);
const entry = new URL('index.js', dist);

describe('the built bindings', () => {
  it('exist: run `pnpm --filter @appricot/react build` first', () => {
    expect(existsSync(entry)).toBe(true);
  });

  it("load under Node's ESM loader and export the provider and the surface", () => {
    const script =
      `const m = await import(${JSON.stringify(entry.href)});` +
      "if (typeof m.AppricotProvider !== 'function' || typeof m.AppricotSurface !== 'function') " +
      "throw new Error('the entry point lacks its exports');";
    const run = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
      encoding: 'utf8',
      timeout: 30_000,
    });
    expect(run.stderr).toBe('');
    expect(run.status).toBe(0);
  });
});
