//! The wire's outermost bound as the transport sees it: one WebSocket message over
//! `MAX_MESSAGE_BYTES`.
//!
//! Finding (2026-09-21, internal test over the real loopback server): a single-frame binary
//! message of `MAX_MESSAGE_BYTES + 1` bytes never reaches the streamer's bounded decoder. The
//! server's WebSocket stack caps a frame at 16 MiB — the same number the limits table caps a
//! whole message at — so a message big enough to break the wire cap is also big enough to break
//! the frame cap, and the transport kills the connection from the frame header alone, while the
//! sender is still writing. The streamer-level enforcement (`ServerError` code 1 +
//! `Bye(BYE_LIMIT_VIOLATION)`) still exists and is proved by the hand-built oversized-token
//! test in `tests/streamer.rs`: an envelope inside the transport cap whose field lengths break
//! the limits table gets the full refusal. What this file pins is which layer answers when the
//! envelope itself cannot even arrive.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use std::net::SocketAddr;
use std::time::Duration;

use appricot_proto::limits::MAX_MESSAGE_BYTES;
use appricot_proto::wire::{Body, Envelope, Hello, decode_envelope, encode_envelope};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::server::{self, ServerState};

const TOKEN: &[u8] = b"test-stream-token";

/// Binds the server on an ephemeral loopback port and serves it in the background.
async fn spawn_server() -> SocketAddr {
    let (backend, _mock) = common::MockBackend::pair();
    let backend = BackendHandle::spawn(backend);
    let state = ServerState::new(TOKEN.to_vec(), backend);
    state.set_ready();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("an ephemeral loopback port is free");
    let addr = listener
        .local_addr()
        .expect("the listener knows its address");
    let app = server::router::<common::MockBackend>().with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("the server serves");
    });
    addr
}

/// A connected client WebSocket.
type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connects and completes the handshake, returning the live session's socket.
async fn session_started(addr: &SocketAddr) -> Client {
    let url = format!("ws://{addr}/session");
    let mut ws = match connect_async(url).await {
        Ok((stream, _response)) => stream,
        Err(e) => panic!("cannot connect to /session: {e}"),
    };
    let hello = encode_envelope(&Envelope {
        body: Some(Body::Hello(Hello {
            protocol_version: 0,
            client_name: "limits-test".into(),
            stream_token: TOKEN.to_vec(),
            codecs: vec![],
            resume_serial: None,
        })),
    })
    .expect("the test builds valid messages");
    ws.send(Message::Binary(hello.into()))
        .await
        .expect("the client sends");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next = tokio::time::timeout(left, ws.next())
            .await
            .expect("an envelope arrives within the deadline")
            .expect("the socket is open");
        match next.expect("the socket carries a message") {
            Message::Binary(bytes) => {
                let body = decode_envelope(&bytes)
                    .expect("the server sends valid envelopes")
                    .body
                    .expect("the server never sends an empty envelope");
                assert!(matches!(body, Body::HelloReply(_)), "expected a HelloReply");
                return ws;
            }
            Message::Pong(_) => {}
            other => panic!("expected a binary envelope, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn an_oversized_single_frame_message_never_reaches_the_handler() {
    let addr = spawn_server().await;
    let mut ws = session_started(&addr).await;

    // One binary WebSocket message one byte over the wire cap, sent as the single frame a
    // normal client emits. The transport's own frame cap (16 MiB, the same number as
    // MAX_MESSAGE_BYTES) refuses it first: the server reads the frame header, sees the size,
    // and can kill the connection while the client is still writing — the send itself may
    // fail. The session handler never sees these bytes.
    let oversized = vec![0u8; MAX_MESSAGE_BYTES + 1];
    if ws.send(Message::Binary(oversized.into())).await.is_ok() {
        // The connection ends. What must NOT happen is a protocol envelope in either
        // direction first: no ServerError, no Bye — the enforcement happened below the
        // streamer.
        let deadline = Duration::from_secs(15);
        let ended = tokio::time::timeout(deadline, ws.next())
            .await
            .expect("the oversized message ends the connection");
        match ended {
            // However the ending arrives — an outright close, a transport error, or a bare
            // close frame — the enforcement happened below the streamer and no protocol
            // envelope preceded it.
            None | Some(Err(_) | Ok(Message::Close(None))) => {}
            // A close frame from the WebSocket layer; tungstenite answers a size violation
            // with close code 1009 (message too big) when the peer gets that far.
            Some(Ok(Message::Close(Some(close)))) => {
                assert_eq!(
                    close.code,
                    CloseCode::Size,
                    "the close names a size violation"
                );
            }
            Some(Ok(Message::Binary(bytes))) => panic!(
                "a {}-byte message reached the protocol layer; the transport cap did not fire",
                bytes.len()
            ),
            Some(Ok(other)) => panic!("expected the connection to end, got {other:?}"),
        }
    }
}
