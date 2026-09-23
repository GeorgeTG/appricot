//! The unix-socket bind: the WebSocket session and `/readyz` over a unix domain socket.
//!
//! `APPRICOT_BIND=unix:<path>` is the bind a container-side host prefers (no TCP stack in the
//! way, filesystem permissions as the access control). This file drives the real `serve` entry
//! point — not a hand-routed test server — over a unique socket in the temp directory, with a
//! `tokio-tungstenite` client on a `tokio` `UnixStream` and a bare HTTP/1.1 GET for readiness.
//! It also pins what the bind does to whatever already sits at the path, the socket's mode, and
//! that `serve` returns once the session is over.

#![cfg(unix)]

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use appricot_core::Size;
use appricot_proto::wire::{Body, ByeReason};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::client_async;

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::config::Bind;
use appricot_streamer::server::{self, Bound, ServerState};
use appricot_streamer::session::EndCause;
use common::harness::{TOKEN, expect_bye, hello, read_body, send};

/// Numbers the sockets this binary binds, so parallel tests never share a path.
static NEXT_SOCKET: AtomicU32 = AtomicU32::new(0);

/// A unique socket path in the temp directory; removed when dropped.
struct SocketPath(PathBuf);

impl SocketPath {
    fn new() -> Self {
        let n = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "appricot-streamer-test-{}-{n}.sock",
            std::process::id()
        )))
    }
}

impl Drop for SocketPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A running `serve`: its state, the mock's handle, and the task that returns when it stops.
struct Serving {
    state: Arc<ServerState<common::MockBackend>>,
    mock: common::MockHandle,
    task: JoinHandle<std::io::Result<Bound>>,
}

/// Runs the real `serve` entry point on `path`. Readiness is left for the test to latch.
fn serve_on(path: &Path) -> Serving {
    let (backend, mock) = common::MockBackend::pair();
    let state = ServerState::new(TOKEN.to_vec(), BackendHandle::spawn(backend));
    let bind = Bind::Unix {
        path: path.to_owned(),
    };
    let serving = Arc::clone(&state);
    let task = tokio::spawn(async move { server::serve(serving, &bind).await });
    Serving { state, mock, task }
}

/// Connects a unix stream, retrying while the server has not bound the socket yet.
async fn connect_unix(path: &Path) -> UnixStream {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(path).await {
            Ok(stream) => return stream,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("cannot connect to the unix socket: {e}"),
        }
    }
}

/// A bare HTTP/1.1 GET over the unix socket, returning the status code.
async fn http_get(path: &Path, target: &str) -> u16 {
    let mut stream = connect_unix(path).await;
    let request = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("GET sends");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("response reads");
    let text = String::from_utf8_lossy(&response);
    text.split_whitespace()
        .nth(1)
        .expect("a status code")
        .parse()
        .expect("the status code is a number")
}

/// Upgrades a `tokio-tungstenite` client over the unix socket to `/session`.
async fn connect_session(path: &Path) -> WebSocketStream<UnixStream> {
    let stream = connect_unix(path).await;
    // The URI names the path (the router never sees the host or the transport); what carries
    // the frames is the unix socket underneath.
    match client_async("ws://localhost/session", stream).await {
        Ok((ws, response)) => {
            assert_eq!(response.status(), 101, "the upgrade succeeds");
            ws
        }
        Err(e) => panic!("cannot upgrade to a WebSocket over the unix socket: {e}"),
    }
}

/// Upgrades and completes the handshake.
async fn session_started(path: &Path) -> WebSocketStream<UnixStream> {
    let mut ws = connect_session(path).await;
    send(&mut ws, &hello(None)).await;
    let Body::HelloReply(reply) = read_body(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    assert!(!reply.resumed);
    ws
}

#[tokio::test]
async fn the_unix_socket_serves_readiness_and_a_session() {
    let path = SocketPath::new();
    let serving = serve_on(&path.0);

    // Readiness over the unix socket: red before the latch, green after it.
    assert_eq!(http_get(&path.0, "/readyz").await, 503);
    serving.state.set_ready();
    assert_eq!(http_get(&path.0, "/readyz").await, 200);

    // The WebSocket handshake over the same socket: a real session, not a handshake echo.
    let mut ws = session_started(&path.0).await;

    // And the session streams: a surface is announced and framed over the unix socket.
    serving.mock.create_surface(3, Size::new(64, 48));
    match read_body(&mut ws).await {
        Body::SurfaceNew(m) => assert_eq!(m.surface_id, 3),
        other => panic!("expected SurfaceNew(3), got {other:?}"),
    }
    match read_body(&mut ws).await {
        Body::Frame(m) => {
            assert_eq!(m.surface_id, 3);
            assert!(m.full_redraw);
        }
        other => panic!("expected a Frame of 3, got {other:?}"),
    }
}

#[tokio::test]
async fn the_socket_is_the_owners_alone() {
    let path = SocketPath::new();
    let serving = serve_on(&path.0);
    serving.state.set_ready();
    assert_eq!(http_get(&path.0, "/readyz").await, 200);

    let mode = std::fs::metadata(&path.0)
        .expect("the socket exists")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "filesystem permissions are the access control"
    );
}

#[tokio::test]
async fn a_stale_socket_file_is_replaced() {
    let path = SocketPath::new();
    // A socket file nobody answers on: what a crashed process leaves behind.
    drop(std::os::unix::net::UnixListener::bind(&path.0).expect("a socket binds"));
    assert!(path.0.exists(), "the stale file stays behind");

    let serving = serve_on(&path.0);
    serving.state.set_ready();
    assert_eq!(
        http_get(&path.0, "/readyz").await,
        200,
        "the new server took the path"
    );
}

#[tokio::test]
async fn a_live_socket_is_not_taken_over() {
    let path = SocketPath::new();
    // Another server still owns the path and answers on it.
    let _other = std::os::unix::net::UnixListener::bind(&path.0).expect("a socket binds");

    let serving = serve_on(&path.0);
    let refused = tokio::time::timeout(Duration::from_secs(5), serving.task)
        .await
        .expect("serve gives up at once")
        .expect("the serve task does not panic")
        .expect_err("a live socket is refused, not replaced");
    assert_eq!(refused.kind(), ErrorKind::AddrInUse);
    assert!(
        std::os::unix::net::UnixStream::connect(&path.0).is_ok(),
        "the other server still answers on its path"
    );
}

#[tokio::test]
async fn a_file_that_is_not_a_socket_is_never_removed() {
    let path = SocketPath::new();
    std::fs::write(&path.0, b"precious").expect("a regular file is written");

    let serving = serve_on(&path.0);
    let refused = tokio::time::timeout(Duration::from_secs(5), serving.task)
        .await
        .expect("serve gives up at once")
        .expect("the serve task does not panic")
        .expect_err("a regular file is refused, not replaced");
    assert_eq!(refused.kind(), ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(&path.0).expect("the file is still there"),
        b"precious"
    );
}

#[tokio::test]
async fn a_stop_says_bye_and_serve_returns() {
    let path = SocketPath::new();
    let serving = serve_on(&path.0);
    serving.state.set_ready();
    let mut ws = session_started(&path.0).await;

    // The path a termination signal takes: the live session hears BYE_SERVER_SHUTDOWN, the
    // session is over, and serve returns so the process can exit.
    serving.state.shutdown();
    expect_bye(&mut ws, ByeReason::ByeServerShutdown).await;
    let bound = tokio::time::timeout(Duration::from_secs(5), serving.task)
        .await
        .expect("serve returns once the session is over")
        .expect("the serve task does not panic")
        .expect("serve ends cleanly");
    assert!(matches!(bound, Bound::Unix(p) if p == path.0));
    assert_eq!(serving.state.outcome(), Some(EndCause::Clean));
}
