//! The clock the mock reads: injected, so keep-alive, cooldown and arrival times never depend on the wall clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub use crate::clock::Clock;

/// A clock that moves only when a test calls [`advance`](Self::advance). Clones share one time.
#[derive(Debug, Clone, Default)]
pub struct FakeClock(Arc<AtomicU64>);

impl FakeClock {
    /// Moves the clock forward by `by`. Steps are whole nanoseconds; a total beyond about 584 years wraps.
    pub fn advance(&self, by: Duration) {
        self.0.fetch_add(u64::try_from(by.as_nanos()).unwrap_or(u64::MAX), Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.0.load(Ordering::SeqCst))
    }
}
