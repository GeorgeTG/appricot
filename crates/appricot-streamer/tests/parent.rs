//! Integration tests: the dialog parent on the wire (`SurfaceNew.parent_id`).
//!
//! A dialog is an ordinary toplevel whose announcement names another toplevel in
//! `parent_id`; a plain toplevel's names nothing. These tests pin the mapping end to end
//! over the real loopback WebSocket — the announcement, and the re-announcement a resume
//! makes — with the mock backend standing in for the display.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use appricot_core::Size;
use appricot_proto::wire::Body;

use common::harness::{handshake, read_body_skipping_frames, spawn_mock_server};

/// Reads the next SurfaceNew of `surface_id`, looking past frames, panicking with what
/// arrived instead.
async fn expect_surface_new(
    ws: &mut common::harness::Client,
    surface_id: u32,
) -> appricot_proto::wire::SurfaceNew {
    match read_body_skipping_frames(ws).await {
        Body::SurfaceNew(m) if m.surface_id == surface_id => m,
        Body::Bye(bye) => panic!(
            "got Bye({:?}, {:?}) while waiting for SurfaceNew",
            bye.reason, bye.text
        ),
        other => panic!("expected SurfaceNew of {surface_id}, got {other:?}"),
    }
}

#[tokio::test]
async fn a_dialog_announces_its_parent_id_and_a_plain_toplevel_none() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, reply) = handshake(&server.addr, None).await;
    assert!(!reply.resumed);

    // The plain toplevel first, so the dialog's parent is tracked when the dialog comes.
    mock.create_surface(1, Size::new(300, 200));
    let plain = expect_surface_new(&mut ws, 1).await;
    assert_eq!(plain.parent_id, None, "a plain toplevel names no parent");

    mock.create_dialog(2, 1, Size::new(200, 120));
    let dialog = expect_surface_new(&mut ws, 2).await;
    assert_eq!(
        dialog.role,
        appricot_proto::wire::Role::Toplevel as i32,
        "a dialog is an ordinary toplevel"
    );
    assert_eq!(dialog.parent_id, Some(1), "the dialog names its parent");
}

#[tokio::test]
async fn a_resume_re_announces_the_dialog_with_its_parent_id() {
    let (server, mock) = spawn_mock_server().await;
    let (mut first, reply) = handshake(&server.addr, None).await;
    let resume_serial = reply.resume_serial.expect("a session can be resumed");

    mock.create_surface(1, Size::new(300, 200));
    expect_surface_new(&mut first, 1).await;
    mock.create_dialog(2, 1, Size::new(200, 120));
    expect_surface_new(&mut first, 2).await;

    // Drop the socket without a Bye: the session parks, and a reconnect naming the serial
    // resumes it.
    drop(first);

    let (mut second, resumed) = handshake(&server.addr, Some(resume_serial)).await;
    assert!(resumed.resumed, "the parked session resumes");

    // The whole window set is re-announced in creation order, parents included.
    let plain = expect_surface_new(&mut second, 1).await;
    assert_eq!(plain.parent_id, None);
    let dialog = expect_surface_new(&mut second, 2).await;
    assert_eq!(
        dialog.parent_id,
        Some(1),
        "the resume re-announces the dialog's parent"
    );
}
