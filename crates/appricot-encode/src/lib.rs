//! Tile encoders: they turn captured pixels into the payloads of draw messages.
//!
//! # Responsibility
//!
//! - Cut a damaged rectangle into grid-aligned tiles ([`cut_into_tiles`]) and encode each
//!   one ([`encode_tile`], or [`encode_tile_owned`] on the hot path) with a lossless codec:
//!   RAW (wire id 1, the pixels themselves) or QOI (wire id 2, implemented in-house from
//!   the published specification in the private `qoi` module).
//! - Mirror both codecs back to captured pixels ([`decode_tile`]), for tests, resync
//!   paths and future server-side consumers.
//!
//! # The fourth byte
//!
//! A `Bgrx8888` pixel is blue, green, red and one unused byte (docs/protocol/v0.md §11).
//! The unused byte carries nothing on the wire, whatever codec a tile gets: RAW sends it
//! and every decoder ignores it, QOI does not encode it, and every tile decodes to opaque
//! pixels. So a capture buffer decodes to the same pixels as RAW and as QOI, and the
//! per-tile choice between them never shows. A depth-32 window's alpha is dropped: v0
//! defines no transparency.
//!
//! # Not its responsibility
//!
//! - Capture (`appricot-x11`), pacing (`appricot-core`) and the wire format
//!   (`appricot-proto`).
//! - Any dependency outside the permissive allow list (docs/adr/0002-licence.md, enforced
//!   by `deny.toml`). Codecs are where copyleft hides: QOI is implemented in-house from
//!   the spec at <https://qoiformat.org/> rather than pulled off crates.io, so the codec
//!   surface stays licence-audited by hand.

mod qoi;
#[cfg(test)]
mod tile_vectors;

use appricot_core::{PixelBuffer, PixelFormat, Rect, Size};
use std::fmt;

/// Edge length of a square tile, in pixels.
///
/// The tile grid of [`cut_into_tiles`] is aligned to multiples of this. A full redraw of
/// the development surface (1400x900) is `6 * 4 = 24` tiles and even the wire protocol's
/// largest surface (1920x1200) is `8 * 5 = 40`, both inside the wire budget of
/// MAX_TILES_PER_FRAME = 48 (crates/appricot-proto/proto/appricot/v0/wire.proto).
pub const TILE_SIZE: u32 = 256;

/// The wire limit on one tile's payload, in bytes: MAX_TILE_BYTES, mirrored from
/// crates/appricot-proto/proto/appricot/v0/wire.proto (encode depends on no sibling but
/// core, so the number is repeated here). A RAW 256x256 tile is exactly this many bytes,
/// and [`encode_tile`] never returns more: QOI falls back to RAW before it could.
///
/// Both numbers are pinned at compile time: against each other below, and against the
/// wire's limits table in crates/appricot-streamer/tests/encode_limits.rs, the one crate
/// that sees both sides.
pub const MAX_TILE_BYTES: usize = 262_144;

// A full RAW tile is exactly the payload cap: TILE_SIZE squared, four bytes per pixel.
const _: () = assert!(MAX_TILE_BYTES == 4 * (TILE_SIZE as usize) * (TILE_SIZE as usize));

/// How a tile's payload is encoded. The wire codec ids are stable and live in the limits
/// table of the protocol; [`Encoding::id`] and [`Encoding::from_id`] translate.
///
/// Not `#[non_exhaustive]`: a codec is added together with every crate that matches on
/// this, so a new variant should break those matches rather than hide behind a wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// Uncompressed pixels: row-major from the top, 4 bytes per pixel in blue, green,
    /// red, unused order, no stride padding. The unused byte is sent as captured, and
    /// every decoder ignores it. Wire id 1; every peer must decode it.
    Raw,
    /// The QOI image format, implemented in-house from the published specification: a
    /// 3-channel stream of opaque pixels. Wire id 2.
    Qoi,
}

impl Encoding {
    /// The stable wire id of this encoding.
    pub const fn id(self) -> u32 {
        match self {
            Self::Raw => 1,
            Self::Qoi => 2,
        }
    }

    /// The encoding a wire id names, or `None` when the id is not a v0 codec (0 is never
    /// a codec).
    pub const fn from_id(id: u32) -> Option<Self> {
        match id {
            1 => Some(Self::Raw),
            2 => Some(Self::Qoi),
            _ => None,
        }
    }

    /// True when decoding gives back exactly the captured colour: blue, green and red.
    /// Every v0 codec is lossless. The unused fourth byte is not colour, and it decodes as
    /// 255 whatever the codec (see "The fourth byte" in the crate documentation).
    pub const fn is_lossless(self) -> bool {
        match self {
            Self::Raw | Self::Qoi => true,
        }
    }
}

/// One encoded tile: the wire codec id that was actually used, and its payload.
///
/// [`encode_tile`] takes the preferred encoding as a hint, not an order: when QOI would
/// not pay for itself the tile comes back as RAW, which is why this carries the codec id
/// rather than an [`Encoding`] the caller already had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedTile {
    /// The wire codec id of `data` (1 RAW, 2 QOI).
    pub codec: u32,
    /// The encoded pixels, never longer than [`MAX_TILE_BYTES`].
    pub data: Vec<u8>,
}

/// What can go wrong while encoding a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// The buffer covers no pixel; a tile is never empty.
    EmptyTile,
    /// The buffer is wider or taller than [`TILE_SIZE`]; a tile is at most one grid cell.
    TileTooLarge {
        /// The offending width.
        width: u32,
        /// The offending height.
        height: u32,
    },
    /// The buffer's pixel layout is not `Bgrx8888`, the only one the wire v0 codecs name.
    UnsupportedFormat(PixelFormat),
    /// The stride is not `width * 4`: tiles carry no padding.
    StrideMismatch {
        /// The stride the buffer carries.
        stride: usize,
        /// The stride a tile of this width must have.
        expected: usize,
    },
    /// The data is not exactly `width * height * 4` bytes.
    DataLengthMismatch {
        /// The byte count the buffer carries.
        len: usize,
        /// The byte count a tile of this size must have.
        expected: usize,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTile => write!(f, "a tile is never empty"),
            Self::TileTooLarge { width, height } => {
                write!(
                    f,
                    "a tile is at most {TILE_SIZE} pixels, got {width}x{height}"
                )
            }
            Self::UnsupportedFormat(format) => {
                write!(
                    f,
                    "unsupported pixel layout {format:?}, only Bgrx8888 is encoded"
                )
            }
            Self::StrideMismatch { stride, expected } => {
                write!(f, "stride {stride} does not match width*4 ({expected})")
            }
            Self::DataLengthMismatch { len, expected } => {
                write!(f, "tile data is {len} bytes, expected {expected}")
            }
        }
    }
}

impl std::error::Error for EncodeError {}

/// What can go wrong while decoding a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The codec id names no v0 codec (0 is never a codec).
    UnknownCodec(u32),
    /// The size covers no pixel.
    EmptySize,
    /// The size is wider or taller than [`TILE_SIZE`]; a tile is at most one grid cell.
    SizeTooLarge {
        /// The offending width.
        width: u32,
        /// The offending height.
        height: u32,
    },
    /// A RAW payload whose length is not `width * height * 4`.
    RawLengthMismatch {
        /// The byte count the payload carries.
        len: usize,
        /// The byte count a tile of this size must have.
        expected: usize,
    },
    /// A QOI stream whose magic is not `qoif`.
    QoiMagic,
    /// A QOI header whose channels byte is neither 3 nor 4.
    QoiChannels(u8),
    /// A QOI header whose colorspace byte is neither 0 nor 1.
    QoiColorspace(u8),
    /// A QOI header naming dimensions other than the expected size.
    QoiSizeMismatch {
        /// The dimensions the header names.
        header: Size,
        /// The size the caller asked for.
        expected: Size,
    },
    /// The stream ends before the image and its end marker are complete.
    QoiTruncated,
    /// Bytes found after the end marker: the count includes the marker's eight bytes.
    QoiTrailingData(usize),
    /// The eight bytes where the end marker belongs are not it.
    QoiBadEndMarker,
    /// A chunk produced more pixels than the header declares.
    QoiPixelOverflow,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCodec(id) => write!(f, "unknown codec id {id}"),
            Self::EmptySize => write!(f, "a tile is never empty"),
            Self::SizeTooLarge { width, height } => {
                write!(
                    f,
                    "a tile is at most {TILE_SIZE} pixels, got {width}x{height}"
                )
            }
            Self::RawLengthMismatch { len, expected } => {
                write!(f, "RAW payload is {len} bytes, expected {expected}")
            }
            Self::QoiMagic => write!(f, "not a QOI stream"),
            Self::QoiChannels(channels) => write!(f, "QOI channels byte {channels}, not 3 or 4"),
            Self::QoiColorspace(colorspace) => {
                write!(f, "QOI colorspace byte {colorspace}, not 0 or 1")
            }
            Self::QoiSizeMismatch { header, expected } => write!(
                f,
                "QOI header names {}x{}, expected {}x{}",
                header.width, header.height, expected.width, expected.height
            ),
            Self::QoiTruncated => write!(f, "QOI stream ends early"),
            Self::QoiTrailingData(count) => {
                write!(
                    f,
                    "{count} bytes trail the last QOI pixel (marker expected)"
                )
            }
            Self::QoiBadEndMarker => write!(f, "QOI end marker is damaged"),
            Self::QoiPixelOverflow => write!(f, "QOI chunk overruns the declared pixels"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Cuts `rect`, typically a damage rectangle, into tiles on a grid aligned to the surface
/// origin, clipped to `bounds` (the surface, whose origin is `(0, 0)`).
///
/// # The alignment rule
///
/// The grid lines are the multiples of [`TILE_SIZE`], aligned to the surface, NOT to
/// `rect`: every returned tile is the intersection of the clipped rectangle with one
/// grid cell, so a tile never crosses a grid line in either axis. Two damage rectangles
/// that overlap therefore cut into tiles whose edges coincide wherever they touch the
/// same cell, and the client can cache decoded tiles keyed by grid cell: partial damage
/// updates a cell's contents, never its position.
///
/// The tiles come back row-major from the top-left, each at most [`TILE_SIZE`] by
/// [`TILE_SIZE`] (smaller where the rectangle or the surface ends). A rectangle that is
/// empty or does not overlap `bounds` yields no tiles. This is pure geometry: it does not
/// enforce the wire protocol's MAX_TILES_PER_FRAME; the caller composes frames.
pub fn cut_into_tiles(rect: Rect, bounds: Size) -> Vec<Rect> {
    let surface = Rect::new(0, 0, bounds.width, bounds.height);
    let Some(damage) = rect.intersection(surface) else {
        return Vec::new();
    };
    if damage.is_empty() {
        return Vec::new();
    }

    let s = i64::from(TILE_SIZE);
    let left = i64::from(damage.origin.x);
    let top = i64::from(damage.origin.y);
    let right = left + i64::from(damage.size.width);
    let bottom = top + i64::from(damage.size.height);
    let first_column = left.div_euclid(s);
    let last_column = (right - 1).div_euclid(s);
    let first_row = top.div_euclid(s);
    let last_row = (bottom - 1).div_euclid(s);

    let mut tiles = Vec::new();
    for row in first_row..=last_row {
        for column in first_column..=last_column {
            let cell = Rect::new(
                saturate_to_i32(column * s),
                saturate_to_i32(row * s),
                TILE_SIZE,
                TILE_SIZE,
            );
            if let Some(tile) = damage.intersection(cell) {
                tiles.push(tile);
            }
        }
    }
    tiles
}

/// Converts to `i32`, clamping at its bounds instead of wrapping.
fn saturate_to_i32(value: i64) -> i32 {
    let clamped = value.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    i32::try_from(clamped).unwrap_or_default()
}

/// Encodes exactly one tile of pixels.
///
/// `buf` must be a single tile: format `Bgrx8888`, at most [`TILE_SIZE`] by
/// [`TILE_SIZE`], `stride == width * 4`, exactly `width * height * 4` bytes of data.
/// `prefer` names the codec to try; RAW is always accepted, and QOI is used only when it
/// does not lose to RAW: if the QOI stream would be longer than the RAW payload or exceed
/// [`MAX_TILE_BYTES`], the tile comes back as RAW. A tie keeps QOI. The QOI encoder stops
/// as soon as its stream passes that size, so a tile that does not compress costs little
/// more than a RAW one. Either way the payload never exceeds the RAW size, and both codecs
/// decode to the same opaque pixels (see "The fourth byte" in the crate documentation).
///
/// This borrows the buffer, so a tile that goes out RAW copies its pixels. A caller that
/// owns the buffer and drops it afterwards should call [`encode_tile_owned`], which moves
/// them instead.
///
/// # Examples
///
/// ```
/// use appricot_core::{PixelFormat, PixelBuffer, Size};
/// use appricot_encode::{Encoding, decode_tile, encode_tile};
///
/// let buf = PixelBuffer {
///     size: Size::new(2, 1),
///     stride: 8,
///     format: PixelFormat::Bgrx8888,
///     data: vec![10, 20, 30, 255, 40, 50, 60, 255],
/// };
/// // QOI cannot compress two pixels below eight raw bytes, so the tile comes back RAW.
/// let tile = encode_tile(&buf, Encoding::Qoi).expect("a valid tile encodes");
/// assert_eq!(tile.codec, Encoding::Raw.id());
/// let back = decode_tile(tile.codec, &tile.data, buf.size).expect("our own payload decodes");
/// assert_eq!(back, buf);
/// ```
pub fn encode_tile(buf: &PixelBuffer, prefer: Encoding) -> Result<EncodedTile, EncodeError> {
    let raw_len = check_tile(buf)?;
    Ok(
        qoi_tile(buf, prefer, raw_len).unwrap_or_else(|| EncodedTile {
            codec: Encoding::Raw.id(),
            data: buf.data.clone(),
        }),
    )
}

/// Encodes exactly one tile of pixels, as [`encode_tile`] does, and consumes the buffer.
///
/// The result is the one [`encode_tile`] gives for the same buffer. The difference is the
/// cost: a tile that goes out RAW moves the buffer's pixels into the payload instead of
/// copying them. This is the form for the capture-and-send hot path, whose buffer is
/// dropped right after encoding.
///
/// # Examples
///
/// ```
/// use appricot_core::{PixelFormat, PixelBuffer, Size};
/// use appricot_encode::{Encoding, encode_tile_owned};
///
/// let buf = PixelBuffer {
///     size: Size::new(1, 1),
///     stride: 4,
///     format: PixelFormat::Bgrx8888,
///     data: vec![10, 20, 30, 255],
/// };
/// let pixels = buf.data.as_ptr();
/// let tile = encode_tile_owned(buf, Encoding::Raw).expect("a valid tile encodes");
/// assert_eq!(tile.data.as_ptr(), pixels, "the RAW payload is the buffer, not a copy");
/// ```
pub fn encode_tile_owned(buf: PixelBuffer, prefer: Encoding) -> Result<EncodedTile, EncodeError> {
    let raw_len = check_tile(&buf)?;
    Ok(qoi_tile(&buf, prefer, raw_len).unwrap_or(EncodedTile {
        codec: Encoding::Raw.id(),
        data: buf.data,
    }))
}

/// Checks that `buf` is exactly one tile, and returns its RAW payload length.
fn check_tile(buf: &PixelBuffer) -> Result<usize, EncodeError> {
    let Size { width, height } = buf.size;
    if width == 0 || height == 0 {
        return Err(EncodeError::EmptyTile);
    }
    if width > TILE_SIZE || height > TILE_SIZE {
        return Err(EncodeError::TileTooLarge { width, height });
    }
    if buf.format != PixelFormat::Bgrx8888 {
        return Err(EncodeError::UnsupportedFormat(buf.format));
    }
    let expected_stride = 4 * width as usize;
    if buf.stride != expected_stride {
        return Err(EncodeError::StrideMismatch {
            stride: buf.stride,
            expected: expected_stride,
        });
    }
    let expected_len = expected_stride * height as usize;
    if buf.data.len() != expected_len {
        return Err(EncodeError::DataLengthMismatch {
            len: buf.data.len(),
            expected: expected_len,
        });
    }
    Ok(expected_len)
}

/// The QOI tile for a checked `buf`, when `prefer` asks for QOI and the stream stays within
/// [`qoi_budget`]; `None` means the tile goes out RAW.
fn qoi_tile(buf: &PixelBuffer, prefer: Encoding, raw_len: usize) -> Option<EncodedTile> {
    match prefer {
        Encoding::Raw => None,
        Encoding::Qoi => qoi::encode_within(
            buf.size.width,
            buf.size.height,
            &buf.data,
            qoi_budget(raw_len),
        )
        .map(|data| EncodedTile {
            codec: Encoding::Qoi.id(),
            data,
        }),
    }
}

/// The longest QOI stream worth sending in place of a RAW payload of `raw_len` bytes. A
/// longer one loses to RAW, and the tile is sent RAW instead; a stream of exactly this
/// length is kept. The cap is redundant for a checked tile, whose RAW payload is at most
/// [`MAX_TILE_BYTES`], and stays as a second guard.
fn qoi_budget(raw_len: usize) -> usize {
    raw_len.min(MAX_TILE_BYTES)
}

/// Decodes one tile's payload back into the pixels it was cut from, whatever codec was
/// used on the wire.
///
/// `codec` is the wire id; `size` must be the tile's true size and at most
/// [`TILE_SIZE`] by [`TILE_SIZE`]. The result is always `Bgrx8888` with no stride
/// padding, and its unused fourth byte is always 255, whatever the codec, the QOI channels
/// byte or the stream's alpha say: every tile is opaque (see "The fourth byte" in the crate
/// documentation). The TypeScript client decodes every payload to the same pixels.
/// The bounds are strict: a RAW payload must be exactly `width * height * 4` bytes, a
/// QOI header must name `size` exactly, and a QOI stream must end with its marker and
/// nothing after it.
pub fn decode_tile(codec: u32, data: &[u8], size: Size) -> Result<PixelBuffer, DecodeError> {
    let Size { width, height } = size;
    if width == 0 || height == 0 {
        return Err(DecodeError::EmptySize);
    }
    if width > TILE_SIZE || height > TILE_SIZE {
        return Err(DecodeError::SizeTooLarge { width, height });
    }
    let stride = 4 * width as usize;
    let expected_len = stride * height as usize;

    match Encoding::from_id(codec) {
        Some(Encoding::Raw) => {
            if data.len() != expected_len {
                return Err(DecodeError::RawLengthMismatch {
                    len: data.len(),
                    expected: expected_len,
                });
            }
            let mut data = data.to_vec();
            for pixel in data.chunks_exact_mut(4) {
                pixel[3] = 0xFF; // the unused byte: every tile is opaque
            }
            Ok(PixelBuffer {
                size,
                stride,
                format: PixelFormat::Bgrx8888,
                data,
            })
        }
        Some(Encoding::Qoi) => {
            let decoded = qoi::decode(data, size)?;
            let mut data = vec![0_u8; expected_len];
            for (source, target) in decoded.rgba.chunks_exact(4).zip(data.chunks_exact_mut(4)) {
                target[0] = source[2]; // blue
                target[1] = source[1]; // green
                target[2] = source[0]; // red
                target[3] = 0xFF; // the stream's alpha is dropped: every tile is opaque
            }
            Ok(PixelBuffer {
                size,
                stride,
                format: PixelFormat::Bgrx8888,
                data,
            })
        }
        None => Err(DecodeError::UnknownCodec(codec)),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DecodeError, EncodeError, Encoding, MAX_TILE_BYTES, TILE_SIZE, cut_into_tiles, decode_tile,
        encode_tile, encode_tile_owned, qoi, qoi_budget,
    };
    use appricot_core::{PixelBuffer, PixelFormat, Rect, Size};

    /// A tiny deterministic PRNG (xorshift32), so every pattern is reproducible.
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

        /// A fresh random byte.
        fn byte(&mut self) -> u8 {
            u8::try_from(self.next() & 0xFF).expect("masked to a byte")
        }

        /// A two-bit value, widened to a byte: 0..=3.
        fn two_bits(&mut self) -> u8 {
            u8::try_from(self.next() & 3).expect("masked to two bits")
        }
    }

    /// One deterministic pattern: a name for failures, and a tile builder.
    type Pattern = fn(u32, u32) -> PixelBuffer;

    /// Builds a `Bgrx8888` buffer of `width` by `height` from a per-pixel function
    /// producing `[blue, green, red, unused]`.
    fn tile_buffer(
        width: u32,
        height: u32,
        mut pixel: impl FnMut(u32, u32) -> [u8; 4],
    ) -> PixelBuffer {
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

    /// One flat colour with a non-trivial unused byte, to prove it survives the trip.
    fn flat(width: u32, height: u32) -> PixelBuffer {
        tile_buffer(width, height, |_, _| [0x2A, 0x39, 0x87, 0xC3])
    }

    /// A horizontal gradient: red rises by one per pixel, DIFF-chunk friendly.
    fn gradient(width: u32, height: u32) -> PixelBuffer {
        tile_buffer(width, height, |x, _| {
            let red = u8::try_from(x).expect("tile widths stay below 256");
            [0x80, 0x40, red, 0xFF]
        })
    }

    /// Flat rows: every row one flat colour, RUN-chunk friendly.
    fn runs(width: u32, height: u32) -> PixelBuffer {
        tile_buffer(width, height, |_, y| {
            let value = u8::try_from(y).expect("tile heights stay below 256");
            [value, value, value, 0xFF]
        })
    }

    /// A random walk whose steps stay inside the DIFF range.
    fn walk(width: u32, height: u32) -> PixelBuffer {
        let mut rng = XorShift32(0x9E37_79B9);
        let (mut r, mut g, mut b) = (0x80_u8, 0x60_u8, 0x40_u8);
        tile_buffer(width, height, |_, _| {
            r = r.wrapping_add(rng.two_bits().wrapping_sub(2));
            g = g.wrapping_add(rng.two_bits().wrapping_sub(2));
            b = b.wrapping_add(rng.two_bits().wrapping_sub(2));
            [b, g, r, 0xEE]
        })
    }

    /// Uniform noise in all four bytes. QOI saves only the odd LUMA step on it, and the
    /// fourth byte, which QOI ignores, is garbage.
    fn noise(width: u32, height: u32) -> PixelBuffer {
        let mut rng = XorShift32(0x00DD_B1A5);
        tile_buffer(width, height, |_, _| {
            [rng.byte(), rng.byte(), rng.byte(), rng.byte()]
        })
    }

    /// Every deterministic pattern at every size the task names.
    fn patterns() -> Vec<(&'static str, Pattern)> {
        vec![
            ("flat", flat),
            ("gradient", gradient),
            ("runs", runs),
            ("walk", walk),
            ("noise", noise),
        ]
    }

    fn sizes() -> [(u32, u32); 3] {
        [(1, 1), (256, 256), (200, 100)]
    }

    #[test]
    fn encoding_ids_round_trip() {
        assert_eq!(Encoding::Raw.id(), 1);
        assert_eq!(Encoding::Qoi.id(), 2);
        assert_eq!(Encoding::from_id(1), Some(Encoding::Raw));
        assert_eq!(Encoding::from_id(2), Some(Encoding::Qoi));
        assert_eq!(Encoding::from_id(0), None);
        assert_eq!(Encoding::from_id(3), None);
        assert!(Encoding::Raw.is_lossless());
        assert!(Encoding::Qoi.is_lossless());
    }

    /// A pattern that no QOI chunk shortens: every pixel is 128 away in green from the one
    /// before (outside DIFF and LUMA; the first is 77 away in red from the starting black),
    /// and no pixel repeats, so every pixel costs a full 4-byte RGB chunk.
    fn incompressible(width: u32, height: u32) -> PixelBuffer {
        tile_buffer(width, height, |x, y| {
            let blue = u8::try_from(x).expect("tile widths stay below 256");
            let red = u8::try_from(y)
                .expect("tile heights stay below 256")
                .wrapping_add(77);
            let green = if (y * width + x).is_multiple_of(2) {
                0
            } else {
                128
            };
            [blue, green, red, 0xFF]
        })
    }

    /// The pixels every decoder gives back for `buf`: the same, with the unused byte 255.
    fn opaque(buf: &PixelBuffer) -> PixelBuffer {
        let mut data = buf.data.clone();
        for pixel in data.chunks_exact_mut(4) {
            pixel[3] = 0xFF;
        }
        PixelBuffer { data, ..*buf }
    }

    #[test]
    fn both_codecs_round_trip_every_pattern_to_the_same_opaque_pixels() {
        for (name, pattern) in patterns() {
            for (width, height) in sizes() {
                let buf = pattern(width, height);
                for prefer in [Encoding::Raw, Encoding::Qoi] {
                    let tile = encode_tile(&buf, prefer)
                        .unwrap_or_else(|e| panic!("{name} {width}x{height}: {e:?}"));
                    let back = decode_tile(tile.codec, &tile.data, buf.size)
                        .unwrap_or_else(|e| panic!("{name} {width}x{height}: {e:?}"));
                    assert_eq!(
                        back,
                        opaque(&buf),
                        "{name} {width}x{height} must survive codec {}",
                        tile.codec
                    );
                }
            }
        }
    }

    #[test]
    fn raw_and_qoi_decode_a_garbage_fourth_byte_to_the_same_opaque_pixels() {
        // Encode-2: a depth-32 window's fourth byte (0 in transparent corners, anything in
        // between) must not make a QOI tile draw differently from a RAW one.
        let buf = tile_buffer(16, 16, |x, y| {
            let fourth = [0x00, 0x7F, 0xC3, 0xFF][usize::try_from((x + y) % 4).expect("small")];
            [0x40, 0x80, u8::try_from(x / 4).expect("small"), fourth]
        });
        let raw = encode_tile(&buf, Encoding::Raw).expect("a valid tile encodes");
        let qoi = encode_tile(&buf, Encoding::Qoi).expect("a valid tile encodes");
        assert_eq!(raw.codec, Encoding::Raw.id());
        assert_eq!(qoi.codec, Encoding::Qoi.id(), "this tile compresses");
        let from_raw = decode_tile(raw.codec, &raw.data, buf.size).expect("RAW decodes");
        let from_qoi = decode_tile(qoi.codec, &qoi.data, buf.size).expect("QOI decodes");
        assert_eq!(from_raw, from_qoi);
        assert_eq!(from_qoi, opaque(&buf));
    }

    #[test]
    fn the_owned_form_gives_the_same_tiles_and_moves_raw_pixels() {
        let mut all = patterns();
        all.push(("incompressible", incompressible));
        for (name, pattern) in all {
            for (width, height) in sizes() {
                let buf = pattern(width, height);
                for prefer in [Encoding::Raw, Encoding::Qoi] {
                    let borrowed = encode_tile(&buf, prefer).expect("a valid tile encodes");
                    let owned = encode_tile_owned(buf.clone(), prefer).expect("a valid tile");
                    assert_eq!(owned, borrowed, "{name} {width}x{height} {prefer:?}");
                    // A RAW tile's payload is the moved buffer itself, not a copy of it.
                    let moved = buf.clone();
                    let moved_pixels = moved.data.as_ptr();
                    let tile = encode_tile_owned(moved, prefer).expect("a valid tile encodes");
                    if tile.codec == Encoding::Raw.id() {
                        assert_eq!(tile.data.as_ptr(), moved_pixels, "{name}: RAW is moved");
                    }
                }
            }
        }
        // A buffer that is not one tile is refused the same way.
        assert_eq!(
            encode_tile_owned(tile_buffer(257, 1, |_, _| [0; 4]), Encoding::Qoi),
            Err(EncodeError::TileTooLarge {
                width: 257,
                height: 1
            })
        );
    }

    #[test]
    fn compressible_tiles_stay_qoi_and_shrink() {
        // These patterns reduce to roughly a byte per pixel, far below the raw four.
        let compressible: [(&str, Pattern); 4] = [
            ("flat", flat),
            ("gradient", gradient),
            ("runs", runs),
            ("walk", walk),
        ];
        for (name, pattern) in compressible {
            let buf = pattern(256, 256);
            let raw_len = buf.data.len();
            let tile = encode_tile(&buf, Encoding::Qoi).expect("a valid tile encodes");
            assert_eq!(tile.codec, Encoding::Qoi.id(), "{name} should stay QOI");
            assert!(
                tile.data.len() < raw_len,
                "{name}: {} bytes must beat {} raw bytes",
                tile.data.len(),
                raw_len
            );
        }
    }

    #[test]
    fn an_incompressible_tile_falls_back_to_raw() {
        for (width, height) in sizes().into_iter().chain([(1, 256), (256, 1), (120, 132)]) {
            let buf = incompressible(width, height);
            let tile = encode_tile(&buf, Encoding::Qoi).expect("a valid tile encodes");
            assert_eq!(
                tile.codec,
                Encoding::Raw.id(),
                "{width}x{height} must come back RAW"
            );
            assert_eq!(tile.data, buf.data, "the RAW fallback is the input itself");
            // And the fallback was not vacuous: the QOI stream is exactly the worst case,
            // 22 bytes of header and end marker over the RAW payload.
            let qoi_len = qoi::encode(width, height, &buf.data).len();
            assert_eq!(qoi_len, buf.data.len() + 22, "{width}x{height}");
        }
        let tile = encode_tile(&incompressible(256, 256), Encoding::Qoi).expect("encodes");
        assert_eq!(tile.data.len(), MAX_TILE_BYTES);
    }

    #[test]
    fn the_qoi_budget_keeps_a_tie_and_never_exceeds_the_tile_cap() {
        assert_eq!(qoi_budget(1000), 1000, "a stream as long as RAW is kept");
        assert_eq!(qoi_budget(MAX_TILE_BYTES), MAX_TILE_BYTES);
        assert_eq!(qoi_budget(MAX_TILE_BYTES + 1), MAX_TILE_BYTES);

        // The tie, end to end: one pixel's QOI stream is 26 bytes (header 14, one RGB
        // chunk, end marker 8), so a 26-byte budget keeps it and 25 does not.
        let pixel = [0x33, 0x22, 0x11, 0xFF];
        assert_eq!(
            qoi::encode_within(1, 1, &pixel, 26).map(|stream| stream.len()),
            Some(26)
        );
        assert_eq!(qoi::encode_within(1, 1, &pixel, 25), None);
    }

    #[test]
    fn qoi_is_deterministic_through_a_raw_round_trip() {
        let buf = walk(256, 256);
        let first = encode_tile(&buf, Encoding::Qoi).expect("a valid tile encodes");
        assert_eq!(first.codec, Encoding::Qoi.id());

        // The same input yields the same stream, twice.
        let second = encode_tile(&buf, Encoding::Qoi).expect("a valid tile encodes");
        assert_eq!(first, second);

        // Decoding to pixels and re-encoding yields the same stream again.
        let back = decode_tile(first.codec, &first.data, buf.size).expect("our payload decodes");
        let third = encode_tile(&back, Encoding::Qoi).expect("a valid tile encodes");
        assert_eq!(first, third);
    }

    #[test]
    fn encoding_rejects_an_empty_tile() {
        let empty = PixelBuffer {
            size: Size::new(0, 4),
            stride: 0,
            format: PixelFormat::Bgrx8888,
            data: Vec::new(),
        };
        assert_eq!(
            encode_tile(&empty, Encoding::Raw),
            Err(EncodeError::EmptyTile)
        );
    }

    #[test]
    fn encoding_rejects_an_oversized_tile() {
        let oversized = tile_buffer(257, 4, |_, _| [0, 0, 0, 0xFF]);
        assert_eq!(
            encode_tile(&oversized, Encoding::Raw),
            Err(EncodeError::TileTooLarge {
                width: 257,
                height: 4
            })
        );
    }

    #[test]
    fn encoding_rejects_a_padded_stride() {
        let buf = flat(4, 4);
        let padded = PixelBuffer {
            stride: 4 * 4 + 8,
            ..buf
        };
        assert_eq!(
            encode_tile(&padded, Encoding::Raw),
            Err(EncodeError::StrideMismatch {
                stride: 24,
                expected: 16
            })
        );
    }

    #[test]
    fn encoding_rejects_wrong_data_lengths() {
        let buf = flat(4, 4);
        let short = PixelBuffer {
            data: buf.data[..60].to_vec(),
            ..buf
        };
        assert_eq!(
            encode_tile(&short, Encoding::Raw),
            Err(EncodeError::DataLengthMismatch {
                len: 60,
                expected: 64
            })
        );
        let mut long_data = buf.data.clone();
        long_data.push(0);
        let long = PixelBuffer {
            data: long_data,
            ..buf
        };
        assert_eq!(
            encode_tile(&long, Encoding::Raw),
            Err(EncodeError::DataLengthMismatch {
                len: 65,
                expected: 64
            })
        );
    }

    #[test]
    fn decoding_is_strict() {
        let size = Size::new(4, 4);
        // Unknown codecs, including 0, which is never a codec.
        assert_eq!(
            decode_tile(0, &[0; 64], size),
            Err(DecodeError::UnknownCodec(0))
        );
        assert_eq!(
            decode_tile(3, &[0; 64], size),
            Err(DecodeError::UnknownCodec(3))
        );

        // Empty and oversized tiles.
        assert_eq!(
            decode_tile(1, &[0; 4], Size::new(4, 0)),
            Err(DecodeError::EmptySize)
        );
        assert_eq!(
            decode_tile(1, &[0; 4], Size::new(257, 1)),
            Err(DecodeError::SizeTooLarge {
                width: 257,
                height: 1
            })
        );

        // RAW payload of the wrong length, short and long.
        assert_eq!(
            decode_tile(1, &[0; 63], size),
            Err(DecodeError::RawLengthMismatch {
                len: 63,
                expected: 64
            })
        );
        assert_eq!(
            decode_tile(1, &[0; 65], size),
            Err(DecodeError::RawLengthMismatch {
                len: 65,
                expected: 64
            })
        );

        // A QOI payload whose header names other dimensions than the caller's size.
        let qoi_stream = qoi::encode(4, 4, &flat(4, 4).data);
        assert_eq!(
            decode_tile(Encoding::Qoi.id(), &qoi_stream, Size::new(8, 2)),
            Err(DecodeError::QoiSizeMismatch {
                header: Size::new(4, 4),
                expected: Size::new(8, 2),
            })
        );
        // The same stream truncates and trails badly.
        assert_eq!(
            decode_tile(
                Encoding::Qoi.id(),
                &qoi_stream[..qoi_stream.len() - 1],
                size
            ),
            Err(DecodeError::QoiTruncated)
        );
        let mut trailing = qoi_stream.clone();
        trailing.push(0xC0);
        assert_eq!(
            decode_tile(Encoding::Qoi.id(), &trailing, size),
            Err(DecodeError::QoiTrailingData(9))
        );
    }

    /// Assembles a QOI stream: header, chunks, end marker.
    fn qoi_stream(width: u32, height: u32, channels: u8, chunks: &[u8]) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(b"qoif");
        stream.extend_from_slice(&width.to_be_bytes());
        stream.extend_from_slice(&height.to_be_bytes());
        stream.extend_from_slice(&[channels, 0]);
        stream.extend_from_slice(chunks);
        stream.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        stream
    }

    #[test]
    fn decoding_restores_a_three_channel_qoi_tile_as_opaque() {
        // A hand-built 3-channel stream: one RGB pixel, then a run of one.
        let stream = qoi_stream(2, 1, 3, &[0xFE, 10, 20, 30, 0xC0]);
        let back = decode_tile(Encoding::Qoi.id(), &stream, Size::new(2, 1))
            .expect("a well-formed 3-channel tile decodes");
        assert_eq!(back.size, Size::new(2, 1));
        assert_eq!(back.stride, 8);
        assert_eq!(back.format, PixelFormat::Bgrx8888);
        assert_eq!(back.data, vec![30, 20, 10, 0xFF, 30, 20, 10, 0xFF]);

        // Encode-5: an INDEX chunk into a slot no pixel has filled reads (0,0,0,0), and
        // the RGB chunk after it carries that alpha 0 forward. Both come out opaque.
        let unfilled = qoi_stream(2, 1, 3, &[0x05, 0xFE, 1, 2, 3]);
        let back = decode_tile(Encoding::Qoi.id(), &unfilled, Size::new(2, 1))
            .expect("an INDEX into an unfilled slot decodes");
        assert_eq!(back.data, vec![0, 0, 0, 0xFF, 3, 2, 1, 0xFF]);

        // Encode-6: an RGBA chunk in a 3-channel stream decodes, as the reference decoder
        // decodes it, and the pixel is opaque all the same.
        let rgba_chunk = qoi_stream(1, 1, 3, &[0xFF, 1, 2, 3, 7]);
        let back = decode_tile(Encoding::Qoi.id(), &rgba_chunk, Size::new(1, 1))
            .expect("an RGBA chunk in a 3-channel stream decodes");
        assert_eq!(back.data, vec![3, 2, 1, 0xFF]);
    }

    #[test]
    fn decoding_makes_a_four_channel_qoi_tile_opaque() {
        // A 4-channel stream whose pixels carry alpha 0 and 128: the wire ignores alpha.
        let stream = qoi_stream(2, 1, 4, &[0xFF, 1, 2, 3, 0, 0xFF, 4, 5, 6, 128]);
        let back = decode_tile(Encoding::Qoi.id(), &stream, Size::new(2, 1))
            .expect("a well-formed 4-channel tile decodes");
        assert_eq!(back.data, vec![3, 2, 1, 0xFF, 6, 5, 4, 0xFF]);
    }

    #[test]
    fn decoding_a_raw_tile_makes_it_opaque() {
        let payload = [1, 2, 3, 0, 4, 5, 6, 0x80];
        let back =
            decode_tile(Encoding::Raw.id(), &payload, Size::new(2, 1)).expect("a RAW tile decodes");
        assert_eq!(back.data, vec![1, 2, 3, 0xFF, 4, 5, 6, 0xFF]);
    }

    /// The shared `cut_into_tiles` property check: valid, aligned, ordered, covering.
    fn check_cut(rect: Rect, bounds: Size) -> Vec<Rect> {
        let tiles = cut_into_tiles(rect, bounds);
        let surface = Rect::new(0, 0, bounds.width, bounds.height);
        let Some(clip) = rect.intersection(surface).filter(|r| !r.is_empty()) else {
            assert!(tiles.is_empty(), "no overlap with the surface, no tiles");
            return tiles;
        };

        let s = i64::from(TILE_SIZE);
        let clip_left = i64::from(clip.origin.x);
        let clip_top = i64::from(clip.origin.y);
        let clip_right = clip_left + i64::from(clip.size.width);
        let clip_bottom = clip_top + i64::from(clip.size.height);

        let mut covered: u64 = 0;
        let mut previous: Option<(i64, i64)> = None;
        for tile in &tiles {
            assert!(!tile.is_empty(), "no empty tiles");
            assert!(
                tile.size.width <= TILE_SIZE && tile.size.height <= TILE_SIZE,
                "tiles never exceed the grid cell"
            );
            assert_eq!(
                tile.intersection(clip),
                Some(*tile),
                "tiles stay in the damage"
            );

            let left = i64::from(tile.origin.x);
            let top = i64::from(tile.origin.y);
            let right = left + i64::from(tile.size.width);
            let bottom = top + i64::from(tile.size.height);
            let column = left.div_euclid(s);
            let row = top.div_euclid(s);
            assert_eq!(
                (right - 1).div_euclid(s),
                column,
                "a tile never crosses a vertical grid line"
            );
            assert_eq!(
                (bottom - 1).div_euclid(s),
                row,
                "a tile never crosses a horizontal grid line"
            );
            // Each edge is either a grid line or the damage's own edge.
            assert!(left == column * s || left == clip_left, "left edge aligned");
            assert!(
                right == (column + 1) * s || right == clip_right,
                "right edge aligned"
            );
            assert!(top == row * s || top == clip_top, "top edge aligned");
            assert!(
                bottom == (row + 1) * s || bottom == clip_bottom,
                "bottom edge aligned"
            );

            if let Some((previous_row, previous_column)) = previous {
                assert!(
                    (row, column) > (previous_row, previous_column),
                    "row-major order"
                );
            }
            previous = Some((row, column));
            covered += u64::from(tile.size.width) * u64::from(tile.size.height);
        }
        assert_eq!(
            covered,
            u64::from(clip.size.width) * u64::from(clip.size.height),
            "tiles cover the damage exactly, with no overlap"
        );
        tiles
    }

    #[test]
    fn cuts_are_valid_for_many_rectangles() {
        let dev = Size::new(1400, 900);
        let max = Size::new(1920, 1200);
        check_cut(Rect::new(0, 0, 1400, 900), dev);
        check_cut(Rect::new(10, 20, 500, 300), dev);
        check_cut(Rect::new(250, 0, 10, 10), dev); // crosses a vertical grid line
        check_cut(Rect::new(255, 255, 2, 2), Size::new(256, 256)); // straddles a corner
        check_cut(Rect::new(-50, -50, 100, 100), max); // clipped at the origin
        check_cut(Rect::new(1900, 1150, 100, 100), max); // clipped at the far edge
        check_cut(Rect::new(1370, 870, 100, 100), dev); // clipped at both far edges
        check_cut(Rect::new(-1000, -1000, 10, 10), dev); // entirely outside
        check_cut(Rect::new(0, 0, 1, 1), Size::new(1, 1)); // a one-pixel surface
    }

    #[test]
    fn non_overlapping_or_empty_rectangles_yield_no_tiles() {
        assert!(cut_into_tiles(Rect::new(500, 0, 10, 10), Size::new(100, 100)).is_empty());
        assert!(cut_into_tiles(Rect::new(0, 0, 0, 10), Size::new(100, 100)).is_empty());
        assert!(cut_into_tiles(Rect::new(0, 0, 10, 10), Size::new(0, 0)).is_empty());
    }

    #[test]
    fn a_full_redraw_of_the_dev_surface_is_24_tiles() {
        let tiles = check_cut(Rect::new(0, 0, 1400, 900), Size::new(1400, 900));
        assert_eq!(tiles.len(), 24, "6 columns by 4 rows");
        assert_eq!(tiles[0], Rect::new(0, 0, 256, 256));
        // 1400 = 5*256 + 120 and 900 = 3*256 + 132, so the last tile is the remainder.
        assert_eq!(
            *tiles.last().expect("24 tiles exist"),
            Rect::new(1280, 768, 120, 132)
        );
    }

    #[test]
    fn the_largest_surface_never_exceeds_the_frame_budget() {
        let max = Size::new(1920, 1200);
        let full = check_cut(Rect::new(0, 0, 1920, 1200), max);
        assert_eq!(full.len(), 40, "8 columns by 5 rows");
        assert!(full.len() <= 48, "inside MAX_TILES_PER_FRAME");
        // A rect that skips the first pixel still touches every grid cell.
        let worst = check_cut(Rect::new(1, 1, 1918, 1198), max);
        assert_eq!(worst.len(), 40);
        assert!(worst.len() <= 48, "inside MAX_TILES_PER_FRAME");
    }

    #[test]
    fn consecutive_damage_rectangles_share_tile_edges() {
        let dev = Size::new(1400, 900);
        // A 300-wide rect starting at 10 splits exactly at the grid line 256; a 300-tall
        // rect starting at 20 splits exactly at 256 as well, whatever their origins.
        let wide = cut_into_tiles(Rect::new(10, 10, 300, 10), dev);
        assert_eq!(
            wide,
            vec![Rect::new(10, 10, 246, 10), Rect::new(256, 10, 54, 10)]
        );
        let tall = cut_into_tiles(Rect::new(20, 0, 50, 300), dev);
        assert_eq!(
            tall,
            vec![Rect::new(20, 0, 50, 256), Rect::new(20, 256, 50, 44)]
        );
    }
}
