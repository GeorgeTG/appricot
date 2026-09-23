/**
 * Proves by source inspection that the demo never reads the token — or anything else the
 * server could influence — out of the page's URL or cookies. `src/token.ts` takes the form
 * field as its only input, and these greps keep the rest of the page from growing a second
 * path (a `location.search` read, a URLSearchParams, a `document.cookie`) that undoes it.
 *
 * The same grep stands guard over the stricter rule from eslint.config.js: no markup sink
 * may appear in demo sources at all (`innerHTML` and friends are forbidden there by lint;
 * this test is the demo's own tripwire, independent of the shared config).
 */
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const HERE = dirname(fileURLToPath(import.meta.url));

/** Every source the page ships or serves as its own: src/** (tests excluded), index.html,
 * server.mjs. The tests themselves are not shipped and do name the banned strings. */
function pageSources(): string[] {
  const files: string[] = [join(HERE, '..', 'index.html'), join(HERE, '..', 'server.mjs')];
  const walk = (dir: string): void => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(path);
      } else if (/\.test\.(ts|tsx)$/.test(entry.name)) {
        continue;
      } else if (/\.(ts|tsx|html|mjs)$/.test(entry.name)) {
        files.push(path);
      }
    }
  };
  walk(HERE);
  return files;
}

describe('the demo page never consults its URL or cookies for the token', () => {
  it('no source reads the query string, the hash, the location, or cookies', () => {
    const banned = [
      'location.search',
      'location.hash',
      'location.href',
      'URLSearchParams',
      'searchParams',
      'document.cookie',
      'getParameter',
    ];
    const offenders: string[] = [];
    for (const file of pageSources()) {
      const source = readFileSync(file, 'utf8');
      for (const needle of banned) {
        if (source.includes(needle)) {
          offenders.push(`${file}: contains ${needle}`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it('no source uses a markup sink (setTextOnly/textContent are the only writers)', () => {
    const banned = ['innerHTML', 'outerHTML', 'insertAdjacentHTML', 'document.write', 'srcdoc'];
    const offenders: string[] = [];
    for (const file of pageSources()) {
      const source = readFileSync(file, 'utf8');
      for (const needle of banned) {
        if (source.includes(needle)) {
          offenders.push(`${file}: contains ${needle}`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it('the page has no inline handlers or inline styles (the CSP has no unsafe-inline)', () => {
    const html = readFileSync(join(HERE, '..', 'index.html'), 'utf8');
    // An inline handler is an attribute: whitespace, 'on…', '='. Anchoring on the leading
    // whitespace keeps ordinary attributes ('content=') out of the match.
    expect(/\son[a-z]+\s*=/i.test(html)).toBe(false);
    expect(/\sstyle\s*=/i.test(html)).toBe(false);
    // The only script tag allowed is the external module — not even an import map:
    // Chromium applies script-src to inline import maps (measured 2026-09-21), and this
    // page keeps its CSP at plain 'self' with nothing inline at all.
    expect(/<script(?![^>]*type=["']module["'])[^>]*>/i.test(html)).toBe(false);
  });
});
