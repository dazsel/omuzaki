//! The system clock as a [`WallClock`].

use std::time::{SystemTime, UNIX_EPOCH};

use omuzaki_update_api::clock::{Timestamp, WallClock};

/// Reads [`SystemTime`].
#[derive(Copy, Clone, Debug, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    /// Unix seconds. A clock set before 1970 reads as negative; one past `i64::MAX` seconds saturates.
    fn now(&self) -> Timestamp {
        Timestamp(match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
            Err(before) => i64::try_from(before.duration().as_secs()).map_or(i64::MIN, |s| -s),
        })
    }
}
