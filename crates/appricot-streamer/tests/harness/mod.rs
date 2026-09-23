//! A loopback server and a wire client, for the tests that watch a whole session's order of
//! messages: which fact reaches the client before which frame.
//!
//! The server is the real one, bound on `127.0.0.1:0`, around any backend the test brings;
//! the client speaks the real codec over a real WebSocket.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use appricot_core::{CaptureBackend, InputSink};
use appricot_proto::wire::{Body, Envelope, FrameAck, Hello, HelloReply};
use appricot_proto::wire::{decode_envelope, encode_envelope};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::server::{self, ServerState};

/// The stream token every test server expects.
const TOKEN: &[u8] = b"test-stream-token";

/// A connected client WebSocket.
pub type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Serves `backend` on an ephemeral loopback port, ready, and returns the address.
pub async fn serve<B>(backend: B) -> SocketAddr
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let state = ServerState::new(TOKEN.to_vec(), BackendHandle::spawn(backend));
    state.set_ready();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("an ephemeral loopback port is free");
    let addr = listener
        .local_addr()
        .expect("the listener knows its address");
    let app = server::router::<B>().with_state(Arc::clone(&state));
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("the server serves");
    });
    addr
}

/// Connects and says `Hello`, naming `resume` when the test resumes; returns the socket and
/// the `HelloReply`. Retries the upgrade while an earlier socket still holds the session.
pub async fn start(addr: SocketAddr, resume: Option<u32>) -> (Client, HelloReply) {
    let url = format!("ws://{addr}/session");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut ws = loop {
        match connect_async(url.clone()).await {
            Ok((stream, _response)) => break stream,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("cannot connect to /session, even with retries: {e}"),
        }
    };
    send(
        &mut ws,
        Body::Hello(Hello {
            protocol_version: 0,
            client_name: "integration-test".into(),
            stream_token: TOKEN.to_vec(),
            codecs: vec![],
            resume_serial: resume,
        }),
    )
    .await;
    match read_body(&mut ws).await {
        Body::HelloReply(reply) => (ws, reply),
        other => panic!("expected a HelloReply, got {other:?}"),
    }
}

/// Sends one envelope.
pub async fn send(ws: &mut Client, body: Body) {
    let bytes =
        encode_envelope(&Envelope { body: Some(body) }).expect("the test builds valid messages");
    ws.send(Message::Binary(bytes.into()))
        .await
        .expect("the client sends");
}

/// Acks frame `sequence` of `surface`.
pub async fn ack(ws: &mut Client, surface: u32, sequence: u32) {
    send(
        ws,
        Body::FrameAck(FrameAck {
            surface_id: surface,
            sequence,
        }),
    )
    .await;
}

/// Reads the next body, skipping pongs; panics after five seconds or on anything else.
pub async fn read_body(ws: &mut Client) -> Body {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next = tokio::time::timeout(left, ws.next())
            .await
            .expect("an envelope arrives within the deadline")
            .expect("the socket is open")
            .expect("the socket carries a message");
        match next {
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

/// Asserts that nothing arrives for `millis` milliseconds.
pub async fn expect_quiet(ws: &mut Client, millis: u64, why: &str) {
    let next = tokio::time::timeout(Duration::from_millis(millis), read_body(ws)).await;
    if let Ok(body) = next {
        panic!("{why}: got {body:?}");
    }
}
