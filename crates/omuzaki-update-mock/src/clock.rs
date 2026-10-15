//! A settable wall clock.

use std::sync::atomic::{AtomicI64, Ordering};

use omuzaki_update_api::clock::{Timestamp, WallClock};

/// Reports the time the test set. It only moves when told to, so expiry checks are deterministic.
#[derive(Debug)]
pub struct FakeWallClock {
    now: AtomicI64,
}

impl FakeWallClock {
    /// A clock showing `now`.
    #[must_use]
    pub fn new(now: Timestamp) -> Self {
        Self { now: AtomicI64::new(now.0) }
    }

    /// Jumps to `now`, forwards or back.
    pub fn set(&self, now: Timestamp) {
        self.now.store(now.0, Ordering::SeqCst);
    }

    /// Moves forward by `seconds` (back when negative). Saturates instead of wrapping.
    pub fn advance(&self, seconds: i64) {
        // The closure always answers `Some`, so the update cannot fail and there is no error to report.
        self.now.try_update(Ordering::SeqCst, Ordering::SeqCst, |t| Some(t.saturating_add(seconds))).ok();
    }
}

impl WallClock for FakeWallClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.now.load(Ordering::SeqCst))
    }
}
