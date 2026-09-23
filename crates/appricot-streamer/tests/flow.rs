//! The frame path under pressure: a client that never pauses, credits that run out, captures
//! that break their contract, and input aimed at windows the client was never shown.
//!
//! Absence is proved by order, never by a quiet window on a clock: a sentinel the server must
//! answer — the app's paste request, or input the backend records — arrives after whatever did
//! not happen would have.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use std::time::Duration;

use appricot_core::{Rect, Size, SurfaceEvent, SurfaceId};
use appricot_proto::wire::Body;

use common::harness::{
    Client, ack, envelope, expect_frame, expect_surface_new, handshake, read_body, read_until,
    send, spawn_mock_server,
};
use common::{CaptureFault, Input, MockHandle};

/// The union of a frame's tile rectangles, as `(x, y, width, height)`.
fn covered(frame: &appricot_proto::wire::Frame) -> (i32, i32, u32, u32) {
    let union = frame
        .tiles
        .iter()
        .map(|tile| {
            let r = tile.rect.expect("every tile carries a rect");
            Rect::new(r.x, r.y, r.width, r.height)
        })
        .reduce(Rect::union)
        .expect("the frame carries tiles");
    (
        union.origin.x,
        union.origin.y,
        union.size.width,
        union.size.height,
    )
}

/// Pushes the app's paste request and reads up to its `ClipboardAsk`: anything the feed carried
/// before it has been handled, and nothing else may arrive first.
async fn sentinel(ws: &mut Client, mock: &MockHandle) {
    mock.push(SurfaceEvent::ClipboardRequested);
    read_until(ws, "the sentinel ClipboardAsk", |b| {
        matches!(b, Body::ClipboardAsk(_))
    })
    .await;
}

#[tokio::test]
async fn a_client_that_talks_without_pause_does_not_starve_the_frames() {
    const MOVES: usize = 100;

    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;
    mock.create_surface(7, Size::new(120, 80));
    expect_surface_new(&mut ws, 7).await;
    let first = expect_frame(&mut ws, 7).await;
    ack(&mut ws, 7, first.sequence).await;

    // Every pointer move now repaints the window under it, and costs the backend 5 ms, as a
    // display round trip does. All the moves are sent at once, so the server's socket has
    // input ready the whole time the backend works through them.
    mock.damage_on_motion(Duration::from_millis(5));
    for x in 0..MOVES {
        send(
            &mut ws,
            &envelope(Body::PointerMove(appricot_proto::wire::PointerMove {
                surface_id: 7,
                x: i32::try_from(x).expect("a small coordinate"),
                y: 0,
            })),
        )
        .await;
    }

    // Count the frames that arrive while the moves are still being handled. The acks this test
    // could send would queue behind the moves, so the credits allow four; a feed starved by the
    // busy socket allows none until the moves are done (measured 2026-09-22: 169 moves, 2
    // frames).
    let mut during = 0;
    while mock.motions() < MOVES {
        match tokio::time::timeout(Duration::from_millis(50), read_body(&mut ws)).await {
            Ok(Body::Frame(frame)) => {
                assert_eq!(frame.surface_id, 7);
                if mock.motions() < MOVES {
                    during += 1;
                }
            }
            Ok(Body::CursorImage(_) | Body::CursorGone(_)) | Err(_) => {}
            Ok(other) => panic!("expected frames, got {other:?}"),
        }
    }
    assert!(
        during >= 3,
        "only {during} frames arrived while the pointer moved: the feed starved"
    );
}

#[tokio::test]
async fn frames_stop_at_the_credit_limit_and_damage_coalesces_until_an_ack() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;
    mock.create_surface(7, Size::new(200, 200));
    expect_surface_new(&mut ws, 7).await;
    let first = expect_frame(&mut ws, 7).await;
    assert_eq!(first.sequence, 1);

    // Nothing is acked. Three damages, one at a time, take the other three credits.
    for (sequence, x) in [(2, 0), (3, 10), (4, 20)] {
        mock.push(SurfaceEvent::Damaged {
            id: SurfaceId::new(7),
            rect: Rect::new(x, 0, 5, 5),
        });
        let frame = expect_frame(&mut ws, 7).await;
        assert_eq!(frame.sequence, sequence);
    }

    // No credit is left: two more damages are held and coalesced, not sent.
    mock.push(SurfaceEvent::Damaged {
        id: SurfaceId::new(7),
        rect: Rect::new(100, 100, 10, 10),
    });
    mock.push(SurfaceEvent::Damaged {
        id: SurfaceId::new(7),
        rect: Rect::new(110, 110, 10, 10),
    });
    sentinel(&mut ws, &mock).await;

    // One ack frees one credit: exactly one frame, carrying both damages.
    ack(&mut ws, 7, 1).await;
    let coalesced = expect_frame(&mut ws, 7).await;
    assert_eq!(coalesced.sequence, 5);
    assert!(!coalesced.full_redraw);
    assert_eq!(covered(&coalesced), (100, 100, 20, 20));

    // And no other: the damage went out once.
    sentinel(&mut ws, &mock).await;
}

#[tokio::test]
async fn a_capture_that_breaks_its_contract_skips_the_tile_not_the_session() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    // The first frame of a 300x300 window is four tiles. The first capture comes back empty
    // (the encoder refuses it), the second a pixel too narrow (it does not match its tile).
    mock.fail_next_captures(&[CaptureFault::Empty, CaptureFault::Narrower]);
    mock.create_surface(7, Size::new(300, 300));
    expect_surface_new(&mut ws, 7).await;
    let first = expect_frame(&mut ws, 7).await;
    assert_eq!(
        first.tiles.len(),
        2,
        "the two broken tiles are skipped, the rest are sent"
    );
    for tile in &first.tiles {
        let r = tile.rect.expect("every tile carries a rect");
        assert_eq!(
            tile.data.len(),
            4 * r.width as usize * r.height as usize,
            "a RAW tile carries exactly the pixels its rect names"
        );
    }

    // No Bye: the session goes on, and the next damage is framed as usual.
    ack(&mut ws, 7, first.sequence).await;
    mock.push(SurfaceEvent::Damaged {
        id: SurfaceId::new(7),
        rect: Rect::new(0, 0, 10, 10),
    });
    let next = expect_frame(&mut ws, 7).await;
    assert_eq!(next.sequence, 2);
    assert_eq!(covered(&next), (0, 0, 10, 10));
}

#[tokio::test]
async fn input_for_a_surface_never_announced_is_dropped() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;
    mock.create_surface(1, Size::new(100, 100));
    expect_surface_new(&mut ws, 1).await;

    // Surface 2 was never announced to this client; the backend would take input for it all
    // the same, so the streamer must not pass any on.
    let unannounced = [
        Body::PointerMove(appricot_proto::wire::PointerMove {
            surface_id: 2,
            x: 1,
            y: 1,
        }),
        Body::PointerButton(appricot_proto::wire::PointerButton {
            surface_id: 2,
            button: 1,
            pressed: true,
        }),
        Body::PointerAxis(appricot_proto::wire::PointerAxis {
            surface_id: 2,
            steps_x: 0,
            steps_y: 1,
        }),
        Body::FocusNotify(appricot_proto::wire::FocusNotify { surface_id: 2 }),
    ];
    for body in unannounced {
        send(&mut ws, &envelope(body)).await;
    }

    // The sentinel: focus on the announced surface. Handled in order, it arrives alone.
    send(
        &mut ws,
        &envelope(Body::FocusNotify(appricot_proto::wire::FocusNotify {
            surface_id: 1,
        })),
    )
    .await;
    assert_eq!(mock.wait_input(1).await, vec![Input::Focus { surface: 1 }]);
}
