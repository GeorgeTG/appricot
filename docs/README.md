# APPricot documentation

This folder is everything written down about the project: what it is, how it is built, what the
wire says, where it is going, and how to work on it. The working contract for a change — the
gates, the hard rules — is [AGENTS.md](../AGENTS.md) at the repository root; these pages are the
reasons and the detail behind it.

## Reading order for a newcomer

1. [vision.md](vision.md) — the problem (desktop-only tools that must live in a web product) and
   the product APPricot is: a per-window, sandboxed, preconfigured application streamer.
2. [architecture.md](architecture.md) — the three layers, the as-built L1 streamer, the data
   path, the window model, the trust boundaries and the backend matrix.
3. [protocol/README.md](protocol/README.md), then
   [protocol/v0.md](protocol/v0.md) — the wire protocol's design rules and the v0 spec:
   every message, the limits table, flow control, reattachment.
4. [roadmap.md](roadmap.md) — milestones M0-M6 and each one's exit criteria, with a note of
   where the code stands today.
5. [adr/](adr/README.md) — the decision records. An Accepted ADR outranks every other document
   here; a Proposed one is a recommendation that code follows but the user has not adopted.
6. [security/threat-model.md](security/threat-model.md) — assets, adversaries and controls, and
   why the client treats the server as untrusted.
7. [glossary.md](glossary.md) — the project's words, in the sense it uses them.
8. [prior-art.md](prior-art.md) — the systems that came before, compared and cited.
9. [development.md](development.md) — how to build, gate, run and extend this repository.
10. [spike/README.md](spike/README.md) — the M1 capture spike's harness, and its findings.

## The pages

| Page | What it holds |
|---|---|
| [vision.md](vision.md) | The problem, the four bad roads that exist today, what APPricot is and is not, the product principles, and how we will know it works. |
| [architecture.md](architecture.md) | The as-built L1 (crates, packages, dependency edges), the documented L2/L3, the pixel and input paths, the Wayland-shaped window model, trust boundaries, the backend matrix and the pilot-deployment lessons. |
| [protocol/README.md](protocol/README.md) | The eleven design rules every wire message must satisfy. |
| [protocol/v0.md](protocol/v0.md) | The v0 spec: encoding, handshake, the limits table, all 24 messages, flow control, reattachment, keyboard and pointer mapping, popup placement, tile codecs, versioning. |
| [roadmap.md](roadmap.md) | M0-M6, their tasks and exit criteria. |
| [adr/README.md](adr/README.md) | The ADR rules and the index of ADR-0001 to ADR-0005. |
| [security/threat-model.md](security/threat-model.md) | The first-cut threat model: assets, adversaries, planned controls per adversary. |
| [glossary.md](glossary.md) | Every term of art, one entry each. |
| [prior-art.md](prior-art.md) | Prior art, from browser remote desktops to per-window forwarding and commercial application streaming, with checked citations. |
| [development.md](development.md) | The developer guide: Docker-only rule, the gates, the dev image, lockfiles, the demo run, dependencies, the untrusted-server lint rules, port policy, the spike service. |
| [spike/README.md](spike/README.md) | The M1 capture spike's harness: what a run starts, how to run and steer it, what it writes. Dated findings sit next to it. |

## The diagrams

All three live in [diagrams/](diagrams/) and are embedded where they belong in
[architecture.md](architecture.md).

| Diagram | What it shows |
|---|---|
| [diagrams/shape.svg](diagrams/shape.svg) | The shape in one picture: host app backend, browser host page, node (edge router and proxy, planned) and the sandboxed app container — Xvfb, the streamed application and `appricot-streamer` behind a loopback or unix socket with a per-session stream token. |
| [diagrams/data-path.svg](diagrams/data-path.svg) | The data path: app draws, Composite off-screen pixmap, DamageNotify, per-surface damage, the credit check, capture, 256-grid tiles in RAW or in-house QOI, the Frame on the wire, bounded decode into the host canvas, and the FrameAck after the draw; input flows the other way. |
| [diagrams/code-map.svg](diagrams/code-map.svg) | The code map: the five Rust crates and three TypeScript packages, what each owns, and the one-way dependency edges between them. |

## Rules these pages follow

The rules are in [AGENTS.md](../AGENTS.md) ("Docs"); the ones that bite most often:

- English everywhere. Plain, short sentences. Prose wrapped at about 100 characters.
- Every claim about an external project carries a URL that was checked, or the word
  **(unverified)**. A number says where it comes from: measured figures are attributed to the
  internal benchmark, with date and method, and are never rounded or reworded.
- Relative links resolve, anchors included. The pages under [adr/](adr/README.md) are history:
  bodies are never rewritten, only amended or superseded.
