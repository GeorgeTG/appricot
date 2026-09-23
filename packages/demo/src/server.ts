/**
 * The demo's static server and reverse proxy (the typed core behind `server.mjs`).
 *
 * Contract (justfile `demo` recipe, compose.yml port scheme): plain node, zero dependencies,
 * listening on 0.0.0.0:8390 inside the container — the ONLY published port, on the host's
 * loopback. The streamer stays on its loopback-only bind at 127.0.0.1:8391 behind this proxy,
 * so it never listens on a non-loopback address even in the manual demo run.
 *
 * Routes:
 *   /            -> packages/demo/index.html
 *   /styles.css  -> packages/demo/styles.css
 *   /dist/*      -> packages/demo/dist       (the page's own modules)
 *   /client/*    -> packages/client/dist     (@appricot/client, ESM)
 *   /react/*     -> packages/react/dist      (@appricot/react, ESM)
 *   /vendor/*    -> node_modules files an import map would have named (see VENDOR_FILES —
 *                   empty, measured; the map itself is gone, see sendDistModule)
 *   /session     -> proxied to 127.0.0.1:8391 (WebSocket upgrade AND plain requests)
 *   /readyz      -> proxied to 127.0.0.1:8391
 *
 * Every response this server emits — static hits, 404s, 405s, proxied replies and the 101 of a
 * proxied WebSocket upgrade — carries the strict CSP of ADR-0003 §3: no 'unsafe-inline', no
 * 'unsafe-eval', so the page's styles live in a .css file, its scripts are external, and even
 * the import map is gone — the page's modules get their bare specifier rewritten at serve
 * time (see sendDistModule), because nothing inline may stay inline.
 */

import { Agent, createServer, request as httpRequest } from 'node:http';
import type { IncomingMessage, Server, ServerResponse } from 'node:http';
import { createReadStream, existsSync, readFileSync } from 'node:fs';
import { dirname, extname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** The Content-Security-Policy every response carries (ADR-0003 §3): no 'unsafe-inline',
 *  no 'unsafe-eval' — and nothing inline to allow. The page's stylesheet, scripts and the
 *  modules those import are all same-origin URLs; the one bare specifier its modules use
 *  is rewritten at serve time (see `sendDistModule`), so no import map exists. */
export const CSP =
  "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; " +
  "img-src 'self' data: blob:; frame-ancestors 'none'";

/**
 * The exact node_modules files an import map would have named. EMPTY, on a measurement:
 * react 19.3.0, react-dom 19.3.0 and scheduler 0.28.0 (pnpm-lock) ship CommonJS only — no
 * .mjs, no ESM entries — and a browser cannot execute `module.exports` under
 * `script-src 'self'` without a require() shim nobody should hand-write. The import map
 * itself was later removed outright (see `sendDistModule`): the page's modules get their
 * bare specifier rewritten at serve time instead, so nothing inline remains. The demo
 * runs client-only (@appricot/client under /client/); @appricot/react stays a declared
 * dependency and is served under /react/ for a later bundler-backed showcase, but the page
 * never imports it. If an ESM-capable runtime lands, list the exact files here.
 */
export const VENDOR_FILES: Readonly<Record<string, string>> = {};

/** The upstream the proxy side forwards to: the streamer's loopback bind. */
export interface ProxyTarget {
  host: string;
  port: number;
}

/** Which on-disk tree each URL prefix maps to. */
export interface StaticRoots {
  /** `/` and `/styles.css`. */
  demoRoot: string;
  /** `/dist/*`. */
  demoDist: string;
  /** `/client/*`. */
  clientDist: string;
  /** `/react/*`. */
  reactDist: string;
}

/** A static route that resolved to a real file. */
export interface ResolvedStatic {
  file: string;
  contentType: string;
}

export interface DemoServerOptions {
  /** Listen port; 0 lets the OS choose (the unit tests). Default 8390. */
  port?: number;
  /** Listen address. Default '0.0.0.0' (the container publishes it on the host's loopback). */
  host?: string;
  /** Default { host: '127.0.0.1', port: 8391 }. */
  upstream?: ProxyTarget;
  /** Default: the real package trees, located relative to this module. */
  roots?: StaticRoots;
  /** One line per notable event; the shim wires it to the console. */
  onLog?: (line: string) => void;
}

const CONTENT_TYPES: Readonly<Record<string, string>> = {
  '.css': 'text/css; charset=utf-8',
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.map': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
};

/**
 * Maps a URL prefix to a root directory. A plain table, so a test can pin every row. Order
 * matters only for readability: prefixes do not overlap.
 */
const PREFIX_ROOT: ReadonlyArray<readonly [prefix: string, root: keyof Omit<StaticRoots, 'demoRoot'>]> = [
  ['/dist/', 'demoDist'],
  ['/client/', 'clientDist'],
  ['/react/', 'reactDist'],
];

/** The percent-decoded pathname of a request URL, or null when it does not decode. The URL
 * parser collapses literal '/../' before this returns, and `resolveStatic` rejects the
 * percent-encoded variant, so the two traversal spellings are both covered. */
export function decodePathname(rawUrl: string | undefined): string | null {
  const parsed = new URL(rawUrl ?? '/', 'http://demo.invalid');
  try {
    return decodeURIComponent(parsed.pathname);
  } catch {
    return null;
  }
}

function contentTypeFor(file: string): string {
  return CONTENT_TYPES[extname(file)] ?? 'application/octet-stream';
}

/**
 * Splits a prefix-stripped path into safe segments: no '..', no '.', no empty segment, no NUL.
 * Returns null when the path is not a plain name under the root.
 */
function safeSegments(sub: string): string[] | null {
  const parts = sub.split('/').filter((part) => part.length > 0 && part !== '.');
  for (const part of parts) {
    if (part === '..' || part.includes('\0')) {
      return null;
    }
  }
  return parts;
}

/**
 * Maps a pathname to a file under the roots, or null when it maps nowhere. Two behaviours a
 * browser needs and a bundler normally provides:
 *
 * - The TS-ESM extension probe: `tsc`'s ESNext output imports './protocol' with no extension,
 *   and a browser 404s that. A path with no extension that misses on disk is retried once
 *   with '.js'.
 * - Existence is part of resolution: a mapped but missing file is a 404, not a 500.
 */
export function resolveStatic(roots: StaticRoots, pathname: string): ResolvedStatic | null {
  if (pathname === '/' || pathname === '/index.html') {
    const file = join(roots.demoRoot, 'index.html');
    return existsSync(file) ? { file, contentType: contentTypeFor(file) } : null;
  }
  if (pathname === '/styles.css') {
    const file = join(roots.demoRoot, 'styles.css');
    return existsSync(file) ? { file, contentType: contentTypeFor(file) } : null;
  }
  if (pathname.startsWith('/vendor/')) {
    const name = pathname.slice('/vendor/'.length);
    const file = name.length > 0 ? VENDOR_FILES[name] : undefined;
    return file !== undefined && existsSync(file)
      ? { file, contentType: contentTypeFor(file) }
      : null;
  }
  for (const [prefix, rootKey] of PREFIX_ROOT) {
    if (!pathname.startsWith(prefix)) {
      continue;
    }
    const segments = safeSegments(pathname.slice(prefix.length));
    if (segments === null) {
      return null;
    }
    const direct = join(roots[rootKey], ...segments);
    if (existsSync(direct)) {
      return { file: direct, contentType: contentTypeFor(direct) };
    }
    const base = segments.at(-1);
    if (base !== undefined && !base.includes('.')) {
      const probed = `${direct}.js`;
      if (existsSync(probed)) {
        return { file: probed, contentType: contentTypeFor(probed) };
      }
    }
    return null;
  }
  return null;
}

/**
 * The real package trees, located relative to this module: under vitest it runs from `src/`,
 * under node from `dist/`, so the package root is the first ancestor holding `index.html`.
 */
export function defaultRoots(): StaticRoots {
  const here = dirname(fileURLToPath(import.meta.url));
  let demoRoot = join(here, '../..');
  for (const candidate of [join(here, '..'), join(here, '../..')]) {
    if (existsSync(join(candidate, 'index.html'))) {
      demoRoot = candidate;
      break;
    }
  }
  return {
    demoRoot,
    demoDist: join(demoRoot, 'dist'),
    clientDist: join(demoRoot, '../client/dist'),
    reactDist: join(demoRoot, '../react/dist'),
  };
}

/** The headers every response carries, whatever its type. */
function securityHeaders(): Record<string, string> {
  return {
    'content-security-policy': CSP,
    'x-content-type-options': 'nosniff',
    'cache-control': 'no-store',
  };
}

/** Sends a short text response with the security headers on it. */
function sendText(
  res: ServerResponse,
  status: number,
  body: string,
  method: string,
): void {
  res.writeHead(status, {
    ...securityHeaders(),
    'content-type': 'text/plain; charset=utf-8',
    'content-length': String(Buffer.byteLength(body)),
  });
  res.end(method === 'HEAD' ? undefined : body);
}

/** Sends one resolved file. `stream` stays the demo's only body writer. */
function sendFile(res: ServerResponse, resolved: ResolvedStatic, method: string): void {
  const stream = createReadStream(resolved.file);
  stream.on('error', () => {
    if (!res.headersSent) {
      sendText(res, 404, 'not found', method);
    } else {
      res.destroy();
    }
  });
  res.writeHead(200, {
    ...securityHeaders(),
    'content-type': resolved.contentType,
  });
  if (method === 'HEAD') {
    stream.destroy();
    res.end();
    return;
  }
  stream.pipe(res);
}

/** The one bare specifier the page's own modules import the client under, single- or
 *  double-quoted. Rewritten to the URL this server serves it under; a rewrite, not an
 *  import map, because Chromium applies script-src to inline import maps (measured
 *  2026-09-21: under a plain 'self' policy the map is blocked, `@appricot/client` then
 *  fails to resolve and no page script runs at all, and neither the block's sha256 nor a
 *  per-response nonce made this browser accept it). The rewrite keeps the source — and
 *  its tests, which mock '@appricot/client' by name — on the workspace name. */
const CLIENT_SPECIFIER = /(['"])@appricot\/client\1/g;

/** Sends one of the page's own modules (/dist/*.js) with its bare client specifier
 *  rewritten to the served URL. Plain text in, plain text out; HEAD gets no body. */
function sendDistModule(res: ServerResponse, resolved: ResolvedStatic, method: string): void {
  const body = readFileSync(resolved.file, 'utf8').replace(
    CLIENT_SPECIFIER,
    '$1/client/index.js$1',
  );
  res.writeHead(200, {
    ...securityHeaders(),
    'content-type': resolved.contentType,
    'content-length': String(Buffer.byteLength(body)),
  });
  res.end(method === 'HEAD' ? undefined : body);
}

/** The proxied paths: the WebSocket session and the streamer's readiness probe. */
function isProxied(pathname: string): boolean {
  return pathname === '/session' || pathname === '/readyz';
}

/** Forwards one plain HTTP request to the streamer and the reply back, CSP added. The
 * request rides a no-keep-alive agent: the default global agent pools connections (Node 19+),
 * and a pooled proxy socket would keep the streamer's server from ever closing in tests. */
function proxyRequest(
  req: IncomingMessage,
  res: ServerResponse,
  target: ProxyTarget,
  agent: Agent,
): void {
  const upstream = httpRequest(
    {
      hostname: target.host,
      port: target.port,
      path: req.url,
      method: req.method,
      headers: { ...req.headers, host: `${target.host}:${target.port}` },
      agent,
    },
    (upRes) => {
      res.writeHead(upRes.statusCode ?? 502, {
        ...upRes.headers,
        ...securityHeaders(),
      });
      upRes.pipe(res);
    },
  );
  upstream.on('error', () => {
    if (!res.headersSent) {
      sendText(res, 502, 'upstream unavailable', req.method ?? 'GET');
    } else {
      res.destroy();
    }
  });
  req.pipe(upstream);
}

/** One proxied WebSocket upgrade, written raw so our own 101 carries the CSP. */
function proxyUpgrade(
  req: IncomingMessage,
  clientSocket: import('node:stream').Duplex,
  head: Buffer,
  target: ProxyTarget,
  agent: Agent,
  onLog: (line: string) => void,
): void {
  const upstream = httpRequest({
    hostname: target.host,
    port: target.port,
    path: req.url,
    method: 'GET',
    headers: { ...req.headers, host: `${target.host}:${target.port}` },
    agent,
  });
  upstream.on('upgrade', (upRes, upSocket, upHead) => {
    const lines = ['HTTP/1.1 101 Switching Protocols'];
    for (const [name, value] of Object.entries(upRes.headers)) {
      if (value === undefined) {
        continue;
      }
      lines.push(`${name}: ${Array.isArray(value) ? value.join(', ') : value}`);
    }
    for (const [name, value] of Object.entries(securityHeaders())) {
      lines.push(`${name}: ${value}`);
    }
    clientSocket.write(`${lines.join('\r\n')}\r\n\r\n`);
    if (upHead.length > 0) {
      clientSocket.write(upHead);
    }
    if (head.length > 0) {
      upSocket.write(head);
    }
    upSocket.pipe(clientSocket);
    clientSocket.pipe(upSocket);
    upSocket.on('error', () => {
      upSocket.destroy();
      clientSocket.destroy();
    });
    clientSocket.on('error', () => {
      upSocket.destroy();
      clientSocket.destroy();
    });
    upSocket.on('close', () => clientSocket.destroy());
    clientSocket.on('close', () => upSocket.destroy());
    onLog('session websocket proxied');
  });
  // The streamer answered the upgrade with a plain response: relay its head, then end both
  // sides — nothing sensible can follow a refused upgrade. The status is logged because the
  // streamer itself is silent per connection: a refused upgrade is the only place the demo's
  // log shows what the session endpoint said (measured 2026-09-21: this line was the missing
  // eye on the one-shot 503s).
  upstream.on('response', (upRes) => {
    onLog(`session websocket upgrade refused with ${upRes.statusCode ?? '?'}`);
    const lines = [`HTTP/1.1 ${upRes.statusCode ?? 502} ${upRes.statusMessage ?? ''}`.trimEnd()];
    for (const [name, value] of Object.entries(upRes.headers)) {
      if (value !== undefined) {
        lines.push(`${name}: ${Array.isArray(value) ? value.join(', ') : value}`);
      }
    }
    for (const [name, value] of Object.entries(securityHeaders())) {
      lines.push(`${name}: ${value}`);
    }
    clientSocket.write(`${lines.join('\r\n')}\r\n\r\n`);
    upRes.resume();
    upRes.on('end', () => clientSocket.destroy());
    clientSocket.destroy();
  });
  upstream.on('error', () => clientSocket.destroy());
  upstream.end();
}

/** Writes a short raw response on an upgrade socket that will not be proxied. */
function refuseUpgrade(
  clientSocket: import('node:stream').Duplex,
  status: number,
  reason: string,
): void {
  const body = reason;
  const headers = [
    `HTTP/1.1 ${status} ${reason}`,
    'Content-Type: text/plain; charset=utf-8',
    ...Object.entries(securityHeaders()).map(([n, v]) => `${n}: ${v}`),
    `Content-Length: ${Buffer.byteLength(body)}`,
    'Connection: close',
    '',
    '',
  ];
  clientSocket.write(headers.join('\r\n') + body);
  clientSocket.destroy();
}

/** Builds the demo server: request handler, upgrade handler, tracked sockets. */
export function createDemoServer(options: DemoServerOptions = {}): Server {
  const roots = options.roots ?? defaultRoots();
  const target = options.upstream ?? { host: '127.0.0.1', port: 8391 };
  const onLog = options.onLog ?? ((): void => undefined);
  const proxyAgent = new Agent({ keepAlive: false });

  const server = createServer((req, res) => {
    const pathname = decodePathname(req.url);
    if (pathname === null) {
      sendText(res, 400, 'bad request path', req.method ?? 'GET');
      return;
    }
    if (isProxied(pathname)) {
      proxyRequest(req, res, target, proxyAgent);
      return;
    }
    const method = req.method ?? 'GET';
    if (method !== 'GET' && method !== 'HEAD') {
      sendText(res, 405, 'method not allowed', method);
      return;
    }
    const resolved = resolveStatic(roots, pathname);
    if (resolved === null) {
      sendText(res, 404, 'not found', method);
      return;
    }
    if (pathname.startsWith('/dist/') && extname(resolved.file) === '.js') {
      sendDistModule(res, resolved, method);
      return;
    }
    sendFile(res, resolved, method);
  });

  server.on('upgrade', (req, clientSocket, head) => {
    const pathname = decodePathname(req.url);
    if (pathname === '/session') {
      proxyUpgrade(req, clientSocket, head, target, proxyAgent, onLog);
      return;
    }
    refuseUpgrade(clientSocket, 404, 'not found');
  });

  // Every socket is tracked so close() can tear a live proxy down deterministically: node's
  // close() waits for open connections forever, and closeAllConnections() alone was measured
  // (2026-09-21, this test suite) to leave proxied WebSocket connections undestroyed.
  const sockets = new Set<import('node:net').Socket>();
  server.on('connection', (socket) => {
    sockets.add(socket);
    socket.on('close', () => {
      sockets.delete(socket);
    });
  });

  const originalClose = server.close.bind(server);
  server.close = ((callback?: (err?: Error) => void): Server => {
    server.closeAllConnections();
    for (const socket of [...sockets]) {
      socket.destroy();
    }
    sockets.clear();
    proxyAgent.destroy();
    return originalClose(callback);
  }) as typeof server.close;

  return server;
}

/** What `startDemoServer` resolves with once the socket is listening. */
export interface RunningDemoServer {
  server: Server;
  /** The bound port (useful when 0 was asked for). */
  port: number;
  /** Destroys the sockets and closes the listener; resolves once the server is down. */
  close(): Promise<void>;
}

/** Starts the demo server and resolves when it is listening. */
export function startDemoServer(options: DemoServerOptions = {}): Promise<RunningDemoServer> {
  const server = createDemoServer(options);
  const port = options.port ?? 8390;
  const host = options.host ?? '0.0.0.0';
  return new Promise((resolve, reject) => {
    const onError = (err: Error): void => reject(err);
    server.once('error', onError);
    server.listen(port, host, () => {
      server.off('error', onError);
      const address = server.address();
      const boundPort = typeof address === 'object' && address !== null ? address.port : port;
      const close = (): Promise<void> =>
        new Promise((resolveClose, rejectClose) => {
          server.close((err) => {
            if (err === undefined) {
              resolveClose();
            } else {
              rejectClose(err);
            }
          });
        });
      resolve({ server, port: boundPort, close });
    });
  });
}
