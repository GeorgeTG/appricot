//! The shared popup placement vectors: `testdata/placement.json` must equal exactly what
//! [`Positioner::place`] computes for the cases below.
//!
//! The TypeScript client (`placePopup` in `packages/client/src/registry.ts`) places popups on
//! its own, and `packages/client/src/placement.vector.test.ts` asserts every entry of the
//! committed file against it, so the two sides cannot drift apart again. The format is frozen:
//! top-level `format` (always 1 for now) and `cases`, one per line, each
//! `{ "name", "anchor_rect": [x, y, width, height], "anchor", "gravity", "offset": [x, y],
//! "size": [width, height], "placed": [x, y, width, height] }`, with `anchor` and `gravity` as
//! wire `Anchor` numbers.
//!
//! Regenerate after changing a case here:
//!
//! ```sh
//! docker compose run -e APPRICOT_REGEN_VECTORS=1 --rm dev \
//!   cargo test -p appricot-core --test placement_vectors
//! ```
//!
//! and commit the file together with the change.

use appricot_core::{Anchor, Point, Positioner, Rect, Size};
use appricot_proto::wire;

/// Every anchor, in wire order, with its wire number and its wire name.
fn anchors() -> [(Anchor, i32, &'static str); 9] {
    [
        (Anchor::Center, wire::Anchor::Center as i32, "center"),
        (Anchor::Top, wire::Anchor::Top as i32, "top"),
        (Anchor::Bottom, wire::Anchor::Bottom as i32, "bottom"),
        (Anchor::Left, wire::Anchor::Left as i32, "left"),
        (Anchor::Right, wire::Anchor::Right as i32, "right"),
        (Anchor::TopLeft, wire::Anchor::TopLeft as i32, "top_left"),
        (
            Anchor::BottomLeft,
            wire::Anchor::BottomLeft as i32,
            "bottom_left",
        ),
        (Anchor::TopRight, wire::Anchor::TopRight as i32, "top_right"),
        (
            Anchor::BottomRight,
            wire::Anchor::BottomRight as i32,
            "bottom_right",
        ),
    ]
}

fn wire_number(anchor: Anchor) -> i32 {
    anchors()
        .iter()
        .find(|(a, _, _)| *a == anchor)
        .map(|(_, number, _)| *number)
        .expect("every anchor is listed")
}

/// One entry of `testdata/placement.json`.
struct Case {
    name: String,
    positioner: Positioner,
}

/// Every anchor x gravity pair against one anchor rectangle, popup size and offset.
fn pairs(group: &str, anchor_rect: Rect, size: Size, offset: Point) -> Vec<Case> {
    let mut cases = Vec::new();
    for (anchor, _, anchor_name) in anchors() {
        for (gravity, _, gravity_name) in anchors() {
            cases.push(Case {
                name: format!("{group}: anchor {anchor_name}, gravity {gravity_name}"),
                positioner: Positioner {
                    size,
                    anchor_rect,
                    anchor,
                    gravity,
                    offset,
                },
            });
        }
    }
    cases
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    // Even spans, no offset: the plain geometry.
    cases.extend(pairs(
        "even",
        Rect::new(100, 50, 40, 20),
        Size::new(30, 10),
        Point::new(0, 0),
    ));
    // Odd spans at negative coordinates, plus an offset: pins how both sides round a midpoint
    // and half a popup, and that the offset comes last.
    cases.extend(pairs(
        "odd, negative, offset",
        Rect::new(-7, -5, 13, 9),
        Size::new(11, 7),
        Point::new(3, -2),
    ));
    // What an X11 backend sends for every override-redirect window.
    for (name, at, size) in [
        ("x11 menu at (5, 5)", Point::new(5, 5), Size::new(80, 40)),
        (
            "x11 menu at (30, 40)",
            Point::new(30, 40),
            Size::new(100, 20),
        ),
        (
            "x11 window left of its parent",
            Point::new(-12, 7),
            Size::new(9, 3),
        ),
    ] {
        cases.push(Case {
            name: name.to_owned(),
            positioner: Positioner::at(at, size),
        });
    }
    // Absent sub-messages on the wire decode to zeroes: a zero rect at the origin.
    cases.push(Case {
        name: "zero rect, zero size".to_owned(),
        positioner: Positioner {
            size: Size::new(0, 0),
            anchor_rect: Rect::new(0, 0, 0, 0),
            anchor: Anchor::Center,
            gravity: Anchor::Center,
            offset: Point::new(0, 0),
        },
    });
    // Coordinates that leave i32 saturate on both sides.
    cases.push(Case {
        name: "saturates past i32::MAX".to_owned(),
        positioner: Positioner {
            size: Size::new(10, 10),
            anchor_rect: Rect::new(i32::MAX - 1, i32::MAX - 1, 10, 10),
            anchor: Anchor::BottomRight,
            gravity: Anchor::BottomRight,
            offset: Point::new(100, 100),
        },
    });
    cases.push(Case {
        name: "saturates past i32::MIN".to_owned(),
        positioner: Positioner {
            size: Size::new(10, 10),
            anchor_rect: Rect::new(i32::MIN, i32::MIN, 0, 0),
            anchor: Anchor::TopLeft,
            gravity: Anchor::TopLeft,
            offset: Point::new(-100, -100),
        },
    });
    cases
}

fn rect_json(rect: Rect) -> String {
    format!(
        "[{}, {}, {}, {}]",
        rect.origin.x, rect.origin.y, rect.size.width, rect.size.height
    )
}

fn render(cases: &[Case]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"format\": 1,\n");
    out.push_str("  \"cases\": [\n");
    for (index, case) in cases.iter().enumerate() {
        let p = &case.positioner;
        writeln!(
            out,
            "    {{ \"name\": \"{}\", \"anchor_rect\": {}, \"anchor\": {}, \"gravity\": {}, \
             \"offset\": [{}, {}], \"size\": [{}, {}], \"placed\": {} }}{}",
            case.name,
            rect_json(p.anchor_rect),
            wire_number(p.anchor),
            wire_number(p.gravity),
            p.offset.x,
            p.offset.y,
            p.size.width,
            p.size.height,
            rect_json(p.place()),
            if index + 1 == cases.len() { "" } else { "," },
        )
        .expect("writing to a String cannot fail");
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}

#[test]
fn committed_placement_vectors_are_current() {
    let cases = cases();
    assert_eq!(
        cases.len(),
        2 * 81 + 6,
        "two full anchor x gravity grids plus the extras"
    );
    let generated = render(&cases);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("placement.json");
    if std::env::var_os("APPRICOT_REGEN_VECTORS").is_some() {
        std::fs::create_dir_all(path.parent().expect("the path has a parent"))
            .expect("create testdata/");
        std::fs::write(&path, generated).expect("rewrite testdata/placement.json");
        return;
    }
    let committed = std::fs::read_to_string(&path).expect(
        "testdata/placement.json is missing; run the test once with APPRICOT_REGEN_VECTORS=1",
    );
    assert_eq!(
        committed, generated,
        "testdata/placement.json does not match Positioner::place; regenerate with: docker \
         compose run -e APPRICOT_REGEN_VECTORS=1 --rm dev cargo test -p appricot-core --test \
         placement_vectors"
    );
}

#[test]
fn gravity_is_the_direction_the_popup_grows() {
    // The anchor point of every case below is (140, 70), the bottom-right corner of the rect.
    let rect = Rect::new(100, 50, 40, 20);
    let size = Size::new(30, 10);
    let place = |gravity| {
        Positioner {
            size,
            anchor_rect: rect,
            anchor: Anchor::BottomRight,
            gravity,
            offset: Point::new(0, 0),
        }
        .place()
    };
    // BOTTOM_RIGHT grows right and down: the popup's top-left corner is on the point.
    assert_eq!(place(Anchor::BottomRight), Rect::new(140, 70, 30, 10));
    // TOP_LEFT grows left and up: its bottom-right corner is on the point.
    assert_eq!(place(Anchor::TopLeft), Rect::new(110, 60, 30, 10));
    // CENTER centres it on the point.
    assert_eq!(place(Anchor::Center), Rect::new(125, 65, 30, 10));
}
