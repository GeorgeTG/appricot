# syntax=docker/dockerfile:1
#
# APPricot dev image: the ONE place cargo, pnpm and node run. Local work and CI use it alike:
#
#   docker compose build dev
#   docker compose run --rm dev just check
#
# It holds the Rust toolchain (the base image's 1.94.1, which rust-toolchain.toml pins), node 22,
# pnpm via corepack, just, cargo-deny, and an Xvfb that the entrypoint starts on :99 for the X11
# integration test. The repo is bind-mounted at /work. target/, CARGO_HOME, the pnpm store and
# every node_modules are named volumes (compose.yml says why).
#
# Dev only. This image is never pushed and never conveyed, so the licences of the tools in it
# (just is CC0-1.0) are not part of the linked graph that docs/adr/0002-licence.md gates.
#
# What is pinned, and how:
#
# - The base image by tag AND digest. A tag can be rebuilt in place, and then two builds a week
#   apart would start from different layers, with nothing checked. The digest is the
#   multi-arch index of rust:1.94.1-slim-bookworm (`docker buildx imagetools inspect`, checked
#   2026-09-23). Bump the tag and the digest together with rust-toolchain.toml; `just pins`
#   fails when the two versions differ.
# - The node, just and cargo-deny downloads by version and SHA-256. A mismatch fails the build.
# - pnpm by version, fetched by corepack, which checks its integrity by default (the
#   `COREPACK_INTEGRITY_KEYS` entry of https://github.com/nodejs/corepack, checked 2026-09-23).
# - Debian packages are NOT pinned by version: apt checks Debian's signatures instead.
#
# linux/amd64 only: the node, just and cargo-deny downloads are x86_64 builds with one SHA-256
# each. compose.yml declares `platform: linux/amd64` for building and running, so an arm64 host
# builds and runs the amd64 image under emulation, where the host provides it, instead of
# failing at `node --version` with an error that never names the architecture. A plain
# `docker build` outside compose needs `--platform linux/amd64` for the same reason. (A constant
# `FROM --platform=` would do it here too, but BuildKit warns on it in every build:
# FromPlatformFlagConstDisallowed, measured 2026-09-23.)

FROM rust:1.94.1-slim-bookworm@sha256:cf9dd0ec73e75f827fe59123fff9dc65af1a1c8363c3c31ee8d7f8ad0b6a5fb2

# The uid/gid the container runs as. 1000 matches Docker Desktop's bind-mount view on Windows.
# CI passes its runner's uid (compose.yml: APPRICOT_UID/APPRICOT_GID) so the checkout stays
# readable and writable.
ARG APPRICOT_UID=1000
ARG APPRICOT_GID=1000

# --- apt -----------------------------------------------------------------------------------
# The Debian mirror. The default is Debian's own CDN alias. It measured 56 kB/s from this host
# on 2026-09-19, which is 20 minutes for this layer, while cdn-aws.deb.debian.org — one of the
# backends that same alias resolves to — measured 351 kB/s. So the mirror is a build argument:
#
#   APPRICOT_DEBIAN_MIRROR=cdn-aws.deb.debian.org docker compose build dev
#
# It is safe to point elsewhere: apt verifies the Release signature against Debian's keyring
# whatever the mirror serves, so a mirror can be slow or stale, but not forge a package.
ARG DEBIAN_MIRROR=deb.debian.org
#
# xvfb, xauth: the headless X server for crates/appricot-x11/tests/extensions.rs.
# x11-utils: xdpyinfo (the entrypoint's readiness probe) and xwininfo/xprop for debugging.
# xdotool, x11-apps: drive and inspect windows by hand (xdotool type/click; xeyes, xclock, xwd).
# fonts-dejavu-core: one font set, so X clients render text at all.
# xz-utils: the node tarball is .tar.xz.
# protobuf-compiler: protoc, for prost-build (crates/appricot-proto). A build tool like just,
# never part of the linked graph ADR-0002 gates.
#
# No X11 -dev headers and no libxcb: x11rb 0.14.0 has no default features, and libxcb comes
# only with `allow-unsafe-code` or `dl-libxcb`, which this workspace does not enable
# (https://crates.io/api/v1/crates/x11rb/0.14.0, checked 2026-09-19). Its RustConnection
# speaks the X protocol in Rust. The test binary's `ldd` output confirms it (see the gates).
#
# git is here for exactly one reason: cargo-deny 0.20.2 clones the RustSec advisory database
# with the git CLI, and without it `just deny` fails with
#   failed to clone: failed to spawn git: No such file or directory (os error 2)
# (measured 2026-09-19). Cargo itself does not need it: the crates.io index is fetched over
# HTTPS (sparse), and deny.toml denies git sources outright. It drags in perl and man-db.
# Commits are made on the host, where the work tree is checked out.
#
# The two cache mounts keep the .deb files and the package indices in the BuildKit cache, so a
# later edit to this line re-downloads only what actually changed — and nothing at all when the
# only change is the mirror. They need docker-clean gone (it deletes the archives after each
# install) and Keep-Downloaded-Packages on. A cache mount is not part of the image, so the
# archives and lists are not shipped in a layer either.
# hadolint ignore=DL3008
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
 && echo 'Binary::apt::APT::Keep-Downloaded-Packages "true";' > /etc/apt/apt.conf.d/keep-cache \
 && sed -i "s|//deb.debian.org/|//${DEBIAN_MIRROR}/|g" /etc/apt/sources.list.d/debian.sources \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl xz-utils git \
      xvfb xauth x11-utils xdotool x11-apps \
      fonts-dejavu-core protobuf-compiler

# --- Rust components -------------------------------------------------------------------------
# rust-toolchain.toml asks for rustfmt and clippy, but rust:*-slim images are built with
# `--profile minimal` and ship neither. Without this step the components are not in the image,
# so the first `cargo fmt` or `cargo clippy` in every fresh container downloads them: a stall on
# a slow link, and a hard failure wherever the container has no network at all. Installing them
# once, here, makes the gates a function of the image and nothing else.
RUN rustup component add rustfmt clippy \
 && rustc --version && cargo fmt --version && cargo clippy --version

# --- node 22 ---------------------------------------------------------------------------------
# v22.23.2 (LTS "Jod", 2026-07-28), the newest 22.x in https://nodejs.org/dist/index.json
# (checked 2026-09-19). jsdom 30.1.0 needs >=22.22.2 (package.json `engines`). The SHA-256 is
# the linux-x64 line of https://nodejs.org/dist/v22.23.2/SHASUMS256.txt (checked 2026-09-19).
ARG NODE_VERSION=22.23.2
ARG NODE_SHA256=d60acfe00a2932254bb0ad20e01b0d74397a0875595de719654b214f4b03f307
RUN curl -fsSL -o /tmp/node.tar.xz \
      "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" \
 && echo "${NODE_SHA256}  /tmp/node.tar.xz" | sha256sum -c - \
 && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 --no-same-owner \
      --exclude='*/CHANGELOG.md' --exclude='*/README.md' \
 && rm /tmp/node.tar.xz \
 && node --version

# --- just ------------------------------------------------------------------------------------
# 1.58.0, the latest release (https://github.com/casey/just/releases/tag/1.58.0, published
# 2026-08-03, checked 2026-09-19). The SHA-256 is the musl line of that release's SHA256SUMS.
ARG JUST_VERSION=1.58.0
ARG JUST_SHA256=4a5cc2f53e6f0f8c59092a6cc38291eb729d46a7dd95d3ae582008881b84931d
RUN curl -fsSL -o /tmp/just.tar.gz \
      "https://github.com/casey/just/releases/download/${JUST_VERSION}/just-${JUST_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
 && echo "${JUST_SHA256}  /tmp/just.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/just.tar.gz -C /usr/local/bin --no-same-owner just \
 && rm /tmp/just.tar.gz \
 && just --version

# --- cargo-deny ------------------------------------------------------------------------------
# 0.20.2, the latest release (https://github.com/EmbarkStudios/cargo-deny/releases/tag/0.20.2,
# published 2026-07-09, checked 2026-09-19), "MIT OR Apache-2.0" on crates.io. A prebuilt
# static binary, so the image does not compile it. The SHA-256 equals the release's own
# cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz.sha256 asset.
ARG CARGO_DENY_VERSION=0.20.2
ARG CARGO_DENY_SHA256=9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f
RUN curl -fsSL -o /tmp/cargo-deny.tar.gz \
      "https://github.com/EmbarkStudios/cargo-deny/releases/download/${CARGO_DENY_VERSION}/cargo-deny-${CARGO_DENY_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
 && echo "${CARGO_DENY_SHA256}  /tmp/cargo-deny.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/cargo-deny.tar.gz -C /usr/local/bin --no-same-owner --strip-components=1 \
      "cargo-deny-${CARGO_DENY_VERSION}-x86_64-unknown-linux-musl/cargo-deny" \
 && rm /tmp/cargo-deny.tar.gz \
 && cargo-deny --version

# --- the non-root user -----------------------------------------------------------------------
# Every mount point that a named volume covers is created here and owned by the user. Docker
# seeds an EMPTY named volume from the image's directory at that path, ownership included, so
# the volumes come up writable for uid 1000 without a root step at run time.
#
# The group is created only when APPRICOT_GID is free. Debian already has 20 (dialout), 100
# (users) and others, and a plain `groupadd --gid 100` fails with "GID '100' already exists"
# and kills the build (measured 2026-09-19, exit 4). CI passes the runner's own gid, and a
# self-hosted runner or a Linux developer may well run as one of those. The user is always
# named `dev` (`USER dev` below depends on that), but its group may keep the name the base
# image gave it, so the mount points are owned by the NUMERIC gid.
RUN { getent group "${APPRICOT_GID}" >/dev/null || groupadd --gid "${APPRICOT_GID}" dev; } \
 && useradd --uid "${APPRICOT_UID}" --gid "${APPRICOT_GID}" --create-home --shell /bin/bash dev \
 && install -d -o "${APPRICOT_UID}" -g "${APPRICOT_GID}" \
      /work /work/node_modules /work/packages \
      /work/packages/client /work/packages/client/node_modules \
      /work/packages/react /work/packages/react/node_modules \
      /work/packages/demo /work/packages/demo/node_modules \
      /cargo /target /pnpm-store /opt/corepack \
 && install -d -m 1777 /tmp/.X11-unix

# --- pnpm via corepack -----------------------------------------------------------------------
# corepack ships with node 22 (0.34.6 in v22.23.2). `enable` writes the pnpm shims into
# /usr/local/bin, so it runs as root. The pnpm release itself is fetched into COREPACK_HOME at
# BUILD time, as the user, so no container ever downloads it. It must equal package.json's
# `packageManager` field; corepack refuses any other version for this repo.
ARG PNPM_VERSION=10.33.0
ENV COREPACK_HOME=/opt/corepack \
    COREPACK_ENABLE_DOWNLOAD_PROMPT=0 \
    COREPACK_ENABLE_AUTO_PIN=0
RUN corepack enable pnpm
USER dev
RUN corepack install --global "pnpm@${PNPM_VERSION}" \
 && pnpm --version

# --- runtime environment ---------------------------------------------------------------------
# CARGO_HOME moves off /usr/local/cargo, where the rustup proxies live, so the cargo_home
# volume holds only the registry and git caches and can never shadow a newer image's binaries.
# CARGO_TARGET_DIR keeps build output off the Windows bind mount. npm_config_store_dir is how
# pnpm reads `store-dir` from the environment.
ENV CARGO_HOME=/cargo \
    CARGO_TARGET_DIR=/target \
    CARGO_TERM_COLOR=always \
    CARGO_NET_RETRY=5 \
    RUST_BACKTRACE=1 \
    PROTOC=/usr/bin/protoc \
    npm_config_store_dir=/pnpm-store \
    PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    APPRICOT_XVFB_DISPLAY=:99 \
    APPRICOT_XVFB_SCREEN=1400x900x24

COPY --chmod=0755 entrypoint.sh /usr/local/bin/appricot-entrypoint

WORKDIR /work
ENTRYPOINT ["/usr/local/bin/appricot-entrypoint"]
CMD ["just", "--list", "--unsorted"]
