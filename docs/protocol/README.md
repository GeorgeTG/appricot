# Wire protocol

**Status: placeholder.** Protocol v0 is not designed yet. It is designed in the task
`l1-wire-spec-v0` (milestone M1, [roadmap.md](../roadmap.md)), together with the X11 capture
spike, and the spec will live in this folder. The Rust types and codec live in
`crates/appricot-proto`; the TypeScript mirror lives in `packages/client`.

A first message list was sketched during the design research that preceded this repository. It is
input to the design, not the design. The window model the protocol carries is in
[architecture.md §4](../architecture.md#4-the-window-model); the measurements that make a
purpose-built protocol worth the work are in
[vision.md](../vision.md#3-forward-the-display-protocol-itself).

## Design rules

Every v0 message must follow these rules. A proposed message that breaks one needs an ADR.
Rules 3, 4, 6, 7 and 8 carry [ADR-0003](../adr/0003-untrusted-server-client.md) and the handshake
of [ADR-0004](../adr/0004-layers-window-model-and-first-backend.md) into the wire. Both ADRs are
still Proposed; if the user amends them, these rules change with them.

1. **Versioned.** The first messages negotiate a protocol version. A peer refuses a version it
   does not support and closes; it never guesses. Within a major version, changes only add.
   How an unknown message type is handled (skip by length, or close) is decided in v0 and written
   down.
2. **Binary WebSocket frames.** One protocol message per WebSocket binary message. No text
   frames. Fixed byte order (little-endian unless v0 says otherwise). No JSON, no
   self-describing encoding that a decoder must interpret freely.
3. **Bounded.** Every length, count and dimension has a maximum in a limits table in the spec:
   message size, string lengths, surfaces per session, popups per parent, rectangles per frame,
   tile width and height, pixels in flight, cursor and icon size, clipboard size. A decoder checks
   the limit **before** it allocates or loops. A violation closes the connection. Both sides
   enforce the same table; the node proxy enforces the message size cap in both directions.
4. **Text-only strings.** Strings are UTF-8, validated, and capped per field. They are data for
   display as text. No field carries HTML, markup, a URL, a file path the client should open, or
   anything the client would execute or navigate to
   ([ADR-0003](../adr/0003-untrusted-server-client.md)).
5. **Flow control.** Frames carry a per-surface sequence number. The client acks what it has
   drawn. The server keeps at most a small fixed number of frames in flight per surface and
   coalesces damage while it waits. Nothing is sent on a timer; an idle window sends nothing.
   Relays buffer within a bound and never grow without one.
6. **The host decides.** Size, position, stacking and focus are the host's. The server may ask
   (a configure request, a focus request); the client reports the request and the host answers.
   Configure proposals carry a serial, and the server acks with that serial.
7. **Surface-local coordinates.** Input and popup placement are expressed relative to a surface,
   never in a global screen space that would let a server aim outside its windows.
8. **Authentication first.** The first client message carries the credential for that leg (a
   ticket to the node, a stream token to the streamer). Nothing else is accepted before it. No
   credential ever travels in a URL.
9. **No I/O in the codec.** `appricot-proto` turns bytes into typed messages and back. Sockets,
   timers and retries belong to the streamer and the client.
10. **Test vectors are shared.** The Rust codec generates the test vectors; the TypeScript codec
    must pass them. Both decoders are fuzzed.
11. **A dropped transport is not a dead session.** The session belongs to the node, not to the
    socket. Losing the connection never ends the application; a client reattaches with its ticket
    and is resynchronised — the server re-sends the window set and a full frame per surface rather
    than replaying damage the client may have missed. How much state a reattach may carry, and for
    how long a session waits for one, are v0 questions with answers written down.
