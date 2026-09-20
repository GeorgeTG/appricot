//! Integration tests: the dialog parent on the wire (`SurfaceNew.parent_id`).
//!
//! A dialog is an ordinary toplevel whose announcement names another toplevel in
//! `parent_id`; a plain toplevel's names nothing. These tests pin the mapping end to end
//! over the real loopback WebSocket — the announcement, and the re-announcement a resume
//! makes — with the mock backend standing in for the display.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use appricot_core::Size;
use appricot_proto::wire::{Body, Envelope, Hello, decode_envelope, encode_envelope};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::server::{self, ServerState};
use common::MockBackend;

const TOKEN: &[u8] = b"test-stream-token";

/// A spawned server: its address, its state, and the mock backend's handle.
struct TestServer {
    addr: SocketAddr,
    state: Arc<ServerState<MockBackend>>,
    mock: common::MockHandle,
}

/// Binds the server on an ephemeral loopback port and serves it in the background.
async fn spawn_server() -> TestServer {
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

    TestServer { addr, state, mock }
}

/// A connected client WebSocket.
type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connects a WebSocket client to the server's `/session`.
async fn connect(server: &TestServer) -> Client {
    let url = format!("ws://{}/session", server.addr);
    match connect_async(url).await {
        Ok((stream, _response)) => stream,
        Err(e) => panic!("cannot connect to /session: {e}"),
    }
}

/// Connects, retrying while the server answers 503 (a previous session's socket is still
/// live). Gives up after five seconds.
async fn connect_with_retry(server: &TestServer) -> Client {
    let url = format!("ws://{}/session", server.addr);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match connect_async(url.clone()).await {
            Ok((stream, _response)) => return stream,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("cannot connect to /session, even with retries: {e}"),
        }
    }
}

/// The bytes of one client envelope.
fn envelope(body: Body) -> Vec<u8> {
    encode_envelope(&Envelope { body: Some(body) }).expect("the test builds valid messages")
}

/// A Hello with this token.
fn hello(resume_serial: Option<u32>) -> Vec<u8> {
    envelope(Body::Hello(Hello {
        protocol_version: 0,
        client_name: "integration-test".into(),
        stream_token: TOKEN.to_vec(),
        codecs: vec![],
        resume_serial,
    }))
}

/// Sends one envelope.
async fn send(ws: &mut Client, bytes: &[u8]) {
    ws.send(Message::Binary(bytes.to_vec().into()))
        .await
        .expect("the client sends");
}

/// Reads the next body, skipping pings and the frames that follow every announcement —
/// these tests are about announcements, not pixels — failing on anything else.
async fn read_body_skipping_frames(ws: &mut Client) -> Body {
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
                let envelope = decode_envelope(&bytes).expect("the server sends valid envelopes");
                let body = envelope
                    .body
                    .expect("the server never sends an empty envelope");
                if matches!(body, Body::Frame(_)) {
                    continue;
                }
                return body;
            }
            Message::Pong(_) => {}
            other => panic!("expected a binary envelope, got {other:?}"),
        }
    }
}

/// Reads the next SurfaceNew of `surface_id`, panicking with what arrived instead.
async fn expect_surface_new(ws: &mut Client, surface_id: u32) -> appricot_proto::wire::SurfaceNew {
    match read_body_skipping_frames(ws).await {
        Body::SurfaceNew(m) if m.surface_id == surface_id => m,
        Body::Bye(bye) => panic!(
            "got Bye({:?}, {:?}) while waiting for SurfaceNew",
            bye.reason, bye.text
        ),
        other => panic!("expected SurfaceNew of {surface_id}, got {other:?}"),
    }
}

#[tokio::test]
async fn a_dialog_announces_its_parent_id_and_a_plain_toplevel_none() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = connect(&server).await;
    send(&mut ws, &hello(None)).await;
    let Body::HelloReply(reply) = read_body_skipping_frames(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    assert!(!reply.resumed);

    // The plain toplevel first, so the dialog's parent is tracked when the dialog comes.
    server.mock.create_surface(1, Size::new(300, 200));
    let plain = expect_surface_new(&mut ws, 1).await;
    assert_eq!(plain.parent_id, None, "a plain toplevel names no parent");

    server.mock.create_dialog(2, 1, Size::new(200, 120));
    let dialog = expect_surface_new(&mut ws, 2).await;
    assert_eq!(
        dialog.role,
        appricot_proto::wire::Role::Toplevel as i32,
        "a dialog is an ordinary toplevel"
    );
    assert_eq!(dialog.parent_id, Some(1), "the dialog names its parent");
}

#[tokio::test]
async fn a_resume_re_announces_the_dialog_with_its_parent_id() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut first = connect(&server).await;
    send(&mut first, &hello(None)).await;
    let Body::HelloReply(reply) = read_body_skipping_frames(&mut first).await else {
        panic!("expected a HelloReply");
    };
    let resume_serial = reply.resume_serial.expect("a session can be resumed");

    server.mock.create_surface(1, Size::new(300, 200));
    expect_surface_new(&mut first, 1).await;
    server.mock.create_dialog(2, 1, Size::new(200, 120));
    expect_surface_new(&mut first, 2).await;

    // Drop the socket without a Bye: the session parks, and a reconnect naming the serial
    // resumes it.
    drop(first);

    let mut second = connect_with_retry(&server).await;
    send(&mut second, &hello(Some(resume_serial))).await;
    let Body::HelloReply(resumed) = read_body_skipping_frames(&mut second).await else {
        panic!("expected a HelloReply");
    };
    assert!(resumed.resumed, "the parked session resumes");

    // The whole window set is re-announced in creation order, parents included.
    let plain = expect_surface_new(&mut second, 1).await;
    assert_eq!(plain.parent_id, None);
    let dialog = expect_surface_new(&mut second, 2).await;
    assert_eq!(
        dialog.parent_id,
        Some(1),
        "the resume re-announces the dialog's parent"
    );
}
