# @app-ricot/client

<img src="https://github.com/GeorgeTG/appricot/raw/main/assets/branding/appricot-banner-1600x500.png"
     alt="APPricot banner: the apricot mark with an application window inside its ring. Tagline:
          per-window application streaming — sandboxed sessions, damage-driven pixels, an
          embeddable browser client."
     width="840">

APPricot's embeddable browser client: the TypeScript half of a per-window application
stream. An [appricot-streamer](https://github.com/GeorgeTG/appricot) runs beside a
desktop application and streams **each of its windows** over a WebSocket; this client
decodes the stream into canvases your host provides. It is framework-agnostic — the
React bindings live in
[`@app-ricot/react`](https://www.npmjs.com/package/@app-ricot/react).

The host draws the chrome. This client draws pixels, and only into the canvases the
host attaches — it never touches the rest of the DOM. Every string the server sends
(titles, app ids, errors) is **text to render, never markup or script**: the server
shares the streamed application's sandbox and is treated as untrusted
([ADR-0003](https://github.com/GeorgeTG/appricot/blob/main/docs/adr/0003-untrusted-server-client.md)).

## Install

```sh
pnpm add @app-ricot/client
```

Zero runtime dependencies. The server half is the streamer image:

```dockerfile
FROM ghcr.io/georgetg/appricot/streamer:<version>
```

built per tag by [the APPricot repository](https://github.com/GeorgeTG/appricot) —
see its README, "Consuming APPricot", for what a host must supply (the per-session
stream token, the proxy leg, the window chrome).

## What is inside

- **Connection** — `connectAppricot(url, { token, reconnect })` over a WebSocket, or
  `AppricotConnection` over your own `Transport`. The per-session token travels in
  the Hello message, never in the URL. Reconnect resumes a parked session inside the
  server's grace window.
- **Windows, not a screen** — `SurfaceRegistry` keeps one record per streamed window:
  toplevels and popups, titles, app ids, activation, per-surface scale. Popups are
  placed by positioner and clamped (`placePopup`, `clampPopup`).
- **Rendering** — `SurfaceRenderer` decodes tile frames into the canvases you attach
  and acks what it drew; frames flow only as fast as the acks.
- **Input** — `attachInput` maps keyboard, pointer and wheel events onto the focused
  surface, including keysyms per layout (`keysymFor`, `modifiersFor`); `isPasteChord`
  leaves clipboard policy with the host.
- **Cursor** — `cursorToImageData` / `drawCursor` turn the server's cursor image into
  an overlay your chrome composites.
- **Codec mirror** — the whole v0 wire codec is re-exported, pinned byte-exact
  against the Rust implementation's test vectors.

The wire contract itself is
[documented in the repository](https://github.com/GeorgeTG/appricot/blob/main/docs/protocol/README.md).

## Status

`0.1.x` — the wire protocol is v0 and may still move. Every size and count on the
wire is bounded before anything is allocated; the client runs under a strict CSP
(no `unsafe-inline`, no `unsafe-eval`) in the repository's own demo host.

## Licence

Dual-licensed `MIT OR Apache-2.0`, like the rest of APPricot — see
[LICENSE-MIT](./LICENSE-MIT) and [LICENSE-APACHE](./LICENSE-APACHE), or the texts in
[the repository](https://github.com/GeorgeTG/appricot).
