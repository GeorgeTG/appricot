//! The shared test vectors: `testdata/vectors.json` must equal exactly what this file
//! generates.
//!
//! The TypeScript codec in `packages/client` consumes the committed file, so its format is
//! pinned. Format 2: top-level `format`, `protocol_version`, then three arrays, one entry per
//! line, where every `hex` is one full message in lowercase hex:
//!
//! - `vectors`, each `{ "name", "note", "hex" }`: canonical messages. Both codecs decode each
//!   one and re-encode it to the same bytes. Fields are emitted in field-number order, so a
//!   TypeScript encoder that does the same is byte-identical. Together they cover every
//!   message kind, every field at a value other than its default, every enum value, and
//!   every per-message row of the limits table at its cap.
//! - `lenient`, each `{ "name", "note", "hex", "canonical" }`: bytes both codecs must accept
//!   although no encoder emits them (an unknown field, fields out of order, an unpacked
//!   repeated field, a non-minimal varint). They decode to the message `canonical` encodes.
//! - `invalid`, each `{ "name", "note", "hex", "error", "field" }`: bytes both codecs must
//!   refuse. `error` and `field` are what the Rust decoder answers (`LimitViolation`,
//!   `ProtocolViolation` or `UnknownMessage`, and the dotted field path). Every per-message
//!   row of the limits table has its cap + 1 here; a payload over 4 KiB is left out and only
//!   its length prefix is sent, which both decoders must refuse before looking for the bytes.
//!
//! `MAX_MESSAGE_BYTES` itself is the one row with no vector: at the cap and over it the file
//! would hold 32 MiB of hex. Each codec tests it on its own.
//!
//! Regenerate after changing a generator here:
//!
//! ```sh
//! docker compose run -e APPRICOT_REGEN_VECTORS=1 --rm dev \
//!   cargo test -p appricot-proto --test vectors
//! ```
//!
//! and commit the file together with the change.

use appricot_proto::limits::{
    MAX_APP_ID_BYTES, MAX_BYE_TEXT_BYTES, MAX_CLIENT_NAME_BYTES, MAX_CLIPBOARD_BYTES,
    MAX_CODECS_OFFERED, MAX_CURSOR_BYTES, MAX_CURSOR_HEIGHT, MAX_CURSOR_WIDTH,
    MAX_ERROR_TEXT_BYTES, MAX_KEY_CODE_BYTES, MAX_POINTER_AXIS_STEPS, MAX_SESSION_ID_BYTES,
    MAX_SURFACE_HEIGHT, MAX_SURFACE_WIDTH, MAX_TILE_BYTES, MAX_TILE_HEIGHT, MAX_TILE_WIDTH,
    MAX_TILES_PER_FRAME, MAX_TITLE_BYTES, MAX_TOKEN_BYTES, codec,
};
use appricot_proto::wire::*;
use prost::Message as _;

/// One entry of `testdata/vectors.json`.
struct Vector {
    name: &'static str,
    note: &'static str,
    envelope: Envelope,
}

fn envelope(body: Body) -> Envelope {
    Envelope { body: Some(body) }
}

// Both helpers return Option because the message fields they fill are Option<Message>;
// wrapping here keeps every call site flat.
#[allow(clippy::unnecessary_wraps)]
fn size(width: u32, height: u32) -> Option<Size> {
    Some(Size { width, height })
}

#[allow(clippy::unnecessary_wraps)]
fn rect(x: i32, y: i32, width: u32, height: u32) -> Option<Rect> {
    Some(Rect {
        x,
        y,
        width,
        height,
    })
}

fn hello() -> Hello {
    Hello {
        protocol_version: 0,
        client_name: "client-0".into(),
        stream_token: b"token-0".to_vec(),
        codecs: vec![codec::RAW],
        resume_serial: None,
    }
}

fn surface_new() -> SurfaceNew {
    SurfaceNew {
        surface_id: 1,
        role: Role::Toplevel as i32,
        parent_id: None,
        size: size(640, 480),
        title: "Main window".into(),
        app_id: "app.editor".into(),
        positioner: None,
        scale_120ths: 120,
    }
}

/// Handshake and session lifecycle: Hello, HelloReply, Bye, ServerError.
fn handshake_vectors() -> Vec<Vector> {
    vec![
        Vector {
            name: "hello-minimal",
            note: "Hello with every field at its default; the regen marker: set \
                   APPRICOT_REGEN_VECTORS=1 to rewrite this file from the generators",
            envelope: envelope(Body::Hello(Hello::default())),
        },
        Vector {
            name: "hello-resume",
            note: "Hello with codecs [QOI, RAW] and a resume_serial, exercising packed \
                   repeated and optional fields",
            envelope: envelope(Body::Hello(Hello {
                client_name: "web".into(),
                stream_token: b"stream-token-0".to_vec(),
                codecs: vec![codec::QOI, codec::RAW],
                resume_serial: Some(5),
                ..hello()
            })),
        },
        Vector {
            name: "hello-reply",
            note: "HelloReply with resume fields set; max_frame_credits is MAX_FRAME_CREDITS",
            envelope: envelope(Body::HelloReply(HelloReply {
                protocol_version: 0,
                session_id: "session-1".into(),
                max_frame_credits: 4,
                codecs: vec![codec::RAW, codec::QOI],
                resumed: false,
                resume_serial: Some(9),
                resume_grace_ms: Some(10_000),
            })),
        },
        Vector {
            name: "bye-peer-closed",
            note: "Bye with the zero enum value, which proto3 omits from the wire",
            envelope: envelope(Body::Bye(Bye {
                reason: ByeReason::ByePeerClosed as i32,
                text: String::new(),
            })),
        },
        Vector {
            name: "bye-limit-violation",
            note: "Bye with reason BYE_LIMIT_VIOLATION (0x101): a non-zero enum value in \
                   the oneof",
            envelope: envelope(Body::Bye(Bye {
                reason: ByeReason::ByeLimitViolation as i32,
                text: "frame tiles over cap".into(),
            })),
        },
        Vector {
            name: "server-error",
            note: "ServerError code 4 (decode failure) with untrusted display text",
            envelope: envelope(Body::ServerError(ServerError {
                code: 4,
                text: "decode failure".into(),
            })),
        },
    ]
}

/// Surfaces: SurfaceNew, SurfaceGone, SurfaceMetadata, FocusAsk, ResizeAsk, Configure,
/// ConfigureAck.
fn surface_vectors() -> Vec<Vector> {
    vec![
        Vector {
            name: "surface-new-toplevel",
            note: "A plain toplevel at 1x scale with title and app id",
            envelope: envelope(Body::SurfaceNew(surface_new())),
        },
        Vector {
            name: "surface-new-greek-title",
            note: "SurfaceNew whose title is Greek: UTF-8 multibyte text in a string field",
            envelope: envelope(Body::SurfaceNew(SurfaceNew {
                size: size(400, 300),
                title: "Ρύθμιση παραθύρου".into(),
                app_id: "app.el".into(),
                ..surface_new()
            })),
        },
        Vector {
            name: "surface-new-popup",
            note: "A popup with parent, positioner (anchor, gravity) and negative offset \
                   coordinates as zigzag sint32",
            envelope: envelope(Body::SurfaceNew(SurfaceNew {
                surface_id: 9,
                role: Role::Popup as i32,
                parent_id: Some(1),
                size: size(120, 24),
                positioner: Some(Positioner {
                    anchor_rect: rect(20, 40, 60, 16),
                    anchor: Anchor::BottomLeft as i32,
                    gravity: Anchor::TopRight as i32,
                    offset: Some(Point { x: -1, y: -6 }),
                    size: size(120, 24),
                }),
                ..surface_new()
            })),
        },
        Vector {
            name: "surface-gone",
            note: "SurfaceGone with reason GONE_SESSION_END",
            envelope: envelope(Body::SurfaceGone(SurfaceGone {
                surface_id: 1,
                reason: SurfaceGoneReason::GoneSessionEnd as i32,
            })),
        },
        Vector {
            name: "surface-metadata",
            note: "SurfaceMetadata changing title and scale, leaving app id absent",
            envelope: envelope(Body::SurfaceMetadata(SurfaceMetadata {
                surface_id: 1,
                title: Some("Renamed".into()),
                app_id: None,
                scale_120ths: Some(240),
            })),
        },
        Vector {
            name: "focus-ask",
            note: "The app asking the host for focus",
            envelope: envelope(Body::FocusAsk(FocusAsk { surface_id: 2 })),
        },
        Vector {
            name: "resize-ask",
            note: "The app asking to resize itself to 800x600",
            envelope: envelope(Body::ResizeAsk(ResizeAsk {
                surface_id: 2,
                size: size(800, 600),
            })),
        },
        Vector {
            name: "configure",
            note: "The host proposing 800x600 under serial 7",
            envelope: envelope(Body::Configure(Configure {
                surface_id: 2,
                serial: 7,
                size: size(800, 600),
            })),
        },
        Vector {
            name: "configure-ack",
            note: "The app applying serial 7 at a size of its own",
            envelope: envelope(Body::ConfigureAck(ConfigureAck {
                surface_id: 2,
                serial: 7,
                size: size(800, 599),
            })),
        },
    ]
}

/// Frames: Frame, FrameAck, CursorImage, CursorGone.
fn frame_vectors() -> Vec<Vector> {
    vec![
        Vector {
            name: "frame-two-tiles",
            note: "A full redraw of two 2x2 RAW tiles (16 bytes each) at sequence 1",
            envelope: envelope(Body::Frame(Frame {
                surface_id: 1,
                sequence: 1,
                full_redraw: true,
                tiles: vec![
                    Tile {
                        rect: rect(0, 0, 2, 2),
                        codec: codec::RAW,
                        data: vec![
                            0x10, 0x20, 0x30, 0xff, 0x11, 0x21, 0x31, 0xff, 0x12, 0x22, 0x32, 0xff,
                            0x13, 0x23, 0x33, 0xff,
                        ],
                    },
                    Tile {
                        rect: rect(2, 0, 2, 2),
                        codec: codec::RAW,
                        data: vec![0xa0; 16],
                    },
                ],
            })),
        },
        Vector {
            name: "frame-raw-1x1",
            note: "A delta frame of one 1x1 RAW tile (codec 1, 4 bytes B,G,R,unused)",
            envelope: envelope(Body::Frame(Frame {
                surface_id: 1,
                sequence: 2,
                full_redraw: false,
                tiles: vec![Tile {
                    rect: rect(5, 5, 1, 1),
                    codec: codec::RAW,
                    data: vec![0x10, 0x20, 0x30, 0xff],
                }],
            })),
        },
        Vector {
            name: "frame-ack",
            note: "The client acking sequence 1 as drawn",
            envelope: envelope(Body::FrameAck(FrameAck {
                surface_id: 1,
                sequence: 1,
            })),
        },
        Vector {
            name: "cursor-image",
            note: "A 2x2 ARGB cursor, hotspot (1,0), 16 premultiplied bytes",
            envelope: envelope(Body::CursorImage(CursorImage {
                serial: 3,
                width: 2,
                height: 2,
                hotspot_x: 1,
                hotspot_y: 0,
                argb_premultiplied: vec![
                    0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff,
                    0xff, 0xff, 0xff,
                ],
            })),
        },
        Vector {
            name: "cursor-gone",
            note: "The empty CursorGone message",
            envelope: envelope(Body::CursorGone(CursorGone {})),
        },
    ]
}

/// Input and the rest: PointerMove, PointerButton, PointerAxis, Key, FocusNotify,
/// BlurRelease, ClipboardSet, ClipboardAsk, CloseRequest.
fn input_vectors() -> Vec<Vector> {
    vec![
        Vector {
            name: "pointer-move",
            note: "A move to (100, -5); the negative y is a zigzag sint32",
            envelope: envelope(Body::PointerMove(PointerMove {
                surface_id: 1,
                x: 100,
                y: -5,
            })),
        },
        Vector {
            name: "pointer-button",
            note: "Left button (X button 1) pressed",
            envelope: envelope(Body::PointerButton(PointerButton {
                surface_id: 1,
                button: 1,
                pressed: true,
            })),
        },
        Vector {
            name: "pointer-axis",
            note: "Two discrete wheel steps up (steps_y negative is up), none sideways",
            envelope: envelope(Body::PointerAxis(PointerAxis {
                surface_id: 1,
                steps_x: 0,
                steps_y: -2,
            })),
        },
        Vector {
            name: "key-press",
            note: "'a' down with shift (modifier 1)",
            envelope: envelope(Body::Key(Key {
                keysym: 0x61,
                code: "KeyA".into(),
                pressed: true,
                modifiers: 1,
            })),
        },
        Vector {
            name: "key-altgr-greek",
            note: "Greek alpha (keysym 0x03b1) through AltGr (modifier bit 32)",
            envelope: envelope(Body::Key(Key {
                keysym: 0x03b1,
                code: "KeyA".into(),
                pressed: true,
                modifiers: 32,
            })),
        },
        Vector {
            name: "focus-notify",
            note: "The host moving focus to surface 3",
            envelope: envelope(Body::FocusNotify(FocusNotify { surface_id: 3 })),
        },
        Vector {
            name: "blur-release",
            note: "The empty BlurRelease message",
            envelope: envelope(Body::BlurRelease(BlurRelease {})),
        },
        Vector {
            name: "clipboard-set",
            note: "Paste text from the host to the app",
            envelope: envelope(Body::ClipboardSet(ClipboardSet {
                text: "hello world".into(),
            })),
        },
        Vector {
            name: "clipboard-ask",
            note: "The empty ClipboardAsk message",
            envelope: envelope(Body::ClipboardAsk(ClipboardAsk {})),
        },
        Vector {
            name: "close-request",
            note: "The host user closing surface 1's chrome",
            envelope: envelope(Body::CloseRequest(CloseRequest { surface_id: 1 })),
        },
    ]
}

/// A string of exactly `len` UTF-8 bytes, mostly the two-byte Greek alpha, so that a cap
/// counted in characters instead of bytes shows.
fn greek_text(len: usize) -> String {
    let mut out = "α".repeat(len / 2);
    if len % 2 == 1 {
        out.push('x');
    }
    out
}

/// A minimal valid 2x2 three-channel QOI stream, built from the published specification
/// (<https://qoiformat.org>, checked 2026-09-20): the header, two `QOI_OP_RGB` pixels, a
/// two-pixel `QOI_OP_RUN` and the end marker. The same 31 bytes as `decoder-fuzz.test.ts`.
fn qoi_2x2() -> Vec<u8> {
    vec![
        0x71, 0x6f, 0x69, 0x66, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x03, 0x00, 0xfe,
        0xe1, 0x00, 0x00, 0xfe, 0x00, 0xe1, 0x00, 0xc1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x01,
    ]
}

/// A popup of surface 1 whose size is its positioner's.
fn popup(
    anchor: Anchor,
    gravity: Anchor,
    anchor_rect: Option<Rect>,
    offset: Option<Point>,
    width: u32,
    height: u32,
) -> Envelope {
    envelope(Body::SurfaceNew(SurfaceNew {
        surface_id: 9,
        role: Role::Popup as i32,
        parent_id: Some(1),
        size: size(width, height),
        positioner: Some(Positioner {
            anchor_rect,
            anchor: anchor as i32,
            gravity: gravity as i32,
            offset,
            size: size(width, height),
        }),
        ..surface_new()
    }))
}

fn bye(reason: ByeReason, text: &str) -> Envelope {
    envelope(Body::Bye(Bye {
        reason: reason as i32,
        text: text.into(),
    }))
}

/// Handshake and surface messages at their caps, every enum value the vectors above miss,
/// and optional fields present at their defaults.
#[allow(clippy::too_many_lines)] // One entry per edge case.
fn edge_handshake_and_surface_vectors() -> Vec<Vector> {
    vec![
        Vector {
            name: "hello-at-every-cap",
            note: "Hello with client_name, stream_token and codecs each exactly at its cap",
            envelope: envelope(Body::Hello(Hello {
                client_name: "c".repeat(MAX_CLIENT_NAME_BYTES),
                stream_token: vec![0xa5; MAX_TOKEN_BYTES],
                codecs: [codec::QOI, codec::RAW].repeat(MAX_CODECS_OFFERED / 2),
                ..hello()
            })),
        },
        Vector {
            name: "hello-version-1-resume-zero",
            note: "Hello asking for version 1, which a v0 server refuses with \
                   BYE_PROTOCOL_VERSION but the codec carries; resume_serial present at 0, an \
                   optional field at its default",
            envelope: envelope(Body::Hello(Hello {
                protocol_version: 1,
                codecs: vec![],
                resume_serial: Some(0),
                ..hello()
            })),
        },
        Vector {
            name: "hello-reply-at-every-cap",
            note: "HelloReply resumed, version 1, session_id and codecs at their caps, and both \
                   optional fields present at 0",
            envelope: envelope(Body::HelloReply(HelloReply {
                protocol_version: 1,
                session_id: "s".repeat(MAX_SESSION_ID_BYTES),
                max_frame_credits: 4,
                codecs: [codec::RAW, codec::QOI].repeat(MAX_CODECS_OFFERED / 2),
                resumed: true,
                resume_serial: Some(0),
                resume_grace_ms: Some(0),
            })),
        },
        Vector {
            name: "bye-protocol-version",
            note: "Bye with reason BYE_PROTOCOL_VERSION (0x100)",
            envelope: bye(ByeReason::ByeProtocolVersion, "server speaks v0"),
        },
        Vector {
            name: "bye-auth-failed",
            note: "Bye with reason BYE_AUTH_FAILED (0x102) and no text",
            envelope: bye(ByeReason::ByeAuthFailed, ""),
        },
        Vector {
            name: "bye-protocol-violation-text-at-cap",
            note: "Bye with reason BYE_PROTOCOL_VIOLATION (0x103) and a Greek text of exactly \
                   MAX_BYE_TEXT_BYTES",
            envelope: bye(
                ByeReason::ByeProtocolViolation,
                &greek_text(MAX_BYE_TEXT_BYTES),
            ),
        },
        Vector {
            name: "bye-session-gone",
            note: "Bye with reason BYE_SESSION_GONE (0x104)",
            envelope: bye(ByeReason::ByeSessionGone, "gone"),
        },
        Vector {
            name: "bye-server-shutdown",
            note: "Bye with reason BYE_SERVER_SHUTDOWN (0x105)",
            envelope: bye(ByeReason::ByeServerShutdown, "shutting down"),
        },
        Vector {
            name: "server-error-at-cap",
            note: "ServerError code 5 (internal error) with a Greek text of exactly \
                   MAX_ERROR_TEXT_BYTES",
            envelope: envelope(Body::ServerError(ServerError {
                code: 5,
                text: greek_text(MAX_ERROR_TEXT_BYTES),
            })),
        },
        Vector {
            name: "surface-new-at-every-cap",
            note: "SurfaceNew at the width, height, title and app id caps (the title in \
                   two-byte Greek), the largest id, parent_id present at 0, scale 2x",
            envelope: envelope(Body::SurfaceNew(SurfaceNew {
                surface_id: u32::MAX,
                parent_id: Some(0),
                size: size(MAX_SURFACE_WIDTH, MAX_SURFACE_HEIGHT),
                title: greek_text(MAX_TITLE_BYTES),
                app_id: "a".repeat(MAX_APP_ID_BYTES),
                scale_120ths: 240,
                ..surface_new()
            })),
        },
        Vector {
            name: "surface-new-popup-top-bottom",
            note: "A popup anchored at ANCHOR_TOP (1) with gravity ANCHOR_BOTTOM (2)",
            envelope: popup(
                Anchor::Top,
                Anchor::Bottom,
                rect(0, 0, 60, 16),
                None,
                120,
                24,
            ),
        },
        Vector {
            name: "surface-new-popup-left-right",
            note: "A popup anchored at ANCHOR_LEFT (3) with gravity ANCHOR_RIGHT (4)",
            envelope: popup(
                Anchor::Left,
                Anchor::Right,
                rect(4, 4, 8, 8),
                Some(Point { x: 2, y: 0 }),
                120,
                24,
            ),
        },
        Vector {
            name: "surface-new-popup-corners-extremes",
            note: "A popup anchored at ANCHOR_TOP_LEFT (5) with gravity ANCHOR_BOTTOM_RIGHT \
                   (8), its offset at the sint32 extremes: five-byte zigzag varints",
            envelope: popup(
                Anchor::TopLeft,
                Anchor::BottomRight,
                rect(-1, -1, 256, 256),
                Some(Point {
                    x: i32::MIN,
                    y: i32::MAX,
                }),
                120,
                24,
            ),
        },
        Vector {
            name: "surface-new-popup-center-at-cap",
            note: "A popup at ANCHOR_CENTER (0) with gravity ANCHOR_CENTER, both omitted as \
                   proto3 defaults, no anchor_rect, and the largest size",
            envelope: popup(
                Anchor::Center,
                Anchor::Center,
                None,
                None,
                MAX_SURFACE_WIDTH,
                MAX_SURFACE_HEIGHT,
            ),
        },
        Vector {
            name: "surface-gone-app-closed",
            note: "SurfaceGone with reason GONE_APP_CLOSED (0), omitted as the proto3 default",
            envelope: envelope(Body::SurfaceGone(SurfaceGone {
                surface_id: 4,
                reason: SurfaceGoneReason::GoneAppClosed as i32,
            })),
        },
        Vector {
            name: "surface-gone-parent-gone",
            note: "SurfaceGone with reason GONE_PARENT_GONE (1)",
            envelope: envelope(Body::SurfaceGone(SurfaceGone {
                surface_id: 9,
                reason: SurfaceGoneReason::GoneParentGone as i32,
            })),
        },
        Vector {
            name: "surface-metadata-app-id-at-cap",
            note: "SurfaceMetadata with title present but empty, an optional string at its \
                   default, and app_id at MAX_APP_ID_BYTES",
            envelope: envelope(Body::SurfaceMetadata(SurfaceMetadata {
                surface_id: 2,
                title: Some(String::new()),
                app_id: Some("a".repeat(MAX_APP_ID_BYTES)),
                scale_120ths: None,
            })),
        },
        Vector {
            name: "resize-ask-at-cap",
            note: "ResizeAsk for the largest surface",
            envelope: envelope(Body::ResizeAsk(ResizeAsk {
                surface_id: 2,
                size: size(MAX_SURFACE_WIDTH, MAX_SURFACE_HEIGHT),
            })),
        },
        Vector {
            name: "configure-at-cap",
            note: "Configure with the largest serial and the largest surface",
            envelope: envelope(Body::Configure(Configure {
                surface_id: 2,
                serial: u32::MAX,
                size: size(MAX_SURFACE_WIDTH, MAX_SURFACE_HEIGHT),
            })),
        },
        Vector {
            name: "configure-ack-serial-zero",
            note: "ConfigureAck with serial 0, omitted as the proto3 default: the server's ack \
                   of a size change the host did not configure",
            envelope: envelope(Body::ConfigureAck(ConfigureAck {
                surface_id: 2,
                serial: 0,
                size: size(640, 400),
            })),
        },
    ]
}

/// Frame, cursor and input messages at their caps.
fn edge_frame_and_input_vectors() -> Vec<Vector> {
    let side = MAX_TILE_WIDTH.min(MAX_TILE_HEIGHT);
    let steps = i32::try_from(MAX_POINTER_AXIS_STEPS).expect("a small cap");
    vec![
        Vector {
            name: "frame-tiles-at-cap",
            note: "A frame of MAX_TILES_PER_FRAME 1x1 RAW tiles",
            envelope: envelope(Body::Frame(Frame {
                surface_id: 3,
                sequence: 7,
                full_redraw: false,
                tiles: (0..MAX_TILES_PER_FRAME)
                    .map(|i| Tile {
                        rect: rect(i32::try_from(i).expect("under 48"), 0, 1, 1),
                        codec: codec::RAW,
                        data: vec![0x10, 0x20, 0x30, 0xff],
                    })
                    .collect(),
            })),
        },
        Vector {
            name: "frame-qoi-2x2",
            note: "One 2x2 tile in codec 2 (QOI): a three-channel stream built from the spec",
            envelope: envelope(Body::Frame(Frame {
                surface_id: 3,
                sequence: 8,
                full_redraw: false,
                tiles: vec![Tile {
                    rect: rect(1, 1, 2, 2),
                    codec: codec::QOI,
                    data: qoi_2x2(),
                }],
            })),
        },
        Vector {
            name: "frame-tile-at-every-cap",
            note: "One RAW tile at the tile width, height and byte caps: 256x256, 262144 bytes",
            envelope: envelope(Body::Frame(Frame {
                surface_id: 3,
                sequence: 9,
                full_redraw: true,
                tiles: vec![Tile {
                    rect: rect(0, 0, side, side),
                    codec: codec::RAW,
                    data: (0..MAX_TILE_BYTES)
                        .map(|i| u8::try_from(i % 251).expect("under 251"))
                        .collect(),
                }],
            })),
        },
        Vector {
            name: "cursor-image-at-cap",
            note: "A 128x128 cursor, hotspot at its far corner: 65536 bytes, MAX_CURSOR_BYTES",
            envelope: envelope(Body::CursorImage(CursorImage {
                serial: 4,
                width: MAX_CURSOR_WIDTH,
                height: MAX_CURSOR_HEIGHT,
                hotspot_x: MAX_CURSOR_WIDTH - 1,
                hotspot_y: MAX_CURSOR_HEIGHT - 1,
                argb_premultiplied: (0..MAX_CURSOR_BYTES)
                    .map(|i| u8::try_from(i % 256).expect("a byte"))
                    .collect(),
            })),
        },
        Vector {
            name: "pointer-move-extremes",
            note: "A move to the sint32 extremes: five-byte zigzag varints",
            envelope: envelope(Body::PointerMove(PointerMove {
                surface_id: 1,
                x: i32::MIN,
                y: i32::MAX,
            })),
        },
        Vector {
            name: "pointer-axis-at-cap",
            note: "MAX_POINTER_AXIS_STEPS steps right (steps_x 64) and up (steps_y -64)",
            envelope: envelope(Body::PointerAxis(PointerAxis {
                surface_id: 1,
                steps_x: steps,
                steps_y: -steps,
            })),
        },
        Vector {
            name: "key-code-at-cap",
            note: "A key release whose code is 16 bytes, MAX_KEY_CODE_BYTES, with the X11 \
                   Unicode keysym for Greek alpha (0x010003b1)",
            envelope: envelope(Body::Key(Key {
                keysym: 0x0100_03b1,
                code: "BrowserFavorites".into(),
                pressed: false,
                modifiers: 4,
            })),
        },
        Vector {
            name: "clipboard-set-at-cap",
            note: "ClipboardSet text of exactly MAX_CLIPBOARD_BYTES, in two-byte Greek",
            envelope: envelope(Body::ClipboardSet(ClipboardSet {
                text: greek_text(MAX_CLIPBOARD_BYTES),
            })),
        },
    ]
}

/// Every canonical vector, in a stable order: the first 30 (handshake, surfaces, frames,
/// input), then the edge cases. New vectors are appended, so the committed file only grows at
/// the end of its `vectors` array.
fn vectors() -> Vec<Vector> {
    let mut all = Vec::new();
    all.extend(handshake_vectors());
    all.extend(surface_vectors());
    all.extend(frame_vectors());
    all.extend(input_vectors());
    all.extend(edge_handshake_and_surface_vectors());
    all.extend(edge_frame_and_input_vectors());
    all
}

// ---------------------------------------------------------------------------------------------
// Hand-built protobuf, for the bytes no encoder emits.
// ---------------------------------------------------------------------------------------------

/// Appends a protobuf varint.
fn put_varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(u8::try_from(value & 0x7f).expect("seven bits") | 0x80);
        value >>= 7;
    }
    out.push(u8::try_from(value).expect("under 0x80"));
}

/// A field key.
fn key(number: u32, wire_type: u8) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(u64::from((number << 3) | u32::from(wire_type)), &mut out);
    out
}

/// A varint field.
fn vfield(number: u32, value: u64) -> Vec<u8> {
    let mut out = key(number, 0);
    put_varint(value, &mut out);
    out
}

/// A length-delimited field.
fn lfield(number: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = key(number, 2);
    put_varint(payload.len() as u64, &mut out);
    out.extend_from_slice(payload);
    out
}

/// The key and the length of a length-delimited field, with none of its payload.
fn length_only(number: u32, len: usize) -> Vec<u8> {
    let mut out = key(number, 2);
    put_varint(len as u64, &mut out);
    out
}

/// The fields of one message as prost encodes them, with no envelope around them.
fn fields(message: &impl prost::Message) -> Vec<u8> {
    message.encode_to_vec()
}

fn hello_resume() -> Hello {
    Hello {
        client_name: "web".into(),
        stream_token: b"stream-token-0".to_vec(),
        codecs: vec![codec::QOI, codec::RAW],
        resume_serial: Some(5),
        ..hello()
    }
}

fn popup_positioner() -> Positioner {
    Positioner {
        anchor_rect: rect(20, 40, 60, 16),
        anchor: Anchor::BottomLeft as i32,
        gravity: Anchor::TopRight as i32,
        offset: Some(Point { x: -1, y: -6 }),
        size: size(120, 24),
    }
}

/// One `lenient` entry: bytes no encoder emits that both decoders must accept.
struct Lenient {
    name: &'static str,
    note: &'static str,
    bytes: Vec<u8>,
    canonical: Envelope,
}

#[allow(clippy::too_many_lines)] // One entry per leniency.
fn lenient_vectors() -> Vec<Lenient> {
    let popup_new = SurfaceNew {
        surface_id: 9,
        role: Role::Popup as i32,
        parent_id: Some(1),
        size: size(120, 24),
        positioner: Some(popup_positioner()),
        ..surface_new()
    };
    let key_press = Key {
        keysym: 0x61,
        code: "KeyA".into(),
        pressed: true,
        modifiers: 1,
    };
    let nested_popup = {
        let positioner = popup_positioner();
        let anchor_rect = [
            fields(&positioner.anchor_rect.expect("set above")),
            key(5, 1),
            vec![1, 2, 3, 4, 5, 6, 7, 8],
        ]
        .concat();
        let positioner_bytes = [
            lfield(1, &anchor_rect),
            vfield(2, Anchor::BottomLeft as u64),
            vfield(3, Anchor::TopRight as u64),
            lfield(4, &fields(&positioner.offset.expect("set above"))),
            lfield(5, &fields(&positioner.size.expect("set above"))),
            key(6, 5),
            vec![9, 9, 9, 9],
        ]
        .concat();
        let head = fields(&SurfaceNew {
            positioner: None,
            ..popup_new.clone()
        });
        lfield(5, &[head, lfield(7, &positioner_bytes)].concat())
    };
    vec![
        Lenient {
            name: "hello-unknown-varint-field",
            note: "hello-resume with an unknown field 9 (a varint) at its end: skipped",
            bytes: lfield(1, &[fields(&hello_resume()), vfield(9, 150)].concat()),
            canonical: envelope(Body::Hello(hello_resume())),
        },
        Lenient {
            name: "surface-new-unknown-string-field",
            note: "surface-new-toplevel with an unknown field 9 (a string, as a future \
                   icon_name would be): skipped, the additive change v0.md section 13 allows",
            bytes: lfield(
                5,
                &[fields(&surface_new()), lfield(9, b"icon-name")].concat(),
            ),
            canonical: envelope(Body::SurfaceNew(surface_new())),
        },
        Lenient {
            name: "surface-new-popup-nested-unknown-fields",
            note: "surface-new-popup with an unknown fixed64 field in its anchor Rect, an \
                   unknown fixed32 field in its Positioner, and the positioner (field 7) after \
                   scale_120ths (field 8)",
            bytes: nested_popup,
            canonical: envelope(Body::SurfaceNew(popup_new)),
        },
        Lenient {
            name: "hello-codecs-unpacked",
            note: "Hello whose codecs [QOI, RAW] come unpacked, one key per entry",
            bytes: lfield(1, &[vfield(4, 2), vfield(4, 1)].concat()),
            canonical: envelope(Body::Hello(Hello {
                codecs: vec![codec::QOI, codec::RAW],
                ..Hello::default()
            })),
        },
        Lenient {
            name: "hello-codecs-split-packed",
            note: "Hello whose codecs [QOI, RAW] come as two packed runs, counted together",
            bytes: lfield(1, &[lfield(4, &[2]), lfield(4, &[1])].concat()),
            canonical: envelope(Body::Hello(Hello {
                codecs: vec![codec::QOI, codec::RAW],
                ..Hello::default()
            })),
        },
        Lenient {
            name: "focus-notify-non-minimal-varint",
            note: "FocusNotify whose surface_id 3 is a two-byte varint (83 00)",
            bytes: lfield(20, &[0x08, 0x83, 0x00]),
            canonical: envelope(Body::FocusNotify(FocusNotify { surface_id: 3 })),
        },
        Lenient {
            name: "frame-ack-explicit-default",
            note: "FrameAck that writes surface_id 0 out, which proto3 omits",
            bytes: lfield(13, &[0x08, 0x00, 0x10, 0x01]),
            canonical: envelope(Body::FrameAck(FrameAck {
                surface_id: 0,
                sequence: 1,
            })),
        },
        Lenient {
            name: "pointer-button-pressed-as-two",
            note: "PointerButton whose bool pressed is the varint 2: any non-zero is true",
            bytes: lfield(17, &[0x08, 0x01, 0x10, 0x01, 0x18, 0x02]),
            canonical: envelope(Body::PointerButton(PointerButton {
                surface_id: 1,
                button: 1,
                pressed: true,
            })),
        },
        Lenient {
            name: "clipboard-ask-unknown-field",
            note: "ClipboardAsk carrying an unknown field 1 with markup in it: skipped, so the \
                   string never reaches the host",
            bytes: lfield(23, &lfield(1, b"<b>smuggled</b>")),
            canonical: envelope(Body::ClipboardAsk(ClipboardAsk {})),
        },
        Lenient {
            name: "key-unknown-fixed64-field",
            note: "key-press with an unknown field 5 (a fixed64) before its known fields",
            bytes: lfield(19, &[key(5, 1), vec![0; 8], fields(&key_press)].concat()),
            canonical: envelope(Body::Key(key_press)),
        },
    ]
}

/// What the Rust decoder answers for an `invalid` entry.
#[derive(Debug, Clone, Copy)]
enum Refused {
    Limit(&'static str),
    Protocol(&'static str),
    Unknown,
}

impl Refused {
    fn error(self) -> &'static str {
        match self {
            Self::Limit(_) => "LimitViolation",
            Self::Protocol(_) => "ProtocolViolation",
            Self::Unknown => "UnknownMessage",
        }
    }

    fn field(self) -> &'static str {
        match self {
            Self::Limit(field) | Self::Protocol(field) => field,
            Self::Unknown => "",
        }
    }

    fn matches(self, error: &DecodeError) -> bool {
        match (self, error) {
            (Self::Limit(want), DecodeError::LimitViolation { field })
            | (Self::Protocol(want), DecodeError::ProtocolViolation { field }) => want == *field,
            (Self::Unknown, DecodeError::UnknownMessage) => true,
            _ => false,
        }
    }

    fn matches_encode(self, error: &EncodeError) -> bool {
        match (self, error) {
            (Self::Limit(want), EncodeError::LimitViolation { field })
            | (Self::Protocol(want), EncodeError::ProtocolViolation { field }) => want == *field,
            _ => false,
        }
    }
}

/// One `invalid` entry. `envelope` is set when prost can hold the refused message: the
/// encoder must then refuse it too, with the same error.
struct Invalid {
    name: &'static str,
    note: &'static str,
    bytes: Vec<u8>,
    want: Refused,
    envelope: Option<Envelope>,
}

/// A refused message prost can hold: its bytes as prost writes them.
fn refused(name: &'static str, note: &'static str, envelope: Envelope, want: Refused) -> Invalid {
    Invalid {
        name,
        note,
        bytes: envelope.encode_to_vec(),
        want,
        envelope: Some(envelope),
    }
}

/// A refused message too big for the file: `bytes` stops after the key and length of the
/// field over its cap, and `envelope` is the whole message, which the encoder must refuse.
fn prefix_only(
    name: &'static str,
    note: &'static str,
    bytes: Vec<u8>,
    envelope: Envelope,
    field: &'static str,
) -> Invalid {
    Invalid {
        name,
        note,
        bytes,
        want: Refused::Limit(field),
        envelope: Some(envelope),
    }
}

/// Refused bytes no message can stand for: the grammar is broken, or there is no known body.
fn broken(name: &'static str, note: &'static str, bytes: Vec<u8>, want: Refused) -> Invalid {
    Invalid {
        name,
        note,
        bytes,
        want,
        envelope: None,
    }
}

/// Every per-message row of the limits table at its cap + 1.
#[allow(clippy::too_many_lines)] // One entry per row of the table, in its order.
fn over_cap_vectors() -> Vec<Invalid> {
    use Refused::Limit;
    let surface = |width, height| {
        envelope(Body::SurfaceNew(SurfaceNew {
            size: size(width, height),
            ..surface_new()
        }))
    };
    let positioned = |width, height| {
        envelope(Body::SurfaceNew(SurfaceNew {
            positioner: Some(Positioner {
                size: size(width, height),
                ..popup_positioner()
            }),
            ..surface_new()
        }))
    };
    let pixel = || Tile {
        rect: rect(0, 0, 1, 1),
        codec: codec::RAW,
        data: vec![0; 4],
    };
    let one_tile = |tile: Tile| {
        envelope(Body::Frame(Frame {
            surface_id: 1,
            sequence: 1,
            full_redraw: false,
            tiles: vec![tile],
        }))
    };
    let cursor = |width: u32, height: u32| {
        envelope(Body::CursorImage(CursorImage {
            serial: 1,
            width,
            height,
            hotspot_x: 0,
            hotspot_y: 0,
            argb_premultiplied: vec![0; usize::try_from(width * height * 4).expect("small")],
        }))
    };
    let over = i32::try_from(MAX_POINTER_AXIS_STEPS + 1).expect("a small cap");
    let axis = |steps_x, steps_y| {
        envelope(Body::PointerAxis(PointerAxis {
            surface_id: 1,
            steps_x,
            steps_y,
        }))
    };
    vec![
        refused(
            "hello-client-name-over-cap",
            "Hello.client_name of MAX_CLIENT_NAME_BYTES + 1",
            envelope(Body::Hello(Hello {
                client_name: "c".repeat(MAX_CLIENT_NAME_BYTES + 1),
                ..hello()
            })),
            Limit("hello.client_name"),
        ),
        refused(
            "hello-stream-token-over-cap",
            "Hello.stream_token of MAX_TOKEN_BYTES + 1",
            envelope(Body::Hello(Hello {
                stream_token: vec![0xa5; MAX_TOKEN_BYTES + 1],
                ..hello()
            })),
            Limit("hello.stream_token"),
        ),
        refused(
            "hello-codecs-over-cap",
            "Hello.codecs with MAX_CODECS_OFFERED + 1 entries, packed",
            envelope(Body::Hello(Hello {
                codecs: vec![codec::RAW; MAX_CODECS_OFFERED + 1],
                ..hello()
            })),
            Limit("hello.codecs"),
        ),
        broken(
            "hello-codecs-over-cap-unpacked",
            "Hello.codecs with MAX_CODECS_OFFERED + 1 entries, one key each",
            lfield(1, &vfield(4, 1).repeat(MAX_CODECS_OFFERED + 1)),
            Limit("hello.codecs"),
        ),
        refused(
            "hello-reply-session-id-over-cap",
            "HelloReply.session_id of MAX_SESSION_ID_BYTES + 1",
            envelope(Body::HelloReply(HelloReply {
                session_id: "s".repeat(MAX_SESSION_ID_BYTES + 1),
                ..HelloReply::default()
            })),
            Limit("hello_reply.session_id"),
        ),
        refused(
            "hello-reply-codecs-over-cap",
            "HelloReply.codecs with MAX_CODECS_OFFERED + 1 entries",
            envelope(Body::HelloReply(HelloReply {
                codecs: vec![codec::QOI; MAX_CODECS_OFFERED + 1],
                ..HelloReply::default()
            })),
            Limit("hello_reply.codecs"),
        ),
        refused(
            "bye-text-over-cap",
            "Bye.text of MAX_BYE_TEXT_BYTES + 1, in two-byte Greek and one ASCII byte",
            bye(
                ByeReason::ByePeerClosed,
                &greek_text(MAX_BYE_TEXT_BYTES + 1),
            ),
            Limit("bye.text"),
        ),
        refused(
            "server-error-text-over-cap",
            "ServerError.text of MAX_ERROR_TEXT_BYTES + 1",
            envelope(Body::ServerError(ServerError {
                code: 1,
                text: "e".repeat(MAX_ERROR_TEXT_BYTES + 1),
            })),
            Limit("server_error.text"),
        ),
        refused(
            "surface-new-title-over-cap",
            "SurfaceNew.title of MAX_TITLE_BYTES + 1, in two-byte Greek and one ASCII byte",
            envelope(Body::SurfaceNew(SurfaceNew {
                title: greek_text(MAX_TITLE_BYTES + 1),
                ..surface_new()
            })),
            Limit("surface_new.title"),
        ),
        refused(
            "surface-new-app-id-over-cap",
            "SurfaceNew.app_id of MAX_APP_ID_BYTES + 1",
            envelope(Body::SurfaceNew(SurfaceNew {
                app_id: "a".repeat(MAX_APP_ID_BYTES + 1),
                ..surface_new()
            })),
            Limit("surface_new.app_id"),
        ),
        refused(
            "surface-new-width-over-cap",
            "SurfaceNew.size.width of MAX_SURFACE_WIDTH + 1",
            surface(MAX_SURFACE_WIDTH + 1, 480),
            Limit("surface_new.size.width"),
        ),
        refused(
            "surface-new-height-over-cap",
            "SurfaceNew.size.height of MAX_SURFACE_HEIGHT + 1",
            surface(640, MAX_SURFACE_HEIGHT + 1),
            Limit("surface_new.size.height"),
        ),
        refused(
            "positioner-width-over-cap",
            "A positioner size of MAX_SURFACE_WIDTH + 1 wide",
            positioned(MAX_SURFACE_WIDTH + 1, 24),
            Limit("surface_new.positioner.size.width"),
        ),
        refused(
            "positioner-height-over-cap",
            "A positioner size of MAX_SURFACE_HEIGHT + 1 high",
            positioned(120, MAX_SURFACE_HEIGHT + 1),
            Limit("surface_new.positioner.size.height"),
        ),
        refused(
            "surface-metadata-title-over-cap",
            "SurfaceMetadata.title of MAX_TITLE_BYTES + 1",
            envelope(Body::SurfaceMetadata(SurfaceMetadata {
                surface_id: 1,
                title: Some("t".repeat(MAX_TITLE_BYTES + 1)),
                ..SurfaceMetadata::default()
            })),
            Limit("surface_metadata.title"),
        ),
        refused(
            "surface-metadata-app-id-over-cap",
            "SurfaceMetadata.app_id of MAX_APP_ID_BYTES + 1",
            envelope(Body::SurfaceMetadata(SurfaceMetadata {
                surface_id: 1,
                app_id: Some("a".repeat(MAX_APP_ID_BYTES + 1)),
                ..SurfaceMetadata::default()
            })),
            Limit("surface_metadata.app_id"),
        ),
        refused(
            "resize-ask-width-over-cap",
            "ResizeAsk.size.width of MAX_SURFACE_WIDTH + 1",
            envelope(Body::ResizeAsk(ResizeAsk {
                surface_id: 2,
                size: size(MAX_SURFACE_WIDTH + 1, 600),
            })),
            Limit("resize_ask.size.width"),
        ),
        refused(
            "configure-height-over-cap",
            "Configure.size.height of MAX_SURFACE_HEIGHT + 1",
            envelope(Body::Configure(Configure {
                surface_id: 2,
                serial: 1,
                size: size(800, MAX_SURFACE_HEIGHT + 1),
            })),
            Limit("configure.size.height"),
        ),
        refused(
            "configure-ack-width-over-cap",
            "ConfigureAck.size.width of MAX_SURFACE_WIDTH + 1",
            envelope(Body::ConfigureAck(ConfigureAck {
                surface_id: 2,
                serial: 1,
                size: size(MAX_SURFACE_WIDTH + 1, 600),
            })),
            Limit("configure_ack.size.width"),
        ),
        refused(
            "frame-tiles-over-cap",
            "A frame of MAX_TILES_PER_FRAME + 1 tiles",
            envelope(Body::Frame(Frame {
                surface_id: 1,
                sequence: 1,
                full_redraw: false,
                tiles: vec![pixel(); MAX_TILES_PER_FRAME + 1],
            })),
            Limit("frame.tiles"),
        ),
        refused(
            "tile-width-over-cap",
            "A tile rect of MAX_TILE_WIDTH + 1 by 1",
            one_tile(Tile {
                rect: rect(0, 0, MAX_TILE_WIDTH + 1, 1),
                ..pixel()
            }),
            Limit("tile.rect.width"),
        ),
        refused(
            "tile-height-over-cap",
            "A tile rect of 1 by MAX_TILE_HEIGHT + 1",
            one_tile(Tile {
                rect: rect(0, 0, 1, MAX_TILE_HEIGHT + 1),
                ..pixel()
            }),
            Limit("tile.rect.height"),
        ),
        prefix_only(
            "tile-data-over-cap",
            "Tile.data of MAX_TILE_BYTES + 1: only the key and the length are sent",
            lfield(
                12,
                &lfield(
                    4,
                    &[
                        lfield(1, &fields(&rect(0, 0, 1, 1).expect("set"))),
                        vfield(2, u64::from(codec::RAW)),
                        length_only(3, MAX_TILE_BYTES + 1),
                    ]
                    .concat(),
                ),
            ),
            one_tile(Tile {
                data: vec![0; MAX_TILE_BYTES + 1],
                ..pixel()
            }),
            "tile.data",
        ),
        refused(
            "cursor-width-over-cap",
            "A cursor MAX_CURSOR_WIDTH + 1 wide and 1 high, with the bytes to match",
            cursor(MAX_CURSOR_WIDTH + 1, 1),
            Limit("cursor_image.width"),
        ),
        refused(
            "cursor-height-over-cap",
            "A cursor 1 wide and MAX_CURSOR_HEIGHT + 1 high, with the bytes to match",
            cursor(1, MAX_CURSOR_HEIGHT + 1),
            Limit("cursor_image.height"),
        ),
        prefix_only(
            "cursor-bytes-over-cap",
            "A 128x128 cursor of MAX_CURSOR_BYTES + 1 bytes: only the key and the length are \
             sent",
            lfield(
                14,
                &[
                    vfield(2, u64::from(MAX_CURSOR_WIDTH)),
                    vfield(3, u64::from(MAX_CURSOR_HEIGHT)),
                    length_only(6, MAX_CURSOR_BYTES + 1),
                ]
                .concat(),
            ),
            envelope(Body::CursorImage(CursorImage {
                width: MAX_CURSOR_WIDTH,
                height: MAX_CURSOR_HEIGHT,
                argb_premultiplied: vec![0; MAX_CURSOR_BYTES + 1],
                ..CursorImage::default()
            })),
            "cursor_image.argb_premultiplied",
        ),
        refused(
            "key-code-over-cap",
            "Key.code of MAX_KEY_CODE_BYTES + 1",
            envelope(Body::Key(Key {
                keysym: 0x61,
                code: "K".repeat(MAX_KEY_CODE_BYTES + 1),
                pressed: true,
                modifiers: 0,
            })),
            Limit("key.code"),
        ),
        prefix_only(
            "clipboard-set-over-cap",
            "ClipboardSet.text of MAX_CLIPBOARD_BYTES + 1: only the key and the length are \
             sent",
            lfield(22, &length_only(1, MAX_CLIPBOARD_BYTES + 1)),
            envelope(Body::ClipboardSet(ClipboardSet {
                text: "x".repeat(MAX_CLIPBOARD_BYTES + 1),
            })),
            "clipboard_set.text",
        ),
        refused(
            "pointer-axis-steps-x-over-cap",
            "PointerAxis.steps_x of MAX_POINTER_AXIS_STEPS + 1",
            axis(over, 0),
            Limit("pointer_axis.steps_x"),
        ),
        refused(
            "pointer-axis-steps-y-over-cap",
            "PointerAxis.steps_y of -(MAX_POINTER_AXIS_STEPS + 1)",
            axis(0, -over),
            Limit("pointer_axis.steps_y"),
        ),
        refused(
            "pointer-axis-steps-min",
            "PointerAxis.steps_y of the least sint32, whose magnitude no int32 holds",
            axis(0, i32::MIN),
            Limit("pointer_axis.steps_y"),
        ),
    ]
}

/// The value rules beside the table, and the closed enums.
#[allow(clippy::too_many_lines)] // One entry per rule and enum.
fn value_rule_vectors() -> Vec<Invalid> {
    use Refused::{Limit, Protocol};
    let key_with = |code: &str| {
        envelope(Body::Key(Key {
            keysym: 0x61,
            code: code.into(),
            pressed: true,
            modifiers: 0,
        }))
    };
    let positioned = |anchor, gravity| {
        envelope(Body::SurfaceNew(SurfaceNew {
            positioner: Some(Positioner {
                anchor,
                gravity,
                ..popup_positioner()
            }),
            ..surface_new()
        }))
    };
    let bye_with = |reason| {
        envelope(Body::Bye(Bye {
            reason,
            text: String::new(),
        }))
    };
    vec![
        refused(
            "surface-new-scale-zero",
            "SurfaceNew with scale_120ths 0",
            envelope(Body::SurfaceNew(SurfaceNew {
                scale_120ths: 0,
                ..surface_new()
            })),
            Limit("surface_new.scale_120ths"),
        ),
        refused(
            "surface-metadata-scale-zero",
            "SurfaceMetadata with scale_120ths present at 0",
            envelope(Body::SurfaceMetadata(SurfaceMetadata {
                surface_id: 1,
                scale_120ths: Some(0),
                ..SurfaceMetadata::default()
            })),
            Limit("surface_metadata.scale_120ths"),
        ),
        refused(
            "tile-codec-zero",
            "A tile with codec 0, which is never a codec",
            envelope(Body::Frame(Frame {
                surface_id: 1,
                sequence: 1,
                full_redraw: false,
                tiles: vec![Tile {
                    rect: rect(0, 0, 1, 1),
                    codec: 0,
                    data: vec![0; 4],
                }],
            })),
            Limit("tile.codec"),
        ),
        refused(
            "key-code-empty",
            "Key with an empty code: a code is ASCII and never empty",
            key_with(""),
            Limit("key.code"),
        ),
        refused(
            "key-code-not-ascii",
            "Key whose code is valid UTF-8 but not ASCII",
            key_with("κeyA"),
            Limit("key.code"),
        ),
        refused(
            "configure-serial-zero",
            "Configure with serial 0, which is never a host serial",
            envelope(Body::Configure(Configure {
                surface_id: 2,
                serial: 0,
                size: size(800, 600),
            })),
            Limit("configure.serial"),
        ),
        refused(
            "cursor-bytes-not-width-height-4",
            "A 2x2 cursor with 8 bytes where width * height * 4 is 16",
            envelope(Body::CursorImage(CursorImage {
                serial: 1,
                width: 2,
                height: 2,
                argb_premultiplied: vec![0; 8],
                ..CursorImage::default()
            })),
            Limit("cursor_image.argb_premultiplied"),
        ),
        refused(
            "bye-reason-unknown",
            "Bye with reason 999, which ByeReason does not name",
            bye_with(999),
            Protocol("bye.reason"),
        ),
        refused(
            "bye-reason-past-the-set",
            "Bye with reason 0x106, one past the last ByeReason",
            bye_with(0x106),
            Protocol("bye.reason"),
        ),
        refused(
            "bye-reason-below-the-set",
            "Bye with reason 1, between BYE_PEER_CLOSED and the 0x100 range",
            bye_with(1),
            Protocol("bye.reason"),
        ),
        refused(
            "surface-new-role-unknown",
            "SurfaceNew with role 2, which Role does not name",
            envelope(Body::SurfaceNew(SurfaceNew {
                role: 2,
                ..surface_new()
            })),
            Protocol("surface_new.role"),
        ),
        refused(
            "surface-new-role-negative",
            "SurfaceNew with role -1: an int32 enum written as a ten-byte varint, over 32 bits",
            envelope(Body::SurfaceNew(SurfaceNew {
                role: -1,
                ..surface_new()
            })),
            Protocol("surface_new.role"),
        ),
        refused(
            "positioner-anchor-unknown",
            "A positioner whose anchor is 9, which Anchor does not name",
            positioned(9, Anchor::TopRight as i32),
            Protocol("surface_new.positioner.anchor"),
        ),
        refused(
            "positioner-gravity-unknown",
            "A positioner whose gravity is 9, which Anchor does not name",
            positioned(Anchor::BottomLeft as i32, 9),
            Protocol("surface_new.positioner.gravity"),
        ),
        refused(
            "surface-gone-reason-unknown",
            "SurfaceGone with reason 3, which SurfaceGoneReason does not name",
            envelope(Body::SurfaceGone(SurfaceGone {
                surface_id: 1,
                reason: 3,
            })),
            Protocol("surface_gone.reason"),
        ),
    ]
}

/// Bytes that break the grammar, and envelopes with no known body.
#[allow(clippy::too_many_lines)] // One entry per rule of the grammar.
fn grammar_vectors() -> Vec<Invalid> {
    use Refused::{Protocol, Unknown};
    let hello_resume_bytes = envelope(Body::Hello(hello_resume())).encode_to_vec();
    vec![
        broken(
            "two-bodies",
            "A Frame, then a Hello, in one envelope (62 02 22 00, then hello-resume): prost \
             would keep the Hello and drop the Frame unchecked",
            [lfield(12, &[0x22, 0x00]), hello_resume_bytes].concat(),
            Protocol("body"),
        ),
        broken(
            "two-same-bodies",
            "CursorGone twice in one envelope: prost would merge the two into one",
            [lfield(15, &[]), lfield(15, &[])].concat(),
            Protocol("body"),
        ),
        broken(
            "hello-version-over-32-bits",
            "Hello.protocol_version of 2^32: a uint32 varint over 32 bits, which prost would \
             truncate to 0",
            lfield(1, &[0x08, 0x80, 0x80, 0x80, 0x80, 0x10]),
            Protocol("hello.protocol_version"),
        ),
        broken(
            "pointer-move-x-over-32-bits",
            "PointerMove.x as a varint over 32 bits, which prost would truncate",
            lfield(16, &[0x10, 0x80, 0x80, 0x80, 0x80, 0x10]),
            Protocol("pointer_move.x"),
        ),
        broken(
            "frame-ack-varint-over-ten-bytes",
            "FrameAck.sequence as an eleven-byte varint",
            lfield(13, &[&[0x10][..], &[0xff; 10], &[0x01]].concat()),
            Protocol("frame_ack.sequence"),
        ),
        broken(
            "focus-ask-truncated-varint",
            "FocusAsk whose surface_id varint ends with its message",
            lfield(8, &[0x08, 0x80]),
            Protocol("focus_ask.surface_id"),
        ),
        broken(
            "surface-new-title-past-its-message",
            "SurfaceNew.title claiming 5 bytes where 2 remain",
            lfield(5, &[0x2a, 0x05, b'a', b'b']),
            Protocol("surface_new.title"),
        ),
        broken(
            "envelope-body-past-the-input",
            "A Hello body claiming 5 bytes where 2 remain",
            vec![0x0a, 0x05, 0x08, 0x00],
            Protocol("hello"),
        ),
        broken(
            "hello-client-name-wrong-wire-type",
            "Hello.client_name sent as a varint",
            lfield(1, &[0x10, 0x01]),
            Protocol("hello.client_name"),
        ),
        broken(
            "envelope-body-wrong-wire-type",
            "The Hello body sent as a varint",
            vec![0x08, 0x00],
            Protocol("hello"),
        ),
        broken(
            "frame-tiles-wrong-wire-type",
            "Frame.tiles sent as a varint",
            lfield(12, &[0x20, 0x01]),
            Protocol("frame.tiles"),
        ),
        broken(
            "hello-field-zero",
            "A Hello field numbered 0",
            lfield(1, &[0x00, 0x00]),
            Protocol("hello"),
        ),
        broken(
            "hello-group",
            "A Hello carrying an unknown field 9 as a group: a group is refused, not skipped",
            lfield(1, &[0x4b, 0x4c]),
            Protocol("hello"),
        ),
        broken(
            "hello-wire-type-7",
            "A Hello field key with wire type 7, which protobuf does not define",
            lfield(1, &[0x4f]),
            Protocol("hello"),
        ),
        broken(
            "hello-client-name-not-utf8",
            "Hello.client_name holding the byte ff",
            lfield(1, &[0x12, 0x01, 0xff]),
            Protocol("hello.client_name"),
        ),
        broken(
            "surface-new-title-twice",
            "SurfaceNew.title twice: a field that is not repeated appears at most once",
            lfield(
                5,
                &[lfield(5, b"a"), lfield(5, b"b"), vfield(8, 120)].concat(),
            ),
            Protocol("surface_new.title"),
        ),
        broken(
            "surface-new-size-twice",
            "SurfaceNew.size twice: prost would merge the two, a replacing decoder would not",
            lfield(
                5,
                &[
                    lfield(4, &vfield(1, 1)),
                    lfield(4, &vfield(2, 2)),
                    vfield(8, 120),
                ]
                .concat(),
            ),
            Protocol("surface_new.size"),
        ),
        broken(
            "hello-codecs-truncated-packed",
            "A packed Hello.codecs run whose second varint ends with the run",
            lfield(1, &lfield(4, &[0x01, 0x80])),
            Protocol("hello.codecs"),
        ),
        broken(
            "empty-envelope",
            "No bytes at all: an envelope with no body",
            Vec::new(),
            Unknown,
        ),
        broken(
            "unknown-body",
            "Envelope field 25, which names no v0 message",
            lfield(25, &[]),
            Unknown,
        ),
        broken(
            "unknown-body-after-a-known-one",
            "CursorGone, then envelope field 25",
            [lfield(15, &[]), lfield(25, &[])].concat(),
            Unknown,
        ),
        broken(
            "unknown-body-as-varint",
            "Envelope field 25 with wire type 0",
            vfield(25, 0),
            Unknown,
        ),
    ]
}

/// Every `invalid` entry: the caps, the value rules, the grammar.
fn invalid_vectors() -> Vec<Invalid> {
    let mut all = over_cap_vectors();
    all.extend(value_rule_vectors());
    all.extend(grammar_vectors());
    all
}

/// Lowercase hex, no separators.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

/// Escapes a JSON string body. Notes here are ASCII, but escape properly regardless.
fn json_escape(s: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                write!(out, "\\u{:04x}", u32::from(c)).expect("writing to a String cannot fail");
            }
            c => out.push(c),
        }
    }
    out
}

/// One JSON array of the committed file: `"key": [`, one entry per line, `]`.
fn render_array(out: &mut String, key: &str, entries: &[String], last: bool) {
    use std::fmt::Write as _;

    writeln!(out, "  \"{key}\": [").expect("writing to a String cannot fail");
    for (index, entry) in entries.iter().enumerate() {
        let comma = if index + 1 == entries.len() { "" } else { "," };
        writeln!(out, "    {{ {entry} }}{comma}").expect("writing to a String cannot fail");
    }
    out.push_str(if last { "  ]\n" } else { "  ],\n" });
}

/// The `"name", "note", "hex"` members every entry starts with.
fn head(name: &str, note: &str, bytes: &[u8]) -> String {
    format!(
        "\"name\": \"{}\", \"note\": \"{}\", \"hex\": \"{}\"",
        json_escape(name),
        json_escape(note),
        hex(bytes)
    )
}

/// Renders the committed file's exact bytes.
fn render(vectors: &[Vector], lenient: &[Lenient], invalid: &[Invalid]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"format\": 2,\n");
    writeln!(
        out,
        "  \"protocol_version\": {},",
        appricot_proto::PROTOCOL_VERSION
    )
    .expect("writing to a String cannot fail");
    let canonical: Vec<String> = vectors
        .iter()
        .map(|v| {
            let bytes = encode_envelope(&v.envelope)
                .unwrap_or_else(|e| panic!("vector {} does not encode: {e}", v.name));
            head(v.name, v.note, &bytes)
        })
        .collect();
    let lenient: Vec<String> = lenient
        .iter()
        .map(|l| {
            let canonical = encode_envelope(&l.canonical)
                .unwrap_or_else(|e| panic!("lenient {} does not encode: {e}", l.name));
            format!(
                "{}, \"canonical\": \"{}\"",
                head(l.name, l.note, &l.bytes),
                hex(&canonical)
            )
        })
        .collect();
    let invalid: Vec<String> = invalid
        .iter()
        .map(|i| {
            format!(
                "{}, \"error\": \"{}\", \"field\": \"{}\"",
                head(i.name, i.note, &i.bytes),
                i.want.error(),
                json_escape(i.want.field())
            )
        })
        .collect();
    render_array(&mut out, "vectors", &canonical, false);
    render_array(&mut out, "lenient", &lenient, false);
    render_array(&mut out, "invalid", &invalid, true);
    out.push('}');
    out.push('\n');
    out
}

#[test]
fn committed_vectors_are_current() {
    let vectors = vectors();
    assert_eq!(
        vectors.len(),
        58,
        "30 first vectors (24 message kinds and six extras) and 28 edge cases"
    );
    let generated = render(&vectors, &lenient_vectors(), &invalid_vectors());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("vectors.json");
    if std::env::var_os("APPRICOT_REGEN_VECTORS").is_some() {
        std::fs::create_dir_all(path.parent().expect("the path has a parent"))
            .expect("create testdata/");
        std::fs::write(&path, generated).expect("rewrite testdata/vectors.json");
        return;
    }
    let committed = std::fs::read_to_string(&path).expect(
        "testdata/vectors.json is missing; run the test once with APPRICOT_REGEN_VECTORS=1",
    );
    assert_eq!(
        committed, generated,
        "testdata/vectors.json does not match the generators; regenerate with: docker compose \
         run -e APPRICOT_REGEN_VECTORS=1 --rm dev cargo test -p appricot-proto --test vectors"
    );
}

#[test]
fn every_vector_round_trips_byte_identically() {
    for vector in vectors() {
        let bytes = encode_envelope(&vector.envelope)
            .unwrap_or_else(|e| panic!("vector {} does not encode: {e}", vector.name));
        let decoded = decode_envelope(&bytes)
            .unwrap_or_else(|e| panic!("vector {} does not decode: {e}", vector.name));
        assert_eq!(decoded, vector.envelope, "vector {} changed", vector.name);
        let again = encode_envelope(&decoded)
            .unwrap_or_else(|e| panic!("vector {} does not re-encode: {e}", vector.name));
        assert_eq!(again, bytes, "vector {} is not byte-stable", vector.name);
    }
}

/// The envelope field number of a body: 1 for Hello through 24 for CloseRequest.
fn body_number(envelope: &Envelope) -> u32 {
    let bytes = envelope.encode_to_vec();
    u32::from(bytes.first().copied().expect("a body")) >> 3
}

#[test]
fn every_message_kind_has_a_canonical_vector() {
    let mut seen = [false; 24];
    for vector in vectors() {
        let number = body_number(&vector.envelope);
        seen[usize::try_from(number - 1).expect("a small number")] = true;
    }
    let missing: Vec<usize> = (1..=24).filter(|n| !seen[n - 1]).collect();
    assert!(
        missing.is_empty(),
        "no vector for envelope fields {missing:?}"
    );
}

#[test]
fn names_are_unique_across_the_file() {
    let mut names: Vec<&str> = vectors().iter().map(|v| v.name).collect();
    names.extend(lenient_vectors().iter().map(|l| l.name));
    names.extend(invalid_vectors().iter().map(|i| i.name));
    let total = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), total, "two entries share a name");
}

#[test]
fn lenient_bytes_decode_to_their_canonical_message() {
    for lenient in lenient_vectors() {
        let canonical = encode_envelope(&lenient.canonical)
            .unwrap_or_else(|e| panic!("lenient {}: canonical does not encode: {e}", lenient.name));
        assert_ne!(
            lenient.bytes, canonical,
            "lenient {} is canonical already; it belongs in `vectors`",
            lenient.name
        );
        let decoded = decode_envelope(&lenient.bytes)
            .unwrap_or_else(|e| panic!("lenient {} is refused: {e}", lenient.name));
        assert_eq!(
            decoded, lenient.canonical,
            "lenient {} decodes wrong",
            lenient.name
        );
    }
}

#[test]
fn invalid_bytes_are_refused_with_the_named_error() {
    for invalid in invalid_vectors() {
        let err = decode_envelope(&invalid.bytes)
            .expect_err(&format!("invalid {} decodes", invalid.name));
        assert!(
            invalid.want.matches(&err),
            "invalid {}: want {:?}, got {err:?}",
            invalid.name,
            invalid.want
        );
    }
}

#[test]
fn every_invalid_message_is_refused_by_the_encoder_too() {
    let mut encodable = 0;
    for invalid in invalid_vectors() {
        let Some(envelope) = invalid.envelope.as_ref() else {
            continue;
        };
        encodable += 1;
        let err =
            encode_envelope(envelope).expect_err(&format!("invalid {} encodes", invalid.name));
        assert!(
            invalid.want.matches_encode(&err),
            "invalid {}: want {:?} on encode, got {err:?}",
            invalid.name,
            invalid.want
        );
        let err =
            validate_envelope(envelope).expect_err(&format!("invalid {} validates", invalid.name));
        assert!(
            invalid.want.matches(&err),
            "invalid {}: want {:?} from validate_envelope, got {err:?}",
            invalid.name,
            invalid.want
        );
    }
    assert!(
        encodable > 40,
        "only {encodable} invalid entries are messages"
    );
}

#[test]
fn every_limit_row_has_its_cap_plus_one() {
    // The rows of the limits table one message can break, by the field path they name. A new
    // row must add an `invalid` entry here and in the generators above.
    let rows = [
        "hello.client_name",
        "hello.stream_token",
        "hello.codecs",
        "hello_reply.session_id",
        "hello_reply.codecs",
        "bye.text",
        "server_error.text",
        "surface_new.title",
        "surface_new.app_id",
        "surface_new.size.width",
        "surface_new.size.height",
        "surface_new.positioner.size.width",
        "surface_new.positioner.size.height",
        "surface_metadata.title",
        "surface_metadata.app_id",
        "resize_ask.size.width",
        "configure.size.height",
        "configure_ack.size.width",
        "frame.tiles",
        "tile.rect.width",
        "tile.rect.height",
        "tile.data",
        "cursor_image.width",
        "cursor_image.height",
        "cursor_image.argb_premultiplied",
        "key.code",
        "clipboard_set.text",
        "pointer_axis.steps_x",
        "pointer_axis.steps_y",
    ];
    let invalid = invalid_vectors();
    for row in rows {
        assert!(
            invalid
                .iter()
                .any(|i| matches!(i.want, Refused::Limit(field) if field == row)),
            "no cap + 1 entry for {row}"
        );
    }
}
