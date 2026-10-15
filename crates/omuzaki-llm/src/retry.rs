//! Retries before the first event (llm-api D5): how long to wait, and which failures are worth another try.

use std::time::Duration;

/// The most a server's `retry-after` is honoured.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// The ceiling of the jittered wait.
pub(crate) const MAX_JITTER_CEILING: Duration = Duration::from_secs(16);

/// Chooses the wait before each retry. Jitter comes from a seeded generator, so a test that fixes the seed gets the
/// same waits every time.
#[derive(Clone, Debug)]
pub(crate) struct Backoff {
    state: u64,
}

impl Backoff {
    /// A backoff whose jitter follows `seed`.
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
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
    /// [`MAX_RETRY_AFTER`]; otherwise full jitter: uniform in `[0, min(16 s, 1 s × 2^(attempt − 1))]`.
    pub(crate) fn wait(&mut self, attempt: u8, retry_after: Option<Duration>) -> Duration {
        if let Some(asked) = retry_after {
            return asked.min(MAX_RETRY_AFTER);
        }
        let doublings = u32::from(attempt.saturating_sub(1)).min(16);
        let ceiling = Duration::from_secs(1).saturating_mul(1 << doublings).min(MAX_JITTER_CEILING);
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
        let mut b = Backoff::new(1);
        assert_eq!(b.wait(1, Some(Duration::from_secs(7))), Duration::from_secs(7));
        assert_eq!(b.wait(4, Some(Duration::from_secs(3_600))), MAX_RETRY_AFTER);
        assert_eq!(b.wait(1, Some(Duration::ZERO)), Duration::ZERO);
    }

    #[test]
    fn jitter_stays_under_the_doubling_ceiling_and_the_16_second_cap() {
        let mut b = Backoff::new(42);
        for attempt in 1..=40u8 {
            let ceiling = Duration::from_secs(1).saturating_mul(1 << u32::from(attempt - 1).min(16)).min(MAX_JITTER_CEILING);
            for _ in 0..200 {
                assert!(b.wait(attempt, None) <= ceiling, "attempt {attempt}");
            }
        }
        // The ceiling really is reached by the bound, not stuck low.
        let mut b = Backoff::new(7);
        assert!((0..500).any(|_| b.wait(1, None) > Duration::from_millis(500)));
        assert!((0..500).any(|_| b.wait(8, None) > Duration::from_secs(8)));
    }

    #[test]
    fn the_same_seed_gives_the_same_waits() {
        let run = |seed| {
            let mut b = Backoff::new(seed);
            (1..=4).map(|n| b.wait(n, None)).collect::<Vec<_>>()
        };
        assert_eq!(run(9), run(9));
        assert_ne!(run(9), run(10));
    }
}
