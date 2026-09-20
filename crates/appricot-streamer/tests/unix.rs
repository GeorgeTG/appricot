//! The unix-socket bind: the WebSocket session and `/readyz` over a unix domain socket.
//!
//! `APPRICOT_BIND=unix:<path>` is the bind a container-side host prefers (no TCP stack in the
//! way, filesystem permissions as the access control). This file drives the real `serve` entry
//! point — not a hand-routed test server — over a unique socket in the temp directory, with a
//! `tokio-tungstenite` client on a `tokio` `UnixStream` and a bare HTTP/1.1 GET for readiness.

#![cfg(unix)]

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use appricot_core::Size;
use appricot_proto::wire::{Body, Envelope, Hello, decode_envelope, encode_envelope};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::Message;

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::config::Bind;
use appricot_streamer::server::{self, ServerState};

const TOKEN: &[u8] = b"test-stream-token";

/// Numbers the sockets this binary binds, so parallel tests never share a path.
static NEXT_SOCKET: AtomicU32 = AtomicU32::new(0);

/// A unique socket path in the temp directory.
fn socket_path() -> PathBuf {
    let n = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "appricot-streamer-test-{}-{n}.sock",
        std::process::id()
    ))
}

/// Serves the real `serve` entry point on `path`, returning the state (readiness is latched
/// through it) and the mock's handle.
fn spawn_server(path: &Path) -> (Arc<ServerState<common::MockBackend>>, common::MockHandle) {
    let (backend, mock) = common::MockBackend::pair();
    let backend = BackendHandle::spawn(backend);
    let state = ServerState::new(TOKEN.to_vec(), backend);
    let bind = Bind::Unix {
        path: path.to_owned(),
    };
    let serving = Arc::clone(&state);
    tokio::spawn(async move {
        if let Err(e) = server::serve(serving, &bind).await {
            panic!("cannot serve on the unix socket: {e}");
        }
    });
    (state, mock)
}

/// Connects a unix stream, retrying while the server has not bound the socket yet.
async fn connect_unix(path: &Path) -> UnixStream {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(path).await {
            Ok(stream) => return stream,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
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

/// Sends one envelope.
async fn send(ws: &mut WebSocketStream<UnixStream>, bytes: &[u8]) {
    ws.send(Message::Binary(bytes.to_vec().into()))
        .await
        .expect("the client sends");
}

/// Reads the next envelope body, skipping pings, failing on anything else.
async fn read_body(ws: &mut WebSocketStream<UnixStream>) -> Body {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next = tokio::time::timeout(left, ws.next())
            .await
            .expect("an envelope arrives within the deadline")
            .expect("the socket is open");
        let message = next.expect("the socket carries a message");
        match message {
            Message::Binary(bytes) => {
                return decode_envelope(&bytes)
                    .expect("the server sends valid envelopes")
                    .body
                    .expect("the server never sends an empty envelope");
            }
            Message::Pong(_) => {}
            other => panic!("expected a binary envelope, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn the_unix_socket_serves_readiness_and_a_session() {
    let path = socket_path();
    let (state, mock) = spawn_server(&path);

    // Readiness over the unix socket: red before the latch, green after it.
    assert_eq!(http_get(&path, "/readyz").await, 503);
    state.set_ready();
    assert_eq!(http_get(&path, "/readyz").await, 200);

    // The WebSocket handshake over the same socket: a real session, not a handshake echo.
    let mut ws = connect_session(&path).await;
    send(
        &mut ws,
        &encode_envelope(&Envelope {
            body: Some(Body::Hello(Hello {
                protocol_version: 0,
                client_name: "unix-test".into(),
                stream_token: TOKEN.to_vec(),
                codecs: vec![],
                resume_serial: None,
            })),
        })
        .expect("the test builds valid messages"),
    )
    .await;
    let Body::HelloReply(reply) = read_body(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    assert!(!reply.resumed);

    // And the session streams: a surface is announced and framed over the unix socket.
    mock.create_surface(3, Size::new(64, 48));
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

    let _ = std::fs::remove_file(&path);
}
