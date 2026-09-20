//! The QOI image codec, implemented in-house from the published specification.
//!
//! Reference: the QOI specification at <https://qoiformat.org/> (fetched 2026-09-20) by
//! Dominic Szablewski, released as public domain (CC0). Everything in this module is
//! written from the published specification; no third-party code is used or copied.
//! QOI is a lossless codec whose chunks reference the previous pixel and a small index of
//! recently seen pixels, so flat areas and small deltas cost a fraction of the raw bytes.
//!
//! Scope here:
//!
//! - [`encode`] always writes a 4-channel stream (colorspace 0, sRGB with linear alpha).
//! - [`decode`] accepts 3- and 4-channel streams and is strict, because its input arrives
//!   on the wire: bad magic, an unknown channels or colorspace byte, header dimensions
//!   that disagree with the expected size, a truncated chunk, a missing end marker, bytes
//!   after the end marker and chunks that overrun the declared pixel count are all
//!   errors. A 3-channel stream decodes with alpha 255 (the wire protocol says a
//!   3-channel tile is opaque); an RGBA chunk inside one is rejected, as the spec forbids
//!   encoders from issuing it there.
//!
//! Format summary (the spec is the authority). A 14-byte header: magic `qoif`, width and
//! height as unsigned 32-bit big-endian, one channels byte (3 RGB or 4 RGBA), one
//! colorspace byte (0 sRGB with linear alpha, 1 all channels linear). Then chunks, then
//! the 8-byte end marker `00 00 00 00 00 00 00 01`. The chunk tag is the top two bits of
//! the first byte; `0xFE` and `0xFF` name the full-pixel chunks:
//!
//! | Chunk | First byte | Meaning |
//! |---|---|---|
//! | RGB | `FE` | 3 literal bytes follow; alpha carries over from the previous pixel |
//! | RGBA | `FF` | 4 literal bytes follow |
//! | INDEX | `00 nnnnnn` | the pixel stored at index slot `nnnnnn` |
//! | DIFF | `01` | 2-bit deltas for red, green, blue, each biased by +2 (range -2..=1); alpha carries over |
//! | LUMA | `10` | 6-bit green delta biased by +32 (-32..=31); one more byte: red-minus-green + 8 in the high nibble (-8..=7), blue-minus-green + 8 in the low nibble; alpha carries over |
//! | RUN | `11 nnnnnn` | the previous pixel repeated `nnnnnn + 1` times, so runs of 1..=62 |
//!
//! The index holds 64 pixels, starts all zero `(0,0,0,0)`, and every newly produced pixel
//! is stored at slot `(r*3 + g*5 + b*7 + a*11) % 64`. The previous pixel starts
//! `(0,0,0,255)`. A RUN chunk stores no index entry: it repeats a value that is already
//! in it.

use crate::DecodeError;
use appricot_core::Size;

/// The header magic, `qoif`.
const MAGIC: [u8; 4] = *b"qoif";
/// Header size: magic, width, height, channels, colorspace.
const HEADER_LEN: usize = 14;
/// The end-of-stream marker: seven zero bytes and a one.
const END_MARKER: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];
/// Slots in the recently-seen-pixel index.
const INDEX_SLOTS: usize = 64;
/// The longest RUN chunk: the 6-bit field holds run-1, and 63 would collide with the
/// full-pixel tag bytes `0xFE` and `0xFF`.
const MAX_RUN: u32 = 62;

/// One pixel, in the codec's own channel order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pixel {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Pixel {
    /// The value every index slot holds before any pixel is seen.
    const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    /// The previous pixel every QOI stream starts from.
    const BLACK_OPAQUE: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };

    /// Reads a pixel from BGRX bytes (blue, green, red, unused/alpha).
    fn from_bgrx(bytes: [u8; 4]) -> Self {
        Self {
            b: bytes[0],
            g: bytes[1],
            r: bytes[2],
            a: bytes[3],
        }
    }

    /// The index slot this pixel hashes to, per the spec's table.
    fn index_slot(self) -> usize {
        (usize::from(self.r) * 3
            + usize::from(self.g) * 5
            + usize::from(self.b) * 7
            + usize::from(self.a) * 11)
            % INDEX_SLOTS
    }
}

/// True when a delta byte, read as two's complement, lies in -2..=1 (the DIFF range).
fn is_diff_delta(delta: u8) -> bool {
    matches!(delta, 0 | 1 | 254 | 255)
}

/// True when a delta byte lies in -32..=31 (the LUMA green range).
fn is_luma_green(delta: u8) -> bool {
    !(32..224).contains(&delta)
}

/// True when a delta byte lies in -8..=7 (the LUMA red/blue-minus-green range).
fn is_luma_small(delta: u8) -> bool {
    !(8..248).contains(&delta)
}

/// Encodes one image as a 4-channel QOI stream.
///
/// `bgrx` must hold exactly `width * height` pixels in BGRX byte order, as every
/// [`PixelBuffer`](appricot_core::pixels::PixelBuffer) with format `Bgrx8888` and no
/// stride padding does; the fourth byte of each pixel becomes the QOI alpha channel, so
/// the round trip is byte-exact.
///
/// The chunk choice follows the spec's natural ladder: run, then index, then the smallest
/// delta chunk that fits, then a full pixel. The output is deterministic: the same input
/// always yields the same bytes.
pub(crate) fn encode(width: u32, height: u32, bgrx: &[u8]) -> Vec<u8> {
    let pixels = usize::try_from(width).expect("width fits in usize")
        * usize::try_from(height).expect("height fits in usize");
    debug_assert_eq!(bgrx.len(), pixels * 4, "caller supplies exactly one tile");

    let mut out = Vec::with_capacity(HEADER_LEN + pixels + END_MARKER.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.push(4); // channels: RGBA
    out.push(0); // colorspace: sRGB with linear alpha

    let mut index = [Pixel::TRANSPARENT; INDEX_SLOTS];
    let mut prev = Pixel::BLACK_OPAQUE;
    let mut run: u32 = 0;
    for chunk in bgrx.chunks_exact(4) {
        let px = Pixel::from_bgrx(chunk.try_into().expect("chunks_exact yields four bytes"));
        if px == prev {
            run += 1;
            if run == MAX_RUN {
                push_run(&mut out, run);
                run = 0;
            }
        } else {
            if run > 0 {
                push_run(&mut out, run);
                run = 0;
            }
            emit_pixel(&mut out, &mut index, prev, px);
        }
        prev = px;
    }
    if run > 0 {
        push_run(&mut out, run);
    }
    out.extend_from_slice(&END_MARKER);
    out
}

/// Emits the chunk for `px`, which differs from `prev`: an index hit, a DIFF, a LUMA, a
/// full RGB (alpha carried) or a full RGBA, whichever comes first and fits.
fn emit_pixel(out: &mut Vec<u8>, index: &mut [Pixel; INDEX_SLOTS], prev: Pixel, px: Pixel) {
    let slot = px.index_slot();
    if index[slot] == px {
        // INDEX: the tag bits are zero, so the byte is the slot number itself.
        out.push(u8::try_from(slot).expect("an index slot fits in six bits"));
        return;
    }
    index[slot] = px;

    if px.a != prev.a {
        out.extend_from_slice(&[0xFF, px.r, px.g, px.b, px.a]);
        return;
    }
    // Deltas as two's-complement bytes: wrapping arithmetic on u8 is exactly the spec's
    // signed arithmetic on channel values, without a single cast.
    let dr = px.r.wrapping_sub(prev.r);
    let dg = px.g.wrapping_sub(prev.g);
    let db = px.b.wrapping_sub(prev.b);
    if is_diff_delta(dr) && is_diff_delta(dg) && is_diff_delta(db) {
        let red_bits = dr.wrapping_add(2) & 0x03;
        let green_bits = dg.wrapping_add(2) & 0x03;
        let blue_bits = db.wrapping_add(2) & 0x03;
        out.push(0x40 | (red_bits << 4) | (green_bits << 2) | blue_bits);
        return;
    }
    let red_minus_green = dr.wrapping_sub(dg);
    let blue_minus_green = db.wrapping_sub(dg);
    if is_luma_green(dg) && is_luma_small(red_minus_green) && is_luma_small(blue_minus_green) {
        out.push(0x80 | (dg.wrapping_add(32) & 0x3F));
        out.push(
            ((red_minus_green.wrapping_add(8) & 0x0F) << 4)
                | (blue_minus_green.wrapping_add(8) & 0x0F),
        );
        return;
    }
    out.extend_from_slice(&[0xFE, px.r, px.g, px.b]);
}

/// Emits a RUN chunk for `run` (1..=`MAX_RUN`) repeats of the previous pixel.
fn push_run(out: &mut Vec<u8>, run: u32) {
    debug_assert!((1..=MAX_RUN).contains(&run));
    let biased = u8::try_from(run - 1).expect("a run length fits in six bits");
    out.push(0xC0 | biased);
}

/// A decoded QOI stream: the pixels as `r, g, b, a` quadruples, row-major from the top.
///
/// The dimensions always match the expected size the caller asked for; a 3-channel stream
/// decodes with alpha 255 everywhere.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Decoded {
    /// The pixels, `width * height * 4` bytes.
    pub(crate) rgba: Vec<u8>,
}

/// Decodes a QOI stream whose header must name exactly `expected` as its size.
///
/// Nothing is allocated before the header has been checked against `expected`, so the
/// allocation is always bounded by it. Every malformation the specification allows a
/// decoder to reject is rejected; see the module documentation for the full list.
pub(crate) fn decode(data: &[u8], expected: Size) -> Result<Decoded, DecodeError> {
    if data.len() < HEADER_LEN {
        return Err(DecodeError::QoiTruncated);
    }
    if data[..4] != MAGIC {
        return Err(DecodeError::QoiMagic);
    }
    let width = u32::from_be_bytes(data[4..8].try_into().expect("four header bytes"));
    let height = u32::from_be_bytes(data[8..12].try_into().expect("four header bytes"));
    let channels = data[12];
    if channels != 3 && channels != 4 {
        return Err(DecodeError::QoiChannels(channels));
    }
    let colorspace = data[13];
    if colorspace > 1 {
        return Err(DecodeError::QoiColorspace(colorspace));
    }
    if width != expected.width || height != expected.height {
        return Err(DecodeError::QoiSizeMismatch {
            header: Size::new(width, height),
            expected,
        });
    }

    let pixels = usize::try_from(width).expect("width fits in usize")
        * usize::try_from(height).expect("height fits in usize");
    let mut rgba = vec![0_u8; pixels * 4];
    let mut index = [Pixel::TRANSPARENT; INDEX_SLOTS];
    let mut prev = Pixel::BLACK_OPAQUE;
    let mut pos = HEADER_LEN;
    let mut written = 0_usize;
    while written < pixels {
        if pos >= data.len() {
            return Err(DecodeError::QoiTruncated);
        }
        let b1 = data[pos];
        pos += 1;
        let px = if b1 == 0xFE {
            read_triple(data, &mut pos, prev.a)?
        } else if b1 == 0xFF {
            if channels == 3 {
                return Err(DecodeError::QoiAlphaChunkInThreeChannel);
            }
            if pos + 4 > data.len() {
                return Err(DecodeError::QoiTruncated);
            }
            let px = Pixel {
                r: data[pos],
                g: data[pos + 1],
                b: data[pos + 2],
                a: data[pos + 3],
            };
            pos += 4;
            px
        } else {
            match b1 >> 6 {
                0 => index[usize::from(b1 & 0x3F)],
                1 => {
                    let dr = (b1 >> 4) & 0x03;
                    let dg = (b1 >> 2) & 0x03;
                    let db = b1 & 0x03;
                    Pixel {
                        r: prev.r.wrapping_add(dr.wrapping_sub(2)),
                        g: prev.g.wrapping_add(dg.wrapping_sub(2)),
                        b: prev.b.wrapping_add(db.wrapping_sub(2)),
                        a: prev.a,
                    }
                }
                2 => read_luma(data, &mut pos, prev, b1)?,
                _ => {
                    // RUN: repeat the previous pixel. The index needs no update: the
                    // value is already stored at its slot by the chunk that produced it.
                    let run = usize::from(b1 & 0x3F) + 1;
                    if written + run > pixels {
                        return Err(DecodeError::QoiPixelOverflow);
                    }
                    for _ in 0..run {
                        rgba[written * 4..written * 4 + 4]
                            .copy_from_slice(&[prev.r, prev.g, prev.b, prev.a]);
                        written += 1;
                    }
                    continue;
                }
            }
        };
        index[px.index_slot()] = px;
        prev = px;
        rgba[written * 4..written * 4 + 4].copy_from_slice(&[px.r, px.g, px.b, px.a]);
        written += 1;
    }

    let trailing = data.len() - pos;
    if trailing < END_MARKER.len() {
        return Err(DecodeError::QoiTruncated);
    }
    if trailing > END_MARKER.len() {
        return Err(DecodeError::QoiTrailingData(trailing));
    }
    if data[pos..] != END_MARKER {
        return Err(DecodeError::QoiBadEndMarker);
    }
    Ok(Decoded { rgba })
}

/// Reads the three payload bytes of an RGB chunk; `alpha` is the previous alpha.
fn read_triple(data: &[u8], pos: &mut usize, alpha: u8) -> Result<Pixel, DecodeError> {
    if *pos + 3 > data.len() {
        return Err(DecodeError::QoiTruncated);
    }
    let px = Pixel {
        r: data[*pos],
        g: data[*pos + 1],
        b: data[*pos + 2],
        a: alpha,
    };
    *pos += 3;
    Ok(px)
}

/// Reads a LUMA chunk: the green delta in `b1`'s low six bits, the red- and
/// blue-minus-green deltas in the second byte's nibbles.
fn read_luma(data: &[u8], pos: &mut usize, prev: Pixel, b1: u8) -> Result<Pixel, DecodeError> {
    if *pos >= data.len() {
        return Err(DecodeError::QoiTruncated);
    }
    let b2 = data[*pos];
    *pos += 1;
    // Wrapping u8 arithmetic is the spec's signed deltas in two's complement.
    let green = (b1 & 0x3F).wrapping_sub(32);
    let red_minus_green = (b2 >> 4).wrapping_sub(8);
    let blue_minus_green = (b2 & 0x0F).wrapping_sub(8);
    Ok(Pixel {
        r: prev.r.wrapping_add(green.wrapping_add(red_minus_green)),
        g: prev.g.wrapping_add(green),
        b: prev.b.wrapping_add(green.wrapping_add(blue_minus_green)),
        a: prev.a,
    })
}

#[cfg(test)]
mod tests {
    use super::{Decoded, END_MARKER, HEADER_LEN, MAGIC, decode, encode};
    use crate::DecodeError;
    use appricot_core::Size;

    /// Assembles a QOI stream from its parts.
    fn stream(width: u32, height: u32, channels: u8, chunks: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + chunks.len() + END_MARKER.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out.push(channels);
        out.push(0);
        out.extend_from_slice(chunks);
        out.extend_from_slice(&END_MARKER);
        out
    }

    /// BGRX pixels (blue, green, red, unused) packed as one row.
    fn bgrx(pixels: &[[u8; 4]]) -> Vec<u8> {
        pixels.concat()
    }

    fn decode_ok(data: &[u8], width: u32, height: u32) -> Vec<u8> {
        let Decoded { rgba } = decode(data, Size::new(width, height))
            .unwrap_or_else(|e| panic!("decode failed: {e:?}"));
        rgba
    }

    #[test]
    fn encodes_the_header_a_single_rgb_chunk_and_the_end_marker() {
        // One pixel (r 0x11, g 0x22, b 0x33, alpha 0xFF): no diff or luma chunk fits the
        // delta from the initial previous pixel (0,0,0,255), so a full RGB chunk follows.
        let out = encode(1, 1, &bgrx(&[[0x33, 0x22, 0x11, 0xFF]]));
        let mut expected = Vec::new();
        expected.extend_from_slice(b"qoif");
        expected.extend_from_slice(&1_u32.to_be_bytes());
        expected.extend_from_slice(&1_u32.to_be_bytes());
        expected.extend_from_slice(&[4, 0]);
        expected.extend_from_slice(&[0xFE, 0x11, 0x22, 0x33]);
        expected.extend_from_slice(&END_MARKER);
        assert_eq!(out, expected);
    }

    #[test]
    fn flat_opaque_black_is_one_run_chunk() {
        // Eight copies of the initial previous pixel (0,0,0,255): a single run of 8,
        // stored as run-1 = 7 with the 2-bit tag 0b11.
        let row = [[0_u8, 0, 0, 0xFF]; 8];
        let out = encode(4, 2, &bgrx(&row));
        // Run of 8: the byte is 0xC0 | (8 - 1).
        assert_eq!(out[HEADER_LEN], 0xC7);
        assert_eq!(out.len(), HEADER_LEN + 1 + END_MARKER.len());
    }

    #[test]
    fn rising_red_channel_is_a_diff_chain_after_a_one_pixel_run() {
        // x=0 equals the initial previous pixel (0,0,0,255): a run of one. Every later
        // pixel is +1 red: a DIFF chunk per pixel (dr=1, dg=0, db=0 -> 3, 2, 2).
        let pixels: Vec<[u8; 4]> = (0..8)
            .map(|x| {
                let red = u8::try_from(x).expect("x stays below 256");
                [0, 0, red, 0xFF]
            })
            .collect();
        let out = encode(8, 1, &bgrx(&pixels));
        let chunks = &out[HEADER_LEN..out.len() - END_MARKER.len()];
        assert_eq!(chunks[0], 0xC0); // run of 1: 1-1 = 0
        assert_eq!(&chunks[1..], &[0x7A; 7]); // 0x40 | 3<<4 | 2<<2 | 2
    }

    #[test]
    fn a_repeated_pixel_after_two_new_ones_is_an_index_chunk() {
        // A = (0x0A, 0x14, 0x1E, 0xFF), B = (0xC8, 0x64, 0x32, 0xFF), then A again: no
        // diff or luma fits either step, but A is still in the index at slot
        // (10*3 + 20*5 + 30*7 + 255*11) % 64 = 9, so the third pixel is one INDEX byte.
        let out = encode(
            3,
            1,
            &bgrx(&[
                [0x1E, 0x14, 0x0A, 0xFF],
                [0x32, 0x64, 0xC8, 0xFF],
                [0x1E, 0x14, 0x0A, 0xFF],
            ]),
        );
        let chunks = &out[HEADER_LEN..out.len() - END_MARKER.len()];
        assert_eq!(
            chunks,
            &[0xFE, 0x0A, 0x14, 0x1E, 0xFE, 0xC8, 0x64, 0x32, 0x09]
        );
    }

    #[test]
    fn an_alpha_change_forces_rgba_chunks() {
        // DIFF and LUMA carry the alpha over, so two pixels that differ only in alpha
        // need full RGBA chunks.
        let out = encode(2, 1, &bgrx(&[[0, 0, 0, 0x80], [0, 0, 0, 0x81]]));
        let chunks = &out[HEADER_LEN..out.len() - END_MARKER.len()];
        assert_eq!(chunks, &[0xFF, 0, 0, 0, 0x80, 0xFF, 0, 0, 0, 0x81]);
    }

    #[test]
    fn a_two_channel_step_uses_luma() {
        // A = (0x64, 0x64, 0x64, 0xFF) then B = (0x55, 0x50, 0x56, 0xFF): deltas
        // dr = -15, dg = -20, db = -14. Green -20 is out of DIFF range but inside LUMA's
        // -32..=31, with dr-dg = 5 and db-dg = 6 inside -8..=7.
        let out = encode(
            2,
            1,
            &bgrx(&[[0x64, 0x64, 0x64, 0xFF], [0x56, 0x50, 0x55, 0xFF]]),
        );
        let chunks = &out[HEADER_LEN..out.len() - END_MARKER.len()];
        // The LUMA chunk: 0x8C = 0x80 | (-20 + 32); 0xDE = (5 + 8) << 4 | (6 + 8).
        assert_eq!(chunks, &[0xFE, 0x64, 0x64, 0x64, 0x8C, 0xDE]);
    }

    #[test]
    fn long_runs_flush_in_max_chunks() {
        // 126 copies of one gray: the first differs from the initial previous pixel by
        // too much for a delta chunk (green delta 100 is outside -32..=31), so one full
        // RGB chunk, then two runs of 62 (the 6-bit maximum) and one of one.
        let pixels = [[0x64_u8, 0x64, 0x64, 0xFF]; 126];
        let out = encode(126, 1, &bgrx(&pixels));
        let chunks = &out[HEADER_LEN..out.len() - END_MARKER.len()];
        // 0xFD is a run of 62 (62 - 1 = 61), 0xC0 a run of one.
        assert_eq!(chunks, &[0xFE, 0x64, 0x64, 0x64, 0xFD, 0xFD, 0xC0]);
    }

    #[test]
    fn decodes_a_luma_chunk_by_hand() {
        // Header 2x1, then RGB (0x64,0x64,0x64) then LUMA (dg=-20, dr-dg=5, db-dg=6).
        let data = stream(2, 1, 4, &[0xFE, 0x64, 0x64, 0x64, 0x8C, 0xDE]);
        assert_eq!(
            decode_ok(&data, 2, 1),
            vec![0x64, 0x64, 0x64, 0xFF, 0x55, 0x50, 0x56, 0xFF]
        );
    }

    #[test]
    fn decodes_run_chunks() {
        // One RGB pixel followed by a run of four: five pixels in two chunks.
        let data = stream(5, 1, 4, &[0xFE, 10, 20, 30, 0xC3]);
        let rgba = decode_ok(&data, 5, 1);
        assert_eq!(rgba.len(), 20);
        assert!(rgba.chunks_exact(4).all(|px| px == [10, 20, 30, 255]));
    }

    #[test]
    fn decodes_three_channel_streams_as_opaque() {
        // A 3-channel stream: RGB then a run of one. Alpha stays at the initial 255.
        let data = stream(2, 1, 3, &[0xFE, 10, 20, 30, 0xC0]);
        assert_eq!(
            decode_ok(&data, 2, 1),
            vec![10, 20, 30, 255, 10, 20, 30, 255]
        );
    }

    #[test]
    fn rejects_malformed_streams() {
        let good = stream(2, 1, 4, &[0xFE, 1, 2, 3, 0xC0]);
        let expected = Size::new(2, 1);

        // Bad magic.
        let mut bad_magic = good.clone();
        bad_magic[0] = b'x';
        assert_eq!(decode(&bad_magic, expected), Err(DecodeError::QoiMagic));

        // Channels outside 3 and 4.
        let bad_channels = stream(2, 1, 2, &[0xC0, 0xC0]);
        assert_eq!(
            decode(&bad_channels, expected),
            Err(DecodeError::QoiChannels(2))
        );

        // Colorspace outside 0 and 1.
        let mut bad_colorspace = good.clone();
        bad_colorspace[13] = 7;
        assert_eq!(
            decode(&bad_colorspace, expected),
            Err(DecodeError::QoiColorspace(7))
        );

        // Header dimensions that disagree with the expected size.
        assert_eq!(
            decode(&good, Size::new(1, 2)),
            Err(DecodeError::QoiSizeMismatch {
                header: Size::new(2, 1),
                expected: Size::new(1, 2),
            })
        );

        // A truncated stream, chunk payload and end marker alike.
        assert_eq!(
            decode(&good[..good.len() - 1], expected),
            Err(DecodeError::QoiTruncated)
        );
        assert_eq!(
            decode(&stream(2, 1, 4, &[0xFE, 1, 2]), expected),
            Err(DecodeError::QoiTruncated)
        );
        assert_eq!(
            decode(&good[..HEADER_LEN], expected),
            Err(DecodeError::QoiTruncated)
        );

        // Bytes after the end marker: the count includes the marker's eight bytes.
        let mut trailing = good.clone();
        trailing.extend_from_slice(&[0xC0, 0xC0]);
        assert_eq!(
            decode(&trailing, expected),
            Err(DecodeError::QoiTrailingData(10))
        );

        // A damaged end marker.
        let mut bad_marker = good.clone();
        let last = bad_marker.len() - 1;
        bad_marker[last] = 0x02;
        assert_eq!(
            decode(&bad_marker, expected),
            Err(DecodeError::QoiBadEndMarker)
        );

        // A run that produces more pixels than the header declares.
        let overflowing = stream(2, 1, 4, &[0xFE, 1, 2, 3, 0xC2]);
        assert_eq!(
            decode(&overflowing, expected),
            Err(DecodeError::QoiPixelOverflow)
        );

        // An RGBA chunk inside a 3-channel stream, which the spec forbids encoders from
        // issuing.
        let alpha_in_rgb = stream(2, 1, 3, &[0xFF, 1, 2, 3, 4, 0xC0]);
        assert_eq!(
            decode(&alpha_in_rgb, expected),
            Err(DecodeError::QoiAlphaChunkInThreeChannel)
        );
    }

    #[test]
    fn encoded_streams_decode_back_byte_for_byte() {
        // Every chunk type appears in this mixed image: a run, a delta chunk, a full
        // pixel, an index hit and a full pixel with an alpha change.
        let mut pixels = Vec::new();
        pixels.extend([[0_u8, 0, 0, 0xFF]; 40]); // run (equals the initial previous pixel)
        pixels.push([1, 2, 3, 0xFF]); // a delta chunk
        pixels.extend([[1, 2, 3, 0xFF]; 70]); // run, flushing at 62
        pixels.push([2, 2, 3, 0xFF]); // a smaller delta chunk
        pixels.push([40, 60, 70, 0xFF]); // a full pixel (deltas too large)
        pixels.push([1, 2, 3, 0xFF]); // an index hit (seen above)
        pixels.push([0, 0, 0, 0x80]); // a full pixel with an alpha change
        let width = u32::try_from(pixels.len()).expect("a one-row image");
        let bgrx_pixels = pixels;
        let out = encode(width, 1, &bgrx(&bgrx_pixels));
        let Decoded { rgba } =
            decode(&out, Size::new(width, 1)).expect("a well-formed stream decodes");
        for (source, decoded) in bgrx_pixels.iter().zip(rgba.chunks_exact(4)) {
            assert_eq!(decoded, [source[2], source[1], source[0], source[3]]);
        }
    }
}
