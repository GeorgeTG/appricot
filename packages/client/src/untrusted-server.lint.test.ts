import { fileURLToPath } from 'node:url';

import { ESLint, type Linter } from 'eslint';
import { beforeAll, describe, expect, it } from 'vitest';

/**
 * The untrusted-server rule (docs/adr/0003-untrusted-server-client.md) is enforced by the
 * repository's eslint.config.js. This file pins that rule set and proves it in three steps:
 *
 * 1. PINNED. The config holds exactly the entries listed below, in every file type it lints.
 *    Dropping an entry, or narrowing one (a name taken out of a selector's pattern, say), makes
 *    the config differ from the pin, and the test goes red. A new entry needs a pin and probes.
 * 2. EACH ENTRY FIRES ALONE. Every entry is linted on its own against its probes, so a probe
 *    cannot pass because some other, overlapping entry reports it. A selector whose pattern lists
 *    names has one probe per name, checked against the pattern itself.
 * 3. THE REAL CONFIG. Every probe is reported by the full config under its entry's rule, and the
 *    safe spellings are reported by none of the untrusted-server rules.
 */

const repoRoot = fileURLToPath(new URL('../../../', import.meta.url));
const eslint = new ESLint({ cwd: repoRoot });

const CLIENT_FILE = 'packages/client/src/lint-probe.ts';
const REACT_FILE = 'packages/react/src/lint-probe.tsx';
const NODE_FILE = 'packages/demo/lint-probe.mjs';

const UNTRUSTED_RULES = [
  'no-eval',
  'no-implied-eval',
  'no-new-func',
  'no-script-url',
  'no-restricted-globals',
  'no-restricted-properties',
  'no-restricted-syntax',
] as const;
type UntrustedRule = (typeof UNTRUSTED_RULES)[number];

interface Probe {
  readonly file: string;
  readonly code: string;
}

const ts = (code: string): Probe => ({ file: CLIENT_FILE, code });
const tsx = (code: string): Probe => ({ file: REACT_FILE, code });

/** One pinned entry: its rule, the key that names it in the config, and its probes. */
interface Pin {
  readonly rule: UntrustedRule;
  /**
   * `object.property` (`*.property` for any object), a selector or a global name; empty for a
   * rule with no options.
   */
  readonly key: string;
  readonly probes: readonly Probe[];
}

const URL_ATTRIBUTE = '/^(on.*|href|src|srcdoc|action|formaction|style|xlink:href)$/i';
const URL_PROPERTY = '/^(href|src|srcset|action|formAction|style|cssText)$/';

/** One sample name per alternative of URL_ATTRIBUTE, cased to prove the `i` flag. */
const URL_ATTRIBUTE_SAMPLES = [
  'onclick',
  'OnError',
  'href',
  'src',
  'srcdoc',
  'action',
  'formaction',
  'style',
  'xlink:href',
];
/** One sample name per alternative of URL_PROPERTY. */
const URL_PROPERTY_SAMPLES = ['href', 'src', 'srcset', 'action', 'formAction', 'style', 'cssText'];

const pinsOf =
  (rule: UntrustedRule) =>
  (key: string, ...probes: Probe[]): Pin => ({ rule, key, probes });
const plain = (rule: UntrustedRule, ...probes: Probe[]): Pin => pinsOf(rule)('', ...probes);
const prop = pinsOf('no-restricted-properties');
const globalName = pinsOf('no-restricted-globals');
const syntax = pinsOf('no-restricted-syntax');

const PINS: readonly Pin[] = [
  plain('no-eval', ts('eval(s);')),
  plain('no-implied-eval', ts("setTimeout('run()', 0);"), ts("setInterval('run()', 10);")),
  plain('no-new-func', ts('new Function(s);')),
  plain('no-script-url', ts("a.href = 'javascript:void 0';")),

  globalName('location', ts('location.reload();')),
  globalName('history', ts('history.back();')),
  globalName('navigation', ts('navigation.navigate(s);')),
  globalName('open', ts('open(s);')),
  globalName('opener', ts("opener.postMessage(s, '*');")),
  globalName('top', ts("top.postMessage(s, '*');")),
  globalName('parent', ts("parent.postMessage(s, '*');")),
  globalName('frames', ts("frames[0].postMessage(s, '*');")),

  prop('*.innerHTML', ts('el.innerHTML = s;'), ts("el['innerHTML'] = s;")),
  prop('*.outerHTML', ts('el.outerHTML = s;')),
  prop('*.insertAdjacentHTML', ts("el.insertAdjacentHTML('beforeend', s);")),
  prop('*.setHTMLUnsafe', ts('el.setHTMLUnsafe(s);')),
  prop('*.createContextualFragment', ts('range.createContextualFragment(s);')),
  prop('*.srcdoc', ts('frame.srcdoc = s;')),
  prop('document.write', ts('document.write(s);')),
  prop('document.writeln', ts('document.writeln(s);')),
  prop('document.open', ts('document.open();')),
  prop('document.location', ts('document.location = s;')),
  prop('window.location', ts('window.location.assign(s);')),
  prop('window.open', ts('window.open(s);')),
  prop('window.history', ts('window.history.back();')),
  prop('window.navigation', ts('window.navigation.navigate(s);')),
  prop('globalThis.open', ts('globalThis.open(s);')),
  prop('globalThis.history', ts('globalThis.history.back();')),
  prop('globalThis.navigation', ts('globalThis.navigation.navigate(s);')),
  prop('self.open', ts('self.open(s);')),
  prop('self.history', ts('self.history.back();')),
  prop('self.navigation', ts('self.navigation.navigate(s);')),
  prop('location.href', ts('location.href = s;')),
  prop('location.assign', ts('location.assign(s);')),
  prop('location.replace', ts('location.replace(s);')),
  prop('history.pushState', ts("history.pushState(null, '', s);")),
  prop('history.replaceState', ts("history.replaceState(null, '', s);")),

  syntax(
    "JSXAttribute[name.name='dangerouslySetInnerHTML']",
    tsx('export const x = <div dangerouslySetInnerHTML={{ __html: s }} />;'),
  ),
  syntax(
    "Property[key.name='dangerouslySetInnerHTML']",
    tsx("createElement('div', { dangerouslySetInnerHTML: { __html: s } });"),
  ),
  syntax(
    'JSXAttribute[name.name=/^srcdoc$/i]',
    tsx('export const x = <iframe srcDoc={s} />;'),
    tsx('export const x = <iframe srcdoc={s} />;'),
  ),
  syntax(
    `CallExpression[callee.property.name='setAttribute'][arguments.0.value=${URL_ATTRIBUTE}]`,
    ...URL_ATTRIBUTE_SAMPLES.map((name) => ts(`el.setAttribute('${name}', s);`)),
  ),
  syntax(
    "CallExpression[callee.property.name='setAttribute'][arguments.0.type!='Literal']",
    ts('el.setAttribute(name, s);'),
  ),
  syntax(
    `CallExpression[callee.property.name='setAttributeNS'][arguments.1.value=${URL_ATTRIBUTE}]`,
    ...URL_ATTRIBUTE_SAMPLES.map((name) => ts(`el.setAttributeNS(ns, '${name}', s);`)),
  ),
  syntax(
    "CallExpression[callee.property.name='setAttributeNS'][arguments.1.type!='Literal']",
    ts('el.setAttributeNS(ns, name, s);'),
  ),
  syntax(
    `AssignmentExpression[left.property.name=${URL_PROPERTY}]`,
    ...URL_PROPERTY_SAMPLES.map((name) => ts(`el.${name} = s;`)),
  ),
  syntax(
    `AssignmentExpression[left.property.value=${URL_PROPERTY}]`,
    ...URL_PROPERTY_SAMPLES.map((name) => ts(`el['${name}'] = s;`)),
  ),
  syntax(
    "MemberExpression[property.name='location']",
    ts('globalThis.location.href = s;'),
    ts('self.location.href = s;'),
    ts('win.location.reload();'),
  ),
  syntax("MemberExpression[property.value='location']", ts("win['location'].reload();")),
  syntax("ImportExpression[source.type!='Literal']", ts('void import(s);')),
  syntax(
    "NewExpression[callee.name=/^(Shared)?Worker$/]:not([arguments.0.type='NewExpression'][arguments.0.callee.name='URL'][arguments.0.arguments.0.type='Literal'])",
    ts('new Worker(s);'),
    ts('new SharedWorker(s);'),
    ts('new Worker(new URL(s, import.meta.url));'),
  ),
];

const SAFE: readonly Probe[] = [
  ts('el.textContent = s;'),
  ts('setTextOnly(el, s);'),
  ts("el.setAttribute('aria-label', s);"),
  ts("el.setAttributeNS(null, 'aria-label', s);"),
  ts('img.alt = s;'),
  ts("el.style.width = '10px';"),
  ts('const parent = node.parentElement; parent?.focus({ preventScroll: true });'),
  ts("const url = new URL('/', document.baseURI);"),
  ts("void import('./lazy');"),
  ts("new Worker(new URL('./decode.worker.ts', import.meta.url), { type: 'module' });"),
  ts('setTimeout(() => run(), 0);'),
  tsx('export const x = <span>{s}</span>;'),
];

const abs = (file: string): string => `${repoRoot}${file}`;

/** The name of one configured entry, in the form a Pin's key uses. */
function keyOf(rule: UntrustedRule, entry: unknown): string {
  if (typeof entry === 'string') {
    return entry;
  }
  const e = entry as { object?: string; property?: string; selector?: string; name?: string };
  switch (rule) {
    case 'no-restricted-properties':
      return `${e.object ?? '*'}.${e.property ?? '*'}`;
    case 'no-restricted-syntax':
      return e.selector ?? '';
    case 'no-restricted-globals':
      return e.name ?? '';
    default:
      return '';
  }
}

/**
 * The config's entries for one rule, as [key, entry] pairs. A rule with no options yields one
 * empty key.
 */
function configuredEntries(config: Linter.Config, rule: UntrustedRule): [string, unknown][] {
  const setting = config.rules?.[rule];
  if (setting === undefined) {
    return [];
  }
  const [severity, ...options] = Array.isArray(setting) ? setting : [setting];
  if (severity !== 2 && severity !== 'error') {
    return [];
  }
  if (options.length === 0) {
    return [['', undefined]];
  }
  return options.map((entry) => [keyOf(rule, entry), entry]);
}

async function configFor(file: string): Promise<Linter.Config> {
  return (await eslint.calculateConfigForFile(abs(file))) as Linter.Config;
}

async function reportedRules(linter: ESLint, code: string, file: string): Promise<string[]> {
  const [result] = await linter.lintText(code, { filePath: abs(file) });
  if (result === undefined) {
    throw new Error(`ESLint returned no result for ${file}`);
  }
  const fatal = result.messages.find((message) => message.fatal === true);
  if (fatal !== undefined) {
    throw new Error(`ESLint could not parse the probe: ${fatal.message}`);
  }
  return result.messages.map((message) => message.ruleId ?? 'no-rule-id');
}

/** An ESLint that runs one entry of one rule and nothing else, parsing as the repository does. */
async function linterForOneEntry(pin: Pin, file: string): Promise<ESLint> {
  const config = await configFor(file);
  const entry = configuredEntries(config, pin.rule).find(([key]) => key === pin.key);
  if (entry === undefined) {
    throw new Error(`eslint.config.js has no ${pin.rule} entry ${pin.key}`);
  }
  if (config.languageOptions === undefined) {
    throw new Error(`eslint.config.js sets no languageOptions for ${file}`);
  }
  const options = entry[1] === undefined ? ['error'] : ['error', entry[1]];
  return new ESLint({
    cwd: repoRoot,
    overrideConfigFile: true,
    overrideConfig: [
      {
        files: ['**/*.{js,mjs,cjs,ts,mts,cts,tsx}'],
        languageOptions: config.languageOptions,
        rules: { [pin.rule]: options } as Linter.RulesRecord,
      },
    ],
  });
}

/** The names a selector's `/^(a|b|...)$/` pattern lists. */
function patternAlternatives(pattern: string): string[] {
  const match = /^\/\^\((.*)\)\$\/[a-z]*$/.exec(pattern);
  if (match?.[1] === undefined) {
    throw new Error(`not a /^(...)$/ pattern: ${pattern}`);
  }
  return match[1].split('|');
}

function flagsOf(pattern: string): string {
  return pattern.slice(pattern.lastIndexOf('/') + 1);
}

const probeCases = PINS.flatMap((entry) =>
  entry.probes.map((probe) => ({
    ...entry,
    probe,
    name: `${entry.rule} ${entry.key || '(no options)'}: ${probe.code}`,
  })),
);

describe('the untrusted-server lint rules', () => {
  beforeAll(async () => {
    // The first lint loads the config and the TypeScript parser, which takes seconds.
    await eslint.lintText('', { filePath: abs(CLIENT_FILE) });
  }, 60_000);

  describe('1. the config holds exactly the pinned entries', () => {
    it.each([CLIENT_FILE, REACT_FILE, NODE_FILE])('in %s', async (file) => {
      const config = await configFor(file);
      for (const rule of UNTRUSTED_RULES) {
        const configured = configuredEntries(config, rule)
          .map(([key]) => key)
          .sort();
        const pinned = PINS.filter((pin) => pin.rule === rule)
          .map((pin) => pin.key)
          .sort();
        expect(configured, rule).toEqual(pinned);
      }
    });

    it('every pin has a probe', () => {
      for (const pin of PINS) {
        expect(pin.probes.length, `${pin.rule} ${pin.key}`).toBeGreaterThan(0);
      }
    });

    it.each([
      [URL_ATTRIBUTE, URL_ATTRIBUTE_SAMPLES],
      [URL_PROPERTY, URL_PROPERTY_SAMPLES],
    ])('every name %s lists has a probe', (pattern, samples) => {
      const flags = flagsOf(pattern);
      for (const alternative of patternAlternatives(pattern)) {
        const re = new RegExp(`^(?:${alternative})$`, flags);
        expect(
          samples.some((sample) => re.test(sample)),
          alternative,
        ).toBe(true);
      }
    });
  });

  describe('2. each entry, alone, reports its probes', () => {
    it.each(probeCases)('$name', async (probe) => {
      const linter = await linterForOneEntry(probe, probe.probe.file);
      const rules = await reportedRules(linter, probe.probe.code, probe.probe.file);
      expect(rules).toContain(probe.rule);
    });
  });

  describe('3. the repository config', () => {
    it.each(probeCases)('reports $name', async (probe) => {
      expect(await reportedRules(eslint, probe.probe.code, probe.probe.file)).toContain(probe.rule);
    });

    it.each(SAFE)('allows $code', async ({ code, file }) => {
      const untrusted: readonly string[] = UNTRUSTED_RULES;
      const rules = await reportedRules(eslint, code, file);
      expect(rules.filter((rule) => untrusted.includes(rule))).toEqual([]);
    });
  });
});
