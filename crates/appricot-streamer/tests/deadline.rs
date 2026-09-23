//! The handshake deadline: a socket that upgrades and says nothing must not hold the one slot.
//!
//! The deadline (docs/protocol/v0.md §2, `HANDSHAKE_TIMEOUT_MS`) is 5 s; the tests shorten it
//! with `appricot_streamer::config::set_handshake_timeout_ms`, which is process-wide, so every
//! test here holds one lock and restores the default before it ends.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use std::time::Duration;

use appricot_core::Size;
use appricot_proto::wire::ByeReason;
use futures_util::SinkExt;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use appricot_streamer::config::set_handshake_timeout_ms;
use common::harness::{
    connect, expect_bye, expect_frame, expect_surface_new, handshake, spawn_mock_server,
};

/// One lock for every test that touches the process-wide deadline override.
static DEADLINE_KNOB: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Holds the knob for a test and sets the deadline; the default comes back when the guard
/// drops, even when the test fails.
struct Deadline(#[allow(dead_code)] tokio::sync::MutexGuard<'static, ()>);

impl Drop for Deadline {
    fn drop(&mut self) {
        set_handshake_timeout_ms(0);
    }
}

async fn deadline(ms: u32) -> Deadline {
    let guard = DEADLINE_KNOB.lock().await;
    set_handshake_timeout_ms(ms);
    Deadline(guard)
}

#[tokio::test]
async fn a_silent_socket_is_refused_at_the_deadline_and_the_next_client_is_served() {
    let _deadline = deadline(300).await;
    let (server, _mock) = spawn_mock_server().await;

    let started = Instant::now();
    let mut silent = connect(&server.addr).await;
    expect_bye(&mut silent, ByeReason::ByeProtocolViolation).await;
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "refused after {:?}, before the deadline",
        started.elapsed()
    );

    let (_ws, reply) = handshake(&server.addr, None).await;
    assert!(
        !reply.resumed,
        "the slot went back; the next client is served"
    );
}

#[tokio::test]
async fn pings_do_not_extend_the_deadline() {
    let _deadline = deadline(300).await;
    let (server, _mock) = spawn_mock_server().await;

    let started = Instant::now();
    let mut chatty = connect(&server.addr).await;
    // Pings every 50 ms, never a Hello: the deadline covers the whole wait, pings included.
    let pinger = async {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if chatty.send(Message::Ping(vec![1].into())).await.is_err() {
                return;
            }
            if started.elapsed() > Duration::from_millis(250) {
                return;
            }
        }
    };
    pinger.await;
    expect_bye(&mut chatty, ByeReason::ByeProtocolViolation).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the pings kept the socket past its deadline"
    );
}

#[tokio::test]
async fn a_silent_socket_cannot_starve_a_parked_session() {
    let _deadline = deadline(300).await;
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, reply) = handshake(&server.addr, None).await;
    mock.create_surface(7, Size::new(120, 80));
    expect_surface_new(&mut ws, 7).await;
    expect_frame(&mut ws, 7).await;
    drop(ws);
    let serial = reply.resume_serial.expect("a session can be resumed");

    // A silent peer takes the slot while the session is parked, and is sent away at its
    // deadline; the real client then resumes.
    let mut silent = common::harness::connect_with_retry(&server.addr).await;
    expect_bye(&mut silent, ByeReason::ByeProtocolViolation).await;

    let (mut resumed, reply) = handshake(&server.addr, Some(serial)).await;
    assert!(
        reply.resumed,
        "the parked session survived the silent socket"
    );
    expect_surface_new(&mut resumed, 7).await;
}
