//! Frames across the surface's life: damage across a resize, a surface with no pixels, a
//! surface that is gone, and a planned frame handed back unsent.

use appricot_core::{Rect, Role, Session, Size, SurfaceEvent, SurfaceId};

const ID: SurfaceId = SurfaceId::new(1);

fn with_window(width: u32, height: u32) -> Session {
    let mut s = Session::new();
    s.apply_event(
        SurfaceEvent::Created {
            id: ID,
            role: Role::Toplevel,
            size: Size::new(width, height),
            parent: None,
        },
        &mut Vec::new(),
    );
    s
}

fn damage(s: &mut Session, x: i32, y: i32, width: u32, height: u32) {
    s.apply_event(
        SurfaceEvent::Damaged {
            id: ID,
            rect: Rect::new(x, y, width, height),
        },
        &mut Vec::new(),
    );
}

fn resize(s: &mut Session, width: u32, height: u32) {
    s.apply_event(
        SurfaceEvent::Resized {
            id: ID,
            size: Size::new(width, height),
        },
        &mut Vec::new(),
    );
}

#[test]
fn damage_then_a_shrink_is_one_frame_of_the_whole_new_size() {
    let mut s = with_window(400, 300);
    s.plan_frame(ID).expect("the first frame");
    damage(&mut s, 350, 250, 50, 50);
    resize(&mut s, 200, 100);
    let plan = s.plan_frame(ID).expect("the resize damaged everything");
    assert!(
        !plan.full_redraw,
        "a resize is new pixels, not a reset of the client"
    );
    assert_eq!(plan.rects, vec![Rect::new(0, 0, 200, 100)]);
}

#[test]
fn damage_then_a_grow_is_one_frame_of_the_whole_new_size() {
    let mut s = with_window(400, 300);
    s.plan_frame(ID).expect("the first frame");
    damage(&mut s, 10, 10, 5, 5);
    resize(&mut s, 640, 480);
    // Damage in the new area, which the old bounds would have dropped, is kept.
    damage(&mut s, 500, 400, 20, 20);
    let plan = s.plan_frame(ID).expect("the resize damaged everything");
    let union = plan.rects.iter().copied().reduce(Rect::union);
    assert_eq!(union, Some(Rect::new(0, 0, 640, 480)));
}

#[test]
fn damage_past_the_current_edge_is_cut_to_it() {
    let mut s = with_window(100, 100);
    s.plan_frame(ID).expect("the first frame");
    damage(&mut s, 90, -5, 50, 20);
    damage(&mut s, 200, 200, 5, 5);
    let plan = s.plan_frame(ID).expect("damage inside");
    assert_eq!(plan.rects, vec![Rect::new(90, 0, 10, 15)]);
}

#[test]
fn a_surface_with_no_pixels_owes_its_full_redraw_until_it_has_some() {
    let mut s = with_window(0, 0);
    assert_eq!(s.plan_frame(ID), None, "nothing to draw");
    damage(&mut s, 0, 0, 10, 10);
    assert_eq!(s.plan_frame(ID), None, "damage outside no pixels is none");
    resize(&mut s, 30, 20);
    let plan = s.plan_frame(ID).expect("pixels at last");
    assert_eq!(plan.sequence, 1);
    assert!(
        plan.full_redraw,
        "the first frame is still the complete one"
    );
    assert_eq!(plan.rects, vec![Rect::new(0, 0, 30, 20)]);
}

#[test]
fn a_gone_surface_plans_nothing_and_its_acks_change_nothing() {
    let mut s = with_window(40, 30);
    let plan = s.plan_frame(ID).expect("the first frame");
    s.apply_event(SurfaceEvent::Destroyed { id: ID }, &mut Vec::new());
    damage(&mut s, 0, 0, 5, 5);
    assert_eq!(s.plan_frame(ID), None);
    s.frame_ack(ID, plan.sequence);
    assert!(!s.abort_frame(ID, &plan));
    assert_eq!(s.surface_count(), 0);
}

#[test]
fn an_aborted_frame_gives_back_its_credit_its_sequence_and_its_damage() {
    let mut s = with_window(100, 100);
    let first = s.plan_frame(ID).expect("the first frame");
    s.frame_ack(ID, first.sequence);
    damage(&mut s, 10, 20, 30, 40);
    let lost = s.plan_frame(ID).expect("fresh damage");
    assert_eq!(lost.sequence, 2);
    assert!(s.abort_frame(ID, &lost));
    // Handed back once; a second time names a plan that no longer exists.
    assert!(!s.abort_frame(ID, &lost));
    let again = s.plan_frame(ID).expect("the damage came back");
    assert_eq!(
        again, lost,
        "same sequence, same damage: the client sees no gap"
    );
}

#[test]
fn an_aborted_first_frame_is_still_owed_whole() {
    let mut s = with_window(100, 100);
    let first = s.plan_frame(ID).expect("the first frame");
    assert!(first.full_redraw);
    assert!(s.abort_frame(ID, &first));
    let again = s.plan_frame(ID).expect("owed again");
    assert_eq!(again.sequence, 1);
    assert!(again.full_redraw);
    assert_eq!(again.rects, vec![Rect::new(0, 0, 100, 100)]);
}

#[test]
fn only_the_last_plan_can_be_handed_back() {
    let mut s = with_window(100, 100);
    let first = s.plan_frame(ID).expect("the first frame");
    damage(&mut s, 0, 0, 5, 5);
    let second = s.plan_frame(ID).expect("the second frame");
    assert!(!s.abort_frame(ID, &first), "an older plan cannot come back");
    s.frame_ack(ID, second.sequence);
    assert!(
        !s.abort_frame(ID, &second),
        "nor one the client already drew"
    );
}
