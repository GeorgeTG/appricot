#!/usr/bin/env node
/*
 * The npm half of the licence gate in docs/adr/0002-licence.md §2. deny.toml is the Rust half.
 * Run it inside the dev container, after `just web-install`:
 *
 *   docker compose run --rm dev just web-licences
 *
 * It fails when:
 *
 * - a package in the PRODUCTION graph of the npm workspace (`pnpm licenses list --prod`) has a
 *   licence expression that Tier A does not satisfy. That graph is what a host inherits when it
 *   bundles @app-ricot/client or @app-ricot/react.
 * - a package only in the DEVELOPMENT graph (the devDependencies) has a licence expression that
 *   the development list does not satisfy: Tier A plus DEV_EXTRA (ADR-0002, Amendment 1).
 * - a workspace package does not declare exactly "MIT OR Apache-2.0" (ADR-0002 §1). This is the
 *   npm twin of deny.toml's `[licenses.private] ignore = false`.
 *
 * Tier A is the list in ADR-0002 §2, and TIER_A below must equal deny.toml's `allow` list.
 * DEV_EXTRA must equal the list in the ADR's Amendment 1. web-licences.test.mjs fails when either
 * drifts. As in cargo-deny:
 *
 * - an expression passes when it is satisfiable with Tier A licences alone: an `OR` needs one
 *   side, an `AND` needs both, and `X WITH Y` needs that exact pair on the list;
 * - anything else fails, including an unknown, missing or unparseable licence.
 *
 * The development graph has its own list because it is never bundled or conveyed: the packages
 * are built with tsc alone, and the tools run in the dev container. Its extras are permissive,
 * plus MPL-2.0, whose duties attach to distribution. Everything else fails there too, GPL and
 * unknown licences included. The script prints the development packages outside Tier A, so what
 * the extras let in stays visible.
 *
 * Offline: pnpm reads the installed packages and makes no network request. No dependency: plain
 * node, the pnpm the image already carries, and nothing else.
 */
/* global console, process */
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

/** ADR-0002 §2, in deny.toml's order. */
export const TIER_A = Object.freeze([
  'MIT',
  'Apache-2.0',
  'Apache-2.0 WITH LLVM-exception',
  'BSD-2-Clause',
  'BSD-3-Clause',
  'ISC',
  'Unicode-3.0',
  'Zlib',
  '0BSD',
]);

/**
 * ADR-0002, Amendment 1: the licences the development graph may carry beyond Tier A. MPL-2.0 is
 * here only; the production graph still denies it.
 */
export const DEV_EXTRA = Object.freeze(['MIT-0', 'CC0-1.0', 'BlueOak-1.0.0', 'MPL-2.0']);

/** The development graph's list: Tier A plus DEV_EXTRA. */
export const DEV_ALLOWED = Object.freeze([...TIER_A, ...DEV_EXTRA]);

/** What every workspace package declares (ADR-0002 §1). */
export const OWN_LICENCE = 'MIT OR Apache-2.0';

const OPERATORS = new Set(['AND', 'OR', 'WITH']);
// SPDX license and exception ids: letters, digits, `.`, `-`, and a trailing `+` ("or later").
// `LicenseRef-…` fits the same shape. Anything else is unparseable.
const ID = /^[A-Za-z0-9.-]+\+?$/;

/**
 * Parses an SPDX licence expression into a tree of
 * `{ id, exception? } | { op: 'AND' | 'OR', left, right }`.
 * Operators are upper case, as SPDX requires; `WITH` binds tighter than `AND`, and `AND`
 * tighter than `OR`. Throws on anything it cannot parse.
 */
export function parseSpdx(expression) {
  if (typeof expression !== 'string') {
    throw new Error(`not a string: ${JSON.stringify(expression)}`);
  }
  const tokens = expression.replace(/[()]/g, ' $& ').trim().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) {
    throw new Error('empty expression');
  }
  let at = 0;
  const peek = () => tokens[at];
  const take = () => tokens[at++];

  const license = () => {
    const token = take();
    if (token === '(') {
      const inner = either();
      if (take() !== ')') {
        throw new Error(`missing ")" in ${JSON.stringify(expression)}`);
      }
      return inner;
    }
    if (token === undefined || token === ')' || OPERATORS.has(token) || !ID.test(token)) {
      throw new Error(`unexpected ${JSON.stringify(token)} in ${JSON.stringify(expression)}`);
    }
    if (peek() !== 'WITH') {
      return { id: token };
    }
    take();
    const exception = take();
    if (exception === undefined || OPERATORS.has(exception) || !ID.test(exception)) {
      throw new Error(`bad exception after WITH in ${JSON.stringify(expression)}`);
    }
    return { id: token, exception };
  };
  const both = () => {
    let left = license();
    while (peek() === 'AND') {
      take();
      left = { op: 'AND', left, right: license() };
    }
    return left;
  };
  const either = () => {
    let left = both();
    while (peek() === 'OR') {
      take();
      left = { op: 'OR', left, right: both() };
    }
    return left;
  };

  const tree = either();
  if (at !== tokens.length) {
    throw new Error(`unexpected ${JSON.stringify(peek())} in ${JSON.stringify(expression)}`);
  }
  return tree;
}

/**
 * Whether `expression` is satisfiable with licences from `allowed` alone. SPDX ids match case
 * insensitively (SPDX 2.3, Annex D); operators do not. An unparseable expression never passes.
 */
export function satisfies(expression, allowed = TIER_A) {
  const allow = new Set(allowed.map((entry) => entry.toLowerCase()));
  const passes = (node) => {
    if ('op' in node) {
      return node.op === 'AND'
        ? passes(node.left) && passes(node.right)
        : passes(node.left) || passes(node.right);
    }
    const name = node.exception === undefined ? node.id : `${node.id} WITH ${node.exception}`;
    return allow.has(name.toLowerCase());
  };
  try {
    return passes(parseSpdx(expression));
  } catch {
    return false;
  }
}

/**
 * The packages of a `pnpm licenses list --json` report whose licence `allowed` (Tier A unless
 * given) does not satisfy, as `{ name, versions, license }`, sorted by name.
 */
export function violations(report, allowed = TIER_A) {
  const found = [];
  for (const [group, packages] of Object.entries(report)) {
    for (const pkg of packages) {
      const license = typeof pkg.license === 'string' ? pkg.license : group;
      if (!satisfies(license, allowed)) {
        found.push({ name: pkg.name, versions: pkg.versions ?? [], license });
      }
    }
  }
  return found.sort((a, b) => a.name.localeCompare(b.name));
}

/**
 * Parses the stdout of `pnpm licenses list --json`. pnpm prints a plain sentence instead of
 * `{}` when the graph is empty.
 */
export function parseReport(stdout) {
  const text = stdout.trim();
  if (text === 'No licenses in packages found') {
    return {};
  }
  return JSON.parse(text);
}

/** The names of the packages in a `pnpm licenses list --json` report. */
export function packageNames(report) {
  return new Set(Object.values(report).flatMap((packages) => packages.map((pkg) => pkg.name)));
}

/** The workspace packages whose own `license` field is not exactly OWN_LICENCE. */
export function ownViolations(manifests) {
  return manifests
    .filter((manifest) => manifest.license !== OWN_LICENCE)
    .map((manifest) => ({ name: manifest.name, license: manifest.license ?? '(none)' }));
}

function pnpm(args) {
  return execFileSync('pnpm', args, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
}

const describe = (pkg) => `${pkg.name}@${pkg.versions.join(',')} (${pkg.license})`;

function main() {
  let failed = false;

  const projects = JSON.parse(pnpm(['ls', '--recursive', '--depth', '-1', '--json']));
  const manifests = projects.map((project) =>
    JSON.parse(readFileSync(join(project.path, 'package.json'), 'utf8')),
  );
  const own = ownViolations(manifests);
  for (const pkg of own) {
    console.error(`web-licences: ${pkg.name} declares ${pkg.license}, not "${OWN_LICENCE}"`);
    failed = true;
  }

  const prod = parseReport(pnpm(['licenses', 'list', '--prod', '--json']));
  const prodCount = Object.values(prod).reduce((sum, packages) => sum + packages.length, 0);
  const denied = violations(prod);
  for (const pkg of denied) {
    console.error(`web-licences: denied in the production graph: ${describe(pkg)}`);
    failed = true;
  }

  // The full report holds both graphs; a package in the production graph was judged above.
  const all = parseReport(pnpm(['licenses', 'list', '--json']));
  const prodNames = packageNames(prod);
  const devCount = [...packageNames(all)].filter((name) => !prodNames.has(name)).length;
  const devDenied = violations(all, DEV_ALLOWED).filter((pkg) => !prodNames.has(pkg.name));
  for (const pkg of devDenied) {
    console.error(`web-licences: denied in the development graph: ${describe(pkg)}`);
    failed = true;
  }
  const devExtra = violations(all)
    .filter((pkg) => !prodNames.has(pkg.name))
    .filter((pkg) => satisfies(pkg.license, DEV_ALLOWED));
  if (devExtra.length > 0) {
    console.log(
      `web-licences: note: ${devExtra.length} development packages are outside Tier A, on the ` +
        'development list (ADR-0002, Amendment 1):',
    );
    for (const pkg of devExtra) {
      console.log(`  ${describe(pkg)}`);
    }
  }

  if (failed) {
    console.error(
      'web-licences: FAILED. Tier A is ADR-0002 §2 and the development list its Amendment 1; a ' +
        'licence outside them needs an amendment to that ADR first.',
    );
    process.exit(1);
  }
  console.log(
    `web-licences: ok. ${manifests.length} workspace packages declare "${OWN_LICENCE}"; ` +
      `${prodCount} production packages, all Tier A; ${devCount} development packages, all on ` +
      'the development list.',
  );
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
