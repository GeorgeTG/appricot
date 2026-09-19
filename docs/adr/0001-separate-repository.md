# ADR-0001 — A separate repository from day one

**Status**: Accepted (2026-09-19, by the user)

## Context

APPricot streams the individual windows of a sandboxed desktop application into a host web UI,
where each one becomes one of that UI's own floating windows. It is three things: a wire
protocol, a capture and input server that runs beside the application, and an embeddable browser
client.

The work started from a concrete need. A web product had to show a desktop-only administration
tool inside its own UI, and was doing it by streaming a whole virtual screen through a
third-party web VNC stack. In September 2026 a research pass asked whether an existing transport
could do better, and found two things that shaped this repository:

- **Per-window capture and input need no exotic display server.** On a plain headless X server,
  the Composite extension's `NameWindowPixmap` returns one window's clean pixels while another
  window covers it, and XTEST types into that window once X focus is set on it. What is missing
  from the existing stacks is not a capability of the display server; it is a protocol and a
  client that carry windows instead of a screen.
- **The browser side is where the risk lives.** Every third-party web client examined turned
  server-sent strings into markup. That finding is the subject of
  [ADR-0003](0003-untrusted-server-client.md) and is not re-argued here.

The user then decided to build a **first-party per-window streamer**. The question this ADR
answers is where that streamer lives.

Nothing in capture, encoding, the window model or the browser client knows anything about the
application being streamed. The first consumer is **a private application in a separate
repository**, which is not named here and whose internals are not APPricot's business. It needs
the streamer first, but it is one consumer of a component that other products can use for other
desktop-only applications, and it has its own dependency policy, its own CI and its own release
cadence.

## Decision

APPricot lives in its **own repository** from day one, with its own compose project (`appricot`),
its own toolchain pins, its own CI and its own licence ([ADR-0002](0002-licence.md)).

The user decided the sentence above. The points below spell out what a separate repository means
here. They can be amended without re-opening the decision.

- **A host application is a consumer, not a host repository.** It consumes APPricot's released
  artefacts — the streamer binary or image layer, and the npm packages `@appricot/client` and
  `@appricot/react`. It does not vendor APPricot's source.
- **APPricot never reads a consumer's tables, types or configuration.** Anything specific to one
  consumer (an app profile, the egress scope a particular deployment needs) stays on that
  consumer's side of a documented interface.
- **Work in either repository never edits the other.** The first host integration (M3) is done in
  the consumer's repository, by that project's own process and review.
- **The pilot application is the first app profile**, not a special case in the code. A profile is
  data: image, launch command, env, mounts, resource caps, egress policy.

## Consequences

- APPricot can define a clean boundary — the wire protocol, the client API, later the ticket API
  — without any consumer's internals leaking into it.
- Two release trains. A consumer pins an APPricot version, and every APPricot release it takes
  needs a check on the consumer's side. This is the price of a reusable component.
- APPricot's licence is decided on its own ([ADR-0002](0002-licence.md): `MIT OR Apache-2.0`). It
  is permissive, so a consumer whose policy allows only permissive dependencies takes APPricot
  with no exception entry.
- Duplication for a while. The first host application keeps its own container supervision, warm
  pool, reaper and egress scoping until APPricot's L2 node exists (M4). APPricot re-implements
  those designs, informed by what that deployment recorded
  ([architecture.md](../architecture.md), the section on lessons carried over).
- Tooling, CI and the Docker dev setup must be built again for this repository. The bootstrap
  cohort (M0) does that.

## Alternatives considered

- **A crate and a package inside the first consumer, split out later.** Rejected by the user. It
  is the cheapest start, but the streamer would grow around that application's service, its types
  and its UI store. Splitting a grown component out later costs more than starting apart, and the
  boundary would be shaped by one consumer rather than by the problem.
- **A workspace member in the consumer's repository, with a separate npm package.** Rejected for
  the same reason, and because it puts APPricot under another project's licence decisions and CI
  by default.
- **Extend an existing project (xpra, KasmVNC) upstream.** Not a repository question on its own.
  Both are GPL-licensed: KasmVNC's `LICENSE.TXT` is GPL version 2
  ([source](https://raw.githubusercontent.com/kasmtech/KasmVNC/master/LICENSE.TXT)) and xpra's
  `setup.py` says "GPLv2+" ([source](https://raw.githubusercontent.com/Xpra-org/xpra/master/setup.py)),
  both checked 2026-09-19. Their web clients are the problem
  ([ADR-0003](0003-untrusted-server-client.md)). A server-side adapter to an unmodified xpra stays
  open as shape S3 ([ADR-0004](0004-layers-window-model-and-first-backend.md)), and it would still
  live in this repository.
