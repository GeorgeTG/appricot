//! Detach and resume, and the cursor: what a session does while its client is away, and what
//! it tells the client that comes back, in which order.

use appricot_core::{
    CursorImage, MAX_FRAME_CREDITS, Point, Positioner, Rect, Role, Session, SessionEvent, Size,
    SurfaceEvent, SurfaceId,
};
use appricot_proto::limits::{AppId, Title};

fn created(id: u32, width: u32, height: u32) -> SurfaceEvent {
    SurfaceEvent::Created {
        id: SurfaceId::new(id),
        role: Role::Toplevel,
        size: Size::new(width, height),
        parent: None,
    }
}

fn popup(id: u32, parent: u32) -> SurfaceEvent {
    SurfaceEvent::Created {
        id: SurfaceId::new(id),
        role: Role::Popup {
            parent: SurfaceId::new(parent),
            positioner: Positioner::at(Point::new(5, 5), Size::new(80, 40)),
        },
        size: Size::new(80, 40),
        parent: None,
    }
}

/// A cursor image of `width` x 1 pixels, every byte `fill`, under the backend's `serial`.
fn cursor(serial: u32, width: u32, fill: u8) -> SurfaceEvent {
    let len = usize::try_from(width * 4).expect("small");
    SurfaceEvent::CursorChanged {
        cursor: CursorImage {
            serial,
            size: Size::new(width, 1),
            hotspot: Point::new(0, 0),
            argb: vec![fill; len],
        },
    }
}

/// The serials and first bytes of every cursor event in `out`, in order.
fn cursors(out: &[SessionEvent]) -> Vec<(u32, u8)> {
    out.iter()
        .filter_map(|event| match event {
            SessionEvent::CursorChanged { cursor } => Some((cursor.serial, cursor.argb[0])),
            _ => None,
        })
        .collect()
}

#[test]
fn cursor_serials_are_the_sessions_own_and_rise_even_for_an_image_seen_before() {
    let mut s = Session::new();
    let mut out = Vec::new();
    // The arrow (backend serial 5), the I-beam (9), the arrow again (5).
    s.apply_event(cursor(5, 2, 0xAA), &mut out);
    s.apply_event(cursor(9, 2, 0xBB), &mut out);
    s.apply_event(cursor(5, 2, 0xAA), &mut out);
    assert_eq!(cursors(&out), vec![(1, 0xAA), (2, 0xBB), (3, 0xAA)]);
}

#[test]
fn an_image_identical_to_the_last_one_is_not_forwarded() {
    let mut s = Session::new();
    let mut out = Vec::new();
    s.apply_event(cursor(5, 2, 0xAA), &mut out);
    s.apply_event(cursor(6, 2, 0xAA), &mut out);
    assert_eq!(cursors(&out), vec![(1, 0xAA)]);
}

#[test]
fn a_cursor_the_wire_cannot_carry_is_dropped() {
    let mut s = Session::new();
    let mut out = Vec::new();
    // Wider than MAX_CURSOR_WIDTH.
    s.apply_event(cursor(1, 129, 0xAA), &mut out);
    // A pixel buffer that is not width * height * 4 bytes.
    s.apply_event(
        SurfaceEvent::CursorChanged {
            cursor: CursorImage {
                serial: 2,
                size: Size::new(2, 2),
                hotspot: Point::new(0, 0),
                argb: vec![0; 3],
            },
        },
        &mut out,
    );
    assert!(out.is_empty());
    // A later image that fits still gets the first serial.
    s.apply_event(cursor(3, 128, 0xCC), &mut out);
    assert_eq!(cursors(&out), vec![(1, 0xCC)]);
}

#[test]
fn a_resume_announces_the_windows_as_they_stand_then_exactly_one_cursor() {
    let mut s = Session::new();
    let mut out = Vec::new();
    s.apply_event(created(1, 400, 300), &mut out);
    s.apply_event(popup(2, 1), &mut out);
    s.apply_event(created(3, 200, 100), &mut out);
    s.apply_event(cursor(7, 2, 0xAA), &mut out);
    let last_seen = cursors(&out).last().copied().expect("one cursor").0;
    out.clear();

    s.detach();
    // The client is away; the app carries on.
    s.apply_event(
        SurfaceEvent::Destroyed {
            id: SurfaceId::new(3),
        },
        &mut out,
    );
    s.apply_event(
        SurfaceEvent::Metadata {
            id: SurfaceId::new(1),
            title: Title::new("Notes").expect("short"),
            app_id: AppId::new("notes").expect("short"),
        },
        &mut out,
    );
    s.apply_event(
        SurfaceEvent::Resized {
            id: SurfaceId::new(2),
            size: Size::new(120, 60),
        },
        &mut out,
    );
    s.apply_event(created(4, 50, 50), &mut out);
    s.apply_event(cursor(8, 2, 0xBB), &mut out);
    s.apply_event(SurfaceEvent::ClipboardRequested, &mut out);
    s.apply_event(
        SurfaceEvent::FocusRequested {
            id: SurfaceId::new(1),
        },
        &mut out,
    );
    assert!(out.is_empty(), "a detached session says nothing");
    assert_eq!(s.plan_frame(SurfaceId::new(1)), None, "and plans nothing");

    s.resume(&mut out);
    assert_eq!(out.len(), 4, "three windows, then one cursor: {out:?}");
    match &out[0] {
        SessionEvent::SurfaceNew {
            id, title, size, ..
        } => {
            assert_eq!(*id, SurfaceId::new(1));
            assert_eq!(title.as_str(), "Notes");
            assert_eq!(*size, Size::new(400, 300));
        }
        other => panic!("expected surface 1, got {other:?}"),
    }
    match &out[1] {
        SessionEvent::SurfaceNew {
            id,
            size,
            positioner,
            parent,
            ..
        } => {
            assert_eq!(*id, SurfaceId::new(2));
            assert_eq!(*parent, Some(SurfaceId::new(1)));
            assert_eq!(*size, Size::new(120, 60));
            // The popup is placed at the size it has now, not the size it was created with.
            assert_eq!(
                positioner.map(|p| p.place()),
                Some(Rect::new(5, 5, 120, 60))
            );
        }
        other => panic!("expected surface 2, got {other:?}"),
    }
    assert!(
        matches!(&out[2], SessionEvent::SurfaceNew { id, .. } if *id == SurfaceId::new(4)),
        "{:?}",
        out[2]
    );
    // The cursor the app showed last, under a serial above every one the client saw.
    let resent = cursors(&out);
    assert_eq!(resent.len(), 1);
    assert_eq!(resent[0].1, 0xBB);
    assert!(resent[0].0 > last_seen);
}

#[test]
fn a_resume_with_no_cursor_known_says_so() {
    let mut s = Session::new();
    let mut out = Vec::new();
    s.apply_event(created(1, 40, 30), &mut out);
    s.detach();
    out.clear();
    s.resume(&mut out);
    assert_eq!(out.len(), 2);
    assert_eq!(out[1], SessionEvent::CursorGone);
}

#[test]
fn a_resume_restarts_every_surfaces_frames_and_old_acks_change_nothing() {
    let mut s = Session::new();
    let id = SurfaceId::new(1);
    s.apply_event(created(1, 40, 30), &mut Vec::new());
    let credits = u32::try_from(MAX_FRAME_CREDITS).expect("tiny");
    for sequence in 1..=credits {
        assert_eq!(s.plan_frame(id).expect("a free credit").sequence, sequence);
        s.apply_event(
            SurfaceEvent::Damaged {
                id,
                rect: Rect::new(0, 0, 1, 1),
            },
            &mut Vec::new(),
        );
    }
    assert_eq!(s.plan_frame(id), None, "every credit is spent");
    s.detach();
    s.resume(&mut Vec::new());

    let plan = s
        .plan_frame(id)
        .expect("a fresh connection holds nothing in flight");
    assert_eq!(plan.sequence, 1);
    assert!(plan.full_redraw);
    assert_eq!(plan.rects, vec![Rect::new(0, 0, 40, 30)]);
    // An ack from the old connection names a sequence the new one never sent: nothing.
    s.frame_ack(id, credits);
    for sequence in 2..=credits {
        s.apply_event(
            SurfaceEvent::Damaged {
                id,
                rect: Rect::new(0, 0, 1, 1),
            },
            &mut Vec::new(),
        );
        assert_eq!(s.plan_frame(id).expect("a free credit").sequence, sequence);
    }
    s.apply_event(
        SurfaceEvent::Damaged {
            id,
            rect: Rect::new(0, 0, 1, 1),
        },
        &mut Vec::new(),
    );
    assert_eq!(s.plan_frame(id), None, "the stale ack freed no credit");
}
