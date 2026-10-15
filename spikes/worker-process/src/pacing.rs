//! Deadline arithmetic for the steady output tick, shared by both lanes.

use std::time::{Duration, Instant};

/// The deadline after the tick due at `deadline` that has just finished at `now`: `max(deadline, now) + period`, the worker
/// loop's rule. A tick that ran late never makes the next one fire sooner than a full period after it, so no two presents
/// are closer than `period` apart. Also returns how many whole periods the tick was late (deadlines that passed unused).
#[must_use]
pub fn advance(deadline: Instant, period: Duration, now: Instant) -> (Instant, u64) {
    let late = now.saturating_duration_since(deadline);
    let missed = u64::try_from(late.as_nanos() / period.as_nanos()).unwrap_or(u64::MAX);
    (deadline.max(now) + period, missed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Duration = Duration::from_millis(33);

    #[test]
    fn on_time_ticks_are_one_period_apart() {
        let t0 = Instant::now();
        assert_eq!(advance(t0 + PERIOD, PERIOD, t0 + PERIOD), (t0 + PERIOD * 2, 0));
    }

    #[test]
    fn a_late_tick_waits_a_full_period_after_it_ends() {
        let t0 = Instant::now();
        // 100 ms late: three whole periods passed, and the next present is a full period after this one.
        let now = t0 + Duration::from_millis(100);
        assert_eq!(advance(t0, PERIOD, now), (now + PERIOD, 3));
    }

    #[test]
    fn a_tick_that_ends_just_before_the_next_deadline_does_not_get_a_short_follower() {
        let t0 = Instant::now();
        // The tick due at t0 finishes 1 ms before t0 + PERIOD. The old rule scheduled the next at t0 + PERIOD, 1 ms later.
        let now = t0 + Duration::from_millis(32);
        let (next, _) = advance(t0, PERIOD, now);
        assert!(next - now >= PERIOD, "follower only {:?} after", next - now);
    }
}
