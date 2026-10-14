//! Time: a seconds type and a [`Clock`] that tests can fake.
//!
//! Nothing reads the OS clock directly; systems take a `&dyn Clock`. That's what lets the life layer, physics and
//! op logs run in tests and replays without sleeping or flaking (spec §8, §39 rule 6).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A duration or a point on a monotonic timeline, in seconds.
///
/// `f64` keeps sub-microsecond precision over multi-day sessions, which `f32` can't (it loses milliseconds after
/// about 4.6 hours).
#[derive(Copy, Clone, Debug, Default, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Seconds(pub f64);

/// A monotonic time source.
pub trait Clock: Send + Sync {
    /// The current time since this clock's epoch.
    fn now(&self) -> Seconds;
}

/// The real clock: seconds since it was created.
#[derive(Debug)]
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    /// Starts a clock at zero.
    pub fn new() -> Self {
        Self { start: Instant::now() }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Seconds {
        Seconds(self.start.elapsed().as_secs_f64())
    }
}

/// A clock that only moves when told to: for tests and deterministic replays.
#[derive(Debug, Default)]
pub struct FakeClock {
    bits: AtomicU64, // f64 bits, so the clock can be shared (`&self`) across threads without a lock
}

impl FakeClock {
    /// A fake clock at `start`.
    pub fn new(start: Seconds) -> Self {
        Self { bits: AtomicU64::new(start.0.to_bits()) }
    }

    /// Moves the clock forward (or back, for tests of misbehaving sources).
    pub fn advance(&self, by: Seconds) {
        let now = f64::from_bits(self.bits.load(Ordering::Acquire));
        self.bits.store((now + by.0).to_bits(), Ordering::Release);
    }

    /// Sets the clock to an exact time.
    pub fn set(&self, to: Seconds) {
        self.bits.store(to.0.to_bits(), Ordering::Release);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Seconds {
        Seconds(f64::from_bits(self.bits.load(Ordering::Acquire)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_moves_only_when_told() {
        let c = FakeClock::new(Seconds(1.0));
        assert_eq!(c.now(), Seconds(1.0));
        c.advance(Seconds(0.5));
        assert_eq!(c.now(), Seconds(1.5));
        c.set(Seconds(10.0));
        assert_eq!(c.now(), Seconds(10.0));
    }

    #[test]
    fn system_clock_is_monotonic() {
        let c = SystemClock::new();
        let a = c.now();
        let b = c.now();
        assert!(b >= a);
    }
}
