# Development

How to work in this repository. The contract is [AGENTS.md](../AGENTS.md) at the root — the
gates, the hard rules, the map of what belongs where; this page is the narrative version, with
the reasons. Where the two disagree, AGENTS.md wins, and one of them is then a bug.

## 1. Everything runs in Docker

No `cargo`, `pnpm`, `node`, `just` or `rustc` ever runs on the host. Everything — build, lint,
test, docs, licence gate, the demo — runs in the `dev` service of [compose.yml](../compose.yml),
built from [docker/dev/Dockerfile](../docker/dev/Dockerfile). Three reasons:

- **The toolchain is pinned and the image is the only place it is guaranteed.** The image is
  `rust:1.94.1-slim-bookworm`, pinned by tag and digest (rust-toolchain.toml pins 1.94.1, with
  rustfmt and clippy preinstalled so a gate never stalls downloading components), node 22.23.2,
  `just` 1.58.0 and cargo-deny 0.20.2 (each tarball pinned to a version and a SHA-256), and
  pnpm 10.33.0 through corepack, which checks its integrity. Debian packages are not pinned by
  version; apt checks their signatures. `just pins` fails when the Rust or pnpm version in the
  image drifts from rust-toolchain.toml or package.json. The image is linux/amd64 only;
  compose.yml declares it, so an arm64 host runs it under emulation.
- **The X11 integration test needs a display server.**
  `crates/appricot-x11/tests/extensions.rs` asserts Composite, Damage, XTEST and XFixes are on
  `$DISPLAY`, and it panics rather than skips when there is none. The container's entrypoint
  starts one.
- **The host stays clean.** No toolchain directories, no caches, no ports — and no "works on my
  machine": a clean clone plus two commands is the whole setup.

One more rule that saves an afternoon: **use the host `docker compose` CLI, and only that.** A
second compose driver against the same project name — an editor integration, an agent tool
running `docker compose` from inside its own helper container — fights the first over the same
containers, and the loser's error message never says so.

```sh
docker compose build dev                    # build the dev image (appricot-dev:local)
docker compose run --rm dev just check      # THE gate: everything, in order
docker compose run --rm dev just            # list the recipes
docker compose run --rm dev bash            # a shell, with Xvfb already up on :99
```

`scripts/dev.sh <cmd...>` (Git Bash) and `scripts/dev.ps1 <cmd...>` (PowerShell) are thin
wrappers around `docker compose run --rm dev <cmd...>`. They do not build the image, and they
return the command's own exit code. With no arguments they list the recipes. PowerShell swallows
the first bare `--` of a script's arguments; `dev.ps1` reads its own invocation and puts it back,
so `.\scripts\dev.ps1 cargo test -- --nocapture` works. Where it cannot do that safely (a splatted
array after the `--`), it stops and asks for a quoted `'--'`, which always passes through.

## 2. The gates

`just check` = `names-check pins fmt-check clippy test display-levers doc deny web-check`, and
`web-check` in turn is `web-install web-licences web-build web-typecheck web-lint web-test`. The recipes (see
[justfile](../justfile)):

| Recipe | What it runs |
|---|---|
| `just names-check` | `scripts/check-names.sh`: no private name, no absolute path into a home or another checkout (§11) |
| `just pins` | fails when rust-toolchain.toml and the dev image's base, or package.json's `packageManager` and the image's pnpm, name different versions |
| `just fmt` / `fmt-check` | `cargo fmt --all` / `--check` |
| `just clippy` | `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` |
| `just test` | `cargo test --workspace --all-features --locked` |
| `just display-levers` | proves both entrypoint levers hold and that the X11 test fails, by a panic, without a display (§4) |
| `just doc` | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked` |
| `just deny` | `cargo deny check advisories bans licenses sources` — **needs network** (it clones the RustSec database; the image carries git for exactly this) |
| `just msrv` | `cargo +<rust-version> check --workspace --all-targets --all-features --locked` on Cargo.toml's declared MSRV — **needs network** (rustup fetches that toolchain); a CI job, not part of `check` |
| `just web-licences` | the npm licence gate (§7): `scripts/web-licences.mjs` and its tests |
| `just web-typecheck` / `web-lint` / `web-test` / `web-build` | `pnpm -r typecheck` / `lint` / `test` / `build` |
| `just run-streamer` | prints the streamer's version line and exits 0 |
| `just demo` | builds the web packages and the streamer, starts the streamer, and serves the demo host page on container port 8390 once the streamer is ready |

`web-build` runs before `web-typecheck`/`web-test` inside `web-check` on purpose:
`@appricot/react` resolves `@appricot/client` through its exports map, which points at `dist/`,
so the client must be built before anything can typecheck against it. The web tests also load
each package's own `dist/index.js` under Node's ESM loader (`src/dist-esm.test.ts` in both
packages), so a package is built before its tests run. Both compile under `NodeNext`, which
makes tsc refuse a relative import without its `.js` extension: Node, and a strict bundler,
resolve a specifier exactly as written.

**Read the exit code, never the tail of the log.** Chain commands with `&&`; a `| tail -N`
throws the status away and turns a red gate green.

## 3. Iterating one gate at a time

`just check` is the final word, not the inner loop. While iterating, run the one gate you are
actually touching — it is faster and its failure is the only signal you want:

- Rust formatting drift: `just fmt`, then `just fmt-check`.
- A clippy warning is an error (`-D warnings`); fix the code, never silence the lint.
- `just test` covers the workspace, the X11 integration test and doc tests.
- TypeScript: `web-install` once per dependency change, `web-build`, then `web-typecheck`,
  `web-lint`, `web-test`.
- `just deny` needs network and is slow; run it when dependencies move, not on every edit.

On a fresh clone the lockfiles are already there (see §5), so `web-install` alone brings
`node_modules` in exactly as pinned. Do not run `just lock web-lock` to start: it resolves the
whole graph again, to the newest versions of everything.

## 4. The dev image and its volumes

What the image holds beyond the toolchains:

- **protoc** (`protobuf-compiler`) — prost-build compiles
  `crates/appricot-proto/proto/appricot/v0/wire.proto` at build time. A build tool like `just`,
  never part of the linked graph ADR-0002 gates.
- **Xvfb, xauth, x11-utils, xdotool, x11-apps, fonts-dejavu-core** — the headless X server, the
  readiness probe (xdpyinfo) and window-driving tools (xdotool type/click, xwininfo, xprop) for
  debugging the backend by hand.
- **No X11 -dev headers and no libxcb.** x11rb 0.14.0 has no default features and its
  `RustConnection` speaks the X protocol in pure Rust; the workspace enables neither
  `allow-unsafe-code` nor `dl-libxcb`.

The X server: the entrypoint starts `Xvfb :99 -screen 0 1400x900x24 -nolisten tcp -noreset` and
waits until it answers. `-nolisten tcp` keeps it reachable only through its unix socket inside
the container. `-noreset` is load-bearing: Xvfb resets its state when its **last** client
disconnects, `cargo test --workspace` runs several test binaries back to back in one container,
and a reset between two binaries wipes the window manager's state out from under the next
binary's tests (measured 2026-09-20). Two levers, both honoured rather than silently
overridden: `-e APPRICOT_XVFB=0` starts no X server and leaves `DISPLAY` as it arrived (that is
how you prove the X11 test fails loudly instead of skipping), and `-e DISPLAY=<value>` uses the
caller's display and starts nothing. `just display-levers`, part of `just check`, makes that proof
on every run: it calls the repository's `docker/dev/entrypoint.sh` with each lever and requires
`DISPLAY` to come through as asked and the X11 test to fail by a panic.

The volumes (compose.yml narrates each one):

- The repository is bind-mounted at `/work`, source only.
- `cargo_home` (CARGO_HOME is `/cargo`, off the image's rustup proxies), `target`
  (CARGO_TARGET_DIR) and `pnpm_store` are named volumes: build output and caches live inside
  the Docker Desktop VM's native filesystem, not through the slow bind mount — and debuginfo
  makes `target/` large.
- **One `node_modules` volume per workspace package, plus the root.** pnpm's `node_modules` is
  a symlink farm, and symlinks created through a Windows bind mount are slow and unreliable;
  the per-package volumes hold relative links into the root volume's `node_modules/.pnpm`, both
  sides inside the container. **A new package under `packages/` needs its own volume line in
  compose.yml and its own directory in the Dockerfile** — forgetting the second half is why a
  fresh build suddenly cannot resolve imports.

The image runs as uid/gid 1000 by default (`APPRICOT_UID`/`APPRICOT_GID` build args); CI passes
the runner's own so the checkout stays writable.

## 5. Lockfiles

`Cargo.lock` and `pnpm-lock.yaml` are committed, so a fresh clone needs no resolution step and
every build is reproducible. Only two recipes may rewrite them: `just lock`
(`cargo generate-lockfile`) and `just web-lock` (`pnpm install`). Every other recipe builds
`--locked` / `--frozen-lockfile` and fails loudly on a stale lockfile instead of rewriting it
behind your back. A deliberate dependency change is: edit the manifest, run the matching lock
recipe, commit both files together.

## 6. The demo host page

The demo is a whole host product in miniature: `@appricot/demo` draws the streamed windows as
its own floating windows (title bars, minimise, focus, popups clamped to their parent), served
by a zero-dependency static server under the strict CSP of ADR-0003 — no `'unsafe-inline'`, no
`'unsafe-eval'` in `script-src`, and Trusted Types required for every script sink. Keyboard paste
is opt-in: a toolbar checkbox, off by default, sends the text of a paste into a streamed window
to the app (ADR-0003 §7); unchecked, pasted text stays in the page. From the host:

```sh
docker compose run --rm -p "127.0.0.1:${APPRICOT_DEMO_HOST_PORT:-8390}:8390" dev just demo
```

then open `http://127.0.0.1:8390` and paste the token the recipe prints once at its start. It is
random, 24 bytes from `/dev/urandom` as hex, and fresh for every run; set `APPRICOT_DEMO_TOKEN`
to choose it instead. Ctrl-C stops both processes. The recipe builds the streamer before it
starts anything, so a compile error fails it at once. It serves the page only after the
streamer's `/readyz` answers 200, and when either process exits it stops the other and exits
with that status. A streamer that cannot start, for example with no display, fails the recipe
instead of leaving a page whose every WebSocket fails.

Port 8390 is the repository's **one** published port, loopback-only, overridable through
`APPRICOT_DEMO_HOST_PORT`. The streamer itself is never published: the demo's in-container
server reverse-proxies the `/session` WebSocket upgrade and `/readyz` to the streamer's
loopback bind at `127.0.0.1:8391`, so the streamer keeps its loopback-only rule even in the
manual demo run. The streamer is untrusted (ADR-0003), and the proxy treats it so:

- it accepts the upgrade only from the page itself: a loopback `Host` (`127.0.0.1`,
  `localhost` or `[::1]`, any port) and an `Origin` equal to it, so another site open in the
  same browser cannot drive the session;
- it passes allowlisted headers only, both ways: no cookie goes to the streamer, and no
  `Set-Cookie`, redirect or content type comes back from it;
- it answers a plain request to `/session` with 426, and times out a silent streamer.

## 7. Adding a dependency

The linked core graph is **Tier A only**: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception,
BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Zlib, 0BSD
([ADR-0002](adr/0002-licence.md) §2). `deny.toml` has no deny list — anything not on the allow
list is denied, including unknown licences — and `exceptions = []`. Concretely:

1. Pin the crate in `[workspace.dependencies]` in the root `Cargo.toml`, with a comment naming
   the version, its licence, where that was checked, and the date.
2. `just lock`, then `just deny` (needs network). A denial is the answer, not an obstacle.
3. Anything under GPL, LGPL, AGPL, MPL-2.0, EPL or a source-available licence needs an
   **amendment to ADR-0002 first** — not a `deny.toml` edit. (Amendment 1 admits MPL-2.0 in
   the npm development graph only; see below.)

The npm side has its own gate, `just web-licences` (part of `web-check`), in
[scripts/web-licences.mjs](../scripts/web-licences.mjs):

- Every package in the production graph (`pnpm licenses list --prod`) must have a licence
  expression that Tier A satisfies. The rules are cargo-deny's: an `OR` needs one side, an `AND`
  needs both, and an unknown, missing or unparseable licence fails.
- Every workspace package must declare exactly `MIT OR Apache-2.0` (ADR-0002 §1).
- Its Tier A list must equal `deny.toml`'s `allow` list. Its own tests fail when the two drift.
- Every package only in the development graph (the devDependencies: compiler, linter, test
  runner) must pass the development list: Tier A plus MIT-0, CC0-1.0, BlueOak-1.0.0 and MPL-2.0
  ([ADR-0002, Amendment 1](adr/0002-licence.md#amendment-1-the-npm-development-graph-2026-09-23)).
  These packages are never bundled, because the packages are built with `tsc` alone. GPL, LGPL,
  AGPL, EPL and unknown licences still fail. The gate prints the development packages outside
  Tier A, so what the extras let in stays visible. Its tests fail when the list drifts from the
  amendment.

It reads the installed `node_modules` and needs no network, so run it after `web-install`.

## 8. The untrusted-server rules

The server is untrusted ([ADR-0003](adr/0003-untrusted-server-client.md), Proposed — and
enforced as if Accepted): the streamer shares the application's sandbox, so the client treats
every server byte as hostile. Strings become text, never markup; sizes and counts are checked
against the limits table before anything is allocated; pixels land only in host-given canvases.

This is enforced by lint, not by good intentions. `eslint.config.js` forbids the sinks by name
(`innerHTML`, `outerHTML`, `insertAdjacentHTML`, `setHTMLUnsafe`, `dangerouslySetInnerHTML`,
`document.write`, the navigation globals, every `.location`, `history` and `open` spelling, URL
and style attributes and properties, computed `setAttribute` names, non-literal `import()`, …).
`packages/client/src/untrusted-server.lint.test.ts` proves the set in three steps: the config
holds exactly the entries the test pins, for every file type it lints; each entry, linted alone,
reports its probes, with one probe per name a selector's pattern lists; and the full config
reports every probe and leaves the safe spellings alone. A new rule therefore needs a pin and
probes in that test. A sink reached through an alias (`const w = window`) is beyond a syntactic
rule, so review still owns it. To stay green:

- Render text through `setTextOnly()` (from `@appricot/client`) or as React children — never
  through a sink.
- `packages/client/src/hostile/` proves the whole policy end-to-end in CI: a fixture server
  that sends markup in every string field, oversized lengths, a popup the size of the screen
  and focus requests, none of which may become markup, escape the parent's box, or take focus.
- **Weakening a lint rule turns that test red — fix the code instead.**

## 9. Port policy

No host port is published by default, and the gates need none: the streamer binds loopback or
a unix socket inside its container. The development host runs many other stacks at once, so
before ever adding a port:

1. Check what is taken: the host's listening sockets (`netstat -ano | findstr LISTENING` on
   Windows, `ss -ltnp` on Linux) and every running container's published ports
   (`docker ps --format '{{.Names}}\t{{.Ports}}'`).
2. Pick a free **high** port.
3. Publish it through a named variable bound to loopback, so a collision stays debuggable —
   one `.env` line moves it, and the name says which service it belongs to:

   ```yaml
   ports:
     - "127.0.0.1:${APPRICOT_<SERVICE>_HOST_PORT:-NNNN}:<container port>"
   ```

The comment block at the top of [compose.yml](../compose.yml) is the canonical scheme; update
it when you add a port.

## 10. Where things are written down

The documentation index is [docs/README.md](README.md). Two rules from it bite most often:
every claim about an external project carries a checked URL or the word **(unverified)**, and
every measured number is attributed to the internal benchmark with its date and method — never
rounded, never reworded. Decisions are ADRs under [adr/](adr/README.md); their bodies are
history and are never rewritten, only amended or superseded, and the index table is updated in
the same change.

## 11. The names-and-paths check

AGENTS.md's hard rule keeps two things out of the repository: the names of the owner's private
projects, customers and vendors, and paths into other checkouts on the development machine.
`just names-check` runs `scripts/check-names.sh`, and it is the first step of `just check`.

- **What it reads.** Every tracked file, and every untracked file that `.gitignore` does not
  exclude, so a new file is caught before it is added. It checks contents and file paths. A
  binary file is checked through its runs of printable characters, where embedded metadata sits.
- **Paths, always.** It fails on a drive-letter path (the Windows system folders aside), a Linux
  or macOS home directory in any spelling, Git Bash and WSL included, a WSL share, or a
  home-relative path into a projects or documents folder. Container paths such as `/work`,
  `/target` and `/cargo` are not homes and pass, and so do web URLs.
- **Names, from a denylist that is never in the tree.** Listing the names would put them in the
  repository, so the list lives outside it. The check reads the variable
  `APPRICOT_NAMES_DENYLIST` first; CI fills it from the repository secret of the same name. Then
  it reads `.git/info/names-denylist` (strictly, `info/names-denylist` under
  `git rev-parse --git-common-dir`), which git never tracks. The format is one name per line;
  blank lines and lines starting with `#` are skipped. Case is ignored. A name of five
  characters or more matches anywhere; a shorter one matches only as a whole word, so it does
  not fire inside base64 data. With no list at all, the check says so and runs the path half
  alone: a fresh clone, or a pull request from a fork, where CI passes no secrets.
- **What a hit prints.** `file:line`, or the file alone for a binary file, and never the matched
  text, so the output is safe in a public CI log. A hit in a file's own path is printed as its
  position in `git ls-files --cached --others --exclude-standard`. The exit status is 0 when
  clean, 1 on a hit and 2 when the check cannot run.
- **Git worktrees.** A worktree's `.git` file names its git directory by a host path, which the
  container cannot see, so the check exits 2 there. Mount the main checkout's `.git` into the
  container and set `GIT_DIR` and `GIT_WORK_TREE`, or run the check from the main checkout.

A list only catches the spellings it holds. Add a new name or spelling to the list the day it
becomes relevant, and still read the diff before a commit.

## 12. The capture spike

The M1 spike ([spike/README.md](spike/README.md)) has its own compose service, `spike`, behind
the profile of the same name, so `docker compose build` and `run dev` never touch it. It
`extends` the dev service — the same user, volumes and entrypoint — and its image is the dev
image plus the shared libraries a Qt 6 application on the xcb platform plugin loads. The pilot
application is mounted read-only at `/pilot` from `APPRICOT_PILOT_DIR`, which lives in the
gitignored `.env` with `APPRICOT_PILOT_CMD`; without them `/pilot` is an empty placeholder.

```sh
docker compose --profile spike build spike
docker compose --profile spike run --rm spike just spike-selftest   # the harness, on xclock
docker compose --profile spike run --rm spike just spike --help
```

It publishes no port. Its demo mode reuses the demo page's one port on the command line, as
`just demo` does. A run writes only under `artifacts/spike/`, which is gitignored, and the
application runs in a scratch directory of its own, so nothing it writes lands in the tree.
