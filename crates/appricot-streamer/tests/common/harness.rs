//! The server and client halves the streamer's integration tests share.
//!
//! The server half spawns the in-process axum server on `127.0.0.1:0` (never a fixed port)
//! around any backend; the client half is a real `tokio-tungstenite` client speaking the actual
//! wire codec. File-specific helpers stay in their test file; everything two files need lives
//! here, so a handshake change is made once.

// Each test binary uses its own subset of these helpers; a helper outside a binary's subset
// would read as dead code there.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use appricot_core::{CaptureBackend, InputSink};
use appricot_proto::wire::{
    Body, ByeReason, Envelope, Hello, HelloReply, decode_envelope, encode_envelope,
};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::server::{self, ServerState};
use appricot_streamer::session::EndCause;

use super::{MockBackend, MockHandle};

/// The stream token every test server is started with.
pub const TOKEN: &[u8] = b"test-stream-token";

/// How long any one wait in these helpers may take before the test fails.
pub const WAIT: Duration = Duration::from_secs(5);

/// A connected client WebSocket over loopback TCP.
pub type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A spawned server: its address and its state.
pub struct TestServer<B> {
    /// Where it listens.
    pub addr: SocketAddr,
    /// Its state, for readiness and the slot.
    pub state: Arc<ServerState<B>>,
}

/// Serves `backend` on an ephemeral loopback port in the background, readiness latched.
pub async fn spawn_server_around<B>(backend: B) -> TestServer<B>
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let state = ServerState::new(TOKEN.to_vec(), BackendHandle::spawn(backend));
    state.set_ready();
    let addr = serve_state(Arc::clone(&state)).await;
    TestServer { addr, state }
}

/// Serves `state` on an ephemeral loopback port in the background, as it is.
pub async fn serve_state<B>(state: Arc<ServerState<B>>) -> SocketAddr
where
    B: CaptureBackend + InputSink + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("an ephemeral loopback port is free");
    let addr = listener
        .local_addr()
        .expect("the listener knows its address");
    let app = server::router::<B>().with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("the server serves");
    });
    addr
}

/// Serves a fresh mock backend, and returns the test's handle to it.
pub async fn spawn_mock_server() -> (TestServer<MockBackend>, MockHandle) {
    let (backend, mock) = MockBackend::pair();
    (spawn_server_around(backend).await, mock)
}

/// Upgrades a WebSocket client to the server's `/session`.
pub async fn connect(addr: &SocketAddr) -> Client {
    match connect_async(format!("ws://{addr}/session")).await {
        Ok((stream, _response)) => stream,
        Err(e) => panic!("cannot connect to /session: {e}"),
    }
}

/// Connects, retrying while the server answers `503` (the previous socket's end has not
/// settled yet). Gives up after [`WAIT`].
pub async fn connect_with_retry(addr: &SocketAddr) -> Client {
    let url = format!("ws://{addr}/session");
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match connect_async(url.clone()).await {
            Ok((stream, _response)) => return stream,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("cannot connect to /session, even with retries: {e}"),
        }
    }
}

/// Asserts the upgrade to `/session` is refused over HTTP, and returns the status: `503` while
/// a session is live, `410` once the session is over. Any other failure — a refused TCP
/// connection, a crash — fails the test instead of passing for a refusal.
pub async fn upgrade_refused(addr: &SocketAddr) -> u16 {
    use tokio_tungstenite::tungstenite::Error;
    match connect_async(format!("ws://{addr}/session")).await {
        Ok(_) => panic!("the upgrade was expected to be refused"),
        Err(Error::Http(response)) => response.status().as_u16(),
        Err(e) => panic!("expected an HTTP refusal of the upgrade, got {e}"),
    }
}

/// The bytes of one envelope.
pub fn envelope(body: Body) -> Vec<u8> {
    encode_envelope(&Envelope { body: Some(body) }).expect("the test builds valid messages")
}

/// A Hello with the right token, offering `codecs`, naming `resume_serial` when it has one.
pub fn hello_offering(codecs: Vec<u32>, resume_serial: Option<u32>) -> Vec<u8> {
    envelope(Body::Hello(Hello {
        protocol_version: 0,
        client_name: "integration-test".into(),
        stream_token: TOKEN.to_vec(),
        codecs,
        resume_serial,
    }))
}

/// A Hello with the right token, offering RAW, naming `resume_serial` when it has one.
pub fn hello(resume_serial: Option<u32>) -> Vec<u8> {
    hello_offering(vec![], resume_serial)
}

/// A Hello with a token that is not the server's.
pub fn hello_with_wrong_token(resume_serial: Option<u32>) -> Vec<u8> {
    envelope(Body::Hello(Hello {
        protocol_version: 0,
        client_name: "integration-test".into(),
        stream_token: b"not-the-token".to_vec(),
        codecs: vec![],
        resume_serial,
    }))
}

/// Sends one envelope's bytes as one binary message.
pub async fn send<S>(ws: &mut WebSocketStream<S>, bytes: &[u8])
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    ws.send(Message::Binary(bytes.to_vec().into()))
        .await
        .expect("the client sends");
}

/// Reads the next envelope, skipping pongs, failing on anything else.
pub async fn read_envelope<S>(ws: &mut WebSocketStream<S>) -> Envelope
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let next = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("an envelope arrives within the deadline")
            .expect("the socket is open");
        match next.expect("the socket carries a message") {
            Message::Binary(bytes) => {
                return decode_envelope(&bytes).expect("the server sends valid envelopes");
            }
            Message::Pong(_) => {}
            other => panic!("expected a binary envelope, got {other:?}"),
        }
    }
}

/// Reads the next envelope's body.
pub async fn read_body<S>(ws: &mut WebSocketStream<S>) -> Body
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    read_envelope(ws)
        .await
        .body
        .expect("the server never sends an empty envelope")
}

/// Reads the next body that is not a `Frame` or a cursor message: for tests about
/// announcements, not pixels.
pub async fn read_body_skipping_frames<S>(ws: &mut WebSocketStream<S>) -> Body
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        match read_body(ws).await {
            Body::Frame(_) | Body::CursorImage(_) | Body::CursorGone(_) => {}
            body => return body,
        }
    }
}

/// Reads one body that `is` accepts; panics on anything else, naming the Bye that arrived
/// instead when that is what came.
///
/// Cursor messages are looked past: the cursor is session state an announcement re-sends
/// after the window set (v0.md §7), and these tests wait for something else.
pub async fn read_until<S>(
    ws: &mut WebSocketStream<S>,
    want: &str,
    is: impl Fn(&Body) -> bool,
) -> Body
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        match read_body(ws).await {
            body if is(&body) => return body,
            Body::CursorImage(_) | Body::CursorGone(_) => {}
            Body::Bye(bye) => panic!(
                "got Bye({:?}, {:?}) while waiting for {want}",
                bye.reason, bye.text
            ),
            other => panic!("expected {want}, got {other:?}"),
        }
    }
}

/// Reads the next `SurfaceNew` of `surface_id`.
pub async fn expect_surface_new<S>(
    ws: &mut WebSocketStream<S>,
    surface_id: u32,
) -> appricot_proto::wire::SurfaceNew
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match read_until(
        ws,
        "SurfaceNew",
        |b| matches!(b, Body::SurfaceNew(m) if m.surface_id == surface_id),
    )
    .await
    {
        Body::SurfaceNew(m) => m,
        _ => unreachable!("read_until checked the variant"),
    }
}

/// Reads the next `Frame` of `surface_id`.
pub async fn expect_frame<S>(
    ws: &mut WebSocketStream<S>,
    surface_id: u32,
) -> appricot_proto::wire::Frame
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match read_until(
        ws,
        "Frame",
        |b| matches!(b, Body::Frame(m) if m.surface_id == surface_id),
    )
    .await
    {
        Body::Frame(m) => m,
        _ => unreachable!("read_until checked the variant"),
    }
}

/// Acks the frame `sequence` of `surface_id`.
pub async fn ack<S>(ws: &mut WebSocketStream<S>, surface_id: u32, sequence: u32)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    send(
        ws,
        &envelope(Body::FrameAck(appricot_proto::wire::FrameAck {
            surface_id,
            sequence,
        })),
    )
    .await;
}

/// Reads the next body and asserts it is the `Bye` with `reason`, then that the socket closes.
pub async fn expect_bye<S>(ws: &mut WebSocketStream<S>, reason: ByeReason)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match read_body(ws).await {
        Body::Bye(b) => assert_eq!(b.reason, reason as i32, "the Bye reason ({:?})", b.text),
        other => panic!("expected Bye({reason:?}), got {other:?}"),
    }
    expect_closed(ws).await;
}

/// Reads the ServerError and the Bye the server sends for a fault, asserts the codes, then
/// that the socket closes.
pub async fn expect_error_and_bye<S>(ws: &mut WebSocketStream<S>, code: u32, reason: ByeReason)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match read_body(ws).await {
        Body::ServerError(e) => assert_eq!(e.code, code, "the ServerError code"),
        other => panic!("expected a ServerError, got {other:?}"),
    }
    expect_bye(ws, reason).await;
}

/// Asserts that nothing but the end of the connection follows: a close frame, an error or
/// the end of the stream, and never a protocol message.
pub async fn expect_closed<S>(ws: &mut WebSocketStream<S>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let next = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("the socket closes within the deadline");
        match next {
            None | Some(Err(_) | Ok(Message::Close(_))) => return,
            Some(Ok(Message::Pong(_) | Message::Ping(_))) => {}
            Some(Ok(other)) => panic!("expected the socket to close, got {other:?}"),
        }
    }
}

/// Upgrades, sends a Hello naming `resume_serial`, and returns the reply.
pub async fn handshake(addr: &SocketAddr, resume_serial: Option<u32>) -> (Client, HelloReply) {
    let mut ws = connect_with_retry(addr).await;
    send(&mut ws, &hello(resume_serial)).await;
    match read_body(&mut ws).await {
        Body::HelloReply(reply) => (ws, reply),
        other => panic!("expected a HelloReply, got {other:?}"),
    }
}

/// A bare HTTP/1.1 GET over loopback TCP, returning the status code.
pub async fn http_get(addr: &SocketAddr, path: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = TcpStream::connect(addr).await.expect("TCP connects");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("GET sends");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("response reads");
    let line = String::from_utf8_lossy(&response);
    let status = line.split_whitespace().nth(1).expect("a status code");
    status.parse().expect("the status code is a number")
}

/// Waits until `condition` holds, polling; panics with `what` after [`WAIT`].
pub async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting until {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Waits until the session is over, then asserts it ended for `cause`, that the next upgrade
/// meets `410`, and that readiness reads red. The wait matters: the client sees the close
/// before the server has settled the slot.
pub async fn expect_over<B>(server: &TestServer<B>, cause: EndCause) {
    wait_until("the session is over", || server.state.outcome().is_some()).await;
    assert_eq!(server.state.outcome(), Some(cause));
    assert_eq!(upgrade_refused(&server.addr).await, 410);
    assert_eq!(http_get(&server.addr, "/readyz").await, 503);
}
