//! Summary statistics over one metric's samples.

use serde::{Deserialize, Serialize};

/// The statistics the spec's budget table names: mean, p50, p95, p99 and maximum.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Arithmetic mean.
    pub mean: f64,
    /// Median (nearest rank).
    pub p50: f64,
    /// 95th percentile (nearest rank), the budget percentile for every per-frame row.
    pub p95: f64,
    /// 99th percentile (nearest rank).
    pub p99: f64,
    /// Largest sample.
    pub max: f64,
}

impl Summary {
    /// Summarise `samples`, sorting them in place. `None` when there are none: a metric with no samples has no
    /// statistics, and printing zeros would pass every budget.
    #[allow(clippy::cast_precision_loss)] // a sample count never nears 2^52
    pub fn of(samples: &mut [f64]) -> Option<Self> {
        if samples.is_empty() || samples.iter().any(|s| !s.is_finite()) {
            return None;
        }
        samples.sort_by(f64::total_cmp);
        let sum: f64 = samples.iter().sum();
        Some(Self {
            mean: sum / samples.len() as f64,
            p50: nearest_rank(samples, 50),
            p95: nearest_rank(samples, 95),
            p99: nearest_rank(samples, 99),
            max: samples[samples.len() - 1],
        })
    }
}

/// Nearest-rank percentile of an ascending, non-empty slice: the smallest sample with at least `pct` % at or below it.
fn nearest_rank(sorted: &[f64], pct: usize) -> f64 {
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let mut v: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        let s = Summary::of(&mut v).unwrap();
        assert_eq!((s.p50, s.p95, s.p99, s.max), (50.0, 95.0, 99.0, 100.0));
        assert!((s.mean - 50.5).abs() < 1e-12);
    }

    #[test]
    fn empty_or_non_finite_input_has_no_summary() {
        assert!(Summary::of(&mut []).is_none());
        assert!(Summary::of(&mut [1.0, f64::NAN]).is_none());
    }

    #[test]
    fn one_sample_is_every_statistic() {
        let s = Summary::of(&mut [7.0]).unwrap();
        assert_eq!((s.mean, s.p50, s.p95, s.p99, s.max), (7.0, 7.0, 7.0, 7.0, 7.0));
    }
}
