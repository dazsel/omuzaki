//! [`FakePace`]: waits advance a shared fake clock instead of sleeping.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use omuzaki_ai_api::Progress;
use omuzaki_connect_api::Pace;
use omuzaki_core::time::{FakeClock, Seconds};

/// A [`Pace`] that moves `clock` forward by each wait and counts the waits. Tests never sleep. A wait whose
/// `progress` is already cancelled returns at once, as the contract says, and moves the clock not at all.
#[derive(Debug, Default)]
pub struct FakePace {
    /// The clock waits advance; share it with whatever the test reads time from.
    pub clock: Arc<FakeClock>,
    /// How many waits there were.
    pub waits: AtomicU32,
}

impl FakePace {
    /// A pace over `clock`.
    pub fn new(clock: Arc<FakeClock>) -> Self {
        Self { clock, waits: AtomicU32::new(0) }
    }
}

impl Pace for FakePace {
    fn wait(&self, seconds: f64, progress: &dyn Progress) {
        self.waits.fetch_add(1, Ordering::Relaxed);
        if !progress.is_cancelled() {
            self.clock.advance(Seconds(seconds));
        }
    }
}
