use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use omuzaki_history_api::{WallClock, WallTime};

/// The real wall clock: UTC Unix milliseconds from the operating system. A clock set before 1970 reads as 0.
#[derive(Copy, Clone, Debug, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> WallTime {
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
        WallTime(u64::try_from(ms).unwrap_or(u64::MAX))
    }
}

/// A clock a test sets and advances; it never moves by itself.
#[derive(Debug, Default)]
pub struct FakeWallClock(AtomicU64);

impl FakeWallClock {
    /// A clock reading `start`.
    pub fn new(start: WallTime) -> Self {
        Self(AtomicU64::new(start.0))
    }

    /// Sets the time.
    pub fn set(&self, time: WallTime) {
        self.0.store(time.0, Ordering::SeqCst);
    }

    /// Moves the time forward by `ms` milliseconds.
    pub fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl WallClock for FakeWallClock {
    fn now(&self) -> WallTime {
        WallTime(self.0.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_clock_moves_only_when_told() {
        let clock = FakeWallClock::new(WallTime(1_000));
        assert_eq!(clock.now(), WallTime(1_000));
        clock.advance(250);
        assert_eq!(clock.now(), WallTime(1_250));
        clock.set(WallTime(5));
        assert_eq!(clock.now(), WallTime(5));
    }

    #[test]
    fn the_system_clock_is_after_2020() {
        assert!(SystemWallClock.now().0 > 1_577_836_800_000);
    }
}
