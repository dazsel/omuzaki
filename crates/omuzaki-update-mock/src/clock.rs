//! A settable wall clock.

use std::sync::Mutex;

use omuzaki_update_api::clock::{Timestamp, WallClock};

use crate::locked;

/// Reports the time the test set. It only moves when told to, so expiry checks are deterministic.
#[derive(Debug)]
pub struct FakeWallClock {
    now: Mutex<i64>,
}

impl FakeWallClock {
    /// A clock showing `now`.
    #[must_use]
    pub fn new(now: Timestamp) -> Self {
        Self { now: Mutex::new(now.0) }
    }

    /// Jumps to `now`, forwards or back.
    pub fn set(&self, now: Timestamp) {
        *locked(&self.now) = now.0;
    }

    /// Moves forward by `seconds` (back when negative). Saturates instead of wrapping.
    pub fn advance(&self, seconds: i64) {
        let mut now = locked(&self.now);
        *now = now.saturating_add(seconds);
    }
}

impl WallClock for FakeWallClock {
    fn now(&self) -> Timestamp {
        Timestamp(*locked(&self.now))
    }
}
