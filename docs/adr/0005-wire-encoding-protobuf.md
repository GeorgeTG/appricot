# ADR-0005 — Wire encoding: Protocol Buffers (proto3)

**Status**: Proposed (2026-09-20). The user has not accepted it. It is proposed together with
the v0 spec ([protocol/v0.md](../protocol/v0.md)), which transcribes the message set it governs.

## Context

The protocol's design rules ([protocol/README.md](../protocol/README.md)) demand, before any
message is written: binary WebSocket frames with a fixed byte order (rule 2); a bounded,
non-self-describing encoding whose every length is checked before allocation (rule 3); and two
independent codecs — Rust in `crates/appricot-proto`, TypeScript in `packages/client` — proven
against each other by shared test vectors (rule 10). No I/O in either codec (rule 9). The linked
dependency graphs are permissive-only ([ADR-0002](0002-licence.md)), so whatever encodes the
wire must satisfy the licence gate on the Rust side and stay dependency-light on the TypeScript
side, where `@appricot/client` is bundled into host pages.

The choice of encoding was open. What exists before this ADR is a sketch of a bespoke format in
`crates/appricot-proto/src/bounded.rs`: length-prefixed strings with the length checked before
the payload is touched. That sketch stays — it is the bounded-string discipline every codec
needs — but it is not an envelope.

Two requirements pushed away from a fully bespoke format. First, the unknown-message rule
(rule 1: decide skip-or-close in v0) and the unknown-enum question both need well-defined
decoder behaviour a bespoke format would have to invent, test and document alone. Second, the
vectors-of-byte-identity contract (rule 10) is only strong if both codecs agree on wire details
— field order, varint boundaries, default-value elision — that a hand-rolled format leaves to
convention.

## Decision (proposed)

**The v0 wire is Protocol Buffers, proto3** ([proto3 language guide](https://protobuf.dev/programming-guides/proto3/),
checked 2026-09-20), one `.proto` file as the single source of truth:
`crates/appricot-proto/proto/appricot/v0/wire.proto`.

- **Rust side**: the messages are compiled by prost, 0.14.4, Apache-2.0, published 2026-06-07
  ([crates.io](https://crates.io/api/v1/crates/prost), checked 2026-09-20). Apache-2.0 is on
  the permissive list of [ADR-0002](0002-licence.md) §2, so the codec dependency passes the
  licence gate.
- **TypeScript side**: a hand-written, zero-dependency codec in
  `packages/client/src/wire.ts` (re-exported through `protocol.ts`), mirroring the `.proto`
  field by field. No protobuf runtime is bundled into host pages.
- **The two codecs are gated together** by shared byte-exact test vectors
  ([protocol/v0.md §12](../protocol/v0.md)): the Rust side generates them, the TypeScript side
  must decode and re-encode every one to the same bytes.
- **Deterministic field order is a protocol requirement, stricter than protobuf's.** Protobuf
  does not promise field order on the wire; this protocol requires both encoders to emit fields
  in field-number order, so `encode(decode(x))` is byte-identical. That determinism is what
  makes the hand-written mirror checkable by vectors at all.
- **Unknown-message handling is the envelope oneof.** Every message rides in an `Envelope` whose
  oneof names exactly one body. An envelope naming no known field is a violation and the peer
  closes — rule 1's skip-or-close question is answered "close", trivially, because the oneof
  makes the case unambiguous. There is no skip-by-length in v0.
- **Unknown enum values are a violation, decided when the Rust codec landed.** Protobuf
  implementations disagree on unrecognized enum values — proto3 says they "will be preserved in
  the message" ([proto3 guide](https://protobuf.dev/programming-guides/proto3/), checked
  2026-09-20), but prost rejects them on decode. Rather than let the Rust and TypeScript codecs
  diverge silently, v0 takes the strictest common rule: an unknown enum value inside a known
  message closes the connection, and both codecs enforce the closed sets in both directions.
  New enum values ship only with a new protocol version.

Where the other ADRs shaped the fields: the text-only, capped-string rules of
[ADR-0003](0003-untrusted-server-client.md) produced `string` fields that are untrusted text
with a per-field cap in the limits table, and bounds (message, tile, cursor, clipboard) checked
before allocation. The host-plays-compositor handshake of
[ADR-0004](0004-layers-window-model-and-first-backend.md) produced `Configure`/`ConfigureAck`
with a serial, `FocusAsk` versus `FocusNotify`, and the positioner-carrying popup messages.

## Consequences

- **`protoc` enters the dev image as a build tool.** prost compiles the `.proto` at build time
  and needs the protobuf compiler available in the image. It is a build tool, not a linked
  dependency, so it sits outside the linked-graph gate of [ADR-0002](0002-licence.md) §2 — the
  same treatment OS packages in runtime images get in its §3.
- **The TypeScript codec is hand-maintained** until generated code is worth its toolchain.
  Revisit criterion: more than about 40 messages, or a third consumer of the encoding. v0 has 24
  messages and two consumers.
- **The vectors are load-bearing, not decoration.** With one generated and one hand-written
  codec, byte-exact vectors are the only mechanical proof the TypeScript side speaks the same
  bytes. A message change that touches the `.proto` must regenerate the vectors in the same
  change.
- **Determinism costs a little freedom.** Both encoders must order fields by number and agree on
  when a default is elided; a future protobuf implementation detail cannot be inherited blindly,
  because the vectors pin the bytes.
- **Adding a message inside major 0 needs deployment care**
  ([protocol/v0.md §13](../protocol/v0.md)): an older peer closes on an unknown envelope message
  rather than skipping it.

## Alternatives considered

- **A custom length-prefixed TLV** (the `bounded.rs` sketch grown into an envelope). Full
  control, zero new dependencies, and exactly the bytes we choose. Rejected as the envelope:
  two bespoke codecs to write and fuzz with no tooling, every decoder behaviour (unknown field,
  unknown enum, nesting) specified and tested from nothing, and no schema language for review.
  The bounded-string discipline survives inside the decoders.
- **FlatBuffers** ([flatbuffers.dev](https://flatbuffers.dev/), checked 2026-09-20). Zero-copy
  reads without decoding, schema language, TypeScript support. Rejected: a heavier TypeScript
  story (generated code plus a runtime in host bundles) for a protocol whose messages are small
  and whose hot path is tile bytes we pass through untouched anyway; its current maintenance
  status was not verified (unverified).
- **Cap'n Proto** ([capnproto.org](https://capnproto.org/), checked 2026-09-20: "an insanely
  fast data interchange format and capability-based RPC system"). The same zero-copy argument,
  plus an RPC machinery this protocol does not need — APPricot has one transport (a WebSocket)
  and its own session semantics. Rejected.
- **JSON.** Rejected outright by design rule 2: no self-describing encoding a decoder must
  interpret freely, and no text frames on the wire.
