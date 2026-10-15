//! Device time onto the stage clock: an offset and a drift, fitted from when frames arrive.
//!
//! A camera stamps frames on its own clock; the stage runs on its monotonic clock. Neither the offset between them
//! nor the camera's drift (crystals differ by up to a few hundred ppm) is known, and arrival times carry the
//! driver's jitter. The map keeps the last [`CLOCK_WINDOW`] pairs `(device time, arrival time)`, fits the rate by
//! least squares (clamped to [`MAX_DRIFT`]) and puts the line through the **lowest** arrival: the frame that waited
//! least. A mapped time is therefore the capture time plus the smallest transport delay seen, which is constant and
//! not observable from timestamps alone; what matters downstream is that the spacing between frames is the
//! camera's, without the jitter.
//!
//! That is the **arrival floor**, not the capture time. Latency measured from a mapped time to publish
//! ([`LatencyStats`](crate::tracker::LatencyStats)) leaves the constant transport delay out. Including it needs a
//! calibrated device-to-stage offset, which this map does not have; a later change would add one.

use omuzaki_core::time::Seconds;

use crate::camera::DeviceTime;

/// Pairs kept for the fit: about 2 s at 30 fps, 1 s at 60.
pub const CLOCK_WINDOW: usize = 64;

/// The largest drift believed, as a fraction (1000 ppm); a fitted rate outside `1 ± MAX_DRIFT` is clamped.
pub const MAX_DRIFT: f64 = 1e-3;

/// A device-time jump forward larger than this, in seconds, restarts the fit (the stream paused, or the device
/// reset its clock forward).
pub const RESYNC_GAP: f64 = 1.0;

/// The smallest step between two mapped times, in seconds (1 µs): output is strictly increasing, so the
/// retarget's filters never see `dt = 0`.
pub const MIN_STEP: f64 = 1e-6;

/// Pairs needed before the rate is fitted; with fewer it is 1.
const MIN_FIT: usize = 8;

/// Why a timestamp was not mapped.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ClockError {
    /// The device clock went backwards. The frame is rejected (drop it); the map is unchanged.
    #[error("device time went backwards, from {last:?} to {got:?}")]
    Backwards {
        /// The newest time accepted so far.
        last: DeviceTime,
        /// The time that was rejected.
        got: DeviceTime,
    },
}

/// Maps one stream's [`DeviceTime`]s onto the stage clock. Fixed-size; [`map`](Self::map) is hot path.
#[derive(Clone, Debug)]
pub struct ClockMap {
    /// Device time of fit coordinate 0; `None` before the first frame.
    origin: Option<DeviceTime>,
    /// The newest device time accepted.
    last: DeviceTime,
    /// Device seconds since `origin`, a ring of `len` entries ending before `head`.
    device: [f64; CLOCK_WINDOW],
    /// Stage seconds at arrival, paired with `device`.
    arrived: [f64; CLOCK_WINDOW],
    len: usize,
    head: usize,
    /// The last mapped time, so the next one is at least [`MIN_STEP`] later.
    last_out: f64,
}

impl Default for ClockMap {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockMap {
    /// An empty map.
    pub const fn new() -> Self {
        Self {
            origin: None,
            last: DeviceTime(0),
            device: [0.0; CLOCK_WINDOW],
            arrived: [0.0; CLOCK_WINDOW],
            len: 0,
            head: 0,
            last_out: f64::NEG_INFINITY,
        }
    }

    /// Forgets the fit, for a reopened stream whose device clock has a new epoch. Output still strictly increases.
    pub fn reset(&mut self) {
        *self = Self { last_out: self.last_out, ..Self::new() };
    }

    /// The arrival-floor stage time of the frame stamped `captured`, given that it `arrived` (stage clock) now: its
    /// capture time plus the smallest transport delay seen, not the capture time itself (see the module docs).
    /// Equal device times are fine. The result is **strictly increasing**: at least [`MIN_STEP`] after the previous
    /// one, also when the lower envelope drops (a frame that waited less than any before it) or the device repeats a
    /// time, so no two frames share a stage time. Hot path: no allocation.
    ///
    /// # Errors
    /// [`ClockError::Backwards`] when `captured` is before the newest accepted time.
    pub fn map(&mut self, captured: DeviceTime, arrived: Seconds) -> Result<Seconds, ClockError> {
        if self.origin.is_some() && captured < self.last {
            return Err(ClockError::Backwards { last: self.last, got: captured });
        }
        if self.origin.is_none() || nanos_to_secs(captured.0 - self.last.0) > RESYNC_GAP {
            *self = Self { origin: Some(captured), last_out: self.last_out, ..Self::new() };
        }
        self.last = captured;
        let origin = self.origin.unwrap_or(captured);
        let d = nanos_to_secs(captured.0 - origin.0);
        self.device[self.head] = d;
        self.arrived[self.head] = arrived.0;
        self.head = (self.head + 1) % CLOCK_WINDOW;
        self.len = (self.len + 1).min(CLOCK_WINDOW);

        let rate = self.fit_rate();
        let offset = (0..self.len).map(|i| self.arrived[i] - rate * self.device[i]).fold(f64::INFINITY, f64::min);
        let out = (offset + rate * d).max(self.last_out + MIN_STEP);
        self.last_out = out;
        Ok(Seconds(out))
    }

    /// Least-squares slope of arrival against device time, clamped to `1 ± MAX_DRIFT`.
    #[allow(clippy::cast_precision_loss)] // len <= CLOCK_WINDOW
    fn fit_rate(&self) -> f64 {
        if self.len < MIN_FIT {
            return 1.0;
        }
        let n = self.len as f64;
        let (d, a) = (&self.device[..self.len], &self.arrived[..self.len]);
        let (md, ma) = (d.iter().sum::<f64>() / n, a.iter().sum::<f64>() / n);
        let (mut cov, mut var) = (0.0, 0.0);
        for (x, y) in d.iter().zip(a) {
            cov += (x - md) * (y - ma);
            var += (x - md) * (x - md);
        }
        if var <= 0.0 { 1.0 } else { (cov / var).clamp(1.0 - MAX_DRIFT, 1.0 + MAX_DRIFT) }
    }
}

#[allow(clippy::cast_precision_loss)] // exact below 2^53 ns, about 104 days
fn nanos_to_secs(nanos: u64) -> f64 {
    nanos as f64 * 1e-9
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
mod tests {
    use super::*;

    const LATENCY: f64 = 0.010;

    /// Device stamp of true time `t` on a clock that runs `ppm` fast from an arbitrary epoch.
    fn stamp(t: f64, ppm: f64) -> DeviceTime {
        DeviceTime(7_000_000_000 + (t * (1.0 + ppm * 1e-6) * 1e9).round() as u64)
    }

    /// 0 to 4.9 ms of arrival jitter, deterministic.
    fn jitter(i: u64) -> f64 {
        ((i * 7919) % 50) as f64 * 1e-4
    }

    #[test]
    fn a_fixed_offset_maps_exactly() {
        let mut map = ClockMap::new();
        for i in 0..200 {
            let t = f64::from(i) / 30.0;
            let out = map.map(stamp(t, 0.0), Seconds(100.0 + t + LATENCY)).unwrap();
            assert!((out.0 - (100.0 + t + LATENCY)).abs() < 1e-6, "frame {i}: {}", out.0);
        }
    }

    #[test]
    fn drift_of_200_ppm_either_way_stays_within_a_millisecond_and_jitter_is_removed() {
        for ppm in [200.0, -200.0] {
            let mut map = ClockMap::new();
            let mut previous = f64::NEG_INFINITY;
            for i in 0..1800_u64 {
                let t = i as f64 / 30.0;
                let out = map.map(stamp(t, ppm), Seconds(t + LATENCY + jitter(i))).unwrap().0;
                assert!(out > previous, "strictly increasing at frame {i}");
                previous = out;
                if i >= 64 {
                    let error = out - (t + LATENCY);
                    assert!(error.abs() < 1e-3, "{ppm} ppm, frame {i}: off by {error}");
                }
            }
        }
    }

    #[test]
    fn a_falling_delay_never_repeats_a_time() {
        // Each frame waits less than the one before by more than the frame spacing, so every frame is a new lowest
        // arrival and the envelope drops below the last output; then the device repeats a time.
        let mut map = ClockMap::new();
        let mut previous = f64::NEG_INFINITY;
        for i in 0..20_u64 {
            let t = i as f64 / 30.0;
            let delay = 0.8 - i as f64 * 0.04;
            let out = map.map(stamp(t, 0.0), Seconds(t + delay)).unwrap().0;
            assert!(out >= previous + MIN_STEP * 0.999, "frame {i}: {out} after {previous}");
            previous = out;
        }
        let same = stamp(19.0 / 30.0, 0.0);
        let out = map.map(same, Seconds(previous)).unwrap().0;
        assert!(out >= previous + MIN_STEP * 0.999, "a repeated device time: {out} after {previous}");
    }

    #[test]
    fn a_backwards_jump_is_rejected_and_tracking_continues() {
        let mut map = ClockMap::new();
        let a = map.map(DeviceTime(2_000_000_000), Seconds(10.0)).unwrap();
        let err = map.map(DeviceTime(1_000_000_000), Seconds(10.033)).unwrap_err();
        assert_eq!(err, ClockError::Backwards { last: DeviceTime(2_000_000_000), got: DeviceTime(1_000_000_000) });
        let b = map.map(DeviceTime(2_033_000_000), Seconds(10.033)).unwrap();
        assert!((b.0 - a.0 - 0.033).abs() < 1e-9);
    }

    #[test]
    fn a_long_forward_gap_resyncs_without_going_backwards() {
        let mut map = ClockMap::new();
        for i in 0..30 {
            map.map(DeviceTime(i * 33_000_000), Seconds(5.0 + i as f64 * 0.033)).unwrap();
        }
        // The device clock leaps an hour; the frame really arrived 2 s later.
        let out = map.map(DeviceTime(3_600_000_000_000), Seconds(7.0)).unwrap();
        assert!((out.0 - 7.0).abs() < 1e-9, "{}", out.0);
        map.reset();
        assert!(map.map(DeviceTime(0), Seconds(6.0)).unwrap().0 >= 7.0, "reset keeps output monotonic");
    }
}
