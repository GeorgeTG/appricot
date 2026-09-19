# Vision

## The problem

Many useful applications exist only as desktop programs. Some are legacy. Some are current but
ship for the desktop only: a vendor's configuration tool, a lab instrument's console, an internal
line-of-business program.

Take the case this project was built for. A **legacy X11 administration tool** — a Qt6/xcb desktop
program with no web version — is the supported way to operate a class of devices. A web product
wants to offer that tool to its own users, inside its own UI, with nothing to install. Four roads
exist today, and all four are bad.

### 1. Ask the user to install it

That fails on an iPad, a Chromebook or a locked-down laptop. It also moves the program, and
everything it parses, onto the user's own machine — the machine you least want parsing input
from a device that may be hostile.

### 2. Stream a whole remote desktop into the page

VNC, RDP, a WebRTC desktop streamer. The application then lives in one canvas with its own desktop
inside it. Its windows cannot be moved, stacked or minimised by the web product. A second window
hides the first. The web product also has to host a third-party web client, which may turn server
data into markup.

This road has been walked, and the bill is known:

- **One canvas for every window of the application.** An instruction like "close the login window
  first" cannot be carried out, because the window that must be closed is behind the one on top and
  the host UI has no way to raise it.
- **The embedded web client is a markup sink.** KasmVNC 1.4.0 pins its `kasmweb` submodule at
  [kasmtech/noVNC@ae1e012f](https://api.github.com/repos/kasmtech/KasmVNC/contents/kasmweb?ref=v1.4.0),
  and that commit writes a server-sent string into `innerHTML`
  ([`app/ui.js:1384`](https://github.com/kasmtech/noVNC/blob/ae1e012f/app/ui.js#L1384)); both
  checked 2026-09-19. Embedding it hands the host page that sink.
- **A GPL-licensed web client to patch and maintain.** KasmVNC's
  [`LICENSE.TXT`](https://raw.githubusercontent.com/kasmtech/KasmVNC/master/LICENSE.TXT) is GPL
  version 2 (checked 2026-09-19). A modified copy is a modified GPL work, with everything that
  follows for a product that ships it.

### 3. Forward the display protocol itself

X forwarding over SSH, or any scheme that carries a raw display protocol to the client. It gives
per-window output for free, and it does not survive a WAN. From an internal benchmark
(September 2026; a Qt6/xcb application on Debian 12 under Docker, no GPU, remote X over SSH):

| What happens | What it costs |
|---|---|
| A blinking text caret, nothing else, default GL path | **55-58 Mbit/s**, continuously, while a field has focus |
| One keystroke | **3.36 MB** — the whole 1200x700 window is re-sent per character |
| The connection drops | **the application exits** — the toolkit quits when its display connection breaks |

Those three rows are the case for a purpose-built protocol. Output must be **damage-driven** (an
idle window sends nothing), **encoded** (a repaint is compressed, not a raw pixel dump), and
**per-window** with **flow control** (a fixed number of frames in flight, damage coalesced while
waiting). And the session must outlive the transport: the application keeps running inside its
container when a browser goes away, so a client can reconnect to a session that is still there.

### 4. Rewrite the application for the web

Usually impossible — closed protocols, closed source — or far too expensive.

## Another road

APPricot streams **each window** of a sandboxed application as a separate stream. The web product
shows each one as **its own** window, with its own chrome, in its own UI. The application runs in a
container the web product's operator controls. The browser receives pixels and window metadata,
never a page.

## Who it is for

- **Teams building a web product that must include a desktop-only tool.** Network and device
  management, vendors' configuration tools, lab and instrument software, internal line-of-business
  programs.
- **Their end users on browser-only devices.** An iPad, a ChromeOS tablet, a kiosk, a managed
  laptop where nothing may be installed.
- **Operators who must contain what the application touches.** The thing being administered feeds
  the tool its input and may be hostile. The tool should run in a sandbox with scoped egress, not
  on the user's desktop.

## What it is not

- **Not a VDI desktop.** There is no desktop, no taskbar and no wallpaper on the server. APPricot
  streams application windows. The host web UI is the desktop.
- **Not a game streamer.** The target is UI made of text, tables and forms. It favours sharp,
  lossless pixels and near-zero cost at rest over high frame rates. It does not need a GPU.
- **Not a general remote-access tool.** Sessions are preconfigured from app profiles. Users do not
  get a shell, a file manager or arbitrary programs.
- **Not a web framework.** It does not draw window chrome. The host does.
- **Not a density play.** A streamer is chosen for control, UX and licence freedom. The streamed
  application dominates the cost of a session, so the transport is not where capacity is won; see
  [architecture.md §6](architecture.md#6-backend-matrix).

## Product principles

Principles 1, 2, 4, 5 and 6 follow from the product brief, from what the first host application
needs, and from the licence decision of 2026-09-20. Principle 3 rests on
[ADR-0003](adr/0003-untrusted-server-client.md), which is still **Proposed**: it is the
recommendation, not yet a decision.

1. **Per-window.** The unit of streaming is a window (a toplevel surface), not a screen. Popups
   belong to a parent window. The host decides where each window goes, how windows stack and which
   one has focus.
2. **Embeddable.** The browser client is a library. It draws into canvases the host provides and
   reports events. It has no UI of its own and does not take over the page. It works without a
   framework; React bindings are a thin layer on top.
3. **The server is untrusted (proposed).** The client treats every byte from the server as hostile.
   Strings become text, never HTML. Nothing from the server becomes script or navigation. Every
   size and count on the wire is bounded. A compromised application can draw inside its own windows
   and nowhere else. See [ADR-0003](adr/0003-untrusted-server-client.md).
4. **GPU-less efficiency.** It must run on an ordinary VPS with no GPU. Idle windows cost nothing
   on the wire and close to nothing in CPU: work is driven by damage, never by a timer. The
   internal benchmark put the floor of a session at one headless X server plus the application
   itself ([architecture.md §6](architecture.md#6-backend-matrix)).
5. **Sandboxed by default.** One container per session, its own network namespace, no shared
   display, a read-only root filesystem, dropped capabilities, resource caps, and egress limited to
   what the app profile names. A session never shares a display, a clipboard, a loopback interface
   or a credential with another session.
6. **Permissive by licence.** The code that host applications link, bundle or ship to browsers
   depends only on permissive licences, and APPricot itself is MIT OR Apache-2.0
   ([ADR-0002](adr/0002-licence.md)). A host product on either licence can embed the client without
   a legal review of its own build.

## How we will know it works

- A host web product opens the pilot application's login window and each window it spawns as
  **separate host windows**. The operator can raise the login window again after another window has
  covered it. A menu opens where the application puts it, inside its own window, and is dismissed
  with it.
- A deliberately hostile test server cannot put markup, script or navigation into the host page,
  and cannot draw outside the windows the host gave it.
- An idle session sends **no bytes** while nothing changes on screen, and costs no more memory than
  the remote-desktop transport it replaces.
- A browser that reconnects finds its session still running, with its windows where it left them.
