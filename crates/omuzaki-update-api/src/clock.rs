//! Wall-clock time. `omuzaki_core::Clock` is monotonic; a manifest's expiry needs the calendar.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Unix seconds, UTC.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

/// The calendar time. `omuzaki-update-fs` reads the system clock; tests use a settable fake.
pub trait WallClock: Send + Sync {
    /// Now, in unix seconds.
    fn now(&self) -> Timestamp;
}
