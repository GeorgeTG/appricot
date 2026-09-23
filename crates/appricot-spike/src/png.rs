//! PNG snapshots, written by hand.
//!
//! A snapshot is evidence a person looks at (the spike's visual checks), so it must open in any
//! viewer, and it is written rarely. So the encoder is the simplest valid PNG: 8-bit RGB, no
//! interlace, filter type 0 on every row, and a zlib stream of STORED deflate blocks — no
//! compression at all. The file is about as large as the raw pixels, which is fine for a
//! handful of screenshots, and it keeps a compression library out of the graph.
//!
//! The format follows the PNG specification (W3C, Third Edition,
//! <https://www.w3.org/TR/png-3/>), zlib (RFC 1950) and deflate's stored blocks (RFC 1951
//! §3.2.4), all checked 2026-09-23.

/// What stops a snapshot from being encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PngError {
    /// A side is zero. PNG has no empty image.
    Empty,
    /// The pixel buffer is not `width * height` pixels long.
    LengthMismatch {
        /// The buffer's length in bytes.
        len: usize,
        /// The length the size asks for.
        expected: usize,
    },
}

impl std::fmt::Display for PngError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "an image needs a width and a height"),
            Self::LengthMismatch { len, expected } => {
                write!(f, "{len} bytes of pixels where {expected} were expected")
            }
        }
    }
}

impl std::error::Error for PngError {}

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// The largest payload one stored deflate block carries (its LEN field is 16 bits).
const STORED_BLOCK_MAX: usize = 0xffff;

/// Encodes BGRX pixels (4 bytes per pixel, blue first, the fourth byte ignored, rows packed
/// with no padding) as a PNG. This is the layout of a decoded tile.
pub fn encode_bgrx(width: u32, height: u32, bgrx: &[u8]) -> Result<Vec<u8>, PngError> {
    let (w, h) = sides(width, height)?;
    let expected = w * h * 4;
    if bgrx.len() != expected {
        return Err(PngError::LengthMismatch {
            len: bgrx.len(),
            expected,
        });
    }
    // Each scanline: the filter-type byte 0 (None), then RGB.
    let mut scanlines = Vec::with_capacity(h * (1 + w * 3));
    for row in bgrx.chunks_exact(w * 4) {
        scanlines.push(0);
        for px in row.chunks_exact(4) {
            scanlines.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    Ok(assemble(width, height, &scanlines))
}

fn sides(width: u32, height: u32) -> Result<(usize, usize), PngError> {
    if width == 0 || height == 0 {
        return Err(PngError::Empty);
    }
    // u32 always fits in usize on the 64-bit targets this tool runs on; a failure is a
    // length the buffer check would refuse anyway.
    let w = usize::try_from(width).map_err(|_| PngError::Empty)?;
    let h = usize::try_from(height).map_err(|_| PngError::Empty)?;
    Ok((w, h))
}

fn assemble(width: u32, height: u32, scanlines: &[u8]) -> Vec<u8> {
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // Bit depth 8, colour type 2 (truecolour), compression 0, filter 0, interlace 0.
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);

    let idat = zlib_stored(scanlines);
    let mut out = Vec::with_capacity(SIGNATURE.len() + idat.len() + 64);
    out.extend_from_slice(&SIGNATURE);
    push_chunk(&mut out, *b"IHDR", &ihdr);
    push_chunk(&mut out, *b"IDAT", &idat);
    push_chunk(&mut out, *b"IEND", &[]);
    out
}

/// A zlib stream (RFC 1950) holding `data` in stored deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let blocks = data.len().div_ceil(STORED_BLOCK_MAX).max(1);
    let mut out = Vec::with_capacity(data.len() + blocks * 5 + 6);
    // CMF 0x78: deflate with a 32 KiB window. FLG 0x01: no dictionary, fastest level, and
    // (0x78 << 8 | 0x01) is a multiple of 31, as the header check requires.
    out.extend_from_slice(&[0x78, 0x01]);
    if data.is_empty() {
        push_stored_block(&mut out, &[], true);
    } else {
        let mut chunks = data.chunks(STORED_BLOCK_MAX).peekable();
        while let Some(chunk) = chunks.next() {
            push_stored_block(&mut out, chunk, chunks.peek().is_none());
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn push_stored_block(out: &mut Vec<u8>, chunk: &[u8], last: bool) {
    // BFINAL in bit 0, BTYPE 00 (stored); the rest of the byte is padding to the boundary.
    out.push(u8::from(last));
    let len = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(chunk);
}

fn push_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(&kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// Adler-32 (RFC 1950 §8.2).
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1_u32, 0_u32);
    // 5552 bytes is the most that can be summed before `b` could overflow a u32.
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// CRC-32 as PNG uses it: the reflected polynomial 0xEDB88320 (PNG spec, Annex D).
#[derive(Debug, Clone, Copy)]
pub struct Crc32(u32);

const CRC_TABLE: [u32; 256] = {
    let mut table = [0_u32; 256];
    let mut n = 0;
    while n < 256 {
        #[expect(clippy::cast_possible_truncation, reason = "n < 256")]
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// A CRC over no bytes yet.
    pub const fn new() -> Self {
        Self(0xffff_ffff)
    }

    /// Feeds `data`.
    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            let index = usize::from(self.0.to_le_bytes()[0] ^ byte);
            self.0 = CRC_TABLE[index] ^ (self.0 >> 8);
        }
    }

    /// The CRC of everything fed so far.
    pub const fn finish(self) -> u32 {
        self.0 ^ 0xffff_ffff
    }
}

#[cfg(test)]
mod tests {
    use super::{Crc32, PngError, SIGNATURE, adler32, encode_bgrx};

    fn crc(data: &[u8]) -> u32 {
        let mut c = Crc32::new();
        c.update(data);
        c.finish()
    }

    fn be32(bytes: &[u8]) -> u32 {
        u32::from_be_bytes(bytes.try_into().expect("four bytes"))
    }

    /// Walks the chunks, checking every CRC, and returns (type, data) pairs.
    fn chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&png[..8], &SIGNATURE);
        let mut at = 8;
        let mut out = Vec::new();
        while at < png.len() {
            let len = usize::try_from(be32(&png[at..at + 4])).expect("a length");
            let kind: [u8; 4] = png[at + 4..at + 8].try_into().expect("a type");
            let data = png[at + 8..at + 8 + len].to_vec();
            let stored_crc = be32(&png[at + 8 + len..at + 12 + len]);
            assert_eq!(crc(&png[at + 4..at + 8 + len]), stored_crc, "chunk CRC");
            out.push((kind, data));
            at += 12 + len;
        }
        out
    }

    /// Reads a zlib stream of stored blocks back, checking the header and the Adler-32.
    fn inflate_stored(z: &[u8]) -> Vec<u8> {
        assert_eq!(
            (u16::from(z[0]) << 8 | u16::from(z[1])) % 31,
            0,
            "zlib FCHECK"
        );
        let mut at = 2;
        let mut out = Vec::new();
        loop {
            let header = z[at];
            assert_eq!(header & 0b110, 0, "a stored block");
            let len = usize::from(u16::from_le_bytes([z[at + 1], z[at + 2]]));
            let nlen = u16::from_le_bytes([z[at + 3], z[at + 4]]);
            assert_eq!(usize::from(!nlen), len, "NLEN is the complement of LEN");
            out.extend_from_slice(&z[at + 5..at + 5 + len]);
            at += 5 + len;
            if header & 1 == 1 {
                break;
            }
        }
        assert_eq!(be32(&z[at..at + 4]), adler32(&out), "Adler-32");
        assert_eq!(at + 4, z.len(), "nothing after the checksum");
        out
    }

    #[test]
    fn the_checksums_match_their_published_values() {
        // The CRC of the IEND type is the fixed trailer of every PNG.
        assert_eq!(crc(b"IEND"), 0xae42_6082);
        assert_eq!(crc(b"123456789"), 0xcbf4_3926);
        // RFC 1950's Adler-32 of "Wikipedia", the usual worked example.
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
        assert_eq!(adler32(b""), 1);
    }

    #[test]
    fn a_small_image_is_a_valid_png_holding_its_pixels() {
        // 2x2: red, green / blue, white, in BGRX.
        let bgrx = [0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0];
        let png = encode_bgrx(2, 2, &bgrx).expect("a valid image encodes");
        let chunks = chunks(&png);
        let kinds: Vec<_> = chunks.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, [*b"IHDR", *b"IDAT", *b"IEND"]);
        let ihdr = &chunks[0].1;
        assert_eq!(be32(&ihdr[0..4]), 2);
        assert_eq!(be32(&ihdr[4..8]), 2);
        assert_eq!(&ihdr[8..], &[8, 2, 0, 0, 0]);
        let raw = inflate_stored(&chunks[1].1);
        // Each row: filter byte 0, then RGB per pixel.
        let rows: [[u8; 7]; 2] = [[0, 255, 0, 0, 0, 255, 0], [0, 0, 0, 255, 255, 255, 255]];
        assert_eq!(raw, rows.concat());
    }

    #[test]
    fn a_large_image_spans_several_stored_blocks() {
        let (w, h) = (300_u32, 300_u32);
        let bgrx: Vec<u8> = (0..w * h * 4)
            .map(|i| u8::try_from(i % 251).expect("below 251"))
            .collect();
        let png = encode_bgrx(w, h, &bgrx).expect("a valid image encodes");
        let raw = inflate_stored(&chunks(&png)[1].1);
        assert_eq!(raw.len(), 300 * (1 + 300 * 3));
        assert!(raw.len() > 0xffff, "the test needs more than one block");
    }

    #[test]
    fn a_bad_size_is_refused() {
        assert_eq!(encode_bgrx(0, 4, &[]), Err(PngError::Empty));
        assert_eq!(
            encode_bgrx(2, 2, &[0; 15]),
            Err(PngError::LengthMismatch {
                len: 15,
                expected: 16
            })
        );
    }
}
