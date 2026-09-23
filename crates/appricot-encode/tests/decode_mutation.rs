//! A deterministic mutation test of `decode_tile`: damaged payloads decode to exactly one
//! tile of opaque pixels, or fail with an error, and never panic.
//!
//! `decode_tile` has no production caller yet, but it is documented for resync paths and
//! server-side consumers, which would feed it bytes from outside. This guards later edits
//! to the QOI decoder (a fast path, decoding straight into BGRX) against an out-of-bounds
//! slice or an unbounded loop. The seeds are streams from the in-house encoder and
//! hand-built streams that exercise every chunk kind. The mutations are bit flips, byte
//! overwrites, truncations, insertions, deletions and splices of two seeds, sometimes
//! decoded against a size other than the seed's.

use appricot_core::{PixelBuffer, PixelFormat, Size};
use appricot_encode::{DecodeError, Encoding, TILE_SIZE, decode_tile, encode_tile};

/// Iterations of the mutation loop.
const ITERATIONS: u32 = 10_000;

/// A tiny deterministic PRNG (xorshift32), so every run mutates the same way.
struct XorShift32(u32);

impl XorShift32 {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// A value in `0..bound`; `bound` is never zero here.
    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next()).expect("u32 fits in usize") % bound
    }

    fn byte(&mut self) -> u8 {
        u8::try_from(self.next() & 0xFF).expect("masked to a byte")
    }
}

/// A seed: a codec id, a payload and the size it decodes to.
struct Seed {
    codec: u32,
    size: Size,
    payload: Vec<u8>,
}

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

fn byte(value: u32) -> u8 {
    u8::try_from(value & 0xFF).expect("masked to a byte")
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

fn seeds() -> Vec<Seed> {
    let mut seeds = Vec::new();
    let buffers = [
        tile(16, 16, |x, y| [byte(x * 3), byte(y * 5), byte(x ^ y), 0xFF]),
        tile(64, 8, |x, _| [0x10, 0x20, byte(x / 8), 0x7F]),
        tile(33, 7, |x, y| {
            if (x + y) % 5 == 0 {
                [0xF0, 0xF0, 0xF0, 0xFF]
            } else {
                [0x28, byte(x), byte(y * 9), 0x00]
            }
        }),
        tile(200, 3, |_, y| [byte(y), byte(y), byte(y), 0xFF]),
    ];
    for buf in &buffers {
        for prefer in [Encoding::Qoi, Encoding::Raw] {
            let encoded = encode_tile(buf, prefer).expect("a seed tile encodes");
            seeds.push(Seed {
                codec: encoded.codec,
                size: buf.size,
                payload: encoded.data,
            });
        }
    }
    let qoi = Encoding::Qoi.id();
    let hand_built: [(u32, u32, u8, &[u8]); 4] = [
        (4, 1, 4, &[0xFF, 10, 10, 10, 0, 0x7F, 0xFE, 200, 0, 0, 37]),
        (3, 1, 3, &[0xC1, 53]),
        (2, 1, 3, &[0x05, 0xFE, 1, 2, 3]),
        (6, 1, 4, &[0xFE, 1, 2, 3, 0x8C, 0xDE, 0x40, 0xC1, 0x09]),
    ];
    for (width, height, channels, chunks) in hand_built {
        seeds.push(Seed {
            codec: qoi,
            size: Size::new(width, height),
            payload: qoi_stream(width, height, channels, chunks),
        });
    }
    seeds
}

/// Applies one to three random mutations to `payload`, sometimes splicing in `other`.
fn mutate(rng: &mut XorShift32, payload: &mut Vec<u8>, other: &[u8]) {
    for _ in 0..=rng.below(3) {
        let len = payload.len();
        match rng.below(7) {
            0 if len > 0 => {
                let at = rng.below(len);
                payload[at] ^= 1 << rng.below(8);
            }
            1 if len > 0 => {
                let at = rng.below(len);
                payload[at] = rng.byte();
            }
            2 => payload.truncate(rng.below(len + 1)),
            3 => {
                let at = rng.below(len + 1);
                let value = rng.byte();
                payload.insert(at, value);
            }
            4 if len > 0 => {
                let at = rng.below(len);
                payload.remove(at);
            }
            5 if !other.is_empty() => {
                // Splice: a prefix of this payload, then a suffix of the other one.
                let cut = rng.below(len + 1);
                let from = rng.below(other.len());
                payload.truncate(cut);
                payload.extend_from_slice(&other[from..]);
            }
            _ => {
                // A chunk-looking byte: a tag with a random payload, where chunks live.
                let at = rng.below(len + 1);
                let tag = [0x00, 0x40, 0x80, 0xC0, 0xFE, 0xFF][rng.below(6)];
                let low = if tag >= 0xFE { 0 } else { rng.byte() & 0x3F };
                payload.insert(at, tag | low);
            }
        }
    }
}

#[test]
fn mutated_payloads_decode_to_one_opaque_tile_or_fail_and_never_panic() {
    let seeds = seeds();
    let mut rng = XorShift32(0x5EED_7117);
    let (mut decoded, mut refused) = (0_u32, 0_u32);
    for iteration in 0..ITERATIONS {
        let seed = &seeds[rng.below(seeds.len())];
        let other = &seeds[rng.below(seeds.len())].payload;
        let mut payload = seed.payload.clone();
        mutate(&mut rng, &mut payload, other);
        // Mostly the seed's own size; sometimes another one, the edge sizes included.
        // Sides of 0..=259 reach past the tile cap too.
        let size = match rng.below(8) {
            0 => Size::new(
                u32::try_from(rng.below(260)).expect("small"),
                u32::try_from(rng.below(260)).expect("small"),
            ),
            1 => Size::new(TILE_SIZE, TILE_SIZE),
            _ => seed.size,
        };
        // A codec id that is sometimes not the seed's own.
        let codec = if rng.below(16) == 0 {
            u32::try_from(rng.below(4)).expect("small")
        } else {
            seed.codec
        };

        match decode_tile(codec, &payload, size) {
            Ok(buf) => {
                decoded += 1;
                let expected = 4 * size.width as usize * size.height as usize;
                assert_eq!(buf.size, size, "iteration {iteration}");
                assert_eq!(buf.stride, 4 * size.width as usize, "iteration {iteration}");
                assert_eq!(buf.format, PixelFormat::Bgrx8888, "iteration {iteration}");
                assert_eq!(buf.data.len(), expected, "iteration {iteration}");
                assert!(
                    buf.data.chunks_exact(4).all(|pixel| pixel[3] == 0xFF),
                    "iteration {iteration}: every decoded pixel is opaque"
                );
            }
            Err(error) => {
                refused += 1;
                // Every refusal names itself; formatting it must not panic either.
                assert!(!error.to_string().is_empty());
                if let DecodeError::SizeTooLarge { width, height } = error {
                    assert!(width > TILE_SIZE || height > TILE_SIZE);
                }
            }
        }
    }
    // The loop must exercise both outcomes, or it proves little.
    assert!(
        decoded > ITERATIONS / 20,
        "only {decoded} mutated payloads decoded"
    );
    assert!(
        refused > ITERATIONS / 4,
        "only {refused} mutated payloads were refused"
    );
}
