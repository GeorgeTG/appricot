//! The pre-scan: a walk over the raw bytes of one message that allocates nothing and runs
//! before prost does.
//!
//! prost builds every repeated element and copies every string while it parses. A check on
//! the decoded [`Envelope`](crate::wire::Envelope) therefore comes too late for a count or a
//! length: an unauthenticated peer could make prost build millions of empty tiles before the
//! count was ever compared with its cap. This module walks the input first, with the message
//! set of `wire.proto` written out as the static schema below, and refuses:
//!
//! - a length or a count over its row of the limits table, at cap + 1, before the payload is
//!   touched (a length is compared with its cap before it is compared with the input);
//! - anything that is not exactly one known envelope body;
//! - bytes that break the grammar: a truncated varint or field, a length that runs past its
//!   message, a wrong wire type on a known field, a varint over 32 bits on a known field, a
//!   field number 0, a group (wire type 3 or 4), a wire type 6 or 7, invalid UTF-8 in a string,
//!   or a singular field that appears twice.
//!
//! An unknown field inside a known message is skipped by length, bounded by the message it sits
//! in (v0.md §13: a new field is an additive change). It is never looked into.
//!
//! What it does not check are the rules on decoded values (enum sets, dimensions, a zero scale,
//! an empty key code, a serial of 0): those need no allocation to enforce, so
//! [`crate::wire`] checks them after prost, on the typed message.
//!
//! The walk keeps a few integers per nesting level and the schema is at most four levels deep,
//! so its cost is one pass over the input and its memory is a constant.

use crate::limits::{
    MAX_APP_ID_BYTES, MAX_BYE_TEXT_BYTES, MAX_CLIENT_NAME_BYTES, MAX_CLIPBOARD_BYTES,
    MAX_CODECS_OFFERED, MAX_CURSOR_BYTES, MAX_ERROR_TEXT_BYTES, MAX_KEY_CODE_BYTES,
    MAX_SESSION_ID_BYTES, MAX_TILE_BYTES, MAX_TILES_PER_FRAME, MAX_TITLE_BYTES, MAX_TOKEN_BYTES,
};

/// Why the pre-scan refused the input. The field path names where, as in [`crate::wire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A length or a count over its cap in the limits table.
    Limit(&'static str),
    /// Bytes that break the v0 grammar.
    Grammar(&'static str),
    /// No body at all, or an envelope field that names no v0 message.
    UnknownMessage,
}

/// Protobuf wire types. 3 and 4 open and close a group, which v0 never uses; 6 and 7 are not
/// defined at all.
const VARINT: u8 = 0;
const FIXED64: u8 = 1;
const LEN: u8 = 2;
const FIXED32: u8 = 5;

/// What one known field holds, and the cap on it.
#[derive(Clone, Copy)]
enum Kind {
    /// `uint32`, `sint32`, `bool` or an enum: one varint that fits 32 bits.
    Varint,
    /// A UTF-8 `string` of at most this many bytes.
    Str(usize),
    /// `bytes`, at most this many.
    Bytes(usize),
    /// One embedded message.
    Message(&'static Schema),
    /// A `repeated` embedded message, at most this many entries.
    Messages(&'static Schema, usize),
    /// A `repeated uint32`, packed or not, at most this many entries in all.
    Varints(usize),
}

/// One field of the schema: its number, the path an error names, and what it holds.
struct Field {
    number: u32,
    path: &'static str,
    kind: Kind,
}

/// One message of the schema.
struct Schema {
    /// The path an error inside this message names when no field is to blame.
    path: &'static str,
    fields: &'static [Field],
}

impl Schema {
    /// The known field with this number, and its index in `fields`.
    fn field(&self, number: u32) -> Option<(usize, &Field)> {
        self.fields
            .iter()
            .enumerate()
            .find(|(_, f)| f.number == number)
    }
}

/// The most fields any v0 message declares (SurfaceNew has eight); the per-message counters
/// below are sized by it.
const MAX_FIELDS: usize = 8;

const fn field(number: u32, path: &'static str, kind: Kind) -> Field {
    Field { number, path, kind }
}

// ---------------------------------------------------------------------------------------------
// The schema: wire.proto, field for field. `schema_matches_wire_proto` below compares the two.
// ---------------------------------------------------------------------------------------------

static POINT: Schema = Schema {
    path: "point",
    fields: &[
        field(1, "point.x", Kind::Varint),
        field(2, "point.y", Kind::Varint),
    ],
};

static SIZE: Schema = Schema {
    path: "size",
    fields: &[
        field(1, "size.width", Kind::Varint),
        field(2, "size.height", Kind::Varint),
    ],
};

static RECT: Schema = Schema {
    path: "rect",
    fields: &[
        field(1, "rect.x", Kind::Varint),
        field(2, "rect.y", Kind::Varint),
        field(3, "rect.width", Kind::Varint),
        field(4, "rect.height", Kind::Varint),
    ],
};

static POSITIONER: Schema = Schema {
    path: "positioner",
    fields: &[
        field(1, "positioner.anchor_rect", Kind::Message(&RECT)),
        field(2, "positioner.anchor", Kind::Varint),
        field(3, "positioner.gravity", Kind::Varint),
        field(4, "positioner.offset", Kind::Message(&POINT)),
        field(5, "positioner.size", Kind::Message(&SIZE)),
    ],
};

static HELLO: Schema = Schema {
    path: "hello",
    fields: &[
        field(1, "hello.protocol_version", Kind::Varint),
        field(2, "hello.client_name", Kind::Str(MAX_CLIENT_NAME_BYTES)),
        field(3, "hello.stream_token", Kind::Bytes(MAX_TOKEN_BYTES)),
        field(4, "hello.codecs", Kind::Varints(MAX_CODECS_OFFERED)),
        field(5, "hello.resume_serial", Kind::Varint),
    ],
};

static HELLO_REPLY: Schema = Schema {
    path: "hello_reply",
    fields: &[
        field(1, "hello_reply.protocol_version", Kind::Varint),
        field(2, "hello_reply.session_id", Kind::Str(MAX_SESSION_ID_BYTES)),
        field(3, "hello_reply.max_frame_credits", Kind::Varint),
        field(4, "hello_reply.codecs", Kind::Varints(MAX_CODECS_OFFERED)),
        field(5, "hello_reply.resumed", Kind::Varint),
        field(6, "hello_reply.resume_serial", Kind::Varint),
        field(7, "hello_reply.resume_grace_ms", Kind::Varint),
    ],
};

static BYE: Schema = Schema {
    path: "bye",
    fields: &[
        field(1, "bye.reason", Kind::Varint),
        field(2, "bye.text", Kind::Str(MAX_BYE_TEXT_BYTES)),
    ],
};

static SERVER_ERROR: Schema = Schema {
    path: "server_error",
    fields: &[
        field(1, "server_error.code", Kind::Varint),
        field(2, "server_error.text", Kind::Str(MAX_ERROR_TEXT_BYTES)),
    ],
};

static SURFACE_NEW: Schema = Schema {
    path: "surface_new",
    fields: &[
        field(1, "surface_new.surface_id", Kind::Varint),
        field(2, "surface_new.role", Kind::Varint),
        field(3, "surface_new.parent_id", Kind::Varint),
        field(4, "surface_new.size", Kind::Message(&SIZE)),
        field(5, "surface_new.title", Kind::Str(MAX_TITLE_BYTES)),
        field(6, "surface_new.app_id", Kind::Str(MAX_APP_ID_BYTES)),
        field(7, "surface_new.positioner", Kind::Message(&POSITIONER)),
        field(8, "surface_new.scale_120ths", Kind::Varint),
    ],
};

static SURFACE_GONE: Schema = Schema {
    path: "surface_gone",
    fields: &[
        field(1, "surface_gone.surface_id", Kind::Varint),
        field(2, "surface_gone.reason", Kind::Varint),
    ],
};

static SURFACE_METADATA: Schema = Schema {
    path: "surface_metadata",
    fields: &[
        field(1, "surface_metadata.surface_id", Kind::Varint),
        field(2, "surface_metadata.title", Kind::Str(MAX_TITLE_BYTES)),
        field(3, "surface_metadata.app_id", Kind::Str(MAX_APP_ID_BYTES)),
        field(4, "surface_metadata.scale_120ths", Kind::Varint),
    ],
};

static FOCUS_ASK: Schema = Schema {
    path: "focus_ask",
    fields: &[field(1, "focus_ask.surface_id", Kind::Varint)],
};

static RESIZE_ASK: Schema = Schema {
    path: "resize_ask",
    fields: &[
        field(1, "resize_ask.surface_id", Kind::Varint),
        field(2, "resize_ask.size", Kind::Message(&SIZE)),
    ],
};

static CONFIGURE: Schema = Schema {
    path: "configure",
    fields: &[
        field(1, "configure.surface_id", Kind::Varint),
        field(2, "configure.serial", Kind::Varint),
        field(3, "configure.size", Kind::Message(&SIZE)),
    ],
};

static CONFIGURE_ACK: Schema = Schema {
    path: "configure_ack",
    fields: &[
        field(1, "configure_ack.surface_id", Kind::Varint),
        field(2, "configure_ack.serial", Kind::Varint),
        field(3, "configure_ack.size", Kind::Message(&SIZE)),
    ],
};

static TILE: Schema = Schema {
    path: "tile",
    fields: &[
        field(1, "tile.rect", Kind::Message(&RECT)),
        field(2, "tile.codec", Kind::Varint),
        field(3, "tile.data", Kind::Bytes(MAX_TILE_BYTES)),
    ],
};

static FRAME: Schema = Schema {
    path: "frame",
    fields: &[
        field(1, "frame.surface_id", Kind::Varint),
        field(2, "frame.sequence", Kind::Varint),
        field(3, "frame.full_redraw", Kind::Varint),
        field(4, "frame.tiles", Kind::Messages(&TILE, MAX_TILES_PER_FRAME)),
    ],
};

static FRAME_ACK: Schema = Schema {
    path: "frame_ack",
    fields: &[
        field(1, "frame_ack.surface_id", Kind::Varint),
        field(2, "frame_ack.sequence", Kind::Varint),
    ],
};

static CURSOR_IMAGE: Schema = Schema {
    path: "cursor_image",
    fields: &[
        field(1, "cursor_image.serial", Kind::Varint),
        field(2, "cursor_image.width", Kind::Varint),
        field(3, "cursor_image.height", Kind::Varint),
        field(4, "cursor_image.hotspot_x", Kind::Varint),
        field(5, "cursor_image.hotspot_y", Kind::Varint),
        field(
            6,
            "cursor_image.argb_premultiplied",
            Kind::Bytes(MAX_CURSOR_BYTES),
        ),
    ],
};

static CURSOR_GONE: Schema = Schema {
    path: "cursor_gone",
    fields: &[],
};

static POINTER_MOVE: Schema = Schema {
    path: "pointer_move",
    fields: &[
        field(1, "pointer_move.surface_id", Kind::Varint),
        field(2, "pointer_move.x", Kind::Varint),
        field(3, "pointer_move.y", Kind::Varint),
    ],
};

static POINTER_BUTTON: Schema = Schema {
    path: "pointer_button",
    fields: &[
        field(1, "pointer_button.surface_id", Kind::Varint),
        field(2, "pointer_button.button", Kind::Varint),
        field(3, "pointer_button.pressed", Kind::Varint),
    ],
};

static POINTER_AXIS: Schema = Schema {
    path: "pointer_axis",
    fields: &[
        field(1, "pointer_axis.surface_id", Kind::Varint),
        field(2, "pointer_axis.steps_x", Kind::Varint),
        field(3, "pointer_axis.steps_y", Kind::Varint),
    ],
};

static KEY: Schema = Schema {
    path: "key",
    fields: &[
        field(1, "key.keysym", Kind::Varint),
        field(2, "key.code", Kind::Str(MAX_KEY_CODE_BYTES)),
        field(3, "key.pressed", Kind::Varint),
        field(4, "key.modifiers", Kind::Varint),
    ],
};

static FOCUS_NOTIFY: Schema = Schema {
    path: "focus_notify",
    fields: &[field(1, "focus_notify.surface_id", Kind::Varint)],
};

static BLUR_RELEASE: Schema = Schema {
    path: "blur_release",
    fields: &[],
};

static CLIPBOARD_SET: Schema = Schema {
    path: "clipboard_set",
    fields: &[field(
        1,
        "clipboard_set.text",
        Kind::Str(MAX_CLIPBOARD_BYTES),
    )],
};

static CLIPBOARD_ASK: Schema = Schema {
    path: "clipboard_ask",
    fields: &[],
};

static CLOSE_REQUEST: Schema = Schema {
    path: "close_request",
    fields: &[field(1, "close_request.surface_id", Kind::Varint)],
};

/// The Envelope oneof: field number `n` names `BODIES[n - 1]`.
static BODIES: [&Schema; 24] = [
    &HELLO,
    &HELLO_REPLY,
    &BYE,
    &SERVER_ERROR,
    &SURFACE_NEW,
    &SURFACE_GONE,
    &SURFACE_METADATA,
    &FOCUS_ASK,
    &RESIZE_ASK,
    &CONFIGURE,
    &CONFIGURE_ACK,
    &FRAME,
    &FRAME_ACK,
    &CURSOR_IMAGE,
    &CURSOR_GONE,
    &POINTER_MOVE,
    &POINTER_BUTTON,
    &POINTER_AXIS,
    &KEY,
    &FOCUS_NOTIFY,
    &BLUR_RELEASE,
    &CLIPBOARD_SET,
    &CLIPBOARD_ASK,
    &CLOSE_REQUEST,
];

/// The body schema an envelope field number names, if any.
fn body(number: u32) -> Option<&'static Schema> {
    let index = usize::try_from(number.checked_sub(1)?).ok()?;
    BODIES.get(index).copied()
}

// ---------------------------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------------------------

/// Scans one whole message: exactly one known envelope body, every cap, the grammar.
///
/// Allocates nothing. The caller has already refused input over
/// [`crate::limits::MAX_MESSAGE_BYTES`].
pub(crate) fn envelope(bytes: &[u8]) -> Result<(), Refusal> {
    let mut input = Input(bytes);
    let mut bodies = 0usize;
    while !input.is_empty() {
        let (number, wire_type) = input.key("envelope")?;
        let Some(schema) = body(number) else {
            return Err(Refusal::UnknownMessage);
        };
        if wire_type != LEN {
            return Err(Refusal::Grammar(schema.path));
        }
        let payload = input.len_delimited(schema.path)?;
        bodies += 1;
        if bodies > 1 {
            return Err(Refusal::Grammar("body"));
        }
        message(payload, schema)?;
    }
    if bodies == 0 {
        return Err(Refusal::UnknownMessage);
    }
    Ok(())
}

/// Scans the fields of one message against its schema.
fn message(bytes: &[u8], schema: &Schema) -> Result<(), Refusal> {
    let mut input = Input(bytes);
    // Singular fields already read, as a bit per field number (every v0 number is below 64),
    // and the entries counted so far per repeated field, by index in the schema.
    let mut seen = 0u64;
    let mut counts = [0usize; MAX_FIELDS];
    while !input.is_empty() {
        let (number, wire_type) = input.key(schema.path)?;
        let Some((index, known)) = schema.field(number) else {
            input.skip(wire_type, schema.path)?;
            continue;
        };
        let path = known.path;
        match known.kind {
            Kind::Varints(cap) => {
                let budget = cap.saturating_sub(counts[index]);
                let entries = match wire_type {
                    VARINT => {
                        input.varint32(path)?;
                        1
                    }
                    LEN => count_varints32(input.len_delimited(path)?, budget, path)?,
                    _ => return Err(Refusal::Grammar(path)),
                };
                counts[index] += entries;
                if counts[index] > cap {
                    return Err(Refusal::Limit(path));
                }
            }
            Kind::Messages(sub, cap) => {
                if wire_type != LEN {
                    return Err(Refusal::Grammar(path));
                }
                counts[index] += 1;
                if counts[index] > cap {
                    return Err(Refusal::Limit(path));
                }
                message(input.len_delimited(path)?, sub)?;
            }
            singular => {
                let bit = 1u64 << number;
                if seen & bit != 0 {
                    return Err(Refusal::Grammar(path));
                }
                seen |= bit;
                singular_field(&mut input, singular, wire_type, path)?;
            }
        }
    }
    Ok(())
}

/// Scans one occurrence of a field that is not repeated.
fn singular_field(
    input: &mut Input<'_>,
    kind: Kind,
    wire_type: u8,
    path: &'static str,
) -> Result<(), Refusal> {
    let want = if matches!(kind, Kind::Varint) {
        VARINT
    } else {
        LEN
    };
    if wire_type != want {
        return Err(Refusal::Grammar(path));
    }
    match kind {
        Kind::Varint => {
            input.varint32(path)?;
        }
        Kind::Str(cap) => {
            let text = input.capped(cap, path)?;
            if std::str::from_utf8(text).is_err() {
                return Err(Refusal::Grammar(path));
            }
        }
        Kind::Bytes(cap) => {
            input.capped(cap, path)?;
        }
        Kind::Message(sub) => message(input.len_delimited(path)?, sub)?,
        // The caller routes the repeated kinds elsewhere; reaching here is a schema bug, and
        // refusing is the safe answer to it.
        Kind::Messages(..) | Kind::Varints(_) => return Err(Refusal::Grammar(path)),
    }
    Ok(())
}

/// Counts the varints of a packed `repeated uint32` payload, each of at most 32 bits, and
/// stops with [`Refusal::Limit`] as soon as the count passes `budget`.
fn count_varints32(bytes: &[u8], budget: usize, path: &'static str) -> Result<usize, Refusal> {
    let mut input = Input(bytes);
    let mut count = 0usize;
    while !input.is_empty() {
        input.varint32(path)?;
        count += 1;
        if count > budget {
            return Err(Refusal::Limit(path));
        }
    }
    Ok(count)
}

/// The unread rest of one message.
struct Input<'a>(&'a [u8]);

impl<'a> Input<'a> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// One varint of up to ten bytes, as protobuf (and prost) define it: the tenth byte may
    /// only carry the top bit of a `u64`.
    fn varint(&mut self, path: &'static str) -> Result<u64, Refusal> {
        let mut value = 0u64;
        for (index, &byte) in self.0.iter().enumerate().take(10) {
            if index == 9 && byte > 1 {
                return Err(Refusal::Grammar(path));
            }
            value |= u64::from(byte & 0x7f) << (7 * index);
            if byte < 0x80 {
                self.0 = &self.0[index + 1..];
                return Ok(value);
            }
        }
        // The input ended inside the varint, or its tenth byte still said "more".
        Err(Refusal::Grammar(path))
    }

    /// A varint that must fit 32 bits: every scalar field in v0 is 32 bits wide.
    fn varint32(&mut self, path: &'static str) -> Result<u32, Refusal> {
        u32::try_from(self.varint(path)?).map_err(|_| Refusal::Grammar(path))
    }

    /// A field key: the field number (never 0) and a wire type protobuf defines (0 to 5).
    fn key(&mut self, path: &'static str) -> Result<(u32, u8), Refusal> {
        let key = self.varint32(path)?;
        let number = key >> 3;
        let wire_type = u8::try_from(key & 7).map_err(|_| Refusal::Grammar(path))?;
        if number == 0 || wire_type > FIXED32 {
            return Err(Refusal::Grammar(path));
        }
        Ok((number, wire_type))
    }

    /// Takes `len` bytes, or fails when the message has fewer left.
    fn take(&mut self, len: u64, path: &'static str) -> Result<&'a [u8], Refusal> {
        let len = usize::try_from(len).map_err(|_| Refusal::Grammar(path))?;
        let Some((taken, rest)) = self.0.split_at_checked(len) else {
            return Err(Refusal::Grammar(path));
        };
        self.0 = rest;
        Ok(taken)
    }

    /// A length-delimited payload whose length must fit the message it sits in.
    fn len_delimited(&mut self, path: &'static str) -> Result<&'a [u8], Refusal> {
        let len = self.varint(path)?;
        self.take(len, path)
    }

    /// A length-delimited payload with a cap. The length is compared with the cap first, so
    /// a length over the cap is a limit violation whether or not its bytes are there.
    fn capped(&mut self, cap: usize, path: &'static str) -> Result<&'a [u8], Refusal> {
        let len = self.varint(path)?;
        if !usize::try_from(len).is_ok_and(|len| len <= cap) {
            return Err(Refusal::Limit(path));
        }
        self.take(len, path)
    }

    /// Skips one unknown field by its wire type, inside the bounds of its message. A group
    /// (wire type 3 or 4) is refused, not skipped: v0 has none, and skipping one would need a
    /// walk into it.
    fn skip(&mut self, wire_type: u8, path: &'static str) -> Result<(), Refusal> {
        match wire_type {
            VARINT => self.varint(path).map(|_| ()),
            FIXED64 => self.take(8, path).map(|_| ()),
            LEN => self.len_delimited(path).map(|_| ()),
            FIXED32 => self.take(4, path).map(|_| ()),
            _ => Err(Refusal::Grammar(path)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BODIES, Kind, MAX_FIELDS, POINT, POSITIONER, RECT, Refusal, SIZE, Schema, TILE, envelope,
    };

    /// Every schema: the 24 bodies and the five messages only nested ones use.
    fn all_schemas() -> Vec<&'static Schema> {
        let mut all: Vec<&'static Schema> = BODIES.to_vec();
        all.extend([&POINT, &SIZE, &RECT, &POSITIONER, &TILE]);
        all
    }

    #[test]
    fn every_schema_fits_the_counters() {
        for schema in all_schemas() {
            assert!(schema.fields.len() <= MAX_FIELDS, "{}", schema.path);
            for f in schema.fields {
                assert!(f.number < 64, "{}", f.path);
            }
        }
    }

    /// `CamelCase` to `snake_case`, as the schema paths spell message names.
    fn snake(name: &str) -> String {
        let mut out = String::new();
        for (i, c) in name.chars().enumerate() {
            if c.is_ascii_uppercase() && i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        }
        out
    }

    /// One field line of wire.proto: label, type, name, number.
    struct ProtoField {
        repeated: bool,
        ty: String,
        name: String,
        number: u32,
    }

    /// The messages of wire.proto, each with its fields in file order, plus the enum names.
    fn parse_proto() -> (Vec<(String, Vec<ProtoField>)>, Vec<String>) {
        let source = include_str!("../proto/appricot/v0/wire.proto");
        let mut messages: Vec<(String, Vec<ProtoField>)> = Vec::new();
        let mut enums = Vec::new();
        let mut in_message = false;
        for raw in source.lines() {
            let line = raw.split("//").next().unwrap_or("").trim();
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                ["message", name, "{"] => {
                    messages.push(((*name).to_owned(), Vec::new()));
                    in_message = true;
                }
                ["message", name, "{}"] => messages.push(((*name).to_owned(), Vec::new())),
                ["enum", name, "{"] => {
                    enums.push((*name).to_owned());
                    in_message = false;
                }
                ["}"] => {}
                _ if in_message && line.ends_with(';') && line.contains('=') => {
                    let (repeated, rest) = match words.as_slice() {
                        ["repeated", rest @ ..] => (true, rest),
                        // `optional` changes presence, which the pre-scan does not track.
                        ["optional", rest @ ..] | rest => (false, rest),
                    };
                    let [ty, name, "=", number] = rest else {
                        panic!("unexpected field line in wire.proto: {line}");
                    };
                    let number = number
                        .trim_end_matches(';')
                        .parse()
                        .expect("a field number");
                    let fields = &mut messages.last_mut().expect("inside a message").1;
                    fields.push(ProtoField {
                        repeated,
                        ty: (*ty).to_owned(),
                        name: (*name).to_owned(),
                        number,
                    });
                }
                _ => {}
            }
        }
        (messages, enums)
    }

    #[test]
    fn schema_matches_wire_proto() {
        let (messages, enums) = parse_proto();
        let schemas = all_schemas();
        let find = |path: &str| {
            schemas
                .iter()
                .copied()
                .find(|s| s.path == path)
                .unwrap_or_else(|| panic!("no pre-scan schema for {path}"))
        };
        let mut checked = 0;
        for (name, fields) in &messages {
            if name == "Envelope" {
                assert_eq!(fields.len(), BODIES.len(), "the Envelope oneof");
                for f in fields {
                    let index = usize::try_from(f.number - 1).expect("small");
                    assert_eq!(BODIES[index].path, f.name, "Envelope field {}", f.number);
                }
                continue;
            }
            let schema = find(&snake(name));
            assert_eq!(schema.fields.len(), fields.len(), "the fields of {name}");
            for f in fields {
                let (_, known) = schema
                    .field(f.number)
                    .unwrap_or_else(|| panic!("{name}.{} is missing", f.name));
                assert_eq!(known.path, format!("{}.{}", schema.path, f.name));
                let scalar =
                    matches!(f.ty.as_str(), "uint32" | "sint32" | "bool") || enums.contains(&f.ty);
                let ok = match (known.kind, f.repeated) {
                    (Kind::Varint, false) => scalar,
                    (Kind::Str(_), false) => f.ty == "string",
                    (Kind::Bytes(_), false) => f.ty == "bytes",
                    (Kind::Varints(_), true) => f.ty == "uint32",
                    (Kind::Message(sub), false) | (Kind::Messages(sub, _), true) => {
                        sub.path == snake(&f.ty)
                    }
                    _ => false,
                };
                assert!(ok, "{name}.{} does not match its wire.proto type", f.name);
                checked += 1;
            }
        }
        assert!(checked > 60, "the parser found too few fields: {checked}");
    }

    #[test]
    fn refuses_an_empty_input_as_an_unknown_message() {
        assert_eq!(envelope(&[]), Err(Refusal::UnknownMessage));
    }

    #[test]
    fn refuses_a_tile_count_at_cap_plus_one_before_reading_the_tile() {
        // Frame (field 12) with 49 tiles; the 49th names a length far past the input, which is
        // never looked at because the count fails first.
        let mut frame = Vec::new();
        for _ in 0..48 {
            frame.extend_from_slice(&[0x22, 0x00]);
        }
        frame.extend_from_slice(&[0x22, 0x7f]);
        let mut bytes = vec![0x62, u8::try_from(frame.len()).expect("small")];
        bytes.extend_from_slice(&frame);
        assert_eq!(envelope(&bytes), Err(Refusal::Limit("frame.tiles")));
    }

    #[test]
    fn a_length_over_its_cap_is_a_limit_even_without_its_bytes() {
        // Hello.client_name claiming 65 bytes, with none of them present.
        assert_eq!(
            envelope(&[0x0a, 0x02, 0x12, 0x41]),
            Err(Refusal::Limit("hello.client_name"))
        );
        // At the cap, the missing bytes are a grammar error instead.
        assert_eq!(
            envelope(&[0x0a, 0x02, 0x12, 0x40]),
            Err(Refusal::Grammar("hello.client_name"))
        );
    }

    #[test]
    fn skips_unknown_fields_of_every_skippable_wire_type() {
        // Hello with unknown field 9 as varint, fixed64, length-delimited and fixed32.
        let hello = [
            0x48, 0x96, 0x01, // 9: varint 150
            0x49, 1, 2, 3, 4, 5, 6, 7, 8, // 9: fixed64
            0x4a, 0x02, 0xff, 0xfe, // 9: bytes, not UTF-8 and never looked into
            0x4d, 1, 2, 3, 4, // 9: fixed32
        ];
        let mut bytes = vec![0x0a, u8::try_from(hello.len()).expect("small")];
        bytes.extend_from_slice(&hello);
        assert_eq!(envelope(&bytes), Ok(()));
    }

    #[test]
    fn refuses_groups_field_zero_and_undefined_wire_types() {
        // Hello with an unknown field 9 as a group start.
        assert_eq!(
            envelope(&[0x0a, 0x02, 0x4b, 0x4c]),
            Err(Refusal::Grammar("hello"))
        );
        // Field number 0.
        assert_eq!(
            envelope(&[0x0a, 0x02, 0x00, 0x00]),
            Err(Refusal::Grammar("hello"))
        );
        // Wire type 7.
        assert_eq!(
            envelope(&[0x0a, 0x01, 0x4f]),
            Err(Refusal::Grammar("hello"))
        );
    }

    #[test]
    fn refuses_a_varint_over_32_bits_on_a_known_field() {
        // Hello.protocol_version = 2^32.
        assert_eq!(
            envelope(&[0x0a, 0x06, 0x08, 0x80, 0x80, 0x80, 0x80, 0x10]),
            Err(Refusal::Grammar("hello.protocol_version"))
        );
        // The same value in an unknown field is skipped.
        assert_eq!(
            envelope(&[0x0a, 0x06, 0x48, 0x80, 0x80, 0x80, 0x80, 0x10]),
            Ok(())
        );
    }

    #[test]
    fn refuses_a_singular_field_twice_but_counts_a_repeated_one() {
        // SurfaceNew.surface_id twice.
        assert_eq!(
            envelope(&[0x2a, 0x04, 0x08, 0x01, 0x08, 0x02]),
            Err(Refusal::Grammar("surface_new.surface_id"))
        );
        // Hello.codecs unpacked twice, then packed: three entries.
        assert_eq!(
            envelope(&[0x0a, 0x07, 0x20, 0x01, 0x20, 0x02, 0x22, 0x01, 0x01]),
            Ok(())
        );
    }

    #[test]
    fn refuses_a_second_body() {
        // CursorGone, twice.
        assert_eq!(
            envelope(&[0x7a, 0x00, 0x7a, 0x00]),
            Err(Refusal::Grammar("body"))
        );
    }
}
