//! The codec bench: re-encodes every recorded tile and reports time and bytes per scenario.
//!
//! The input is `tiles.bin` ([`crate::tiles`]): the pixels the application actually drew,
//! decoded from the wire. Each tile is encoded with each codec [`REPS`] times; the fastest run
//! counts, since the slower ones measure the machine's other work, not the codec. Bytes are
//! what the wire would carry: QOI falls back to RAW whenever QOI would be longer, as the
//! streamer does, and the fallbacks are counted.
//!
//! Only the codecs this repository implements are measured. Another candidate (WebP lossless,
//! for one) needs its encoder in the graph first, which is an ADR-0002 question.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use appricot_core::{PixelBuffer, PixelFormat, Size};
use appricot_encode::{Encoding, encode_tile};

use crate::stats::{Summary, kib, u64_to_f64};
use crate::tiles::TileRecord;

/// Encodes per tile per codec; the fastest counts.
pub const REPS: u32 = 5;

/// What the bench measured for one scenario label.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchRow {
    /// The scenario label, or `all`.
    pub label: String,
    /// How many tiles.
    pub tiles: usize,
    /// How many pixels they cover.
    pub pixels: u64,
    /// RAW bytes: four per pixel.
    pub raw_bytes: u64,
    /// Bytes with QOI preferred, RAW where QOI would be longer.
    pub qoi_bytes: u64,
    /// Tiles that went out RAW although QOI was preferred.
    pub qoi_fallbacks: usize,
    /// QOI encode time per tile, microseconds.
    pub qoi_encode_us: Option<Summary>,
    /// QOI encode time over all tiles, milliseconds.
    pub qoi_encode_total_ms: f64,
}

/// What stops the bench.
#[derive(Debug)]
pub enum BenchError {
    /// A recorded tile does not encode: it is not a valid tile.
    Encode(appricot_encode::EncodeError),
}

impl std::fmt::Display for BenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(e) => write!(f, "a recorded tile does not encode: {e:?}"),
        }
    }
}

impl std::error::Error for BenchError {}

#[derive(Default)]
struct Acc {
    tiles: usize,
    pixels: u64,
    raw_bytes: u64,
    qoi_bytes: u64,
    fallbacks: usize,
    qoi_us: Vec<f64>,
}

impl Acc {
    fn add(&mut self, pixels: u64, qoi_len: usize, fell_back: bool, qoi_time: Duration) {
        self.tiles += 1;
        self.pixels += pixels;
        self.raw_bytes += pixels * 4;
        self.qoi_bytes += u64::try_from(qoi_len).unwrap_or(u64::MAX);
        self.fallbacks += usize::from(fell_back);
        self.qoi_us.push(qoi_time.as_secs_f64() * 1e6);
    }

    fn row(self, label: String) -> BenchRow {
        let total_ms = self.qoi_us.iter().sum::<f64>() / 1000.0;
        BenchRow {
            label,
            tiles: self.tiles,
            pixels: self.pixels,
            raw_bytes: self.raw_bytes,
            qoi_bytes: self.qoi_bytes,
            qoi_fallbacks: self.fallbacks,
            qoi_encode_us: Summary::of(&self.qoi_us),
            qoi_encode_total_ms: total_ms,
        }
    }
}

/// Benches `tiles`: one row per label in first-seen order, then `all`.
pub fn run(tiles: &[TileRecord], reps: u32) -> Result<Vec<BenchRow>, BenchError> {
    let reps = reps.max(1);
    let mut order: Vec<String> = Vec::new();
    let mut by_label: BTreeMap<String, Acc> = BTreeMap::new();
    let mut all = Acc::default();
    for tile in tiles {
        let buf = PixelBuffer {
            size: Size::new(tile.width, tile.height),
            stride: usize::try_from(tile.width).unwrap_or(0) * 4,
            format: PixelFormat::Bgrx8888,
            data: tile.pixels.clone(),
        };
        let mut best = Duration::MAX;
        let mut encoded = None;
        for _ in 0..reps {
            let start = Instant::now();
            let out = encode_tile(&buf, Encoding::Qoi).map_err(BenchError::Encode)?;
            best = best.min(start.elapsed());
            encoded = Some(out);
        }
        let Some(encoded) = encoded else { continue };
        let pixels = u64::from(tile.width) * u64::from(tile.height);
        let fell_back = encoded.codec != Encoding::Qoi.id();
        if !by_label.contains_key(&tile.label) {
            order.push(tile.label.clone());
        }
        by_label.entry(tile.label.clone()).or_default().add(
            pixels,
            encoded.data.len(),
            fell_back,
            best,
        );
        all.add(pixels, encoded.data.len(), fell_back, best);
    }
    let mut rows: Vec<BenchRow> = order
        .into_iter()
        .filter_map(|label| by_label.remove(&label).map(|acc| acc.row(label)))
        .collect();
    rows.push(all.row("all".to_owned()));
    Ok(rows)
}

/// The rows as a Markdown table.
pub fn markdown(rows: &[BenchRow]) -> String {
    let mut out = String::from(
        "| Scenario | Tiles | Mpx | RAW KiB | QOI KiB | QOI/RAW | RAW fallbacks | QOI µs/tile p50 | p95 | QOI ms total |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for row in rows {
        let ratio = if row.raw_bytes == 0 {
            0.0
        } else {
            u64_to_f64(row.qoi_bytes) / u64_to_f64(row.raw_bytes)
        };
        let (p50, p95) = row.qoi_encode_us.map_or((0.0, 0.0), |s| (s.p50, s.p95));
        let _ = writeln!(
            out,
            "| {} | {} | {:.2} | {:.1} | {:.1} | {:.3} | {} | {:.0} | {:.0} | {:.1} |",
            row.label,
            row.tiles,
            u64_to_f64(row.pixels) / 1e6,
            kib(row.raw_bytes),
            kib(row.qoi_bytes),
            ratio,
            row.qoi_fallbacks,
            p50,
            p95,
            row.qoi_encode_total_ms,
        );
    }
    let _ = writeln!(
        out,
        "\nEach tile encoded {REPS} times; the fastest run counts. {} tiles in all.",
        rows.last().map_or(0, |r| r.tiles)
    );
    out
}

#[cfg(test)]
mod tests {
    use super::{markdown, run};
    use crate::tiles::TileRecord;

    fn solid(label: &str, width: u32, height: u32) -> TileRecord {
        let len = usize::try_from(width * height * 4).expect("small");
        TileRecord {
            label: label.to_owned(),
            surface: 1,
            sequence: 1,
            x: 0,
            y: 0,
            width,
            height,
            pixels: [10_u8, 20, 30, 255].repeat(len / 4),
        }
    }

    fn noise(label: &str, width: u32, height: u32) -> TileRecord {
        let mut t = solid(label, width, height);
        let mut state = 0x1234_5678_u32;
        for b in &mut t.pixels {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *b = state.to_le_bytes()[0];
        }
        t
    }

    #[test]
    fn rows_follow_the_labels_and_end_with_all() {
        let tiles = [
            solid("login", 64, 64),
            noise("scroll", 32, 32),
            solid("login", 16, 16),
        ];
        let rows = run(&tiles, 1).expect("valid tiles");
        let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["login", "scroll", "all"]);
        assert_eq!(rows[0].tiles, 2);
        assert_eq!(rows[0].pixels, 64 * 64 + 16 * 16);
        assert_eq!(rows[0].raw_bytes, rows[0].pixels * 4);
        assert!(
            rows[0].qoi_bytes < rows[0].raw_bytes / 10,
            "a solid tile compresses"
        );
        assert_eq!(rows[0].qoi_fallbacks, 0);
        // Noise does not compress, so the tile goes out RAW, as the streamer would send it.
        assert_eq!(rows[1].qoi_fallbacks, 1);
        assert_eq!(rows[1].qoi_bytes, rows[1].raw_bytes);
        assert_eq!(rows[2].tiles, 3);
        let table = markdown(&rows);
        assert!(table.contains("| login | 2 |"));
        assert!(table.contains("| all | 3 |"));
    }

    #[test]
    fn no_tiles_is_one_empty_row() {
        let rows = run(&[], 3).expect("nothing to encode");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tiles, 0);
        assert!(rows[0].qoi_encode_us.is_none());
    }
}
