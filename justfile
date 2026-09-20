# Task recipes. They run INSIDE the dev container, never on the host:
#
#   docker compose run --rm dev just check
#
# First run on a fresh clone, before the lockfiles exist: `just lock web-lock`.
# Every other recipe builds `--locked` / `--frozen-lockfile`, so a stale lockfile fails loudly
# instead of being rewritten behind your back.
#
# The X11 integration test (crates/appricot-x11/tests/extensions.rs) needs an X server with
# Composite, Damage, XTEST and XFixes on $DISPLAY. The dev container provides one. Without it
# the test panics; it never skips.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes.
default:
    @just --list --unsorted

# Every gate: Rust format, lints, tests, docs and licences, then the TypeScript gates.
check: fmt-check clippy test doc deny web-check

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

# Build the docs with warnings as errors. Only rustdoc checks intra-doc links.
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked

# The licence, advisory, ban and source gate (deny.toml).
deny:
    cargo deny check advisories bans licenses sources

# Resolve Cargo.lock from scratch: the first run, or a deliberate update of everything.
lock:
    cargo generate-lockfile

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

# Every TypeScript gate. web-build comes first: packages/react resolves @appricot/client
# through its exports map, which points at dist/, so the client must be built before any
# other package can typecheck or test against it.
web-check: web-install web-build web-typecheck web-lint web-test

# --- the demo host page (manual) -------------------------------------------------------------
# One container, ONE published loopback port. The demo's static server proxies the WebSocket
# to the streamer's loopback bind inside the container, so the streamer never listens on a
# non-loopback address. From the host:
#
#   docker compose run --rm -p "127.0.0.1:${APPRICOT_DEMO_HOST_PORT:-8390}:8390" dev just demo
#
# then open http://127.0.0.1:8390 and paste the token (printed below, or set
# APPRICOT_DEMO_TOKEN). Ctrl-C stops both.
demo:
    @echo "demo token: ${APPRICOT_DEMO_TOKEN:-demo-token}"
    pnpm build
    APPRICOT_BIND=loopback:8391 APPRICOT_STREAM_TOKEN="${APPRICOT_DEMO_TOKEN:-demo-token}" \
        cargo run --locked -p appricot-streamer -- serve &
    node packages/demo/server.mjs
