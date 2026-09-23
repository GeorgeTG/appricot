//! A planned frame that ends up with no tile goes back to the session (contract C2).
//!
//! When every capture of a frame fails, nothing goes out, and the plan is handed back as if it
//! had never been made: its credit, its sequence, its damage and the full redraw it owed. The
//! next frame carries the next sequence the client has not seen, and all four credits still
//! work.
//!
//! Absence is proved by order, never by a quiet window on a clock: the app's paste request is
//! a sentinel the server must answer, and it arrives after whatever did not happen would have.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use appricot_core::{MAX_FRAME_CREDITS, Rect, Size, SurfaceEvent, SurfaceId};
use appricot_proto::wire::Body;

use common::harness::{
    Client, ack, expect_frame, expect_surface_new, handshake, read_until, spawn_mock_server,
    wait_until,
};
use common::{CaptureFault, MockHandle};

/// Pushes the app's paste request and reads up to its `ClipboardAsk`. Anything the feed carried
/// before it has been handled by then; a `Frame` arriving first fails the test.
async fn sentinel(ws: &mut Client, mock: &MockHandle) {
    mock.push(SurfaceEvent::ClipboardRequested);
    read_until(ws, "the sentinel ClipboardAsk", |b| {
        matches!(b, Body::ClipboardAsk(_))
    })
    .await;
}

/// Damages a small corner of surface 7.
fn damage(mock: &MockHandle, x: i32) {
    mock.push(SurfaceEvent::Damaged {
        id: SurfaceId::new(7),
        rect: Rect::new(x, 0, 5, 5),
    });
}

#[tokio::test]
async fn a_frame_whose_every_capture_fails_is_rolled_back() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    // The first frame of a 300x300 window is four tiles, and all four captures break their
    // contract: two come back empty (the encoder refuses them), two a pixel too narrow.
    mock.fail_next_captures(&[
        CaptureFault::Empty,
        CaptureFault::Narrower,
        CaptureFault::Empty,
        CaptureFault::Narrower,
    ]);
    mock.create_surface(7, Size::new(300, 300));
    expect_surface_new(&mut ws, 7).await;

    // No frame with zero tiles went out: the sentinel's ask is the next message.
    sentinel(&mut ws, &mock).await;

    // The sentinel's feed item pumps the frames again, and the plan that came back is the one
    // that goes out: the same sequence 1, still the full redraw, still the whole window.
    let first = expect_frame(&mut ws, 7).await;
    assert_eq!(first.sequence, 1, "the aborted frame spent no sequence");
    assert!(first.full_redraw, "the full redraw is still owed");
    assert_eq!(first.tiles.len(), 4, "the whole window, every tile");

    // No ack is sent. The failed plan returned its credit, so all the credits are still there:
    // three more damages give three more frames, in sequence.
    let credits = u32::try_from(MAX_FRAME_CREDITS).expect("a small count");
    for (sequence, x) in (2..=credits).zip([0, 10, 20]) {
        damage(&mock, x);
        let frame = expect_frame(&mut ws, 7).await;
        assert_eq!(frame.sequence, sequence);
        assert!(!frame.full_redraw);
    }

    // The cap still holds: one more damage is held until an ack frees a credit.
    damage(&mock, 100);
    sentinel(&mut ws, &mock).await;
    ack(&mut ws, 7, first.sequence).await;
    let next = expect_frame(&mut ws, 7).await;
    assert_eq!(next.sequence, credits + 1);
}

#[tokio::test]
async fn a_rolled_back_frame_keeps_the_damage_it_carried() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;
    mock.create_surface(7, Size::new(300, 300));
    expect_surface_new(&mut ws, 7).await;
    let first = expect_frame(&mut ws, 7).await;
    ack(&mut ws, 7, first.sequence).await;

    // A small damage fits one tile; its one capture fails. The sentinel waits until the
    // backend took the damage, so the two travel in separate feed items and the failed frame
    // is planned before the sentinel's.
    mock.fail_next_captures(&[CaptureFault::Narrower]);
    damage(&mock, 40);
    wait_until("the backend took the damage", || mock.queued() == 0).await;
    sentinel(&mut ws, &mock).await;

    // The damage came back with the plan, and goes out at the next pump, under the next
    // sequence and as a partial frame.
    let second = expect_frame(&mut ws, 7).await;
    assert_eq!(second.sequence, 2, "the aborted frame spent no sequence");
    assert!(!second.full_redraw);
    assert_eq!(second.tiles.len(), 1);
    let rect = second.tiles[0].rect.expect("every tile carries a rect");
    assert_eq!((rect.x, rect.y, rect.width, rect.height), (40, 0, 5, 5));
}
