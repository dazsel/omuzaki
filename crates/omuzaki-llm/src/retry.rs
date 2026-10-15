//! Retries before the first event (llm-api D5): how long to wait, and which failures are worth another try.

use std::time::Duration;

/// The most a server's `retry-after` is honoured.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// The jittered wait doubles from one unit and stops growing at this many units (16 s with the default 1 s unit).
pub(crate) const MAX_UNITS: u32 = 16;
/// The unit of the jittered wait, 1 s (llm-api D5).
pub(crate) const DEFAULT_UNIT: Duration = Duration::from_secs(1);

/// Chooses the wait before each retry. Jitter comes from a seeded generator, so a test that fixes the seed gets the
/// same waits every time.
#[derive(Clone, Debug)]
pub(crate) struct Backoff {
    state: u64,
    unit: Duration,
}

impl Backoff {
    /// A backoff whose jitter follows `seed` and whose waits double from `unit`.
    pub(crate) fn new(seed: u64, unit: Duration) -> Self {
        Self { state: seed, unit }
    }

    /// splitmix64: small, fast and good enough to spread retries.
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// The wait before retry number `attempt` (from 1). It is `retry_after` when the server gave one, capped at
    /// [`MAX_RETRY_AFTER`]; otherwise full jitter: uniform in `[0, min(16 units, 1 unit × 2^(attempt − 1))]`, where a
    /// unit is 1 s.
    pub(crate) fn wait(&mut self, attempt: u8, retry_after: Option<Duration>) -> Duration {
        if let Some(asked) = retry_after {
            return asked.min(MAX_RETRY_AFTER);
        }
        let doublings = u32::from(attempt.saturating_sub(1)).min(MAX_UNITS);
        let ceiling = self.unit.saturating_mul((1u32 << doublings).min(MAX_UNITS));
        let micros = u64::try_from(ceiling.as_micros()).unwrap_or(u64::MAX);
        Duration::from_micros(self.next() % micros.saturating_add(1))
    }
}

/// A seed for real use: random per process, so clients that failed together do not retry together.
pub(crate) fn random_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new().build_hasher().finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_servers_retry_after_wins_and_is_capped() {
        let mut b = Backoff::new(1, DEFAULT_UNIT);
        assert_eq!(b.wait(1, Some(Duration::from_secs(7))), Duration::from_secs(7));
        assert_eq!(b.wait(4, Some(Duration::from_secs(3_600))), MAX_RETRY_AFTER);
        assert_eq!(b.wait(1, Some(Duration::ZERO)), Duration::ZERO);
    }

    #[test]
    fn jitter_stays_under_the_doubling_ceiling_and_the_16_second_cap() {
        let mut b = Backoff::new(42, DEFAULT_UNIT);
        for attempt in 1..=40u8 {
            let ceiling = Duration::from_secs(u64::from((1u32 << u32::from(attempt - 1).min(16)).min(MAX_UNITS)));
            for _ in 0..200 {
                assert!(b.wait(attempt, None) <= ceiling, "attempt {attempt}");
            }
        }
        // The ceiling really is reached by the bound, not stuck low.
        let mut b = Backoff::new(7, DEFAULT_UNIT);
        assert!((0..500).any(|_| b.wait(1, None) > Duration::from_millis(500)));
        assert!((0..500).any(|_| b.wait(8, None) > Duration::from_secs(8)));
    }

    #[test]
    fn the_unit_scales_the_jitter() {
        let mut b = Backoff::new(5, Duration::from_millis(2));
        for attempt in 1..=10u8 {
            assert!(b.wait(attempt, None) <= Duration::from_millis(32), "attempt {attempt}");
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_waits() {
        let run = |seed| {
            let mut b = Backoff::new(seed, DEFAULT_UNIT);
            (1..=4).map(|n| b.wait(n, None)).collect::<Vec<_>>()
        };
        assert_eq!(run(9), run(9));
        assert_ne!(run(9), run(10));
    }
}
