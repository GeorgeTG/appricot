# syntax=docker/dockerfile:1
#
# APPricot streamer image: the artefact a host pulls, built and pushed only by
# .github/workflows/release.yml at a `v*` tag (GHCR, the runner's own token, no other secret):
#
#   docker run --rm ghcr.io/<owner>/appricot/streamer:0.1.0   # the version line, exit 0
#   docker run --rm ghcr.io/<owner>/appricot/streamer:0.1.0 serve   # serve one session
#
# A session image consumes it as a layer instead of building the streamer from source:
#
#   FROM ghcr.io/<owner>/appricot/streamer:0.1.0
#   CMD ["serve"]                     # this image sets no CMD (see the end of the file)
#   ENV APPRICOT_BIND=unix:/run/appricot/session.sock
#   # plus Xvfb, the application, and APPRICOT_STREAM_TOKEN from your own session machinery
#
# or take the binary alone and keep your own base:
#
#   COPY --from=ghcr.io/<owner>/appricot/streamer:0.1.0 \
#        /usr/local/bin/appricot-streamer /usr/local/bin/appricot-streamer
#
# `<owner>` is the GitHub account that owns this repository, lower-cased; the workflow derives it
# from `github.repository_owner`, so the name cannot drift from the push.
#
# What this image is NOT, on purpose:
#
# - **No display server and no application.** The streamer connects to whatever `APPRICOT_DISPLAY`
#   (or `$DISPLAY`) names, and who starts that X server is an open question this artefact must not
#   answer by itself (docs/architecture.md §8, carried to M4). M3's session image is the host's:
#   it brings Xvfb, the application and its libraries, and APPricot brings the stream.
# - **No per-session token.** `appricot-streamer serve` refuses to start without
#   `APPRICOT_STREAM_TOKEN`, and a stream is served only to a client whose first message carries
#   it (docs/protocol/README.md, rule 8). The token is the host's to mint, one per session.
# - **No port declared.** The streamer binds loopback or a unix socket inside its own container
#   and refuses every other address form, so the host's own proxy opens the loopback leg.
#   `EXPOSE` would suggest it can be reached directly, which is exactly what its bind grammar
#   forbids.
#
# Both bases are pinned by tag AND digest — a tag can be rebuilt in place, and then two builds a
# week apart would start from different layers with nothing checked. The Rust base is the same
# tag and digest as docker/dev/Dockerfile, so the released binary is compiled by the toolchain
# every gate ran on; `just pins` fails when that tag's version drifts from rust-toolchain.toml.
# Bump tag, digest and that file together.
#
# linux/amd64 only, like the dev image: one platform is what this release builds and verifies. A
# multi-arch release (`docker buildx build --platform linux/amd64,linux/arm64`, under QEMU) is a
# decision that needs its own measurement, never a default.
#
# The wire protocol is v0 and the interface may still move, so this is a 0.1.x artefact: a host
# may take it, and a minor bump may still break it (docs/roadmap.md).

# --- build ------------------------------------------------------------------------------------
FROM rust:1.94.1-slim-bookworm@sha256:cf9dd0ec73e75f827fe59123fff9dc65af1a1c8363c3c31ee8d7f8ad0b6a5fb2 AS build

# Debian's CDN alias by default, and a build argument for the same reason the dev image has one
# (docker/dev/Dockerfile). It is safe to point elsewhere: apt verifies the Release signature
# against Debian's keyring whatever the mirror serves, so a mirror can be slow or stale, but not
# forge a package.
ARG DEBIAN_MIRROR=deb.debian.org

# protobuf-compiler: prost-build compiles
# crates/appricot-proto/proto/appricot/v0/wire.proto at build time, as it does in the dev image.
# Nothing else is installed: x11rb's RustConnection speaks the X protocol in pure Rust, so there
# is no libxcb to link and no X11 -dev header to add (the x11rb pin in Cargo.toml).
# hadolint ignore=DL3008
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    sed -i "s|//deb.debian.org/|//${DEBIAN_MIRROR}/|g" /etc/apt/sources.list.d/debian.sources \
 && apt-get update \
 && apt-get install -y --no-install-recommends protobuf-compiler

WORKDIR /src

# The whole workspace, then one release build of one crate. The registry cache mount keeps the
# crate downloads out of a rebuild, and a release builds this graph once per tag, so the extra
# stub-deps layer a per-edit cache would buy is not worth the moving parts here (AGENTS.md,
# "Rust builds"). `--locked` is the repository's rule everywhere: a Cargo.lock that disagrees with
# the manifests fails the release instead of resolving something new.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates

ENV CARGO_TARGET_DIR=/build \
    PROTOC=/usr/bin/protoc

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked --package appricot-streamer \
 && install -D -m 0755 /build/release/appricot-streamer /out/appricot-streamer

# --- runtime ----------------------------------------------------------------------------------
# The same distribution the binary was built on, and nothing else. Its shared libraries are the
# base's own libc and friends; the release workflow runs `ldd` inside this image and fails if any
# line says "not found".
FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251

LABEL org.opencontainers.image.title="APPricot streamer" \
      org.opencontainers.image.description="The per-window application streamer: one session, served over a loopback or unix WebSocket beside the application in its own container." \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"

# The licences travel with the artefact: Apache-2.0 §4 owes a recipient a copy of the licence, and
# a distributed image is a distribution. The dev image says the opposite about itself — it is never
# pushed and never conveyed, so its tools' licences are not part of the gated graph. `--chmod`
# because a checkout on Windows gives a text file the executable bit through the build context.
COPY --chmod=0644 LICENSE-MIT LICENSE-APACHE /usr/share/doc/appricot/

COPY --from=build /out/appricot-streamer /usr/local/bin/appricot-streamer

# No CMD, on purpose. `appricot-streamer` with no argument prints its version line and exits 0
# (crates/appricot-streamer/src/main.rs), so a bare `docker run <image>` is a version probe — the
# release's own smoke test, and the reason a tag whose version and manifests disagree cannot be
# published. A session image names what it wants: `docker run <image> serve`, or `CMD ["serve"]` in
# the image that builds `FROM` this one.
#
# `serve` then reads its configuration from the environment — `APPRICOT_BIND` (`loopback:<port>`
# or `unix:<path>`, nothing else), the required `APPRICOT_STREAM_TOKEN`, `APPRICOT_DISPLAY`, and
# `APPRICOT_LOG` (crates/appricot-streamer/src/config.rs). Without the token it refuses to start.
#
# No USER is set. Debian's default is root, and which uid the streamer runs under — its own, or
# the application's — is an open M4 question (docs/architecture.md §8); the session image decides,
# with its own `USER` or `docker run --user`.
ENTRYPOINT ["appricot-streamer"]
