//! appricot-streamer: the per-window capture and input server. It runs inside an app's
//! container, next to a headless display server.
//!
//! What it will do, once l1-wire-spec-v0 and the M1 spike are done:
//!
//! - serve protocol v0 over a WebSocket bound to loopback or a unix socket, never to a public
//!   address;
//! - authenticate the first message with a per-session token handed in by L2 or the host, and
//!   refuse a stream without it;
//! - expose a readiness endpoint that stays red until the display server has every extension
//!   the backend needs.
//!
//! Today it prints its version and exits 0. That proves the binary builds and runs.

#[expect(
    clippy::print_stdout,
    reason = "the version line is this program's output"
)]
fn main() {
    println!(
        "appricot-streamer {} (wire protocol v{})",
        env!("CARGO_PKG_VERSION"),
        appricot_proto::PROTOCOL_VERSION
    );
}
