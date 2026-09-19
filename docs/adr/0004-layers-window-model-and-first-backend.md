# ADR-0004 — Three layers, a Wayland-shaped window model, and X11 first

**Status**: Proposed (2026-09-19). Already decided by the user, and not re-opened here: a
first-party per-window streamer, in its own repository ([ADR-0001](0001-separate-repository.md)).
This ADR proposes how it is layered, which window model it uses, and which display backend comes
first.

## Context

The question that started this work was the user's: could a desktop-only X11 application be
streamed through **Wayland** instead of a whole-screen VNC stack, and would that be better?

The evidence below comes from an **internal benchmark (September 2026)**: a Qt6/xcb X11
administration tool, idle, running headless under Docker with no GPU, on Debian 12 and Debian 13
images. Every number in this section is from that benchmark unless a URL says otherwise.

**Can such an application run under Wayland?** Yes, but only as an X11 client. The pilot
application's Linux build carries only Qt's xcb platform plugin and no Wayland client (binary
scan). At run time it maps libGL, libEGL, xcb-glx and xcb-shm (read from `/proc/<pid>/maps`).
Under weston or sway it runs through rootless Xwayland, headless and without a GPU.

**Is Wayland better for it?** No. Measured with the application idle:

| Session | Summed RSS | Display side, cgroup anon + shmem | Session idle CPU |
|---|---|---|---|
| Xvfb + the application | 286 MiB | 36 MiB | 2.40-2.77 % |
| A KasmVNC session + the application | 304 MiB | 62 MiB | 2.16 % |
| weston 14 + Xwayland + the application | 370 MiB | 69-70 MiB | 2.59-2.80 % |

Read the rows with care.

- The Xvfb and weston rows have **no streaming server** in them: the Composite redirect holder was
  left out of the Xvfb sums, and headless weston exports nothing. The KasmVNC row includes its
  whole VNC and web server. So the table compares display stacks, not products.
- The streamer's own memory and CPU come on top of either, and have not been measured
  (unverified). The M1 spike measures them.
- The weston figure of 69 MiB is its headless backend, 70 MiB its VNC backend.
- The Xvfb and weston rows were measured on Debian 13 and the KasmVNC row on Debian 12. About
  15 MiB of any Debian 12 against Debian 13 comparison is the application itself, not the display
  stack.

What follows from it:

- A Wayland stack is always a compositor, plus Xwayland, plus the same X11 application. It is
  strictly more than one X server. Summed RSS overstates the gap for multi-process stacks; the
  cgroup figure is the fair one.
- The idle CPU is mostly the application retrying an update check with no network. It cancels
  between rows.
- **Per-window capture does not need Wayland.** Composite `NameWindowPixmap` returned one
  window's clean pixels while another window covered it, on plain Xvfb, once the redirect was held
  from start-up. XTEST typing reached that covered window once X focus was set on it. An XTEST
  click lands on whichever window is on top, so the streamer must own stacking.
- The pilot application draws its menus and its settings panel **inside** its main window. No
  extra X windows appear for them. The popup problem looks small for this profile. Combo lists,
  tooltips and the windows that open after a successful login were not logged; the M1 spike does
  it.
- The Wayland exports that were benched (weston 14's VNC and RDP backends, wayvnc 0.9.1) send one
  flattened output, and each still needs a web VNC or RDP client. wayvnc 0.10.0 added a
  `-T, --toplevel` option, "Capture a toplevel"
  ([wayvnc.scd at v0.10.0](https://github.com/any1/wayvnc/blob/v0.10.0/wayvnc.scd), checked
  2026-09-19). It was not benched, and its output is still RFB for a web client to decode.
- Weston 14.0.2's RDP backend accepted any user name and password on the bench. The rule drawn
  from that finding lives in the [threat model](../security/threat-model.md).
- Greenfield, a Wayland compositor that runs in the browser, is `AGPL-3.0-or-later`
  ([package.json](https://raw.githubusercontent.com/udevbe/greenfield/master/packages/compositor-proxy-cli/package.json),
  checked 2026-09-19) and started Xwayland with access control off.

**Is a Wayland design still useful?** For the model, yes. Wayland names the concepts a per-window
streamer needs, and names them well: surfaces with damage, roles, popups placed relative to a
parent, per-surface scale, and an explicit configure and ack handshake
([xdg-shell](https://wayland.app/protocols/xdg-shell), [wayland](https://wayland.app/protocols/wayland)).
And a future application may be Wayland-native.

Three shapes for the streamer were compared. **S1**: X11 through x11rb, with our own window
manager. **S2**: a smithay headless compositor with Xwayland. **S3**, added during verification:
an unmodified xpra server with our own client.

## Decision (proposed)

### 1. Three layers

- **L1 streamer**: the wire protocol, `appricot-streamer` inside the app container, and the
  embeddable browser client. Only L1 gets code at bootstrap.
- **L2 node**: a single-node session manager (app profiles, a container per session, a warm pool,
  a readiness gate, a reaper, per-session egress, audit, a session-ticket API).
- **L3 broker**: multi-node placement and session-affine routing. The ticket names the owning
  node, because a stateful stream cannot migrate.

Each layer works without the one above it. L1 alone serves one session to a host that manages
containers itself, which is what the first host integration does at M3. L2 alone serves one node.
L3 is only for more than one node.

### 2. A Wayland-shaped window model, whatever the backend

`appricot-core` and the wire protocol use one model
([architecture.md](../architecture.md), the window model section):

- **surfaces**, each with its own id, size and damage;
- the roles **toplevel** (optionally with a parent) and **popup**, set once;
- popups placed by a **positioner** relative to their parent;
- **per-surface scale**;
- an explicit **configure / ack**. The host plays the compositor: it proposes, the streamer acks
  with what the application really took.

No X11 or Wayland type appears in `appricot-core`, `appricot-proto` or the client. Each backend
maps its display server into the model. The X11 backend maps:

- a managed window to a toplevel, with `WM_TRANSIENT_FOR` giving its parent;
- an override-redirect window to a popup, with `WM_TRANSIENT_FOR` or the focused toplevel as the
  parent, and a positioner derived from root coordinates;
- DAMAGE rectangles to surface damage;
- a session-wide scale to per-surface scale.

### 3. Backends: S1 first, S2 later, S3 open

- **S1, X11, first** (`crates/appricot-x11`, M1-M3). It has the cheapest measured display stack
  (the streamer itself is not measured yet). It puts nothing between an X11 application and our
  capture code: no compositor, no Xwayland, no third-party server. Its key dependency is released:
  x11rb 0.14.0, MIT OR Apache-2.0, 2026-07-16 ([crates.io](https://crates.io/api/v1/crates/x11rb),
  checked 2026-09-19), and the author already runs it in production for X11 window inspection.
- **S2, Wayland, later** (a future `crates/appricot-wayland`, **not created now**; M6). It is for
  Wayland-native applications. It would give per-surface buffers and damage for free, but for X11
  applications it still needs an X11 window manager (smithay's `X11Wm`). Its main dependency has
  not released in over a year: smithay's last crates.io release is 0.7.0, 2025-06-24
  ([crates.io](https://crates.io/api/v1/crates/smithay/versions), checked 2026-09-19).
- **S3, an xpra-server adapter, open.** An unmodified xpra server in the container would do the
  window management, capture and encoding, and APPricot would keep only a client and an adapter.
  Measured under xpra 6.5.3 with its own html5 client, untuned, in the same benchmark: one browser
  window per X window; 347 MiB session RSS with no client and 388 MiB with one, against 316 and
  334 MiB for the KasmVNC session in the same bench. After the operator's first input it used
  5.8 % CPU against the KasmVNC session's 3.3 %, and sent about 100-140x its bytes while the text
  caret blinked (177-246 kB/s against 1.8 kB/s). An earlier "about 2x" reading compared against a
  KasmVNC state that ends at the first mouse move. Against S3: a Python server in every session, a
  GPLv2+ package in the image (aggregation, not linking;
  [`setup.py`](https://raw.githubusercontent.com/Xpra-org/xpra/master/setup.py), checked
  2026-09-19), and a wire format APPricot does not own. The M1 spike prices it. If it wins, it
  becomes a backend behind the same model; the client and the wire protocol do not change.

## Consequences

- L1 can be built and tested with no orchestration at all: one container, one Xvfb, one
  application.
- The model costs a little now. X11 has no native positioner or configure serial; the X11 backend
  must derive them. In exchange, a Wayland backend or an xpra adapter needs no client change.
- The streamer is a window manager. It takes over the ICCCM and EWMH duties that a
  no-window-manager setup avoided. Every atom it advertises must really work.
- Input depends on the window manager owning stacking: raise or separate a window before injecting
  a click.
- Xvfb cannot grow past its start size, and shrinking needs a new RandR mode. The streamer starts
  it large.
- Estimates, unmeasured, from one senior engineer: about 8-12 engineer-weeks to an S1 MVP and
  20-28 to parity with a mature whole-screen VNC stack; S2 about 12-18 and 28-40. The long tail is
  keyboard layouts, clipboard rules, lossy encoding in motion and HiDPI.
- **This backend choice does not buy density.** Xvfb's display side is 26 MiB smaller than the
  KasmVNC session's (36 against 62 MiB), but the 36 MiB has no streamer in it. The streamer's own
  buffers, and the backing pixmap Composite keeps per window (about 4 MB for a 1300x800 window),
  come out of that margin, and the net saving could be close to zero (an estimate, not a
  measurement). The M1 spike settles it.

## Alternatives considered

- **Wayland first (S2 before S1).** Rejected for now. For an X11 application it adds a compositor
  and Xwayland (about 33 MiB more display side per session than Xvfb), still needs an X11 window
  manager, and depends on a crate with no release since 2025-06-24. Chosen later, for
  Wayland-native applications.
- **An X11-shaped model** (windows, override-redirect, root coordinates on the wire). Simpler for
  S1 alone. Rejected because every later backend would have to fake X11 semantics, and root
  coordinates on the wire invite a server to place things outside its parent.
- **A desktop model** (one framebuffer, as VNC). Rejected by the product itself: it is not
  per-window ([vision.md](../vision.md)).
- **Weston or sway with their VNC/RDP exports.** Rejected. The versions benched export one
  flattened output, every one of them still needs a web VNC or RDP client, and weston's RDP
  backend accepted any credentials. wayvnc 0.10.0's per-toplevel capture (link above) does not
  change the second point.
- **Greenfield.** Rejected. AGPL-3.0-or-later (link above), its only GitHub release is 1.0.0-rc1,
  published 2023-12-04 ([releases API](https://api.github.com/repos/udevbe/greenfield/releases),
  checked 2026-09-19), and it started Xwayland with access control off.
- **L1 only, no L2/L3 plan.** Rejected. The ticket must name its node from the start, and the
  stream token must never reach the browser; both shape L1's handshake now.
