# Task recipes. They run INSIDE the dev container, never on the host:
#
#   docker compose run --rm dev just check
#
# Cargo.lock and pnpm-lock.yaml are committed, so a fresh clone needs no resolution step. Only
# `just lock` and `just web-lock` rewrite them, and only when a dependency change is meant.
# Every other recipe builds `--locked` / `--frozen-lockfile`, so a stale lockfile fails loudly
# instead of being rewritten behind your back.
#
# The X11 integration test (crates/appricot-x11/tests/extensions.rs) needs an X server with
# Composite, Damage, XTEST and XFixes on $DISPLAY. The dev container provides one. Without it
# the test panics; it never skips, and `just display-levers` proves that.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes.
default:
    @just --list --unsorted

# Every gate: names and paths, version pins, the Rust gates and licences, then the TypeScript
# gates.
check: names-check pins fmt-check clippy test display-levers doc deny web-check

# AGENTS.md, hard rule 5. The denylist comes from APPRICOT_NAMES_DENYLIST or
# .git/info/names-denylist and is never in the tree. A hit prints file:line, never the name.
# Fail on a private name or an absolute path into a home or another checkout.
names-check:
    sh scripts/check-names.sh

# Two versions are pinned twice: the Rust toolchain (rust-toolchain.toml and the dev image's
# base) and pnpm (package.json and the dev image). A drifted pair makes rustup or corepack
# download the other version into every throwaway container.
# Fail when a version pinned in two places has drifted.
pins:
    #!/usr/bin/env bash
    set -euo pipefail
    status=0
    same() {
        if [[ -z "$2" || "$2" != "$3" ]]; then
            echo "pins: $1 differ: '$2' vs '$3'" >&2
            status=1
        fi
    }
    same "Rust (rust-toolchain.toml channel, docker/dev/Dockerfile FROM rust:<version>)" \
        "$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)" \
        "$(sed -n 's/^FROM .*rust:\([0-9][0-9.]*\)-.*$/\1/p' docker/dev/Dockerfile)"
    same "pnpm (package.json packageManager, docker/dev/Dockerfile ARG PNPM_VERSION)" \
        "$(sed -n 's/^ *"packageManager": "pnpm@\([^"+]*\).*$/\1/p' package.json)" \
        "$(sed -n 's/^ARG PNPM_VERSION=\(.*\)$/\1/p' docker/dev/Dockerfile)"
    if [[ "${status}" == 0 ]]; then echo "pins: ok"; fi
    exit "${status}"

# --- Rust -------------------------------------------------------------------

# Format the Rust code in place.
fmt:
    cargo fmt --all

# Fail if any Rust file is not formatted.
fmt-check:
    cargo fmt --all -- --check

# Lint every crate and target, warnings as errors.
clippy:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Run every Rust test, the X11 integration test and doc tests included.
test:
    cargo test --workspace --all-features --locked

# A test that skips would look like a test that passes. This runs the repository's
# docker/dev/entrypoint.sh, not the image's copy, so an edit is checked before any rebuild.
# Prove both entrypoint levers hold and the X11 test fails by a panic without a display.
display-levers:
    #!/usr/bin/env bash
    set -euo pipefail
    entry=docker/dev/entrypoint.sh
    fail() { echo "display-levers: $*" >&2; exit 1; }
    env -u DISPLAY APPRICOT_XVFB=0 bash "${entry}" bash -c '[[ -z "${DISPLAY+set}" ]]' \
        || fail "APPRICOT_XVFB=0 did not leave DISPLAY unset"
    shown="$(env DISPLAY=:5 bash "${entry}" printenv DISPLAY 2>/dev/null)" || shown="(failed)"
    [[ "${shown}" == ":5" ]] || fail "DISPLAY=:5 reached the command as '${shown}'"
    log="$(mktemp)"
    trap 'rm -f "${log}"' EXIT
    for lever in APPRICOT_XVFB=0 DISPLAY=; do
        if env -u DISPLAY "${lever}" bash "${entry}" \
            cargo test --workspace --all-features --locked --test extensions >"${log}" 2>&1; then
            cat "${log}" >&2
            fail "with ${lever} the X11 test passed; without a display it must fail"
        fi
        if ! grep -q 'panicked at' "${log}"; then
            cat "${log}" >&2
            fail "with ${lever} the X11 test failed, but not by a panic"
        fi
    done
    echo "display-levers: ok (APPRICOT_XVFB=0 and DISPLAY= both fail the X11 test by a panic)"

# Build the docs with warnings as errors. Only rustdoc checks intra-doc links.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked

# The licence, advisory, ban and source gate (deny.toml).
deny:
    cargo deny check advisories bans licenses sources

# Resolve Cargo.lock from scratch: a deliberate update of everything.
lock:
    cargo generate-lockfile

# Needs network: rustup fetches that toolchain into this throwaway container, and
# `cargo +<version>` overrides rust-toolchain.toml. Not part of `check`; CI runs it as a job.
# Check the workspace with the declared MSRV (Cargo.toml's rust-version), not the build toolchain.
msrv:
    #!/usr/bin/env bash
    set -euo pipefail
    msrv="$(sed -n 's/^rust-version = "\(.*\)"$/\1/p' Cargo.toml)"
    [[ -n "${msrv}" ]] || { echo "msrv: no rust-version in Cargo.toml" >&2; exit 1; }
    # "1.91" means 1.91.0: the floor itself, not the newest 1.91.x.
    [[ "${msrv}" == *.*.* ]] || msrv="${msrv}.0"
    rustup toolchain install "${msrv}" --profile minimal --no-self-update
    cargo "+${msrv}" check --workspace --all-targets --all-features --locked

# Print the streamer's version line (it exits 0).
run-streamer:
    cargo run --locked -p appricot-streamer

# --- TypeScript ---------------------------------------------------------------

# Resolve dependencies and rewrite pnpm-lock.yaml. The only recipe that may change it.
web-lock:
    pnpm install

# Install node_modules exactly as pnpm-lock.yaml says.
web-install:
    pnpm install --frozen-lockfile

# Every package in the production graph must be Tier A, every development-only package must be
# on the development list (ADR-0002, Amendment 1), and every workspace package must declare
# MIT OR Apache-2.0. The gate's own tests run first.
# The npm licence gate (ADR-0002 §2).
web-licences:
    node --test scripts/web-licences.test.mjs
    node scripts/web-licences.mjs

# Type-check every package, test files included.
web-typecheck:
    pnpm typecheck

# Lint every package, including the untrusted-server rules.
web-lint:
    pnpm lint

# Run every package's tests once.
web-test:
    pnpm test

# Build every package into its dist/.
web-build:
    pnpm build

# web-build comes before typecheck and test: packages/react resolves @appricot/client through
# its exports map, which points at dist/, so the client must be built before any other package
# can typecheck or test against it. web-licences needs only node_modules.
# Every TypeScript gate.
web-check: web-install web-licences web-build web-typecheck web-lint web-test

# --- the demo host page (manual) -------------------------------------------------------------
# One container, ONE published loopback port. The demo's static server proxies the WebSocket
# to the streamer's loopback bind inside the container, so the streamer never listens on a
# non-loopback address. From the host:
#
#   docker compose run --rm -p "127.0.0.1:${APPRICOT_DEMO_HOST_PORT:-8390}:8390" dev just demo
#
# then open http://127.0.0.1:8390 and paste the token the recipe prints. The token is random,
# fresh for every run, unless APPRICOT_DEMO_TOKEN sets one. Ctrl-C stops both.
#
# The streamer is supervised: it is built first, so a compile error fails the recipe; the page is
# served only once its /readyz answers 200; and when either process exits, the other is stopped
# and the recipe exits with that status.
# Serve the demo host page, with the streamer behind it (manual; see above).
demo:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "${APPRICOT_DEMO_TOKEN:-}" ]; then
        token="${APPRICOT_DEMO_TOKEN}"
    else
        # 24 bytes from the kernel, as hex: a fresh token every run, never a fixed default.
        token="$(head -c 24 /dev/urandom | od -An -vtx1 | tr -d ' \n')"
    fi
    echo "demo token (paste it into the page): ${token}"
    pnpm build
    cargo build --locked -p appricot-streamer
    streamer="" server=""
    trap 'kill ${streamer} ${server} 2>/dev/null || true' EXIT
    trap 'exit 130' INT TERM
    APPRICOT_BIND=loopback:8391 APPRICOT_STREAM_TOKEN="${token}" \
        "${CARGO_TARGET_DIR:-target}/debug/appricot-streamer" serve &
    streamer=$!
    # Up to 30 s: the backend connects to the X server and probes its extensions first.
    for _ in $(seq 1 300); do
        if ! kill -0 "${streamer}" 2>/dev/null; then
            status=0
            wait "${streamer}" || status=$?
            echo "demo: the streamer exited during startup (status ${status})" >&2
            exit 1
        fi
        if curl -fsS -o /dev/null http://127.0.0.1:8391/readyz 2>/dev/null; then
            break
        fi
        sleep 0.1
    done
    curl -fsS -o /dev/null http://127.0.0.1:8391/readyz \
        || { echo "demo: the streamer was not ready within 30 s" >&2; exit 1; }
    node packages/demo/server.mjs &
    server=$!
    status=0
    wait -n "${streamer}" "${server}" || status=$?
    echo "demo: a process exited (status ${status}); stopping the other" >&2
    exit "${status}"
