# Roadmap

Written 2026-09-19. Milestones are ordered by dependency. Each one lists the task that tracks it
and the exit criteria that close it. A milestone is done when every exit criterion is met, not
when its code exists, unless the user closes it by a recorded decision. The "where it stands"
notes are dated 2026-09-21 unless they carry their own date.

The effort estimates are one senior engineer's own figures from the September 2026 research, with
no external source: about 8-12 engineer-weeks to an S1 MVP and 20-28 to parity with a mature
whole-screen VNC stack. They are not commitments.

| Milestone | Tasks | Depends on |
|---|---|---|
| M0 bootstrap | `bootstrap`, `licence-decision` | nothing |
| M1 wire spec v0 + X11 capture spike | `l1-wire-spec-v0`, `l1-spike-x11-capture` | M0 |
| M2 client MVP | `l1-client-mvp` | M1 |
| M3 first host integration behind a flag | `first-host-integration` | M2 |
| M4 L2 node | `l2-node-session-manager` | M3 |
| M5 L3 broker | `l3-broker` | M4 |
| M6 a Wayland backend | none yet | M2; a Wayland-native application to stream |

## M0: Bootstrap

The repository exists, builds in Docker, and says what it is.

Exit criteria:

- `docker compose build dev` and `docker compose run --rm dev just check` pass on a clean clone.
  Nothing runs on the host. No host port is published.
- `AGENTS.md` states the working rules, and they agree with the compose file, the
  justfile and the scripts.
- The five Rust crates and two npm packages exist as stubs, and the check runs format, lint, test
  and the licence gate for both graphs.
- The docs in this folder exist: vision, architecture (Draft), roadmap, glossary, ADRs 0001-0004,
  the first threat model, the protocol placeholder, and prior art.
- The repository is initialised with `git init -b main`.
- **`licence-decision`: decided.** The user accepted [ADR-0002](adr/0002-licence.md) on
  2026-09-20: APPricot is **MIT OR Apache-2.0**. `LICENSE-MIT` and `LICENSE-APACHE` sit at the
  root, every crate and every package carries that SPDX expression, and `cargo deny` gates the
  Rust graph against the permissive list in ADR-0002 §2.

Where it stands (2026-09-21): done. The gates pass on a clean clone, the five crates and the
client/react packages exist with full contents, and the docs are written. A third npm package,
`@appricot/demo` (the demo host page), joined later with the client work — the criterion's "two
npm packages" counted `client` and `react`.

## M1: Wire spec v0 and the X11 capture spike

Two tasks that run side by side and meet at the end.

**`l1-wire-spec-v0`**: the protocol, written down and coded.

- `docs/protocol/` holds the v0 spec: every message, every field, the byte order, the version
  handshake, and a limits table with a number for every length and count
  ([protocol/README.md](protocol/README.md) lists the rules).
- `appricot-proto` encodes and decodes every v0 message, with no I/O, and refuses any input that
  breaks a limit before it allocates.
- A round-trip test covers every message. A fuzz target covers the decoder and runs in CI for a
  bounded time.
- The TypeScript codec in `@appricot/client` passes the same test vectors, generated from the
  Rust side.

**`l1-spike-x11-capture`**: measure before building. It answers the open questions of
[ADR-0004](adr/0004-layers-window-model-and-first-backend.md) against the pilot application under
Xvfb, using `appricot-x11` as the window manager.

- Every window the application maps is logged (type, override-redirect, transient-for, Motif
  hints), during login, menus, combo boxes, tooltips and "open in new", and after a successful
  connection to a real device.
- The toolkit's scene-graph backend and the Damage rectangles for a table refresh and a scroll are
  recorded, with and without the software scene graph forced.
- One window's pixels are captured while another window covers it, with the redirect held from
  start-up.
- **The keyboard check.** XTEST types a Greek word, an AltGr character and a dead-key accent into
  the application's text fields, and each arrives correctly. A click reaches a window that was
  covered until the streamer raised it.
- **Every environment knob the app profile sets is recorded, with what it buys and how it was
  visually proved.** A knob can halve the application's memory and quietly break a widget's
  rendering, so "it still starts" is not proof: each knob carries a screenshot or an equivalent
  recorded visual check, per profile. A knob with no recorded check does not go in a profile.
- Encode time and bytes are measured for the candidate lossless codecs on those rectangles.
- Session RSS and cgroup anon + shmem, **with the streamer running**, are measured next to the
  reference figures recorded in
  [ADR-0004](adr/0004-layers-window-model-and-first-backend.md). That is the number nobody has
  yet: every reference figure there excludes the streamer.
- S3 is priced: the application under an unmodified xpra server, its window stream read by a
  throwaway client.
- The open questions in [architecture.md](architecture.md) that the spike owns have answers,
  written into the architecture document.

M1 is done when both tasks are done and the spec has been revised with what the spike measured.
Decision point: S1 or S3 for M2. If neither lands within the estimates, stop and say so.

**Where it stands (2026-09-24): closed by the user's decision, with two criteria moved rather
than met (the keyboard check and S3, below).**

`l1-wire-spec-v0`: `appricot-proto` encodes and decodes all 24 v0 messages, refuses any input
that breaks a limit before it allocates, generates the test vectors, and the TypeScript mirror in
`@appricot/client` passes them; both decoders are fuzzed. The spec is revised with what the spike
measured, and the spike moved nothing on the wire ([protocol/v0.md](protocol/v0.md)).

`l1-spike-x11-capture`: the harness is [spike/README.md](spike/README.md), the results
[spike/findings-2026-09-23.md](spike/findings-2026-09-23.md). Criterion by criterion:

| Criterion | Outcome |
|---|---|
| Every window logged, before and after connecting | met: one X window per main window, each from its own process; every popup is drawn inside its window |
| Scene graph and damage, with and without the software scene graph | met for OpenGL: every repaint, tables and scrolls of the connected run included, is a whole-window Damage event. The software scene graph was measured on the login screen only (it damages only what changed) and fails its visual check there |
| Capture under a covering window | met |
| The keyboard check | **not met**: Greek next to US, AltGr and dead-key accents do not arrive. Moved to M2, whose exit criteria require them |
| A click on a covered window | met: the streamer raised the window and the click opened its combo box |
| Every knob recorded with a visual check | met: the software scene graph fails its check (popups lose their background) and stays out of profiles |
| Encode time and bytes | met: QOI is 2 % of RAW at 90 µs per tile |
| RSS and cgroup with the streamer | met: the streamer is about 6-7 MiB RSS and 2-3 MiB anon |
| S3 priced | **not done**, by decision: S1 met every need the spike measured |
| The architecture's open questions | answered where the spike could; the supervisor and the uid moved to M4 |

Decision point: **S1**, decided by the user on 2026-09-24, with
[ADR-0004](adr/0004-layers-window-model-and-first-backend.md) accepted. Carried forward: the
keysym delivery fix and the proposal to skip unchanged tiles to M2; the supervisor, the
streamer's uid and a per-session cap on windows or memory to M4.

## M2: Client MVP

`l1-client-mvp`. A host page shows the application's windows as its own windows.

Exit criteria:

- `appricot-streamer` serves those windows from a container, with token auth, a readiness
  endpoint and ack-based flow control. An idle window sends nothing.
- `@appricot/client` and `@appricot/react` drive a demo host page: one host window per toplevel,
  popups placed and clamped to their parent, titles as text, the cursor, resize, minimise, close
  and focus.
- Input works on desktop Chrome, Firefox and Safari, with US and Greek layouts, AltGr and dead
  keys.
- Text clipboard in both directions, only as the host's policy allows, and a clipboard write to
  the user only inside a user gesture.
- The demo page runs under a strict CSP with no `'unsafe-inline'` and no `'unsafe-eval'` in
  `script-src`, and the lint rules of [ADR-0003](adr/0003-untrusted-server-client.md) pass.
- A hostile test server (a fixture that sends markup in every string, oversized lengths, a popup
  the size of the screen, and focus requests) cannot put markup into the page, draw outside the
  parent-plus-margin box, or take focus. This is a CI test.
- Decoders in the browser realm have fuzz tests.

Where it stands (2026-09-21; refreshed 2026-09-24): the code side has landed and is gated in CI —
the streamer (token auth, readiness, ack-based flow control, the resume grace), `@appricot/client`
and `@appricot/react`, the hostile fixture and the decoder fuzz tests, and a demo host page that
draws the streamed windows as its own floating windows under a strict CSP. Three more units
landed in-repo with their gates on 2026-09-24:

- **The keysym delivery fix** carried from M1 ([spike findings
  §5](spike/findings-2026-09-23.md#5-the-keyboard-check)): the X11 backend delivers keysyms
  beyond the keymap's first group and two levels — Greek next to US, AltGr, dead-key accents —
  driven through the backend and asserted by a test X client on a private Xvfb, for the `us`,
  `gr` and `us,gr` layouts: direct keymap columns, the layout's own dead-key sequences, and a
  rebind onto a spare keycode as the fallback, restored on release.
- **The skip-unchanged-tiles sender optimisation**, carried from M1 as a proposal ([spike
  findings §2](spike/findings-2026-09-23.md#2-the-scene-graph-and-what-it-damages), where an
  OpenGL application damages its whole window on every repaint): the streamer omits a tile whose
  encoded payload is byte-identical to the last one it sent for that grid cell, so a frame may
  carry fewer tiles than the damage, and a frame with no changed tile is not sent at all.
- **The app-to-host clipboard**, the gap the 2026-09-21 state of this note named: envelope #25
  `clipboard_text` carries the app's copy to the host as untrusted text, capped and UTF-8,
  exposed as a client event only; the host's policy decides, and a write to the user's clipboard
  happens only inside a user gesture (ADR-0003 §7's rules, landed ahead of v0's acceptance).

Still open for M2: the real-browser pass on desktop Chrome, Firefox and Safari, and everything
that needs the pilot application on a real device.

## M3: First host integration behind a flag

`first-host-integration`. The pilot application becomes APPricot's first app profile, inside the
first host application.

That host application lives in its own repository ([ADR-0001](adr/0001-separate-repository.md)).
The work in this milestone happens **there**, by that project's own process and review. APPricot
delivers versioned artefacts — the streamer binary or image layer, and the npm packages — plus
an app profile, and nothing else. Nothing in this milestone edits APPricot to suit one consumer;
anything that looks like it
must is a gap in the interface and is fixed as one.

Exit criteria:

- A session image variant runs `appricot-streamer` and Xvfb instead of the incumbent whole-screen
  VNC stack. A flag selects it, and the incumbent stays available as the fallback for as long as
  the consumer wants it.
- The host's own service relays APPricot frames through its existing proxy. Its warm pool, reaper
  and egress scoping keep their shape; APPricot does not require them to change.
- The host's UI draws one floating window per streamed toplevel, and a window that has gone behind
  another can be raised again.
- Every requirement that host recorded for its existing streaming session is either met or
  recorded by that project as an accepted gap.
- A real-browser pass on the supported browsers, against a real device.
- The host's dependency policy accepts APPricot's packages. `MIT OR Apache-2.0` is permissive, so
  this should need no exception entry ([ADR-0002](adr/0002-licence.md)).

## M4: L2 node

`l2-node-session-manager`. The session machinery, generalised out of one integration.

Exit criteria:

- App profiles as data: image, launch command, env, mounts, resource caps, egress policy,
  clipboard and scale policy. Each profile carries the recorded visual check for every env knob
  it sets (M1).
- One container per session with the envelope from [architecture.md](architecture.md); the
  container-engine socket reached only through a socket proxy.
- **One network namespace per session**, by construction, not by filter rule
  ([threat model §4.2](security/threat-model.md)).
- A warm pool per profile, a readiness gate that re-knocks at claim, and a reaper (idle on input,
  hard TTL, container gone).
- Egress rules armed deny-all at create and widened to the profile's list at activation; swept at
  boot; fail-closed.
- A session-ticket API for host backends, and a node proxy that checks the ticket and the
  `Origin` and relays with bounded buffers.
- **An unauthenticated client is refused.** For every backend the node can start, a client is
  pointed at it carrying no credentials at all, and the connection is refused. The result is
  recorded per backend and repeated for each new backend
  ([threat model §4.5](security/threat-model.md)).
- **A session cannot reach a sibling's socket.** An automated test starts two sessions and asserts
  that neither can open or authenticate to the other's listeners — including any abstract unix
  socket — and that every internal control verb rejects a caller without that session's token
  ([threat model §4.2](security/threat-model.md)).
- Audit rows for open and close.
- The first host application can switch its sessions to the L2 node and drop its own copies of
  this machinery, if it chooses to.

Carried from M1 (2026-09-24): who supervises Xvfb and the application, and whether the streamer
runs under its own uid ([architecture.md §8](architecture.md#8-open-questions)); and a per-session
cap on windows or memory, since each extra window of the pilot application is a new process of
about 110 MiB ([spike findings §8](spike/findings-2026-09-23.md#8-connected-to-a-device)).

## M5: L3 broker

`l3-broker`. More than one node.

Exit criteria:

- Placement across at least two nodes by capacity and warm-session availability.
- Tickets that are signed, short-lived and name their node; a ticket for node A is refused by
  node B.
- An edge router that sends each WebSocket to the ticket's node without parsing frames.
- A node that dies ends its sessions cleanly; nothing tries to migrate a stream.

## M6: A Wayland backend

No task yet. Build it when a Wayland-native application needs streaming; the pilot application is
an X11 client and does not
([ADR-0004](adr/0004-layers-window-model-and-first-backend.md)).

Exit criteria:

- `crates/appricot-wayland`: a headless compositor that implements `CaptureBackend` and
  `InputSink` for Wayland-native applications.
- The wire protocol and the client need **no** change. That is the test of the Wayland-shaped
  model.
- The dependency question is settled first. smithay's last crates.io release is 0.7.0, from
  2025-06-24 ([crates.io](https://crates.io/api/v1/crates/smithay/versions)). A git pin would need
  an exception in the licence and source gate, which [ADR-0002](adr/0002-licence.md) does not
  grant without an amendment.
- Session memory and CPU are measured against S1's numbers before the backend is offered.
