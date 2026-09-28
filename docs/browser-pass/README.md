# The real-browser pass: the rig

The method behind [findings-2026-09-28.md](findings-2026-09-28.md), which is the browser half of
M2's exit. The device half is equipment and is not this rig's business.

## What it drives

The demo host page from the `demo` recipe ([justfile](../../justfile), "the demo host page"), with
the recipe itself unchanged. What changes is the invocation around it, because two things the
pass needs are not in the recipe's default:

- the display's keyboard layout, which the rig sets to `us,gr` before the streamer starts, so a
  Greek character has a keymap that holds it (see the findings' observation 2);
- a cursor on the display, so there is one for the streamer to report.

```sh
# once: the two X tools the dev image does not carry, extracted without running dpkg's scripts
# (an `apt-get install` in the demo container left the streamer reporting a dead display twice)
docker compose run --rm -u root -v appricot-xtools:/xtools dev bash -c \
  'cd /tmp && apt-get update -qq && apt-get download xclip x11-xserver-utils \
   && for d in *.deb; do dpkg-deb -x "$d" /xtools; done'

# the demo, with the layout and the cursor set first; nothing else about the recipe changes
docker compose run --rm -v appricot-xtools:/xtools \
  -p "127.0.0.1:${APPRICOT_DEMO_HOST_PORT:-8390}:8390" dev bash -c \
  'setxkbmap -display :99 us,gr && /xtools/usr/bin/xsetroot -cursor_name hand2 && exec just demo'
```

The streamer serves **one session per process** (`docs/protocol/v0.md` §7), and the recipe stops
the server when the streamer exits, so one browser is one demo run: start the recipe, drive one
browser to the end, read the evidence, and start it again for the next browser.

## The browser, and the route to it

The browsers are Playwright's, in its container image (`mcr.microsoft.com/playwright:v1.63.0-noble`,
<https://playwright.dev/docs/docker>, checked 2026-09-28): Chromium 153.0.8010.12 and Firefox
155.0, both headless, both in a container, never on the host.

The browser container shares the demo container's **network namespace**
(`docker run --network container:<demo>`), so it reaches the page at `http://127.0.0.1:8390`
exactly as the recipe intends. That is not a preference: `packages/demo/src/server.ts` answers the
session endpoint only to a loopback `Host` and an `Origin` equal to it, so a browser that reaches
the demo by container IP on the compose network is refused at the WebSocket upgrade. Sharing the
namespace keeps the recipe's loopback publish, its `Host` rule and its `Origin` rule exactly as
they are, and needs no proxy that rewrites either.

## The rig's files

| File | What it is |
|---|---|
| [pass.mjs](../../scripts/browser-pass/pass.mjs) | the pass, one browser per run: navigate and read the CSP, the token check both ways, the window inventory, the idle and ack counts, focus, drag, resize, minimise/restore, close, popups, input, both clipboard directions, and the cursor step last |
| [input-probe.mjs](../../scripts/browser-pass/input-probe.mjs) | the input criterion alone, so its evidence fits in one file |
| [orchestrate.sh](../../scripts/browser-pass/orchestrate.sh) | answers the pass's hand-offs from the development host: starts the stand-in app, moves its popup, fronts the CLIPBOARD selection, reads it back, changes the cursor. Each hand-off must appear *after* the previous one, so a log left by an earlier run cannot answer one early |
| [x-observer.sh](../../scripts/browser-pass/x-observer.sh) | on the display: the window inventory, geometry and input focus every 300 ms, plus the X client's key events, into the run's own log |
| [move-popup.sh](../../scripts/browser-pass/move-popup.sh) | moves the stand-in app's override-redirect popup window outside its parent, so the pass can see whether the host holds it in place |

Run it in one shell:

```sh
# start the demo (the command above), then in the demo container:
#   the observer, xlogo and xev, each on the same display
# then:
docker exec -e APPRICOT_DEMO_TOKEN=<the token the recipe printed> \
  -e BROWSER=chromium appricot-browser-pass bash -c 'cd /driver && node driver.mjs'
# and, from the host, alongside it:
sh scripts/browser-pass/orchestrate.sh <the run log> <the demo container> \
  /work/artifacts/browser-pass/out <the steps directory>
```

A run writes its log, its screenshots and a JSON record under `artifacts/browser-pass/out/`
(gitignored); the X observer's log and the browser's console log are part of that record.

## What the rig does not do

- **No Safari.** There is no macOS here, so the third desktop browser in the criterion cannot run.
- **Firefox cannot be given a Greek letter, an AltGr character or a dead key by this rig.**
  Playwright's Firefox refuses a key name outside its layout table (`Unknown key: "α"`), and no
  rig-side workaround was used: a synthetic DOM event would not be the browser's own key input.
  The client-to-streamer path for those characters is exercised in Chromium, and the backend's own
  test (`crates/appricot-x11/tests/keysym_groups.rs`) owns the mapping across the `us`, `gr` and
  `us,gr` layouts.
- **No host-to-app paste in Firefox.** A synthesized `Ctrl+V` does not fill the `paste` event's
  data there, so the client sends no `clipboard_set` and the direction stays unexercised.
- **Nothing that needs a device.** The demo streams the image's own X clients and the repository's
  stand-in application; the pilot application on a real device is the other half of M2's tail.
