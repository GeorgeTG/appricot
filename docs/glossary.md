# Glossary

Words this project uses, in the sense it uses them. Where a word comes from Wayland or X11, the
entry says so. The window-model words follow Wayland's xdg-shell
([wayland.app/protocols/xdg-shell](https://wayland.app/protocols/xdg-shell)) whatever the backend
is, as proposed in [ADR-0004](adr/0004-layers-window-model-and-first-backend.md).

**App profile.** The preconfigured recipe for one application: container image, launch command,
environment, mounts, resource caps, egress policy, clipboard policy and scale policy. A session
is always started from a profile. The pilot application is the first profile. Node capacity is
planned per profile, not per session slot, because the application dominates the cost of a
session ([architecture.md §6](architecture.md#6-backend-matrix)). (L2.)

**Backend.** The part of the streamer that talks to one kind of display server. It implements the
`CaptureBackend` and `InputSink` traits of `appricot-core`. `appricot-x11` is the first. A
Wayland backend may come later.

**Broker.** The L3 service that places new sessions on nodes and issues tickets that name the
owning node. It does not carry frames; an edge router does, by reading the ticket.

**Configure / ack.** The two-step size and state handshake. The host proposes a configure with a
serial; the streamer applies it to the app and sends an ack with the same serial and the size the
app really took. An ack with serial 0 answers no configure: it reports a size the app took on its
own. It follows xdg-shell's `configure` / `ack_configure`. Here the host plays the
compositor, so configure travels from the client library to the streamer.

**Credit.** One frame a surface may have in flight: sent and not yet acked. At most
`MAX_FRAME_CREDITS` (4) per surface; while none is free the server coalesces damage instead of
queueing frames ([protocol/v0.md §6](protocol/v0.md#6-flow-control)).

**Damage.** The rectangles of a surface that changed since the last frame. Only damaged areas are
encoded and sent. With no damage, nothing is sent. From Wayland's `wl_surface.damage_buffer`; in
X11 it comes from the DAMAGE extension.

**Envelope.** The protobuf message every WebSocket binary frame carries; its oneof names exactly
one protocol message, and an envelope that names none is a protocol violation that closes the
connection ([protocol/v0.md §1](protocol/v0.md#1-encoding)).

**First host application, pilot host application.** The web product that embeds APPricot's client
first. It is private and lives in its own repository, and it is not named in this repository. Used
as a stand-in for "the first real consumer" when a document needs one.

**Full redraw.** A Frame flag meaning "drop every tile you hold for this surface: this frame's
tiles cover the whole surface". Set on a surface's first frame and on the first frame after a
resume, so missed damage is repainted rather than replayed
([protocol/v0.md §7](protocol/v0.md#7-reattachment)).

**Grace window.** The `RESUME_GRACE_MS` (10,000 ms) a session outlives its socket. Inside it a
reconnect with the right resume serial resumes the session; at its expiry the session — in a v0
streamer, one session per process, the backend with it — is torn down
([protocol/v0.md §7](protocol/v0.md#7-reattachment)).

**Host, host app.** The web product that embeds APPricot's client. It owns the page, the window
chrome (title bars, frames, taskbar), focus, stacking and the user's identity.

**Hostile fixture.** The test server in `packages/client/src/hostile/` that sends markup in
every string field, oversized lengths, a popup the size of the screen and focus requests —
proving in CI that none of it becomes markup, escapes the parent-plus-margin box, or takes
focus ([ADR-0003](adr/0003-untrusted-server-client.md)).

**Internal benchmark.** The measurement run this project's numbers come from, September 2026: a
Qt6/xcb X11 application on Debian 12 under Docker, no GPU, compared across several display and
transport paths. Every number quoted in these documents says it comes from there, with the method
in a clause. A number with no such attribution and no checked URL is marked **(unverified)**.

**Least-overlap layout.** The S1 backend's placement rule: each new X toplevel goes where it
overlaps the live toplevels least, 16 pixels from its neighbours when there is room. On the
1400x900 root realistic windows still overlap, so nothing relies on the layout: the input path
raises its target before a press (`crates/appricot-x11/src/wm.rs`; the spike may revisit it).

**Limits table.** The 24 caps — every length, count and dimension on the wire — that both peers
check before allocating or looping. Its authoritative home is `wire.proto`; the Rust codec, the
TypeScript mirror and the spec transcribe it
([protocol/v0.md §3](protocol/v0.md#3-the-limits-table)).

**Node.** One machine running the L2 session manager: app profiles, a container per session, a
warm pool, a readiness gate, a reaper, egress scoping, audit, and the node proxy. A session lives
on exactly one node for its whole life.

**Pilot application.** The application APPricot streams first: a legacy X11 administration tool
built on Qt6/xcb, with no web version. It opens several toplevel windows at once, which is why
per-window streaming is the product. It supplies the first app profile and the subject of the
internal benchmark. It is not named in this repository, and no design decision may depend on
anything specific to it.

**Popup.** A short-lived surface tied to a parent surface: a menu, a combo-box list, a tooltip. It
has no host chrome and is placed by a positioner. In X11, an override-redirect window becomes a
popup. The client keeps a popup inside its parent's box plus a small margin (proposed in
[ADR-0003](adr/0003-untrusted-server-client.md)).

**Positioner.** The rule that places a popup relative to its parent: an anchor rectangle inside the
parent, a size, anchor and gravity, and what to do when the popup would not fit. From xdg-shell's
`xdg_positioner`. For X11 the streamer derives it from root coordinates.

**Resume serial.** The serial each `HelloReply` carries. A reconnecting client presents the one
from the most recent `HelloReply` in `Hello.resume_serial` to reattach to that session inside the
grace window ([protocol/v0.md §7](protocol/v0.md#7-reattachment)).

**Seat.** Not used here. Some systems call a per-user application container a "seat"; this project
says **session**. Wayland also has a `wl_seat`, a group of input devices belonging to one user;
that sense is not used either. Outside this entry, the word standing for a session anywhere in
this repository is a mistake.

**Session.** One running instance of an app profile for one user: one container with its own
network namespace, one display server, one streamer, one or more app processes, and the windows
they map. It ends when the app's last process exits, when the reaper ends it, or when the host
ends it. A session outlives the browser that opened it: a client may disconnect and reconnect.

**Stream token.** The per-session secret the node (or, before L2, the host backend) hands to the
streamer and presents on the loopback leg. It never reaches the browser.

**Surface.** A rectangle of pixels with its own id, size, scale and damage. Every streamed window
is a surface. A surface has exactly one role, set once: toplevel or popup. From Wayland's
`wl_surface`.

**Ticket.** A short-lived credential that lets one browser attach to one session. The host app's
backend gets it from the session-ticket API and gives it to its page. It names the tenant, the
user, the session and the node that owns the session, so an edge can route it and another node
refuses it. The browser presents it in the first WebSocket message, not in the URL.

**Tile.** One encoded rectangle of a Frame: at most 256x256 pixels, cut on a grid aligned to the
surface origin so a tile never crosses a grid line (the client can cache by grid cell), carried
RAW or QOI ([protocol/v0.md §4.3](protocol/v0.md#43-frames)).

**Toplevel.** A surface the host shows as a window with its own chrome. It may have a parent
toplevel, as a dialog does. From xdg-shell's `xdg_toplevel`. In X11, a window the app maps through
the window manager becomes a toplevel; `WM_TRANSIENT_FOR` gives its parent.

**Warm pool.** Containers started ahead of time for a profile, idle and ready, so that a user's
request only activates one. The first host application already runs one for its streamed-app
sessions, and that design is re-implemented here rather than linked
([architecture.md](architecture.md) §7).
