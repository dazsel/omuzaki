//! Small summary statistics for latency samples.

use serde::{Deserialize, Serialize};

/// Count, spread and tail of a set of samples (milliseconds in this spike).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Number of samples.
    pub count: usize,
    /// Smallest sample.
    pub min: f64,
    /// Arithmetic mean.
    pub mean: f64,
    /// Median.
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// 99th percentile.
    pub p99: f64,
    /// Largest sample.
    pub max: f64,
}

impl Summary {
    /// Summarises `samples`; all zeros for an empty set. NaNs sort last and so only ever show up in `max`.
    #[must_use]
    pub fn of(samples: &[f64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| {
            // Nearest-rank percentile: small sample counts (hundreds) do not justify interpolation.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
            let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
            sorted[rank - 1]
        };
        #[allow(clippy::cast_precision_loss)]
        let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
        Self {
            count: sorted.len(),
            min: sorted[0],
            mean,
            p50: at(0.50),
            p95: at(0.95),
            p99: at(0.99),
            max: sorted[sorted.len() - 1],
        }
    }
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n={} min {:.2} mean {:.2} p50 {:.2} p95 {:.2} p99 {:.2} max {:.2} ms",
            self.count, self.min, self.mean, self.p50, self.p95, self.p99, self.max
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zeros() {
        assert_eq!(Summary::of(&[]), Summary::default());
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = Summary::of(&samples);
        assert_eq!((s.min, s.p50, s.p95, s.p99, s.max), (1.0, 50.0, 95.0, 99.0, 100.0));
        assert!((s.mean - 50.5).abs() < 1e-9);
    }

    #[test]
    fn one_sample_fills_every_field() {
        let s = Summary::of(&[2.5]);
        assert_eq!((s.min, s.mean, s.p50, s.p99, s.max), (2.5, 2.5, 2.5, 2.5, 2.5));
    }
}
