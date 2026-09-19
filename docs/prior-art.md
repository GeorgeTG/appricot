# Prior art

What already exists that streams applications to a browser, how each project relates to APPricot,
and what we can take from it. Researched on 2026-09-19.

## How to read this

**Method.** Versions and dates come from each project's release list: the GitHub API (via `gh`), the
GitLab API, or the vendor's own release notes. Licences come from the project's LICENSE file and,
where they differ, from the source headers and package manifests, read through GitHub's contents
API or the raw file, not from badges. Every claim about an external project links the page we
read. A claim we could not check says **(unverified)**. Every claim here was re-checked against
its primary source on 2026-09-19; what that pass changed is listed in
[§7](#7-what-the-verification-changed).

Where a line number is cited, the link is pinned to a commit, because `master` moves. A release
date is the date of the release or tag, which is not always the date in the project's own
changelog (xpra 6.5 is the example, below).

**The eight questions** (the table columns):

1. **Per-window in the browser?** Does each application toplevel arrive as its own stream and
   become its own element on the page? "Inside its own page" means yes, but the project draws the
   windows itself.
2. **Embeddable as host elements?** Can a host web app take each window and draw it inside the
   host's own floating window, with the host's chrome? This is APPricot's defining feature.
3. **Licence.**
4. **Self-hostable?**
5. **Sandbox / session orchestration?** Does it create and destroy an isolated environment per
   session (container or VM), with a pool and a lifecycle?
6. **Load balancing?** Does it place sessions across several machines?
7. **GPU needs.** What it needs on the server to run at a sane cost.
8. **Last release** as of 2026-09-19.

**Related internal research.** The measurements behind APPricot's first backend come from an
**internal benchmark (September 2026)**: the pilot application — a Qt6/xcb X11 desktop
application — under Docker with no GPU, on Debian 12 and Debian 13. That work also read several
of the projects below in depth (Wayland paths, X forwarding, xpra, and the design of a streamer
of our own). Where this page relies on a finding from it that we did not re-read today, it says
so.

## 1. Comparison table

Thirty projects and the APPricot target row. The APPricot row is the target, not a claim about
shipped code.

| Project | Per-window in the browser? | Embeddable as host elements? | Licence | Self-hostable? | Sandbox / session orchestration | Load balancing | GPU needs | Last release |
|---|---|---|---|---|---|---|---|---|
| [Kasm Workspaces](#kasm-workspaces) | No. One KasmVNC display per session | Partly: the whole Kasm web app in an iframe, plus a session API | Proprietary. Community Edition free for testing, non-profit and eligible non-commercial use, 5 concurrent sessions | Yes | Yes: container per session, staged (pre-started) pool, expiry, autoscale | Yes: sessions spread over agents in zones | Optional | 1.19.0, GA 2026-06-16 |
| [KasmVNC](#kasmvnc) | No. One canvas | Only as an iframe of its own client | GPL-2.0-or-later (server). Its web client is MPL-2.0 at source, GPL-2+ as packaged | Yes | No | No | None. Optional VA-API/NVENC video mode | v1.5.0, 2026-07-29 |
| [Apache Guacamole](#apache-guacamole) | No. One display per connection | Yes, as a JS library, but one display element per connection | Apache-2.0 | Yes | No sandbox. Connection records only | Yes: balancing groups, optional session affinity | None at the gateway | 1.6.0, 2025-06-22 |
| [xpra + xpra-html5](#xpra-and-xpra-html5) | Yes, inside xpra-html5's own page | No: the stock client owns the page. A client of our own could speak its protocol | Server GPL-2.0-or-later. html5 client MPL-2.0 | Yes | One server per session. A proxy can start sessions. No containers | No | None. Optional video encoders | xpra 6.5.3, 2026-08-18. xpra-html5 v20, 2026-03-18 |
| [Greenfield](#greenfield) | Yes, inside its compositor canvas | Library, but a canvas is a whole output. No per-window canvas API | AGPL-3.0-or-later | Yes | Starts apps from a config. No sandbox (unverified) | No | GPU preferred. CPU fallback is slow | 1.0.0-rc1, 2023-12-04 (pre-release only) |
| [Selkies](#selkies) | No. A desktop or one app in one stream | Own client. iframe use (unverified) | MPL-2.0 | Yes | Not in Selkies itself. [SealSkin](#sealskin) is the project's separate orchestrator | No | Optional. CPU x264 / OpenH264 / JPEG | 2.0.0rc0, 2026-09-12 |
| [neko](#neko) | No. The whole X display over WebRTC | Own client. Its README names "embed in your web app" as a use case; the mechanism was not read (unverified) | Apache-2.0 | Yes | neko-rooms: one Docker container per room, with per-room CPU, memory and shm caps | No | Optional (unverified) | v3.1.5, 2026-08-05 |
| [wprs](#wprs) | Native desktop windows. No browser | Not applicable | Apache-2.0 | Yes | No | No | None (no dmabuf support yet) | No releases. Last push 2026-08-31 |
| [waypipe](#waypipe) | Native desktop windows. No browser | Not applicable | GPL-3.0-or-later (`waypipe-c`: MIT) | Yes | No | No | None for shared memory. GPU for DMABUF and video | v0.11.2, 2026-08-25 |
| [GTK Broadway](#gtk-broadway) | Yes, GTK apps only, in its own page | No (unverified) | LGPL-2.0-or-later | Yes | No | No | None | Ships in GTK 4.24.0, tagged 2026-09-11 |
| [Qt WebGL streaming](#qt-webgl-streaming) | One Qt Quick app, one viewer | No | GPL-3.0 or commercial | Yes | No | No | WebGL in the viewer's browser | Tech preview in Qt 5.10, released in 5.12, last tags 5.15 LTS. Not in Qt 6 |
| [RustDesk](#rustdesk) | No. Whole screen (unverified) | No | AGPL-3.0 | Yes. Self-hosting its web client is a paid Pro feature | No | Pro picks the closest relay | Optional (unverified) | 1.4.9, 2026-07-06. Server 1.1.16, 2026-07-20 |
| [Sunshine + Moonlight](#sunshine-moonlight-and-wolf) | No. Whole display | No official browser client | GPL-3.0 (both) | Yes | No | No | Hardware encoders on AMD, Intel and NVIDIA; software encoding also available | Sunshine v2026.914.233613, 2026-09-15. Moonlight-qt v6.1.0, 2024-09-17 |
| [Wolf](#sunshine-moonlight-and-wolf) | No. Moonlight clients only | No browser client | MIT | Yes | Yes: a container per client session, on demand | No (one host) | GPU for encoding (a CPU-only path was not confirmed) | v2024.07. Commits to 2026-09-07 |
| [linuxserver.io webtop](#linuxserverio-webtop) | No. A desktop | Its own Selkies page | GPL-3.0 (the repo) | Yes | No. One container, user-run | No | Optional | Rolling builds. Latest stable tag `c6d858e8-ls314`, 2026-09-15; dev tags daily |
| [Amazon WorkSpaces Applications](#amazon-workspaces-applications-formerly-appstream-20) (formerly AppStream 2.0) | No. Per-app native windows need the Windows client | Yes, one session per iframe via its Embed JS SDK | Proprietary service | No | Yes: fleets. An instance per user, terminated after the session, except multi-session Windows fleets | Yes: auto scaling rules | Optional (unverified) | Managed service |
| [Azure Virtual Desktop RemoteApp / Windows 365 Cloud Apps](#azure-virtual-desktop-remoteapp-and-windows-365-cloud-apps) | No evidence. RemoteApps from one host pool share one browser tab | No (unverified) | Proprietary service | No | Yes: host pools, shared Cloud PCs | Yes: breadth-first or depth-first | Optional (unverified) | Managed service |
| [Citrix Virtual Apps (HDX)](#citrix-virtual-apps-hdx) | Unverified for the HTML5 client | An HTML5 SDK exists (scope unverified) | Proprietary | Yes | Yes: machines in delivery groups | Yes: horizontal or vertical | Optional (unverified) | CVAD 2607 LTSR (2026-08). Workspace app for HTML5 2603.10, 2026-07-22 |
| [Cameyo by Google](#cameyo-by-google) | Per app (one PWA per app). Per window unverified | No (unverified) | Proprietary | Partly: its Windows servers can be self-hosted (Server 2019 or later; 2025 not yet supported). The admin console is Cameyo's | Yes (managed servers) | Unverified | Optional (unverified) | SaaS |
| [ThinLinc](#thinlinc) | Unverified. It has single-application publishing | No (unverified) | Proprietary. Free up to 10 concurrent users | Yes | Session manager, no sandbox (unverified) | Yes | None (unverified) | 4.21.0, 2026-08-21 |
| [NoMachine](#nomachine) | Unverified | No (unverified) | Proprietary | Yes | Session manager (unverified) | Yes, in cluster editions | Optional (unverified) | 10.1.7, 2026-09-14 |
| [WebX (ILL)](#webx) | Windows are separate textures, but drawn in one desktop scene | Its npm client draws a whole desktop | GPL-3.0 | Yes | A router spawns Xorg + WM + engine per session | No (one host per router) | None (unverified) | Engine 1.5.1, 2026-02-19. Router 1.5.7, 2026-07-15 |
| [Webland](#webland) | Yes. One canvas per window | No. Its frontend is a Leptos/WASM shell that draws its own chrome, panel and workspaces | MIT in `Cargo.toml`, but no LICENSE file | Yes | No. No auth | No | VA-API for H.264. CPU fallback: deflated damage rectangles | None. Created 2026-08-26 |
| [Elsewhere](#elsewhere) | Yes, since 2026-09-04: one stream per window | Own viewer; a window opens in a popup at `/?window=ID`. Embedding in a host page unverified | MIT | Yes | No. Bearer tokens with per-feature permissions | No | GPU render node, or llvmpipe with software encoding at 30 Hz | v0.10.1, 2026-09-11 |
| [lwfa](#lwfa) | Yes | Own page | MIT | Yes | No. Runs nested in the user's compositor | No | NVENC for hardware H.264/HEVC; Intel and AMD fall back to JPEG. Eight concurrent NVENC streams, then the ninth window degrades | v1.5.11, 2026-09-12 |
| [node-x11's JS X server](#node-x11s-javascript-x-server) | The browser is the X server. Per-window by construction. Untested with the pilot application | A library | MIT | Yes | No | No | None | v4.2.1, 2026-09-13 |
| [Webswing](#webswing) | Yes for Java: its CWM renders each window into its own canvas, positioned in the host's HTML | Closest commercial match: the canvases live in the host document, with two-way JS calls. Java only | Proprietary | Yes | Session Pools run the application instances | Yes: stateless cluster servers, round-robin or active-session | None stated (unverified) | 26.1 is the newest version in its [docs index](https://www.webswing.org/docs); the Linux Web Launcher is early access |
| [WSLg / Weston RDP RAIL](#wslg-weston-rdp-rail-and-ms-rdperp) | Yes, but into a native desktop, not a browser | Not applicable: the RDP client owns the window | MIT (WSLg, Weston). FreeRDP Apache-2.0 | Yes | No | No | None required (VAIL shares memory; RAIL encodes) | WSLg v1.0.66, 2025-02-26 (pre-release v1.0.71, 2025-10-06) |
| [IronRDP](#ironrdp) | A RAIL wire codec exists; no per-window browser rendering shown (unverified) | A published Web Component, `@devolutions/iron-remote-desktop` | MIT OR Apache-2.0 | Yes | No | No | None | `ironrdp-viewer` v0.1.0, 2026-07-10; RAIL crate added 2026-08-11 |
| [SealSkin](#sealskin) | No. It launches Selkies desktops | Own client plus a browser extension | MPL-2.0 (`selkies-project/sealskin`) | Yes | Yes: a container per session over the Docker socket, fresh credentials, ephemeral or persistent storage | Docker only today; other providers are future work | As Selkies | Server image 0.3.2-ls59, 2026-09-19 |
| **APPricot (target)** | **Yes** | **Yes: the host draws each window's chrome** | **MIT OR Apache-2.0 ([ADR-0002](adr/0002-licence.md))** | **Yes** | **L2 (documented)** | **L3 (documented)** | **None for S1 X11** | Not released |

## 2. Project notes

### Browser remote desktops and their platforms

#### Kasm Workspaces

- **What it is.** A container streaming platform. Each session is a container whose display is
  served by KasmVNC. Workspaces is commercially licensed; the Community Edition is "free for
  testing, nonprofit, and eligible non-commercial use", with "up to 5 concurrent sessions"
  ([licensing](https://docs.kasm.com/docs/reference/license)). The licence page does not say
  "closed source"; the platform's own code is not published, while the image recipes are
  ([workspaces-images](https://github.com/kasmtech/workspaces-images) and
  [workspaces-core-images](https://github.com/kasmtech/workspaces-core-images) carry an MIT
  grant in `LICENSE.md`, scoped to those repositories only).
- **Release.** 1.19.0 is the latest in the release notes
  ([release notes](https://docs.kasm.com/docs/reference/release-notes)); general availability was
  announced on 2026-06-16
  ([press release](https://www.prnewswire.com/news-releases/kasm-technologies-delivers-kubernetes-ga-zero-trust-access-and-enterprise-grade-diagnostics-in-kasm-workspaces-1-19--302801925.html)).
  1.19.0 adds H.264, H.265 and AV1 video modes to KasmVNC for container sessions, and Kubernetes
  at GA ([1.19.0 notes](https://docs.kasm.com/docs/reference/release-notes/1.19.0)). It also adds an
  OpenZiti egress provider, so a workspace's traffic is scoped to named services
  ([Kasm blog](https://kasm.com/kasm-insights/kasm-workspaces-119-kubernetes-goes-ga-zero-trust-egress-and-a-release-built-for-production)).
- **Session lifecycle.** An integrator calls `request_kasm`, then polls `get_kasm_status` until the
  session is running, then sends the user to the returned URL
  ([developer API](https://docs.kasm.com/docs/develop/reference/developer-api)). Admins can
  *stage* containers: a pool of pre-started sessions that users are assigned from; unassigned
  staged sessions expire and are re-created ([staging](https://kasm.com/docs/latest/guide/staging.html)).
- **Scale.** New sessions are spread over the healthy agents of a zone; the manager "can only use
  agents that have the Docker image and Docker network … and are assigned to the correct zone",
  and retries on the next agent when a provision fails; sessions are balanced over connection
  proxies too
  ([sizing guide](https://kasm.com/docs/latest/how_to/sizing_operations.html)). Autoscale creates
  and destroys servers on demand
  ([autoscale](https://www.kasmweb.com/docs/develop/how_to/infrastructure_components/autoscale.html)).
- **Embedding.** The iframe recipe embeds the whole Kasm web app. Its two session cookies "will be
  blocked by default" inside an iframe, so the host page and Kasm must be sibling domains or
  sub-domains and the **Kasm Auth Domain** must be set to the shared parent
  ([embed in iframe](https://kasm.com/docs/latest/how_to/embed_kasm_in_iframe.html); the page
  does not mention SameSite). The windows of the app do not become the host's windows.
- **Relation to APPricot.** The closest match to L2 in shape: container per session, warm pool,
  readiness polling, expiry, egress scoping, multi-node placement. It is not per-window, and it
  is not open source.

#### KasmVNC

- **What it is.** An X server plus VNC server with its own web client. It is what the first host
  application's sessions run today, and what APPricot replaces there.
- **Licence and release.** The server is GPL-2.0-**or-later**: `LICENSE.TXT` holds the GPL-2 text
  and the sources say "either version 2 of the License, or (at your option) any later version"
  ([`common/rfb/VNCServerST.cxx` at v1.5.0](https://github.com/kasmtech/KasmVNC/blob/v1.5.0/common/rfb/VNCServerST.cxx)).
  The web client is a separate repository, [kasmtech/noVNC](https://github.com/kasmtech/noVNC),
  whose `LICENSE.txt` puts `core/**/*.js` and `app/*.js` under MPL-2.0; an earlier internal
  reading of the shipped 1.4.0 `.deb` found that it nevertheless declares every file, `www/`
  included, GPL-2+ (not re-read here). Either way it is not on the permissive allow list.
  v1.5.0, 2026-07-29, adds "a video streaming mode with hardware- and software-accelerated H.264,
  H.265, and AV1 encoders. Hardware acceleration uses VAAPI (Intel/AMD) and NVENC (NVIDIA)"
  ([v1.5.0](https://github.com/kasmtech/KasmVNC/releases/tag/v1.5.0)). Its notes list no Wayland
  and no per-window feature.
- **A lesson for APPricot's client.** KasmVNC's web client writes the server's type-178 stats
  string into `innerHTML`. v1.5.0 pins the `kasmweb` submodule at noVNC commit `475ecfa`
  (checked through the contents API), and that commit still does it
  ([`app/ui.js` at 475ecfa, line 1738](https://github.com/kasmtech/noVNC/blob/475ecfa5356579ef222983c7ce4619a7576a3bce/app/ui.js#L1738)).
  The fix is commit
  [`e0978e6`, "VNC-528 Use textContent instead of innerHTML", 2026-08-27](https://github.com/kasmtech/noVNC/commit/e0978e6);
  KasmVNC's `master` moved the submodule onto it on 2026-08-28, so the fix is in `master` and in
  no release: v1.5.0 predates it by a month
  ([`app/ui.js` at 1746dfe, line 1781](https://github.com/kasmtech/noVNC/blob/1746dfe9ed8084aee57b55c720ef00d4d7c26513/app/ui.js#L1781)).
  Even the fixed file still round-trips that element through `innerHTML` to append a latency tag
  ([same file, line 1815](https://github.com/kasmtech/noVNC/blob/1746dfe9ed8084aee57b55c720ef00d4d7c26513/app/ui.js#L1815));
  reading `innerHTML` escapes the text, so it is not exploitable as written, but it is exactly the
  pattern [ADR-0003](adr/0003-untrusted-server-client.md) bans. The original sink was first found
  in earlier internal research; the links above were re-checked here. This is why ADR-0003 treats
  every server string as text.

#### Apache Guacamole

- **What it is.** A clientless gateway. `guacd` translates VNC, RDP, SSH, telnet and Kubernetes
  into the Guacamole protocol; a Java web app does auth; a JS client draws
  ([architecture](https://guacamole.apache.org/doc/gug/guacamole-architecture.html),
  [protocols](https://guacamole.apache.org/doc/gug/configuring-guacamole.html)). There is no
  native X11 support. RDP RemoteApp shows only the named application; that it is still drawn in
  Guacamole's one display is our inference from the client's single display.
- **Licence and release.** Apache-2.0 (LICENSE in
  [guacamole-client](https://github.com/apache/guacamole-client)). 1.6.0, 2025-06-22, is the
  current release ([releases](https://guacamole.apache.org/releases/)).
- **Embedding.** `guacamole-common-js` is a library: you create a `Guacamole.Client` over a tunnel
  and append its display element to your own page
  ([guacamole-common-js](https://guacamole.apache.org/doc/gug/guacamole-common-js.html)). That is
  the right *shape* for an embeddable client. But it yields one display per connection, not one
  element per window.
- **Protocol.** A handshake of `select`, `args`, `connect` and `ready`. All drawing targets a
  numbered layer; layer 0 is the display, negative indices are off-screen buffers
  ([protocol](https://guacamole.apache.org/doc/gug/guacamole-protocol.html)).
- **Balancing.** A *balancing* connection group picks the member connection with the fewest active
  users. Optional session affinity keeps a user on the same underlying connection until logout
  ([administration](https://guacamole.apache.org/doc/gug/administration.html)).

#### Selkies

- **What it is.** A self-hosted remote desktop streamer for containers, Kubernetes and HPC.
  MPL-2.0 ([repo](https://github.com/selkies-project/selkies)).
- **Release.** 2.0.0rc0, 2026-09-12
  ([release](https://github.com/selkies-project/selkies/releases/tag/2.0.0rc0)). WebSockets on one
  TCP port is now the default transport, carrying video, audio, input, clipboard and files;
  WebRTC is opt-in. Video is H.264 (x264, or OpenH264 in a GPL-free build) or Motion JPEG, with
  optional NVENC or VA-API. X11 is the default backend; `--wayland=true` runs a headless Wayland
  backend. There is an optional HTTP basic-auth user. The rc is not on a Python index.
- **Capture and encode live in a Rust extension**, [pixelflux](https://github.com/selkies-project/pixelflux)
  (MPL-2.0): it captures X11 through pure-Rust XCB (or `NvFBC`/DRI3 on a GPU) or Wayland through
  a Smithay-based compositor, detects changes, and cuts JPEG and H.264 frames into **stripes
  encoded in parallel**. GStreamer is gone from Selkies 2.0's runtime.
- **Server-side input authority.** A viewer's keyboard and mouse are "refused whatever its client
  sends", and secure mode replaces share links with "provisioned session tokens that carry a role
  and a slot" ([2.0.0rc0](https://github.com/selkies-project/selkies/releases/tag/2.0.0rc0)).
- **Relation to APPricot.** A desktop (or one app) in one stream. Not per-window. Its orchestrator
  is [SealSkin](#sealskin).

#### neko

- **What it is.** A self-hosted virtual browser in Docker, streamed over WebRTC, with several users
  sharing control. It streams the whole X display; its README says it "is not only limited to a
  browser; it can run anything that runs on linux", including a full desktop environment
  ([repo](https://github.com/m1k1o/neko)). Apache-2.0. v3.1.5, 2026-08-05. The README's use-case
  list includes "**Embed anything** — embed virtual browser in your web app"; how a host page does
  that was not read **(unverified)**.
- **Orchestration.** [neko-rooms](https://github.com/m1k1o/neko-rooms) (Apache-2.0, v1.6.5,
  2026-03-29) manages rooms. Each room is a Docker container created through the Docker API, with
  per-room CPU shares, `NanoCPUs`, memory, shm size, GPUs and devices
  ([`internal/room/manager.go`](https://github.com/m1k1o/neko-rooms/blob/8ab1caf/internal/room/manager.go)).
  That is container isolation, not a sandbox profile.

#### linuxserver.io webtop

- **What it is.** Linux desktops in a browser, built on linuxserver's Selkies base image, which
  streams a desktop or a single application
  ([docker-webtop](https://github.com/linuxserver/docker-webtop)). GPU is optional. There is no
  auth by default; the README warns against exposing it without protection.
- **Licence.** The repo is GPL-3.0. The move off KasmVNC is dated in its own changelog —
  "17.06.25: Rebase all images to Selkies" — and the KasmVNC base image repo was last pushed
  2025-07-12 ([docker-baseimage-kasmvnc](https://github.com/linuxserver/docker-baseimage-kasmvnc)).
  This is no longer an inference.

#### SealSkin

- **What it is.** The Selkies project's orchestration layer: "a self hosted client server system
  that manages users and launches Selkies containers on demand", a container per session with a
  fresh UUID, fresh credentials and ephemeral or persistent storage
  ([SealSkin docs](https://docs.linuxserver.io/selkies/components/sealskin/)).
- **Auth worth copying.** No passwords: a user holds an RSA key pair and signs a short-lived JWT
  (five-minute expiry) that the server verifies against the stored public key.
- **Licence and release.** MPL-2.0 ([selkies-project/sealskin](https://github.com/selkies-project/sealskin)).
  The server image is [linuxserver/docker-sealskin](https://github.com/linuxserver/docker-sealskin)
  (GPL-3.0 packaging), `0.3.2-ls59`, 2026-09-19. "Docker via the socket is the implemented
  provider"; Kubernetes and remote hosts are future work, so there is no multi-node placement yet.
- **Relation to APPricot.** The nearest permissively licensed L2: session lifecycle, per-session
  credentials, storage policy. It streams desktops, not windows.

#### RustDesk

- **What it is.** A self-hosted remote desktop, an alternative to TeamViewer. AGPL-3.0 (`LICENCE`,
  [repo](https://github.com/rustdesk/rustdesk)). Client 1.4.9, 2026-07-06; server 1.1.16,
  2026-07-20 ([server](https://github.com/rustdesk/rustdesk-server)).
- **Web client.** Self-hosting the web client is listed as a Server Pro (commercial) feature. Pro
  also picks the closest of several relays
  ([Server Pro](https://rustdesk.com/docs/en/self-host/rustdesk-server-pro/)).
- **Relation to APPricot.** A whole-screen tool for reaching machines, not an app streamer
  (whole-screen claim unverified).

#### Sunshine, Moonlight and Wolf

- **Sunshine** is a game-stream host for Moonlight. GPL-3.0. Latest stable v2026.914.233613,
  2026-09-15; pre-releases run ahead of it daily ([repo](https://github.com/LizardByte/Sunshine)).
  Its README says it supports "AMD, Intel, and Nvidia GPUs for hardware encoding. Software
  encoding is also available", so a GPU is not a hard requirement.
- **Moonlight-qt** is the desktop client. GPL-3.0. v6.1.0, 2024-09-17
  ([repo](https://github.com/moonlight-stream/moonlight-qt)). There is no official browser client.
  The unofficial [moonlight-web-stream](https://github.com/MrCreativ3001/moonlight-web-stream)
  (GPL-3.0, v3.0.0-prerelease.7, 2026-09-16) forwards Sunshine to a browser over WebRTC.
- **Wolf** is a Moonlight-compatible server that creates a virtual desktop per client, on demand,
  in containers, so several users share one server ("Allow multiple users to stream different
  content by sharing a single remote host hardware", "On demand creation of virtual desktops").
  MIT ([repo](https://github.com/games-on-whales/wolf)). The last tagged release is v2024.07;
  commits continue on the default `stable` branch (last 2026-09-07). Its virtual desktop is
  [gst-wayland-display](https://github.com/games-on-whales/gst-wayland-display), "a micro Wayland
  compositor that can be used as a Gstreamer plugin", MIT, built on a Smithay fork — the closest
  permissive precedent for a future `appricot-wayland`.
- **Relation to APPricot.** Wolf is MIT prior art for "a container per session, started on demand
  by the streaming server". None of them is per-window.

### Per-window and rootless forwarding

#### xpra and xpra-html5

- **What it is.** A server that keeps X11 applications running remotely and, in seamless mode,
  forwards each X window on its own. The server is GPL-2.0-or-later (`COPYING` holds the GPL-2 text,
  [repo](https://github.com/Xpra-org/xpra); the "or later" comes from its source headers and
  `pyproject.toml`, per an earlier internal reading). The html5 client is MPL-2.0 (`LICENSE`,
  [repo](https://github.com/Xpra-org/xpra-html5)).
- **Releases.** xpra 6.5.3, 2026-08-18. A `v6.5.4` tag exists (tagged 2026-09-18) with no GitHub
  release yet. xpra-html5 v20, 2026-03-18; a `v21` tag exists (tagged 2026-05-11) with no GitHub
  release. Careful with 6.5, the release that brought the Wayland backend: its `CHANGELOG.md`
  heading says **2026-05-06**, but the `v6.5` tag is dated **2026-06-15** and the GitHub release
  **2026-06-16**. Earlier internal notes quote the changelog date; both are right about different
  events.
- **Browser behaviour.** In the html5 client, remote windows stay inside the browser page
  ([Seamless.md](https://github.com/Xpra-org/xpra/blob/master/docs/Usage/Seamless.md)). The same
  page says the Wayland backend is experimental and recommends the X11 backend.
- **Protocol.** A written spec on `master`
  ([Protocol.md](https://github.com/Xpra-org/xpra/blob/master/docs/Network/Protocol.md)): an
  8-byte header, `rencodeplus` packets, LZ4 / Brotli / Zstandard compression, a `hello`
  capability exchange, and window packets (`window-create`, `window-metadata`,
  `window-move-resize`, `window-draw`). The client answers each draw with `window-ack`
  (`wid`, `width`, `height`, `sequence`, `decode_time_us`, `message`) or `window-draw-ack`, so the
  acknowledgement carries the draw's sequence and the client's decode time in microseconds. The
  limits: "Before the peer's `hello` is accepted, an implementation MUST impose a maximum declared
  payload length of 4 MiB"; "After capability processing, the normal per-record limit is 16 MiB";
  "No decompressed record may exceed 256 MiB", and display negotiation may replace the absolute
  limit with a bound derived from the negotiated pixel area. Read the document's own scope line:
  it specifies "the `master` branch on 16 August 2026 (Xpra 6.6 development series)" and
  deliberately excludes what `XPRA_BACKWARDS_COMPATIBLE=1` accepts. The Protocol.md shipped in
  6.5.3 is a different, 116-line page covering only the header, flags and compression, so whether
  6.5.3 on the wire uses these packet names **(unverified)**.
- **Sessions.** The proxy server is a single entry point: it authenticates, identifies the
  session wanted, and can start new ones
  ([Proxy-Server.md](https://github.com/Xpra-org/xpra/blob/master/docs/Usage/Proxy-Server.md)).
- **A lesson for APPricot's client.** xpra-html5 still writes the window title with jQuery's
  `.html()`: on `master`
  ([Window.js at 3029a04, line 653](https://github.com/Xpra-org/xpra-html5/blob/3029a04/html5/js/Window.js#L653))
  and in the shipped v20 release
  ([Window.js at v20, line 649](https://github.com/Xpra-org/xpra-html5/blob/v20/html5/js/Window.js#L649)).
  The title comes straight from `window-metadata`, so it is server text.
  Earlier internal research also found notification text built as HTML and a server-driven
  `window.open` (not re-read here).
- **Measured against the pilot application** (internal benchmark, September 2026, no GPU):
  WebP/JPEG chosen for its window, about 600 B/s idle, about 415 KB for a maximise.
  [ADR-0004](adr/0004-layers-window-model-and-first-backend.md) keeps "xpra's server, unmodified,
  plus our own client" (S3) as an open alternative backend.
- **Relation to APPricot.** The most mature per-window X11 server. Its client owns the page, so
  the windows are not the host's. Its code is GPL; ideas only.

#### Greenfield

- **What it is.** A Wayland compositor that runs *in the browser*. Apps draw into WebGL textures
  inside a canvas; that canvas is one Wayland output. A compositor-proxy on the server acts as the
  native compositor and encodes each application's content to H.264 with GStreamer; the browser
  decodes with WebCodecs, with a WebAssembly fallback. Transport is WebSockets; WebTransport is a
  plan. Without a GPU, encoding falls back to slower software encoding on the CPU
  ([design](https://greenfield.app/pages/design/)).
- **Licence and release.** AGPL-3.0-or-later: the LICENSE is the AGPL-3 text, and both
  `packages/compositor/package.json` and `packages/compositor-proxy/package.json` say
  `"license": "AGPL-3.0-or-later"` ([repo](https://github.com/udevbe/greenfield)). One
  pre-release, 1.0.0-rc1, 2023-12-04. Last commit 2025-10-15. In
  [issue #167](https://github.com/udevbe/greenfield/issues/167), opened 2026-03-07, the maintainer
  answered on 2026-03-17: "I don't have the time and energy for the moment to work on it as
  intensively as I did in the last 8 years. I do keep an eye on the repository".
- **Alpha, and what the design page does not say.** The design page covers GStreamer encoding,
  WebCodecs and the WebAssembly fallback, but says nothing about alpha or about encoding on
  commit. The split-alpha stream is in the code: a second x264 pipeline at `bitrate=1200` beside
  the `bitrate=12000` colour pipeline
  ([`gst_frame_encoder.c`](https://github.com/udevbe/greenfield/blob/6c578f4/packages/compositor-proxy/native/encoding/src/gst_frame_encoder.c)).
  "Encode per surface on commit, so an idle window costs nothing" is our own inference from
  Wayland's commit model, not a statement by the project.
- **Embedding.** The public shell API has `initScene(canvasCreator)`: one canvas per scene. Its
  events are `surfaceCreated`, `surfaceDestroyed`, `surfaceTitleUpdated`, `surfaceAppIdUpdated`
  and `surfaceActivationUpdated`
  ([UserShellApi.ts](https://github.com/udevbe/greenfield/blob/master/packages/compositor/src/UserShellApi.ts)).
  There is no call that renders one toplevel into its own canvas.
- **Relation to APPricot.** The closest design in spirit: per-surface encoding, a browser that
  knows about surfaces. AGPL in both halves, dormant, and GPU-leaning. Ideas only.

#### wprs

- **What it is.** Its README calls it xpra for Wayland, written in Rust. `wprsd` is a Smithay
  compositor that *serialises* session state instead of rendering it; `wprsc` is a Wayland client on the viewer's desktop that
  recreates each window there. Buffers are compressed by a transpose, DPCM on the colour
  channels, a YUV-like transform and zstd, in single-digit milliseconds per frame. X11 apps go
  through a separate `xwayland-xdg-shell` binary. Only core and xdg-shell protocols; no dmabuf,
  no touch ([README](https://github.com/wayland-transpositor/wprs)).
- **Licence and release.** Apache-2.0 (LICENSE; the sources carry "Copyright 2024 Google LLC").
  No releases; last push 2026-08-31.
- **Its own ancestor.** wprs names ChromeOS's
  [sommelier](https://chromium.googlesource.com/chromiumos/platform2/+/main/vm_tools/sommelier/README.md)
  as the model for its XWayland binary. Sommelier is "an implementation of a Wayland compositor
  that delegates compositing to a 'host' compositor", with X11 through Xwayland, under ChromiumOS's
  BSD-style licence. It is the other production example of windows the *host* owns.
- **Relation to APPricot.** The cleanest rootless model, with a permissive licence. No browser.
  Useful for a future `appricot-wayland` and for lossless tile compression.

#### waypipe

- **What it is.** A proxy for Wayland clients over a socket, usually carried by ssh. GPL-3.0-or-later
  for the Rust `waypipe`; MIT for the older `waypipe-c`
  ([README, License](https://gitlab.freedesktop.org/mstoeckl/waypipe/-/blob/master/README.md)).
  v0.11.2, 2026-08-25 ([releases](https://gitlab.freedesktop.org/mstoeckl/waypipe/-/releases)).
- **Relation to APPricot.** The viewer must be a native Wayland compositor. There is no browser
  endpoint.

#### WSLg, Weston RDP RAIL and MS-RDPERP

- **What it is.** WSLg shows Linux GUI applications as ordinary Windows windows. Weston is the
  compositor; Microsoft "extended the existing RDP backend of libweston to teach it how to remote
  applications rather than monitor/desktop", using RAIL (Remote Application Integrated Locally) or,
  when guest and host share memory, VAIL. X11 applications go through XWayland. A new **RAIL
  shell** does the window management and is "very simplistic and doesn't involve any actual widgets
  or shell owned pixels" — the server draws no chrome, the host does
  ([README](https://github.com/microsoft/wslg)).
- **Licence and release.** WSLg is MIT; Weston is MIT (Expat); FreeRDP is Apache-2.0. WSLg's
  latest stable release is v1.0.66, 2025-02-26, with a v1.0.71 pre-release on 2025-10-06 and
  commits to 2026-06-24. The RAIL work is **not** upstream: `libweston/backend-rdp` on
  [weston `main`](https://gitlab.freedesktop.org/wayland/weston) holds `rdp.c`, `rdpclip.c`,
  `rdpdisp.c` and `rdputil.c` and no RAIL shell; the shell lives in the `working` branch of
  [microsoft/Weston-mirror](https://github.com/microsoft/Weston-mirror).
- **The protocol is specified.** [MS-RDPERP](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdperp/485e6f6d-2401-4a9c-9330-46454f0c5aba)
  is a written, public window-remoting protocol: "The RAIL client … creates one local window or
  notification icon for every window or notification icon running on the RAIL server", fed by
  "drawing orders from the server to the client describing individual windows and notification
  icons". It also carries local move/resize and the interactions that are not keyboard or mouse.
- **A lesson for APPricot's backends: read a backend's authentication, never assume it.** Weston's
  own remoting backends were exercised in an internal benchmark (September 2026), headless under
  Docker with no GPU. The **RDP backend of weston 14 accepted any username and any password** —
  re-verified with random credentials; its source sets `NlaSecurity` to FALSE and checks no
  credential
  ([`libweston/backend-rdp/rdp.c` at 14.0.2, line 1778](https://gitlab.freedesktop.org/wayland/weston/-/blob/14.0.2/libweston/backend-rdp/rdp.c)).
  The **VNC backend with TLS on shared no security type with an unmodified noVNC** (1.7.0, its
  latest release; its offer was read over the wire and compared with
  [`core/rfb.js`](https://github.com/novnc/noVNC/blob/master/core/rfb.js)): weston offers VeNCrypt
  with the single subtype X509Plain, plus RA256 and RA2. So that pairing is not "authenticated in
  the browser" — it is unreachable from one. Neither behaviour is a bug in the table above; both
  are library defaults doing what they were told. The rule APPricot draws from this pair lives
  in [security/threat-model.md §4.5](security/threat-model.md#45-rule-authentication-is-never-inherited-from-a-library-default).
- **Relation to APPricot.** A shipped, widely used implementation of exactly APPricot's split —
  one toplevel, one host window, chrome drawn by the host — where the host is a desktop rather
  than a web page. Its specification is the most detailed public description of what such a
  protocol must carry.

#### IronRDP

- **What it is.** A Rust implementation of RDP that "supports the web browser as a first class
  target": `web-client/iron-remote-desktop` is published to npm as
  `@devolutions/iron-remote-desktop`, "Web Component providing agnostic implementation for Iron
  Wasm base client", with a Svelte demo beside it
  ([repo](https://github.com/Devolutions/IronRDP), latest npm tag `npm-iron-remote-desktop-v0.11.0`).
- **Licence.** `MIT OR Apache-2.0` in the workspace manifest, with both LICENSE files present.
  That is on the permissive allow list: this is one of the few surveyed projects whose **code**
  we could adapt.
- **RAIL.** `crates/ironrdp-rail` is a "direction-agnostic wire codec for the `RAIL` static virtual
  channel specified by [MS-RDPERP] section 2.2.2", added on 2026-08-11
  ([commit `0161906`](https://github.com/Devolutions/IronRDP/commit/0161906)). The client side is
  deliberately partial: its own comment says window move and IME controls "are not yet exposed".
  `ironrdp-viewer` v0.1.0 was released 2026-07-10. Whether the web client draws RemoteApp windows
  separately **(unverified)**.
- **Relation to APPricot.** A permissive, reviewable Rust reading of the RAIL window model, and a
  worked example of shipping a Rust protocol stack to a browser through WebAssembly.

#### GTK Broadway

- **What it is.** A GDK backend that shows GTK apps in a browser over HTML5 and WebSockets.
  Several apps can share one browser window. The docs call it an experiment that may lag behind
  newer GTK features ([Broadway](https://docs.gtk.org/gtk4/broadway.html)).
- **Licence and release.** LGPL-2.0-or-later (source headers, e.g.
  [gdkdisplay-broadway.c at 4.24.0](https://gitlab.gnome.org/GNOME/gtk/-/blob/4.24.0/gdk/broadway/gdkdisplay-broadway.c)).
  The backend is still in GTK 4.24.0, tagged 2026-09-11.
- **Relation to APPricot.** Proof that per-window HTML5 display works, but only for GTK apps.
  The pilot application is Qt.

#### Qt WebGL streaming

- **What it is.** A Qt platform plugin that streamed a Qt Quick UI's GL calls to one browser. Qt
  Widgets apps were not supported, and only one client per process
  ([Qt 5 docs](https://doc.qt.io/qt-5/webgl.html)). Released in Qt 5.12, under GPL-3.0 or a
  commercial licence ([Qt blog](https://www.qt.io/blog/2018/11/23/qt-quick-webgl-release-512)).
- **Qt 6.** There is no Qt 6 documentation page (`doc.qt.io/qt-6/webgl.html` returns 404). The
  [repo](https://github.com/qt/qtwebglplugin) has `6.2` and `6.3` branches but only a qmake project
  file, and a [forum thread](https://forum.qt.io/topic/130312/error-trying-to-build-qtwebglplugin-in-qt-6-2-0)
  reports the 6.2.0 build failing. Its last tags are Qt 5.15 LTS. We read this as: not shipped
  for Qt 6.
- **Relation to APPricot.** A dead end. The pilot application is a statically built Qt 6 binary
  carrying only the xcb platform plugin, so no other plugin could be loaded into it anyway.

#### WebX

- **What it is.** An X11 remote desktop that tracks *windows* rather than one desktop image. The
  engine connects to an X display and publishes window layout and window images over ZeroMQ; a
  router spawns Xorg, a window manager and an engine per session; a Java relay bridges to
  WebSockets ([webx-engine](https://github.com/ILLGrenoble/webx-engine)). The npm client draws
  each window as a texture in one three.js scene, from JPEG data, and can lower quality and update
  rate on slow links ([webx-client](https://github.com/ILLGrenoble/webx-client)). Two details worth
  keeping: JPEG has no alpha, so a window image "can contain both a color and a grey-scale alpha
  image", the second used as an alpha map; and when WebGL is missing the client falls back to
  plain 2D canvases with blending done in a web worker. Its keyboard mapping is generated from
  Guacamole's keymaps by a small tool in the repo (`utils/guacd-kbd-translator`).
- **Licence and release.** GPL-3.0 for engine, router, relay and client. Engine 1.5.1, 2026-02-19;
  router 1.5.7, 2026-07-15; relay 1.8.6, 2026-07-15; client 1.14.0, 2026-02-10.
- **Relation to APPricot.** The nearest X11 design to `appricot-x11`. But its client composes a
  desktop, it needs a window manager, and its code is GPL. Ideas only.

### Commercial application streaming

#### Amazon WorkSpaces Applications (formerly AppStream 2.0)

- **What it is.** A managed service that streams desktop applications. Users connect with the
  client or an HTML5 browser ([what is](https://docs.aws.amazon.com/appstream2/latest/developerguide/what-is-appstream.html);
  [product page](https://aws.amazon.com/workspaces/applications/) confirms the rename).
- **Lifecycle.** A *fleet* runs streaming instances; a *stack* ties a fleet to access and storage
  policy; a streaming instance "is made available to a single user for application streaming. After
  the user's session completes, the instance is terminated by EC2"; auto scaling rules size
  Always-On and On-Demand fleets. The exception is a *multi-session* fleet, which provisions
  several user sessions on one instance — Windows only, not Linux and not Elastic fleets
  ([concepts](https://docs.aws.amazon.com/appstream2/latest/developerguide/what-is-concepts.html)).
- **Per-window.** *Native application mode* opens each app in its own local window, but it "is not
  available when streaming from Linux instances, streaming in Desktop mode, or when using the
  … macOS client application", and it needs the Windows client 1.1.129 or later
  ([native application mode](https://docs.aws.amazon.com/appstream2/latest/developerguide/feature-support-native-application-mode.html)).
  In the browser the session stays one iframe: the embed SDK's hideable elements include a
  `WINDOW_SWITCHER_BUTTON`, which is what you need when several windows share one view.
- **Embedding.** The stack lists the host domains allowed to embed a session, and AWS adds them
  to the session's Content-Security-Policy header
  ([host domains](https://docs.aws.amazon.com/appstream2/latest/developerguide/specify-host-domain-embedded-streaming-sessions.html)).
  The page loads `appstream-embed.js` and gives it a container `div`; the SDK injects an iframe
  and can hide toolbar items
  ([website integration](https://docs.aws.amazon.com/appstream2/latest/developerguide/configure-website-for-integration.html)).
  The SDK reports session states (`Unknown`, `Reserved`, `Started`, `Disconnected`, `Ended`),
  a `SESSION_ERROR` event with `errorCode` and `errorMessage`, and an interface-state event; its
  functions include `launchApp(appId)`, `launchAppSwitcher()`, `getSessionState()`, `endSession()`
  (ends the session, keeps the iframe) and `destroy()` (deletes the iframe, leaves a running
  session alone)
  ([functions and events](https://docs.aws.amazon.com/appstream2/latest/developerguide/constants-functions-events-embedded-sessions.html)).
  Embedded sessions need "a streaming URL for user authentication. SAML 2.0 and WorkSpaces
  Applications user pools are currently not supported"; custom domains are required where the
  browser blocks third-party cookies
  ([prerequisites](https://docs.aws.amazon.com/appstream2/latest/developerguide/embed-streaming-sessions-prerequisites.html)).

#### Azure Virtual Desktop RemoteApp and Windows 365 Cloud Apps

- **What they are.** RemoteApp publishes single Windows apps from a host pool. Windows 365 Cloud
  Apps give users single apps on Windows 365 Flex Cloud PCs in shared mode
  ([Cloud Apps](https://learn.microsoft.com/en-us/windows-365/enterprise/cloud-apps)).
- **In the browser.** Opening a second RemoteApp from the same host pool in a new tab disconnects
  the first tab, and both apps then show in the new tab
  ([direct launch URLs](https://learn.microsoft.com/en-us/windows-app/direct-launch-urls)). The
  older Remote Desktop web client lost support for public cloud on 2026-03-27; Windows App
  replaces it ([archive note](https://learn.microsoft.com/en-us/previous-versions/remote-desktop-client/connect-windows-cloud-services)).
  The "RemoteApp enhancements" preview lists only Windows desktop clients
  ([RemoteApp enhancements](https://learn.microsoft.com/en-us/azure/virtual-desktop/remoteapp-enhancements)).
- **Balancing.** Breadth-first spreads new sessions; depth-first fills one host up to a limit. A
  returning user goes back to the host holding their session, even in drain mode
  ([load balancing](https://learn.microsoft.com/en-us/azure/virtual-desktop/configure-host-pool-load-balancing)).
- **GA date, now checked.** Windows 365 Cloud Apps went generally available on **2025-11-18**:
  "Windows 365 Cloud Apps, now generally available, uses Windows 365 Flex in shared mode to
  provide users with access to individual applications"
  ([Windows IT Pro blog, datePublished 2025-11-18](https://techcommunity.microsoft.com/blog/Windows-ITPro-blog/windows-365-frontline-updates-and-cloud-apps-general-availability/4470644)).
  Windows 365 Frontline has since been renamed Windows 365 Flex.

#### Citrix Virtual Apps (HDX)

- **Release.** CVAD 2607 LTSR, announced 2026-08-19
  ([Citrix blog](https://www.citrix.com/blogs/2026/08/19/citrix-virtual-apps-and-desktops-2607-ltsr/)).
  Workspace app for HTML5 2603.10, 2026-07-22
  ([what's new](https://docs.citrix.com/en-us/citrix-workspace-app-for-html5/whats-new.html)).
- **Balancing.** Horizontal load balancing sends a session to the least-loaded machine; vertical
  fills the most-loaded machine first, so idle machines can power off
  ([load balance machines](https://docs.citrix.com/en-us/citrix-virtual-apps-desktops/install-configure/load-balance-machines.html)).
- **Per-window in the browser.** The HTML5 session docs describe a **Switch apps** toolbar button:
  "Click the icon to view the already opened apps in the same VDA. This icon doesn't appear in the
  desktop session" ([session experience](https://docs.citrix.com/en-us/citrix-workspace-app-for-html5/session-experience.html)).
  A switcher is what one view of many windows needs, so the reading is "one session view, several
  apps", but the page never says how the windows are drawn **(per-window behaviour unverified)**. Citrix also lists a Workspace app for HTML5 SDK
  ([download page](https://www.citrix.com/downloads/workspace-app/html5/workspace-app-for-html5-sdk-latest.html));
  what it lets a host page do was not read **(unverified)**.

#### Webswing

- **What it is.** A commercial server that runs Java Swing, JavaFX, SWT and Oracle Forms
  applications on the server and delivers them to a browser. Its **Compositing Window Manager**
  "provides superior window handling by rendering each window on its own canvas, improving HTML
  document positioning and java-window communication", and has been "the default and only window
  manager since version 21.1"
  ([CWM docs](https://www.webswing.org/docs/22.1/integrate/cwm.html)). The feature post adds that
  it lets you "run the Webswing (JavaApplication) application with or within the native
  application (html, javascript, etc.)" with two-way calls
  ([CWM post](https://www.webswing.org/en/blog/cwm-feature-that-makes-java-swing-migration-to-web-possible)).
- **Scale.** "Stateless Cluster Servers handle routing, SSL termination, and single sign-on, while
  Session Pools run the application instances behind them", balanced round-robin or by active
  sessions; a browser can reconnect through another cluster server "without terminating the
  application instance" (2026-08-01,
  [scaling post](https://www.webswing.org/en/blog/scaling-java-desktop-apps-in-the-browser-add-nodes-not-desktops)).
- **Beyond Java.** A **Linux Web Launcher** was announced on 2026-06-27: "a custom Wayland
  compositor", with X11 applications through XWayland, which "renders the resulting output into a
  buffer" that capture middleware reads — so one composed output, not one stream per window, on the
  evidence of that post. It is "available for early-access discussions on demand"
  ([post](https://www.webswing.org/en/blog/linux-desktop-to-web-bringing-linux-applications-to-the-browser-with-webswing)).
- **Licence and version.** Proprietary ("© 2012 - 2026 Webswing Ltd. All Rights Reserved."). Its
  [documentation index](https://www.webswing.org/docs) goes up to 26.1; there is no public release
  feed to date a release from **(release date unverified)**.
- **Relation to APPricot.** The closest anyone comes commercially to part 2 of our gap: real
  per-window canvases inside the host's document. It is Java-only, closed, and its Linux path is
  neither per-window (on present evidence) nor generally available.

#### Cameyo by Google

- **What it is.** Virtual app delivery: it streams individual Windows apps, not desktops, and turns
  each into a progressive web app ([cameyo.google](https://cameyo.google/)).
- **Hosting.** Servers can be self-hosted on "an up-to-date Windows Server 2019, or later", and
  the same page says "Windows Server 2025 is not yet supported"
  ([self-hosted server](https://support.google.com/cameyo/answer/16390595?hl=en)). Setup runs
  through the Cameyo Admin console, so the control plane appears to stay Cameyo's; the page does
  not say so outright **(unverified)**.
- **Relation to APPricot.** "One app, one PWA" is a good host-side UX idea. Windows only.

#### ThinLinc

- **What it is.** A Linux remote desktop server with an HTML5 client (Web Access), load balancing
  across servers, and single-application publishing. Proprietary; free up to 10 concurrent users
  ([features](https://www.cendio.com/thinlinc/features/)). 4.21.0, 2026-08-21, adds OpenID Connect
  to Web Access ([4.21.0](https://www.cendio.com/blog/introducing-thinlinc-4-21-0/)). 4.20.0,
  2026-01-09, made Web Access work behind a reverse proxy
  ([4.20.0](https://www.cendio.com/blog/introducing-thinlinc-4-20-0/)).

#### NoMachine

- **What it is.** Remote desktop and terminal servers. The Enterprise products include a Web Player
  served by an embedded web server, with optional WebRTC
  ([web access](https://www.nomachine.com/enterprise/web-based-remote-access)). Terminal Server
  clusters balance sessions over nodes
  ([terminal server](https://www.nomachine.com/enterprise/terminal-server-products)).
  NoMachine 10 launched 2026-07-30; 10.1.7 shipped 2026-09-14 ([news](https://www.nomachine.com/news)).
  Proprietary. Whether the Web Player shows single app windows **(unverified)**.

### New in 2025 and 2026

Three Wayland projects appeared in August and September 2026 that stream one video per window to
a browser: lwfa (repo created 2026-08-04), Webland (2026-08-26) and Elsewhere (2026-09-04). All
are single-author and weeks old. None is a candidate dependency; all are useful
prior art.

#### Webland

- A Wayland compositor whose display is a browser. Each window is its own H.264 video (VA-API);
  the browser composites them, one 2D canvas per window, with WebCodecs. A `wl_shm` client is
  uploaded instead of imported, and "fall[s] back to a deflated damage rectangle diffed against
  what the browser already holds if the encoder cannot take the surface at all". It manages
  XWayland itself. The protocol has no auth
  ([README](https://github.com/husseinhareb/webland)).
- The frontend is a Leptos application compiled to WebAssembly, with its own shell: window chrome,
  panel, launcher, four workspaces, Alt+Tab. It is not a library a host page could drive. The
  frame rate "tracks the load rather than a clock, which is the point of pacing on browser acks" —
  the same ack-driven pacing xpra's `window-ack` gives.
- `backend/Cargo.toml` and `frontend/Cargo.toml` say `license = "MIT"`, but the repo has no
  LICENSE file (GitHub detects none). Created 2026-08-26; last commit 2026-09-18; no releases;
  one author.
- **Bandwidth, read from the README this time** (earlier internal notes quoted it second-hand): `kitty`
  at 1280x800, idle sends nothing after the first keyframe; a cursor costs 2.2 frames/s and
  5.0 KiB/s; scrolling flat out is 39.8 frames/s and 588 KiB/s, "about 4.8 Mbit/s", against "about
  47 Mbit/s as deflated damage rectangles". Typing lands around 52 ms median.

#### Elsewhere

- A headless Wayland compositor whose screen is a browser tab. It encodes with VA-API or NVENC,
  or on the CPU at 30 Hz with Mesa's llvmpipe. It needs XWayland for X11 apps
  ([README](https://github.com/ryanpetris/elsewhere)). MIT (LICENSE). v0.10.1, 2026-09-11; created
  2026-09-04.
- Per-window streams landed on 2026-09-04, hours after the repository was created:
  `GET /ws/window/{id}` streams one window. Each window renders "into a private dmabuf swapchain
  with its own damage tracker after each output frame, so an idle window sends nothing", with one
  encoder per stream. Three details are worth stealing: a size change "is applied once it has held
  for 150 ms, so a drag rebuilds the encoder once"; the socket closes with code 4003 when the
  window closes and 4001 on token rotation; a tab that stops reading is dropped after 10 s
  ([issue #1](https://github.com/ryanpetris/elsewhere/issues/1), closed the same day).
- **Auth.** Bearer tokens with explicit per-feature permissions. "The viewer page takes it from its
  URL fragment once (`#token=`, never sent to the server), keeps it in `sessionStorage` and drops
  it from the address bar, and sends it as the first message on its WebSocket. There are no
  cookies and a token is never in a URL the server sees." It also exposes its window list, input
  and screenshots as MCP tools at `/mcp` ([README](https://github.com/ryanpetris/elsewhere)).

#### lwfa

- Per-window H.264 or HEVC to the browser over one WebSocket, decoded with WebCodecs. Its engine
  is Smithay-based and runs **nested** inside the user's own compositor: "it does not own a display
  outright" ([README](https://github.com/ngodn/lwfa)). MIT. v1.5.11, 2026-09-12. One author.
- **NVENC is not a hard requirement, and its cap is instructive.** The install page says
  "**NVIDIA + NVENC** for hardware video encoding. This is the only hardware encoder supported
  today; Intel and AMD fall back to JPEG, which works and costs bandwidth… Eight concurrent NVENC
  sessions is the consumer-card limit; the ninth window degrades to JPEG"
  ([docs/install.md](https://github.com/ngodn/lwfa/blob/master/docs/install.md)). One encoder per
  window meets a hardware ceiling that a whole-screen encoder never sees — a real risk for
  `appricot-encode` if it ever grows a hardware path.

#### node-x11's JavaScript X server

- `node-x11` now ships a pure-JavaScript X server that runs in the browser, with GLX over WebGL,
  plus an X11 client that supports Composite, Damage, XFixes and XTest
  ([README](https://github.com/sidorares/node-x11)). MIT. v4.2.1, 2026-09-13.
- The idea: the browser *is* the X server and each toplevel is naturally its own element. Earlier
  internal research lists the costs: raw X11 on the wire, and the app dies with the tab. Whether
  the pilot application runs against it **(unverified)**.

#### Moves by the incumbents

- xpra has an experimental Wayland server backend, "`xpra seamless --backend=wayland`", with the
  advice "Use the X11 backend unless you need Wayland"
  ([Seamless.md](https://github.com/Xpra-org/xpra/blob/master/docs/Usage/Seamless.md)). It arrived
  in 6.5 (changelog dated 2026-05-06, tagged 2026-06-15). Earlier internal research found no
  XWayland code in it (not re-read here).
- Selkies 2.0 moved to one WebSocket port by default (above).
- KasmVNC's Wayland work, in its maintainers' own words on
  [issue #193](https://github.com/kasmtech/KasmVNC/issues/193): "Wayland support is in the works.
  We are internally testing KasmVNC for KDE today" (2026-04-27), and "our current PoC for KDE
  requires Kwin patches… Right now we are able to get away with a patch, so no fork" (2026-06-01).
  The same thread names linuxserver.io's pixelflux, "their own purpose-built Wayland compositor
  written on top of the Smithay library", as the alternative approach. v1.5.0 ships no Wayland.

## 3. Where APPricot fits

The gap has five parts. No project in this survey covers all five.

1. **Per-window.** Each toplevel is its own stream with its own damage.
2. **Host-owned windows.** The client library draws into canvases the host provides. The host
   draws the chrome, focus and stacking. The windows become the host's own UI elements.
3. **A permissive licence**, so the client can ship inside the host's own bundle.
4. **GPU-less and X11-first**, because the pilot application is X11-only and runs on a GPU-less
   VPS.
5. **Orchestrated.** A sandboxed container per session, a warm pool, a readiness gate, a reaper,
   egress scoping, and placement with session affinity.

Who comes closest on each part:

- **Per-window in a browser:** xpra-html5, Greenfield, GTK Broadway, WebX, Webland, Elsewhere,
  lwfa and, for Java only, Webswing. All but Webswing draw the windows in their *own* page or
  scene. Most also fail part 3 (GPL, AGPL, MPL, or no licence file) or part 4 (GPU-first, or
  Wayland-only so X11 needs XWayland on top).
- **Per-window outside a browser, with the host owning the window:** WSLg (Weston's RDP RAIL
  backend into `mstsc`) and ChromeOS's sommelier. Both are production systems built on exactly the
  split APPricot wants — server draws no chrome, host owns the window — and neither targets a web
  page. MS-RDPERP is the written spec of that contract.
- **Embeddable:** Webswing's CWM comes closest, placing each window's canvas in the host document,
  but it is Java-only and proprietary. `guacamole-common-js` has the right library shape and a
  permissive licence, but one display per connection. The WorkSpaces Applications Embed SDK and
  Kasm's iframe recipe embed a whole session; neko's README offers embedding as a use case without
  saying how.
- **Orchestration:** Kasm Workspaces is the closest (container per session, staging pool, expiry,
  zones, autoscale, egress scoping), but proprietary and not per-window. Among permissive projects:
  SealSkin (MPL-2.0) runs a container per session over the Docker socket with per-session
  credentials, neko-rooms (Apache-2.0) a container per room with per-room CPU and memory caps, and
  Wolf (MIT) a container per session on demand for Moonlight clients. The cloud services
  orchestrate VMs; Webswing orchestrates stateless routers in front of session pools.

So APPricot is, roughly: **xpra's window model, with a Guacamole-style embeddable client, behind a
Kasm-style session lifecycle**, X11 first and GPU-less, under a permissive licence. Put the other
way round: WSLg's split, delivered to a web page instead of a desktop.

Why X11 first is not a step backwards: the pilot application ships only the xcb platform plugin.
The internal benchmark (September 2026) measured Xvfb plus that application at 286 MiB summed RSS,
against 370 MiB for weston plus Xwayland plus the same application, and per-window capture works
on plain Xvfb through Composite. The Wayland-native projects above would all run it through
XWayland.

**Where APPricot does not compete.** Full desktops (Selkies, webtop, KasmVNC). Games and
high-motion video at low latency (Sunshine, Wolf, Selkies). Windows applications (Citrix, AVD,
WorkSpaces Applications, Cameyo). Reaching an existing machine (RustDesk, NoMachine, ThinLinc).

**Risks the prior art shows.**

- **Keyboard and clipboard** are where browser clients collect bugs. Earlier internal research
  counted them in xpra-html5's tracker (its own keyword classification, unverified).
- **Server strings become markup.** KasmVNC (1.5.0's pinned client) and xpra-html5 (`master` and
  the v20 release) both still write server text as HTML. Note also that KasmVNC's fix, when it
  reaches a release, still leaves an `innerHTML` round-trip in place — a fix that narrows a sink
  is not the same as removing the pattern. [ADR-0003](adr/0003-untrusted-server-client.md) exists
  for this.
- **Lossless is not free.** Webland's fallback path costs about ten times the bandwidth of its
  H.264 path: about 47 Mbit/s against about 4.8 Mbit/s for the same scrolling terminal (its
  README). `appricot-encode` starts lossless, so the codec choice matters.
- **Per-window encoders meet hardware ceilings.** lwfa degrades the ninth window to JPEG because a
  consumer NVIDIA card allows eight concurrent NVENC sessions. One encoder per window is a
  different resource model from one encoder per screen.
- **A single maintainer can stop.** Greenfield did. Three of the closest projects (Webland,
  Elsewhere, lwfa) have exactly one contributor each. Keep APPricot's dependency graph to
  maintained crates.

## 4. What to borrow

"Code?" says whether code may be copied or adapted into APPricot's own crates or its client
packages, under the rule in [ADR-0002](adr/0002-licence.md) §2. "Idea" means: read, describe it in
our own words, write our own code.

| Idea | From | Lands in | Source licence | Code? |
|---|---|---|---|---|
| A window model with create, metadata, move-resize, draw and ack messages; override-redirect windows forwarded as their own windows | xpra [Protocol.md](https://github.com/Xpra-org/xpra/blob/master/docs/Network/Protocol.md) | `appricot-proto`, `appricot-core` | GPL-2.0-or-later | Idea |
| Every draw is acked with its sequence and the client's decode time, which gives the server what it needs to pace each window (xpra's pacing itself lives in its code, not in the spec) | xpra `window-ack` (same spec) | `appricot-core` frame scheduling | GPL-2.0-or-later | Idea |
| Size limits that tighten before authentication (xpra: 4 MiB before `hello`, 16 MiB after, 256 MiB decompressed) | xpra (same spec) | `appricot-proto` bounded decoding, with much smaller numbers | GPL-2.0-or-later | Idea |
| Picture encodings chosen per window and per update (WebP, JPEG, PNG, raw, scroll) | xpra; measured in an internal benchmark (September 2026) | `appricot-encode` | GPL-2.0-or-later | Idea |
| One entry point that authenticates, finds the wanted session, or starts one | xpra [proxy server](https://github.com/Xpra-org/xpra/blob/master/docs/Usage/Proxy-Server.md) | L3 broker, node proxy | GPL-2.0-or-later | Idea |
| Alpha as a second, cheaper stream beside the colour stream (x264 at 1200 kbit/s beside 12000) | Greenfield [`gst_frame_encoder.c`](https://github.com/udevbe/greenfield/blob/6c578f4/packages/compositor-proxy/native/encoding/src/gst_frame_encoder.c); WebX sends a grey-scale alpha image beside its JPEG | `appricot-encode` | AGPL-3.0-or-later; GPL-3.0 | Idea |
| Encode per surface on commit, so an idle window costs nothing (our own inference from Wayland's commit model, not a claim by the project) | Greenfield [design](https://greenfield.app/pages/design/) | `appricot-core` damage | AGPL-3.0-or-later | Idea |
| Window registry events: created, destroyed, title changed, app id changed, activation changed | Greenfield [UserShellApi.ts](https://github.com/udevbe/greenfield/blob/6c578f4/packages/compositor/src/UserShellApi.ts) | `@appricot/client` events | AGPL-3.0-or-later | Idea |
| Rootless model: the server serialises window state; the viewer recreates each window | [wprs](https://github.com/wayland-transpositor/wprs) | ADR-0004 model; future `appricot-wayland` | Apache-2.0 | Yes, with NOTICE |
| Fast lossless compression: transpose, DPCM, YUV-like transform, zstd | wprs | `appricot-encode` lossless tiles | Apache-2.0 | Yes, with NOTICE |
| The embeddable-library shape: `new Client(tunnel)`, then the host appends the element it gets back | [guacamole-common-js](https://guacamole.apache.org/doc/gug/guacamole-common-js.html) | `@appricot/client`, `@appricot/react` | Apache-2.0 | Yes, with NOTICE |
| A versioned handshake (`select`, `args`, `connect`, `ready`); off-screen buffers as negative layers | Guacamole [protocol](https://guacamole.apache.org/doc/gug/guacamole-protocol.html) | `appricot-proto` v0 | Apache-2.0 | Yes, with NOTICE |
| Balancing groups: fewest active users wins; affinity keeps a user on one target | Guacamole [administration](https://guacamole.apache.org/doc/gug/administration.html) | L3 placement | Apache-2.0 | Yes, with NOTICE |
| Browser key events to X11 keysyms: `Guacamole.Keyboard` does exactly this in the browser. The per-layout tables in guacamole-server map scancodes and modifier state to characters and keysyms, one file per layout; WebX generates its own mapping from those files with a small tool in its repo | [Keyboard.js](https://github.com/apache/guacamole-client/blob/main/guacamole-common-js/src/main/webapp/modules/Keyboard.js), [guacamole-server keymaps](https://github.com/apache/guacamole-server/tree/main/src/protocols/rdp/keymaps), [webx-engine `utils/guacd-kbd-translator`](https://github.com/ILLGrenoble/webx-engine/tree/master/utils/guacd-kbd-translator) | `@appricot/client` input capture and key mapping | Apache-2.0 | Yes, with NOTICE |
| Request a session, poll status until running, then connect | Kasm [developer API](https://docs.kasm.com/docs/develop/reference/developer-api) | L2 readiness gate, ticket API | Proprietary | Idea |
| A staged pool whose unassigned members expire and are re-created | Kasm [staging](https://kasm.com/docs/latest/guide/staging.html) | L2 warm pool, reaper | Proprietary | Idea |
| Egress scoped to named services per workspace | Kasm 1.19 [OpenZiti egress](https://kasm.com/kasm-insights/kasm-workspaces-119-kubernetes-goes-ga-zero-trust-egress-and-a-release-built-for-production) | L2 egress policy | Proprietary | Idea |
| An allowlist of host origins per app, enforced through the CSP header; a small session-state enum (reserved, started, disconnected, ended) and an error event for the host page; `end()` and `destroy()` as separate calls | WorkSpaces Applications [host domains](https://docs.aws.amazon.com/appstream2/latest/developerguide/specify-host-domain-embedded-streaming-sessions.html), [functions and events](https://docs.aws.amazon.com/appstream2/latest/developerguide/constants-functions-events-embedded-sessions.html) | L2 ticket API, `@appricot/client`, `@appricot/react` | Proprietary | Idea |
| A fresh instance per session, destroyed afterwards | WorkSpaces Applications [concepts](https://docs.aws.amazon.com/appstream2/latest/developerguide/what-is-concepts.html) | L2 | Proprietary | Idea |
| Breadth-first versus depth-first placement; drain mode; a returning user goes back to their node | AVD [load balancing](https://learn.microsoft.com/en-us/azure/virtual-desktop/configure-host-pool-load-balancing), Citrix [load balance machines](https://docs.citrix.com/en-us/citrix-virtual-apps-desktops/install-configure/load-balance-machines.html) | L3 placement and affinity | Proprietary | Idea |
| One WebSocket on one port for everything; no STUN or TURN by default | Selkies [2.0.0rc0](https://github.com/selkies-project/selkies/releases/tag/2.0.0rc0) | `appricot-streamer` | MPL-2.0 | Idea |
| A per-window stream endpoint; a damage tracker per window, so idle windows send nothing; a resize applied only once it has held 150 ms, so one drag rebuilds one encoder; distinct close codes for "window closed" and "token rotated" | Elsewhere [issue #1](https://github.com/ryanpetris/elsewhere/issues/1) | `appricot-streamer`, `appricot-core` | MIT | Yes, with notice |
| The session token rides in the URL **fragment**, is moved to `sessionStorage`, is dropped from the address bar and is sent as the first WebSocket message, so it never reaches the server in a URL; each token carries explicit per-feature permissions | Elsewhere [README](https://github.com/ryanpetris/elsewhere) | `appricot-streamer` auth, `@appricot/client` | MIT | Yes, with notice |
| Input authority is enforced on the server, not in the page: a viewer's input is refused whatever its client sends; tokens carry a role and a slot | Selkies [2.0.0rc0](https://github.com/selkies-project/selkies/releases/tag/2.0.0rc0) | `appricot-streamer`, L2 ticket API | MPL-2.0 | Idea |
| Capture detects changed regions and cuts a frame into stripes encoded in parallel | Selkies' [pixelflux](https://github.com/selkies-project/pixelflux) | `appricot-encode` | MPL-2.0 | Idea |
| Frame pacing driven by the browser's acknowledgements rather than a clock, so the rate tracks load | Webland [README](https://github.com/husseinhareb/webland) (and xpra's `window-ack`) | `appricot-core` frame scheduling | No LICENSE file | Idea |
| A window-remoting protocol written down: per-window orders, owner windows, notification icons, local move/resize, and the input that is neither keyboard nor mouse | [MS-RDPERP](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdperp/485e6f6d-2401-4a9c-9330-46454f0c5aba), as read through [`ironrdp-rail`](https://github.com/Devolutions/IronRDP/tree/master/crates/ironrdp-rail) | `appricot-proto` v0, `appricot-core` | Spec: Microsoft Open Specifications. Code: MIT OR Apache-2.0 | Idea for the spec; code may be adapted from IronRDP |
| A shell whose only job is remoting windows, with "no actual widgets or shell owned pixels" — the server never draws chrome | WSLg [README](https://github.com/microsoft/wslg) (RAIL shell) | ADR-0004, `appricot-x11` window-management duties | MIT | Idea |
| One container per session created over the Docker socket, with per-session credentials and a user-signed, five-minute JWT instead of a password | [SealSkin](https://docs.linuxserver.io/selkies/components/sealskin/) | L2 session manager, ticket API | MPL-2.0 | Idea |
| Per-room resource caps set at container creation: CPU shares, `NanoCPUs`, memory, shm size, devices | [neko-rooms `manager.go`](https://github.com/m1k1o/neko-rooms/blob/8ab1caf/internal/room/manager.go) | L2 app profiles | Apache-2.0 | Yes, with NOTICE |
| One image recipe per application, versioned in the open | [kasmtech/workspaces-images](https://github.com/kasmtech/workspaces-images) (`LICENSE.md` grants MIT for that repository only) | L2 app profiles | MIT | Yes, with notice |
| Stateless routers in front of session pools, so a dropped browser connection reconnects through another router without ending the application instance | Webswing [scaling post](https://www.webswing.org/en/blog/scaling-java-desktop-apps-in-the-browser-add-nodes-not-desktops) | L3 broker | Proprietary | Idea |
| Each window on its own canvas, positioned by the host's HTML, with two-way calls between the host page and the application | Webswing [CWM](https://www.webswing.org/docs/22.1/integrate/cwm.html) | `@appricot/client`, `@appricot/react` | Proprietary | Idea |
| One canvas per window; the browser composites | Webland [README](https://github.com/husseinhareb/webland) | `@appricot/client` | No LICENSE file | Idea (until a licence file exists) |
| A container per session, created on demand by the streaming server | Wolf [README](https://github.com/games-on-whales/wolf) | L2 | MIT | Yes, with notice |
| Window-based X11 capture: window layout events plus per-window images; quality and rate drop on slow links | WebX [engine](https://github.com/ILLGrenoble/webx-engine), [client](https://github.com/ILLGrenoble/webx-client) | `appricot-x11`, `appricot-core` | GPL-3.0 | Idea |
| Each app presented as its own installable web app | Cameyo [cameyo.google](https://cameyo.google/) | Host-side guidance | Proprietary | Idea |
| The web client served behind the customer's reverse proxy, with the customer's identity provider | ThinLinc [4.20.0](https://www.cendio.com/blog/introducing-thinlinc-4-20-0/), [4.21.0](https://www.cendio.com/blog/introducing-thinlinc-4-21-0/) | L2/L3 deployment docs | Proprietary | Idea |
| Two warnings to turn into tests: server strings written with `innerHTML` / `.html()` | KasmVNC client [ui.js at 475ecfa](https://github.com/kasmtech/noVNC/blob/475ecfa5356579ef222983c7ce4619a7576a3bce/app/ui.js#L1738), xpra-html5 [Window.js at 3029a04](https://github.com/Xpra-org/xpra-html5/blob/3029a04/html5/js/Window.js#L653) | `@appricot/client` tests, [ADR-0003](adr/0003-untrusted-server-client.md) | MPL-2.0 (both; KasmVNC's package declares its copy GPL-2+) | Idea (a test we write) |

## 5. Licence rules for borrowing

APPricot's own code is **MIT OR Apache-2.0**. What it may link is narrower: the permissive set
in [ADR-0002](adr/0002-licence.md) §2 — MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception,
BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Zlib, 0BSD.

- **Code may be adapted, with the licence notice kept:** Guacamole (Apache-2.0), neko and
  neko-rooms (Apache-2.0), wprs (Apache-2.0), IronRDP (MIT OR Apache-2.0), Wolf and
  gst-wayland-display (MIT), WSLg (MIT), Weston (MIT), Elsewhere (MIT), lwfa (MIT), node-x11
  (MIT), Kasm's `workspaces-images` and `workspaces-core-images` (MIT, that repository only),
  sommelier (ChromiumOS BSD-style). Apache-2.0 also needs its NOTICE handling.
- **Ideas only, never code, in APPricot's crates or in `@appricot/*`:**
  - GPL: the KasmVNC server, xpra, Sunshine, Moonlight, moonlight-web-stream, the Rust
    `waypipe`, WebX, the webtop and docker-sealskin packaging repos, Qt WebGL.
  - AGPL: Greenfield (AGPL-3.0-or-later), RustDesk.
  - LGPL: GTK Broadway. Linking rules aside, it is not on the permissive allow list.
  - MPL-2.0: xpra-html5, Selkies, pixelflux, SealSkin, noVNC's core and KasmVNC's web client
    (`kasmtech/noVNC`, MPL-2.0 at source, GPL-2+ as KasmVNC packages it). MPL is not on the
    permissive allow list; ADR-0002 says taking any MPL-2.0 package needs an amendment. So:
    read-only.
  - Proprietary or source-available: Kasm Workspaces, WorkSpaces Applications, AVD / Windows 365,
    Citrix, Cameyo, ThinLinc, NoMachine, Webswing.
  - No licence file: Webland. Its `Cargo.toml` files say MIT, but without the text we do not copy.
  - A published specification is not code: MS-RDPERP may be read and implemented. Microsoft's
    Open Specifications terms govern it; read them before leaning on it
    ([MS-RDPERP](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdperp/485e6f6d-2401-4a9c-9330-46454f0c5aba))
    **(terms not read here)**.
- **How to take an idea from GPL or AGPL code.** Describe the idea in prose, in an ADR or a design
  doc, and link the source. Write our code from the description, not with the source open.
  Do not paste snippets into issues or commits.
- **Running is not copying.** Running an unmodified GPL program as a separate process in an image
  is aggregation. It still needs a notice and a source offer. This matters only if the
  S3 alternative (an unmodified xpra server) is ever chosen. It never enters an APPricot crate's
  dependency graph.

## 6. Open and unverified

- Whether Citrix's HTML5 client, NoMachine's Web Player, ThinLinc's Web Access, Cameyo's PWAs or
  the Windows App web client show separate windows for one app inside the page. Citrix's **Switch
  apps** button and AWS's `WINDOW_SWITCHER_BUTTON` both point at "one view, several windows", but
  neither vendor says it.
- Whether Selkies, neko or Elsewhere can be embedded in a host page. neko's README names it as a
  use case without describing the mechanism; Elsewhere opens a window in a popup, not in a host
  element.
- Whether Webswing's Linux Web Launcher streams per window or one composed output. Its
  announcement describes one rendered output; it is early access, so there is nothing to test.
- Whether IronRDP's web client can draw RemoteApp windows separately, now that the RAIL codec
  exists.
- GPU needs of RustDesk, neko and the commercial services beyond "optional". Wolf's CPU-only path
  was not confirmed either.
- Whether the pilot application runs against node-x11's JavaScript X server.
- Whether xpra 6.5.3 on the wire matches the packet names in `master`'s Protocol.md (the document
  describes the 6.6 development series).
- Microsoft's Open Specifications terms for MS-RDPERP were not read.
- Not surveyed at all, and probably worth a look before L2/L3 design: Anbox Cloud, Hyperbeam,
  Parallels RAS, TSplus, x2go.

## 7. What the verification changed

A second pass tried to refute every version, date, licence and capability claim above against its
primary source: the GitHub and GitLab APIs for releases, tags, commits and submodule pins; LICENSE
files, source headers and package manifests for licences; and the vendor pages themselves for
behaviour. Every release row in the table was re-queried.

**Refuted.**

- **"lwfa: NVIDIA NVENC required."** Its install page says NVENC is the only *hardware* encoder
  today and that "Intel and AMD fall back to JPEG, which works and costs bandwidth". The related
  "it cannot run headless" also overstated the README: lwfa runs nested in an existing compositor
  and "does not own a display outright", which is not the same claim.
- **"Sunshine: hardware encoder expected."** Its README says "Software encoding is also
  available."
- **"Kasm's iframe recipe needs SameSite and auth-domain cookie changes."** The page never
  mentions SameSite. What it requires is sibling or sub-domains plus the **Kasm Auth Domain**
  setting, because the two session cookies are otherwise blocked.
- **"Greenfield: encode per surface on commit; alpha as a separate plane" sourced to its design
  page.** The design page says neither. The split-alpha pipeline is in
  `gst_frame_encoder.c`; encode-on-commit is our own inference. Both rows now cite what they
  actually rest on.
- **"Kasm Workspaces is closed source"** is not what the licence page says. It is commercially
  licensed and its platform code is not published, while its image repositories carry an MIT
  grant.
- **"KasmVNC and its web client are GPL."** The server is GPL-2.0-or-later, but the client
  repository `kasmtech/noVNC` is MPL-2.0 by its own LICENSE and file headers; GPL-2+ is how
  KasmVNC's package declares it. Either way it is not on the permissive allow list, but the table
  said the wrong thing.
- **"webtop: latest 2026-09-19."** That is a `dev-` pre-release. The latest stable tag is
  `c6d858e8-ls314`, 2026-09-15.
- **"The move from KasmVNC to Selkies is an inference."** It is in webtop's own changelog:
  "17.06.25: Rebase all images to Selkies".
- **"AWS: an instance per user, terminated after the session."** True except for multi-session
  fleets, which put several sessions on one instance (Windows only).
- **Licence identifiers sharpened:** Greenfield is AGPL-3.0-**or-later** (both package manifests);
  the KasmVNC server is GPL-2.0-**or-later** (source headers).
- **"Qt WebGL: Qt 5.15 only."** It was a technology preview in Qt 5.10 and a released feature in
  Qt 5.12; 5.15 LTS is only where the tags stop.
- **Two dates for xpra 6.5.** The changelog heading says 2026-05-06; the `v6.5` tag is dated
  2026-06-15 and the GitHub release 2026-06-16. The page now says so instead of picking one.

**Resolved from the open list.**

- Windows 365 Cloud Apps went GA on **2025-11-18** (Microsoft's own blog, with its
  `datePublished`), and Windows 365 Frontline is now Flex.
- neko-rooms isolates a room as a Docker container created through the Docker API, with per-room
  CPU, memory, shm and device limits (`internal/room/manager.go`).
- Webland's bandwidth numbers were read from its README this time, not quoted second-hand from
  earlier internal notes, and they hold: about 4.8 Mbit/s scrolling over H.264 against about
  47 Mbit/s on the fallback path.
- Sunshine's GPU question is answered (software encoding exists).
- KasmVNC's Wayland status is now quoted from the maintainers in issue #193 rather than summarised
  from earlier internal notes.

**Confirmed, with the evidence tightened.**

- The type-178 `innerHTML` sink: v1.5.0 really does pin `kasmweb` at noVNC `475ecfa`, checked
  through the contents API, and the sink is on line 1738 there. The fix is commit `e0978e6`
  (2026-08-27), pulled into KasmVNC `master` on 2026-08-28 — so still in no release. New finding
  on top: the fixed file still round-trips the same element through `innerHTML` for its latency
  tag, which is safe as written but is the pattern ADR-0003 bans.
- xpra-html5's `.html()` title sink is on `master` **and** in the shipped v20 (line 649), so it is
  not a `master`-only wart. Links are now pinned to commits.
- xpra's packet limits, and `window-ack`'s `decode_time_us`, are quoted verbatim; the spec's own
  scope line (the 6.6 development series, as of 2026-08-16) is now stated, because it bounds every
  claim taken from it.
- Elsewhere's per-window streams, Kasm 1.19.0's GA date and OpenZiti egress, the WorkSpaces Embed
  SDK's CSP host domains and session events, AVD's one-tab RemoteApp behaviour and the RD web
  client's 2026-03-27 end of public-cloud support, the Qt 6 documentation 404, ThinLinc 4.21.0 and
  4.20.0, NoMachine 10.1.7, and Citrix 2607 LTSR all stand as written.

**Added.**

- Four projects the first pass missed, three of them material: **Webswing** (the only commercial
  system that puts each window's canvas in the host's own document, Java only), **WSLg with
  Weston's RDP RAIL backend and the MS-RDPERP specification** (the same server/host split APPricot
  wants, in production, with a written protocol), **IronRDP** (a permissively licensed Rust RAIL
  codec and a
  browser client, so code we could actually adapt) and **SealSkin** (the permissively licensed L2
  that Selkies now has).
- Three components named inside existing notes: **pixelflux** (Selkies' Rust capture and encode
  extension, with striped parallel encoding), **gst-wayland-display** (Wolf's MIT Smithay
  compositor) and **sommelier** (ChromeOS's host-owned-window precedent).
- Twelve rows in §4, mostly from those additions, plus Elsewhere's token handling and resize
  debounce, Selkies' server-side input authority, and Webland's ack-driven pacing.
- A new risk in §3: one encoder per window meets hardware session ceilings (lwfa's ninth window).
