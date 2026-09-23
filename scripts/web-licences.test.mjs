/*
 * Tests for the npm licence gate (web-licences.mjs). Plain node:test, no dependency:
 *
 *   docker compose run --rm dev node --test scripts/web-licences.test.mjs
 *
 * `just web-licences` runs them before the gate itself.
 */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import {
  OWN_LICENCE,
  TIER_A,
  ownViolations,
  parseReport,
  parseSpdx,
  satisfies,
  violations,
} from './web-licences.mjs';

test('Tier A equals the allow list in deny.toml, so both graphs obey one policy', () => {
  const deny = readFileSync(new URL('../deny.toml', import.meta.url), 'utf8');
  const block = /^\[licenses\][\s\S]*?^allow = \[([\s\S]*?)^\]/m.exec(deny);
  assert.ok(block, 'deny.toml has a [licenses] allow list');
  const allow = block[1]
    .split('\n')
    .map((line) => line.replace(/#.*/, '').trim())
    .filter(Boolean)
    .map((line) => /^"([^"]+)",?$/.exec(line)?.[1]);
  assert.deepEqual(allow, [...TIER_A]);
});

test('every Tier A licence passes on its own', () => {
  for (const licence of TIER_A) {
    assert.equal(satisfies(licence), true, licence);
  }
});

test('an OR passes when one side is Tier A', () => {
  assert.equal(satisfies('MIT OR Apache-2.0'), true);
  assert.equal(satisfies('(MIT OR Apache-2.0)'), true);
  assert.equal(satisfies('GPL-3.0-only OR MIT'), true);
  assert.equal(satisfies('MPL-2.0 OR LGPL-2.1-only'), false);
});

test('an AND needs every side to be Tier A', () => {
  assert.equal(satisfies('(MIT OR Apache-2.0) AND Unicode-3.0'), true);
  assert.equal(satisfies('MIT AND MPL-2.0'), false);
  assert.equal(satisfies('(MIT OR Apache-2.0) AND BlueOak-1.0.0'), false);
});

test('AND binds tighter than OR', () => {
  // MIT OR (MPL-2.0 AND GPL-3.0-only): the MIT side alone satisfies it.
  assert.equal(satisfies('MIT OR MPL-2.0 AND GPL-3.0-only'), true);
  // (MIT OR MPL-2.0) AND GPL-3.0-only: the GPL side can never be avoided.
  assert.equal(satisfies('(MIT OR MPL-2.0) AND GPL-3.0-only'), false);
  assert.deepEqual(parseSpdx('A OR B AND C'), {
    op: 'OR',
    left: { id: 'A' },
    right: { op: 'AND', left: { id: 'B' }, right: { id: 'C' } },
  });
});

test('WITH passes only as the exact pair on the list', () => {
  assert.equal(satisfies('Apache-2.0 WITH LLVM-exception'), true);
  assert.equal(satisfies('MIT OR Apache-2.0 WITH LLVM-exception'), true);
  assert.equal(satisfies('Apache-2.0 WITH Classpath-exception-2.0'), false);
  assert.equal(satisfies('GPL-2.0-only WITH LLVM-exception'), false);
});

test('licences outside Tier A fail, the dev graph ones included', () => {
  for (const licence of [
    'MPL-2.0',
    'GPL-3.0-only',
    'LGPL-2.1-or-later',
    'AGPL-3.0-only',
    'EPL-2.0',
    'BlueOak-1.0.0',
    'CC0-1.0',
    'CC-BY-4.0',
    'MIT-0',
    'Python-2.0',
    'Apache-2.0+',
    'LicenseRef-Proprietary',
  ]) {
    assert.equal(satisfies(licence), false, licence);
  }
});

test('an unknown, missing or unparseable licence fails', () => {
  for (const licence of [
    'Unknown',
    'UNLICENSED',
    'SEE LICENSE IN LICENSE.md',
    'MIT/Apache-2.0',
    'MIT or Apache-2.0',
    '',
    '   ',
    '(MIT',
    'MIT)',
    'MIT OR',
    'AND MIT',
    'MIT Apache-2.0',
    'Apache-2.0 WITH',
    '(MIT) WITH LLVM-exception',
    undefined,
    null,
    { type: 'MIT' },
  ]) {
    assert.equal(satisfies(licence), false, JSON.stringify(licence));
  }
});

test('SPDX ids match case-insensitively, operators do not', () => {
  assert.equal(satisfies('mit'), true);
  assert.equal(satisfies('apache-2.0 WITH llvm-exception'), true);
  assert.equal(satisfies('GPL-3.0-only or MIT'), false);
});

test('violations reads a pnpm report and lists every package that fails', () => {
  const report = {
    MIT: [{ name: 'react', versions: ['19.3.0'], license: 'MIT' }],
    'MPL-2.0': [{ name: 'lightningcss', versions: ['1.30.0'], license: 'MPL-2.0' }],
    '(MIT OR Apache-2.0) AND Unicode-3.0': [
      { name: 'ident', versions: ['1.0.0'], license: '(MIT OR Apache-2.0) AND Unicode-3.0' },
    ],
    Unknown: [{ name: 'mystery', versions: ['0.1.0'], license: 'Unknown' }],
    // A package without a per-package field falls back to its group's key.
    ISC: [{ name: 'isexe', versions: ['2.0.0'] }],
  };
  assert.deepEqual(violations(report), [
    { name: 'lightningcss', versions: ['1.30.0'], license: 'MPL-2.0' },
    { name: 'mystery', versions: ['0.1.0'], license: 'Unknown' },
  ]);
  assert.deepEqual(violations({ MIT: report.MIT }), []);
});

test('parseReport reads JSON and the sentence pnpm prints for an empty graph', () => {
  assert.deepEqual(parseReport('No licenses in packages found\n'), {});
  assert.deepEqual(parseReport('{"MIT":[]}'), { MIT: [] });
  assert.throws(() => parseReport('ERR_PNPM_SOMETHING'));
});

test('every workspace package must declare exactly MIT OR Apache-2.0', () => {
  assert.equal(OWN_LICENCE, 'MIT OR Apache-2.0');
  assert.deepEqual(
    ownViolations([
      { name: '@appricot/client', license: 'MIT OR Apache-2.0' },
      { name: '@appricot/new', license: 'MIT' },
      { name: '@appricot/bare' },
    ]),
    [
      { name: '@appricot/new', license: 'MIT' },
      { name: '@appricot/bare', license: '(none)' },
    ],
  );
});
