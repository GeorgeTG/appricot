//! The handshake's refusals, and what a refusal leaves behind.
//!
//! A refused handshake claims nothing (docs/protocol/v0.md §7): every refusal — made while the
//! backend is idle or while a session is parked — answers what the spec names and hands the
//! slot back untouched, so the next client is served and a parked session still resumes. The
//! serial a reconnect must name is the one the client last received, and a replaced session is
//! never parked under the serial of the one it replaced.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use appricot_core::Size;
use appricot_proto::wire::{Body, ByeReason, Hello};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use appricot_streamer::session::EndCause;
use common::MockHandle;
use common::harness::{
    Client, TOKEN, TestServer, WAIT, connect_with_retry, envelope, expect_bye, expect_closed,
    expect_error_and_bye, expect_frame, expect_over, expect_surface_new, handshake,
    hello_with_wrong_token, send, spawn_mock_server,
};

/// One way a first message is refused.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    BadToken,
    BadVersion,
    TextFrame,
    Garbage,
    CloseWithoutHello,
    UnknownBody,
}

const EVERY_REFUSAL: [Refusal; 6] = [
    Refusal::BadToken,
    Refusal::BadVersion,
    Refusal::TextFrame,
    Refusal::Garbage,
    Refusal::CloseWithoutHello,
    Refusal::UnknownBody,
];

/// An envelope naming field 26, which v0 does not define: a message this server cannot know.
/// Field 25 was this probe's unknown message until v0 gained `clipboard_text` for it.
const UNKNOWN_BODY: [u8; 3] = [0xd2, 0x01, 0x00];

/// Asserts the socket closes with no protocol message first: only the WebSocket close frame,
/// with code 1002 (protocol error).
async fn expect_protocol_close(ws: &mut Client) {
    let next = tokio::time::timeout(WAIT, ws.next())
        .await
        .expect("the socket closes within the deadline");
    match next {
        Some(Ok(Message::Close(Some(close)))) => {
            assert_eq!(
                close.code,
                CloseCode::Protocol,
                "the close names a protocol error"
            );
        }
        other => panic!("expected a close frame with code 1002 and no reply, got {other:?}"),
    }
}

/// Makes the refusal on a fresh socket and checks the answer the spec names for it.
async fn refuse(server: &TestServer<common::MockBackend>, refusal: Refusal) {
    let mut ws = connect_with_retry(&server.addr).await;
    match refusal {
        Refusal::BadToken => {
            send(&mut ws, &hello_with_wrong_token(None)).await;
            expect_bye(&mut ws, ByeReason::ByeAuthFailed).await;
        }
        Refusal::BadVersion => {
            let futuristic = envelope(Body::Hello(Hello {
                protocol_version: 99,
                client_name: "handshake-test".into(),
                stream_token: TOKEN.to_vec(),
                codecs: vec![],
                resume_serial: None,
            }));
            send(&mut ws, &futuristic).await;
            expect_bye(&mut ws, ByeReason::ByeProtocolVersion).await;
        }
        Refusal::TextFrame => {
            ws.send(Message::Text("hello?".into()))
                .await
                .expect("text sends");
            expect_bye(&mut ws, ByeReason::ByeProtocolViolation).await;
        }
        Refusal::Garbage => {
            send(&mut ws, &[0xff, 0xff, 0xff, 0x7f, 0x00, 0x01]).await;
            expect_error_and_bye(&mut ws, 4, ByeReason::ByeProtocolViolation).await;
        }
        Refusal::CloseWithoutHello => {
            ws.close(None).await.expect("the close sends");
            expect_closed(&mut ws).await;
        }
        Refusal::UnknownBody => {
            // v0.md §1: a protocol violation, closed without a reply — no ServerError, no Bye.
            send(&mut ws, &UNKNOWN_BODY).await;
            expect_protocol_close(&mut ws).await;
        }
    }
}

/// Starts a session with window 7, drops its socket without a Bye, and returns the serial.
async fn park_a_session_with_a_window(
    server: &TestServer<common::MockBackend>,
    mock: &MockHandle,
) -> u32 {
    let (mut ws, reply) = handshake(&server.addr, None).await;
    mock.create_surface(7, Size::new(120, 80));
    expect_surface_new(&mut ws, 7).await;
    expect_frame(&mut ws, 7).await;
    drop(ws);
    reply.resume_serial.expect("a session can be resumed")
}

/// Asserts a resume with `serial` succeeds and resynchronises window 7.
async fn expect_resume(server: &TestServer<common::MockBackend>, serial: u32) -> (Client, u32) {
    let (mut ws, reply) = handshake(&server.addr, Some(serial)).await;
    assert!(reply.resumed, "the parked session resumes");
    let announced = expect_surface_new(&mut ws, 7).await;
    assert_eq!(announced.size.map(|s| (s.width, s.height)), Some((120, 80)));
    let frame = expect_frame(&mut ws, 7).await;
    assert!(frame.full_redraw, "a resume repaints everything");
    (
        ws,
        reply
            .resume_serial
            .expect("a resumed session can be resumed again"),
    )
}

#[tokio::test]
async fn a_refusal_while_idle_claims_nothing() {
    for refusal in EVERY_REFUSAL {
        let (server, _mock) = spawn_mock_server().await;
        refuse(&server, refusal).await;
        let (_ws, reply) = handshake(&server.addr, None).await;
        assert!(
            !reply.resumed,
            "{refusal:?}: the next client gets a fresh session"
        );
    }
}

#[tokio::test]
async fn a_refusal_while_parked_claims_nothing() {
    for refusal in EVERY_REFUSAL {
        let (server, mock) = spawn_mock_server().await;
        let serial = park_a_session_with_a_window(&server, &mock).await;
        refuse(&server, refusal).await;
        // The parked session is untouched: same serial, same window set.
        expect_resume(&server, serial).await;
    }
}

#[tokio::test]
async fn the_serial_the_client_last_received_is_the_one_that_resumes() {
    let (server, mock) = spawn_mock_server().await;
    let first = park_a_session_with_a_window(&server, &mock).await;

    // Resume, receive a new serial, and lose the socket straight away.
    let (ws, second) = expect_resume(&server, first).await;
    assert_ne!(second, first, "every reply mints a new serial");
    drop(ws);

    // The session parks under the serial the client holds now.
    expect_resume(&server, second).await;
}

#[tokio::test]
async fn a_replaced_session_never_answers_to_the_old_serial() {
    let (server, mock) = spawn_mock_server().await;
    let old = park_a_session_with_a_window(&server, &mock).await;

    // A Hello without a serial replaces the parked session; its socket then drops.
    let (ws, reply) = handshake(&server.addr, None).await;
    assert!(!reply.resumed);
    let new = reply.resume_serial.expect("a session can be resumed");
    drop(ws);

    // The old serial names the replaced session, which is gone: no resume of the empty one.
    let (ws, stale) = handshake(&server.addr, Some(old)).await;
    assert!(
        !stale.resumed,
        "the replaced session's serial resumes nothing"
    );
    drop(ws);
    assert_ne!(old, new);
}

#[tokio::test]
async fn an_unknown_message_in_a_session_closes_without_a_reply() {
    let (server, _mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    send(&mut ws, &UNKNOWN_BODY).await;
    expect_protocol_close(&mut ws).await;

    // A protocol violation ends the session; it does not park it.
    expect_over(&server, EndCause::Clean).await;
}
