//! Small statistics over samples, and the number conversions the reports need.

/// Count, sum, and order statistics of a set of samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    /// How many samples.
    pub count: usize,
    /// Their sum.
    pub sum: f64,
    /// The smallest.
    pub min: f64,
    /// The median (nearest rank).
    pub p50: f64,
    /// The 95th percentile (nearest rank).
    pub p95: f64,
    /// The largest.
    pub max: f64,
}

impl Summary {
    /// Summarises `samples`; `None` when there are none. NaNs are dropped first.
    pub fn of(samples: &[f64]) -> Option<Self> {
        let mut sorted: Vec<f64> = samples.iter().copied().filter(|v| !v.is_nan()).collect();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(f64::total_cmp);
        Some(Self {
            count: sorted.len(),
            sum: sorted.iter().sum(),
            min: sorted[0],
            p50: nearest_rank(&sorted, 50),
            p95: nearest_rank(&sorted, 95),
            max: sorted[sorted.len() - 1],
        })
    }

    /// The arithmetic mean.
    pub fn mean(&self) -> f64 {
        self.sum / to_f64(self.count)
    }
}

/// The nearest-rank percentile `p` (1..=100) of a non-empty, sorted slice.
fn nearest_rank(sorted: &[f64], p: usize) -> f64 {
    // rank = ceil(p/100 * n), 1-based; clamp into the slice.
    let rank = (p * sorted.len()).div_ceil(100).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// A count as a float, for rates and means. Counts here stay far below 2^52, where the
/// conversion is exact.
#[expect(
    clippy::cast_precision_loss,
    reason = "report arithmetic on counts far below 2^52"
)]
pub fn to_f64(value: usize) -> f64 {
    value as f64
}

/// A byte count as a float.
#[expect(
    clippy::cast_precision_loss,
    reason = "report arithmetic on byte counts far below 2^52"
)]
pub fn u64_to_f64(value: u64) -> f64 {
    value as f64
}

/// Bytes as KiB, for tables.
pub fn kib(bytes: u64) -> f64 {
    u64_to_f64(bytes) / 1024.0
}

/// Bytes as MiB, for tables.
pub fn mib(bytes: u64) -> f64 {
    u64_to_f64(bytes) / (1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::Summary;

    #[test]
    fn the_summary_of_nothing_is_none() {
        assert_eq!(Summary::of(&[]), None);
        assert_eq!(Summary::of(&[f64::NAN]), None);
    }

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let samples: Vec<f64> = (1..=20).map(f64::from).collect();
        let s = Summary::of(&samples).expect("samples");
        assert_eq!(s.count, 20);
        assert!((s.sum - 210.0).abs() < f64::EPSILON);
        assert!((s.min - 1.0).abs() < f64::EPSILON);
        assert!((s.p50 - 10.0).abs() < f64::EPSILON);
        assert!((s.p95 - 19.0).abs() < f64::EPSILON);
        assert!((s.max - 20.0).abs() < f64::EPSILON);
        assert!((s.mean() - 10.5).abs() < f64::EPSILON);
    }

    #[test]
    fn one_sample_is_every_statistic() {
        let s = Summary::of(&[7.0]).expect("a sample");
        assert!((s.p50 - 7.0).abs() < f64::EPSILON);
        assert!((s.p95 - 7.0).abs() < f64::EPSILON);
    }
}
