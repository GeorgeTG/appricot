//! The structure-aware decoder fuzzer: seeded, deterministic, and part of `cargo test`.
//!
//! The byte-mutation loop in `src/wire.rs` walks around 25 small valid messages and cannot
//! reach the paths a hostile peer aims at: a repeated count far past its cap, a second body,
//! an enum value v0 does not define. This file generates messages from the schema instead,
//! with values drawn from the edges of every rule (0, 1, the cap, the cap + 1, the extremes of
//! the type), and then mutates their bytes along field boundaries. For every input it checks:
//!
//! - **Agreement.** Bytes prost encodes from a generated message decode if and only if the
//!   message passes `validate_envelope`, and then they decode to that message and re-encode
//!   byte for byte.
//! - **Skipping.** An unknown field added to a message changes nothing a decoder returns.
//! - **One body.** A second body after a valid one is refused as a protocol violation.
//! - **Counts.** More repeated entries than the cap are refused as a limit violation.
//! - **Fixed point.** Whatever decodes, re-encodes, and decodes again to the same message.
//! - **Allocation.** No decode holds more than twice its input plus a small constant, measured
//!   with a counting allocator.
//! - **Coverage.** Every outcome bucket, and every path named above, is reached at least once;
//!   a family that never fires fails the run.
//!
//! The PRNG is xorshift64* with a fixed seed, so every run is the same run.

mod common;

use std::collections::BTreeMap;

use appricot_proto::limits::{
    MAX_APP_ID_BYTES, MAX_BYE_TEXT_BYTES, MAX_CLIENT_NAME_BYTES, MAX_CLIPBOARD_BYTES,
    MAX_CODECS_OFFERED, MAX_CURSOR_BYTES, MAX_ERROR_TEXT_BYTES, MAX_KEY_CODE_BYTES,
    MAX_SESSION_ID_BYTES, MAX_TILE_BYTES, MAX_TILES_PER_FRAME, MAX_TITLE_BYTES, MAX_TOKEN_BYTES,
};
use appricot_proto::wire::{
    Anchor, BlurRelease, Body, Bye, ByeReason, ClipboardAsk, ClipboardSet, ClipboardText,
    CloseRequest, Configure, ConfigureAck, CursorGone, CursorImage, DecodeError, Envelope,
    FocusAsk, FocusNotify, Frame, FrameAck, Hello, HelloReply, Key, Point, PointerAxis,
    PointerButton, PointerMove, Positioner, Rect, ResizeAsk, Role, ServerError, Size, SurfaceGone,
    SurfaceGoneReason, SurfaceMetadata, SurfaceNew, Tile, decode_envelope, encode_envelope,
    validate_envelope,
};
use prost::Message;

#[global_allocator]
static ALLOCATOR: common::Counting = common::Counting;

/// The fixed seed. Change it and the whole run changes; that is the point.
const SEED: u64 = 0x0a99_1c07_2026_0923;
/// Generated messages, each checked and then mutated along its fields.
const ITERATIONS: usize = 20_000;
/// No decode may hold more than twice its input plus this many bytes.
const ALLOCATION_SLACK: usize = 16 * 1024;

/// xorshift64*: a deterministic PRNG with no dependency. Truncation is the point: a PRNG hands
/// out bits of its state.
struct Rng(u64);

#[allow(clippy::cast_possible_truncation)]
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// True one time in `n`.
    fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    fn pick<T: Copy>(&mut self, from: &[T]) -> T {
        from[self.below(from.len())]
    }
}

// ---------------------------------------------------------------------------------------------
// Generators: every field drawn from the edges of its rules
// ---------------------------------------------------------------------------------------------

fn any_u32(rng: &mut Rng) -> u32 {
    if rng.one_in(4) {
        rng.u32()
    } else {
        rng.pick(&[
            0,
            1,
            2,
            127,
            128,
            16_383,
            16_384,
            0x7fff_ffff,
            0x8000_0000,
            u32::MAX,
        ])
    }
}

fn any_i32(rng: &mut Rng) -> i32 {
    if rng.one_in(4) {
        i32::from_ne_bytes(rng.u32().to_ne_bytes())
    } else {
        rng.pick(&[0, 1, -1, 2, -2, 63, 64, 65, -64, -65, i32::MIN, i32::MAX])
    }
}

/// A pixel dimension around the surface, tile and cursor caps.
fn any_dim(rng: &mut Rng) -> u32 {
    if rng.one_in(8) {
        rng.u32()
    } else {
        rng.pick(&[
            0, 1, 2, 127, 128, 129, 255, 256, 257, 1199, 1200, 1201, 1919, 1920, 1921,
        ])
    }
}

/// A length at the edges of `cap`; long ones are rare, so the run stays fast.
fn any_len(rng: &mut Rng, cap: usize) -> usize {
    if cap > 4096 && !rng.one_in(64) {
        return rng.below(48);
    }
    match rng.below(6) {
        0 => 0,
        1 => 1,
        2 => cap,
        3 => cap + 1,
        4 => cap.saturating_sub(1),
        _ => rng.below(cap + 2),
    }
}

/// A string of exactly `len` bytes, sometimes with two-byte Greek letters in it.
fn text(rng: &mut Rng, len: usize) -> String {
    let greek = if rng.one_in(3) { len / 4 } else { 0 };
    let mut out = "α".repeat(greek);
    out.push_str(&"x".repeat(len - greek * 2));
    out
}

fn any_text(rng: &mut Rng, cap: usize) -> String {
    let len = any_len(rng, cap);
    text(rng, len)
}

fn any_bytes(rng: &mut Rng, cap: usize) -> Vec<u8> {
    let len = any_len(rng, cap);
    vec![rng.pick(&[0x00, 0x5a, 0xff]); len]
}

/// An enum value: usually one v0 defines, sometimes one it does not.
fn any_enum(rng: &mut Rng, defined: &[i32]) -> i32 {
    if rng.one_in(6) {
        let past = defined.iter().copied().max().unwrap_or(0) + 1;
        rng.pick(&[past, 999, -1, i32::MIN, i32::MAX])
    } else {
        rng.pick(defined)
    }
}

const ROLES: &[i32] = &[Role::Toplevel as i32, Role::Popup as i32];
const ANCHORS: &[i32] = &[
    Anchor::Center as i32,
    Anchor::Top as i32,
    Anchor::Bottom as i32,
    Anchor::Left as i32,
    Anchor::Right as i32,
    Anchor::TopLeft as i32,
    Anchor::BottomLeft as i32,
    Anchor::TopRight as i32,
    Anchor::BottomRight as i32,
];
const BYE_REASONS: &[i32] = &[
    ByeReason::ByePeerClosed as i32,
    ByeReason::ByeProtocolVersion as i32,
    ByeReason::ByeLimitViolation as i32,
    ByeReason::ByeAuthFailed as i32,
    ByeReason::ByeProtocolViolation as i32,
    ByeReason::ByeSessionGone as i32,
    ByeReason::ByeServerShutdown as i32,
];
const GONE_REASONS: &[i32] = &[
    SurfaceGoneReason::GoneAppClosed as i32,
    SurfaceGoneReason::GoneParentGone as i32,
    SurfaceGoneReason::GoneSessionEnd as i32,
];

fn any_size(rng: &mut Rng) -> Option<Size> {
    (!rng.one_in(8)).then(|| Size {
        width: any_dim(rng),
        height: any_dim(rng),
    })
}

fn any_rect(rng: &mut Rng) -> Option<Rect> {
    (!rng.one_in(8)).then(|| Rect {
        x: any_i32(rng),
        y: any_i32(rng),
        width: any_dim(rng),
        height: any_dim(rng),
    })
}

fn any_positioner(rng: &mut Rng) -> Option<Positioner> {
    rng.one_in(2).then(|| Positioner {
        anchor_rect: any_rect(rng),
        anchor: any_enum(rng, ANCHORS),
        gravity: any_enum(rng, ANCHORS),
        offset: (!rng.one_in(4)).then(|| Point {
            x: any_i32(rng),
            y: any_i32(rng),
        }),
        size: any_size(rng),
    })
}

fn any_codecs(rng: &mut Rng) -> Vec<u32> {
    let count = rng.pick(&[0, 1, 2, MAX_CODECS_OFFERED, MAX_CODECS_OFFERED + 1, 40]);
    (0..count).map(|_| rng.pick(&[1, 2, 3, u32::MAX])).collect()
}

fn any_tile(rng: &mut Rng) -> Tile {
    Tile {
        rect: any_rect(rng),
        codec: rng.pick(&[0, 1, 1, 2, 3, u32::MAX]),
        data: any_bytes(rng, MAX_TILE_BYTES),
    }
}

fn any_cursor(rng: &mut Rng) -> CursorImage {
    let width = rng.pick(&[0, 1, 2, 16, 127, 128, 129]);
    let height = rng.pick(&[0, 1, 2, 16, 127, 128, 129]);
    let exact = usize::try_from(width * height * 4).expect("small");
    let len = match rng.below(4) {
        0 => any_len(rng, MAX_CURSOR_BYTES),
        1 => exact.saturating_sub(4),
        _ => exact,
    };
    CursorImage {
        serial: any_u32(rng),
        width,
        height,
        hotspot_x: any_u32(rng),
        hotspot_y: any_u32(rng),
        argb_premultiplied: vec![0x80; len],
    }
}

fn any_key_code(rng: &mut Rng) -> String {
    let long = "K".repeat(MAX_KEY_CODE_BYTES);
    let longer = "K".repeat(MAX_KEY_CODE_BYTES + 1);
    let codes = [
        "",
        "KeyA",
        "Enter",
        "Unidentified",
        "κeyA",
        long.as_str(),
        longer.as_str(),
    ];
    rng.pick(&codes).to_owned()
}

/// Hello, HelloReply, Bye, ServerError and the surface messages.
fn any_handshake_or_surface(rng: &mut Rng, kind: usize) -> Body {
    match kind {
        0 => Body::Hello(Hello {
            protocol_version: any_u32(rng),
            client_name: any_text(rng, MAX_CLIENT_NAME_BYTES),
            stream_token: any_bytes(rng, MAX_TOKEN_BYTES),
            codecs: any_codecs(rng),
            resume_serial: rng.one_in(2).then(|| any_u32(rng)),
        }),
        1 => Body::HelloReply(HelloReply {
            protocol_version: any_u32(rng),
            session_id: any_text(rng, MAX_SESSION_ID_BYTES),
            max_frame_credits: any_u32(rng),
            codecs: any_codecs(rng),
            resumed: rng.one_in(2),
            resume_serial: rng.one_in(2).then(|| any_u32(rng)),
            resume_grace_ms: rng.one_in(2).then(|| any_u32(rng)),
        }),
        2 => Body::Bye(Bye {
            reason: any_enum(rng, BYE_REASONS),
            text: any_text(rng, MAX_BYE_TEXT_BYTES),
        }),
        3 => Body::ServerError(ServerError {
            code: any_u32(rng),
            text: any_text(rng, MAX_ERROR_TEXT_BYTES),
        }),
        4 => Body::SurfaceNew(SurfaceNew {
            surface_id: any_u32(rng),
            role: any_enum(rng, ROLES),
            parent_id: rng.one_in(2).then(|| any_u32(rng)),
            size: any_size(rng),
            title: any_text(rng, MAX_TITLE_BYTES),
            app_id: any_text(rng, MAX_APP_ID_BYTES),
            positioner: any_positioner(rng),
            scale_120ths: rng.pick(&[0, 120, 240, u32::MAX]),
        }),
        5 => Body::SurfaceGone(SurfaceGone {
            surface_id: any_u32(rng),
            reason: any_enum(rng, GONE_REASONS),
        }),
        6 => Body::SurfaceMetadata(SurfaceMetadata {
            surface_id: any_u32(rng),
            title: rng.one_in(2).then(|| any_text(rng, MAX_TITLE_BYTES)),
            app_id: rng.one_in(2).then(|| any_text(rng, MAX_APP_ID_BYTES)),
            scale_120ths: rng.one_in(2).then(|| rng.pick(&[0, 120, 240])),
        }),
        7 => Body::FocusAsk(FocusAsk {
            surface_id: any_u32(rng),
        }),
        8 => Body::ResizeAsk(ResizeAsk {
            surface_id: any_u32(rng),
            size: any_size(rng),
        }),
        9 => Body::Configure(Configure {
            surface_id: any_u32(rng),
            serial: any_u32(rng),
            size: any_size(rng),
        }),
        _ => Body::ConfigureAck(ConfigureAck {
            surface_id: any_u32(rng),
            serial: any_u32(rng),
            size: any_size(rng),
        }),
    }
}

/// Frame, FrameAck, the cursor messages and the input messages.
fn any_frame_or_input(rng: &mut Rng, kind: usize) -> Body {
    match kind {
        11 => {
            let count = rng.pick(&[0, 1, 2, MAX_TILES_PER_FRAME, MAX_TILES_PER_FRAME + 1, 300]);
            Body::Frame(Frame {
                surface_id: any_u32(rng),
                sequence: any_u32(rng),
                full_redraw: rng.one_in(2),
                tiles: (0..count).map(|_| any_tile(rng)).collect(),
            })
        }
        12 => Body::FrameAck(FrameAck {
            surface_id: any_u32(rng),
            sequence: any_u32(rng),
        }),
        13 => Body::CursorImage(any_cursor(rng)),
        14 => Body::CursorGone(CursorGone {}),
        15 => Body::PointerMove(PointerMove {
            surface_id: any_u32(rng),
            x: any_i32(rng),
            y: any_i32(rng),
        }),
        16 => Body::PointerButton(PointerButton {
            surface_id: any_u32(rng),
            button: any_u32(rng),
            pressed: rng.one_in(2),
        }),
        17 => Body::PointerAxis(PointerAxis {
            surface_id: any_u32(rng),
            steps_x: any_i32(rng),
            steps_y: any_i32(rng),
        }),
        18 => Body::Key(Key {
            keysym: any_u32(rng),
            code: any_key_code(rng),
            pressed: rng.one_in(2),
            modifiers: any_u32(rng),
        }),
        19 => Body::FocusNotify(FocusNotify {
            surface_id: any_u32(rng),
        }),
        20 => Body::BlurRelease(BlurRelease {}),
        21 => Body::ClipboardSet(ClipboardSet {
            text: any_text(rng, MAX_CLIPBOARD_BYTES),
        }),
        22 => Body::ClipboardAsk(ClipboardAsk {}),
        23 => Body::CloseRequest(CloseRequest {
            surface_id: any_u32(rng),
        }),
        _ => Body::ClipboardText(ClipboardText {
            text: any_text(rng, MAX_CLIPBOARD_BYTES),
        }),
    }
}

fn any_envelope(rng: &mut Rng) -> Envelope {
    let kind = rng.below(25);
    let body = if kind <= 10 {
        any_handshake_or_surface(rng, kind)
    } else {
        any_frame_or_input(rng, kind)
    };
    Envelope { body: Some(body) }
}

// ---------------------------------------------------------------------------------------------
// Bytes, outcomes and the invariants
// ---------------------------------------------------------------------------------------------

/// A protobuf varint.
fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(u8::try_from(value & 0x7f).expect("seven bits") | 0x80);
        value >>= 7;
    }
    out.push(u8::try_from(value).expect("under 0x80"));
}

/// The envelope field number and the encoded body of an envelope, split apart so mutations
/// can reach inside the body.
fn split(envelope: &Envelope) -> (u32, Vec<u8>) {
    let bytes = envelope.encode_to_vec();
    let mut at = 0;
    let mut key = 0u64;
    let mut shift = 0;
    loop {
        let byte = bytes[at];
        key |= u64::from(byte & 0x7f) << shift;
        at += 1;
        shift += 7;
        if byte < 0x80 {
            break;
        }
    }
    let mut len = 0u64;
    shift = 0;
    loop {
        let byte = bytes[at];
        len |= u64::from(byte & 0x7f) << shift;
        at += 1;
        shift += 7;
        if byte < 0x80 {
            break;
        }
    }
    assert_eq!(
        usize::try_from(len).expect("small"),
        bytes.len() - at,
        "one body"
    );
    (
        u32::try_from(key >> 3).expect("a field number"),
        bytes[at..].to_vec(),
    )
}

/// An envelope field around `body`.
fn wrap(number: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    varint(u64::from((number << 3) | 2), &mut out);
    varint(body.len() as u64, &mut out);
    out.extend_from_slice(body);
    out
}

/// One unknown field, of a number no v0 message uses (9 and above) and a random wire type.
fn unknown_field(rng: &mut Rng) -> Vec<u8> {
    let number = u64::try_from(9 + rng.below(40)).expect("small");
    let mut out = Vec::new();
    match rng.below(4) {
        0 => {
            varint(number << 3, &mut out);
            varint(rng.next_u64() >> rng.below(64), &mut out);
        }
        1 => {
            varint((number << 3) | 1, &mut out);
            out.extend_from_slice(&rng.next_u64().to_le_bytes());
        }
        2 => {
            varint((number << 3) | 2, &mut out);
            let len = rng.below(24);
            varint(len as u64, &mut out);
            out.extend(std::iter::repeat_n(0xfe, len));
        }
        _ => {
            varint((number << 3) | 5, &mut out);
            out.extend_from_slice(&rng.u32().to_le_bytes());
        }
    }
    out
}

/// The name of an outcome, for the coverage buckets.
fn outcome(result: &Result<Envelope, DecodeError>) -> String {
    match result {
        Ok(_) => "ok".into(),
        Err(DecodeError::TooLarge) => "too-large".into(),
        Err(DecodeError::LimitViolation { field }) => format!("limit {field}"),
        Err(DecodeError::ProtocolViolation { field }) => format!("protocol {field}"),
        Err(DecodeError::UnknownMessage) => "unknown-message".into(),
        Err(DecodeError::Decode(e)) => format!("prost {e}"),
    }
}

/// Everything the run counted.
#[derive(Default)]
struct Run {
    buckets: BTreeMap<String, usize>,
    unknown_fields_skipped: usize,
    second_bodies_refused: usize,
    failures: Vec<String>,
}

impl Run {
    /// Decodes `bytes` under the allocation bound and the fixed-point rule, and counts it.
    fn decode(&mut self, label: &str, bytes: &[u8]) -> Result<Envelope, DecodeError> {
        let (result, peak) = common::peak_during(|| decode_envelope(bytes));
        let bound = bytes.len() * 2 + ALLOCATION_SLACK;
        if peak > bound {
            self.failures.push(format!(
                "{label}: decoding {} bytes held {peak} bytes, over {bound}",
                bytes.len()
            ));
        }
        if let Err(DecodeError::Decode(e)) = &result {
            self.failures.push(format!(
                "{label}: prost refused what the pre-scan passed: {e}"
            ));
        }
        if let Ok(envelope) = &result {
            match encode_envelope(envelope) {
                Ok(again) => {
                    if decode_envelope(&again).as_ref().ok() != Some(envelope) {
                        self.failures
                            .push(format!("{label}: decode(encode(x)) is not x"));
                    }
                }
                Err(e) => self
                    .failures
                    .push(format!("{label}: decoded, then refused to encode: {e}")),
            }
        }
        *self.buckets.entry(outcome(&result)).or_default() += 1;
        result
    }
}

/// Damages `body` in one of four ways: flip a bit, cut the tail, duplicate a chunk, or turn
/// the first continuation byte into `ff`, which inflates a length or a varint.
fn mutate(rng: &mut Rng, body: &mut Vec<u8>) {
    match rng.below(4) {
        0 if !body.is_empty() => {
            let at = rng.below(body.len());
            body[at] ^= 1 << rng.below(8);
        }
        1 if !body.is_empty() => body.truncate(rng.below(body.len())),
        2 if !body.is_empty() => {
            let start = rng.below(body.len());
            let len = 1 + rng.below((body.len() - start).min(32));
            let chunk = body[start..start + len].to_vec();
            body.extend_from_slice(&chunk);
        }
        _ => {
            if let Some(at) = body.iter().position(|&b| b >= 0x80) {
                body[at] = 0xff;
            }
        }
    }
}

#[test]
fn generated_and_mutated_messages_keep_every_invariant() {
    let mut rng = Rng(SEED);
    let mut run = Run::default();
    let mut previous: Option<Vec<u8>> = None;

    for iteration in 0..ITERATIONS {
        let envelope = any_envelope(&mut rng);
        let bytes = envelope.encode_to_vec();
        let label = format!("iteration {iteration}");

        // Agreement: the raw prost bytes decode exactly when the message validates.
        let decoded = run.decode(&label, &bytes);
        match (&decoded, validate_envelope(&envelope)) {
            (Ok(back), Ok(())) => {
                if back != &envelope {
                    run.failures
                        .push(format!("{label}: decoded to another message"));
                }
                if encode_envelope(back).as_deref() != Ok(bytes.as_slice()) {
                    run.failures.push(format!("{label}: not byte-identical"));
                }
            }
            (Err(_), Err(_)) => {}
            (d, v) => run.failures.push(format!(
                "{label}: decode says {}, validate says {v:?}",
                outcome(d)
            )),
        }

        let (number, body) = split(&envelope);

        // An unknown body: the same payload under an envelope field no v0 message has.
        // Field 25 is `clipboard_text`, so the probe starts one past the last defined.
        if iteration % 32 == 0 {
            let unknown = 26 + u32::try_from(rng.below(1000)).expect("small");
            let result = run.decode(&format!("{label} as body {unknown}"), &wrap(unknown, &body));
            if !matches!(result, Err(DecodeError::UnknownMessage)) {
                run.failures
                    .push(format!("{label}: body {unknown} gave {}", outcome(&result)));
            }
        }

        // Skipping: an unknown field at the start or the end of the body changes nothing.
        let extra = unknown_field(&mut rng);
        let mut padded = if rng.one_in(2) {
            [extra.as_slice(), body.as_slice()].concat()
        } else {
            [body.as_slice(), extra.as_slice()].concat()
        };
        let with_unknown = run.decode(&format!("{label} + unknown field"), &wrap(number, &padded));
        match (&decoded, &with_unknown) {
            (Ok(a), Ok(b)) if a == b => run.unknown_fields_skipped += 1,
            (Err(_), Err(_)) => {}
            _ => run
                .failures
                .push(format!("{label}: an unknown field changed the outcome")),
        }

        // One body: a valid body followed by any second body is a protocol violation.
        if let Some(other) = previous.replace(bytes.clone()) {
            let mut two = bytes.clone();
            two.extend_from_slice(&other);
            let result = run.decode(&format!("{label} + second body"), &two);
            match (&decoded, &result) {
                (Ok(_), Err(DecodeError::ProtocolViolation { field: "body" })) => {
                    run.second_bodies_refused += 1;
                }
                (Ok(_), _) => run
                    .failures
                    .push(format!("{label}: a second body gave {}", outcome(&result))),
                (Err(_), _) if result.is_ok() => run
                    .failures
                    .push(format!("{label}: a second body made a refusal pass")),
                _ => {}
            }
        }

        mutate(&mut rng, &mut padded);
        let _ = run.decode(&format!("{label} mutated"), &wrap(number, &padded));
    }

    // Counts: a repeated field flooded far past its cap, packed and not.
    let tiles = [0x22u8, 0x00].repeat(100_000);
    let flood = run.decode("tile flood", &wrap(12, &tiles));
    assert!(matches!(
        flood,
        Err(DecodeError::LimitViolation {
            field: "frame.tiles"
        })
    ));
    let codecs = [0x20u8, 0x01].repeat(1000);
    let flood = run.decode("codec flood", &wrap(1, &codecs));
    assert!(matches!(
        flood,
        Err(DecodeError::LimitViolation {
            field: "hello.codecs"
        })
    ));

    let report = format!(
        "{} failures; buckets {:?}; unknown fields skipped {}; second bodies refused {}",
        run.failures.len(),
        run.buckets,
        run.unknown_fields_skipped,
        run.second_bodies_refused
    );
    assert!(
        run.failures.is_empty(),
        "{report}\nfirst failures: {:#?}",
        &run.failures[..run.failures.len().min(8)]
    );

    assert_coverage(&run, &report);
}

/// The fuzzer reaches what it claims to.
fn assert_coverage(run: &Run, report: &str) {
    let reached = |prefix: &str| run.buckets.keys().any(|k| k.starts_with(prefix));
    for needed in [
        "ok",
        "unknown-message",
        "limit frame.tiles",
        "limit hello.codecs",
        "limit tile.data",
        "limit key.code",
        "limit configure.serial",
        "limit pointer_axis.steps",
        "limit cursor_image.argb_premultiplied",
        "limit surface_new.size",
        "protocol body",
        "protocol bye.reason",
        "protocol surface_new.role",
        "protocol surface_new.positioner.anchor",
        "protocol surface_gone.reason",
    ] {
        assert!(
            reached(needed),
            "the run never reached {needed:?}; {report}"
        );
    }
    assert!(run.unknown_fields_skipped > ITERATIONS / 4, "{report}");
    assert!(run.second_bodies_refused > ITERATIONS / 4, "{report}");
}
