//! Integration tests: the streamer server over a real loopback WebSocket.
//!
//! Every test spawns the in-process axum server on `127.0.0.1:0` (never a fixed port) with a
//! set token and the mock backend from `common`, and drives it with a real
//! `tokio-tungstenite` client speaking the actual wire codec (`common::harness`). What is
//! asserted is the behaviour a host sees: the handshake, the refusals, the frame path, the
//! input mapping and the resume.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use appricot_core::{
    KeyCode, KeyEvent, Keysym, PointerButton, PressState, Rect, Size, SurfaceEvent, SurfaceId,
};
use appricot_proto::limits::{MAX_FRAME_CREDITS, MAX_TILE_BYTES, RESUME_GRACE_MS, codec};
use appricot_proto::wire::{Body, ByeReason, Hello};
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

use appricot_streamer::backend::BackendHandle;
use appricot_streamer::server::ServerState;
use appricot_streamer::session::EndCause;
use common::harness::{
    Client, TOKEN, connect, connect_with_retry, envelope, expect_bye, expect_error_and_bye,
    expect_frame, expect_over, expect_surface_new, hello, hello_offering, hello_with_wrong_token,
    http_get, read_body, read_until, send, serve_state, spawn_mock_server, upgrade_refused,
};
use common::{Input, MockBackend};

// -------------------------------------------------------------------------------------------
// Handshake and refusals
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn hello_is_answered_with_version_codecs_and_resume_terms() {
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = connect(&server.addr).await;

    send(&mut ws, &hello_offering(vec![codec::QOI, codec::RAW], None)).await;
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
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = connect(&server.addr).await;

    send(&mut ws, &hello(None)).await;
    let Body::HelloReply(reply) = read_body(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    assert_eq!(reply.codecs, vec![codec::RAW]);
}

#[tokio::test]
async fn a_wrong_token_closes_with_bye_auth_failed() {
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = connect(&server.addr).await;

    send(&mut ws, &hello_with_wrong_token(None)).await;
    expect_bye(&mut ws, ByeReason::ByeAuthFailed).await;
}

#[tokio::test]
async fn a_refused_handshake_does_not_consume_the_session() {
    let (server, _mock) = spawn_mock_server().await;

    // A mistyped token first: the server answers BYE_AUTH_FAILED and closes the socket.
    let mut wrong = connect(&server.addr).await;
    send(&mut wrong, &hello_with_wrong_token(None)).await;
    expect_bye(&mut wrong, ByeReason::ByeAuthFailed).await;
    drop(wrong);

    // The refusal claimed nothing — the session belongs to an authenticated Hello — so the
    // retry with the right token is served. Before this rule held the upgrade alone claimed
    // the session, and one typo left the process refusing every later upgrade with 503
    // (measured 2026-09-21).
    let mut retry = connect_with_retry(&server.addr).await;
    send(&mut retry, &hello(None)).await;
    let Body::HelloReply(reply) = read_body(&mut retry).await else {
        panic!("expected a HelloReply, not a refusal");
    };
    assert!(!reply.resumed, "the retry is a fresh session, not a resume");
}

#[tokio::test]
async fn an_unsupported_version_closes_with_bye_protocol_version() {
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = connect(&server.addr).await;

    let futuristic = envelope(Body::Hello(Hello {
        protocol_version: 99,
        client_name: "integration-test".into(),
        stream_token: TOKEN.to_vec(),
        codecs: vec![],
        resume_serial: None,
    }));
    send(&mut ws, &futuristic).await;
    expect_bye(&mut ws, ByeReason::ByeProtocolVersion).await;
}

#[tokio::test]
async fn a_wrong_first_message_closes_with_bye_protocol_violation() {
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = connect(&server.addr).await;

    // A FrameAck before any Hello: the wrong first message.
    send(
        &mut ws,
        &envelope(Body::FrameAck(appricot_proto::wire::FrameAck {
            surface_id: 1,
            sequence: 1,
        })),
    )
    .await;
    expect_bye(&mut ws, ByeReason::ByeProtocolViolation).await;
}

#[tokio::test]
async fn a_text_frame_is_refused() {
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = connect(&server.addr).await;

    ws.send(Message::Text("hello?".into()))
        .await
        .expect("text sends");
    expect_bye(&mut ws, ByeReason::ByeProtocolViolation).await;
}

#[tokio::test]
async fn garbage_bytes_close_with_server_error_and_bye() {
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = connect(&server.addr).await;

    send(&mut ws, &[0xff, 0xff, 0xff, 0x7f, 0x00, 0x01]).await;
    expect_error_and_bye(&mut ws, 4, ByeReason::ByeProtocolViolation).await;
}

#[tokio::test]
async fn a_limit_violation_closes_with_server_error_and_bye() {
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = connect(&server.addr).await;

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
    let (backend, _mock) = MockBackend::pair();
    let state = ServerState::new(TOKEN.to_vec(), BackendHandle::spawn(backend));
    let addr = serve_state(std::sync::Arc::clone(&state)).await;
    assert_eq!(http_get(&addr, "/readyz").await, 503);
    state.set_ready();
    assert_eq!(http_get(&addr, "/readyz").await, 200);
}

#[tokio::test]
async fn a_second_session_while_one_is_live_is_refused() {
    let (server, _mock) = spawn_mock_server().await;
    let mut first = connect(&server.addr).await;
    send(&mut first, &hello(None)).await;
    let Body::HelloReply(_) = read_body(&mut first).await else {
        panic!("expected a HelloReply");
    };

    // While the first session is live, a second upgrade is refused before any protocol
    // bytes: the WebSocket handshake itself fails with 503, busy.
    assert_eq!(upgrade_refused(&server.addr).await, 503, "busy, not gone");
}

#[tokio::test]
async fn a_second_hello_after_the_handshake_closes_with_bye_protocol_violation() {
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = session_started(&server.addr).await;

    // A Hello after the handshake is a forbidden order (v0.md §5: "a Hello after the
    // handshake"). It earns the same shape every client-caused close earns: a ServerError
    // with the order code, then the Bye naming the reason, then the socket closes.
    send(&mut ws, &hello(None)).await;
    expect_error_and_bye(&mut ws, 3, ByeReason::ByeProtocolViolation).await;

    // After the Bye the session is over for good: the next upgrade meets a dead backend, not
    // a parked session, and is told so with 410.
    expect_over(&server, EndCause::Clean).await;
}

// -------------------------------------------------------------------------------------------
// The session flow
// -------------------------------------------------------------------------------------------

/// Drives a session through the handshake and returns once the HelloReply is read.
async fn session_started(addr: &std::net::SocketAddr) -> Client {
    let mut ws = connect(addr).await;
    send(&mut ws, &hello(None)).await;
    let Body::HelloReply(reply) = read_body(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    assert!(!reply.resumed);
    ws
}

#[tokio::test]
async fn the_full_flow_surfaces_configures_frames_and_acks() {
    let (server, mock) = spawn_mock_server().await;
    let mut ws = session_started(&server.addr).await;

    // A window appears: announced, then framed whole (the first frame is a full redraw).
    mock.create_surface(7, Size::new(300, 200));
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
    mock.push(SurfaceEvent::Damaged {
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
    let input = mock.wait_input(1).await;
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
    let (server, mock) = spawn_mock_server().await;
    let mut ws = session_started(&server.addr).await;
    mock.create_surface(1, Size::new(100, 100));
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
        mock.wait_input(8).await,
        expected_input_sequence(),
        "every input message maps and arrives in order"
    );
}

#[tokio::test]
async fn a_button_that_is_no_x_button_is_ignored() {
    let (server, mock) = spawn_mock_server().await;
    let mut ws = session_started(&server.addr).await;
    mock.create_surface(1, Size::new(100, 100));
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
    // A sentinel the backend does record. Messages are handled in order, so once it arrives
    // everything before it was handled: nothing need be waited for on a clock.
    send(
        &mut ws,
        &envelope(Body::FocusNotify(appricot_proto::wire::FocusNotify {
            surface_id: 1,
        })),
    )
    .await;
    assert_eq!(
        mock.wait_input(1).await,
        vec![Input::Focus { surface: 1 }],
        "button 9 delivered nothing; only the sentinel arrived"
    );
}

// -------------------------------------------------------------------------------------------
// Resume
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_dropped_socket_resumes_within_the_grace() {
    let (server, mock) = spawn_mock_server().await;

    let mut first = connect(&server.addr).await;
    send(&mut first, &hello(None)).await;
    let Body::HelloReply(reply) = read_body(&mut first).await else {
        panic!("expected a HelloReply");
    };
    let resume_serial = reply.resume_serial.expect("a session can be resumed");

    mock.create_surface(7, Size::new(120, 80));
    expect_surface_new(&mut first, 7).await;
    expect_frame(&mut first, 7).await;

    // Drop the socket without a Bye. The server parks the session; a reconnect naming the
    // serial resumes it.
    drop(first);

    let mut second = connect_with_retry(&server.addr).await;
    send(&mut second, &hello(Some(resume_serial))).await;
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
    // Then exactly one cursor message (v0.md §7): no cursor was ever seen, so CursorGone.
    assert!(matches!(read_body(&mut second).await, Body::CursorGone(_)));
    let frame = expect_frame(&mut second, 7).await;
    assert!(frame.full_redraw, "a resume repaints everything");
    assert_eq!(frame.sequence, 1, "the resume restarts the sequences");
}

#[tokio::test]
async fn a_mismatched_resume_serial_replaces_the_session() {
    let (server, _mock) = spawn_mock_server().await;

    let mut first = connect(&server.addr).await;
    send(&mut first, &hello(None)).await;
    let Body::HelloReply(reply) = read_body(&mut first).await else {
        panic!("expected a HelloReply");
    };
    assert!(reply.resume_serial.is_some());

    drop(first);

    // A serial that names nothing replaces the parked session with a fresh one, as the spec
    // allows (v0.md §7): a client that wants a new session says so, and this server's policy
    // is to let it.
    let mut second = connect_with_retry(&server.addr).await;
    send(&mut second, &hello(Some(999_999))).await;
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
    let (server, _mock) = spawn_mock_server().await;

    let mut ws = session_started(&server.addr).await;
    send(
        &mut ws,
        &envelope(Body::Bye(appricot_proto::wire::Bye {
            reason: ByeReason::ByePeerClosed as i32,
            text: String::new(),
        })),
    )
    .await;

    // The server answers with its own Bye and closes.
    expect_bye(&mut ws, ByeReason::ByePeerClosed).await;

    // And the session is over: the next upgrade meets a dead backend, not a parked session,
    // and readiness reads red.
    expect_over(&server, EndCause::Clean).await;
}

#[tokio::test]
async fn windows_opened_before_the_first_client_are_announced_to_it() {
    let (server, mock) = spawn_mock_server().await;
    // The app opens its window while nobody is attached; the keeper keeps it in the standby
    // session instead of letting the feed pile up.
    mock.create_surface(4, Size::new(64, 48));
    common::harness::wait_until("the backend took the event", || mock.queued() == 0).await;

    let mut ws = session_started(&server.addr).await;
    let announced = expect_surface_new(&mut ws, 4).await;
    let size = announced.size.expect("a size is carried");
    assert_eq!((size.width, size.height), (64, 48));
    let frame = expect_frame(&mut ws, 4).await;
    assert!(frame.full_redraw);
    assert_eq!(frame.sequence, 1);
}
