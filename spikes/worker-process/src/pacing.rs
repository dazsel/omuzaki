//! Deadline arithmetic for the steady output tick, shared by both lanes.

use std::time::{Duration, Instant};

/// The tick after `next`. A tick that ran late does not make the following ones fire back to back: deadlines already in the
/// past are skipped, and how many were skipped is returned so the report can show it.
#[must_use]
pub fn advance(next: Instant, period: Duration, now: Instant) -> (Instant, u64) {
    let next = next + period;
    if next >= now {
        return (next, 0);
    }
    let skipped = (now - next).as_nanos() / period.as_nanos() + 1;
    let steps = u32::try_from(skipped).unwrap_or(u32::MAX);
    (next + period * steps, u64::try_from(skipped).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Duration = Duration::from_millis(33);

    #[test]
    fn on_time_ticks_are_one_period_apart() {
        let t0 = Instant::now();
        assert_eq!(advance(t0, PERIOD, t0 + Duration::from_millis(5)), (t0 + PERIOD, 0));
    }

    #[test]
    fn a_late_tick_skips_the_missed_deadlines_instead_of_bursting() {
        let t0 = Instant::now();
        // 100 ms late: the deadlines at +33 and +66 passed, and +99 is also behind 100 ms, so the next is +132.
        let (next, skipped) = advance(t0, PERIOD, t0 + Duration::from_millis(100));
        assert_eq!(skipped, 3);
        assert_eq!(next, t0 + PERIOD * 4);
        assert!(next > t0 + Duration::from_millis(100));
    }
}
