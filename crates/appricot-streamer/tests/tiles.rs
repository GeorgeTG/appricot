//! The streamer omits tiles the client already holds (v0.md §4.3).
//!
//! The spike measured an OpenGL app damaging its whole window on every repaint while almost
//! no pixel changed (docs/spike/findings-2026-09-23.md §2); the answer is sender-side: each
//! tile is compared with what the surface last sent the client for that grid cell, and an
//! identical one is omitted. These pin the contract from the client's side of the socket:
//!
//! - damage whose pixels did not change sends no frame at all, and spends no sequence number;
//! - a one-pixel change inside full-window damage carries only the tile that holds it;
//! - a resume's full redraw carries every tile whatever the connection had cached;
//! - a resize invalidates the cache, so the first frame at the new size is complete.
//!
//! Absence is proved by order, never by a quiet window on a clock: the app's paste request is
//! a sentinel the server must answer, and it arrives after whatever did not happen would
//! have.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use appricot_core::{Rect, Size, SurfaceEvent, SurfaceId};
use appricot_proto::wire::{Body, ConfigureAck, Frame};

use common::harness::{
    Client, TestServer, ack, expect_frame, expect_surface_new, handshake, read_body, read_until,
    spawn_mock_server,
};
use common::{MockBackend, MockHandle};

/// Pushes the app's paste request and reads up to its `ClipboardAsk`. A `Frame` arriving
/// first fails the wait, so one sentinel proves nothing was sent before it; a second also
/// proves nothing was sent by the pump the first sentinel itself triggered.
async fn sentinel(ws: &mut Client, mock: &MockHandle) {
    mock.push(SurfaceEvent::ClipboardRequested);
    read_until(ws, "the sentinel ClipboardAsk", |b| {
        matches!(b, Body::ClipboardAsk(_))
    })
    .await;
}

/// Damages the whole of a `width` x `height` surface 7, then sentinel-proves no frame went
/// out for it.
async fn damage_all_of(ws: &mut Client, mock: &MockHandle, width: u32, height: u32) {
    mock.push(SurfaceEvent::Damaged {
        id: SurfaceId::new(7),
        rect: Rect::new(0, 0, width, height),
    });
    sentinel(ws, mock).await;
}

/// The right and bottom edges the frame's tiles reach.
fn reach(frame: &Frame) -> (u32, u32) {
    frame.tiles.iter().fold((0, 0), |(right, bottom), tile| {
        let rect = tile.rect.expect("every tile carries a rect");
        let x = u32::try_from(rect.x).expect("tiles sit inside the surface");
        let y = u32::try_from(rect.y).expect("tiles sit inside the surface");
        (right.max(x + rect.width), bottom.max(y + rect.height))
    })
}

/// A started session with surface 7, a 300x200 toplevel, whose first frame (two tiles, the
/// whole surface) is drawn and acked — the comparison cache holds every cell — with the
/// resume serial the client holds.
async fn warmed_session() -> (TestServer<MockBackend>, Client, MockHandle, u32) {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, reply) = handshake(&server.addr, None).await;
    mock.create_surface(7, Size::new(300, 200));
    expect_surface_new(&mut ws, 7).await;
    let first = expect_frame(&mut ws, 7).await;
    assert!(
        first.full_redraw,
        "a new surface's first frame redraws it all"
    );
    assert_eq!(first.tiles.len(), 2, "300x200 is two grid cells");
    assert_eq!(reach(&first), (300, 200));
    ack(&mut ws, 7, first.sequence).await;
    let serial = reply.resume_serial.expect("a session can be resumed");
    (server, ws, mock, serial)
}

#[tokio::test]
async fn damage_whose_pixels_did_not_change_sends_no_frame() {
    let (_server, mut ws, mock, _serial) = warmed_session().await;

    // The app damages the whole window and paints exactly what is already there — the OpenGL
    // path's every repaint (findings §2). No frame goes out.
    damage_all_of(&mut ws, &mock, 300, 200).await;
    // And the pump the first sentinel itself triggered sends nothing either.
    sentinel(&mut ws, &mock).await;
}

#[tokio::test]
async fn a_one_pixel_change_inside_full_window_damage_carries_only_its_tile() {
    let (_server, mut ws, mock, _serial) = warmed_session().await;

    // One pixel changes, in the right-hand edge cell (256..299) of the 300-wide surface; the
    // app still damages the whole window.
    mock.repaint(7, Rect::new(260, 10, 1, 1));
    mock.push(SurfaceEvent::Damaged {
        id: SurfaceId::new(7),
        rect: Rect::new(0, 0, 300, 200),
    });
    let frame = expect_frame(&mut ws, 7).await;
    assert_eq!(frame.sequence, 2, "the skipped damage spent no sequence");
    assert!(!frame.full_redraw);
    assert_eq!(frame.tiles.len(), 1, "the unchanged cell is omitted");
    let rect = frame.tiles[0].rect.expect("every tile carries a rect");
    assert_eq!((rect.x, rect.y, rect.width, rect.height), (256, 0, 44, 200));
    ack(&mut ws, 7, frame.sequence).await;

    // The changed cell is cached from this frame now: unchanged full damage sends nothing.
    damage_all_of(&mut ws, &mock, 300, 200).await;
    sentinel(&mut ws, &mock).await;

    // A change in the other cell, and the sequences of the frames that are sent stay gapless:
    // 1, 2, 3, whatever was skipped in between.
    mock.repaint(7, Rect::new(5, 5, 1, 1));
    mock.push(SurfaceEvent::Damaged {
        id: SurfaceId::new(7),
        rect: Rect::new(0, 0, 300, 200),
    });
    let frame = expect_frame(&mut ws, 7).await;
    assert_eq!(frame.sequence, 3);
    assert_eq!(frame.tiles.len(), 1);
    let rect = frame.tiles[0].rect.expect("every tile carries a rect");
    assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 256, 200));
}

#[tokio::test]
async fn a_resume_repaints_every_tile_whatever_the_connection_had_cached() {
    let (server, mut ws, mock, serial) = warmed_session().await;

    // A partial frame replaces the cache's whole-cell entry with a partial one, so the
    // connection holds a mixed cache when its socket dies.
    mock.repaint(7, Rect::new(0, 0, 10, 10));
    mock.push(SurfaceEvent::Damaged {
        id: SurfaceId::new(7),
        rect: Rect::new(0, 0, 10, 10),
    });
    let partial = expect_frame(&mut ws, 7).await;
    assert_eq!(partial.tiles.len(), 1);
    let rect = partial.tiles[0].rect.expect("every tile carries a rect");
    assert_eq!((rect.x, rect.y, rect.width, rect.height), (0, 0, 10, 10));
    ack(&mut ws, 7, partial.sequence).await;

    drop(ws);
    // The park releases held input on its way: once the Blur is recorded, the socket's end
    // has settled into a park.
    mock.wait_input(1).await;

    // The resynchronisation, in v0.md §7's order: the window set, the cursor, then one full
    // redraw at sequence 1 covering the whole surface — the dead connection's cache decided
    // nothing.
    let (mut resumed, reply) = handshake(&server.addr, Some(serial)).await;
    assert!(reply.resumed, "the parked session resumes");
    expect_surface_new(&mut resumed, 7).await;
    assert!(
        matches!(read_body(&mut resumed).await, Body::CursorGone(_)),
        "one cursor message follows the window set (v0.md §7)"
    );
    let frame = expect_frame(&mut resumed, 7).await;
    assert!(frame.full_redraw, "a resume repaints everything");
    assert_eq!(frame.sequence, 1, "the resume restarts the sequences");
    assert_eq!(
        frame.tiles.len(),
        2,
        "every tile of the surface, cache or no cache"
    );
    assert_eq!(reach(&frame), (300, 200));
}

#[tokio::test]
async fn a_resize_invalidates_the_cache_and_the_first_frame_at_the_new_size_is_complete() {
    let (_server, mut ws, mock, _serial) = warmed_session().await;

    // The cache is warm and silent at 300x200; the app then takes a size of its own.
    damage_all_of(&mut ws, &mock, 300, 200).await;
    mock.push(SurfaceEvent::Resized {
        id: SurfaceId::new(7),
        size: Size::new(400, 250),
    });

    // The size is a fact first (v0.md §4.2), as an ack under serial 0.
    match read_body(&mut ws).await {
        Body::ConfigureAck(ConfigureAck { serial, size, .. }) => {
            let size = size.expect("an ack carries a size");
            assert_eq!(serial, 0);
            assert_eq!((size.width, size.height), (400, 250));
        }
        other => panic!("expected the resize's ConfigureAck, got {other:?}"),
    }

    // Then the frame at the new size is complete: every tile of 400x250, nothing omitted
    // against the old size's cache.
    let frame = expect_frame(&mut ws, 7).await;
    assert_eq!(frame.sequence, 2);
    assert!(!frame.full_redraw, "a resize is not a redraw flag");
    assert_eq!(frame.tiles.len(), 2, "400x250 is two grid cells");
    assert_eq!(reach(&frame), (400, 250));
    ack(&mut ws, 7, frame.sequence).await;

    // The cache is warm at the new size now: unchanged damage sends nothing again.
    damage_all_of(&mut ws, &mock, 400, 250).await;
    sentinel(&mut ws, &mock).await;
}
