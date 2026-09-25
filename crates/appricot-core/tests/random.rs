//! Random event sequences, from a small seeded generator, against the session's invariants.
//!
//! Each run drives one session with thousands of backend events and host requests picked by a
//! seeded xorshift generator (no dependency, and every failure reproduces from its seed). A
//! mirror built only from what the session emits checks, after every step:
//!
//! - at most `MAX_SURFACES` surfaces live, and the mirror and the session agree on which;
//! - every popup's parent lives, and no parent keeps more than `MAX_POPUPS_PER_PARENT`;
//! - every size is inside the wire's surface caps;
//! - no surface has more than `MAX_FRAME_CREDITS` frames in flight, and sequences rise by
//!   one from 1, with no gap, through handed-back frames and resumes;
//! - every planned rectangle is non-empty, inside the surface, at most `MAX_DAMAGE_RECTS` of
//!   them, and a full redraw covers the whole surface;
//! - every ack names a proposal that is still waiting, and a size change with a proposal
//!   waiting is never reported as the app's own;
//! - cursor serials rise, and a new surface's id was never announced before;
//! - identical consecutive clipboard text is never re-sent, and a host paste re-arms it.

use std::collections::{BTreeMap, HashMap};

use appricot_core::{
    ConfigureSerial, CursorImage, MAX_DAMAGE_RECTS, MAX_FRAME_CREDITS, MAX_PENDING_CONFIGURES,
    MAX_POPUPS_PER_PARENT, MAX_SURFACES, Point, Positioner, Rect, Role, Session, SessionEvent,
    Size, SurfaceEvent, SurfaceId,
};
use appricot_proto::limits::{
    AppId, MAX_CLIPBOARD_BYTES, MAX_SURFACE_HEIGHT, MAX_SURFACE_WIDTH, Title,
};

/// xorshift64*: small, fast, and good enough to pick test actions.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..n`.
    fn below(&mut self, n: u32) -> u32 {
        u32::try_from(self.next() % u64::from(n.max(1))).expect("below a u32")
    }

    /// True `percent` times in a hundred.
    fn chance(&mut self, percent: u32) -> bool {
        self.below(100) < percent
    }

    /// A size: mostly ordinary, sometimes empty, sometimes past the wire's caps.
    fn size(&mut self) -> Size {
        match self.below(10) {
            0 => Size::new(0, self.below(50)),
            1 => Size::new(1900 + self.below(2000), 1100 + self.below(2000)),
            _ => Size::new(1 + self.below(700), 1 + self.below(500)),
        }
    }

    /// A rectangle near a surface of the sizes above, sometimes partly or wholly outside.
    fn rect(&mut self) -> Rect {
        let x = i32::try_from(self.below(900)).expect("small") - 100;
        let y = i32::try_from(self.below(700)).expect("small") - 100;
        Rect::new(x, y, self.below(300), self.below(300))
    }
}

/// What the mirror knows of one living surface.
#[derive(Debug, Clone)]
struct Mirrored {
    size: Size,
    /// A popup's parent.
    popup_of: Option<u32>,
    /// Proposals not yet answered, oldest first.
    waiting: Vec<ConfigureSerial>,
    /// Frames planned and not yet acked or handed back.
    in_flight: Vec<u32>,
    /// The last sequence planned since creation or the last resume.
    last_sequence: u32,
    /// The last plan, while it may still be handed back.
    last_plan: Option<appricot_core::FramePlan>,
}

/// The session as its output describes it.
#[derive(Debug, Default)]
struct Mirror {
    living: BTreeMap<u32, Mirrored>,
    highest_announced: Option<u32>,
    last_cursor: u32,
    /// The clipboard text the host last received, cleared by a host paste like the session's
    /// own memory is.
    last_clipboard: Option<String>,
}

fn within_caps(size: Size) -> bool {
    size.width <= MAX_SURFACE_WIDTH && size.height <= MAX_SURFACE_HEIGHT
}

impl Mirror {
    /// Applies what the session emitted while attached.
    fn absorb(&mut self, out: &[SessionEvent]) {
        for event in out {
            match event {
                SessionEvent::SurfaceNew {
                    id,
                    role,
                    parent,
                    size,
                    positioner,
                    ..
                } => {
                    let id = id.get();
                    assert!(
                        self.highest_announced.is_none_or(|highest| id > highest),
                        "surface {id} announced twice, or out of order"
                    );
                    self.highest_announced = Some(id);
                    assert!(within_caps(*size), "{size:?}");
                    let popup_of = match role {
                        Role::Popup { parent: p, .. } => {
                            assert_eq!(*parent, Some(*p));
                            assert_eq!(positioner.map(|p| p.size), Some(*size));
                            Some(p.get())
                        }
                        Role::Toplevel => None,
                    };
                    self.living.insert(
                        id,
                        Mirrored {
                            size: *size,
                            popup_of,
                            waiting: Vec::new(),
                            in_flight: Vec::new(),
                            last_sequence: 0,
                            last_plan: None,
                        },
                    );
                }
                SessionEvent::SurfaceGone { id, .. } => {
                    assert!(
                        self.living.remove(&id.get()).is_some(),
                        "{id:?} was not living"
                    );
                }
                SessionEvent::ConfigureAcked { id, serial, size } => {
                    let surface = self.living.get_mut(&id.get()).expect("acked a living one");
                    let at = surface
                        .waiting
                        .iter()
                        .position(|s| s == serial)
                        .unwrap_or_else(|| panic!("{serial:?} is not waiting on {id:?}"));
                    surface.waiting.drain(..=at);
                    assert!(within_caps(*size));
                    surface.size = *size;
                }
                SessionEvent::Resized { id, size } => {
                    let surface = self.living.get_mut(&id.get()).expect("a living one");
                    assert!(
                        surface.waiting.is_empty(),
                        "a change with {:?} waiting is an ack, not the app's own",
                        surface.waiting
                    );
                    assert_ne!(surface.size, *size, "an unchanged size is no event");
                    assert!(within_caps(*size));
                    surface.size = *size;
                }
                SessionEvent::ResizeAsk { size, .. } => assert!(within_caps(*size)),
                SessionEvent::CursorChanged { cursor } => {
                    assert!(cursor.serial > self.last_cursor, "cursor serials rise");
                    self.last_cursor = cursor.serial;
                }
                SessionEvent::SurfaceMetadata { .. }
                | SessionEvent::FocusAsk { .. }
                | SessionEvent::CursorGone
                | SessionEvent::ClipboardAsk => {}
                SessionEvent::ClipboardText { text } => {
                    assert_ne!(
                        self.last_clipboard.as_deref(),
                        Some(text.as_str()),
                        "identical consecutive clipboard text is not re-sent"
                    );
                    self.last_clipboard = Some(text.clone());
                }
            }
        }
    }

    /// Rebuilds from a resume: the window set as announced, then exactly one cursor event.
    fn resumed(&mut self, out: &[SessionEvent]) {
        let (cursor, windows) = out.split_last().expect("a resume always says something");
        let before = std::mem::take(&mut self.living);
        let highest = self.highest_announced;
        self.highest_announced = None;
        for window in windows {
            assert!(
                matches!(window, SessionEvent::SurfaceNew { .. }),
                "only SurfaceNew before the cursor: {window:?}"
            );
        }
        self.absorb(windows);
        self.highest_announced = highest.max(self.highest_announced);
        // Announced in creation order, which is id order here.
        let ids: Vec<u32> = self.living.keys().copied().collect();
        assert!(
            ids.iter()
                .all(|id| before.contains_key(id) || highest < Some(*id))
        );
        match cursor {
            SessionEvent::CursorChanged { cursor } => {
                assert!(cursor.serial > self.last_cursor);
                self.last_cursor = cursor.serial;
            }
            SessionEvent::CursorGone => {}
            other => panic!("the resume ends with one cursor event, not {other:?}"),
        }
    }

    /// Checks the mirror against the session itself.
    fn check(&self, s: &Session) {
        assert!(s.surface_count() <= MAX_SURFACES);
        assert_eq!(s.surface_count(), self.living.len());
        let mut popups: HashMap<u32, usize> = HashMap::new();
        for (id, mirrored) in &self.living {
            let surface = s
                .surface(SurfaceId::new(*id))
                .expect("the mirror's surfaces live");
            assert_eq!(surface.size(), mirrored.size, "surface {id}");
            assert!(within_caps(surface.size()));
            assert!(mirrored.in_flight.len() <= MAX_FRAME_CREDITS);
            if let Some(parent) = mirrored.popup_of {
                assert!(
                    self.living.contains_key(&parent),
                    "popup {id} outlived {parent}"
                );
                *popups.entry(parent).or_default() += 1;
            }
        }
        assert!(popups.values().all(|&n| n <= MAX_POPUPS_PER_PARENT));
    }
}

/// One run of `steps` random steps from `seed`.
#[allow(clippy::too_many_lines)] // one match arm per action reads better than a split
fn run(seed: u64, steps: u32) {
    let mut rng = Rng(seed | 1);
    let mut s = Session::new();
    let mut mirror = Mirror::default();
    let mut next_id = 1_u32;
    let mut detached = false;
    let mut out = Vec::new();

    for _ in 0..steps {
        out.clear();
        let living: Vec<u32> = mirror.living.keys().copied().collect();
        // An id that lives, when one does; otherwise one that never did.
        let any_id = if living.is_empty() || rng.chance(5) {
            next_id + 1000
        } else {
            living[usize::try_from(rng.below(u32::try_from(living.len()).expect("few")))
                .expect("fits")]
        };
        let id = SurfaceId::new(any_id);
        match rng.below(21) {
            0..=2 => {
                // A new window: a toplevel, a dialog or a popup. Now and then an old id.
                let new_id = if rng.chance(5) && next_id > 1 {
                    rng.below(next_id)
                } else {
                    next_id += 1;
                    next_id - 1
                };
                let size = rng.size();
                let role = if rng.chance(50) {
                    Role::Popup {
                        parent: id,
                        positioner: Positioner::at(Point::new(4, 4), size),
                    }
                } else {
                    Role::Toplevel
                };
                let parent = rng.chance(30).then_some(id);
                s.apply_event(
                    SurfaceEvent::Created {
                        id: SurfaceId::new(new_id),
                        role,
                        size,
                        parent,
                    },
                    &mut out,
                );
            }
            3 => s.apply_event(SurfaceEvent::Destroyed { id }, &mut out),
            4..=6 => s.apply_event(
                SurfaceEvent::Damaged {
                    id,
                    rect: rng.rect(),
                },
                &mut out,
            ),
            7 | 8 => {
                // The app's answer: a size proposed, the size it has, or one of its own.
                let size = match (mirror.living.get(&any_id), rng.below(3)) {
                    (Some(m), 0) => m.size,
                    _ => rng.size(),
                };
                s.apply_event(SurfaceEvent::Resized { id, size }, &mut out);
            }
            9 => s.apply_event(
                SurfaceEvent::ResizeRequested {
                    id,
                    size: rng.size(),
                },
                &mut out,
            ),
            10 => s.apply_event(
                SurfaceEvent::Metadata {
                    id,
                    title: Title::new(if rng.chance(50) { "a" } else { "b" }).expect("short"),
                    app_id: AppId::new("app").expect("short"),
                },
                &mut out,
            ),
            11 => {
                let width = 1 + rng.below(140);
                let fill = u8::try_from(rng.below(3)).expect("small");
                let len = usize::try_from(width * 4).expect("small");
                s.apply_event(
                    SurfaceEvent::CursorChanged {
                        cursor: CursorImage {
                            serial: rng.below(4),
                            size: Size::new(width, 1),
                            hotspot: Point::new(0, 0),
                            argb: vec![fill; len],
                        },
                    },
                    &mut out,
                );
            }
            12 | 13 if !detached => {
                let size = match (mirror.living.get(&any_id), rng.below(3)) {
                    (Some(m), 0) => m.size,
                    _ => rng.size(),
                };
                if let Some(serial) = s.configure(id, size, &mut out) {
                    // The mirror queues the proposal before it reads what was said, so an
                    // ack given at once finds it waiting.
                    let surface = mirror
                        .living
                        .get_mut(&any_id)
                        .expect("configured a living one");
                    if surface.waiting.len() >= MAX_PENDING_CONFIGURES {
                        surface.waiting.remove(0);
                    }
                    surface.waiting.push(serial);
                }
            }
            14..=16 if !detached => {
                if let Some(plan) = s.plan_frame(id) {
                    let surface = mirror
                        .living
                        .get_mut(&any_id)
                        .expect("planned a living one");
                    assert_eq!(plan.sequence, surface.last_sequence + 1, "no gap");
                    assert!(!plan.rects.is_empty() && plan.rects.len() <= MAX_DAMAGE_RECTS);
                    let bounds = Rect::new(0, 0, surface.size.width, surface.size.height);
                    for rect in &plan.rects {
                        assert!(!rect.is_empty());
                        assert_eq!(bounds.intersection(*rect), Some(*rect), "inside");
                    }
                    if plan.full_redraw {
                        let union = plan.rects.iter().copied().reduce(Rect::union);
                        assert_eq!(union, Some(bounds), "a full redraw covers everything");
                    }
                    surface.last_sequence = plan.sequence;
                    surface.in_flight.push(plan.sequence);
                    surface.last_plan = Some(plan);
                }
            }
            17 if !detached => {
                if let Some(surface) = mirror.living.get_mut(&any_id) {
                    if let Some(plan) = surface.last_plan.take() {
                        let back = s.abort_frame(id, &plan);
                        assert_eq!(back, surface.in_flight.last() == Some(&plan.sequence));
                        if back {
                            surface.in_flight.pop();
                            surface.last_sequence -= 1;
                        }
                    }
                } else {
                    let plan = appricot_core::FramePlan {
                        sequence: 1,
                        full_redraw: true,
                        rects: Vec::new(),
                    };
                    assert!(!s.abort_frame(id, &plan));
                }
            }
            18 if !detached => {
                if let Some(surface) = mirror.living.get_mut(&any_id) {
                    let sequence = match surface.in_flight.first() {
                        Some(&first) if rng.chance(80) => first,
                        _ => rng.below(50),
                    };
                    s.frame_ack(id, sequence);
                    surface.in_flight.retain(|&s| s != sequence);
                    if surface
                        .last_plan
                        .as_ref()
                        .is_some_and(|p| p.sequence == sequence)
                    {
                        surface.last_plan = None;
                    }
                }
            }
            19 => {
                // The app copied: a small set of texts so repeats meet the not-twice rule,
                // now and then a host paste that re-arms it, and rarely a text over the cap.
                if rng.chance(10) {
                    s.note_clipboard_set();
                    mirror.last_clipboard = None;
                }
                let text = if rng.chance(15) {
                    "x".repeat(MAX_CLIPBOARD_BYTES + 1)
                } else {
                    match rng.below(3) {
                        0 => "a".to_owned(),
                        1 => "b".to_owned(),
                        _ => "αντίγραφο".to_owned(),
                    }
                };
                s.apply_event(SurfaceEvent::ClipboardText { text }, &mut out);
            }
            _ => {
                if detached {
                    s.resume(&mut out);
                    mirror.resumed(&out);
                    detached = false;
                    mirror.check(&s);
                    continue;
                }
                if rng.chance(30) {
                    s.detach();
                    detached = true;
                }
            }
        }
        if detached {
            assert!(out.is_empty(), "a detached session says nothing: {out:?}");
            assert!(s.is_detached());
        } else {
            mirror.absorb(&out);
            mirror.check(&s);
        }
    }
}

#[test]
fn random_sequences_keep_every_invariant() {
    for seed in 1..=24_u64 {
        run(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), 3_000);
    }
}
