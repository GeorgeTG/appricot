import js from '@eslint/js';
import { defineConfig, globalIgnores } from 'eslint/config';
import tseslint from 'typescript-eslint';

/**
 * ESLint flat config for every package under packages/.
 *
 * Its main job is the untrusted-server rule (docs/adr/0003-untrusted-server-client.md): the
 * server is hostile, so the client never turns a server string into markup, script, a URL or
 * a navigation. The rules below forbid the sinks by name, in every package, and
 * packages/client/src/untrusted-server.lint.test.ts proves that each one fires. Weakening a
 * rule here turns that test red.
 *
 * Type-aware linting is not on: `pnpm typecheck` already runs the compiler, and these rules
 * are syntactic on purpose, so they also hold in files the compiler never sees.
 */

const SINK =
  'The server is untrusted (ADR-0003). Render server strings with setTextOnly() or as React ' +
  'children; never through an HTML sink.';
const NAVIGATION =
  'The client never navigates (ADR-0003 §2). The protocol has no message that carries a URL.';
const SCRIPT = 'No code from strings (ADR-0003 §2): the page must run without unsafe-eval.';

const untrustedServerRules = {
  'no-eval': 'error',
  'no-implied-eval': 'error',
  'no-new-func': 'error',
  'no-script-url': 'error',
  'no-restricted-properties': [
    'error',
    { property: 'innerHTML', message: SINK },
    { property: 'outerHTML', message: SINK },
    { property: 'insertAdjacentHTML', message: SINK },
    { property: 'setHTMLUnsafe', message: SINK },
    { property: 'createContextualFragment', message: SINK },
    { property: 'srcdoc', message: SINK },
    { object: 'document', property: 'write', message: SINK },
    { object: 'document', property: 'writeln', message: SINK },
    { object: 'document', property: 'location', message: NAVIGATION },
    { object: 'window', property: 'location', message: NAVIGATION },
    { object: 'window', property: 'open', message: NAVIGATION },
    { object: 'window', property: 'history', message: NAVIGATION },
    { object: 'location', property: 'href', message: NAVIGATION },
    { object: 'location', property: 'assign', message: NAVIGATION },
    { object: 'location', property: 'replace', message: NAVIGATION },
    { object: 'history', property: 'pushState', message: NAVIGATION },
    { object: 'history', property: 'replaceState', message: NAVIGATION },
  ],
  'no-restricted-syntax': [
    'error',
    { selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']", message: SINK },
    { selector: "Property[key.name='dangerouslySetInnerHTML']", message: SINK },
    { selector: 'JSXAttribute[name.name=/^srcdoc$/i]', message: SINK },
    {
      selector:
        "CallExpression[callee.property.name='setAttribute'][arguments.0.value=/^(on.*|href|src|srcdoc|action|formaction|style|xlink:href)$/i]",
      message: `${SINK} An event handler, URL or style attribute is a sink too.`,
    },
    {
      selector: "CallExpression[callee.property.name='setAttribute'][arguments.0.type!='Literal']",
      message: `${SINK} A computed attribute name may be a handler or a URL; name it literally.`,
    },
    {
      selector: "ImportExpression[source.type!='Literal']",
      message: `${SCRIPT} Import modules by a literal specifier only.`,
    },
    {
      selector:
        "NewExpression[callee.name=/^(Shared)?Worker$/]:not([arguments.0.type='NewExpression'][arguments.0.callee.name='URL'][arguments.0.arguments.0.type='Literal'])",
      message: `${SCRIPT} Load a worker with new URL('./file', import.meta.url) only.`,
    },
  ],
};

export default defineConfig([
  globalIgnores(['**/dist/', '**/coverage/', 'target/']),
  {
    name: 'appricot/typescript',
    files: ['**/*.{js,mjs,cjs,ts,mts,cts,tsx}'],
    extends: [js.configs.recommended, tseslint.configs.recommended],
  },
  {
    name: 'appricot/untrusted-server',
    files: ['**/*.{js,mjs,cjs,ts,mts,cts,tsx}'],
    // `no-implied-eval` only reports a call it can resolve to a GLOBAL timer: it looks the
    // name up in the global scope and reports nothing when the name is undeclared. With no
    // globals configured, `setTimeout('run()', 0)` was reported by nothing — measured
    // 2026-09-19, as a red case in untrusted-server.lint.test.ts. Declaring the three timer
    // names is enough for that rule; the TypeScript compiler, not ESLint, is what checks the
    // rest of the browser API (`no-undef` is off for TypeScript files).
    languageOptions: {
      globals: {
        setTimeout: 'readonly',
        setInterval: 'readonly',
        setImmediate: 'readonly',
      },
    },
    rules: untrustedServerRules,
  },
]);
