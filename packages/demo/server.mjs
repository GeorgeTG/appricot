/*
 * The demo server's entry (the justfile `demo` recipe runs `node packages/demo/server.mjs`).
 *
 * Everything lives in the typed, unit-tested src/server.ts, compiled to dist/server.js by the
 * package's build. This shim only reads its environment and starts it: plain node, zero
 * dependencies, ESM, on 0.0.0.0:8390 — the container publishes that one port, on the host's
 * loopback only (compose.yml port scheme), and the streamer stays loopback-only at
 * 127.0.0.1:8391 behind the proxy.
 *
 * Plain JavaScript, no TypeScript syntax: node loads this file as-is (the shared ESLint
 * config parses .mjs with the TypeScript parser, which would accept annotations node then
 * rejects — measured 2026-09-21).
 *
 * `process`/`console` are declared below because the shared ESLint config keeps `no-undef`
 * on for .mjs files (TypeScript turns it off only for .ts), and this file runs in Node.
 */
/* global console, process */
import { startDemoServer } from './dist/server.js';

const DEFAULT_PORT = 8390;
const portArg = Number(process.env['APPRICOT_DEMO_PORT'] ?? DEFAULT_PORT);
const port = Number.isInteger(portArg) && portArg >= 0 && portArg <= 65535 ? portArg : DEFAULT_PORT;

try {
  const running = await startDemoServer({ port, host: '0.0.0.0' });
  console.log(
    `appricot demo: http://127.0.0.1:${running.port}/ ` +
      '(token from the page input field; streamer proxied on 127.0.0.1:8391)',
  );
  const shutdown = () => {
    running.close().finally(() => process.exit(0));
  };
  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);
} catch (err) {
  console.error('appricot demo failed to start:', err instanceof Error ? err.message : err);
  process.exitCode = 1;
}
