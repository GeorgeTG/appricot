# @app-ricot/react

<img src="https://github.com/GeorgeTG/appricot/raw/main/assets/branding/appricot-banner-1600x500.png"
     alt="APPricot banner: the apricot mark with an application window inside its ring. Tagline:
          per-window application streaming — sandboxed sessions, damage-driven pixels, an
          embeddable browser client."
     width="840">

React bindings for
[`@app-ricot/client`](https://www.npmjs.com/package/@app-ricot/client): a provider
that owns the connection and the window registry, hooks over the registry, a canvas
component per streamed window, and a text-only title. An
[appricot-streamer](https://github.com/GeorgeTG/appricot) runs beside a desktop
application and streams **each of its windows**; these bindings put each one inside
your React tree.

**You render the chrome; these bindings render none.** Each streamed window is your
floating window — your title bar, your focus and stacking decisions — and
`AppricotSurface` fills its canvas with that window's pixels. Titles are rendered as
text only: every string the server sends is text, never markup.

## Install

```sh
pnpm add @app-ricot/react @app-ricot/client
```

Peer dependency: React 19. The registry resolves the pair — both packages publish at
the same version, and one add of each installs a matching pair.

## A host in twenty lines

```tsx
import {
  AppricotProvider,
  AppricotSurface,
  AppricotTitle,
  useConnectionStatus,
  useWindows,
} from '@app-ricot/react';

function HostWindows({ focusedId }: { focusedId: number }) {
  const status = useConnectionStatus();   // 'connecting' | 'open' | 'reconnecting' | …
  const windows = useWindows();           // the app's toplevel windows, as records
  return (
    <div data-status={status}>
      {windows.map((window) => (
        <div className="window" key={window.id}>
          <header><AppricotTitle id={window.id} /></header>
          {/* give the canvas a CSS size; focus follows your host's decision */}
          <AppricotSurface id={window.id} focused={window.id === focusedId} />
        </div>
      ))}
    </div>
  );
}

// The token is the per-session stream token your server mints; it never
// travels in the URL.
export function App({ url, token, focusedId }: {
  url: string; token: string; focusedId: number;
}) {
  return (
    <AppricotProvider url={url} token={token} reconnect>
      <HostWindows focusedId={focusedId} />
    </AppricotProvider>
  );
}
```

What each piece decides:

- `AppricotProvider` owns the connection (`url`, `token`, `reconnect`) and the
  surface registry; everything below it re-attaches on resume.
- `AppricotSurface` draws one surface's pixels and carries input back. The host
  decides **size** and **focus**: pass `focused` for the focused surface (it sends
  the one message that moves focus), and either let `autoConfigure` propose the
  host canvas's size to the app or answer resize asks yourself through
  `onResizeAsk`.
- `AppricotTitle` is the text-only title — the proof of pattern for treating server
  strings as data.
- The hooks — `useWindows`, `useSurfaceMeta`, `useConnectionStatus`,
  `useSessionEvents` — read the same registry the provider feeds.

The full window contract (who decides size, position, stacking, popups) is
[ADR-0003 §5 in the repository](https://github.com/GeorgeTG/appricot/blob/main/docs/adr/0003-untrusted-server-client.md),
and the repository's demo host shows a complete integration, floating windows
included.

## Status

`0.1.x` — the wire protocol is v0 and may still move.

## Licence

Dual-licensed `MIT OR Apache-2.0`, like the rest of APPricot — see
[LICENSE-MIT](./LICENSE-MIT) and [LICENSE-APACHE](./LICENSE-APACHE), or the texts in
[the repository](https://github.com/GeorgeTG/appricot).
