//! Waiting on the host's clock. Every wait of the host thread (the paused poll, the rate-limit pause, the Stop poll)
//! is measured on [`HostConfig::clock`](crate::config::HostConfig::clock), so a test on `FakeClock` moves it by advancing
//! the clock and never sleeps for real.

use std::time::Duration;

use omuzaki_core::time::{Clock, Seconds};

/// How often a wait looks at the clock and at its stop condition. Real time, so a wait on a clock that is not moving
/// does not spin.
const STEP: Duration = Duration::from_millis(1);

/// Waits until `clock` reaches `until`, or `stop` returns true. Returns whether the time was reached.
pub(crate) fn until(clock: &dyn Clock, until: Seconds, mut stop: impl FnMut() -> bool) -> bool {
    loop {
        if clock.now().0 >= until.0 {
            return true;
        }
        if stop() {
            return false;
        }
        std::thread::sleep(STEP);
    }
}

/// Waits `seconds` on `clock`, or until `stop` returns true. Returns whether the time passed.
pub(crate) fn for_seconds(clock: &dyn Clock, seconds: f64, stop: impl FnMut() -> bool) -> bool {
    until(clock, Seconds(clock.now().0 + seconds), stop)
}

/// One step of real time, for loops that poll a channel or a flag.
pub(crate) fn step() {
    std::thread::sleep(STEP);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use omuzaki_core::time::FakeClock;

    use super::*;

    #[test]
    fn a_wait_ends_when_the_clock_is_advanced() {
        let clock = Arc::new(FakeClock::default());
        let mover = {
            let clock = Arc::clone(&clock);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                clock.advance(Seconds(0.25));
            })
        };
        assert!(for_seconds(clock.as_ref(), 0.25, || false));
        mover.join().unwrap();
    }

    #[test]
    fn a_wait_ends_early_when_told_to_stop() {
        let clock = FakeClock::default();
        assert!(!for_seconds(&clock, 10.0, || true));
    }
}
