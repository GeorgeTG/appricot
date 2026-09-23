//! `tiles.bin`: the decoded pixels of every tile the recorder received, for the codec bench.
//!
//! The streamer cuts damage into tiles of at most 256x256 and sends each in the codec the
//! client offered. The recorder decodes every tile back to BGRX (every v0 codec is lossless, so
//! these are the captured pixels) and appends it here with the scenario label in force. The
//! bench then re-encodes exactly what the application drew, with every candidate codec.
//!
//! The file is a magic, then records, all little-endian:
//!
//! | Field | Type |
//! |---|---|
//! | label length, label (UTF-8) | u16, bytes |
//! | surface, sequence | u32, u32 |
//! | x, y | i32, i32 |
//! | width, height | u32, u32 |
//! | pixel byte count, pixels (BGRX) | u32, bytes |
//!
//! The reader checks every length against the tile limits before it allocates.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

/// The first eight bytes of a tiles file.
pub const MAGIC: &[u8; 8] = b"APTILES1";

/// The largest tile side the wire allows.
const TILE_MAX: u32 = appricot_encode::TILE_SIZE;

/// One received tile, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileRecord {
    /// The scenario label in force when it arrived.
    pub label: String,
    /// The surface it belongs to.
    pub surface: u32,
    /// The sequence number of its frame.
    pub sequence: u32,
    /// Its left edge in surface coordinates.
    pub x: i32,
    /// Its top edge in surface coordinates.
    pub y: i32,
    /// Its width, 1..=256.
    pub width: u32,
    /// Its height, 1..=256.
    pub height: u32,
    /// `width * height * 4` bytes of BGRX.
    pub pixels: Vec<u8>,
}

/// Appends records to a tiles file.
#[derive(Debug)]
pub struct TileWriter {
    out: BufWriter<File>,
}

impl TileWriter {
    /// Creates (or truncates) `path` and writes the magic.
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        out.write_all(MAGIC)?;
        Ok(Self { out })
    }

    /// Appends one record.
    pub fn write(&mut self, tile: &TileRecord) -> io::Result<()> {
        let label = tile.label.as_bytes();
        let label_len = u16::try_from(label.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "label too long"))?;
        let pixel_len = u32::try_from(tile.pixels.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "tile too large"))?;
        self.out.write_all(&label_len.to_le_bytes())?;
        self.out.write_all(label)?;
        for v in [tile.surface, tile.sequence] {
            self.out.write_all(&v.to_le_bytes())?;
        }
        for v in [tile.x, tile.y] {
            self.out.write_all(&v.to_le_bytes())?;
        }
        for v in [tile.width, tile.height, pixel_len] {
            self.out.write_all(&v.to_le_bytes())?;
        }
        self.out.write_all(&tile.pixels)
    }

    /// Flushes what is buffered.
    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Reads every record of the tiles file at `path`.
pub fn read_all(path: &Path) -> io::Result<Vec<TileRecord>> {
    let mut input = BufReader::new(File::open(path)?);
    let mut magic = [0_u8; 8];
    input.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(bad("not a tiles file"));
    }
    let mut tiles = Vec::new();
    while let Some(tile) = read_one(&mut input)? {
        tiles.push(tile);
    }
    Ok(tiles)
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn read_one(input: &mut impl Read) -> io::Result<Option<TileRecord>> {
    let mut len2 = [0_u8; 2];
    match input.read_exact(&mut len2) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let label_len = usize::from(u16::from_le_bytes(len2));
    if label_len > crate::control::LABEL_MAX {
        return Err(bad("label longer than any label the recorder writes"));
    }
    let mut label = vec![0_u8; label_len];
    input.read_exact(&mut label)?;
    let label = String::from_utf8(label).map_err(|_| bad("label is not UTF-8"))?;
    let surface = read_u32(input)?;
    let sequence = read_u32(input)?;
    let x = i32::from_le_bytes(read_4(input)?);
    let y = i32::from_le_bytes(read_4(input)?);
    let width = read_u32(input)?;
    let height = read_u32(input)?;
    let pixel_len = read_u32(input)?;
    if width == 0 || height == 0 || width > TILE_MAX || height > TILE_MAX {
        return Err(bad("tile size out of range"));
    }
    // Both sides are at most 256, so the product fits comfortably.
    if u64::from(pixel_len) != u64::from(width) * u64::from(height) * 4 {
        return Err(bad("pixel count does not match the tile size"));
    }
    let pixel_len = usize::try_from(pixel_len).map_err(|_| bad("tile too large"))?;
    let mut pixels = vec![0_u8; pixel_len];
    input.read_exact(&mut pixels)?;
    Ok(Some(TileRecord {
        label,
        surface,
        sequence,
        x,
        y,
        width,
        height,
        pixels,
    }))
}

fn read_4(input: &mut impl Read) -> io::Result<[u8; 4]> {
    let mut b = [0_u8; 4];
    input.read_exact(&mut b)?;
    Ok(b)
}

fn read_u32(input: &mut impl Read) -> io::Result<u32> {
    Ok(u32::from_le_bytes(read_4(input)?))
}

#[cfg(test)]
mod tests {
    use super::{TileRecord, TileWriter, read_all};

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "appricot-spike-tiles-{name}-{}.bin",
            std::process::id()
        ))
    }

    fn tile(label: &str, width: u32, height: u32) -> TileRecord {
        let len = usize::try_from(width * height * 4).expect("small");
        TileRecord {
            label: label.to_owned(),
            surface: 3,
            sequence: 7,
            x: -4,
            y: 256,
            width,
            height,
            pixels: (0..len)
                .map(|i| u8::try_from(i % 256).expect("a byte"))
                .collect(),
        }
    }

    #[test]
    fn records_round_trip() {
        let path = scratch("round-trip");
        let tiles = [tile("login", 2, 3), tile("scroll", 256, 256)];
        let mut writer = TileWriter::create(&path).expect("the file is created");
        for t in &tiles {
            writer.write(t).expect("a record is written");
        }
        writer.flush().expect("flushed");
        drop(writer);
        assert_eq!(read_all(&path).expect("the file reads back"), tiles);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_truncated_record_is_an_error() {
        let path = scratch("truncated");
        let mut writer = TileWriter::create(&path).expect("the file is created");
        writer.write(&tile("x", 4, 4)).expect("a record is written");
        writer.flush().expect("flushed");
        drop(writer);
        let bytes = std::fs::read(&path).expect("read");
        std::fs::write(&path, &bytes[..bytes.len() - 1]).expect("truncate");
        assert!(read_all(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_length_that_disagrees_with_the_size_is_refused_before_allocating() {
        let path = scratch("lying");
        let mut bytes = super::MAGIC.to_vec();
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        for v in [1_u32, 1, 0, 0, 2, 2] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, &bytes).expect("write");
        let err = read_all(&path).expect_err("the record lies about its length");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let _ = std::fs::remove_file(&path);
    }
}
