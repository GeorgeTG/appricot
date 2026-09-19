# APPricot

APPricot is a load-balanced, sandboxed, preconfigured application streamer. It runs a desktop
application in its own container and streams each of that application's windows into a host web
UI. Each streamed window becomes one of the host UI's own floating windows: the host draws the
title bar, the frame and the taskbar entry, and APPricot fills the window's content. It is not a
remote-desktop canvas. The browser client is a library that a host application embeds. The
proposed design treats the server as untrusted: the client never turns server data into markup,
script or navigation ([ADR-0003](docs/adr/0003-untrusted-server-client.md), Proposed).

The first host application — the web product that will embed the client — is private and lives
in its own repository. It is not named here, and neither is any customer or vendor: APPricot is a
standalone product and this repository is meant to be readable by anyone. The pilot application,
the program APPricot streams first, is a legacy X11 administration tool built on Qt6/xcb.

> **Status: bootstrap (M0).** Nothing is usable yet. The documents under [docs/](docs/) describe
> what is planned. Decisions still open are written as Proposed ADRs.

## Layers

| Layer | What it is | Status |
|---|---|---|
| **L1 streamer** | The wire protocol; `appricot-streamer`, a per-window capture and input server that runs inside the app's container next to a headless display server; and an embeddable browser client. | planned (the only layer with code at bootstrap) |
| **L2 node** | A single-node session manager. App profiles (image, launch command, env, mounts, resource caps, egress policy), one container per session, a warm pool, a readiness gate, a reaper, per-session egress scoping, audit, and a session-ticket API for host apps. | planned (documented, not scaffolded) |
| **L3 broker** | Multi-node placement and session-affine routing. A ticket names the node that owns the session, because a stateful stream cannot migrate. | planned (documented, not scaffolded) |

[docs/architecture.md](docs/architecture.md) has the components, the data path and the trust
boundaries. [docs/roadmap.md](docs/roadmap.md) has the milestones.

## Repository layout

```
crates/
  appricot-proto/      wire protocol v0: types and codec, no I/O, bounded decoding
  appricot-core/       backend-agnostic window/surface model (Wayland-shaped),
                       the CaptureBackend and InputSink traits, damage and frame scheduling
  appricot-x11/        the X11 backend on x11rb: Composite, Damage, XTEST, XFixes,
                       and the minimal window-manager duties
  appricot-encode/     tile encoders (lossless first; codec to be chosen)
  appricot-streamer/   the binary that runs inside the app container
packages/
  client/              @appricot/client: framework-agnostic TypeScript client
  react/               @appricot/react: React bindings (provider, hooks, window canvas)
docker/                dev image and, later, runtime images
docs/                  vision, architecture, roadmap, glossary, ADRs, threat model, protocol
scripts/               dev.sh and dev.ps1: run a command in the dev container
CLAUDE.md              the working rules for this repository (agents read it first)
AGENTS.md              a pointer to CLAUDE.md, for non-Claude agents
LICENSE-MIT            the MIT licence
LICENSE-APACHE         the Apache-2.0 licence
```

## Getting started

Everything runs in Docker through compose. Do not run `cargo`, `pnpm` or `node` on the host.
The compose project name is `appricot`, and no host ports are published.

```sh
docker compose build dev                  # build the dev image
docker compose run --rm dev just check    # format, lint, test, docs, licences, TypeScript
```

`just check` runs `fmt-check`, `clippy`, `test`, `doc` and `deny` for the Rust workspace, then
`web-install`, `web-typecheck`, `web-lint` and `web-test` for the packages.

```sh
docker compose run --rm dev just          # list every recipe
docker compose run --rm dev bash          # a shell, with the headless X server up
```

`scripts/dev.sh <cmd...>` and `scripts/dev.ps1 <cmd...>` are thin wrappers around the same
`docker compose run --rm dev` call; they do not build the image, and they return the command's
own exit code.

`Cargo.lock` and `pnpm-lock.yaml` belong in the repository, so a clean clone needs no resolution
step. `just lock` and `just web-lock` are the only recipes that may rewrite them. The toolchain
versions are pinned in the repository: Rust in `rust-toolchain.toml`, pnpm through corepack in
`package.json`, and the rest in `docker/dev/Dockerfile`.

`just deny` clones the RustSec advisory database, so `just check` needs network access.

## Documents

- [CLAUDE.md](CLAUDE.md): the working rules — gates, ports, the untrusted-server rule, the licence
  rules, the window model, the crate map, and what may and may not be said about the first host
  application. [AGENTS.md](AGENTS.md) points other agents at it.
- [docs/vision.md](docs/vision.md): the problem, who it is for, what it is not, the principles.
- [docs/architecture.md](docs/architecture.md): components per layer, data path, trust boundaries,
  the window model, the backend matrix, and what is re-implemented from earlier work.
- [docs/roadmap.md](docs/roadmap.md): milestones M0-M6 and their exit criteria.
- [docs/glossary.md](docs/glossary.md): the words this project uses.
- [docs/adr/](docs/adr/README.md): the decision records.
- [docs/security/threat-model.md](docs/security/threat-model.md): assets, adversaries, controls.
- [docs/protocol/](docs/protocol/README.md): the wire protocol (v0 is not designed yet).
- [docs/prior-art.md](docs/prior-art.md): related projects and what we take from them.

## Licence

APPricot is dual-licensed under either of

- the MIT licence ([LICENSE-MIT](LICENSE-MIT)), or
- the Apache Licence, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)),

at your option. This is the Rust ecosystem's convention, and it keeps the client embeddable in a
host product on either licence. Decided 2026-09-20; see
[ADR-0002](docs/adr/0002-licence.md).

Unless you state otherwise, any contribution you intentionally submit for inclusion in this work,
as defined in the Apache-2.0 licence, is dual-licensed as above, with no additional terms or
conditions.

Author: George Gougoudis <george_gougoudis@hotmail.com>
