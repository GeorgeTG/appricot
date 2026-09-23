//! How the one session ends, and what the process says afterwards.
//!
//! - A display that dies ends the session wherever it is: live (`Bye(BYE_SERVER_SHUTDOWN)`),
//!   parked (a resume meets `Bye(BYE_SESSION_GONE)`, v0.md §7), or idle (nothing to tell).
//!   Afterwards the upgrade is refused with `410`, readiness reads red, and the outcome is a
//!   fault.
//! - Every key and button the backend holds is released when the socket goes and when the
//!   session ends (contract C9).
//! - A stop asked of the server says `Bye(BYE_SERVER_SHUTDOWN)` to a live session.
//! - A socket that fails while the server is sending parks the session, exactly as one that
//!   fails while the server is reading (v0.md §7, design rule 11).

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use appricot_core::{KeyCode, KeyEvent, Keysym, PressState, Size};
use appricot_proto::wire::{Body, ByeReason};

use appricot_streamer::session::EndCause;
use common::Input;
use common::harness::{
    connect_with_retry, envelope, expect_bye, expect_frame, expect_over, expect_surface_new,
    handshake, hello, send, spawn_mock_server, wait_until,
};

#[tokio::test]
async fn a_display_that_dies_under_a_live_session_ends_it() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    mock.kill_display();
    expect_bye(&mut ws, ByeReason::ByeServerShutdown).await;
    expect_over(&server, EndCause::Fault).await;
}

#[tokio::test]
async fn a_resume_onto_a_dead_display_meets_session_gone() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, reply) = handshake(&server.addr, None).await;
    mock.create_surface(7, Size::new(120, 80));
    expect_surface_new(&mut ws, 7).await;
    expect_frame(&mut ws, 7).await;
    drop(ws);
    let serial = reply.resume_serial.expect("a session can be resumed");

    // The park releases held input on its way: once the Blur is recorded, the socket's end
    // has settled into a park.
    mock.wait_input(1).await;

    // The display dies while the session is parked. The backend is gone with it; the park
    // stays to tell the client, then the session ends for good (v0.md §7).
    mock.kill_display();
    wait_until("the backend is gone", || mock.is_dropped()).await;

    let mut resumed = connect_with_retry(&server.addr).await;
    send(&mut resumed, &hello(Some(serial))).await;
    expect_bye(&mut resumed, ByeReason::ByeSessionGone).await;
    expect_over(&server, EndCause::Fault).await;
}

#[tokio::test]
async fn a_display_that_dies_with_nobody_attached_ends_the_session() {
    let (server, mock) = spawn_mock_server().await;
    mock.kill_display();
    expect_over(&server, EndCause::Fault).await;
}

#[tokio::test]
async fn held_input_is_released_when_the_socket_goes() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;
    mock.create_surface(1, Size::new(100, 100));
    expect_surface_new(&mut ws, 1).await;

    send(
        &mut ws,
        &envelope(Body::Key(appricot_proto::wire::Key {
            keysym: 0x61,
            code: "KeyA".into(),
            pressed: true,
            modifiers: 0,
        })),
    )
    .await;
    mock.wait_input(1).await;

    // The socket drops with the key still down: the backend must not keep it pressed.
    drop(ws);
    assert_eq!(
        mock.wait_input(2).await,
        vec![
            Input::Key {
                key: KeyEvent {
                    keysym: Keysym(0x61),
                    code: Some(KeyCode::new("KeyA").expect("a valid code")),
                    state: PressState::Pressed,
                },
            },
            Input::Blur,
        ],
        "the park released what was held"
    );
    assert_eq!(
        server.state.outcome(),
        None,
        "a dropped socket parks; it does not end"
    );
}

#[tokio::test]
async fn held_input_is_released_when_the_session_ends() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    send(
        &mut ws,
        &envelope(Body::Bye(appricot_proto::wire::Bye {
            reason: ByeReason::ByePeerClosed as i32,
            text: String::new(),
        })),
    )
    .await;
    expect_bye(&mut ws, ByeReason::ByePeerClosed).await;
    assert_eq!(mock.wait_input(1).await, vec![Input::Blur]);
    expect_over(&server, EndCause::Clean).await;
}

#[tokio::test]
async fn a_stop_says_bye_to_a_live_session() {
    let (server, _mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    server.state.shutdown();
    expect_bye(&mut ws, ByeReason::ByeServerShutdown).await;
    expect_over(&server, EndCause::Clean).await;
}

#[tokio::test]
async fn a_stop_tears_a_parked_session_down() {
    let (server, mock) = spawn_mock_server().await;
    let (ws, _reply) = handshake(&server.addr, None).await;
    drop(ws);
    // Wait for the park to land: the next upgrade would take the slot, so watch the backend's
    // input instead — the park releases held input on its way.
    mock.wait_input(1).await;

    server.state.shutdown();
    wait_until("the parked backend is torn down", || mock.is_dropped()).await;
    expect_over(&server, EndCause::Clean).await;
}

#[tokio::test]
async fn a_socket_that_fails_while_the_server_sends_parks_the_session() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, reply) = handshake(&server.addr, None).await;
    let serial = reply.resume_serial.expect("a session can be resumed");

    // A window as large as the wire allows: its first frame is 40 RAW tiles, about 9 MB — more
    // than loopback buffers take, so the server is still writing it when the client goes.
    mock.create_surface(7, Size::new(1920, 1200));
    expect_surface_new(&mut ws, 7).await;
    drop(ws);

    // Whether the send failed first or the read did, the socket is gone: parked, not ended.
    let (mut resumed, reply) = handshake(&server.addr, Some(serial)).await;
    assert!(
        reply.resumed,
        "a failed send parks the session like a failed read"
    );
    expect_surface_new(&mut resumed, 7).await;
    let frame = expect_frame(&mut resumed, 7).await;
    assert!(frame.full_redraw, "a resume repaints everything");
}
