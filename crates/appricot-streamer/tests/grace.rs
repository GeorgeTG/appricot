//! The resume grace on a short leash: expiry, teardown, and what a reconnect then meets.
//!
//! The protocol's grace (docs/protocol/v0.md §7) is `RESUME_GRACE_MS` = 10 s — too slow to sit
//! out in a test. `appricot_streamer::config::set_resume_grace_ms` shortens it for this process
//! (the knob exists for exactly that; it overrides both the parked wait and the value the
//! `HelloReply` advertises). The override is process-wide and the tests of one binary share a
//! process, so every test here holds one lock and restores the default before it ends.
//!
//! What this file pins, against the real server over a real loopback WebSocket:
//!
//! - a session dropped inside the grace still resumes, however short the grace is;
//! - a `Hello` without a serial replaces a parked session with a fresh one: the old window set
//!   is never re-announced;
//! - once the grace expires, the keeper tears the parked session — backend included — down, and
//!   nothing is served again: the upgrade is refused with `410` and readiness reads red;
//! - the grace ends at the deadline the socket's death set: refused handshakes, and a socket
//!   that holds the slot past the deadline, never extend it.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use std::time::Duration;

use appricot_core::Size;
use appricot_proto::wire::{Body, ByeReason};
use tokio::time::Instant;

use appricot_streamer::config::set_resume_grace_ms;
use appricot_streamer::session::EndCause;
use common::MockHandle;
use common::harness::{
    Client, TestServer, connect_with_retry, expect_bye, expect_frame, expect_surface_new,
    handshake, hello, hello_with_wrong_token, http_get, read_body, read_until, send,
    spawn_mock_server, upgrade_refused, wait_until,
};

/// One lock for every test that touches the process-wide grace override: the tests of this
/// binary run in parallel tasks and the override is shared state. A `tokio` mutex, because
/// the guard is held across every await of a test.
static GRACE_KNOB: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Holds the knob for a test and sets the grace; the default comes back when the guard drops,
/// even when the test fails.
struct Grace(#[allow(dead_code)] tokio::sync::MutexGuard<'static, ()>);

impl Drop for Grace {
    fn drop(&mut self) {
        set_resume_grace_ms(0);
    }
}

async fn grace(ms: u32) -> Grace {
    let guard = GRACE_KNOB.lock().await;
    set_resume_grace_ms(ms);
    Grace(guard)
}

/// Drives one session to a live state with one window, and returns the socket with the
/// `HelloReply`'s resume serial.
async fn live_session_with_a_window(
    server: &TestServer<common::MockBackend>,
    mock: &MockHandle,
    id: u32,
) -> (Client, u32) {
    let (mut ws, reply) = handshake(&server.addr, None).await;
    assert!(!reply.resumed);
    let serial = reply.resume_serial.expect("a session can be resumed");
    mock.create_surface(id, Size::new(120, 80));
    expect_surface_new(&mut ws, id).await;
    expect_frame(&mut ws, id).await;
    (ws, serial)
}

/// Makes one handshake with a wrong token and reads its refusal.
async fn refused_handshake(server: &TestServer<common::MockBackend>) {
    let mut probe = connect_with_retry(&server.addr).await;
    send(&mut probe, &hello_with_wrong_token(None)).await;
    expect_bye(&mut probe, ByeReason::ByeAuthFailed).await;
}

#[tokio::test]
async fn an_expired_grace_tears_the_parked_session_down() {
    let _grace = grace(300).await;
    let (server, mock) = spawn_mock_server().await;
    let (ws, _serial) = live_session_with_a_window(&server, &mock, 7).await;

    // Drop the socket without a Bye: the session parks for 300 ms, then the keeper tears it —
    // backend included — down (v0.md §7: after the grace, the session is gone).
    drop(ws);
    wait_until("the parked backend is torn down", || mock.is_dropped()).await;

    // Past the grace, the parked serial names nothing and nothing is served again: the upgrade
    // itself is refused with 410 (the one session of this process is over), never answered
    // with a HelloReply, and readiness reads red. This streamer's answer to v0.md §7's "what a
    // reconnect then meets".
    assert_eq!(upgrade_refused(&server.addr).await, 410);
    assert_eq!(http_get(&server.addr, "/readyz").await, 503);
    assert_eq!(server.state.outcome(), Some(EndCause::Clean));
}

#[tokio::test]
async fn refused_handshakes_never_extend_the_grace() {
    let _grace = grace(600).await;
    let (server, mock) = spawn_mock_server().await;
    let (ws, _serial) = live_session_with_a_window(&server, &mock, 7).await;

    drop(ws);
    let dropped = Instant::now();

    // Three refused handshakes while parked, 100 ms apart. Each takes the slot and hands it
    // back; none may buy the parked session another grace.
    for n in 1..=3 {
        tokio::time::sleep_until(dropped + Duration::from_millis(100 * n)).await;
        refused_handshake(&server).await;
    }

    wait_until("the parked backend is torn down", || mock.is_dropped()).await;
    let lived = dropped.elapsed();
    // The park landed after the drop, so the teardown cannot come before the grace; a grace
    // re-armed by the last refusal (at about 300 ms) would end at 900 ms or later.
    assert!(
        lived >= Duration::from_millis(600),
        "torn down after {lived:?}, before the grace ended"
    );
    assert!(
        lived < Duration::from_millis(850),
        "torn down after {lived:?}: the refusals extended the grace"
    );
    assert_eq!(upgrade_refused(&server.addr).await, 410);
}

#[tokio::test]
async fn a_socket_holding_the_slot_past_the_deadline_does_not_extend_it() {
    let _grace = grace(300).await;
    let (server, mock) = spawn_mock_server().await;
    let (ws, _serial) = live_session_with_a_window(&server, &mock, 7).await;

    drop(ws);
    let dropped = Instant::now();

    // A peer takes the slot inside the grace and sits on it past the deadline: while it holds
    // the slot nothing can expire, but the deadline stays where it was.
    let mut probe = connect_with_retry(&server.addr).await;
    tokio::time::sleep_until(dropped + Duration::from_millis(500)).await;
    assert!(
        !mock.is_dropped(),
        "the held slot is not torn down under the socket"
    );
    send(&mut probe, &hello_with_wrong_token(None)).await;
    expect_bye(&mut probe, ByeReason::ByeAuthFailed).await;
    let refused = Instant::now();

    // The refusal hands back a park whose deadline has passed: it ends at once, not a whole
    // grace later.
    wait_until("the parked backend is torn down", || mock.is_dropped()).await;
    assert!(
        refused.elapsed() < Duration::from_millis(200),
        "torn down {:?} after the refusal: the refusal re-armed the grace",
        refused.elapsed()
    );
    assert_eq!(upgrade_refused(&server.addr).await, 410);
}

#[tokio::test]
async fn a_short_grace_still_resumes_a_session_dropped_within_it() {
    let _grace = grace(2_000).await;
    let (server, mock) = spawn_mock_server().await;
    let (first, serial) = live_session_with_a_window(&server, &mock, 7).await;
    drop(first);

    // Inside the 2 s grace, a reconnect naming the serial resumes the session.
    let (mut second, reply) = handshake(&server.addr, Some(serial)).await;
    assert!(reply.resumed, "the parked session resumes within the grace");

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
async fn a_hello_without_a_serial_replaces_a_parked_session_with_a_fresh_one() {
    let _grace = grace(2_000).await;
    let (server, mock) = spawn_mock_server().await;
    let (first, _serial) = live_session_with_a_window(&server, &mock, 7).await;
    drop(first);

    // A Hello that names no serial replaces the parked session (v0.md §7): the reply says
    // resumed = false and the client starts from an empty window set.
    let (mut second, reply) = handshake(&server.addr, None).await;
    assert!(!reply.resumed, "no serial named: fresh, not resumed");

    // A new window is announced on demand, and the old one never comes back: the replaced
    // session's window set died with it.
    mock.create_surface(8, Size::new(90, 60));
    let announced = expect_surface_new(&mut second, 8).await;
    let size = announced.size.expect("a size is carried");
    assert_eq!((size.width, size.height), (90, 60));
    expect_frame(&mut second, 8).await;

    // A sentinel from the app: the next message after frame 8 is its ClipboardAsk, so surface
    // 7 was never re-announced in between — proved by order, not by a quiet window on a clock.
    mock.push(appricot_core::SurfaceEvent::ClipboardRequested);
    read_until(&mut second, "the sentinel ClipboardAsk", |b| {
        matches!(b, Body::ClipboardAsk(_))
    })
    .await;
}

#[tokio::test]
async fn the_reply_names_the_grace_actually_honoured() {
    let _grace = grace(250).await;
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = connect_with_retry(&server.addr).await;
    send(&mut ws, &hello(None)).await;
    let Body::HelloReply(reply) = read_body(&mut ws).await else {
        panic!("expected a HelloReply");
    };
    // The knob overrides the wait and the advertisement together: what the reply promises is
    // what the park will honour, or the resume the client plans around the reply is a lie.
    assert_eq!(reply.resume_grace_ms, Some(250));
}
