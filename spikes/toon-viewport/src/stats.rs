//! Frame-time statistics for the readout and the benchmark.

use std::fmt;

/// A summary of a set of durations in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub mean: f32,
    pub p50: f32,
    pub p95: f32,
    pub max: f32,
}

impl Summary {
    /// Summarises `samples`; `None` when there are none.
    #[must_use]
    pub fn of(samples: &[f32]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f32::total_cmp);
        let at = |q: f32| {
            // Nearest-rank percentile: the index is within 0..len, so the float round trip is exact for any real run.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
            let i = ((sorted.len() - 1) as f32 * q).round() as usize;
            sorted[i]
        };
        #[allow(clippy::cast_precision_loss)]
        let mean = sorted.iter().sum::<f32>() / sorted.len() as f32;
        Some(Self { count: sorted.len(), mean, p50: at(0.5), p95: at(0.95), max: sorted[sorted.len() - 1] })
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mean {:.3} ms, p50 {:.3}, p95 {:.3}, max {:.3} ({} frames)",
            self.mean, self.p50, self.p95, self.max, self.count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_of_nothing_is_none() {
        assert_eq!(Summary::of(&[]), None);
    }

    #[test]
    fn summary_orders_and_averages() {
        let s = Summary::of(&[4.0, 1.0, 3.0, 2.0, 10.0]).unwrap();
        assert_eq!(s.count, 5);
        assert!((s.mean - 4.0).abs() < 1e-6);
        assert!((s.p50 - 3.0).abs() < 1e-6);
        assert!((s.max - 10.0).abs() < 1e-6);
    }
}
