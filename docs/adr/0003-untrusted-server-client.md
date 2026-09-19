# ADR-0003 — The client treats the server as untrusted

**Status**: Proposed (2026-09-19)

## Context

The browser client, `@appricot/client`, runs inside the host app's page: the host's origin, its
JavaScript realm, its cookies and its users' keystrokes. The bytes it parses come from a streamer
that shares a sandbox with the streamed application. That application may be hostile. The pilot
application is an administration tool that parses the protocol of whatever remote device it
connects to, so a hostile device that exploits the application or its toolkit owns the session. A
compromised application can attack the streamer next to it, so this ADR assumes the whole server
side is hostile.

Two public web clients were read for this decision, and both turned server-sent strings into
markup.

1. **A server string reached `innerHTML`.** The noVNC client shipped with KasmVNC 1.4.0 (its
   `kasmweb` submodule at `v1.4.0` is noVNC `ae1e012f`,
   [GitHub API](https://api.github.com/repos/kasmtech/KasmVNC/contents/kasmweb?ref=v1.4.0),
   checked 2026-09-19) writes the fields of server message 178 into `innerHTML` with no escaping
   ([kasmtech/noVNC@ae1e012f `app/ui.js:1384`](https://github.com/kasmtech/noVNC/blob/ae1e012f/app/ui.js#L1384),
   checked 2026-09-19). The listener is attached on every connect. A fake server needs only an
   RFB handshake and one type-178 message carrying `<img src=x onerror=...>` to run script in
   the page that frames the client (traced in source, not executed). In the deployment that was
   reviewed, that page was same-origin with the host application's own single-page app.
   kasmtech/noVNC's `master` now uses `textContent` at that spot
   ([`app/ui.js`](https://github.com/kasmtech/noVNC/blob/master/app/ui.js), checked 2026-09-19).
   The fix came from upstream later; a host pinned to the older client still had the hole.
2. **The same class of sink in the next client studied.** xpra-html5 writes a window title with
   jQuery `.html()` ([`html5/js/Window.js:653`](https://github.com/Xpra-org/xpra-html5/blob/master/html5/js/Window.js#L653),
   master, checked 2026-09-19). In an internal benchmark (September 2026) a window title was set
   to markup and the stock client executed it; that test was not re-run here.

One answer to the first finding, taken in a shipped deployment, was to serve a pinned, patched
copy of the third-party client under a strict CSP. It works, but it is a policy boundary that has
to be re-proven on every upgrade of a client somebody else writes.

A per-window design adds a risk that an iframe did not have. The iframe kept every pixel, click
and key of a session inside one box. Per-window surfaces are drawn straight into the host page. A
hostile server could map a popup the size of the viewport, draw a fake of the host's own chrome or
a login prompt, take clicks meant for the host, or ask for focus. The first draft of this design
had that flaw.

## Decision (proposed)

The client treats every byte from the server as hostile input. It never turns server data into
markup, script, navigation or a URL. It confines server pixels to the boxes the host gives it,
and the host alone decides focus and stacking.

### 1. Strings are text

- Every string from the server (titles, app ids, class names, error messages, clipboard text) is
  exposed to the host as a plain JavaScript string, and documented as untrusted.
- The client never writes a server string with `innerHTML`, `outerHTML`, `insertAdjacentHTML`,
  `document.write`, `setAttribute` on an event handler or URL attribute, or a CSS property.
  React bindings render strings as children, never through `dangerouslySetInnerHTML`.
- Strings are UTF-8, validated, and capped per field on the wire. Control characters are stripped
  or rejected by the decoder; the rule is set in the v0 spec.

### 2. No script and no navigation from the server

- No `eval`, `new Function`, string `setTimeout`/`setInterval`, dynamic `import()` of a computed
  specifier, or `Worker` from a computed URL. Workers load from the client's own bundle.
- The protocol has **no message** that carries a URL, a link, a redirect or a "open this"
  request. The client never calls `location`, `window.open`, `history` or `postMessage` because of
  server data.
- Images the server sends (window icons, the cursor) arrive as pixels. The client draws them into
  a canvas and, for the cursor, turns them into a `blob:` URL that the client itself created. A
  server string never reaches `src`, `href`, `url(...)` or `srcdoc`.

### 3. A strict CSP must be possible

- The client must run in a page whose `script-src` has neither `'unsafe-inline'` nor
  `'unsafe-eval'`. Without `'unsafe-eval'`, the browser blocks `eval()`
  ([MDN, script-src](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Security-Policy/script-src),
  checked 2026-09-19).
- The client must also run under `require-trusted-types-for 'script'`, which "disallows using
  strings with DOM XSS injection sink functions"; MDN marks it Baseline since February 2026
  ([MDN](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Security-Policy/require-trusted-types-for),
  checked 2026-09-19). The client needs no Trusted Types policy because it uses no sink.
- The demo host page and the tests run under that CSP. The docs give a recommended CSP for hosts.
  The host owns its CSP; the client cannot enforce one.

### 4. Every size and count is bounded

- The v0 spec has a limits table: message size, string lengths, surface count, popup count per
  parent, rectangles per frame, tile dimensions, total pixels in flight, cursor and icon size,
  clipboard size. The decoder checks each length **before** it allocates. A violation closes the
  connection with a local error; it never throws into the host.
- Tile decoding uses the browser's image decoders (`createImageBitmap` accepts a `Blob`,
  [MDN](https://developer.mozilla.org/en-US/docs/Web/API/Window/createImageBitmap), and workers
  have their own
  [`WorkerGlobalScope.createImageBitmap()`](https://developer.mozilla.org/en-US/docs/Web/API/WorkerGlobalScope/createImageBitmap),
  both checked 2026-09-19) or our own bounds-checked decoder in a Worker. Every decoder that runs in
  the host realm has fuzz tests.
- The node proxy caps frame sizes in both directions too, so a hostile streamer cannot make the
  proxy reserve large buffers. A relay reviewed for this design had exactly that gap.

### 5. Pixels stay inside host-given boxes

- The client draws only into canvases the host attached for a surface. It creates no overlay of
  its own over the page.
- A popup is clamped to its parent's content box plus a small fixed margin, and its size is
  capped. A popup whose parent is gone is dropped.
- The host draws all chrome. The recommended host pattern puts a host-drawn edge or label around
  every streamed surface, naming the application and the session, which the server cannot paint
  over.
- A server request to raise, focus, move or resize a window is an **event** to the host. The host
  decides. The client never raises or focuses a surface by itself.

### 6. Input goes only where the user put it

- Keys go only to the surface the host marks as focused, and only while the host page has focus.
  Keys typed for the host never reach a session.
- On blur the client releases every held key and button.
- Browser shortcuts the client cannot capture stay the browser's.

### 7. The clipboard is the host's decision

- Server-to-user: the client exposes clipboard text as an event. It writes to the user's
  clipboard only if the host's policy allows it, and only inside a user gesture.
- User-to-server: the client sends pasted text only if the host's policy allows it. It reads the
  clipboard only from a `paste` event, never on its own.
- The streamer and the node apply the same policy, but the client is the enforcement point that a
  compromised application cannot reach. In the deployment reviewed, a server-side clipboard switch
  could be flipped from inside the session itself.

### 8. Guard rails in the build

- ESLint rules in `packages/` forbid the sinks in §1-§2 (`innerHTML`, `outerHTML`,
  `insertAdjacentHTML`, `document.write`, `eval`, `Function`, `dangerouslySetInnerHTML`, string
  timers). The tooling unit wires the rules; this ADR is their reason.
- A hostile fixture server (every string is markup, every length is at its limit or past it, a
  full-screen popup, a focus request storm) runs in CI against the client (roadmap, M2).

## Consequences

- The host page stays safe from script injection by a compromised session, as long as the client
  holds these rules. The realm risk does not vanish; it moves into our own parser of hostile
  bytes, which must stay small, bounded and fuzzed.
- A compromised application can still draw anything **inside** its own windows, including a fake
  login prompt. The host's labelled chrome is the only answer to that; it is a UX control, not a
  technical one.
- Some features are off the table: rich clipboard (HTML, images), server-driven links ("open
  this URL"), app-drawn window decorations outside the content box, and server-chosen focus.
- Popups larger than the margin allows are clipped. How often a real application needs more is
  measured in the M1 spike. In the internal benchmark (September 2026) the pilot application drew
  its menus and its settings panel inside its main window, with no extra X windows for them, so
  the clamp looked cheap for that profile.
- Host apps must attach a label to every streamed window if they want the anti-spoofing
  property. The label is host chrome, so `@appricot/react` does not draw it. The bindings should
  make it hard to leave out, for example by handing the host the application and session name to
  show; the shape is decided in `l1-client-mvp`.

## Alternatives considered

- **Trust the server, as the status quo did.** Rejected. It is exactly what produced the type-178
  finding.
- **Serve a pinned, patched third-party client under a CSP.** It closes the known sinks for one
  client version, and it is what a shipped deployment did. Rejected here because APPricot writes
  its own client, so there is nothing third-party to pin, and because the patch must be re-proven
  on every upstream upgrade.
- **Isolate the client in a cross-origin sandboxed iframe.** A hard origin boundary, but it breaks
  the product: the host could no longer draw its own chrome around each window, and each window
  would be a separate frame to keep in sync. The cross-origin route was priced in the same review
  and needs per-session host names, an origin allow-list, `__Host-` cookies and a wildcard
  certificate. Kept as a possible host-side choice for high-risk hosts, not the default.
- **Run the decoder in WebAssembly compiled from the Rust codec.** Possible later for speed. It
  does not change the rules above: the WASM module still runs in the host realm and still needs
  bounds and fuzzing.
