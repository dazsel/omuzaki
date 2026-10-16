//! The seeded kill schedule: the same seed gives the same sequence of delays on every run and machine.

use std::time::Duration;

/// Draws kill delays uniformly from `min_ms..=max_ms` with `SplitMix64` (no dependency, fixed algorithm).
#[derive(Clone, Debug)]
pub struct KillSchedule {
    state: u64,
    min_ms: u64,
    max_ms: u64,
}

impl KillSchedule {
    /// A schedule for `seed`. `max_ms` below `min_ms` is raised to it.
    #[must_use]
    pub fn new(seed: u64, min_ms: u64, max_ms: u64) -> Self {
        Self { state: seed, min_ms, max_ms: max_ms.max(min_ms) }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// How long after a worker's spawn the next kill falls. The modulo bias is below 1e-15 for these ranges.
    pub fn next_delay(&mut self) -> Duration {
        let span = self.max_ms - self.min_ms + 1;
        Duration::from_millis(self.min_ms + self.next_u64() % span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_delays() {
        let draw = |seed| {
            let mut s = KillSchedule::new(seed, 50, 4000);
            (0..64).map(|_| s.next_delay()).collect::<Vec<_>>()
        };
        assert_eq!(draw(1022), draw(1022));
        assert_ne!(draw(1022), draw(1023));
    }

    #[test]
    fn delays_stay_in_range_and_spread() {
        let mut s = KillSchedule::new(7, 50, 4000);
        let delays: Vec<u64> = (0..2000).map(|_| u64::try_from(s.next_delay().as_millis()).unwrap_or(0)).collect();
        assert!(delays.iter().all(|d| (50..=4000).contains(d)));
        // Uniform over 50..4000 puts about a quarter of the draws in each quarter of the range.
        let low = delays.iter().filter(|&&d| d < 1037).count();
        assert!((400..600).contains(&low), "{low} of 2000 below a quarter of the range");
    }
}
