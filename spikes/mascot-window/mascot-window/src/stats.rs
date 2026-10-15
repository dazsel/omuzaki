//! Frame cost bookkeeping for the 1080p / 30 fps measurement.

use std::time::Duration;

/// Milliseconds per frame in a ring: once `bound` samples are held, each new one overwrites the oldest, so recording
/// never allocates after construction, however long a run lasts. Summaries cover the samples still held.
#[derive(Debug)]
pub struct Series {
    samples_ms: Vec<f64>,
    bound: usize,
    /// Samples recorded in all, including those the ring has since overwritten.
    written: usize,
    /// `written` at the last [`Series::take_window`].
    marked: usize,
}

impl Default for Series {
    /// A series that grows as needed; use [`Series::with_capacity`] where recording must not allocate.
    fn default() -> Self {
        Self { samples_ms: Vec::new(), bound: usize::MAX, written: 0, marked: 0 }
    }
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
    /// A ring of `frames` samples (at least one), allocated now.
    pub fn with_capacity(frames: usize) -> Self {
        let bound = frames.max(1);
        Self { samples_ms: Vec::with_capacity(bound), bound, written: 0, marked: 0 }
    }

    /// Records one frame's time.
    pub fn record(&mut self, elapsed: Duration) {
        self.push(elapsed.as_secs_f64() * 1000.0);
    }

    /// Records frames already in milliseconds.
    pub fn extend_ms(&mut self, samples_ms: impl IntoIterator<Item = f64>) {
        for ms in samples_ms {
            self.push(ms);
        }
    }

    fn push(&mut self, ms: f64) {
        if self.samples_ms.len() < self.bound {
            self.samples_ms.push(ms);
        } else {
            self.samples_ms[self.written % self.bound] = ms;
        }
        self.written += 1;
    }

    /// The last `n` samples recorded, oldest first, at most as many as the ring still holds. Allocates the copy, so it is
    /// for the once-per-report summaries, not the frame loop.
    fn recent(&self, n: usize) -> Vec<f64> {
        let n = n.min(self.samples_ms.len());
        (self.written - n..self.written).map(|i| self.samples_ms[i % self.samples_ms.len().max(1)]).collect()
    }

    /// Summarises the frames recorded since the last call; `None` if there were none.
    pub fn take_window(&mut self) -> Option<Summary> {
        let summary = Summary::of(&self.recent(self.written - self.marked));
        self.marked = self.written;
        summary
    }

    /// Summarises every frame still held.
    pub fn overall(&self) -> Option<Summary> {
        Summary::of(&self.recent(self.samples_ms.len()))
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

    #[test]
    fn a_full_ring_overwrites_the_oldest_without_growing() {
        let mut series = Series::with_capacity(4);
        let capacity = series.samples_ms.capacity();
        series.extend_ms((1..=10).map(f64::from));
        assert_eq!(series.samples_ms.capacity(), capacity, "recording past the bound allocates nothing");
        let all = series.overall().unwrap();
        assert_eq!((all.frames, all.max_ms), (4, 10.0), "the last four of ten remain");
        assert!((all.mean_ms - 8.5).abs() < 1e-9);
        let window = series.take_window().unwrap();
        assert_eq!(window.frames, 4, "a window longer than the ring is the ring");
        series.extend_ms([20.0, 30.0]);
        let window = series.take_window().unwrap();
        assert_eq!((window.frames, window.max_ms), (2, 30.0), "the window is what came after the last take");
    }
}
