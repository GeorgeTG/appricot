/**
 * Unit tests for the demo server core (src/server.ts): the path mapping table, the TS-ESM
 * extension probe, traversal rejection, the strict CSP on every response type, and the
 * /readyz + /session proxy against a mock streamer (a plain node:http upstream on an
 * ephemeral port, WebSocket upgrade included).
 */
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import type { Server } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { connect as connectSocket } from 'node:net';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { CSP, resolveStatic, startDemoServer } from './server';
import type { RunningDemoServer, StaticRoots } from './server';

// --- fixtures ---------------------------------------------------------------------------------

let roots: StaticRoots;
let demo: RunningDemoServer;
let upstream: Server;
let upstreamPort = 0;
/** The paths the mock streamer saw, so the proxy tests can prove the forwarding. */
const upstreamPaths: string[] = [];
/**
 * The mock's upgraded sockets. Tracked because node's closeAllConnections() was measured
 * (2026-09-21) to leave upgraded connections alive on BOTH proxy sides, which would hang
 * upstream.close() in the teardown below.
 */
const upstreamUpgraded = new Set<import('node:stream').Duplex>();

beforeAll(async () => {
  const base = mkdtempSync(join(tmpdir(), 'appricot-demo-test-'));
  roots = {
    demoRoot: join(base, 'demo'),
    demoDist: join(base, 'demo', 'dist'),
    clientDist: join(base, 'client'),
    reactDist: join(base, 'react'),
  };
  mkdirSync(roots.demoDist, { recursive: true });
  mkdirSync(roots.clientDist, { recursive: true });
  mkdirSync(roots.reactDist, { recursive: true });
  writeFileSync(join(roots.demoRoot, 'index.html'), '<!doctype html><title>fixture page</title>');
  writeFileSync(join(roots.demoRoot, 'styles.css'), 'body { margin: 0 }');
  writeFileSync(join(roots.demoDist, 'main.js'), "import { CLIENT } from '@appricot/client';\nexport const MAIN = 1;\n");
  mkdirSync(join(roots.demoDist, 'wm'), { recursive: true });
  writeFileSync(join(roots.demoDist, 'wm', 'windows.js'), 'export const WINDOWS = 2;\n');
  writeFileSync(join(roots.clientDist, 'index.js'), 'export const CLIENT = 3;\n');
  writeFileSync(join(roots.reactDist, 'index.js'), 'export const REACT = 4;\n');

  // The mock streamer: readiness says ready, everything else is refused, and the WebSocket
  // upgrade succeeds and echoes raw bytes.
  upstream = createServer((req, res) => {
    upstreamPaths.push(req.url ?? '');
    if ((req.url ?? '').startsWith('/readyz')) {
      res.writeHead(200, { 'content-type': 'text/plain' });
      res.end('ready');
      return;
    }
    res.writeHead(503, { 'content-type': 'text/plain' });
    res.end('no');
  });
  upstream.on('upgrade', (req, socket) => {
    upstreamPaths.push(req.url ?? '');
    upstreamUpgraded.add(socket);
    socket.on('close', () => {
      upstreamUpgraded.delete(socket);
    });
    socket.write(
      'HTTP/1.1 101 Switching Protocols\r\n' +
        'Upgrade: websocket\r\n' +
        'Connection: Upgrade\r\n' +
        'Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n' +
        '\r\n',
    );
    socket.on('data', (chunk) => {
      socket.write(chunk); // echo: proves the pipe is live in both directions
    });
  });
  await new Promise<void>((resolve) => {
    upstream.listen(0, '127.0.0.1', () => resolve());
  });
  upstreamPort = (upstream.address() as { port: number }).port;

  demo = await startDemoServer({
    port: 0,
    host: '127.0.0.1',
    roots,
    upstream: { host: '127.0.0.1', port: upstreamPort },
  });
});

afterAll(async () => {
  await demo.close();
  await new Promise<void>((resolve) => {
    for (const socket of [...upstreamUpgraded]) {
      socket.destroy();
    }
    upstreamUpgraded.clear();
    upstream.closeAllConnections();
    upstream.close(() => resolve());
  });
});

function url(path: string): string {
  return `http://127.0.0.1:${demo.port}${path}`;
}

// --- the path mapping table --------------------------------------------------------------------

describe('the static path mapping table', () => {
  it('serves / as the demo index.html', async () => {
    const res = await fetch(url('/'));
    expect(res.status).toBe(200);
    expect(res.headers.get('content-type')).toBe('text/html; charset=utf-8');
    expect(await res.text()).toContain('fixture page');
  });

  it('serves /styles.css from the demo root', async () => {
    const res = await fetch(url('/styles.css'));
    expect(res.status).toBe(200);
    expect(res.headers.get('content-type')).toBe('text/css; charset=utf-8');
    expect(await res.text()).toContain('margin: 0');
  });

  it('maps /dist, /client and /react to their trees', async () => {
    expect(await (await fetch(url('/dist/main.js'))).text()).toContain('MAIN');
    expect(await (await fetch(url('/client/index.js'))).text()).toContain('CLIENT');
    expect(await (await fetch(url('/react/index.js'))).text()).toContain('REACT');
  });

  it('serves the vendor table, which is empty by measurement (react-dom is CJS-only)', async () => {
    const res = await fetch(url('/vendor/react.js'));
    expect(res.status).toBe(404);
  });

  it('answers 404 for an unmapped path and for a mapped-but-missing file', async () => {
    expect((await fetch(url('/nope'))).status).toBe(404);
    expect((await fetch(url('/dist/missing.js'))).status).toBe(404);
  });

  it('probes the .js extension for extensionless TS-ESM imports (the browser cannot)', async () => {
    const res = await fetch(url('/dist/main'));
    expect(res.status).toBe(200);
    expect(res.headers.get('content-type')).toBe('text/javascript; charset=utf-8');
    expect(await res.text()).toContain('MAIN');
  });

  it('probes nested extensionless paths too', async () => {
    const res = await fetch(url('/dist/wm/windows'));
    expect(res.status).toBe(200);
    expect(await res.text()).toContain('WINDOWS');
  });

  it("rewrites the page modules' bare client specifier to the served URL", async () => {
    // No import map exists (Chromium blocks inline maps under a plain 'self' CSP — measured
    // 2026-09-21), so /dist/*.js is served with '@appricot/client' spelled as /client/index.js.
    const res = await fetch(url('/dist/main.js'));
    expect(res.status).toBe(200);
    const text = await res.text();
    expect(text).not.toContain('@appricot/client');
    expect(text).toContain("from '/client/index.js'");
    expect(text).toContain('export const MAIN = 1;');
  });

  it('rejects any traversal segment the resolver itself can see', () => {
    // Over HTTP a WHATWG URL parser normalizes '/dist/../x' — and even the percent-encoded
    // '%2e%2e' spelling — away before the request line exists, so those never arrive. The
    // resolver still rejects a '..' segment on its own, which is the property that matters.
    expect(resolveStatic(roots, '/dist/../index.html')).toBeNull();
    expect(resolveStatic(roots, '/dist/a/../../index.html')).toBeNull();
    expect(resolveStatic(roots, '/dist/%2e%2e/index.html')).toBeNull();
    expect(resolveStatic(roots, '/dist/a\u0000b/main.js')).toBeNull();
  });

  it('answers an encoded dot-dot path the way the URL parser normalized it: as /', async () => {
    // The fetch client and the server's own decodePathname both run the WHATWG URL parser,
    // which collapses '%2e%2e' during path normalization — the request that reaches the
    // resolver is plain '/index.html'. Assert the collapse, not a lucky 404.
    const res = await fetch(url('/dist/%2e%2e/index.html'));
    expect(res.status).toBe(200);
    expect(res.headers.get('content-type')).toBe('text/html; charset=utf-8');
    expect(await res.text()).toContain('fixture page');
  });

  it('refuses non-GET/HEAD methods on static routes', async () => {
    const res = await fetch(url('/dist/main.js'), { method: 'POST' });
    expect(res.status).toBe(405);
    expect(res.headers.get('content-security-policy')).toBe(CSP);
  });
});

// --- the CSP on every response type -------------------------------------------------------------

describe('every response carries the strict CSP', () => {
  /** A one-path CSP assertion, as a test body. */
  function cspCase(path: string): () => Promise<void> {
    return async () => {
      const res = await fetch(url(path));
      expect(res.headers.get('content-security-policy')).toBe(CSP);
      expect(res.headers.get('x-content-type-options')).toBe('nosniff');
    };
  }

  it('on a static HTML hit', cspCase('/'));
  it('on a static JS hit', cspCase('/dist/main.js'));
  it('on a static CSS hit', cspCase('/styles.css'));
  it('on a 404', cspCase('/missing.js'));
  it('on a proxied 200 (/readyz)', cspCase('/readyz'));
  it('on a proxied 503 (/session)', cspCase('/session'));

  it('on a 405 (POST on a static route)', async () => {
    const res = await fetch(url('/dist/main.js'), { method: 'POST' });
    expect(res.status).toBe(405);
    expect(res.headers.get('content-security-policy')).toBe(CSP);
  });

  it('has no unsafe-inline and no unsafe-eval anywhere', () => {
    expect(CSP).not.toContain('unsafe-inline');
    expect(CSP).not.toContain('unsafe-eval');
  });

  it('gives a HEAD request the headers and no body', async () => {
    const res = await fetch(url('/'), { method: 'HEAD' });
    expect(res.status).toBe(200);
    expect(res.headers.get('content-security-policy')).toBe(CSP);
    expect(await res.text()).toBe('');
  });
});

// --- the proxy ------------------------------------------------------------------------------------

describe('the /readyz and /session proxy', () => {
  it('relays a readyz request and reply verbatim, CSP added', async () => {
    const res = await fetch(url('/readyz'));
    expect(res.status).toBe(200);
    expect(await res.text()).toBe('ready');
    expect(upstreamPaths.at(-1)).toBe('/readyz');
  });

  it('relays the upstream status when the answer is not a success', async () => {
    const res = await fetch(url('/session'));
    expect(res.status).toBe(503);
    expect(await res.text()).toBe('no');
    expect(upstreamPaths.at(-1)).toBe('/session');
  });

  it('answers 502 with the CSP when the streamer is not listening', async () => {
    const down = await startDemoServer({
      port: 0,
      host: '127.0.0.1',
      roots,
      upstream: { host: '127.0.0.1', port: 1 }, // nothing listens on port 1 in the container
    });
    try {
      const res = await fetch(`http://127.0.0.1:${down.port}/readyz`);
      expect(res.status).toBe(502);
      expect(res.headers.get('content-security-policy')).toBe(CSP);
    } finally {
      await down.close();
    }
  });
});

// --- the WebSocket upgrade ------------------------------------------------------------------------

describe('the /session WebSocket upgrade proxy', () => {
  it('proxies the handshake, adds the CSP to the 101, and pipes bytes both ways', async () => {
    const socket = connectSocket(demo.port, '127.0.0.1');
    try {
      const exchange = await new Promise<string>((resolve, reject) => {
        let buffer = '';
        const timer = setTimeout(() => {
          reject(new Error(`timed out waiting for the echo; got: ${buffer}`));
        }, 3000);
        socket.on('connect', () => {
          socket.write(
            `GET /session HTTP/1.1\r\nHost: 127.0.0.1:${demo.port}\r\nUpgrade: websocket\r\n` +
              `Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n` +
              `Sec-WebSocket-Version: 13\r\n\r\n`,
          );
          // Payload bytes right behind the handshake: they ride the upgrade's head buffer.
          socket.write('ping');
        });
        socket.on('data', (chunk: Buffer) => {
          buffer += chunk.toString('latin1');
          const headEnd = buffer.indexOf('\r\n\r\n');
          if (headEnd >= 0 && buffer.endsWith('ping')) {
            clearTimeout(timer);
            resolve(buffer);
          }
        });
        socket.on('error', (err: Error) => {
          clearTimeout(timer);
          reject(err);
        });
      });
      const head = exchange.slice(0, exchange.indexOf('\r\n\r\n'));
      expect(head.startsWith('HTTP/1.1 101 Switching Protocols')).toBe(true);
      const lower = head.toLowerCase();
      expect(lower).toContain(`content-security-policy: ${CSP.toLowerCase()}`);
      expect(lower).toContain('x-content-type-options: nosniff');
      expect(lower).toContain('upgrade: websocket');
      expect(lower).toContain('sec-websocket-accept: s3pplmbitxaq9kygzzhzrbk+xoo=');
      expect(exchange.endsWith('ping')).toBe(true); // the mock echoed through both proxies
    } finally {
      socket.destroy();
    }
  });

  it('refuses an upgrade of an unknown path with the CSP on the raw reply', async () => {
    const socket = connectSocket(demo.port, '127.0.0.1');
    try {
      const reply = await new Promise<string>((resolve, reject) => {
        let buffer = '';
        const timer = setTimeout(() => {
          reject(new Error(`timed out; got: ${buffer}`));
        }, 3000);
        socket.on('connect', () => {
          socket.write(
            `GET /other HTTP/1.1\r\nHost: 127.0.0.1:${demo.port}\r\nUpgrade: websocket\r\n` +
              `Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n` +
              `Sec-WebSocket-Version: 13\r\n\r\n`,
          );
        });
        socket.on('data', (chunk: Buffer) => {
          buffer += chunk.toString('latin1');
          if (buffer.includes('\r\n\r\n')) {
            clearTimeout(timer);
            resolve(buffer);
          }
        });
        socket.on('error', (err: Error) => {
          clearTimeout(timer);
          reject(err);
        });
      });
      expect(reply.startsWith('HTTP/1.1 404')).toBe(true);
      expect(reply.toLowerCase()).toContain(`content-security-policy: ${CSP.toLowerCase()}`);
    } finally {
      socket.destroy();
    }
  });
});
