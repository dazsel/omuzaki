//! The system clock as a [`WallClock`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use omuzaki_update_api::clock::{Timestamp, WallClock};

/// Reads [`SystemTime`].
#[derive(Copy, Clone, Debug, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    /// Unix seconds, rounded down: a clock set before 1970 reads as negative (half a second before the epoch is `-1`),
    /// and one past `i64::MAX` seconds saturates.
    fn now(&self) -> Timestamp {
        Timestamp(unix_seconds(SystemTime::now().duration_since(UNIX_EPOCH).map_err(|before| before.duration())))
    }
}

/// Whole seconds since the epoch, floored. `Err` carries how long before the epoch the time is.
fn unix_seconds(offset: Result<Duration, Duration>) -> i64 {
    match offset {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => {
            // Flooring a negative time: any fraction of a second takes it one second further back.
            let whole = i64::try_from(before.as_secs()).ok();
            let floored = whole.and_then(|s| s.checked_add(i64::from(before.subsec_nanos() > 0)));
            floored.map_or(i64::MIN, |s| -s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_after_the_epoch_drop_the_fraction() {
        assert_eq!(unix_seconds(Ok(Duration::from_millis(1_999))), 1);
        assert_eq!(unix_seconds(Ok(Duration::ZERO)), 0);
    }

    #[test]
    fn times_before_the_epoch_floor_towards_the_past() {
        assert_eq!(unix_seconds(Err(Duration::from_millis(500))), -1);
        assert_eq!(unix_seconds(Err(Duration::from_secs(1))), -1);
        assert_eq!(unix_seconds(Err(Duration::from_millis(1_500))), -2);
        assert_eq!(unix_seconds(Err(Duration::from_secs(2))), -2);
    }

    #[test]
    fn out_of_range_times_saturate() {
        assert_eq!(unix_seconds(Ok(Duration::from_secs(u64::MAX))), i64::MAX);
        assert_eq!(unix_seconds(Err(Duration::from_secs(u64::MAX))), i64::MIN);
    }
}
