//! [`LatencyMeter`]: the camera-to-output latency the health strip shows.
//!
//! The render side records one sample per picture it draws from a tracked frame: how long the tracker took from its
//! arrival floor to publishing the frame ([`Tracker::latency`](omuzaki_track_api::tracker::Tracker::latency)'s median),
//! plus how long the frame's data then waited for this picture (perform thread, mailbox, render). The panel reads the
//! median of the newest samples. Lock-free: the writer is the render thread and the reader is any thread.
//!
//! What it leaves out is the constant best-case transport delay of the camera itself (`LatencyStats` documents it),
//! so it reads a little under the true glass-to-glass figure; the latency harness (#1967) measures that one.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Samples the median is taken over: about a second of frames at 60 Hz.
const WINDOW: usize = 64;

/// A ring of the newest latency samples, in microseconds.
#[derive(Debug)]
pub struct LatencyMeter {
    samples: [AtomicU32; WINDOW],
    /// Samples ever recorded; the next one goes to `count % WINDOW`.
    count: AtomicU64,
}

impl LatencyMeter {
    /// A meter with no samples.
    pub const fn new() -> Self {
        Self { samples: [const { AtomicU32::new(0) }; WINDOW], count: AtomicU64::new(0) }
    }

    /// The meter the registered Live panel reads and the app's render side writes: a panel built by the registry
    /// receives only its runner, so the two meet here.
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<LatencyMeter>> = OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| Arc::new(Self::new())))
    }

    /// Records one sample (single writer). Saturates at about 71 minutes.
    pub fn record(&self, latency: Duration) {
        let micros = u32::try_from(latency.as_micros()).unwrap_or(u32::MAX);
        let n = self.count.load(Ordering::Relaxed);
        #[allow(clippy::cast_possible_truncation)] // the remainder is below WINDOW
        self.samples[(n % WINDOW as u64) as usize].store(micros, Ordering::Relaxed);
        self.count.store(n + 1, Ordering::Release);
    }

    /// The median of the newest samples; `None` before the first. Forgets everything with [`reset`](Self::reset).
    pub fn reading(&self) -> Option<Duration> {
        let count = self.count.load(Ordering::Acquire);
        if count == 0 {
            return None;
        }
        let used = usize::try_from(count).map_or(WINDOW, |n| n.min(WINDOW));
        let mut copy = [0_u32; WINDOW];
        for (slot, sample) in copy.iter_mut().zip(&self.samples).take(used) {
            *slot = sample.load(Ordering::Relaxed);
        }
        let window = &mut copy[..used];
        window.sort_unstable();
        Some(Duration::from_micros(u64::from(window[used / 2])))
    }

    /// Forgets the samples: a camera that stopped has no latency to show.
    pub fn reset(&self) {
        self.count.store(0, Ordering::Release);
    }
}

impl Default for LatencyMeter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn it_reads_nothing_before_the_first_sample() {
        assert_eq!(LatencyMeter::new().reading(), None);
    }

    #[test]
    fn it_reads_the_median_of_what_it_has() {
        let meter = LatencyMeter::new();
        for n in [30, 10, 20] {
            meter.record(ms(n));
        }
        assert_eq!(meter.reading(), Some(ms(20)));
    }

    #[test]
    fn it_forgets_the_oldest_samples_once_the_window_is_full() {
        let meter = LatencyMeter::new();
        for _ in 0..WINDOW {
            meter.record(ms(100));
        }
        for _ in 0..WINDOW {
            meter.record(ms(20));
        }
        assert_eq!(meter.reading(), Some(ms(20)));
    }

    #[test]
    fn reset_takes_it_back_to_unknown() {
        let meter = LatencyMeter::new();
        meter.record(ms(5));
        meter.reset();
        assert_eq!(meter.reading(), None);
    }
}
