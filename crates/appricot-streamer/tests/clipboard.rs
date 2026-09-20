//! Integration tests: the M2 clipboard row as the wire sees it (docs/roadmap.md M2: "Text
//! clipboard in both directions, only as the host's policy allows, and a clipboard write to
//! the user only inside a user gesture").
//!
//! The host-to-app direction is `ClipboardSet`: text capped by `MAX_CLIPBOARD_BYTES` must
//! reach [`appricot_core::InputSink::clipboard_set`] with the same UTF-8 bytes, and one byte
//! over the cap must end the session with `ServerError(1)` + `Bye(BYE_LIMIT_VIOLATION)`. The
//! app-to-host direction is `ClipboardAsk`: a backend that reports a paste it cannot serve
//! must surface it to the client as a `ClipboardAsk` envelope, and nothing else — the answer
//! stays the host's decision.
//!
//! Every test spawns the in-process server on an ephemeral loopback port with the mock
//! backend from `common`, exactly as `tests/streamer.rs` does.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use appricot_core::SurfaceEvent;
use appricot_proto::limits::MAX_CLIPBOARD_BYTES;
use appricot_proto::wire::{Body, ByeReason, Envelope, Hello, decode_envelope, encode_envelope};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::server::{self, ServerState};
use common::{Input, MockBackend};

const TOKEN: &[u8] = b"test-stream-token";

/// Binds the server on an ephemeral loopback port and serves it in the background.
async fn spawn_server() -> (
    SocketAddr,
    Arc<ServerState<MockBackend>>,
    common::MockHandle,
) {
    let (backend, mock) = MockBackend::pair();
    let backend = BackendHandle::spawn(backend);
    let state = ServerState::new(TOKEN.to_vec(), backend);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("an ephemeral loopback port is free");
    let addr = listener
        .local_addr()
        .expect("the listener knows its address");
    let app = server::router::<MockBackend>().with_state(Arc::clone(&state));
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("the server serves");
    });

    (addr, state, mock)
}

/// A connected client WebSocket.
type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The bytes of one client envelope.
fn envelope(body: Body) -> Vec<u8> {
    encode_envelope(&Envelope { body: Some(body) }).expect("the test builds valid messages")
}

/// Sends one envelope.
async fn send(ws: &mut Client, bytes: &[u8]) {
    ws.send(Message::Binary(bytes.to_vec().into()))
        .await
        .expect("the client sends");
}

/// Reads the next envelope, skipping pings, failing on anything else.
async fn read_envelope(ws: &mut Client) -> Envelope {
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
                return decode_envelope(&bytes).expect("the server sends valid envelopes");
            }
            Message::Pong(_) => {}
            other => panic!("expected a binary envelope, got {other:?}"),
        }
    }
}

/// Reads the next envelope and unwraps its body variant, panicking with the actual body.
async fn read_body(ws: &mut Client) -> Body {
    let envelope = read_envelope(ws).await;
    envelope
        .body
        .expect("the server never sends an empty envelope")
}

/// Drives a session through the handshake and returns once the HelloReply is read.
async fn session_started(addr: &SocketAddr, state: &Arc<ServerState<MockBackend>>) -> Client {
    state.set_ready();
    let url = format!("ws://{addr}/session");
    let mut ws = match connect_async(url).await {
        Ok((stream, _response)) => stream,
        Err(e) => panic!("cannot connect to /session: {e}"),
    };
    send(
        &mut ws,
        &envelope(Body::Hello(Hello {
            protocol_version: 0,
            client_name: "clipboard-test".into(),
            stream_token: TOKEN.to_vec(),
            codecs: vec![],
            resume_serial: None,
        })),
    )
    .await;
    match read_body(&mut ws).await {
        Body::HelloReply(_) => ws,
        other => panic!("expected a HelloReply, got {other:?}"),
    }
}

/// Reads the ServerError and Bye the server sends for a client fault, and asserts the codes.
async fn expect_error_and_bye(ws: &mut Client, code: u32, reason: ByeReason) {
    match read_body(ws).await {
        Body::ServerError(e) => assert_eq!(e.code, code, "the ServerError code"),
        other => panic!("expected a ServerError, got {other:?}"),
    }
    match read_body(ws).await {
        Body::Bye(b) => assert_eq!(b.reason, reason as i32, "the Bye reason"),
        other => panic!("expected a Bye, got {other:?}"),
    }
    // After a Bye the socket closes; nothing follows.
    let closed = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("the socket closes after a Bye");
    assert!(matches!(
        closed,
        None | Some(Ok(Message::Close(_)) | Err(_))
    ));
}

// -------------------------------------------------------------------------------------------
// Host to app: ClipboardSet
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn clipboard_set_text_reaches_the_backend_with_the_same_utf8_bytes() {
    let (addr, state, mock) = spawn_server().await;
    let mut ws = session_started(&addr, &state).await;

    // Multi-byte text first: what arrives must be the same UTF-8, not a mangled copy.
    let greek = "Γειά σου, πρόχειρο!".to_owned();
    send(
        &mut ws,
        &envelope(Body::ClipboardSet(appricot_proto::wire::ClipboardSet {
            text: greek.clone(),
        })),
    )
    .await;

    // Then exactly at the cap: MAX_CLIPBOARD_BYTES bytes of two-byte characters. The cap is
    // inclusive, and a boundary-sized paste is legal traffic.
    let at_cap = "α".repeat(MAX_CLIPBOARD_BYTES / 2);
    assert_eq!(
        at_cap.len(),
        MAX_CLIPBOARD_BYTES,
        "the fixture must be exactly at the cap in UTF-8 bytes"
    );
    send(
        &mut ws,
        &envelope(Body::ClipboardSet(appricot_proto::wire::ClipboardSet {
            text: at_cap.clone(),
        })),
    )
    .await;

    assert_eq!(
        mock.wait_input(2).await,
        vec![
            Input::Clipboard { text: greek },
            Input::Clipboard { text: at_cap },
        ],
        "the backend receives the same text, byte for byte, in order"
    );
}

#[tokio::test]
async fn a_clipboard_set_one_byte_over_the_cap_closes_with_server_error_and_bye() {
    let (addr, state, _mock) = spawn_server().await;
    let mut ws = session_started(&addr, &state).await;

    // Envelope.clipboard_set is field 22, wire type 2; ClipboardSet.text is field 1, wire
    // type 2. Hand-built so the encoder's own gate cannot refuse it first: this is what a
    // hostile peer sends, one byte over MAX_CLIPBOARD_BYTES.
    send(&mut ws, &crafted_oversized_clipboard_set()).await;
    expect_error_and_bye(&mut ws, 1, ByeReason::ByeLimitViolation).await;
}

/// Protobuf bytes of `Envelope.clipboard_set` = `ClipboardSet.text` of
/// `MAX_CLIPBOARD_BYTES + 1` bytes, encoded by hand (field 22 of Envelope, wire type 2;
/// field 1 of ClipboardSet, wire type 2).
fn crafted_oversized_clipboard_set() -> Vec<u8> {
    fn varint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if v == 0 {
                return out;
            }
        }
    }

    let text_len = (MAX_CLIPBOARD_BYTES + 1) as u64;
    let mut clipboard_set = vec![0x0a]; // ClipboardSet.text: field 1, wire type 2
    clipboard_set.extend(varint(text_len));
    clipboard_set.extend(vec![
        b'A';
        usize::try_from(text_len)
            .expect("fits the cap plus one")
    ]);

    let mut envelope = vec![0xb2, 0x01]; // Envelope.clipboard_set: field 22, wire type 2
    envelope.extend(varint(clipboard_set.len() as u64));
    envelope.extend(clipboard_set);
    envelope
}

// -------------------------------------------------------------------------------------------
// App to host: ClipboardAsk
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_backend_paste_request_surfaces_as_clipboard_ask_on_the_wire() {
    let (addr, state, mock) = spawn_server().await;
    let mut ws = session_started(&addr, &state).await;

    // The app pasted and the backend holds no text: the host must be asked.
    mock.push(SurfaceEvent::ClipboardRequested);

    match read_body(&mut ws).await {
        Body::ClipboardAsk(_) => {} // the whole message: the host decides what, if anything
        other => panic!("expected a ClipboardAsk, got {other:?}"),
    }
}
