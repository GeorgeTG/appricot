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
 *   /session     -> WebSocket upgrade proxied to 127.0.0.1:8391, from the page's own
 *                   loopback origin only; a plain request gets 426
 *   /readyz      -> proxied to 127.0.0.1:8391: its status and body, as plain text
 *
 * Every response this server emits — static hits, 404s, 405s, proxied replies and the 101 of a
 * proxied WebSocket upgrade — carries the strict CSP of ADR-0003 §3: no 'unsafe-inline', no
 * 'unsafe-eval', so the page's styles live in a .css file, its scripts are external, and even
 * the import map is gone — the page's modules get their bare specifier rewritten at serve
 * time (see sendDistModule), because nothing inline may stay inline.
 *
 * The streamer shares the app's sandbox and is treated as compromised with it (ADR-0003), so
 * the proxy passes allowlisted headers only, in both directions: the browser's cookies never
 * reach the streamer, and the streamer can set no cookie, redirect or content type here.
 */

import { Agent, STATUS_CODES, createServer, request as httpRequest } from 'node:http';
import type { IncomingHttpHeaders, IncomingMessage, Server, ServerResponse } from 'node:http';
import { createReadStream, existsSync, readFileSync } from 'node:fs';
import { basename, dirname, extname, join, resolve, sep } from 'node:path';
import type { Duplex } from 'node:stream';
import { fileURLToPath } from 'node:url';

/** The Content-Security-Policy every response carries (ADR-0003 §3): no 'unsafe-inline',
 *  no 'unsafe-eval' — and nothing inline to allow. The page's stylesheet, scripts and the
 *  modules those import are all same-origin URLs; the one bare specifier its modules use
 *  is rewritten at serve time (see `sendDistModule`), so no import map exists. Trusted Types
 *  are required for every script sink and no policy may be created, so a string that ever
 *  reached a sink the lint rules missed throws instead of running (ADR-0003 §3). The page
 *  and the client use no such sink. */
export const CSP =
  "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; " +
  "img-src 'self' data: blob:; frame-ancestors 'none'; " +
  "require-trusted-types-for 'script'; trusted-types 'none'";

/** How long the proxy waits on a silent streamer before it gives up on a request or an
 *  upgrade handshake. An upgraded session has no idle limit: an idle app sends nothing. */
const DEFAULT_UPSTREAM_TIMEOUT_MS = 10_000;

/** How long a refused upgrade socket may stay half-open after its reply was sent. */
const REFUSAL_LINGER_MS = 2_000;

/** The request headers the upgrade forwards to the streamer; Host is set by the proxy. No
 *  Cookie, no Authorization, no Origin: the streamer needs none of them, and it is not
 *  trusted with them. */
const UPGRADE_REQUEST_HEADERS = [
  'upgrade',
  'connection',
  'sec-websocket-key',
  'sec-websocket-version',
  'sec-websocket-protocol',
  'sec-websocket-extensions',
] as const;

/** The streamer's 101 headers the proxy relays to the browser. Nothing else: no Set-Cookie,
 *  no Location, no Refresh. */
const UPGRADE_RESPONSE_HEADERS = [
  'upgrade',
  'connection',
  'sec-websocket-accept',
  'sec-websocket-protocol',
  'sec-websocket-extensions',
] as const;

/** The Host values the session endpoint answers to: loopback names, any port. The page is
 *  published on the host's loopback only (compose.yml), so these are the only names a
 *  browser uses for it. A DNS-rebound name is not among them. */
const LOOPBACK_HOST = /^(?:127\.0\.0\.1|localhost|\[::1\])(?::\d{1,5})?$/i;

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
  /** How long a proxied request or upgrade handshake may wait on the streamer. Default 10 s. */
  upstreamTimeoutMs?: number;
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

/** The percent-decoded pathname of a request URL, or null when it does not parse or does not
 * decode. The URL parser collapses literal '/../' before this returns, and `resolveStatic`
 * rejects the percent-encoded variant, so the two traversal spellings are both covered. The
 * parse is inside the try: an absolute-form target with a bad authority ('http://a:99999/')
 * makes the URL constructor throw, and a throw here would escape the request listener and
 * take the whole server down. */
export function decodePathname(rawUrl: string | undefined): string | null {
  try {
    const parsed = new URL(rawUrl ?? '/', 'http://demo.invalid');
    return decodeURIComponent(parsed.pathname);
  } catch {
    return null;
  }
}

function contentTypeFor(file: string): string {
  return CONTENT_TYPES[extname(file)] ?? 'application/octet-stream';
}

/**
 * Splits a prefix-stripped path into safe segments: no '..', no '.', no empty segment, no NUL,
 * and no backslash or colon, which a Windows path module would read as a separator or a drive
 * ('%5C..%5C' decodes to a single '..\..' segment). Returns null when the path is not a plain
 * name under the root.
 */
function safeSegments(sub: string): string[] | null {
  const parts = sub.split('/').filter((part) => part.length > 0 && part !== '.');
  for (const part of parts) {
    if (part === '..' || part.includes('\0') || part.includes('\\') || part.includes(':')) {
      return null;
    }
  }
  return parts;
}

/** True when `file` resolves inside `root`: the last guard before a file is served, whatever
 * the segments looked like. */
function isInside(root: string, file: string): boolean {
  return resolve(file).startsWith(resolve(root) + sep);
}

/** Files under /dist/ that are not the page's: the server's own module is node-only and is
 * never served to a browser. */
const DIST_PRIVATE = new Set(['server.js', 'server.js.map', 'server.d.ts']);

/**
 * Maps a pathname to a file under the roots, or null when it maps nowhere. Two behaviours a
 * browser needs and a bundler normally provides:
 *
 * - The TS-ESM extension probe: the demo's own modules are built with bundler resolution, so
 *   `tsc` emits './wm/popups' with no extension, and a browser 404s that. (The client's and
 *   the React bindings' dist name every `.js` file, NodeNext-built, and hit on disk directly.)
 *   A path with no extension that misses on disk is retried once with '.js'.
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
    const root = roots[rootKey];
    const direct = join(root, ...segments);
    const isPrivate = (file: string): boolean =>
      rootKey === 'demoDist' && segments.length === 1 && DIST_PRIVATE.has(basename(file));
    if (existsSync(direct) && isInside(root, direct) && !isPrivate(direct)) {
      return { file: direct, contentType: contentTypeFor(direct) };
    }
    const base = segments.at(-1);
    if (base !== undefined && !base.includes('.')) {
      const probed = `${direct}.js`;
      if (existsSync(probed) && isInside(root, probed) && !isPrivate(probed)) {
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
  extraHeaders: Readonly<Record<string, string>> = {},
): void {
  res.writeHead(status, {
    ...extraHeaders,
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
 *  its tests, which mock '@appricot/client' by name — on the workspace name.
 *
 *  Anchored to module syntax: the quoted name is rewritten only right after `from` (an
 *  import or an `export … from`) or after `import` (a side-effect import, or a dynamic
 *  `import(…)`). The same string as data elsewhere in a module stays what it is. A template
 *  literal is never rewritten; the lint rules allow literal specifiers only. */
const CLIENT_SPECIFIER = /(\bfrom\s*|\bimport\s*\(?\s*)(['"])@appricot\/client\2/g;

/** Sends one of the page's own modules (/dist/*.js) with its bare client specifier
 *  rewritten to the served URL. Plain text in, plain text out; HEAD gets no body. A file
 *  that cannot be read (removed by a rebuild after it was resolved, say) is a 404, never a
 *  throw out of the request listener. */
function sendDistModule(res: ServerResponse, resolved: ResolvedStatic, method: string): void {
  let source: string;
  try {
    source = readFileSync(resolved.file, 'utf8');
  } catch {
    sendText(res, 404, 'not found', method);
    return;
  }
  const body = source.replace(CLIENT_SPECIFIER, '$1$2/client/index.js$2');
  res.writeHead(200, {
    ...securityHeaders(),
    'content-type': resolved.contentType,
    'content-length': String(Buffer.byteLength(body)),
  });
  res.end(method === 'HEAD' ? undefined : body);
}

/**
 * True when an upgrade comes from the demo page itself: the Host is a loopback name (so a
 * DNS-rebound name is refused) and the Origin is exactly that host's origin (so another site
 * the developer visits cannot open the session: cross-site WebSocket hijacking). Browsers
 * always send Origin on a WebSocket handshake; a request without one is refused too.
 */
export function isPageOrigin(headers: IncomingHttpHeaders): boolean {
  const host = headers.host;
  const origin = headers.origin;
  if (host === undefined || origin === undefined || !LOOPBACK_HOST.test(host)) {
    return false;
  }
  const lowerHost = host.toLowerCase();
  const lowerOrigin = origin.toLowerCase();
  return lowerOrigin === `http://${lowerHost}` || lowerOrigin === `https://${lowerHost}`;
}

/** Forwards one plain HTTP request (the readiness probe) to the streamer and relays its
 * status and body. Only Host goes upstream, and none of the reply's headers come back: the
 * body is served as text/plain under the security headers, so the streamer can set no cookie,
 * redirect nothing, and never make its bytes a same-origin script. The request rides a
 * no-keep-alive agent: the default global agent pools connections (Node 19+), and a pooled
 * proxy socket would keep the streamer's server from ever closing in tests. */
function proxyRequest(
  req: IncomingMessage,
  res: ServerResponse,
  target: ProxyTarget,
  agent: Agent,
  timeoutMs: number,
): void {
  const method = req.method ?? 'GET';
  let timedOut = false;
  const upstream = httpRequest(
    {
      hostname: target.host,
      port: target.port,
      path: req.url,
      method,
      headers: { host: `${target.host}:${target.port}` },
      agent,
    },
    (upRes) => {
      // A reply cut short (the streamer dies, or the browser left and took the request
      // down) errors here; pipe() adds no listener of its own, and an unhandled one would
      // take the server down.
      upRes.on('error', () => {
        res.destroy();
      });
      res.writeHead(upRes.statusCode ?? 502, {
        ...securityHeaders(),
        'content-type': 'text/plain; charset=utf-8',
      });
      upRes.pipe(res);
    },
  );
  // A silent streamer does not hold the browser's request forever.
  upstream.setTimeout(timeoutMs, () => {
    timedOut = true;
    upstream.destroy();
  });
  upstream.on('error', () => {
    if (res.destroyed || res.writableEnded) {
      return;
    }
    if (!res.headersSent) {
      sendText(res, timedOut ? 504 : 502, 'upstream unavailable', method);
    } else {
      res.destroy();
    }
  });
  // A browser that goes away takes the upstream request with it.
  res.on('close', () => {
    upstream.destroy();
  });
  req.pipe(upstream);
}

/**
 * One proxied WebSocket upgrade, written raw so our own 101 carries the CSP. Allowlisted
 * headers only, both ways (UPGRADE_REQUEST_HEADERS, UPGRADE_RESPONSE_HEADERS). The caller has
 * already put an error listener on the browser's socket.
 */
function proxyUpgrade(
  req: IncomingMessage,
  clientSocket: Duplex,
  head: Buffer,
  target: ProxyTarget,
  agent: Agent,
  onLog: (line: string) => void,
  timeoutMs: number,
): void {
  const headers: Record<string, string> = { host: `${target.host}:${target.port}` };
  for (const name of UPGRADE_REQUEST_HEADERS) {
    const value = req.headers[name];
    if (typeof value === 'string') {
      headers[name] = value;
    }
  }
  let timedOut = false;
  let answered = false;
  let upstreamSocket: Duplex | null = null;
  const upstream = httpRequest({
    hostname: target.host,
    port: target.port,
    path: req.url,
    method: 'GET',
    headers,
    agent,
  });
  // The browser leaving at any point ends the upstream side too, handshake pending or not.
  clientSocket.on('close', () => {
    upstream.destroy();
    upstreamSocket?.destroy();
  });
  // A browser that half-closes before the handshake is answered has given up. The server's
  // sockets allow half-open, so no 'close' would follow on its own.
  clientSocket.on('end', () => {
    if (!answered) {
      clientSocket.destroy();
    }
  });
  // A streamer that accepts TCP and then says nothing does not hold the browser forever.
  upstream.setTimeout(timeoutMs, () => {
    timedOut = true;
    upstream.destroy();
  });
  upstream.on('upgrade', (upRes, upSocket, upHead) => {
    answered = true;
    upstreamSocket = upSocket;
    // The handshake timeout ends here: an idle session sends nothing and must stay up.
    upSocket.setTimeout(0);
    upSocket.on('error', () => {
      upSocket.destroy();
      clientSocket.destroy();
    });
    if (clientSocket.destroyed) {
      upSocket.destroy();
      return;
    }
    const lines = ['HTTP/1.1 101 Switching Protocols'];
    for (const name of UPGRADE_RESPONSE_HEADERS) {
      const value = upRes.headers[name];
      if (typeof value === 'string') {
        lines.push(`${name}: ${value}`);
      }
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
    upSocket.on('close', () => clientSocket.destroy());
    onLog('session websocket proxied');
  });
  // The streamer answered the upgrade with a plain response: relay its status, then end both
  // sides — nothing sensible can follow a refused upgrade. Only the status crosses: the
  // reason phrase, the headers and the body are the proxy's own. The status is logged
  // because the streamer itself is silent per connection: a refused upgrade is the only
  // place the demo's log shows what the session endpoint said (measured 2026-09-21: this
  // line was the missing eye on the one-shot 503s).
  upstream.on('response', (upRes) => {
    answered = true;
    const status = upRes.statusCode ?? 502;
    onLog(`session websocket upgrade refused with ${status}`);
    upRes.on('error', () => undefined); // a body cut short changes nothing: it is dropped
    upRes.resume();
    refuseUpgrade(clientSocket, status, STATUS_CODES[status] ?? 'Refused');
  });
  upstream.on('error', () => {
    if (answered || clientSocket.destroyed) {
      clientSocket.destroy();
      return;
    }
    onLog(`session websocket upgrade failed: ${timedOut ? 'streamer timed out' : 'streamer unavailable'}`);
    refuseUpgrade(clientSocket, timedOut ? 504 : 502, timedOut ? 'Gateway Timeout' : 'Bad Gateway');
  });
  upstream.end();
}

/** Writes a short raw response on an upgrade socket that will not be proxied, then ends it.
 * end(), not write-then-destroy: a destroy can drop the reply before it is flushed. A peer
 * that never closes its side is destroyed after a short linger. */
function refuseUpgrade(clientSocket: Duplex, status: number, reason: string): void {
  if (clientSocket.destroyed) {
    return;
  }
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
  clientSocket.end(headers.join('\r\n') + body);
  const linger = setTimeout(() => {
    clientSocket.destroy();
  }, REFUSAL_LINGER_MS);
  linger.unref();
  clientSocket.once('close', () => {
    clearTimeout(linger);
  });
}

/** Builds the demo server: request handler, upgrade handler, tracked sockets. */
export function createDemoServer(options: DemoServerOptions = {}): Server {
  const roots = options.roots ?? defaultRoots();
  const target = options.upstream ?? { host: '127.0.0.1', port: 8391 };
  const onLog = options.onLog ?? ((): void => undefined);
  const timeoutMs = options.upstreamTimeoutMs ?? DEFAULT_UPSTREAM_TIMEOUT_MS;
  const proxyAgent = new Agent({ keepAlive: false });

  const server = createServer((req, res) => {
    const pathname = decodePathname(req.url);
    if (pathname === null) {
      sendText(res, 400, 'bad request path', req.method ?? 'GET');
      return;
    }
    if (pathname === '/session') {
      // The session endpoint speaks WebSocket only; a plain request is not proxied at all.
      sendText(res, 426, 'upgrade required', req.method ?? 'GET', {
        upgrade: 'websocket',
        connection: 'Upgrade',
      });
      return;
    }
    if (pathname === '/readyz') {
      proxyRequest(req, res, target, proxyAgent, timeoutMs);
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

  server.on('upgrade', (req, clientSocket: Duplex, head: Buffer) => {
    // First thing: node's server removes its own socket error handler before it emits
    // 'upgrade', so without this a browser reset at any later point (the handshake still
    // pending upstream, say) is an unhandled 'error' that ends the process.
    clientSocket.on('error', () => {
      clientSocket.destroy();
    });
    const pathname = decodePathname(req.url);
    if (pathname === '/session') {
      if (!isPageOrigin(req.headers)) {
        onLog('session websocket upgrade refused: not from the page on a loopback origin');
        refuseUpgrade(clientSocket, 403, 'Forbidden');
        return;
      }
      proxyUpgrade(req, clientSocket, head, target, proxyAgent, onLog, timeoutMs);
      return;
    }
    refuseUpgrade(clientSocket, 404, 'Not Found');
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
