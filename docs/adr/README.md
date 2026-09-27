# Architecture decision records

Each ADR records one decision: its **Status**, the **Context** that forced it, the **Decision**,
its **Consequences**, and the **Alternatives considered**.

## Rules

- **Status** is one of Proposed, Accepted, Superseded or Rejected, with a date. "Accepted" names
  who accepted it.
- **Accepted ADRs win** over prose anywhere else in `docs/`. A Proposed ADR does not override
  anything; it is a recommendation waiting for a decision.
- An ADR is not rewritten after it is accepted. A later change is a dated **Amendment** section
  at the end, or a new ADR that supersedes it.
- Numbers are four digits, never reused. File names are `NNNN-short-title.md`.
- Claims about external projects (versions, dates, licences, capabilities) carry a URL that was
  checked, or the word "(unverified)".
- Measurements carry their method and their date. A number without one is not evidence.

## Index

| ADR | Title | Status |
|---|---|---|
| [0001](0001-separate-repository.md) | A separate repository from day one | **Accepted** 2026-09-19 (the user) |
| [0002](0002-licence.md) | Licence of APPricot, and a permissive-only core graph | **Accepted** 2026-09-20 (the user); Amendment 1, the npm development graph, 2026-09-23 (the user); Amendment 2, the release decision §1 deferred, 2026-09-27 (the user) |
| [0003](0003-untrusted-server-client.md) | The client treats the server as untrusted | Proposed 2026-09-19 |
| [0004](0004-layers-window-model-and-first-backend.md) | Three layers, a Wayland-shaped window model, and X11 first | **Accepted** 2026-09-24 (the user), after the M1 spike; proposed 2026-09-19 |
| [0005](0005-wire-encoding-protobuf.md) | Wire encoding: Protocol Buffers (proto3) | Proposed 2026-09-20 |
