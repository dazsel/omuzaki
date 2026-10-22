//! The clock the bridge and the mock read: injected, so backoff, rate caps, keep-alives and cooldowns never depend on
//! the wall clock in a test.

use std::time::{Duration, Instant};

/// A monotonic clock: the time since an arbitrary start, never going backwards.
pub trait Clock: Send + Sync {
    /// The time since the clock's start.
    fn now(&self) -> Duration;
}

/// The OS monotonic clock, from when it was made.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock(Instant);

impl SystemClock {
    /// A clock starting now.
    #[must_use]
    pub fn new() -> Self {
        Self(Instant::now())
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}
