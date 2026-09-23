# AGENTS.md — APPricot

Instructions for any coding agent working in this repository. This is the full contract: what
APPricot is, how to run every gate, and the rules that a change must not break.

## What APPricot is

A load-balanced, sandboxed, preconfigured application streamer. It runs a desktop application in
its own container and streams **each of that application's windows** into a host web UI, where
each window becomes one of the host UI's own floating windows. It is not a remote-desktop canvas:
the host draws the chrome, APPricot fills the content, and the browser client is a library the
host embeds.

| Layer | What it is | State |
|---|---|---|
| **L1 streamer** | The wire protocol, `appricot-streamer` (per-window capture and input, inside the app container next to a headless display server), and the embeddable browser client. | implemented; the spike's measurements are still open |
| **L2 node** | Single-node session manager: app profiles, a container per session, warm pool, readiness gate, reaper, per-session egress scoping, audit, session-ticket API. | documented, not scaffolded |
| **L3 broker** | Multi-node placement and session-affine routing. A ticket names the owning node; a stateful stream cannot migrate. | documented, not scaffolded |

Do not scaffold L2 or L3. Write them down first.

## How to run anything

**Everything runs in Docker.** Never run `cargo`, `pnpm`, `node`, `just` or `rustc` on the host.
Use the host `docker compose` CLI, and only that — a second compose driver (an agent tool that
runs `docker compose` from inside a helper container of its own, for example) fights this one over
the same container. Compose project name: `appricot`.

```sh
docker compose build dev                    # build the dev image (appricot-dev:local)
docker compose run --rm dev just check      # THE gate: everything, in order
docker compose run --rm dev just            # list the recipes
docker compose run --rm dev bash            # a shell, with Xvfb already up on :99
docker compose run --rm -p "127.0.0.1:8390:8390" dev just demo   # the demo host page
```

`scripts/dev.sh <cmd...>` (Git Bash) and `scripts/dev.ps1 <cmd...>` (PowerShell) are thin wrappers
around `docker compose run --rm dev <cmd...>`. They do not build the image, and they return the
command's own exit code. With no arguments they list the recipes.

`Cargo.lock` and `pnpm-lock.yaml` belong in the repository, so a fresh clone needs no resolution
step. Only `just lock` and `just web-lock` may rewrite them; every other recipe
is `--locked` / `--frozen-lockfile` and fails loudly on a stale lockfile instead of rewriting it.

## The gates

`just check` = `names-check pins fmt-check clippy test display-levers doc deny web-check`, and
`web-check` in turn is `web-install web-licences web-build web-typecheck web-lint web-test`. Run
them one at a time while iterating:

| Recipe | What it runs |
|---|---|
| `just names-check` | `scripts/check-names.sh`: no private name, no absolute path into a home or another checkout (hard rule 5 below) |
| `just pins` | fails when rust-toolchain.toml and the dev image's base, or package.json's `packageManager` and the image's pnpm, name different versions |
| `just fmt` / `fmt-check` | `cargo fmt --all` / `--check` |
| `just clippy` | `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` |
| `just test` | `cargo test --workspace --all-features --locked` |
| `just display-levers` | proves both entrypoint levers hold and that the X11 test fails, by a panic, without a display |
| `just doc` | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked` |
| `just deny` | `cargo deny check advisories bans licenses sources` — **needs network** (it clones the RustSec database) |
| `just msrv` | `cargo +<rust-version> check --workspace --all-targets --all-features --locked` on Cargo.toml's declared MSRV — **needs network** (rustup fetches that toolchain); a CI job, not part of `check` |
| `just web-licences` | the npm licence gate: `scripts/web-licences.mjs` and its tests |
| `just web-typecheck` / `web-lint` / `web-test` / `web-build` | `pnpm -r typecheck` / `lint` / `test` / `build` |
| `just run-streamer` | prints the streamer's version line and exits 0 |
| `just demo` | builds the web packages and serves the demo host page on container port 8390 |

Read the **exit code**, never the tail of the log. Chain with `&&`; a `| tail -N` throws the status
away.

The X server: the container entrypoint starts `Xvfb :99 -screen 0 1400x900x24 -nolisten tcp
-noreset` and waits until it answers. `crates/appricot-x11/tests/extensions.rs` asserts Composite,
Damage, XTEST and XFixes are present, and it **panics rather than skips** when there is no display.
Two levers, both honoured: `-e APPRICOT_XVFB=0` starts no X server and leaves `DISPLAY` as it
arrived; `-e DISPLAY=<value>` uses the caller's display and starts nothing. `just display-levers`
checks both, and the panic, in every `just check`.

## Ports

**No host port is published by default**, and the gates need none. The ONE port the repo uses is
the demo host page, 8390, published loopback-only through `APPRICOT_DEMO_HOST_PORT`. The streamer
itself is never published: the demo's in-container server proxies its WebSocket, so the streamer
keeps its loopback-only bind rule. Before ever adding another port: check what is taken on the
development host (the listening sockets, plus every running container's published ports — many
other stacks run there at once), pick a free high port, and publish it through a named variable
bound to loopback:

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
`document.write`, the navigation globals, every `.location`, `history` and `open` spelling, URL
and style attributes and properties, computed `setAttribute` names, non-literal `import()`, …).
`packages/client/src/untrusted-server.lint.test.ts` pins every entry of that rule set and lints
each entry alone against its probes, one probe per name a selector's pattern lists. **Dropping or
narrowing a rule turns that test red — fix the code instead.** A sink reached through an alias
(`const w = window`) is beyond a syntactic rule; review still owns that.
Render text through `setTextOnly()` or as React children. `packages/client/src/hostile/` proves the
whole policy against a hostile server in CI.

## Licences

APPricot is **dual-licensed MIT OR Apache-2.0**, the Rust convention, decided by the user on
2026-09-20 ([ADR-0002](docs/adr/0002-licence.md)). The texts live in `LICENSE-MIT` and
`LICENSE-APACHE` at the repository root. Author: George Gougoudis
<george_gougoudis@hotmail.com>. A new crate or package declares the same pair and nothing else;
changing the project's licence needs a new ADR, not an edit here.

The linked core graph is **Tier A only**: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception,
BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Zlib, 0BSD. `deny.toml` has no deny list —
anything not on the allow list is denied, including unknown licences — and `exceptions = []`.
Adding a dependency under GPL, LGPL, AGPL, MPL-2.0, EPL or a source-available licence needs an
**amendment to ADR-0002 first**, not a `deny.toml` edit. `just deny` is the gate for the Rust
graph. `just web-licences` is the gate for the npm graph: every package in the production graph
must be Tier A, and every workspace package must declare `MIT OR Apache-2.0`. devDependencies are
not gated, because they are never bundled; the gate lists the ones outside Tier A.

## The window model

The model is Wayland-shaped whatever the backend
([docs/architecture.md §4](docs/architecture.md#4-the-window-model),
[ADR-0004](docs/adr/0004-layers-window-model-and-first-backend.md), Proposed): surfaces each with
their own damage; the roles `toplevel` and `popup`; popups placed by a positioner relative to
their parent; per-surface scale; explicit configure/ack. The host plays the compositor — it
decides size, position, stacking and focus — and the streamed app plays the Wayland client.

**`appricot-core` must never name an X11 or Wayland type.** X11 lives in `appricot-x11` alone,
which maps X into the model: override-redirect → popup, `WM_TRANSIENT_FOR` → parent (popups and
dialogs alike), `DamageNotify` → per-surface damage, `_NET_WM_NAME` → title as text. A later
Wayland backend (`appricot-wayland`, not created now) must need no change to the wire or the
client. X11 is first because the pilot application is X11-only and per-window capture works on
plain Xvfb; that is a measurement, not a preference.

## The map

| Path | What belongs there | What must not |
|---|---|---|
| `crates/appricot-proto` | Wire protocol v0: protobuf messages, codec, the limits table. | I/O, an async runtime, or allocating from an unchecked length. |
| `crates/appricot-core` | The window model, the `CaptureBackend` and `InputSink` traits, the session state machine, damage, frame scheduling driven by client acks. | Any X11 or Wayland type. |
| `crates/appricot-x11` | The S1 backend on x11rb: Composite redirect, Damage, pixmap grab, XTEST, XFixes. It is also the window manager — there is no other WM. | Trusting an X client's stacking, focus or size without clamping it. |
| `crates/appricot-encode` | Tile encoders: RAW and in-house QOI, tile cutting. | Pulling a non-Tier-A crate into the graph. |
| `crates/appricot-streamer` | The binary inside the app container: the protocol over a WebSocket on loopback or a unix socket, per-session token auth, a readiness endpoint. | Listening on a non-loopback address, or accepting a stream without the token. |
| `packages/client` (`@appricot/client`) | Framework-agnostic TS: connection, codec mirror, window registry with events, tile decode, input and key mapping. Draws into canvases the host provides. | Touching the DOM outside those canvases. Turning a server string into markup. |
| `packages/react` (`@appricot/react`) | Provider, hooks, a window-canvas component. | Rendering chrome — the host draws that. |
| `packages/demo` (`@appricot/demo`) | The demo host page and its static server with the WebSocket proxy. Dev-only. | Shipping in anyone's dependency graph. |

Dependency direction points one way and is enforced by review: `proto ← core ← x11`,
`encode → core`, and `streamer` may depend on all of them. `proto` depends on no sibling; `core`
depends on `proto` only.

## The first host application

The first integration is a **private web product in a separate repository**, owned by the same
author. That product is never named in this repository, and neither is any customer, vendor or
sibling project. APPricot is a standalone product; this repository is meant to be readable by
anyone.

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
5. **A check enforces (1) and (2), and a reviewer still reads the diff.** `just names-check`
   (`scripts/check-names.sh`, the first step of `just check`) reads every tracked file and every
   untracked file that is not ignored, contents and paths. It fails on an absolute path into a
   home directory or another checkout, and on any name in the private denylist, in any case. It
   prints `file:line`, never the name. The denylist is never in the tree: CI reads it from the
   repository secret `APPRICOT_NAMES_DENYLIST`, and a local run reads
   `.git/info/names-denylist`, one name per line, which git never tracks. With neither, the
   check runs the path half alone and says so. A list only catches the spellings it holds, so a
   reviewer still greps the diff before a commit for new names and new spellings. A hit is a
   blocker, not a nit.

## Docs

Plain, short sentences. English everywhere — code, comments, commit messages, documents.

- **ADRs** live in `docs/adr/NNNN-short-title.md`: **Status** (Proposed / Accepted / Superseded /
  Rejected, with a date, and who accepted it), **Context**, **Decision**, **Consequences**,
  **Alternatives considered**. Numbers are four digits and never reused. An accepted ADR is not
  rewritten — add a dated **Amendment** section, or supersede it with a new ADR. Update the index
  table in [docs/adr/README.md](docs/adr/README.md) in the same change.
- A **Proposed** ADR overrides nothing. When prose depends on one, say so ("proposed in ADR-000N").
- **Every claim about an external project** — a version, date, licence or capability — carries a
  URL that was actually checked, or the word **(unverified)**. Measurements say they come from
  an internal benchmark, with its date and the method in a clause, and never name a document outside
  this repository.
- Wrap prose at about 100 characters. A table row or a line carrying a long URL may exceed it;
  do not break a URL to fit. Relative links must resolve, anchors included.

## Git

- **Do not commit unless the user asks.** Not at the end of a task, not "to be safe".
- The branch is `main`. Never force-push, never rewrite history.
- When asked to commit: one logical change per commit, present tense, English, and commit by
  pathspec rather than `git add -A`.

Documents live under [docs/](docs/); decisions are ADRs under [docs/adr/](docs/adr/README.md), and
an Accepted ADR outranks every other document.
