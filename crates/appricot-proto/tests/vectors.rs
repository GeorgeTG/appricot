//! The shared test vectors: `testdata/vectors.json` must equal exactly what this file
//! generates.
//!
//! The TypeScript codec in `packages/client` consumes the committed file, so its format is
//! frozen: top-level `format` (always 1 for now), `protocol_version`, then `vectors`, one
//! per line, each `{ "name", "note", "hex" }` where `hex` is one full Envelope-encoded
//! message in lowercase hex. Fields are emitted in field-number order, so a TypeScript
//! encoder that does the same is byte-identical.
//!
//! Regenerate after changing a generator here:
//!
//! ```sh
//! docker compose run -e APPRICOT_REGEN_VECTORS=1 --rm dev \
//!   cargo test -p appricot-proto --test vectors
//! ```
//!
//! and commit the file together with the change.

use appricot_proto::limits::codec;
use appricot_proto::wire::*;

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
            note: "Two discrete wheel steps down, none sideways",
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

/// Every vector, in a stable order: handshake, surfaces, frames, input.
fn vectors() -> Vec<Vector> {
    let mut all = Vec::new();
    all.extend(handshake_vectors());
    all.extend(surface_vectors());
    all.extend(frame_vectors());
    all.extend(input_vectors());
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

/// Renders the committed file's exact bytes.
fn render(vectors: &[Vector]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"format\": 1,\n");
    writeln!(
        out,
        "  \"protocol_version\": {},",
        appricot_proto::PROTOCOL_VERSION
    )
    .expect("writing to a String cannot fail");
    out.push_str("  \"vectors\": [\n");
    for (index, vector) in vectors.iter().enumerate() {
        let bytes = encode_envelope(&vector.envelope)
            .unwrap_or_else(|e| panic!("vector {} does not encode: {e}", vector.name));
        writeln!(
            out,
            "    {{ \"name\": \"{}\", \"note\": \"{}\", \"hex\": \"{}\" }}{}",
            json_escape(vector.name),
            json_escape(vector.note),
            hex(&bytes),
            if index + 1 == vectors.len() { "" } else { "," },
        )
        .expect("writing to a String cannot fail");
    }
    out.push_str("  ]\n");
    out.push('}');
    out.push('\n');
    out
}

#[test]
fn committed_vectors_are_current() {
    let vectors = vectors();
    assert_eq!(vectors.len(), 30, "24 message kinds plus the named extras");
    let generated = render(&vectors);
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
