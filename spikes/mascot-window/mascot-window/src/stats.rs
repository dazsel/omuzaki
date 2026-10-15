//! Frame cost bookkeeping for the 1080p / 30 fps measurement.

use std::time::Duration;

/// Milliseconds per frame, with a moving mark so the same samples give both a rolling window and an overall summary.
#[derive(Debug, Default)]
pub struct Series {
    samples_ms: Vec<f64>,
    mark: usize,
}

/// A summary of some frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    /// Frames summarised.
    pub frames: usize,
    /// Mean, milliseconds.
    pub mean_ms: f64,
    /// 95th percentile, milliseconds.
    pub p95_ms: f64,
    /// Slowest frame, milliseconds.
    pub max_ms: f64,
}

impl Summary {
    /// Summarises `samples_ms`; `None` when there are none.
    pub fn of(samples_ms: &[f64]) -> Option<Self> {
        if samples_ms.is_empty() {
            return None;
        }
        let mut ms = samples_ms.to_vec();
        ms.sort_by(f64::total_cmp);
        let frames = ms.len();
        // Nearest-rank percentile: the smallest value with at least 95% of samples at or below it.
        let rank = (frames * 95).div_ceil(100).max(1);
        Some(Self {
            frames,
            mean_ms: ms.iter().sum::<f64>() / frames as f64,
            p95_ms: ms[rank - 1],
            max_ms: ms[frames - 1],
        })
    }
}

impl Series {
    /// An empty series with room for `frames` samples, so recording them allocates nothing.
    pub fn with_capacity(frames: usize) -> Self {
        Self { samples_ms: Vec::with_capacity(frames), mark: 0 }
    }

    /// Records one frame's time.
    pub fn record(&mut self, elapsed: Duration) {
        self.samples_ms.push(elapsed.as_secs_f64() * 1000.0);
    }

    /// Records frames already in milliseconds.
    pub fn extend_ms(&mut self, samples_ms: impl IntoIterator<Item = f64>) {
        self.samples_ms.extend(samples_ms);
    }

    /// Summarises the frames recorded since the last call; `None` if there were none.
    pub fn take_window(&mut self) -> Option<Summary> {
        let summary = Summary::of(&self.samples_ms[self.mark..]);
        self.mark = self.samples_ms.len();
        summary
    }

    /// Summarises every frame recorded.
    pub fn overall(&self) -> Option<Summary> {
        Summary::of(&self.samples_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_series_has_no_summary() {
        assert_eq!(Series::default().take_window(), None);
        assert_eq!(Series::default().overall(), None);
    }

    #[test]
    fn summary_uses_nearest_rank() {
        let mut series = Series::default();
        for ms in 1..=100 {
            series.record(Duration::from_millis(ms));
        }
        let s = series.take_window().unwrap();
        assert_eq!(s.frames, 100);
        assert!((s.mean_ms - 50.5).abs() < 1e-9);
        assert!((s.p95_ms - 95.0).abs() < 1e-9);
        assert!((s.max_ms - 100.0).abs() < 1e-9);
        assert_eq!(series.take_window(), None, "the window starts again after a take");
        assert_eq!(series.overall().unwrap().frames, 100, "the overall summary keeps everything");
    }
}
