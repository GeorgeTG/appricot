# Threat model (first cut)

**Status: Draft** (written 2026-09-19, before any code existed; revised 2026-09-23 after L1
landed, M1's code side and M2). Each control row says where it is enforced and whether it is
**live** in the landed L1 code, with the file and the test that prove it, or still **planned**.
L2 and L3 are not built, so every control they own is planned. The client-side controls come
from [ADR-0003](../adr/0003-untrusted-server-client.md), which is still Proposed.

## 1. The motivating case

The pilot application is an administration tool that speaks to remote devices. It runs in a
container and its windows are streamed into a web UI. A hostile device that exploits the
application or its toolkit owns that session.

In September 2026 a review asked what such a session could do to the operator's browser. The
answer:

- The web client in the operator's page was a third-party build of noVNC. It wrote the fields of
  one server message, type 178, into `innerHTML` without escaping
  ([kasmtech/noVNC@ae1e012f `app/ui.js:1384`](https://github.com/kasmtech/noVNC/blob/ae1e012f/app/ui.js#L1384),
  checked 2026-09-19).
- A session that replaced its VNC server needed only a handshake and one type-178 message carrying
  `<img src=x onerror=...>`. The next time the operator's client connected, script would run in a
  frame that was **same-origin with the host application's own single-page app**. From there it
  could read anything the operator typed anywhere in that application. (Traced in source; not
  executed.)
- The same class of sink was then found in xpra-html5's window-title code
  ([`Window.js:653`](https://github.com/Xpra-org/xpra-html5/blob/master/html5/js/Window.js#L653),
  checked 2026-09-19; demonstrated in an internal benchmark, September 2026).

The lesson APPricot is built on: **the server side of a stream is untrusted, and the client is
the last line.** A per-window design adds a second lesson: without an iframe, the server's pixels
land directly in the host page, so they must be confined too.

## 2. Assets

| # | Asset | Why it matters |
|---|---|---|
| A1 | **The host app's origin and realm** | Script in the host's realm reads its DOM, its state and its users' input, and acts with their session. This is the asset the type-178 sink exposed. |
| A2 | **Operator input** | Keystrokes, clicks and pastes. Input meant for the host (a password in a host form) must never reach a session. Input meant for a session reaches a possibly hostile application by design. |
| A3 | **The clipboard** | The user's clipboard may hold secrets. A session must not read it without a paste, or write to it without the host's policy and a user gesture. |
| A4 | **Other sessions** | Another user's session, its windows, its input and its credentials. An administration tool commonly caches the credentials of the systems it manages in its home directory, and the pilot application is assumed to do the same (unverified). |
| A5 | **The node** | The host kernel, the container engine, the node daemon, its secrets (stream tokens, ticket keys), and the networks the node can reach. |
| A6 | **Egress reach** | The networks an application may reach are part of its profile. In the first host integration, one device address and one TCP port. A session that reaches more is a pivot. |
| A7 | **The stream itself** | The pixels and text of a session: device configuration, customer data. |

## 3. Adversaries

| # | Adversary | Starting point | Goal |
|---|---|---|---|
| ADV1 | **A compromised application inside its own sandbox** | The attacker has full control of the streamed application's process, and therefore of everything in that one container — the streamer beside it included — but nothing outside it. The motivating case: a hostile remote device feeding the pilot application. | Reach A1-A6 from inside one session. |
| ADV2 | **A tenant trying to reach a sibling session** | A legitimate user of some host app, with valid tickets for their own sessions, who aims them at somebody else's. | Reach another user's session (A4), the node (A5), or more egress (A6). |
| ADV3 | **A network attacker** | On the path between the browser and the edge, or between internal components. | Read or change the stream (A7), steal a ticket, hijack a session. |

Not in scope for the first cut: a malicious host app (it already owns A1-A3 of its own users),
a compromised node administrator, and supply-chain attacks on our dependencies (covered by the
licence and dependency gates of [ADR-0002](../adr/0002-licence.md), to be extended).

## 4. Controls, per adversary

"Where" names the component that enforces the control. **The streamer shares the application's
sandbox**, so a control enforced only by the streamer does not hold against ADV1. Each such row
names a second enforcement point outside the sandbox. "State" says what the landed code does
today (2026-09-23): **live**, with the file and the test, or **planned**.

### 4.1 ADV1: a compromised application

| Threat | Control | Where | State |
|---|---|---|---|
| Script into the host realm through a string (A1) | Strings are text only; no HTML sinks; no `eval`; the client runs under a CSP with no `'unsafe-inline'`/`'unsafe-eval'` and under `require-trusted-types-for 'script'` ([ADR-0003](../adr/0003-untrusted-server-client.md) §1-§3) | `@app-ricot/client`, `@app-ricot/react`; ESLint rules in CI | **Live.** Strings are written as text (`setTextOnly` in `packages/client/src/text-only.ts`, or React children). `eslint.config.js` forbids the sinks, and `untrusted-server.lint.test.ts` proves each rule fires. `hostile/hostile.test.ts` row (a) sends markup in every string. The CSP belongs to the host page: the demo sets it, Trusted Types included (`packages/demo/src/server.ts`, asserted in `server.test.ts`), and every other host must set its own. |
| Script or navigation through a URL (A1) | The protocol has no URL field and no "open" message; icons and cursors arrive as pixels; the client makes no URL from server data | protocol spec (`appricot-proto`); client | **Live.** `wire.proto` has no URL field and no such message. A cursor becomes `ImageData` (`packages/client/src/cursor.ts`). Navigation sinks are forbidden by the lint rules above. |
| Parser exploit in the host realm (A1) | Every length and count bounded before allocation; decoders in Workers where possible; fuzzing of the TS and Rust decoders; browser image decoders for image codecs | `appricot-proto`, client; fuzz tests in CI | **Partly live.** Bounds before allocation: the TS decoder (`packages/client/src/wire.ts`, hostile row (b)) and the Rust decoder, whose `decode_envelope` pre-scans every count and length on the raw bytes before prost allocates (`crates/appricot-proto/src/prescan.rs`; `tests/alloc.rs` counts what a refused message allocates). Fuzzing: `decoder-fuzz.test.ts` and the mutation test in the Rust codec, both inside `just check`; there is no separate CI fuzz job. **Planned:** decoders in Workers; today the client decodes on the main thread. v0's tile codecs, RAW and QOI, are decoded by APPricot's own code, not by a browser image decoder. |
| Spoofing host chrome or another window with pixels (A1, A2) | Pixels only in host-given canvases; popups clamped to parent plus margin and size-capped; host-drawn label on every streamed window | client (clamp); host (label, drawn as host chrome) | **Live in the client.** Tiles land only in the canvas the host attached, and a tile outside the surface is dropped whole (`packages/client/src/render.ts`, hostile row (e)). Popups are placed and clamped (`clampPopup` in `registry.ts`, hostile row (c)). The codec caps surface sizes (`MAX_SURFACE_WIDTH`, `MAX_SURFACE_HEIGHT`). **The label is the host's:** the demo draws its own title bar on every window. **Planned:** `@app-ricot/react` offers the title as text (`AppricotTitle`) but does not yet make a label hard to leave out. |
| Stealing focus or input meant for the host (A2) | Focus and stacking decided only by the host; server focus/raise requests are events; keys go only to the host-focused surface; release all keys on blur | client | **Live.** A `FocusAsk` becomes a `focus-ask` event and nothing else (`registry.ts`, hostile row (d)). Keys go to the surface whose element has focus (`packages/client/src/input.ts`). A window blur sends `BlurRelease`, and the server releases every key and button it holds. |
| Reading or writing the user's clipboard (A3) | Paste only from a `paste` event, and only if the host policy allows; write only inside a user gesture and only if allowed; text only | client (primary), node proxy and streamer (secondary) | **Live in the client.** The SDK never touches `navigator.clipboard`. Text reaches the app only when the host passes it on under its own policy (`sendClipboardText` in `connection.ts`, capped at `MAX_CLIPBOARD_BYTES`), and an app's paste request is only a `clipboard-ask` event (`clipboard-policy.test.ts`). v0 has no message that carries text from the app to the browser, so no write to the user's clipboard can happen. **Planned:** the node proxy's check (L2). |
| Flooding the client or the proxy (A1, A5) | Ack-based flow control, credits per surface; frame size caps in both directions at the node proxy; bounded relay buffers | client (credits), node proxy | **Live in L1.** The server holds at most `MAX_FRAME_CREDITS` frames per surface until the client acks a drawn one (`crates/appricot-core/src/frame.rs` and `session.rs`, with tests). The client drops stale and repeated sequences (hostile row (e)). The streamer caps one WebSocket message at `MAX_MESSAGE_BYTES`. **Planned:** the node proxy's caps and buffers (L2). |
| Escaping the container (A5) | One container per session; read-only root; `CapDrop ALL`; `no-new-privileges`; caps on memory, CPU and pids; no GPU; tmpfs home; no container-engine socket inside the container | L2 container spec | **Planned** (L2). |
| Reaching other networks (A6) | Egress rules per session uid (or per netns), **armed deny-all at create**, widened at activation, swept at boot, fail-closed; DNS counted as egress because Docker's embedded DNS "forwards external DNS lookups to the DNS servers configured on the host" ([Docker docs](https://docs.docker.com/engine/network/), checked 2026-09-19) | L2 egress scoper | **Planned** (L2). |
| Attacking another session on the same node (A4) | One network namespace per session (§4.2 below); every listener authenticates, control verbs included; per-session stream tokens; no authority on abstract socket names; X access control on (xauth cookie); distinct uid per session; no lockouts keyed on a shared source address; no shared display or clipboard | streamer (listener auth), L2 (namespaces, uids, tokens, egress) | **Live in the streamer.** It binds only loopback or a unix socket (`crates/appricot-streamer/src/config.rs`, with tests), and the first message must be a `Hello` with the right token, or the connection closes with `BYE_AUTH_FAILED` (`tests/streamer.rs`, `tests/unix.rs`). **Planned:** namespaces, uids, tokens minted per session, and the rest (L2). |
| Keeping a session alive forever (A5) | Idle measured on user input only; a hard TTL; the server cannot reset either clock | node proxy and reaper (L2) | **Planned** (L2). The streamer's resume grace only ends a session whose socket is gone. |
| Leaking secrets through logs (A5, A7) | Tokens redacted in `Debug`/`Display`; nothing inside a session is logged or audited | streamer, L2 | **Live in the streamer.** The stream token's `Debug` prints no token bytes, and it has no `Display` (`crates/appricot-streamer/src/auth.rs`, with a test). Typed text is not logged: `crates/appricot-streamer/src/session.rs` logs no keysym, key code or clipboard text, nor a backend error on those paths, since one could name what was typed. **Planned:** L2's logs and audit. |

### 4.2 Rule: one network namespace per session

This rule is here because the system APPricot was split out of did the opposite, and that is the
strongest architectural lesson carried into L2.

**What was found there** (read from code and configuration, not exercised):

- **All sessions shared one network namespace.** Every session's loopback was therefore every
  other session's loopback, and every session uid could reach every `127.0.0.0/8` port on the
  node.
- **The in-image supervisor's activation verb carried no token.** One session could call a
  neighbour session's activation verb over that shared loopback.
- **Abstract unix sockets are shared across a namespace and carry no permissions** — "Socket
  permissions have no meaning for abstract sockets"
  ([unix(7)](https://man7.org/linux/man-pages/man7/unix.7.html), checked 2026-09-19) — and packet
  filters do not see them. The VNC server in that image took the connecting user from the peer's
  self-bound abstract name and granted that user's stored rights.
- **A login lockout keyed on the source address** let one session lock the managing daemon out of
  another, because every session and the daemon presented `127.0.0.1`.

**What is unconfirmed.** Whether a raw peer in that shared namespace could get past the VNC
server's own authentication step and actually reach a **neighbour session's** display was never
confirmed. That unconfirmed half is exactly what decides how serious the finding is, so no
severity is stated here and none is borrowed from elsewhere.

**The rule for APPricot**, whatever that answer turns out to be:

1. **One network namespace per session, by construction.** Not a filter rule, not a uid
   convention: a namespace. A socket in one session is not addressable from another.
2. **Internal verbs are authenticated even when they are "only on loopback".** Every control
   verb — activation, readiness, resize, shutdown — carries the session's own token. "Only
   reachable locally" is not an authentication mechanism, and it stops being true the moment
   something shares the namespace.
3. **No authority rests on a name a peer can choose**, including an abstract socket name.
4. **A test asserts that a session cannot reach a sibling's socket.** It is an exit criterion of
   M4 ([roadmap.md](../roadmap.md)), not a manual check.

**Open gap against rule 2 (recorded 2026-09-23, for L1 only).** The v0 streamer's `GET /readyz`
carries no token. It changes nothing and answers one bit of state (`starting`, `ready`, `gone`),
and nothing of the session. A token check there would put the stream token into every health
probe's configuration. The gap closes with L2's readiness gate, which must carry the session's
token as this rule asks. The session route, `GET /session`, is not affected: nothing is served
before an authenticated `Hello`.

### 4.3 ADV2: a tenant trying to reach a sibling session

**State: planned.** Every control in this table belongs to L2 or L3, which are not built. What
L1 has today is the stream token in the first message (§4.1).

| Threat | Planned control | Where |
|---|---|---|
| Attaching to someone else's session (A4) | Tickets are short-lived, signed, and bound to (tenant, user, session, node); presented in the first WebSocket message, never in a URL; a wrong or stale ticket gets the same answer as a missing session, so it does not confirm existence | L2 ticket API and node proxy; L3 broker |
| Replaying a ticket on another node (A4) | The ticket names its node; a node refuses tickets for other nodes | L2 node proxy |
| Guessing session ids (A4) | Session ids are random and unguessable; they are never authority on their own | L2 |
| Exhausting the node (A5) | Per-tenant and per-user session quotas; per-session resource caps; warm pool depth bounded | L2 |
| Cross-site WebSocket hijacking (A4) | `Origin` allow-list on the upgrade; the ticket is required on top of any cookie | L3 edge, L2 node proxy |
| Using a profile they should not have (A5, A6) | The host backend asks for sessions server to server; profiles are allowed per tenant | L2 ticket API (host backend authenticates) |

### 4.4 ADV3: a network attacker

| Threat | Control | Where | State |
|---|---|---|---|
| Reading or changing the stream on the public path (A7) | TLS at the edge; WebSocket only over `wss:` | L3 edge (or the host's own edge before L3) | **Planned** (L3, or the host's edge). |
| Stealing a ticket from a URL, a log or a referrer (A4) | Tickets never in URLs; short lifetime; single use or bound to one connection | client, L2 | **Live for the stream token:** it travels in the first `Hello`, never in a URL (`packages/client/src/connection.ts`; the demo's `no-url-secrets.test.ts`). **Planned:** tickets and their lifetime (L2). |
| Intercepting internal legs (A7) | Streamer bound to loopback or a unix socket only; node-to-edge on an internal, authenticated channel | streamer, L2, L3 | **Live in the streamer:** `config.rs` refuses any other bind, with tests. **Planned:** node to edge (L2, L3). |
| Downgrade to an older protocol (A1) | Version negotiated in the first messages; unsupported versions refused, never guessed | `appricot-proto`, client | **Live.** The streamer answers an unsupported version with `BYE_PROTOCOL_VERSION` (`tests/streamer.rs`), and the client closes on a reply version it did not ask for (`connection.test.ts`). |

### 4.5 Rule: authentication is never inherited from a library default

Two measurements from the internal benchmark (September 2026), on Debian 13 images:

| Backend | What it did |
|---|---|
| weston 14.0.2, RDP backend | **Accepted any user name and any password.** Its source sets `FreeRDP_NlaSecurity` to FALSE and checks no credential anywhere in the file. Re-verified by connecting with random credentials. |
| weston 14.0.2, VNC backend, TLS on | Offered security types that an **unmodified noVNC 1.7.0 does not implement**, so a stock browser client could not connect at all. |

One default let everyone in; the other let nobody in. Both were the library's choice, not a
decision anyone in the stack made. The rule:

1. **A backend's authentication is never inherited from a library default.** If a component's
   authentication was not chosen, configured and read by us, it does not count as a control and
   does not appear in a table above.
2. **Every session is authenticated by a ticket APPricot mints** (§4.3), and **every internal
   verb requires it** (§4.2 rule 2). A backend's own auth, where it has any, is a second layer,
   never the first.
3. **Any backend adopted later must be tested by pointing a client at it with no credentials at
   all and proving it is refused.** That test is an exit criterion of M4
   ([roadmap.md](../roadmap.md)) and is repeated for each new backend, not assumed from the last
   one.

## 5. What stays after the controls

- **A compromised application can draw anything inside its own windows**, including a fake login
  prompt. Only the host's labelled chrome helps. This is a UX control.
- **Keys typed into a session reach a possibly hostile application.** That is the product working
  as intended. The host must name the application clearly.
- **The streamer is as trusted as the application.** Running it under a separate uid from the
  application would raise the bar; whether that is possible with one X server is open
  ([architecture.md](../architecture.md), open questions).
- **Our decoders run in the host realm.** They are the new attack surface that replaced a
  third-party client. They must stay small, bounded and fuzzed.
- **Egress residuals depend on the mechanism.** The deployment reviewed accepted one — unprivileged
  UDP leaving through a tunnel transport's source port — and L2 must record its own.

## 6. Review triggers

Revisit this document when: the v0 protocol spec lands (M1); the client MVP lands (M2); the first
host integration lands (M3); L2 introduces tickets, namespaces and egress (M4); a second node
exists (M5); any feature adds a string, a URL or a new input path to the protocol.

Last revisited 2026-09-23, for the first two triggers: M1's code side and M2 have landed, and
§4 now marks each control live or planned.
