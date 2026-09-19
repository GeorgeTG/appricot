# CLAUDE.md — APPricot

Project instructions. Read this before touching anything here. Accepted ADRs under
[docs/adr/](docs/adr/README.md) outrank this file; this file outranks the rest of `docs/`.

## What APPricot is

A load-balanced, sandboxed, preconfigured application streamer. It runs a desktop application in
its own container and streams **each of that application's windows** into a host web UI, where
each window becomes one of the host UI's own floating windows. It is not a remote-desktop canvas:
the host draws the chrome, APPricot fills the content, and the browser client is a library the
host embeds.

| Layer | What it is | State |
|---|---|---|
| **L1 streamer** | The wire protocol, `appricot-streamer` (per-window capture and input, inside the app container next to a headless display server), and the embeddable browser client. | the only layer with code |
| **L2 node** | Single-node session manager: app profiles, a container per session, warm pool, readiness gate, reaper, per-session egress scoping, audit, session-ticket API. | documented, not scaffolded |
| **L3 broker** | Multi-node placement and session-affine routing. A ticket names the owning node; a stateful stream cannot migrate. | documented, not scaffolded |

Do not scaffold L2 or L3. Write them down first.

## BigBrain

Workspace id: **`appricot`**. At session start:

```
probe_workspace(project_id="appricot")      # or path=<this checkout's absolute path>
```

Eight tasks, all `pending` at bootstrap: `bootstrap`, `licence-decision`, `l1-wire-spec-v0`,
`l1-spike-x11-capture`, `l1-client-mvp`, `first-host-integration`, `l2-node-session-manager`,
`l3-broker`. They map to milestones M0-M5 in [docs/roadmap.md](docs/roadmap.md); M6 (a Wayland
backend) has no task yet. Journal while you work; write a report at the end.

## How to run anything

**Everything runs in Docker.** Never run `cargo`, `pnpm`, `node`, `just` or `rustc` on the host.
Use the host `docker compose` CLI, and only that — a second compose driver (dock-manager's helper,
for example) fights this one over the same container. Compose project name: `appricot`.

```sh
docker compose build dev                    # build the dev image (appricot-dev:local)
docker compose run --rm dev just check      # THE gate: everything, in order
docker compose run --rm dev just            # list the recipes
docker compose run --rm dev bash            # a shell, with Xvfb already up on :99
```

`scripts/dev.sh <cmd...>` (Git Bash) and `scripts/dev.ps1 <cmd...>` (PowerShell) are thin wrappers
around `docker compose run --rm dev <cmd...>`. They do not build the image, and they return the
command's own exit code. With no arguments they list the recipes.

`Cargo.lock` and `pnpm-lock.yaml` belong in the repository, so a fresh clone needs no resolution
step. Only `just lock` and `just web-lock` may rewrite them; every other recipe
is `--locked` / `--frozen-lockfile` and fails loudly on a stale lockfile instead of rewriting it.

## The gates

`just check` = `fmt-check clippy test doc deny web-check`, and `web-check` in turn is
`web-install web-typecheck web-lint web-test`. Run them one at a time while iterating:

| Recipe | What it runs |
|---|---|
| `just fmt` / `just fmt-check` | `cargo fmt --all` / `--check` |
| `just clippy` | `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` |
| `just test` | `cargo test --workspace --all-features --locked` |
| `just doc` | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked` — the only gate that checks intra-doc links |
| `just deny` | `cargo deny check advisories bans licenses sources` — **needs network** (it clones the RustSec database) |
| `just web-typecheck` / `web-lint` / `web-test` / `web-build` | `pnpm -r typecheck` / `lint` / `test` / `build` |
| `just run-streamer` | prints the streamer's version line and exits 0 |

Read the **exit code**, never the tail of the log. Chain with `&&`; a `| tail -N` throws the status
away.

The X server: the container entrypoint starts `Xvfb :99 -screen 0 1400x900x24 -nolisten tcp` and
waits until it answers. `crates/appricot-x11/tests/extensions.rs` asserts Composite, Damage, XTEST
and XFixes are present, and it **panics rather than skips** when there is no display. Two levers,
both honoured: `-e APPRICOT_XVFB=0` starts no X server and leaves `DISPLAY` as it arrived;
`-e DISPLAY=<value>` uses the caller's display and starts nothing.

## Ports

**No host port is published**, and none should be at this stage. The streamer binds loopback or a
unix socket inside its own container; the gates need no port. Before ever adding one: check what
is taken on the development host (the listening sockets, plus every running container's published
ports — many other stacks run there at once), pick a free high port, and publish it through a
named variable bound to loopback:

```yaml
ports:
  - "127.0.0.1:${APPRICOT_<SERVICE>_HOST_PORT:-NNNN}:<container port>"
```

The comment block at the top of `compose.yml` is the canonical scheme.

## The server is untrusted

[ADR-0003](docs/adr/0003-untrusted-server-client.md) (Proposed) is the client's first rule: the
streamer shares the app's sandbox, so it is treated as compromised with it. The client renders
every server string — titles, app ids, errors — as **text**, never as markup, script, a URL or a
navigation. No `innerHTML`, no `eval`, no server-supplied navigation, a strict CSP with no
`'unsafe-inline'` and no `'unsafe-eval'` in `script-src`. Every size and count on the wire is
bounded before anything is allocated.

This is enforced by lint, not by good intentions. `eslint.config.js` forbids the sinks by name
(`innerHTML`, `outerHTML`, `insertAdjacentHTML`, `setHTMLUnsafe`, `dangerouslySetInnerHTML`,
`document.write`, every `location`/`history` mutation, computed `setAttribute` names, non-literal
`import()`, …), and `packages/client/src/untrusted-server.lint.test.ts` runs 33 cases against that
config to prove each rule fires. **Weakening a rule turns that test red — fix the code instead.**
Render text through `setTextOnly()` or as React children.

## Licences

APPricot is **dual-licensed MIT OR Apache-2.0**, the Rust convention, decided by the user on
2026-09-20 ([ADR-0002](docs/adr/0002-licence.md)). The texts live in `LICENSE-MIT` and
`LICENSE-APACHE` at the repository root. Author: George Gougoudis
<george_gougoudis@hotmail.com>. A new crate or package declares the same pair and nothing else;
changing the project's licence needs a new ADR, not an edit here.

The linked core graph is **permissive only** (ADR-0002 §2): MIT, Apache-2.0,
Apache-2.0 WITH LLVM-exception,
BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Zlib, 0BSD. `deny.toml` has no deny list —
anything not on the allow list is denied, including unknown licences — and `exceptions = []`.
Adding a dependency under GPL, LGPL, AGPL, MPL-2.0, EPL or a source-available licence needs an
**amendment to ADR-0002 first**, not a `deny.toml` edit. `just deny` is the gate. (There is no
npm licence gate yet; ADR-0002 §2 asks for one.)

## The window model

The model is Wayland-shaped whatever the backend
([docs/architecture.md §4](docs/architecture.md#4-the-window-model),
[ADR-0004](docs/adr/0004-layers-window-model-and-first-backend.md), Proposed): surfaces each with
their own damage; the roles `toplevel` and `popup`; popups placed by a positioner relative to
their parent; per-surface scale; explicit configure/ack. The host plays the compositor — it
decides size, position, stacking and focus — and the streamed app plays the Wayland client.

**`appricot-core` must never name an X11 or Wayland type.** X11 lives in `appricot-x11` alone,
which maps X into the model: override-redirect → popup, `WM_TRANSIENT_FOR` → parent,
`DamageNotify` → per-surface damage, `_NET_WM_NAME` → title as text. A later Wayland backend
(`appricot-wayland`, not created now) must need no change to the wire or the client. X11 is first
because the pilot application is X11-only and per-window capture works on plain Xvfb; that is a
measurement, not a preference.

## The map

| Path | What belongs there | What must not |
|---|---|---|
| `crates/appricot-proto` | Wire protocol v0: types, codec, version negotiation, the limits table. | I/O, an async runtime, or allocating from an unchecked length. |
| `crates/appricot-core` | The window model, the `CaptureBackend` and `InputSink` traits, damage accumulation, frame scheduling driven by client acks. | Any X11 or Wayland type. |
| `crates/appricot-x11` | The S1 backend on x11rb: Composite redirect, Damage, pixmap grab, XTEST, XFixes. It is also the window manager — there is no other WM. | Trusting an X client's stacking, focus or size without clamping. |
| `crates/appricot-encode` | Tile encoders. Lossless first; the codec is open. | Pulling a crate off the permissive allow list into the graph. |
| `crates/appricot-streamer` | The binary inside the app container: the protocol over a WebSocket on loopback or a unix socket, per-session token auth, a readiness endpoint. | Listening on a non-loopback address, or accepting a stream without the token. |
| `packages/client` (`@appricot/client`) | Framework-agnostic TS: connection, codec mirror, window registry with events, tile decode, input and key mapping. Draws into canvases the host provides. | Touching the DOM outside those canvases. Turning a server string into markup. |
| `packages/react` (`@appricot/react`) | Provider, hooks, a window-canvas component. | Rendering chrome — the host draws that. |

Dependency direction points one way and is enforced by review: `proto ← core ← x11`, and
`streamer` may depend on all of them. `proto` depends on no sibling; `core` depends on `proto`
only; `encode` has no sibling dependency yet.

## The first host application

The first integration is a **private web product in a separate repository**, owned by the same
author. Today it streams a legacy X11 administration tool through KasmVNC; APPricot replaces that
transport, the tool becomes the first app profile, and the product's own proxy, warm pool and
egress scope stay on its side at first (milestone M3).

That product is never named in this repository, and neither is any customer, vendor or sibling
project. APPricot is a standalone product; this repository is meant to be readable by anyone.

- **Nothing outside this repository is edited from here.** Not a file, not a branch, not a
  container.
- **Designs may be re-used, code is re-implemented, never linked.** APPricot depends on no crate
  of that product, and that product depends on no path in APPricot. Adapt the design, review the
  result, and record it in the re-implementation table in
  [docs/architecture.md](docs/architecture.md) §7.
- **No path out of this repository** appears in prose, code, config or a link. A relative link
  that leaves the working tree is a bug; an absolute path to another checkout is barred by the
  rule below.

### Hard rule: no borrowed names, no borrowed paths

A future session must keep this, and it is not negotiable:

1. No name of a customer, of a vendor whose hardware or software we integrate with, or of another
   private project of this repository's owner, enters this repository — not in prose, code,
   comments, config, CI, test fixtures or file names. The names are deliberately not listed here;
   listing them would put them in the file.
2. No absolute or relative path into another checkout on the development machine.
3. Third-party **public** projects stay, with a checked URL and a date: prior art, dependencies and
   the systems we compare against are named plainly. The rule is about *private* relationships,
   not about citation.
4. Measurements are attributed to **an internal benchmark**, with its date and method in a clause,
   never to a document, path or issue id in another repository.
5. **A reviewer greps the diff before a commit** for the names in (1) and for absolute paths, in
   every case and spelling. A hit is a blocker, not a nit.

## Docs

Plain, short sentences. English everywhere — code, comments, commit messages, documents.

- **ADRs** live in `docs/adr/NNNN-short-title.md`: **Status** (Proposed / Accepted / Superseded /
  Rejected, with a date, and who accepted it), **Context**, **Decision**, **Consequences**,
  **Alternatives considered**. Numbers are four digits and never reused. An accepted ADR is not
  rewritten — add a dated **Amendment** section, or supersede it with a new ADR. Update the index
  table in [docs/adr/README.md](docs/adr/README.md) in the same change.
- A **Proposed** ADR overrides nothing. When prose depends on one, say so ("proposed in ADR-000N").
- **Every claim about an external project** — a version, date, licence or capability — carries a
  URL that was actually checked, or the word **(unverified)**. Measurements say they come from an
  internal benchmark, with its date and the method in a clause, and never name a document outside
  this repository.
- Wrap prose at about 100 characters. A table row or a line carrying a long URL may exceed it;
  do not break a URL to fit. Relative links must resolve, anchors included.

## Git

- **Do not commit unless the user asks.** Not at the end of a task, not "to be safe".
- The branch is `main`. There is no remote; do not add one without being asked.
- Never push, never force-push, never rewrite history.
- When asked to commit: one logical change per commit, present tense, English, and commit by
  pathspec rather than `git add -A`.
