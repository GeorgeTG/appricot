//! The shared tile vectors: `testdata/qoi-vectors.json` must equal exactly what this module
//! generates, and the Rust decoder must agree with every vector in it.
//!
//! The TypeScript client consumes the committed file in
//! `packages/client/src/tile.vector.test.ts`, which decodes every payload with `decodeTile`
//! and compares the pixels. So a Rust-encoded QOI tile is decoded by the TypeScript client
//! in a test, and both decoders are held to the same answers on hand-built streams and on
//! streams they must refuse.
//!
//! The format is frozen: top-level `format` (always 1 for now), then `vectors`, one per
//! line. Each vector is `{ "name", "note", "codec", "width", "height", "payload", "expect" }`.
//! `payload` is the tile's data in lowercase hex. `expect` is `"pixels"` or `"reject"`. A
//! `"pixels"` vector also carries `rgba_fnv1a32`, the 32-bit FNV-1a hash of the decoded
//! RGBA bytes (`width * height * 4`, alpha always 255) in eight hex digits, and, when those
//! bytes are 1024 or fewer, `rgba` itself in hex.
//!
//! The expected pixels are never read off the decoder under test. An encoded vector expects
//! its source buffer, made opaque. A hand-built vector expects pixels written out below.
//!
//! Regenerate after changing a generator here:
//!
//! ```sh
//! docker compose run -e APPRICOT_REGEN_VECTORS=1 --rm dev \
//!   cargo test -p appricot-encode tile_vectors
//! ```
//!
//! and commit the file together with the change.

use crate::{Encoding, decode_tile, encode_tile, qoi};
use appricot_core::{PixelBuffer, PixelFormat, Size};

/// What a decoder must do with a vector's payload.
enum Expect {
    /// Decode it to exactly these RGBA bytes.
    Pixels(Vec<u8>),
    /// Refuse it.
    Reject,
}

/// One entry of `testdata/qoi-vectors.json`.
struct Vector {
    name: &'static str,
    note: &'static str,
    codec: u32,
    size: Size,
    payload: Vec<u8>,
    expect: Expect,
}

/// Builds a `Bgrx8888` tile from a per-pixel function giving `[blue, green, red, unused]`.
fn tile(width: u32, height: u32, mut pixel: impl FnMut(u32, u32) -> [u8; 4]) -> PixelBuffer {
    let mut data = Vec::with_capacity(4 * width as usize * height as usize);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&pixel(x, y));
        }
    }
    PixelBuffer {
        size: Size::new(width, height),
        stride: 4 * width as usize,
        format: PixelFormat::Bgrx8888,
        data,
    }
}

/// A one-row tile of the given `[red, green, blue]` pixels, each with the unused byte 0xFF.
fn row(pixels: &[[u8; 3]]) -> PixelBuffer {
    let width = u32::try_from(pixels.len()).expect("a row fits a tile");
    tile(width, 1, |x, _| {
        let [r, g, b] = pixels[x as usize];
        [b, g, r, 0xFF]
    })
}

/// The RGBA bytes every decoder must give back for `buf`: the colour, and alpha 255.
fn opaque_rgba(buf: &PixelBuffer) -> Vec<u8> {
    buf.data
        .chunks_exact(4)
        .flat_map(|bgrx| [bgrx[2], bgrx[1], bgrx[0], 0xFF])
        .collect()
}

/// A byte from a tile coordinate; tiles are at most 256 pixels on a side.
fn byte(value: u32) -> u8 {
    u8::try_from(value & 0xFF).expect("masked to a byte")
}

/// A QOI vector from the in-house encoder, with no RAW fallback, so even a tile too small
/// to compress carries a QOI stream.
fn qoi_encoded(name: &'static str, note: &'static str, buf: &PixelBuffer) -> Vector {
    Vector {
        name,
        note,
        codec: Encoding::Qoi.id(),
        size: buf.size,
        payload: qoi::encode(buf.size.width, buf.size.height, &buf.data),
        expect: Expect::Pixels(opaque_rgba(buf)),
    }
}

/// A vector from `encode_tile`, which must pick `codec` for `buf`.
fn tile_encoded(
    name: &'static str,
    note: &'static str,
    buf: &PixelBuffer,
    prefer: Encoding,
    codec: Encoding,
) -> Vector {
    let encoded = encode_tile(buf, prefer).expect("a generator tile encodes");
    assert_eq!(
        encoded.codec,
        codec.id(),
        "{name}: encode_tile picked another codec"
    );
    Vector {
        name,
        note,
        codec: encoded.codec,
        size: buf.size,
        payload: encoded.data,
        expect: Expect::Pixels(opaque_rgba(buf)),
    }
}

/// A hand-built QOI stream: header, chunks, end marker.
fn qoi_stream(width: u32, height: u32, channels: u8, chunks: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"qoif");
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&[channels, 0]);
    out.extend_from_slice(chunks);
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    out
}

/// A UI-like tile: a gradient title bar, a flat body, a one-pixel border, and strokes of
/// dark "text" with an anti-aliased grey beside each one.
fn ui(width: u32, height: u32) -> PixelBuffer {
    tile(width, height, |x, y| {
        if y < 20 {
            let shade = byte(60 + 3 * y);
            return [byte(u32::from(shade) + 40), shade, byte(40), 0xFF];
        }
        if x == 0 || x == width - 1 || y == height - 1 {
            return [0x80, 0x80, 0x80, 0xFF];
        }
        let ink = (30..110).contains(&y) && (8..width - 8).contains(&x);
        match (x * 7 + y * 3) % 23 {
            0 | 1 if ink => [0x28, 0x28, 0x28, 0xFF],
            2 if ink => [0x8C, 0x8A, 0x8B, 0xFF],
            _ => [0xF0, 0xF0, 0xF0, 0xFF],
        }
    })
}

/// A random walk whose steps stay inside the DIFF and LUMA ranges.
fn walk(width: u32, height: u32) -> PixelBuffer {
    let mut state = 0x9E37_79B9_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let (mut r, mut g, mut b) = (0x80_u8, 0x60_u8, 0x40_u8);
    tile(width, height, |_, _| {
        let step = next();
        let green = byte(step % 9).wrapping_sub(4);
        g = g.wrapping_add(green);
        r = r
            .wrapping_add(green)
            .wrapping_add(byte((step >> 8) % 5).wrapping_sub(2));
        b = b
            .wrapping_add(green)
            .wrapping_add(byte((step >> 16) % 5).wrapping_sub(2));
        [b, g, r, byte(step >> 24)]
    })
}

/// Tiles written by the in-house encoder at the edge sizes: 1x1, a full column, a full row,
/// the largest tile and the remainder tile of the development surface.
fn encoded_size_vectors() -> Vec<Vector> {
    vec![
        qoi_encoded(
            "qoi-1x1",
            "one pixel, one RGB chunk; its unused byte 0xC3 is ignored",
            &tile(1, 1, |_, _| [0x33, 0x22, 0x11, 0xC3]),
        ),
        qoi_encoded(
            "qoi-1x256-gradient",
            "a column whose green rises by one per row: a DIFF chain",
            &tile(1, 256, |_, y| [0x80, byte(y), 0x40, 0xFF]),
        ),
        qoi_encoded(
            "qoi-256x1-gradient",
            "a row whose red rises by one per pixel from 0 to 255: a DIFF chain",
            &tile(256, 1, |x, _| [0x80, 0x40, byte(x), 0xFF]),
        ),
        qoi_encoded(
            "qoi-256x256-rows",
            "the largest tile, in flat grey rows: row 0 is runs of the starting black, every \
             other row a DIFF step and runs of 62, 62, 62, 62 and 7",
            &tile(256, 256, |_, y| [byte(y), byte(y), byte(y), 0xFF]),
        ),
        qoi_encoded(
            "qoi-120x132-ui",
            "the remainder tile of a 1400x900 surface, with UI-like content",
            &ui(120, 132),
        ),
        qoi_encoded(
            "qoi-32x32-walk",
            "a random walk inside the DIFF and LUMA ranges, with garbage unused bytes",
            &walk(32, 32),
        ),
    ]
}

/// Tiles written by the in-house encoder to hit every chunk kind: the exact edges of the
/// DIFF and LUMA ranges and one past each, index hits and a hash collision, and the runs
/// around the 62-pixel maximum.
fn encoded_chunk_vectors() -> Vec<Vector> {
    let x = [0x40, 0x90, 0x20];
    let y = [0x41, 0x91, 0x21];
    let black = [0, 0, 0];
    vec![
        qoi_encoded(
            "qoi-diff-edges",
            "DIFF at -2 and +1 in each channel, wrapping 255 to 0, and 0 to 255 in two channels",
            &row(&[
                [100, 100, 100],
                [98, 100, 100],
                [99, 100, 100],
                [99, 98, 100],
                [99, 99, 100],
                [99, 99, 98],
                [99, 99, 99],
                [255, 255, 255],
                [0, 0, 0],
                [255, 255, 0],
            ]),
        ),
        qoi_encoded(
            "qoi-luma-edges",
            "LUMA at green -32 and +31, red-minus-green -8 and +7, blue-minus-green +7 and -8",
            &row(&[
                [100, 100, 100],
                [68, 68, 68],
                [99, 99, 99],
                [101, 109, 116],
                [118, 119, 118],
                [119, 119, 119],
            ]),
        ),
        qoi_encoded(
            "qoi-past-edges",
            "one past each edge: DIFF -3 and +2 go LUMA; green -33 and +32, and \
             red-minus-green -9 and +8, go RGB",
            &row(&[
                [100, 100, 100],
                [97, 100, 100],
                [99, 100, 100],
                [66, 67, 67],
                [98, 99, 99],
                [99, 109, 109],
                [117, 119, 119],
            ]),
        ),
        qoi_encoded(
            "qoi-index-hits",
            "colours that come back while they are still in the index: slots 9, 31 and 38",
            &row(&[
                [10, 20, 30],
                [200, 100, 50],
                [10, 20, 30],
                [90, 180, 41],
                [200, 100, 50],
                [90, 180, 41],
                [10, 20, 30],
            ]),
        ),
        qoi_encoded(
            "qoi-index-collision",
            "two colours that share slot 31: the second evicts the first, which comes back \
             as a full RGB chunk",
            &row(&[[200, 100, 50], [90, 180, 40], [200, 100, 50]]),
        ),
        qoi_encoded(
            "qoi-run-62",
            "a pixel repeated 62 times after itself: exactly one RUN of 62",
            &row(&[&[x; 63][..], &[y][..]].concat()),
        ),
        qoi_encoded(
            "qoi-run-63",
            "a pixel repeated 63 times after itself: a RUN of 62, then a RUN of 1",
            &row(&[&[x; 64][..], &[y][..]].concat()),
        ),
        qoi_encoded(
            "qoi-leading-run",
            "70 pixels of the starting black: runs of the initial previous pixel",
            &row(&[&[black; 70][..], &[x, black][..]].concat()),
        ),
    ]
}

/// The same capture buffer as RAW and as QOI, with unused bytes that are not 0xFF, and the
/// tiles `encode_tile` itself chooses.
fn codec_choice_vectors() -> Vec<Vector> {
    let garbage = tile(16, 4, |x, y| {
        [
            0x40,
            0x80,
            byte(x / 4),
            [0x00, 0x7F, 0xC3, 0xFF][((x + y) % 4) as usize],
        ]
    });
    let incompressible = tile(8, 8, |x, y| {
        [
            byte(x),
            if (y * 8 + x).is_multiple_of(2) {
                0
            } else {
                128
            },
            byte(y + 77),
            0xFF,
        ]
    });
    vec![
        tile_encoded(
            "raw-garbage-fourth-byte",
            "RAW of a buffer whose unused bytes are 0x00, 0x7F, 0xC3 and 0xFF: opaque pixels",
            &garbage,
            Encoding::Raw,
            Encoding::Raw,
        ),
        tile_encoded(
            "qoi-garbage-fourth-byte",
            "QOI of the same buffer: the same opaque pixels as the RAW vector",
            &garbage,
            Encoding::Qoi,
            Encoding::Qoi,
        ),
        tile_encoded(
            "raw-fallback-incompressible",
            "QOI preferred, but no chunk shortens this tile, so it comes back RAW",
            &incompressible,
            Encoding::Qoi,
            Encoding::Raw,
        ),
    ]
}

/// Streams no in-house encoder writes, which both decoders must decode alike.
fn hand_built_vectors() -> Vec<Vector> {
    let hand = |name, note, width, height, channels, chunks: &[u8], rgba: &[u8]| Vector {
        name,
        note,
        codec: Encoding::Qoi.id(),
        size: Size::new(width, height),
        payload: qoi_stream(width, height, channels, chunks),
        expect: Expect::Pixels(rgba.to_vec()),
    };
    vec![
        hand(
            "qoi3-index-unfilled-slot",
            "3 channels, INDEX into a slot no pixel filled: (0,0,0,0), drawn opaque",
            2,
            1,
            3,
            &[0x05, 0xFE, 1, 2, 3],
            &[0, 0, 0, 255, 1, 2, 3, 255],
        ),
        hand(
            "qoi3-rgba-chunk",
            "3 channels with an RGBA chunk: decoded as the reference decoder does, drawn \
             opaque; its alpha 7 still feeds the index, so INDEX 47 finds (1,2,3,7)",
            3,
            1,
            3,
            &[0xFF, 1, 2, 3, 7, 0xFE, 9, 9, 9, 47],
            &[1, 2, 3, 255, 9, 9, 9, 255, 1, 2, 3, 255],
        ),
        hand(
            "qoi4-alpha-dropped",
            "4 channels with alpha 0 and 128: every tile is opaque",
            2,
            1,
            4,
            &[0xFF, 1, 2, 3, 0, 0xFF, 4, 5, 6, 128],
            &[1, 2, 3, 255, 4, 5, 6, 255],
        ),
        hand(
            "qoi4-alpha-feeds-the-index",
            "alpha 0 carried by DIFF and RGB still hashes: INDEX 37 is (11,11,11,0)",
            4,
            1,
            4,
            &[0xFF, 10, 10, 10, 0, 0x7F, 0xFE, 200, 0, 0, 37],
            &[
                10, 10, 10, 255, 11, 11, 11, 255, 200, 0, 0, 255, 11, 11, 11, 255,
            ],
        ),
        hand(
            "qoi-run-stores-the-start-pixel",
            "a leading RUN stores the starting (0,0,0,255) at slot 53, as qoi.h does; INDEX 53 \
             reads it, a DIFF makes (1,1,1,255) at slot 4, and INDEX 4 finds it. Without the \
             store, INDEX 53 reads (0,0,0,0), the DIFF lands at slot 15, and INDEX 4 is black",
            6,
            1,
            3,
            &[0xC1, 53, 0x7F, 0xFE, 9, 9, 9, 4],
            &[
                0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 1, 1, 1, 255, 9, 9, 9, 255, 1, 1, 1, 255,
            ],
        ),
    ]
}

/// Payloads both decoders must refuse.
fn reject_vectors() -> Vec<Vector> {
    let good = qoi_stream(2, 1, 3, &[0xFE, 1, 2, 3, 0xC0]);
    let mut bad_magic = good.clone();
    bad_magic[0] = b'x';
    let mut bad_channels = good.clone();
    bad_channels[12] = 5;
    let mut bad_colorspace = good.clone();
    bad_colorspace[13] = 2;
    let mut trailing = good.clone();
    trailing.push(0);
    let mut bad_marker = good.clone();
    *bad_marker.last_mut().expect("a stream is never empty") = 2;
    let size = Size::new(2, 1);
    let reject = |name, note, codec, size, payload| Vector {
        name,
        note,
        codec,
        size,
        payload,
        expect: Expect::Reject,
    };
    let qoi = Encoding::Qoi.id();
    vec![
        reject("reject-bad-magic", "no qoif magic", qoi, size, bad_magic),
        reject(
            "reject-channels-5",
            "channels byte 5",
            qoi,
            size,
            bad_channels,
        ),
        reject(
            "reject-colorspace-2",
            "colorspace 2",
            qoi,
            size,
            bad_colorspace,
        ),
        reject(
            "reject-size-mismatch",
            "the header names 2x1, the tile is 1x2",
            qoi,
            Size::new(1, 2),
            good.clone(),
        ),
        reject(
            "reject-truncated-chunk",
            "an RGB chunk cut short, and no end marker",
            qoi,
            size,
            good[..16].to_vec(),
        ),
        reject(
            "reject-no-end-marker",
            "every pixel decoded, the end marker missing",
            qoi,
            size,
            good[..good.len() - 8].to_vec(),
        ),
        reject(
            "reject-trailing-byte",
            "a byte after the end marker",
            qoi,
            size,
            trailing,
        ),
        reject(
            "reject-bad-end-marker",
            "the last marker byte is 2",
            qoi,
            size,
            bad_marker,
        ),
        reject(
            "reject-run-overflow",
            "a RUN of 3 after one pixel of a 2-pixel tile",
            qoi,
            size,
            qoi_stream(2, 1, 3, &[0xFE, 1, 2, 3, 0xC2]),
        ),
        reject(
            "reject-raw-short",
            "a RAW payload one byte short",
            Encoding::Raw.id(),
            size,
            vec![0; 7],
        ),
        reject("reject-codec-0", "0 is never a codec", 0, size, vec![0; 8]),
        reject("reject-codec-3", "3 is not a v0 codec", 3, size, vec![0; 8]),
    ]
}

/// Every vector, in file order.
fn vectors() -> Vec<Vector> {
    let mut all = encoded_size_vectors();
    all.extend(encoded_chunk_vectors());
    all.extend(codec_choice_vectors());
    all.extend(hand_built_vectors());
    all.extend(reject_vectors());
    all
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

/// The 32-bit FNV-1a hash (offset basis 0x811c9dc5, prime 0x01000193).
fn fnv1a32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5_u32, |hash, &byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

/// The largest expected RGBA written out in full; larger ones carry the hash only.
const MAX_INLINE_RGBA: usize = 1024;

fn render(vectors: &[Vector]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    out.push_str("{\n  \"format\": 1,\n  \"vectors\": [\n");
    for (index, vector) in vectors.iter().enumerate() {
        write!(
            out,
            "    {{ \"name\": \"{}\", \"note\": \"{}\", \"codec\": {}, \"width\": {}, \
             \"height\": {}, \"payload\": \"{}\"",
            vector.name,
            vector.note,
            vector.codec,
            vector.size.width,
            vector.size.height,
            hex(&vector.payload),
        )
        .expect("writing to a String cannot fail");
        match &vector.expect {
            Expect::Pixels(rgba) => {
                write!(
                    out,
                    ", \"expect\": \"pixels\", \"rgba_fnv1a32\": \"{:08x}\"",
                    fnv1a32(rgba)
                )
                .expect("writing to a String cannot fail");
                if rgba.len() <= MAX_INLINE_RGBA {
                    write!(out, ", \"rgba\": \"{}\"", hex(rgba))
                        .expect("writing to a String cannot fail");
                }
            }
            Expect::Reject => out.push_str(", \"expect\": \"reject\""),
        }
        out.push_str(if index + 1 == vectors.len() {
            " }\n"
        } else {
            " },\n"
        });
    }
    out.push_str("  ]\n}\n");
    out
}

#[test]
fn names_and_notes_need_no_json_escaping() {
    for vector in vectors() {
        for text in [vector.name, vector.note] {
            assert!(
                text.chars()
                    .all(|c| c.is_ascii() && !c.is_ascii_control() && c != '"' && c != '\\'),
                "{text:?} would need escaping"
            );
        }
    }
}

#[test]
fn the_rust_decoder_agrees_with_every_vector() {
    for vector in vectors() {
        let result = decode_tile(vector.codec, &vector.payload, vector.size);
        match (&vector.expect, result) {
            (Expect::Pixels(rgba), Ok(buf)) => {
                assert_eq!(buf.size, vector.size, "{}", vector.name);
                let decoded = opaque_rgba(&buf);
                assert!(
                    buf.data.chunks_exact(4).all(|bgrx| bgrx[3] == 0xFF),
                    "{}: every decoded pixel is opaque",
                    vector.name
                );
                assert_eq!(&decoded, rgba, "{}", vector.name);
            }
            (Expect::Reject, Err(_)) => {}
            (Expect::Pixels(_), Err(e)) => panic!("{} must decode: {e:?}", vector.name),
            (Expect::Reject, Ok(_)) => panic!("{} must be refused", vector.name),
        }
    }
}

#[test]
fn the_garbage_fourth_byte_pair_decodes_alike() {
    let all = vectors();
    let find = |name: &str| {
        all.iter()
            .find(|vector| vector.name == name)
            .unwrap_or_else(|| panic!("{name} is generated"))
    };
    let (raw, qoi) = (
        find("raw-garbage-fourth-byte"),
        find("qoi-garbage-fourth-byte"),
    );
    assert_eq!(raw.codec, Encoding::Raw.id());
    assert_eq!(qoi.codec, Encoding::Qoi.id());
    let pixels = |vector: &Vector| match &vector.expect {
        Expect::Pixels(rgba) => rgba.clone(),
        Expect::Reject => panic!("{} decodes", vector.name),
    };
    assert_eq!(pixels(raw), pixels(qoi));
}

#[test]
fn committed_tile_vectors_are_current() {
    let generated = render(&vectors());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("qoi-vectors.json");
    if std::env::var_os("APPRICOT_REGEN_VECTORS").is_some() {
        std::fs::create_dir_all(path.parent().expect("the path has a parent"))
            .expect("create testdata/");
        std::fs::write(&path, generated).expect("rewrite testdata/qoi-vectors.json");
        return;
    }
    let committed = std::fs::read_to_string(&path).expect(
        "testdata/qoi-vectors.json is missing; run the test once with APPRICOT_REGEN_VECTORS=1",
    );
    assert!(
        committed == generated,
        "testdata/qoi-vectors.json does not match the generators; regenerate with: docker \
         compose run -e APPRICOT_REGEN_VECTORS=1 --rm dev cargo test -p appricot-encode \
         tile_vectors"
    );
}
