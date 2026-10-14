//! The engine clock (design doc section 5.1): one sample counter at 48 kHz on the clock device, mapped to the
//! pipeline's monotonic time.
//!
//! The audio thread publishes a [`ClockSnapshot`] after each callback (the engine fits it with a delay-locked loop on
//! the callback timestamps, as JACK does). Any thread reads it lock-free through [`EngineClock`], then converts with
//! [`ClockSnapshot::time_of`] and [`ClockSnapshot::sample_at`]. Recording uses it to put audio and video on one timeline.
//! When the clock device changes, the engine publishes a snapshot that continues the old mapping, so time never jumps.

use std::sync::atomic::{AtomicU64, Ordering, fence};

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A line through (sample, time) points: the mapping at one instant.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClockSnapshot {
    /// A sample index on the engine timeline.
    pub sample: u64,
    /// The pipeline time at which `sample` plays or was captured.
    pub time: Seconds,
    /// The measured length of one sample, in seconds. Nominally 1/48000; the loop tracks the device's real rate.
    pub seconds_per_sample: f64,
}

impl ClockSnapshot {
    /// The mapping of a clock that runs at exactly `rate` Hz and has sample 0 at time 0.
    #[must_use]
    pub fn nominal(rate: u32) -> Self {
        Self { sample: 0, time: Seconds(0.0), seconds_per_sample: 1.0 / f64::from(rate) }
    }

    /// The pipeline time of `sample`. Samples before the anchor map to earlier times, so this is total.
    #[must_use]
    pub fn time_of(&self, sample: u64) -> Seconds {
        // Sample indices stay far below 2^53 (178 years at 48 kHz), so the f64 conversions are exact.
        #[allow(clippy::cast_precision_loss)]
        let offset = sample as f64 - self.sample as f64;
        Seconds(self.time.0 + offset * self.seconds_per_sample)
    }

    /// The sample being played or captured at `time`, rounded down; 0 for a time before sample 0.
    #[must_use]
    pub fn sample_at(&self, time: Seconds) -> u64 {
        #[allow(clippy::cast_precision_loss)]
        let anchor = self.sample as f64;
        let exact = anchor + (time.0 - self.time.0) / self.seconds_per_sample;
        if exact.is_nan() || exact <= 0.0 {
            return 0;
        }
        // Floored on purpose: a sample index is not fractional. The millionth of a sample absorbs the rounding of the
        // forward and inverse maps, so `sample_at(time_of(n)) == n`.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            (exact + 1e-6).floor() as u64
        }
    }
}

/// A shared cell holding the latest [`ClockSnapshot`]: one writer (the audio thread), any number of readers, no locks,
/// no allocation.
///
/// It is a sequence lock built from atomics, in safe Rust. The writer makes the counter odd, writes the three fields,
/// and makes it even again; a reader retries if it saw an odd counter or the counter changed while it read. Readers can
/// therefore spin for the few nanoseconds of a publish, but the writer never waits for them.
#[derive(Debug)]
pub struct EngineClock {
    sequence: AtomicU64,
    sample: AtomicU64,
    time_bits: AtomicU64,
    period_bits: AtomicU64,
}

impl EngineClock {
    /// A clock that starts with `initial`.
    #[must_use]
    pub fn new(initial: ClockSnapshot) -> Self {
        Self {
            sequence: AtomicU64::new(0),
            sample: AtomicU64::new(initial.sample),
            time_bits: AtomicU64::new(initial.time.0.to_bits()),
            period_bits: AtomicU64::new(initial.seconds_per_sample.to_bits()),
        }
    }

    /// Publishes a new mapping. **Only one thread may call this** (the audio thread, or the timer clock that replaces
    /// it when the clock device is lost, never both at once).
    pub fn publish(&self, snapshot: ClockSnapshot) {
        let sequence = self.sequence.load(Ordering::Relaxed);
        self.sequence.store(sequence.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.sample.store(snapshot.sample, Ordering::Relaxed);
        self.time_bits.store(snapshot.time.0.to_bits(), Ordering::Relaxed);
        self.period_bits.store(snapshot.seconds_per_sample.to_bits(), Ordering::Relaxed);
        self.sequence.store(sequence.wrapping_add(2), Ordering::Release);
    }

    /// The latest mapping, consistent (never a mixture of two publishes).
    #[must_use]
    pub fn read(&self) -> ClockSnapshot {
        loop {
            let before = self.sequence.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let snapshot = ClockSnapshot {
                sample: self.sample.load(Ordering::Relaxed),
                time: Seconds(f64::from_bits(self.time_bits.load(Ordering::Relaxed))),
                seconds_per_sample: f64::from_bits(self.period_bits.load(Ordering::Relaxed)),
            };
            fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == before {
                return snapshot;
            }
        }
    }

    /// The pipeline time of `sample` under the latest mapping.
    #[must_use]
    pub fn time_of(&self, sample: u64) -> Seconds {
        self.read().time_of(sample)
    }

    /// The sample at `time` under the latest mapping.
    #[must_use]
    pub fn sample_at(&self, time: Seconds) -> u64 {
        self.read().sample_at(time)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn a_nominal_clock_counts_48000_samples_per_second() {
        let clock = EngineClock::new(ClockSnapshot::nominal(48_000));
        assert_eq!(clock.time_of(48_000), Seconds(1.0));
        assert_eq!(clock.sample_at(Seconds(2.5)), 120_000);
        assert_eq!(clock.sample_at(Seconds(-1.0)), 0);
    }

    #[test]
    fn the_mapping_round_trips_and_follows_a_drifting_device() {
        let snapshot = ClockSnapshot { sample: 96_000, time: Seconds(10.0), seconds_per_sample: 1.0 / 48_004.8 };
        for sample in [0u64, 1, 95_999, 96_000, 1_000_000, 123_456_789] {
            assert_eq!(snapshot.sample_at(snapshot.time_of(sample)), sample);
        }
        assert!(snapshot.time_of(96_000 + 48_000).0 < 11.0, "a fast device packs more samples into a second");
    }

    #[test]
    fn publishing_replaces_the_mapping_without_a_jump() {
        let clock = EngineClock::new(ClockSnapshot::nominal(48_000));
        let at = clock.time_of(480_000);
        // A new device continues the old mapping from sample 480000.
        clock.publish(ClockSnapshot { sample: 480_000, time: at, seconds_per_sample: 1.0 / 48_010.0 });
        assert_eq!(clock.time_of(480_000), at);
        assert_eq!(clock.read().seconds_per_sample, 1.0 / 48_010.0);
    }

    #[test]
    fn a_reader_never_sees_a_mixture_of_two_publishes() {
        // Each publish keeps `time == sample / 1000` and `seconds_per_sample == sample / 1e6`, so a torn read breaks one.
        let clock =
            Arc::new(EngineClock::new(ClockSnapshot { sample: 0, time: Seconds(0.0), seconds_per_sample: 0.0 }));
        let writer = {
            let clock = Arc::clone(&clock);
            std::thread::spawn(move || {
                for n in 1..=200_000u64 {
                    #[allow(clippy::cast_precision_loss)]
                    let f = n as f64;
                    clock.publish(ClockSnapshot { sample: n, time: Seconds(f / 1000.0), seconds_per_sample: f / 1e6 });
                }
            })
        };
        let mut last = 0;
        while last < 200_000 {
            let s = clock.read();
            #[allow(clippy::cast_precision_loss)]
            let f = s.sample as f64;
            assert_eq!((s.time.0, s.seconds_per_sample), (f / 1000.0, f / 1e6), "torn read at sample {}", s.sample);
            assert!(s.sample >= last, "went backwards");
            last = s.sample;
        }
        writer.join().unwrap();
    }
}
