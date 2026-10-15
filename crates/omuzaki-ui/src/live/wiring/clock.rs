//! [`PerformClock`]: where the perform thread gets its time and how it waits for the next step.
//!
//! The app runs on [`RealClock`]; a test passes its own clock, which also runs on the perform thread, so it can see
//! exactly what the thread does between steps.

use std::time::{Duration, Instant};

use omuzaki_core::time::Seconds;

/// The time source of the perform loop. It is moved onto the perform thread and only used there.
pub trait PerformClock: Send {
    /// Seconds since the clock started. Never goes backwards.
    fn now(&mut self) -> Seconds;

    /// Returns at `at` or soon after; at once when `at` has passed. Must not allocate or lock.
    fn wait_until(&mut self, at: Seconds);
}

/// The wall clock: [`Instant`] from when it was made.
#[derive(Debug)]
pub struct RealClock {
    origin: Instant,
}

/// How long before a deadline [`RealClock`] stops sleeping and yields instead. Sleeps overshoot by up to a scheduler
/// tick (about 1 ms on Linux, more on Windows without a raised timer resolution); the rest of the wait yields so a
/// step is not late by a whole tick.
const SPIN_BEFORE: Duration = Duration::from_millis(2);

impl RealClock {
    /// A clock starting now.
    pub fn new() -> Self {
        Self { origin: Instant::now() }
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl PerformClock for RealClock {
    fn now(&mut self) -> Seconds {
        Seconds(self.origin.elapsed().as_secs_f64())
    }

    fn wait_until(&mut self, at: Seconds) {
        // A deadline before the origin, or a NaN one, has already passed.
        let Ok(offset) = Duration::try_from_secs_f64(at.0) else { return };
        let deadline = self.origin + offset;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            if left > SPIN_BEFORE {
                std::thread::sleep(left - SPIN_BEFORE);
            } else {
                std::thread::yield_now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_clock_waits_until_the_deadline_and_not_for_a_past_one() {
        let mut clock = RealClock::new();
        let start = clock.now();
        clock.wait_until(Seconds(start.0 + 0.01));
        assert!(clock.now().0 >= start.0 + 0.01);
        let before = Instant::now();
        clock.wait_until(Seconds(-1.0));
        clock.wait_until(Seconds(f64::NAN));
        clock.wait_until(start);
        assert!(before.elapsed() < Duration::from_millis(50), "past deadlines return at once");
    }
}
