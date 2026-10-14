//! A monotonic tick clock that means the same thing in every process on the machine, so the sender can stamp a frame
//! and a different process can measure how long ago that was.
//!
//! Windows: `QueryPerformanceCounter`, which Microsoft documents as consistent across processes. Elsewhere (so the pure
//! logic still tests on Linux) a process-local `Instant`, which is not comparable between processes.

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

    pub fn frequency() -> u64 {
        let mut hz = 0i64;
        // SAFETY: `hz` is a valid, writable i64 for the duration of the call.
        // The call cannot fail on Windows XP and later; on failure `hz` stays 0 and `Clock::new` substitutes 1.
        let _ = unsafe { QueryPerformanceFrequency(&raw mut hz) };
        u64::try_from(hz).unwrap_or(0)
    }

    pub fn now() -> u64 {
        let mut ticks = 0i64;
        // SAFETY: `ticks` is a valid, writable i64 for the duration of the call.
        let _ = unsafe { QueryPerformanceCounter(&raw mut ticks) };
        u64::try_from(ticks).unwrap_or(0)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::OnceLock;
    use std::time::Instant;

    fn origin() -> Instant {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        *ORIGIN.get_or_init(Instant::now)
    }

    pub fn frequency() -> u64 {
        1_000_000_000
    }

    pub fn now() -> u64 {
        u64::try_from(origin().elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// Reads the tick counter and converts ticks to milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    hz: u64,
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock {
    /// Reads the counter frequency once.
    #[must_use]
    pub fn new() -> Self {
        Self { hz: imp::frequency().max(1) }
    }

    /// Ticks now.
    #[must_use]
    pub fn now(&self) -> u64 {
        imp::now()
    }

    /// Milliseconds from `from` to `to`; negative when `to` is earlier (clock skew between processes would show here).
    #[must_use]
    pub fn ms_between(&self, from: u64, to: u64) -> f64 {
        #[allow(clippy::cast_precision_loss)] // tick deltas are far below 2^53
        let delta = if to >= from { (to - from) as f64 } else { -((from - to) as f64) };
        #[allow(clippy::cast_precision_loss)]
        let hz = self.hz as f64;
        delta * 1000.0 / hz
    }

    /// Ticks in `ms` milliseconds.
    #[must_use]
    pub fn ticks_in_ms(&self, ms: f64) -> u64 {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ticks = (ms * self.hz as f64 / 1000.0).max(0.0) as u64;
        ticks
    }

    /// Sleeps until `deadline` ticks: a coarse sleep, then a short spin for the last stretch.
    pub fn sleep_until(&self, deadline: u64) {
        const SPIN_MS: f64 = 1.5;
        loop {
            let now = self.now();
            if now >= deadline {
                return;
            }
            let left = self.ms_between(now, deadline);
            if left > SPIN_MS {
                std::thread::sleep(std::time::Duration::from_secs_f64((left - SPIN_MS) / 1000.0));
            } else {
                std::hint::spin_loop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_moves_forward_and_converts() {
        let clock = Clock::new();
        let a = clock.now();
        clock.sleep_until(a + clock.ticks_in_ms(5.0));
        let b = clock.now();
        let ms = clock.ms_between(a, b);
        assert!((5.0..50.0).contains(&ms), "slept {ms} ms");
        assert!(clock.ms_between(b, a) < 0.0);
    }
}
