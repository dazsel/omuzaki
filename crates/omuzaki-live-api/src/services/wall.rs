//! Wall time: [`WallClock`], which a node reads through `cx.utc()` and a schedule test fakes.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::UtcMillis;

/// A source of wall time. The engine reads it once per step, so every node in a step sees the same instant.
pub trait WallClock: Send + Sync {
    /// The current wall time.
    fn now(&self) -> UtcMillis;
}

/// The OS clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> UtcMillis {
        let millis = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
            // A clock set before 1970: negative, as far before the epoch as it is.
            Err(before) => i64::try_from(before.duration().as_millis()).map_or(i64::MIN, |m| -m),
        };
        UtcMillis(millis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_is_after_2020() {
        assert!(SystemWallClock.now() > UtcMillis(1_577_836_800_000));
    }
}
