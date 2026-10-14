//! Wall-clock time for persisted facts, apart from the runner's session clock.

use omuzaki_core::time::Seconds;
use serde::{Deserialize, Serialize};

/// A point in wall time: UTC Unix milliseconds. A plain number, so it needs no date crate and sorts and compares
/// directly; the UI formats it for display.
///
/// The runner's `Seconds` is a session clock and means nothing across sessions, so anything persisted that must be
/// compared across sessions (when an actor was last seen, when a snapshot was taken) uses this.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WallTime(pub u64);

impl WallTime {
    /// This time plus `at` seconds of the runner's session clock, rounded to the millisecond: the wall time of an
    /// entry whose `at` was taken `at` seconds after the session's zero. `None` when `at` is negative or not finite
    /// (the journal never holds such a time) or the sum overflows.
    ///
    /// ```
    /// use omuzaki_core::time::Seconds;
    /// use omuzaki_history_api::WallTime;
    ///
    /// let started = WallTime(1_760_000_000_000); // from the session's `HistoryEntry::Session`
    /// assert_eq!(started.after(Seconds(2.5)), Some(WallTime(1_760_000_002_500)));
    /// assert_eq!(started.after(Seconds(f64::NAN)), None);
    /// ```
    pub fn after(self, at: Seconds) -> Option<WallTime> {
        if !at.0.is_finite() || at.0 < 0.0 {
            return None;
        }
        let ms = (at.0 * 1000.0).round();
        // 2^64 is the first float past `u64`; the multiplication itself can also overflow to infinity.
        if !ms.is_finite() || ms >= 18_446_744_073_709_551_616.0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ms = ms as u64;
        self.0.checked_add(ms).map(WallTime)
    }
}

/// Where wall time comes from. Injected, so tests use a fake clock and stay deterministic.
pub trait WallClock: Send + Sync {
    /// The current wall time.
    fn now(&self) -> WallTime;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_outside_the_u64_millisecond_range_are_refused_not_saturated() {
        assert_eq!(WallTime(0).after(Seconds(1e20)), None, "huge but finite");
        assert_eq!(WallTime(0).after(Seconds(f64::MAX)), None, "the multiplication overflows to infinity");
        assert_eq!(WallTime(0).after(Seconds(-1.0)), None, "negative");
        assert_eq!(WallTime(u64::MAX).after(Seconds(0.001)), None, "the addition overflows");
        assert_eq!(WallTime(5).after(Seconds(0.0)), Some(WallTime(5)));
    }
}
