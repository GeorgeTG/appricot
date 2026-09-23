//! Cursor serials are the session's own, and a resume re-announces in a fixed order.
//!
//! - Every cursor image the client gets carries a serial above the last one, even an image
//!   the app showed before. A backend's serial names a cursor object, and toolkits return to
//!   cached cursors all the time: forwarded as is, the client would drop the arrow after the
//!   I-beam for good.
//! - After `HelloReply(resumed)` the server sends every living surface's `SurfaceNew`, then
//!   exactly one `CursorImage` or `CursorGone`, then the frames (v0.md §7).

// The shared mock and the harness carry helpers this binary does not exercise.
#[allow(dead_code)]
mod common;
#[allow(dead_code)]
mod harness;

use appricot_core::{CursorImage, Point, Size, SurfaceEvent};
use appricot_proto::wire::Body;

use common::MockBackend;
use harness::{Client, expect_quiet, read_body, serve, start};

/// A 2x1 cursor whose pixels are `fill`, under the backend's serial `serial`.
fn cursor(serial: u32, fill: u8) -> SurfaceEvent {
    SurfaceEvent::CursorChanged {
        cursor: CursorImage {
            serial,
            size: Size::new(2, 1),
            hotspot: Point::new(1, 0),
            argb: vec![fill; 8],
        },
    }
}

/// Reads the next body, which must be a CursorImage; returns its serial and first byte.
async fn expect_cursor(ws: &mut Client) -> (u32, u8) {
    match read_body(ws).await {
        Body::CursorImage(image) => (image.serial, image.argb_premultiplied[0]),
        other => panic!("expected a CursorImage, got {other:?}"),
    }
}

#[tokio::test]
async fn a_cursor_the_app_returns_to_is_sent_again_under_a_higher_serial() {
    let (backend, mock) = MockBackend::pair();
    let addr = serve(backend).await;
    let (mut ws, _reply) = start(addr, None).await;

    // The arrow (created early, serial 5), the I-beam (serial 9), then the arrow again.
    mock.push(cursor(5, 0xAA));
    mock.push(cursor(9, 0xBB));
    mock.push(cursor(5, 0xAA));
    let arrow = expect_cursor(&mut ws).await;
    let beam = expect_cursor(&mut ws).await;
    let back = expect_cursor(&mut ws).await;
    assert_eq!((arrow.1, beam.1, back.1), (0xAA, 0xBB, 0xAA));
    assert!(
        arrow.0 < beam.0 && beam.0 < back.0,
        "serials rise: {arrow:?} {beam:?} {back:?}"
    );

    // The same image again changes nothing the host draws, and is not sent.
    mock.push(cursor(5, 0xAA));
    expect_quiet(&mut ws, 200, "an identical cursor image is not resent").await;
}

#[tokio::test]
async fn a_resume_sends_the_windows_then_the_cursor_then_the_frames() {
    let (backend, mock) = MockBackend::pair();
    let addr = serve(backend).await;
    let (mut first, reply) = start(addr, None).await;
    let resume_serial = reply.resume_serial.expect("a session can be resumed");

    mock.create_surface(1, Size::new(120, 80));
    assert!(matches!(read_body(&mut first).await, Body::SurfaceNew(_)));
    assert!(matches!(read_body(&mut first).await, Body::Frame(_)));
    mock.push(cursor(3, 0xCC));
    let before = expect_cursor(&mut first).await;
    drop(first);

    let (mut second, reply) = start(addr, Some(resume_serial)).await;
    assert!(reply.resumed, "the parked session resumes");
    match read_body(&mut second).await {
        Body::SurfaceNew(m) => assert_eq!(m.surface_id, 1),
        other => panic!("expected the window set first, got {other:?}"),
    }
    // Exactly one cursor message ends the re-announcement: the current image, under a serial
    // above every one the client has seen.
    let after = expect_cursor(&mut second).await;
    assert_eq!(after.1, 0xCC);
    assert!(after.0 > before.0, "{after:?} after {before:?}");
    match read_body(&mut second).await {
        Body::Frame(frame) => assert!(frame.full_redraw),
        other => panic!("expected the full redraw, got {other:?}"),
    }
}

#[tokio::test]
async fn a_resume_with_no_cursor_known_says_cursor_gone() {
    let (backend, mock) = MockBackend::pair();
    let addr = serve(backend).await;
    let (mut first, reply) = start(addr, None).await;
    let resume_serial = reply.resume_serial.expect("a session can be resumed");
    mock.create_surface(1, Size::new(120, 80));
    assert!(matches!(read_body(&mut first).await, Body::SurfaceNew(_)));
    drop(first);

    let (mut second, reply) = start(addr, Some(resume_serial)).await;
    assert!(reply.resumed, "the parked session resumes");
    assert!(matches!(read_body(&mut second).await, Body::SurfaceNew(_)));
    match read_body(&mut second).await {
        Body::CursorGone(_) => {}
        other => panic!("expected CursorGone, got {other:?}"),
    }
    assert!(matches!(read_body(&mut second).await, Body::Frame(_)));
}
