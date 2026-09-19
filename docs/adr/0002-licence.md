# ADR-0002 — Licence of APPricot, and a permissive-only core graph

**Status**: Accepted (2026-09-20, by the user). This ADR was Proposed on 2026-09-19; the user
decided on 2026-09-20 and the decision below is the accepted one. The "Alternatives considered"
section is kept as the history of that decision.

## Context

Two licence questions are open, and they are separate.

1. **What licence does APPricot's own code carry?** This decides who may use, change and ship it.
2. **What licences may APPricot's dependencies carry?** This decides what every consumer inherits
   when it links the crates or bundles the npm packages.

Facts that bear on both:

- **The first consumer is distributed software.** It is installed on a server per customer
  company, so each install conveys the software to a separate legal entity. Whatever APPricot
  ships inside it is conveyed too.
- **The client ships to other people's browsers.** `@appricot/client` is bundled into each host
  app's page. Its licence and notices travel inside that bundle, to every end user.
- **The streamer ships inside other people's containers**, next to the application it streams.
- **A component's licence is inherited by every consumer.** A copyleft dependency anywhere in the
  linked graph would reach each of them, and each would have to answer for it.
- **Copyleft options were studied as components and not chosen.** KasmVNC is GPL-2.0
  ([`LICENSE.TXT`](https://raw.githubusercontent.com/kasmtech/KasmVNC/master/LICENSE.TXT)),
  xpra is GPLv2+ ([`setup.py`](https://raw.githubusercontent.com/Xpra-org/xpra/master/setup.py)),
  Greenfield is `AGPL-3.0-or-later`
  ([`package.json`](https://raw.githubusercontent.com/udevbe/greenfield/master/packages/compositor-proxy-cli/package.json)),
  all checked 2026-09-19.

## Decision

### 1. APPricot's own code: `MIT OR Apache-2.0`

APPricot is dual-licensed under **MIT OR Apache-2.0**, at the recipient's option. The author is
**George Gougoudis \<george_gougoudis@hotmail.com\>**.

What this obliges in the tree:

- `LICENSE-MIT` and `LICENSE-APACHE` at the repository root, with the full text of each.
- Every crate under `crates/` sets `license = "MIT OR Apache-2.0"`, and every package under
  `packages/` sets `"license": "MIT OR Apache-2.0"`.
- The README states the licence and points at both files.
- `publish = false` / `"private": true` stay until there is something worth releasing. That is a
  release decision, not a licence one; the licence no longer blocks it.

Why:

- It is the Rust ecosystem's own convention. The Rust API Guidelines recommend
  `license = "MIT OR Apache-2.0"` for "maximum compatibility with the Rust ecosystem"
  ([C-PERMISSIVE](https://rust-lang.github.io/api-guidelines/necessities.html), checked
  2026-09-19).
- Apache-2.0 adds an express patent licence, which ends for anyone who sues over patents in the
  work ([Apache-2.0 §3](https://www.apache.org/licenses/LICENSE-2.0), checked 2026-09-19). MIT
  keeps compatibility with projects that cannot take Apache-2.0 terms. The recipient picks.
- **Any product can embed it, including a private one.** A host application adds a notice and
  ships. No contract, no exception entry, no separate grant.

### 2. The core linked graph is permissive only

The crates under `crates/` and the packages under `packages/` may depend only on packages
licensed under:

**MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC, Zlib,
Unicode-3.0, 0BSD.**

- **Copyleft and source-available licences stay out of the linked graph.** GPL, LGPL and AGPL are
  never allowed. File-level copyleft (MPL-2.0, EPL-2.0, CDDL and similar) is not allowed either:
  every exception APPricot took would become an exception each consumer has to take as well.
- **The gate is mechanical.** `Cargo.lock` is checked by `cargo deny` against exactly the list
  above (`deny.toml`), and `pnpm-lock.yaml` by a licence check in CI. A graph nobody checks
  drifts, so both graphs are gated, not just the one that is easy to check.
- Anything not on the list fails the gate, including an unknown, unparseable or missing licence.
- Dual-licensed packages pass when one of their options is on the list.
- There is **no exception process**. Adding one — for example for an MPL-2.0 codec — needs a dated
  amendment to this ADR first, and then a change to `deny.toml`.

### 3. What this ADR does not govern

- **OS packages inside runtime images** (the X server, fonts, the libraries an application needs)
  are aggregation, not linking. They carry notice and source-offer duties instead, and are listed
  per image when the first image is published.
- **The streamed application.** Each app profile names an application whose licence is its owner's
  business, and some forbid redistribution outright. In that case the operator supplies the binary
  on their own server and the profile mounts it. **APPricot never bundles an application.**
- **Reading other projects.** Studying GPL or AGPL code to learn a design is allowed. Copying it
  is not (see [prior-art.md](../prior-art.md)).

## Consequences

- **Anyone may build a closed product on APPricot**, including a competitor to its author's own
  products. That is the cost of the permissive choice, and it is what makes an embeddable
  component adoptable.
- **Codec choices narrow.** On crates.io (checked 2026-09-19), `jpeg-encoder` 0.7.1 is
  "(MIT OR Apache-2.0) AND IJG" ([crates.io](https://crates.io/crates/jpeg-encoder)) and
  `env-libvpx-sys` 5.1.3, under the VP8/VP9 bindings, is MPL-2.0
  ([crates.io](https://crates.io/crates/env-libvpx-sys)). Both fail the gate as it stands.
  Lossless encoders remain: `qoi` 0.4.1 is "MIT/Apache-2.0", `image-webp` 0.2.4 and `png` 0.18.1
  are "MIT OR Apache-2.0" (crates.io, same date). The M1 spike picks among them.
- **A Wayland backend on smithay passes** the licence gate: smithay's crates.io licence is MIT
  ([crates.io](https://crates.io/api/v1/crates/smithay/versions), checked 2026-09-19). Its full
  transitive graph has not been resolved yet (unverified).
- The publication hold is lifted. Nothing was publishable while the licence was undecided; now the
  only thing between a crate and a release is having something worth releasing.

## Alternatives considered

| Option | For | Against | Verdict |
|---|---|---|---|
| **MIT OR Apache-2.0** | Rust convention; patent grant available; permissive, so every consumer takes it without an exception; easy to embed | Allows closed forks and competitors | **Accepted** 2026-09-20 |
| Apache-2.0 only | Patent grant always applies | The Rust API Guidelines say Apache-only "imposes restrictions beyond the MIT and BSD licenses that can discourage or prevent their use in some scenarios" ([C-PERMISSIVE](https://rust-lang.github.io/api-guidelines/necessities.html)) | Rejected: worse fit for a library |
| MIT only | Shortest; widest compatibility | No express patent licence | Rejected: the patent grant is worth the extra file |
| MPL-2.0 | Changes to APPricot's own files must be shared, while "allowing them to combine your code with code under other licenses" ([MPL FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/)) | File-level copyleft in the graph of every consumer; a named exception in every consumer's dependency policy, forever | Rejected |
| AGPL-3.0, with a commercial licence on the side | Stops closed SaaS forks: §13 requires offering source to "all users interacting with it remotely through a computer network" ([AGPL-3.0](https://www.gnu.org/licenses/agpl-3.0.html)) | The first consumer could not ship it without a separate commercial grant, and every host app that embeds the client would face the same | Rejected |
| Source-available (BUSL-style) | Keeps commercial control | Restricts use; not open source; no consumer with a permissive-only policy could take it | Rejected |
| Proprietary, with grants to named consumers | Full control | Every host app needs a contract; customers of every consumer would receive a proprietary component; no outside adoption | Rejected |

The second decision (a permissive-only core graph) had one alternative worth naming: **permissive
plus file-level copyleft by named exception**. It is rejected because every exception APPricot
takes becomes an exception every consumer must take too. It can be revisited per package by
amendment.
