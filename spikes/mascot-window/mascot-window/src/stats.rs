//! Frame cost bookkeeping for the 1080p / 30 fps measurement.

use std::time::Duration;

/// CPU time spent inside one `render` call (acquire, encode, submit, present), collected over a window of frames.
#[derive(Debug, Default)]
pub struct FrameStats {
    samples: Vec<Duration>,
}

/// A summary of a window of frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    /// Frames in the window.
    pub frames: usize,
    /// Mean time in `render`, milliseconds.
    pub mean_ms: f64,
    /// 95th percentile, milliseconds.
    pub p95_ms: f64,
    /// Slowest frame, milliseconds.
    pub max_ms: f64,
}

impl FrameStats {
    /// Records one frame's time in `render`.
    pub fn record(&mut self, elapsed: Duration) {
        self.samples.push(elapsed);
    }

    /// Summarises and clears the window; `None` if no frame was recorded.
    pub fn take_summary(&mut self) -> Option<Summary> {
        if self.samples.is_empty() {
            return None;
        }
        let mut ms: Vec<f64> = self.samples.drain(..).map(|d| d.as_secs_f64() * 1000.0).collect();
        ms.sort_by(f64::total_cmp);
        let frames = ms.len();
        // Nearest-rank percentile: the smallest value with at least 95% of samples at or below it.
        let rank = (frames * 95).div_ceil(100).max(1);
        Some(Summary {
            frames,
            mean_ms: ms.iter().sum::<f64>() / frames as f64,
            p95_ms: ms[rank - 1],
            max_ms: ms[frames - 1],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_window_has_no_summary() {
        assert_eq!(FrameStats::default().take_summary(), None);
    }

    #[test]
    fn summary_uses_nearest_rank() {
        let mut stats = FrameStats::default();
        for ms in 1..=100 {
            stats.record(Duration::from_millis(ms));
        }
        let s = stats.take_summary().unwrap();
        assert_eq!(s.frames, 100);
        assert!((s.mean_ms - 50.5).abs() < 1e-9);
        assert!((s.p95_ms - 95.0).abs() < 1e-9);
        assert!((s.max_ms - 100.0).abs() < 1e-9);
        assert_eq!(stats.take_summary(), None, "taking the summary clears the window");
    }
}
