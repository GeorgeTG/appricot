//! What `decode_envelope` allocates, measured with a counting global allocator.
//!
//! A message refused at a cap costs nothing: the pre-scan compares every length and every
//! repeated count with its row of the limits table on the raw bytes, before prost builds an
//! element or copies a string. A message that is accepted costs about its own size. Before the
//! pre-scan, the tile flood below made prost build about 8.4 million `Tile`s, some 400 MB,
//! before the tile count was ever compared with its cap of 48.

mod common;

use appricot_proto::limits::{
    MAX_CLIENT_NAME_BYTES, MAX_MESSAGE_BYTES, MAX_TILE_BYTES, MAX_TILE_HEIGHT, MAX_TILE_WIDTH,
    MAX_TILES_PER_FRAME, codec,
};
use appricot_proto::wire::{
    Body, DecodeError, Envelope, Frame, Hello, Rect, Tile, decode_envelope, encode_envelope,
};

#[global_allocator]
static ALLOCATOR: common::Counting = common::Counting;

/// Appends a protobuf varint.
fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(u8::try_from(value & 0x7f).expect("seven bits") | 0x80);
        value >>= 7;
    }
    out.push(u8::try_from(value).expect("under 0x80"));
}

/// One length-delimited field: key, length, payload.
fn len_field(number: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    varint(u64::from((number << 3) | 2), &mut out);
    varint(payload.len() as u64, &mut out);
    out.extend_from_slice(payload);
    out
}

/// The payload that fills an envelope field up to `MAX_MESSAGE_BYTES` when each entry is
/// `entry` repeated: as many whole entries as fit after a key and a 4-byte length.
fn flood(entry: &[u8]) -> Vec<u8> {
    let count = (MAX_MESSAGE_BYTES - 1 - 4) / entry.len();
    entry.repeat(count)
}

/// Decodes `bytes` and returns the result with the peak bytes the decode held.
fn measured(bytes: &[u8]) -> (Result<Envelope, DecodeError>, usize) {
    common::peak_during(|| decode_envelope(bytes))
}

/// A refusal that allocates nothing at all, with the error it must be.
fn refused_for_free(name: &str, bytes: &[u8], want: &DecodeError) {
    assert!(
        bytes.len() <= MAX_MESSAGE_BYTES,
        "{name}: the input must pass the size gate"
    );
    let (result, peak) = measured(bytes);
    let err = result.expect_err(name);
    assert_eq!(
        format!("{err:?}"),
        format!("{want:?}"),
        "{name}: wrong refusal"
    );
    assert_eq!(peak, 0, "{name}: a refusal allocated {peak} bytes");
}

#[test]
fn a_tile_flood_is_refused_at_the_49th_tile_without_allocating() {
    // Envelope.frame (field 12) holding millions of empty Tile entries (`22 00`).
    let bytes = len_field(12, &flood(&[0x22, 0x00]));
    refused_for_free(
        "tile flood",
        &bytes,
        &DecodeError::LimitViolation {
            field: "frame.tiles",
        },
    );
}

#[test]
fn a_codec_flood_is_refused_packed_or_not_without_allocating() {
    // Hello.codecs (field 4) packed: one length, then millions of one-byte varints.
    let packed = len_field(1, &len_field(4, &flood(&[0x01])[..MAX_MESSAGE_BYTES - 16]));
    refused_for_free(
        "packed codec flood",
        &packed,
        &DecodeError::LimitViolation {
            field: "hello.codecs",
        },
    );
    // The same, unpacked: millions of `20 01` keys and values.
    let unpacked = len_field(1, &flood(&[0x20, 0x01]));
    refused_for_free(
        "unpacked codec flood",
        &unpacked,
        &DecodeError::LimitViolation {
            field: "hello.codecs",
        },
    );
}

#[test]
fn an_oversized_string_is_refused_before_it_is_copied() {
    // Hello.client_name of almost 16 MiB, against a cap of 64 bytes.
    let name = vec![b'a'; MAX_MESSAGE_BYTES - 16];
    assert!(name.len() > MAX_CLIENT_NAME_BYTES);
    let bytes = len_field(1, &len_field(2, &name));
    refused_for_free(
        "oversized client_name",
        &bytes,
        &DecodeError::LimitViolation {
            field: "hello.client_name",
        },
    );
}

#[test]
fn a_flood_then_a_hello_is_still_refused_at_the_flood() {
    // The body that used to slip through: prost kept the last oneof member, so the Frame's
    // millions of tiles were built and then dropped for the Hello behind them, and the tile
    // count was never checked at all.
    let hello = encode_envelope(&Envelope {
        body: Some(Body::Hello(Hello {
            stream_token: b"stream-token-0".to_vec(),
            ..Hello::default()
        })),
    })
    .expect("a valid Hello");
    let mut bytes = len_field(12, &flood(&[0x22, 0x00])[..MAX_MESSAGE_BYTES / 2]);
    bytes.extend_from_slice(&hello);
    refused_for_free(
        "flood then hello",
        &bytes,
        &DecodeError::LimitViolation {
            field: "frame.tiles",
        },
    );

    // A small Frame followed by a Hello is two bodies, and that alone is refused.
    let mut two = len_field(12, &[0x22, 0x00]);
    two.extend_from_slice(&hello);
    refused_for_free(
        "frame then hello",
        &two,
        &DecodeError::ProtocolViolation { field: "body" },
    );
}

#[test]
fn a_length_past_the_input_is_refused_without_allocating() {
    // Envelope.frame claiming 16 MiB of payload, with none of it present.
    let mut bytes = Vec::new();
    varint((12 << 3) | 2, &mut bytes);
    varint((MAX_MESSAGE_BYTES - 8) as u64, &mut bytes);
    refused_for_free(
        "a lying length",
        &bytes,
        &DecodeError::ProtocolViolation { field: "frame" },
    );
}

#[test]
fn input_over_the_message_cap_is_refused_without_allocating() {
    let bytes = vec![0u8; MAX_MESSAGE_BYTES + 1];
    let (result, peak) = measured(&bytes);
    assert!(matches!(result, Err(DecodeError::TooLarge)));
    assert_eq!(peak, 0);
}

#[test]
fn an_unknown_field_of_any_size_is_skipped_without_copying_it() {
    // A Hello carrying an unknown field 9 of almost 16 MiB: it is skipped by length (v0.md §13),
    // never copied, so the decode costs what an empty Hello costs.
    let mut hello = len_field(9, &vec![0xab; MAX_MESSAGE_BYTES - 32]);
    hello.extend_from_slice(&len_field(3, b"tok"));
    let bytes = len_field(1, &hello);
    let (result, peak) = measured(&bytes);
    let envelope = result.expect("an unknown field is skipped");
    let Some(Body::Hello(decoded)) = envelope.body else {
        panic!("expected a Hello");
    };
    assert_eq!(decoded.stream_token, b"tok");
    assert!(
        peak < 1024,
        "skipping an unknown field allocated {peak} bytes"
    );
}

#[test]
fn an_accepted_maximal_frame_costs_about_its_own_size() {
    // 48 tiles of 256x256 RAW pixels: the biggest Frame the table allows.
    let side = MAX_TILE_WIDTH.min(MAX_TILE_HEIGHT);
    let tile = Tile {
        rect: Some(Rect {
            x: 0,
            y: 0,
            width: side,
            height: side,
        }),
        codec: codec::RAW,
        data: vec![0x5a; MAX_TILE_BYTES],
    };
    let bytes = encode_envelope(&Envelope {
        body: Some(Body::Frame(Frame {
            surface_id: 1,
            sequence: 1,
            full_redraw: true,
            tiles: vec![tile; MAX_TILES_PER_FRAME],
        })),
    })
    .expect("a Frame at every cap is valid");
    let (result, peak) = measured(&bytes);
    let envelope = result.expect("a Frame at every cap decodes");
    drop(envelope);
    // prost copies a `bytes` field twice on its way into a `Vec<u8>` (once into a `Bytes`,
    // then into the Vec), one field at a time, so the peak is the input's size plus one tile.
    let bound = bytes.len() + MAX_TILE_BYTES + 64 * 1024;
    assert!(
        peak <= bound,
        "decoding {} bytes held {peak} bytes at its peak, over {bound}",
        bytes.len()
    );
}
