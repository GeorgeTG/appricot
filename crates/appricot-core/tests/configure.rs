//! The configure queue, from the session's public side: every proposal the host makes is
//! answered at most once, with the size the app really took, and a size change nobody
//! proposed is reported as the app's own.

use appricot_core::{
    ConfigureSerial, MAX_PENDING_CONFIGURES, Point, Positioner, Rect, Role, Session, SessionEvent,
    Size, SurfaceEvent, SurfaceId,
};

const ID: SurfaceId = SurfaceId::new(1);

/// A session with one 400x300 toplevel, id 1, and its announcement consumed.
fn one_window() -> Session {
    let mut s = Session::new();
    s.apply_event(
        SurfaceEvent::Created {
            id: ID,
            role: Role::Toplevel,
            size: Size::new(400, 300),
            parent: None,
        },
        &mut Vec::new(),
    );
    s
}

fn resized(width: u32, height: u32) -> SurfaceEvent {
    SurfaceEvent::Resized {
        id: ID,
        size: Size::new(width, height),
    }
}

fn acked(serial: ConfigureSerial, width: u32, height: u32) -> SessionEvent {
    SessionEvent::ConfigureAcked {
        id: ID,
        serial,
        size: Size::new(width, height),
    }
}

fn own(width: u32, height: u32) -> SessionEvent {
    SessionEvent::Resized {
        id: ID,
        size: Size::new(width, height),
    }
}

/// Proposes `width` x `height` for surface 1; the surface lives, so a serial comes back.
fn propose(
    s: &mut Session,
    width: u32,
    height: u32,
    out: &mut Vec<SessionEvent>,
) -> ConfigureSerial {
    s.configure(ID, Size::new(width, height), out)
        .expect("the surface lives")
}

#[test]
fn a_burst_answered_in_order_acks_each_proposal_with_its_own_size() {
    let mut s = one_window();
    let mut out = Vec::new();
    let c1 = propose(&mut s, 800, 600, &mut out);
    let c2 = propose(&mut s, 900, 700, &mut out);
    assert!(out.is_empty(), "nothing is acked before the app answers");
    s.apply_event(resized(800, 600), &mut out);
    s.apply_event(resized(900, 700), &mut out);
    assert_eq!(out, vec![acked(c1, 800, 600), acked(c2, 900, 700)]);
}

#[test]
fn a_proposal_replaced_before_the_app_answers_is_answered_by_the_newer_answer() {
    let mut s = one_window();
    let mut out = Vec::new();
    let _c1 = propose(&mut s, 800, 600, &mut out);
    let c2 = propose(&mut s, 900, 700, &mut out);
    // The app skipped straight to the newest proposal: one ack, naming it, answers both.
    s.apply_event(resized(900, 700), &mut out);
    assert_eq!(out, vec![acked(c2, 900, 700)]);
    // Nothing waits any more, so the next change is the app's own.
    out.clear();
    s.apply_event(resized(800, 600), &mut out);
    assert_eq!(out, vec![own(800, 600)]);
}

#[test]
fn an_answer_matching_no_proposal_acks_the_newest_with_the_size_taken() {
    let mut s = one_window();
    let mut out = Vec::new();
    let _c1 = propose(&mut s, 800, 600, &mut out);
    let c2 = propose(&mut s, 900, 700, &mut out);
    // The app clamped the first proposal to its own maximum height.
    s.apply_event(resized(800, 500), &mut out);
    s.apply_event(resized(900, 500), &mut out);
    assert_eq!(out, vec![acked(c2, 800, 500), own(900, 500)]);
    assert_eq!(s.surface(ID).expect("lives").size(), Size::new(900, 500));
}

#[test]
fn a_proposal_of_the_size_already_taken_is_acked_at_once() {
    let mut s = one_window();
    let mut out = Vec::new();
    let c1 = propose(&mut s, 400, 300, &mut out);
    assert_eq!(out, vec![acked(c1, 400, 300)]);
    // A backend that reports the unchanged size as well adds nothing.
    out.clear();
    s.apply_event(resized(400, 300), &mut out);
    assert!(out.is_empty());
    // The no-op left nothing waiting: the app's next change is its own, not an ack of c1.
    s.apply_event(resized(500, 300), &mut out);
    assert_eq!(out, vec![own(500, 300)]);
}

#[test]
fn a_proposal_the_app_clamps_back_to_its_size_is_acked_with_that_size() {
    let mut s = one_window();
    let mut out = Vec::new();
    // 400x300 is the app's minimum: 300x200 changes nothing, and the backend says so.
    let c1 = propose(&mut s, 300, 200, &mut out);
    assert!(out.is_empty());
    s.apply_event(resized(400, 300), &mut out);
    assert_eq!(out, vec![acked(c1, 400, 300)]);
}

#[test]
fn the_same_size_behind_another_proposal_waits_for_its_own_answer() {
    let mut s = one_window();
    let mut out = Vec::new();
    let c1 = propose(&mut s, 800, 600, &mut out);
    let c2 = propose(&mut s, 400, 300, &mut out);
    assert!(out.is_empty(), "there and back again is not a no-op");
    s.apply_event(resized(800, 600), &mut out);
    s.apply_event(resized(400, 300), &mut out);
    assert_eq!(out, vec![acked(c1, 800, 600), acked(c2, 400, 300)]);
}

#[test]
fn the_waiting_queue_is_bounded() {
    let mut s = one_window();
    let mut out = Vec::new();
    let count = u32::try_from(MAX_PENDING_CONFIGURES).expect("small") + 2;
    let mut newest = None;
    for step in 1..=count {
        newest = Some(propose(&mut s, 500 + step, 300, &mut out));
    }
    // The first two proposals were dropped: an answer of the first size matches nothing
    // still waiting, so it acks the newest, and all of them with it.
    s.apply_event(resized(501, 300), &mut out);
    let newest = newest.expect("proposed");
    assert_eq!(out, vec![acked(newest, 501, 300)]);
}

#[test]
fn an_unknown_surface_spends_no_serial_and_says_nothing() {
    let mut s = one_window();
    let mut out = Vec::new();
    let first = propose(&mut s, 500, 300, &mut out);
    assert_eq!(
        s.configure(SurfaceId::new(9), Size::new(1, 1), &mut out),
        None
    );
    let second = propose(&mut s, 600, 300, &mut out);
    assert_eq!(second.get(), first.get() + 1);
    assert!(out.is_empty());
}

#[test]
fn a_proposal_waiting_across_a_resume_is_forgotten() {
    let mut s = one_window();
    let mut out = Vec::new();
    let _c1 = propose(&mut s, 800, 600, &mut out);
    s.detach();
    s.resume(&mut out);
    out.clear();
    // The connection that proposed it is gone; the new one knows the size from the resume.
    s.apply_event(resized(800, 600), &mut out);
    assert_eq!(out, vec![own(800, 600)]);
}

#[test]
fn an_answer_that_lands_while_detached_is_folded_into_the_resume() {
    let mut s = one_window();
    let mut out = Vec::new();
    let _c1 = propose(&mut s, 800, 600, &mut out);
    s.detach();
    s.apply_event(resized(800, 600), &mut out);
    assert!(out.is_empty(), "a detached session says nothing");
    s.resume(&mut out);
    match out.first() {
        Some(SessionEvent::SurfaceNew { size, .. }) => assert_eq!(*size, Size::new(800, 600)),
        other => panic!("expected the re-announcement first, got {other:?}"),
    }
}

#[test]
fn sizes_past_the_wire_caps_are_cut_to_them() {
    let mut s = Session::new();
    let mut out = Vec::new();
    s.apply_event(
        SurfaceEvent::Created {
            id: ID,
            role: Role::Toplevel,
            size: Size::new(2000, 1300),
            parent: None,
        },
        &mut out,
    );
    s.apply_event(
        SurfaceEvent::Created {
            id: SurfaceId::new(2),
            role: Role::Popup {
                parent: ID,
                positioner: Positioner::at(Point::new(0, 0), Size::new(2500, 40)),
            },
            size: Size::new(2500, 40),
            parent: None,
        },
        &mut out,
    );
    s.apply_event(resized(4000, 100), &mut out);
    s.apply_event(
        SurfaceEvent::ResizeRequested {
            id: ID,
            size: Size::new(5000, 5000),
        },
        &mut out,
    );
    let sizes: Vec<Size> = out
        .iter()
        .map(|event| match event {
            SessionEvent::SurfaceNew { size, .. }
            | SessionEvent::Resized { size, .. }
            | SessionEvent::ResizeAsk { size, .. } => *size,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        sizes,
        vec![
            Size::new(1920, 1200),
            Size::new(1920, 40),
            Size::new(1920, 100),
            Size::new(1920, 1200),
        ]
    );
    match &out[1] {
        SessionEvent::SurfaceNew { positioner, .. } => assert_eq!(
            positioner.map(|p| p.size),
            Some(Size::new(1920, 40)),
            "a popup's positioner is cut too"
        ),
        other => panic!("expected the popup, got {other:?}"),
    }

    // A proposal past the caps is cut before it waits, so the backend's raw report of the
    // same oversized geometry answers it.
    out.clear();
    let c1 = propose(&mut s, 3000, 3000, &mut out);
    s.apply_event(resized(3000, 3000), &mut out);
    assert_eq!(out, vec![acked(c1, 1920, 1200)]);

    // And no frame ever reaches past them.
    let plan = s.plan_frame(ID).expect("the first frame");
    for rect in plan.rects {
        assert!(
            Rect::new(0, 0, 1920, 1200).intersection(rect) == Some(rect),
            "{rect:?}"
        );
    }
}
