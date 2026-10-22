//! [`FakeWall`]: wall time derived from a start instant plus a fake clock, so a schedule test can start on a
//! daylight-saving day, and [`jump`](FakeWall::jump) for wall-clock jump tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_live_api::event::UtcMillis;
use omuzaki_live_api::services::WallClock;

/// Wall time that follows a [`FakeClock`]: `start` plus the time the clock moved since, plus any jumps.
#[derive(Debug)]
pub struct FakeWall {
    start: UtcMillis,
    clock: Arc<FakeClock>,
    origin: Seconds,
    jumped_ms: AtomicI64,
}

impl FakeWall {
    /// Wall time `start` now, moving with `clock` from here on.
    pub fn new(start: UtcMillis, clock: Arc<FakeClock>) -> Self {
        let origin = clock.now();
        Self { start, clock, origin, jumped_ms: AtomicI64::new(0) }
    }

    /// Moves the wall clock by `ms` (forward or back) without moving the runner clock: an NTP correction, a person
    /// changing the time, a daylight-saving bug.
    pub fn jump(&self, ms: i64) {
        self.jumped_ms.fetch_add(ms, Ordering::AcqRel);
    }
}

impl WallClock for FakeWall {
    fn now(&self) -> UtcMillis {
        let elapsed = (self.clock.now().0 - self.origin.0) * 1_000.0;
        // Rounded to the millisecond; fake runs never come near i64's range.
        #[allow(clippy::cast_possible_truncation)]
        let elapsed_ms = elapsed.round() as i64;
        UtcMillis(self.start.0 + elapsed_ms + self.jumped_ms.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_time_follows_the_clock_and_jumps_alone() {
        let clock = Arc::new(FakeClock::new(Seconds(5.0)));
        let wall = FakeWall::new(UtcMillis(1_000), Arc::clone(&clock));
        assert_eq!(wall.now(), UtcMillis(1_000));
        clock.advance(Seconds(1.5));
        assert_eq!(wall.now(), UtcMillis(2_500));
        wall.jump(-3_600_000);
        assert_eq!((wall.now(), clock.now()), (UtcMillis(2_500 - 3_600_000), Seconds(6.5)));
    }
}
