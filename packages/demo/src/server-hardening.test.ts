/**
 * The demo server's failure paths and its distrust of the streamer (src/server.ts), against a
 * mock streamer that misbehaves on purpose:
 *
 * - only the page's own loopback origin may open /session (cross-site WebSocket hijacking,
 *   DNS rebinding);
 * - a malformed request target answers 400 instead of killing the process;
 * - a refused, dead, hung or abandoned upgrade ends both sockets and never crashes;
 * - headers are allowlisted both ways: no cookie goes up, no Set-Cookie, Location or content
 *   type comes down;
 * - the module rewrite touches import specifiers only, an unreadable module is a 404, and the
 *   server's own module is not served;
 * - a backslash or colon in a static path is refused.
 */
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { createServer, get as httpGet } from 'node:http';
import type { IncomingHttpHeaders, IncomingMessage, Server } from 'node:http';
import { connect as connectSocket } from 'node:net';
import type { Socket } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Duplex } from 'node:stream';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';

import { CSP, decodePathname, isPageOrigin, resolveStatic, startDemoServer } from './server';
import type { RunningDemoServer, StaticRoots } from './server';

// --- fixtures ---------------------------------------------------------------------------------

let roots: StaticRoots;
let upstream: Server;
let upstreamPort = 0;
/** Every running demo server, closed in afterAll. */
const demos: RunningDemoServer[] = [];
/** The main demo (short upstream timeout) and its log. */
let demo: RunningDemoServer;
const log = vi.fn();

/** What the mock streamer saw on each upgrade, in order. */
const upgrades: Array<{ url: string; headers: IncomingHttpHeaders; socket: Duplex }> = [];
/** Waiters for the next upgrade the mock sees. */
let onUpgrade: ((socket: Duplex) => void) | null = null;

const HANDSHAKE_101 =
  'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n' +
  'Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n';

beforeAll(async () => {
  const base = mkdtempSync(join(tmpdir(), 'appricot-demo-hardening-'));
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
  writeFileSync(
    join(roots.demoDist, 'rewrite.js'),
    [
      'import { A } from "@app-ricot/client";',
      "export * from '@app-ricot/client';",
      "import '@app-ricot/client';",
      "const lazy = () => import('@app-ricot/client');",
      "const name = '@app-ricot/client';",
      'export const label = "@app-ricot/client";',
      'export { lazy, name };',
      '',
    ].join('\n'),
  );
  writeFileSync(join(roots.demoDist, 'server.js'), 'export const SERVER_SIDE_ONLY = 1;\n');
  // A "module" that exists but cannot be read as a file: readFileSync throws EISDIR.
  mkdirSync(join(roots.demoDist, 'broken.js'));
  // Real files whose names hold a backslash and a colon (legal on Linux, where the tests
  // run): a resolver that let such a segment through would serve them. On a Windows path
  // module the same segments are separators and a drive, and traverse out of the root.
  writeFileSync(join(roots.demoDist, 'a\\b.js'), 'export const BACKSLASH = 1;\n');
  writeFileSync(join(roots.demoDist, 'c:colon.js'), 'export const COLON = 1;\n');
  writeFileSync(
    join(roots.clientDist, 'index.js'),
    "export const NAME = '@app-ricot/client';\n",
  );

  upstream = createServer((req, res) => {
    if (req.url === '/readyz?hostile') {
      res.writeHead(503, {
        'content-type': 'text/javascript',
        'set-cookie': ['planted=1; Path=/', 'other=2; Path=/'],
        location: 'http://evil.example/',
        refresh: '0; url=http://evil.example/',
      });
      res.end('alert(1)');
      return;
    }
    res.writeHead(200, { 'content-type': 'text/plain' });
    res.end('ready');
  });
  upstream.on('upgrade', (req: IncomingMessage, socket: Duplex) => {
    upgrades.push({ url: req.url ?? '', headers: req.headers, socket });
    socket.on('error', () => socket.destroy());
    // An http server's sockets allow half-open: when the proxy ends its side, close ours,
    // as the streamer does, so 'close' tells the tests the proxy let go.
    socket.on('end', () => socket.destroy());
    const waiter = onUpgrade;
    onUpgrade = null;
    waiter?.(socket);
    switch (req.url) {
      case '/session?refuse':
        socket.end(
          'HTTP/1.1 503 Service Unavailable\r\n' +
            'Set-Cookie: planted=1; Path=/\r\n' +
            'Location: http://evil.example/\r\n' +
            'Content-Type: text/javascript\r\n' +
            'Content-Length: 4\r\n\r\nbusy',
        );
        return;
      case '/session?hang':
        return; // accepts TCP, then says nothing
      case '/session?set-cookie':
        socket.write(
          `${HANDSHAKE_101}Set-Cookie: planted=1; Path=/\r\nLocation: http://evil.example/\r\n\r\n`,
        );
        return;
      default:
        socket.write(`${HANDSHAKE_101}\r\n`);
    }
  });
  await new Promise<void>((resolve) => {
    upstream.listen(0, '127.0.0.1', () => resolve());
  });
  upstreamPort = (upstream.address() as { port: number }).port;
  demo = await startDemo({ upstreamTimeoutMs: 300, onLog: log });
});

afterAll(async () => {
  for (const running of demos) {
    await running.close();
  }
  await new Promise<void>((resolve) => {
    for (const { socket } of upgrades) {
      socket.destroy();
    }
    upstream.closeAllConnections();
    upstream.close(() => resolve());
  });
});

async function startDemo(
  options: { upstreamTimeoutMs?: number; onLog?: (line: string) => void; port?: number } = {},
): Promise<RunningDemoServer> {
  const running = await startDemoServer({
    port: 0,
    host: '127.0.0.1',
    roots,
    upstream: { host: '127.0.0.1', port: options.port ?? upstreamPort },
    ...(options.upstreamTimeoutMs !== undefined ? { upstreamTimeoutMs: options.upstreamTimeoutMs } : {}),
    ...(options.onLog !== undefined ? { onLog: options.onLog } : {}),
  });
  demos.push(running);
  return running;
}

/** A raw exchange: writes `request`, collects everything until the server closes the socket
 * (or `until` holds), and reports whether the socket closed. */
function raw(
  port: number,
  request: string,
  until?: (received: string) => boolean,
  timeoutMs = 3000,
): Promise<{ received: string; closed: boolean; socket: Socket }> {
  return new Promise((resolve, reject) => {
    const socket = connectSocket(port, '127.0.0.1');
    let received = '';
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error(`timed out; got: ${JSON.stringify(received)}`));
    }, timeoutMs);
    const finish = (closed: boolean): void => {
      clearTimeout(timer);
      resolve({ received, closed, socket });
    };
    socket.on('connect', () => socket.write(request));
    socket.on('data', (chunk: Buffer) => {
      received += chunk.toString('latin1');
      if (until?.(received) === true) {
        finish(false);
      }
    });
    socket.on('close', () => finish(true));
    socket.on('error', () => undefined); // a reset shows up as a close
  });
}

function upgradeRequest(
  port: number,
  path: string,
  extra: Readonly<Record<string, string>> = {},
  host = `127.0.0.1:${port}`,
): string {
  const headers: Record<string, string> = {
    Host: host,
    Origin: `http://${host}`,
    Upgrade: 'websocket',
    Connection: 'Upgrade',
    'Sec-WebSocket-Key': 'dGhlIHNhbXBsZSBub25jZQ==',
    'Sec-WebSocket-Version': '13',
    ...extra,
  };
  const lines = Object.entries(headers)
    .filter(([, value]) => value !== '')
    .map(([name, value]) => `${name}: ${value}`);
  return `GET ${path} HTTP/1.1\r\n${lines.join('\r\n')}\r\n\r\n`;
}

function headOf(received: string): string {
  const end = received.indexOf('\r\n\r\n');
  return (end < 0 ? received : received.slice(0, end)).toLowerCase();
}

/** Resolves when `socket` closes, or rejects after `ms`. */
function closedWithin(socket: Duplex, ms: number): Promise<void> {
  if (socket.destroyed) {
    return Promise.resolve();
  }
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('socket still open')), ms);
    socket.once('close', () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

function nextUpgrade(): Promise<Duplex> {
  return new Promise((resolve) => {
    onUpgrade = resolve;
  });
}

/** GET through node:http, which shows every response header (fetch hides Set-Cookie). */
function plainGet(port: number, path: string): Promise<{ status: number; headers: IncomingHttpHeaders; body: string }> {
  return new Promise((resolve, reject) => {
    httpGet({ host: '127.0.0.1', port, path, agent: false }, (res) => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', (chunk: string) => {
        body += chunk;
      });
      res.on('end', () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body }));
    }).on('error', reject);
  });
}

async function stillServes(port: number): Promise<void> {
  const res = await fetch(`http://127.0.0.1:${port}/`);
  expect(res.status).toBe(200);
}

// --- the origin check -------------------------------------------------------------------------------

describe('only the page on a loopback origin may open /session', () => {
  it('accepts the page origin on 127.0.0.1, localhost and [::1], any port', () => {
    expect(isPageOrigin({ host: '127.0.0.1:8390', origin: 'http://127.0.0.1:8390' })).toBe(true);
    expect(isPageOrigin({ host: 'localhost:9999', origin: 'http://localhost:9999' })).toBe(true);
    expect(isPageOrigin({ host: '[::1]:8390', origin: 'http://[::1]:8390' })).toBe(true);
    expect(isPageOrigin({ host: 'LOCALHOST:8390', origin: 'http://localhost:8390' })).toBe(true);
  });

  it('refuses another origin, a missing origin, a rebound host and a port mismatch', () => {
    expect(isPageOrigin({ host: '127.0.0.1:8390', origin: 'http://evil.example' })).toBe(false);
    expect(isPageOrigin({ host: '127.0.0.1:8390' })).toBe(false);
    expect(isPageOrigin({ origin: 'http://127.0.0.1:8390' })).toBe(false);
    expect(isPageOrigin({ host: 'evil.example:8390', origin: 'http://evil.example:8390' })).toBe(false);
    expect(isPageOrigin({ host: '127.0.0.1:8390', origin: 'http://127.0.0.1:8391' })).toBe(false);
    expect(isPageOrigin({ host: '127.0.0.1:8390', origin: 'http://localhost:8390' })).toBe(false);
    expect(isPageOrigin({ host: 'evil@127.0.0.1:8390', origin: 'http://evil@127.0.0.1:8390' })).toBe(false);
    expect(isPageOrigin({ host: '127.0.0.1:8390', origin: 'null' })).toBe(false);
  });

  it('answers 403 with the CSP and never reaches the streamer', async () => {
    const seen = upgrades.length;
    const cases = [
      upgradeRequest(demo.port, '/session', { Origin: 'http://evil.example' }),
      upgradeRequest(demo.port, '/session', { Origin: '' }),
      upgradeRequest(demo.port, '/session', {}, `evil.example:${demo.port}`),
    ];
    for (const request of cases) {
      const reply = await raw(demo.port, request);
      expect(reply.received.startsWith('HTTP/1.1 403')).toBe(true);
      expect(headOf(reply.received)).toContain(`content-security-policy: ${CSP.toLowerCase()}`);
      expect(reply.closed).toBe(true);
    }
    expect(upgrades).toHaveLength(seen);
    expect(log).toHaveBeenCalledWith(expect.stringContaining('not from the page'));
  });

  it('proxies the same upgrade from the page origin', async () => {
    const reply = await raw(
      demo.port,
      upgradeRequest(demo.port, '/session', {}, `localhost:${demo.port}`),
      (received) => received.includes('\r\n\r\n'),
    );
    reply.socket.destroy();
    expect(reply.received.startsWith('HTTP/1.1 101')).toBe(true);
  });
});

// --- malformed targets ---------------------------------------------------------------------------------

describe('a malformed request target', () => {
  it('decodes to null instead of throwing', () => {
    expect(decodePathname('http://a:99999/')).toBeNull();
    expect(decodePathname('//a:99999/')).toBeNull();
    expect(decodePathname('http://[::1/')).toBeNull();
  });

  it('is a 400 on the request path, and the server lives on', async () => {
    const reply = await raw(demo.port, 'GET http://a:99999/ HTTP/1.1\r\nHost: a\r\n\r\n', (r) =>
      r.includes('\r\n\r\n'),
    );
    reply.socket.destroy();
    expect(reply.received.startsWith('HTTP/1.1 400')).toBe(true);
    await stillServes(demo.port);
  });

  it('is refused on the upgrade path, and the server lives on', async () => {
    const request = upgradeRequest(demo.port, '/session').replace(
      'GET /session ',
      'GET http://a:99999/session ',
    );
    const reply = await raw(demo.port, request);
    expect(reply.received.startsWith('HTTP/1.1 404')).toBe(true);
    await stillServes(demo.port);
  });
});

// --- upgrade failure paths ---------------------------------------------------------------------------------

describe('an upgrade the streamer does not complete', () => {
  it('relays a refusal: the status, the CSP, a log line, no streamer header, both sockets closed', async () => {
    log.mockClear();
    const upstreamSide = nextUpgrade();
    const reply = await raw(demo.port, upgradeRequest(demo.port, '/session?refuse'));
    const head = headOf(reply.received);
    expect(reply.received.startsWith('HTTP/1.1 503 Service Unavailable')).toBe(true);
    expect(head).toContain(`content-security-policy: ${CSP.toLowerCase()}`);
    expect(head).toContain('content-type: text/plain; charset=utf-8');
    expect(head).not.toContain('set-cookie');
    expect(head).not.toContain('location');
    expect(head).not.toContain('javascript');
    expect(reply.closed).toBe(true);
    await closedWithin(await upstreamSide, 1000);
    expect(log).toHaveBeenCalledWith('session websocket upgrade refused with 503');
  });

  it('answers 502 when the streamer is not listening', async () => {
    const down = await startDemo({ port: 1 }); // nothing listens on port 1 in the container
    const reply = await raw(down.port, upgradeRequest(down.port, '/session'));
    expect(reply.received.startsWith('HTTP/1.1 502')).toBe(true);
    expect(headOf(reply.received)).toContain(`content-security-policy: ${CSP.toLowerCase()}`);
    expect(reply.closed).toBe(true);
  });

  it('answers 504 and drops the streamer socket when the streamer never answers', async () => {
    const upstreamSide = nextUpgrade();
    const reply = await raw(demo.port, upgradeRequest(demo.port, '/session?hang'));
    expect(reply.received.startsWith('HTTP/1.1 504')).toBe(true);
    expect(reply.closed).toBe(true);
    await closedWithin(await upstreamSide, 1000);
  });

  it('survives a browser that resets while the handshake is pending, and ends the upstream side', async () => {
    const patient = await startDemo({ upstreamTimeoutMs: 10_000 });
    const upstreamSide = nextUpgrade();
    const socket = connectSocket(patient.port, '127.0.0.1');
    socket.on('error', () => undefined);
    socket.on('connect', () => socket.write(upgradeRequest(patient.port, '/session?hang')));
    const streamerSocket = await upstreamSide;
    socket.resetAndDestroy();
    // Well before the 10 s timeout: the browser leaving is what ended it.
    await closedWithin(streamerSocket, 1000);
    await stillServes(patient.port);
  });

  it('ends the upstream side when the browser half-closes while the handshake is pending', async () => {
    const patient = await startDemo({ upstreamTimeoutMs: 10_000 });
    const upstreamSide = nextUpgrade();
    const socket = connectSocket(patient.port, '127.0.0.1');
    socket.on('error', () => undefined);
    socket.on('connect', () => socket.write(upgradeRequest(patient.port, '/session?hang')));
    const streamerSocket = await upstreamSide;
    socket.end();
    await closedWithin(streamerSocket, 1000);
    await closedWithin(socket, 1000);
  });
});

// --- header allowlists -----------------------------------------------------------------------------------------

describe('headers cross the proxy by allowlist only', () => {
  it('sends the streamer no cookie, no authorization and no origin on the upgrade', async () => {
    const reply = await raw(
      demo.port,
      upgradeRequest(demo.port, '/session', {
        Cookie: 'session=other-local-app',
        Authorization: 'Basic c2VjcmV0',
        'X-Custom': 'nope',
      }),
      (received) => received.includes('\r\n\r\n'),
    );
    reply.socket.destroy();
    const seen = upgrades.at(-1)?.headers ?? {};
    expect(seen.cookie).toBeUndefined();
    expect(seen.authorization).toBeUndefined();
    expect(seen.origin).toBeUndefined();
    expect(seen['x-custom']).toBeUndefined();
    expect(seen['sec-websocket-key']).toBe('dGhlIHNhbXBsZSBub25jZQ==');
    expect(seen.host).toBe(`127.0.0.1:${upstreamPort}`);
  });

  it("relays none of the streamer's own headers on a 101", async () => {
    const reply = await raw(
      demo.port,
      upgradeRequest(demo.port, '/session?set-cookie'),
      (received) => received.includes('\r\n\r\n'),
    );
    reply.socket.destroy();
    const head = headOf(reply.received);
    expect(reply.received.startsWith('HTTP/1.1 101')).toBe(true);
    expect(head).toContain('sec-websocket-accept: s3pplmbitxaq9kygzzhzrbk+xoo=');
    expect(head).not.toContain('set-cookie');
    expect(head).not.toContain('location');
  });

  it('serves /readyz as status and plain text only: no cookie, redirect or script type', async () => {
    const res = await plainGet(demo.port, '/readyz?hostile');
    expect(res.status).toBe(503);
    expect(res.headers['set-cookie']).toBeUndefined();
    expect(Object.keys(res.headers)).not.toContain('location');
    expect(res.headers['refresh']).toBeUndefined();
    expect(res.headers['content-type']).toBe('text/plain; charset=utf-8');
    expect(res.headers['x-content-type-options']).toBe('nosniff');
    expect(res.headers['content-security-policy']).toBe(CSP);
  });
});

// --- the module rewrite and the static map ------------------------------------------------------------------------

describe('the module rewrite and the static map', () => {
  it('rewrites import and export specifiers in both quote styles, and nothing else', async () => {
    const text = await (await fetch(`http://127.0.0.1:${demo.port}/dist/rewrite.js`)).text();
    expect(text).toContain('import { A } from "/client/index.js";');
    expect(text).toContain("export * from '/client/index.js';");
    expect(text).toContain("import '/client/index.js';");
    expect(text).toContain("import('/client/index.js')");
    // The same string as data stays data.
    expect(text).toContain("const name = '@app-ricot/client';");
    expect(text).toContain('export const label = "@app-ricot/client";');
  });

  it('gives a HEAD on a module its length and no body', async () => {
    const get = await (await fetch(`http://127.0.0.1:${demo.port}/dist/rewrite.js`)).text();
    const head = await fetch(`http://127.0.0.1:${demo.port}/dist/rewrite.js`, { method: 'HEAD' });
    expect(head.status).toBe(200);
    expect(head.headers.get('content-length')).toBe(String(Buffer.byteLength(get)));
    expect(await head.text()).toBe('');
  });

  it('serves /client/* unchanged', async () => {
    const text = await (await fetch(`http://127.0.0.1:${demo.port}/client/index.js`)).text();
    expect(text).toBe("export const NAME = '@app-ricot/client';\n");
  });

  it("does not serve the server's own module", async () => {
    expect((await fetch(`http://127.0.0.1:${demo.port}/dist/server.js`)).status).toBe(404);
    expect((await fetch(`http://127.0.0.1:${demo.port}/dist/server`)).status).toBe(404);
  });

  it('answers 404 for a module it cannot read, and lives on', async () => {
    const res = await fetch(`http://127.0.0.1:${demo.port}/dist/broken.js`);
    expect(res.status).toBe(404);
    expect(res.headers.get('content-security-policy')).toBe(CSP);
    await stillServes(demo.port);
  });

  it('refuses a backslash or a colon in a static path segment', async () => {
    expect(resolveStatic(roots, '/dist/..\\..\\index.html')).toBeNull();
    expect(resolveStatic(roots, '/dist/a\\b.js')).toBeNull();
    expect(resolveStatic(roots, '/dist/c:colon.js')).toBeNull();
    expect((await fetch(`http://127.0.0.1:${demo.port}/dist/a%5Cb.js`)).status).toBe(404);
    expect((await fetch(`http://127.0.0.1:${demo.port}/dist/..%5C..%5Cindex.html`)).status).toBe(404);
  });
});
