//! appricot-streamer: the per-window capture and input server. It runs inside an app's
//! container, next to a headless display server.
//!
//! # Usage
//!
//! ```text
//! appricot-streamer           # print the version line and exit 0
//! appricot-streamer serve     # serve protocol v0 (see the [`appricot_streamer`] library)
//! ```
//!
//! `serve` reads its configuration from the environment (see the [`appricot_streamer::config`]
//! module for every variable):
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `APPRICOT_BIND` | `loopback:<port>` or `unix:<path>`; nothing else is accepted | `loopback:0` |
//! | `APPRICOT_STREAM_TOKEN` | the per-session stream token; required | none |
//! | `APPRICOT_DISPLAY` | the X display to serve | `$DISPLAY` |
//! | `APPRICOT_LOG` | a `tracing` filter directive | `info` |
//!
//! The server binds `127.0.0.1` or a unix socket, never a public address, and serves one
//! session per process over a binary WebSocket at `GET /session`, with readiness at
//! `GET /readyz`. The first client message must carry the stream token; without it nothing is
//! served (rule 8, docs/protocol/README.md).

use std::env;
use std::process::ExitCode;

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::config::Config;
use appricot_streamer::server;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (None, None) => {
            print_version_line();
            ExitCode::SUCCESS
        }
        (Some("serve"), None) => serve(),
        _ => {
            print_usage();
            ExitCode::from(2)
        }
    }
}

/// The line `just run-streamer` gates on: the crate version and the wire protocol it speaks.
#[expect(
    clippy::print_stdout,
    reason = "the version line is this program's output"
)]
fn print_version_line() {
    println!(
        "appricot-streamer {} (wire protocol v{})",
        env!("CARGO_PKG_VERSION"),
        appricot_proto::PROTOCOL_VERSION
    );
}

#[expect(
    clippy::print_stderr,
    reason = "the CLI talks to the operator before logging exists"
)]
fn print_usage() {
    eprintln!("usage: appricot-streamer [serve]");
}

#[expect(
    clippy::print_stderr,
    reason = "the CLI talks to the operator before logging exists"
)]
fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("appricot-streamer: {message}");
    ExitCode::from(1)
}

/// Names a bad `APPRICOT_LOG` directive before logging exists to carry it.
#[expect(
    clippy::print_stderr,
    reason = "logging is not up yet; the operator gets the fallback notice on stderr"
)]
fn report_bad_filter(e: &tracing_subscriber::filter::ParseError) {
    eprintln!("appricot-streamer: APPRICOT_LOG is not a valid filter ({e}); using \"info\"");
}

/// Runs `serve`: configuration, logging, the display backend, then the server.
fn serve() -> ExitCode {
    let cfg = match appricot_streamer::config::from_env() {
        Ok(cfg) => cfg,
        Err(e) => return fail(e),
    };

    let filter = tracing_subscriber::EnvFilter::try_new(&cfg.log_filter).unwrap_or_else(|e| {
        report_bad_filter(&e);
        tracing_subscriber::EnvFilter::new("info")
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return fail(format!("cannot start the async runtime: {e}")),
    };
    runtime.block_on(run(cfg))
}

/// Serves until the process is stopped.
///
/// One backend for the process's lifetime: `X11Backend::connect` is the single X connection,
/// and its success is the readiness condition — a display without the required extensions
/// fails the connect (the probe the `appricot-x11` crate runs there is the same one
/// `crates/appricot-x11/tests/extensions.rs` asserts on a live server).
async fn run(cfg: Config) -> ExitCode {
    let backend = match appricot_x11::X11Backend::connect(cfg.display.as_deref()) {
        Ok(backend) => backend,
        Err(e) => return fail(format!("cannot open the display {:?}: {e}", cfg.display)),
    };
    let handle = BackendHandle::spawn(backend);

    let state = server::ServerState::new(cfg.token.clone(), handle);
    state.set_ready();

    match server::serve(state, &cfg.bind).await {
        Ok(bound) => {
            tracing::info!(?bound, "server ended");
            ExitCode::SUCCESS
        }
        Err(e) => fail(format!("cannot serve on {bind}: {e}", bind = cfg.bind)),
    }
}
