//! Integration tests: the streamer server over a real loopback WebSocket.
//!
//! Every test spawns the in-process axum server on `127.0.0.1:0` (never a fixed port) with a
//! set token and the mock backend from `common`, and drives it with a real
//! `tokio-tungstenite` client speaking the actual wire codec. What is asserted is the
//! behaviour a host sees: the handshake, the refusals, the frame path, the input mapping and
//! the resume.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use appricot_core::{
    KeyCode, KeyEvent, Keysym, PointerButton, PressState, Rect, Size, SurfaceEvent, SurfaceId,
};
use appricot_proto::limits::{MAX_FRAME_CREDITS, MAX_TILE_BYTES, RESUME_GRACE_MS, codec};
use appricot_proto::wire::{Body, ByeReason, Envelope, Hello, decode_envelope, encode_envelope};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::server::{self, ServerState};
use common::{Input, MockBackend};

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
fn hello(codecs: Vec<u32>, resume_serial: Option<u32>) -> Vec<u8> {
    envelope(Body::Hello(Hello {
        protocol_version: 0,
        client_name: "integration-test".into(),
        stream_token: TOKEN.to_vec(),
        codecs,
        resume_serial,
    }))
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

/// Reads one envelope of kind `want` (identified by `is`); panics on anything else, with the
/// Bye that arrived instead when that is what came.
///
/// `read_envelope` already bounds the wait, so there is no loop here: the first body either
/// matches, or the test has learned what it needs from the failure.
async fn read_until<'a>(ws: &mut Client, want: &str, is: impl Fn(&Body) -> bool + 'a) -> Body {
    match read_body(ws).await {
        body if is(&body) => body,
        Body::Bye(bye) => panic!(
            "got Bye({:?}, {:?}) while waiting for {want}",
            bye.reason, bye.text
        ),
        other => panic!("expected {want}, got {other:?}"),
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
// Handshake and refusals
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn hello_is_answered_with_version_codecs_and_resume_terms() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = connect(&server).await;

    send(&mut ws, &hello(vec![codec::QOI, codec::RAW], None)).await;
    let Body::HelloReply(reply) = read_body(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    assert_eq!(reply.protocol_version, 0);
    assert!(!reply.resumed);
    assert_eq!(
        reply.max_frame_credits,
        u32::try_from(MAX_FRAME_CREDITS).expect("fits the limits table")
    );
    // RAW and QOI were offered and both can be encoded; the order is the server's choice.
    assert!(reply.codecs.contains(&codec::RAW));
    assert!(reply.codecs.contains(&codec::QOI));
    assert!(reply.resume_serial.is_some());
    assert_eq!(reply.resume_grace_ms, Some(RESUME_GRACE_MS));
}

#[tokio::test]
async fn an_empty_codec_offer_means_raw() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = connect(&server).await;

    send(&mut ws, &hello(vec![], None)).await;
    let Body::HelloReply(reply) = read_body(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    assert_eq!(reply.codecs, vec![codec::RAW]);
}

#[tokio::test]
async fn a_wrong_token_closes_with_bye_auth_failed() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = connect(&server).await;

    let wrong = envelope(Body::Hello(Hello {
        protocol_version: 0,
        client_name: "integration-test".into(),
        stream_token: b"not-the-token".to_vec(),
        codecs: vec![],
        resume_serial: None,
    }));
    send(&mut ws, &wrong).await;

    match read_body(&mut ws).await {
        Body::Bye(b) => assert_eq!(b.reason, ByeReason::ByeAuthFailed as i32),
        other => panic!("expected Bye BYE_AUTH_FAILED, got {other:?}"),
    }
}

#[tokio::test]
async fn a_refused_handshake_does_not_consume_the_session() {
    let server = spawn_server().await;
    server.state.set_ready();

    // A mistyped token first: the server answers BYE_AUTH_FAILED and closes the socket.
    let mut wrong = connect(&server).await;
    let typo = envelope(Body::Hello(Hello {
        protocol_version: 0,
        client_name: "integration-test".into(),
        stream_token: b"not-the-token".to_vec(),
        codecs: vec![],
        resume_serial: None,
    }));
    send(&mut wrong, &typo).await;
    match read_body(&mut wrong).await {
        Body::Bye(b) => assert_eq!(b.reason, ByeReason::ByeAuthFailed as i32),
        other => panic!("expected Bye BYE_AUTH_FAILED, got {other:?}"),
    }
    drop(wrong);

    // The refusal claimed nothing — the session belongs to an authenticated Hello — so the
    // retry with the right token is served. Before this rule held the upgrade alone claimed
    // the session, and one typo left the process refusing every later upgrade with 503
    // (measured 2026-09-21).
    let mut retry = connect_with_retry(&server).await;
    send(&mut retry, &hello(vec![], None)).await;
    let Body::HelloReply(reply) = read_body(&mut retry).await else {
        panic!("expected a HelloReply, not a refusal");
    };
    assert!(!reply.resumed, "the retry is a fresh session, not a resume");
}

#[tokio::test]
async fn an_unsupported_version_closes_with_bye_protocol_version() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = connect(&server).await;

    let futuristic = envelope(Body::Hello(Hello {
        protocol_version: 99,
        client_name: "integration-test".into(),
        stream_token: TOKEN.to_vec(),
        codecs: vec![],
        resume_serial: None,
    }));
    send(&mut ws, &futuristic).await;

    match read_body(&mut ws).await {
        Body::Bye(b) => assert_eq!(b.reason, ByeReason::ByeProtocolVersion as i32),
        other => panic!("expected Bye BYE_PROTOCOL_VERSION, got {other:?}"),
    }
}

#[tokio::test]
async fn a_wrong_first_message_closes_with_bye_protocol_violation() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = connect(&server).await;

    // A FrameAck before any Hello: the wrong first message.
    send(
        &mut ws,
        &envelope(Body::FrameAck(appricot_proto::wire::FrameAck {
            surface_id: 1,
            sequence: 1,
        })),
    )
    .await;

    match read_body(&mut ws).await {
        Body::Bye(b) => assert_eq!(b.reason, ByeReason::ByeProtocolViolation as i32),
        other => panic!("expected Bye BYE_PROTOCOL_VIOLATION, got {other:?}"),
    }
}

#[tokio::test]
async fn a_text_frame_is_refused() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = connect(&server).await;

    ws.send(Message::Text("hello?".into()))
        .await
        .expect("text sends");
    match read_body(&mut ws).await {
        Body::Bye(b) => assert_eq!(b.reason, ByeReason::ByeProtocolViolation as i32),
        other => panic!("expected Bye BYE_PROTOCOL_VIOLATION, got {other:?}"),
    }
}

#[tokio::test]
async fn garbage_bytes_close_with_server_error_and_bye() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = connect(&server).await;

    send(&mut ws, &[0xff, 0xff, 0xff, 0x7f, 0x00, 0x01]).await;
    expect_error_and_bye(&mut ws, 4, ByeReason::ByeProtocolViolation).await;
}

#[tokio::test]
async fn a_limit_violation_closes_with_server_error_and_bye() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = connect(&server).await;

    // A Hello whose stream_token is 300 bytes, over MAX_TOKEN_BYTES (256). Hand-built so the
    // encoder's own gate cannot refuse it first: this is what a hostile peer sends.
    send(&mut ws, &crafted_hello_with_oversized_token()).await;
    expect_error_and_bye(&mut ws, 1, ByeReason::ByeLimitViolation).await;
}

/// Protobuf bytes of `Envelope.hello.stream_token` = 300 zero bytes, encoded by hand (field
/// 1 of Envelope, wire type 2; field 3 of Hello, wire type 2).
fn crafted_hello_with_oversized_token() -> Vec<u8> {
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

    let token = vec![0u8; 300];
    let mut hello = vec![0x1a]; // Hello.stream_token: field 3, wire type 2
    hello.extend(varint(300));
    hello.extend(token);

    let mut envelope = vec![0x0a]; // Envelope.hello: field 1, wire type 2
    envelope.extend(varint(hello.len() as u64));
    envelope.extend(hello);
    envelope
}

#[tokio::test]
async fn readiness_flips_from_503_to_200() {
    let server = spawn_server().await;
    assert_eq!(http_get(&server.addr, "/readyz").await, 503);
    server.state.set_ready();
    assert_eq!(http_get(&server.addr, "/readyz").await, 200);
}

/// A bare HTTP/1.1 GET over raw TCP, returning the status code.
async fn http_get(addr: &SocketAddr, path: &str) -> u16 {
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

#[tokio::test]
async fn a_second_session_while_one_is_live_is_refused() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut first = connect(&server).await;
    send(&mut first, &hello(vec![], None)).await;
    let Body::HelloReply(_) = read_body(&mut first).await else {
        panic!("expected a HelloReply");
    };

    // While the first session is live, a second upgrade is refused before any protocol
    // bytes: the WebSocket handshake itself fails with 503.
    let url = format!("ws://{}/session", server.addr);
    let second = connect_async(url).await;
    assert!(second.is_err(), "a second session must not upgrade");
}

#[tokio::test]
async fn a_second_hello_after_the_handshake_closes_with_bye_protocol_violation() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = session_started(&server).await;

    // A Hello after the handshake is a forbidden order (v0.md §5: "a Hello after the
    // handshake"). It earns the same shape every client-caused close earns: a ServerError
    // with the order code, then the Bye naming the reason, then the socket closes.
    send(&mut ws, &hello(vec![], None)).await;
    expect_error_and_bye(&mut ws, 3, ByeReason::ByeProtocolViolation).await;

    // After the Bye the socket closes, and the session is over for good: the next upgrade
    // meets a dead backend, not a parked session.
    let url = format!("ws://{}/session", server.addr);
    assert!(
        connect_async(url).await.is_err(),
        "an out-of-order Hello ends the session; nothing is served again"
    );
}

// -------------------------------------------------------------------------------------------
// The session flow
// -------------------------------------------------------------------------------------------

/// Drives a session through the handshake and returns once the HelloReply is read.
async fn session_started(server: &TestServer) -> Client {
    let mut ws = connect(server).await;
    send(&mut ws, &hello(vec![], None)).await;
    let Body::HelloReply(reply) = read_body(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    assert!(!reply.resumed);
    ws
}

/// Reads the next SurfaceNew, panicking with what arrived instead.
async fn expect_surface_new(ws: &mut Client, surface_id: u32) -> appricot_proto::wire::SurfaceNew {
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

/// Reads the next Frame of `surface_id`.
async fn expect_frame(ws: &mut Client, surface_id: u32) -> appricot_proto::wire::Frame {
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

#[tokio::test]
async fn the_full_flow_surfaces_configures_frames_and_acks() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = session_started(&server).await;

    // A window appears: announced, then framed whole (the first frame is a full redraw).
    server.mock.create_surface(7, Size::new(300, 200));
    let announced = expect_surface_new(&mut ws, 7).await;
    assert_eq!(announced.role, appricot_proto::wire::Role::Toplevel as i32);
    let size = announced.size.expect("a size is carried");
    assert_eq!((size.width, size.height), (300, 200));

    let frame = expect_frame(&mut ws, 7).await;
    assert_eq!(frame.sequence, 1);
    assert!(frame.full_redraw);
    assert!(!frame.tiles.is_empty());
    assert!(frame.tiles.len() <= 48, "the tile budget holds");
    let covered = frame
        .tiles
        .iter()
        .map(|tile| tile.rect.expect("every tile carries a rect"))
        .fold(None::<Rect>, |acc, r| {
            let r = Rect::new(r.x, r.y, r.width, r.height);
            Some(match acc {
                None => r,
                Some(acc) => acc.union(r),
            })
        })
        .expect("the frame covers something");
    assert_eq!(
        (
            covered.origin.x,
            covered.origin.y,
            covered.size.width,
            covered.size.height
        ),
        (0, 0, 300, 200),
        "the first frame covers the whole surface"
    );
    for tile in &frame.tiles {
        assert_ne!(tile.codec, 0);
        assert!(tile.data.len() <= MAX_TILE_BYTES);
    }

    // Damage flows after an ack frees the credit.
    send(
        &mut ws,
        &envelope(Body::FrameAck(appricot_proto::wire::FrameAck {
            surface_id: 7,
            sequence: 1,
        })),
    )
    .await;
    server.mock.push(SurfaceEvent::Damaged {
        id: SurfaceId::new(7),
        rect: Rect::new(10, 20, 30, 40),
    });
    let second = expect_frame(&mut ws, 7).await;
    assert_eq!(second.sequence, 2);
    assert!(!second.full_redraw);
    let tile_rect = second.tiles[0].rect.expect("a rect");
    assert_eq!(
        (tile_rect.x, tile_rect.y, tile_rect.width, tile_rect.height),
        (10, 20, 30, 40)
    );
    send(
        &mut ws,
        &envelope(Body::FrameAck(appricot_proto::wire::FrameAck {
            surface_id: 7,
            sequence: 2,
        })),
    )
    .await;

    // A configure is applied by the backend and acked with the client's own serial, whatever
    // serial the session mints internally.
    send(
        &mut ws,
        &envelope(Body::Configure(appricot_proto::wire::Configure {
            surface_id: 7,
            serial: 55,
            size: Some(appricot_proto::wire::Size {
                width: 400,
                height: 250,
            }),
        })),
    )
    .await;

    // The backend saw the configure, and the ack names serial 55 with the size taken.
    let input = server.mock.wait_input(1).await;
    assert_eq!(
        input[0],
        Input::Configure {
            surface: 7,
            size: Size::new(400, 250),
        }
    );
    match read_until(&mut ws, "ConfigureAck", |b| {
        matches!(b, Body::ConfigureAck(_))
    })
    .await
    {
        Body::ConfigureAck(ack) => {
            assert_eq!(ack.serial, 55, "the client's serial comes back");
            assert_eq!(ack.size.map(|s| (s.width, s.height)), Some((400, 250)));
        }
        _ => unreachable!("read_until checked the variant"),
    }
}

/// The input sequence `input_reaches_the_backend_mapped` expects the backend to record, in
/// the order the test sends it.
fn expected_input_sequence() -> Vec<Input> {
    vec![
        Input::Motion {
            surface: 1,
            at: appricot_core::Point::new(10, -20),
        },
        Input::Button {
            surface: 1,
            button: PointerButton::Middle,
            state: PressState::Pressed,
        },
        Input::Axis {
            surface: 1,
            steps: appricot_core::Point::new(0, -3),
        },
        Input::Key {
            key: KeyEvent {
                keysym: Keysym(0x61),
                code: Some(KeyCode::new("KeyA").expect("a valid code")),
                state: PressState::Pressed,
            },
        },
        Input::Focus { surface: 1 },
        Input::Blur,
        Input::Clipboard {
            text: "paste me".to_owned(),
        },
        Input::Close { surface: 1 },
    ]
}

#[tokio::test]
async fn input_reaches_the_backend_mapped() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = session_started(&server).await;
    server.mock.create_surface(1, Size::new(100, 100));
    expect_surface_new(&mut ws, 1).await;

    send(
        &mut ws,
        &envelope(Body::PointerMove(appricot_proto::wire::PointerMove {
            surface_id: 1,
            x: 10,
            y: -20,
        })),
    )
    .await;
    send(
        &mut ws,
        &envelope(Body::PointerButton(appricot_proto::wire::PointerButton {
            surface_id: 1,
            button: 2,
            pressed: true,
        })),
    )
    .await;
    send(
        &mut ws,
        &envelope(Body::PointerAxis(appricot_proto::wire::PointerAxis {
            surface_id: 1,
            steps_x: 0,
            steps_y: -3,
        })),
    )
    .await;
    send(
        &mut ws,
        &envelope(Body::Key(appricot_proto::wire::Key {
            keysym: 0x61,
            code: "KeyA".into(),
            pressed: true,
            modifiers: 1,
        })),
    )
    .await;
    send(
        &mut ws,
        &envelope(Body::FocusNotify(appricot_proto::wire::FocusNotify {
            surface_id: 1,
        })),
    )
    .await;
    send(
        &mut ws,
        &envelope(Body::BlurRelease(appricot_proto::wire::BlurRelease {})),
    )
    .await;
    send(
        &mut ws,
        &envelope(Body::ClipboardSet(appricot_proto::wire::ClipboardSet {
            text: "paste me".into(),
        })),
    )
    .await;
    send(
        &mut ws,
        &envelope(Body::CloseRequest(appricot_proto::wire::CloseRequest {
            surface_id: 1,
        })),
    )
    .await;

    assert_eq!(
        server.mock.wait_input(8).await,
        expected_input_sequence(),
        "every input message maps and arrives in order"
    );
}

#[tokio::test]
async fn a_button_that_is_no_x_button_is_ignored() {
    let server = spawn_server().await;
    server.state.set_ready();
    let mut ws = session_started(&server).await;
    server.mock.create_surface(1, Size::new(100, 100));
    expect_surface_new(&mut ws, 1).await;

    send(
        &mut ws,
        &envelope(Body::PointerButton(appricot_proto::wire::PointerButton {
            surface_id: 1,
            button: 9,
            pressed: true,
        })),
    )
    .await;
    // Button 9 records nothing and kills nothing: the session stays up (this FrameAck of an
    // unknown sequence is ignored too, never fatal).
    send(
        &mut ws,
        &envelope(Body::FrameAck(appricot_proto::wire::FrameAck {
            surface_id: 1,
            sequence: 42,
        })),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(server.mock.input().is_empty(), "button 9 delivered nothing");
}

// -------------------------------------------------------------------------------------------
// Resume
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_dropped_socket_resumes_within_the_grace() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("appricot_streamer=debug")
        .try_init();
    let server = spawn_server().await;
    server.state.set_ready();

    let mut first = connect(&server).await;
    send(&mut first, &hello(vec![], None)).await;
    let Body::HelloReply(reply) = read_body(&mut first).await else {
        panic!("expected a HelloReply");
    };
    let resume_serial = reply.resume_serial.expect("a session can be resumed");

    server.mock.create_surface(7, Size::new(120, 80));
    expect_surface_new(&mut first, 7).await;
    expect_frame(&mut first, 7).await;

    // Drop the socket without a Bye. The server parks the session; a reconnect naming the
    // serial resumes it.
    drop(first);

    let mut second = connect_with_retry(&server).await;
    send(&mut second, &hello(vec![], Some(resume_serial))).await;
    let Body::HelloReply(resumed) = read_body(&mut second).await else {
        panic!("expected a HelloReply");
    };
    assert!(resumed.resumed, "the parked session resumes");
    assert!(
        resumed.resume_serial.is_some(),
        "the resumed session can be resumed again"
    );

    // The whole window set is re-announced, then one full-redraw frame per surface.
    let announced = expect_surface_new(&mut second, 7).await;
    let size = announced.size.expect("a size is carried");
    assert_eq!((size.width, size.height), (120, 80));
    let frame = expect_frame(&mut second, 7).await;
    assert!(frame.full_redraw, "a resume repaints everything");
    assert_eq!(frame.sequence, 1, "the resume restarts the sequences");
}

#[tokio::test]
async fn a_mismatched_resume_serial_replaces_the_session() {
    let server = spawn_server().await;
    server.state.set_ready();

    let mut first = connect(&server).await;
    send(&mut first, &hello(vec![], None)).await;
    let Body::HelloReply(reply) = read_body(&mut first).await else {
        panic!("expected a HelloReply");
    };
    assert!(reply.resume_serial.is_some());

    drop(first);

    // A serial that names nothing replaces the parked session with a fresh one, as the spec
    // allows (v0.md §7): a client that wants a new session says so, and this server's policy
    // is to let it.
    let mut second = connect_with_retry(&server).await;
    send(&mut second, &hello(vec![], Some(999_999))).await;
    let Body::HelloReply(resumed) = read_body(&mut second).await else {
        panic!("expected a HelloReply");
    };
    assert!(
        !resumed.resumed,
        "a wrong serial starts fresh, not a resume"
    );
}

#[tokio::test]
async fn a_bye_ends_the_session_cleanly() {
    let server = spawn_server().await;
    server.state.set_ready();

    let mut ws = session_started(&server).await;
    send(
        &mut ws,
        &envelope(Body::Bye(appricot_proto::wire::Bye {
            reason: ByeReason::ByePeerClosed as i32,
            text: String::new(),
        })),
    )
    .await;

    // The server answers with its own Bye and closes.
    match read_body(&mut ws).await {
        Body::Bye(b) => assert_eq!(b.reason, ByeReason::ByePeerClosed as i32),
        other => panic!("expected the server's Bye, got {other:?}"),
    }

    // And the session is over: the next upgrade meets a dead backend, not a parked session.
    let url = format!("ws://{}/session", server.addr);
    let again = connect_async(url).await;
    assert!(again.is_err(), "a clean end leaves nothing to serve");
}
