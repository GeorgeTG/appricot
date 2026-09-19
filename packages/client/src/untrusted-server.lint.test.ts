import { fileURLToPath } from 'node:url';

import { ESLint } from 'eslint';
import { beforeAll, describe, expect, it } from 'vitest';

/**
 * The untrusted-server rule (docs/adr/0003-untrusted-server-client.md) is enforced by the
 * repository's eslint.config.js. These tests lint small probes with that config and prove
 * each forbidden sink is reported, so a rule cannot be dropped or narrowed without a red test.
 * They also prove the safe spellings stay allowed.
 */

const repoRoot = fileURLToPath(new URL('../../../', import.meta.url));
const eslint = new ESLint({ cwd: repoRoot });

const CLIENT_FILE = 'packages/client/src/lint-probe.ts';
const REACT_FILE = 'packages/react/src/lint-probe.tsx';

const UNTRUSTED_RULES = [
  'no-eval',
  'no-implied-eval',
  'no-new-func',
  'no-script-url',
  'no-restricted-properties',
  'no-restricted-syntax',
];

async function reportedRules(code: string, file: string): Promise<string[]> {
  const [result] = await eslint.lintText(code, { filePath: `${repoRoot}${file}` });
  if (result === undefined) {
    throw new Error(`ESLint returned no result for ${file}`);
  }
  const fatal = result.messages.find((message) => message.fatal === true);
  if (fatal !== undefined) {
    throw new Error(`ESLint could not parse the probe: ${fatal.message}`);
  }
  return result.messages.map((message) => message.ruleId ?? 'no-rule-id');
}

interface Probe {
  readonly name: string;
  readonly file: string;
  readonly code: string;
}

interface SinkProbe extends Probe {
  readonly rule: string;
}

const sinks: readonly SinkProbe[] = [
  { name: 'innerHTML', file: CLIENT_FILE, code: 'el.innerHTML = s;', rule: 'no-restricted-properties' },
  { name: 'computed innerHTML', file: CLIENT_FILE, code: "el['innerHTML'] = s;", rule: 'no-restricted-properties' },
  { name: 'outerHTML', file: CLIENT_FILE, code: 'el.outerHTML = s;', rule: 'no-restricted-properties' },
  {
    name: 'insertAdjacentHTML',
    file: CLIENT_FILE,
    code: "el.insertAdjacentHTML('beforeend', s);",
    rule: 'no-restricted-properties',
  },
  { name: 'setHTMLUnsafe', file: CLIENT_FILE, code: 'el.setHTMLUnsafe(s);', rule: 'no-restricted-properties' },
  {
    name: 'createContextualFragment',
    file: CLIENT_FILE,
    code: 'range.createContextualFragment(s);',
    rule: 'no-restricted-properties',
  },
  { name: 'srcdoc', file: CLIENT_FILE, code: 'frame.srcdoc = s;', rule: 'no-restricted-properties' },
  { name: 'document.write', file: CLIENT_FILE, code: 'document.write(s);', rule: 'no-restricted-properties' },
  { name: 'document.writeln', file: CLIENT_FILE, code: 'document.writeln(s);', rule: 'no-restricted-properties' },
  { name: 'window.open', file: CLIENT_FILE, code: 'window.open(s);', rule: 'no-restricted-properties' },
  { name: 'window.location', file: CLIENT_FILE, code: 'window.location.assign(s);', rule: 'no-restricted-properties' },
  { name: 'location.href', file: CLIENT_FILE, code: 'location.href = s;', rule: 'no-restricted-properties' },
  { name: 'location.replace', file: CLIENT_FILE, code: 'location.replace(s);', rule: 'no-restricted-properties' },
  { name: 'history.pushState', file: CLIENT_FILE, code: "history.pushState(null, '', s);", rule: 'no-restricted-properties' },
  { name: 'eval', file: CLIENT_FILE, code: 'eval(s);', rule: 'no-eval' },
  { name: 'new Function', file: CLIENT_FILE, code: 'new Function(s);', rule: 'no-new-func' },
  { name: 'a string timer', file: CLIENT_FILE, code: "setTimeout('run()', 0);", rule: 'no-implied-eval' },
  { name: 'a javascript: URL', file: CLIENT_FILE, code: "a.href = 'javascript:void 0';", rule: 'no-script-url' },
  { name: 'a computed dynamic import', file: CLIENT_FILE, code: 'void import(s);', rule: 'no-restricted-syntax' },
  { name: 'a worker from a computed URL', file: CLIENT_FILE, code: 'new Worker(s);', rule: 'no-restricted-syntax' },
  {
    name: 'an event-handler attribute',
    file: CLIENT_FILE,
    code: "el.setAttribute('onclick', s);",
    rule: 'no-restricted-syntax',
  },
  { name: 'a URL attribute', file: CLIENT_FILE, code: "el.setAttribute('href', s);", rule: 'no-restricted-syntax' },
  { name: 'a computed attribute name', file: CLIENT_FILE, code: 'el.setAttribute(name, s);', rule: 'no-restricted-syntax' },
  {
    name: 'dangerouslySetInnerHTML in JSX',
    file: REACT_FILE,
    code: 'export const x = <div dangerouslySetInnerHTML={{ __html: s }} />;',
    rule: 'no-restricted-syntax',
  },
  {
    name: 'dangerouslySetInnerHTML in createElement',
    file: REACT_FILE,
    code: "createElement('div', { dangerouslySetInnerHTML: { __html: s } });",
    rule: 'no-restricted-syntax',
  },
  { name: 'srcDoc in JSX', file: REACT_FILE, code: 'export const x = <iframe srcDoc={s} />;', rule: 'no-restricted-syntax' },
];

const safe: readonly Probe[] = [
  { name: 'textContent', file: CLIENT_FILE, code: 'el.textContent = s;' },
  { name: 'setTextOnly', file: CLIENT_FILE, code: 'setTextOnly(el, s);' },
  { name: 'a plain attribute', file: CLIENT_FILE, code: "el.setAttribute('aria-label', s);" },
  { name: 'a literal dynamic import', file: CLIENT_FILE, code: "void import('./lazy');" },
  {
    name: 'a worker from the bundle',
    file: CLIENT_FILE,
    code: "new Worker(new URL('./decode.worker.ts', import.meta.url), { type: 'module' });",
  },
  { name: 'a function timer', file: CLIENT_FILE, code: 'setTimeout(() => run(), 0);' },
  { name: 'a string as a React child', file: REACT_FILE, code: 'export const x = <span>{s}</span>;' },
];

describe('the untrusted-server lint rules', () => {
  beforeAll(async () => {
    // The first lint loads the config and the TypeScript parser, which takes seconds.
    await eslint.lintText('', { filePath: `${repoRoot}${CLIENT_FILE}` });
  }, 60_000);

  it.each(sinks)('report $name', async ({ code, file, rule }) => {
    expect(await reportedRules(code, file)).toContain(rule);
  });

  it.each(safe)('allow $name', async ({ code, file }) => {
    const rules = await reportedRules(code, file);
    expect(rules.filter((rule) => UNTRUSTED_RULES.includes(rule))).toEqual([]);
  });
});
