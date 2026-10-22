//! In-memory stand-ins for the history contract (spec v3 §39 rule 6): a [`RecordingSink`] that keeps what the
//! executor appends, and [`MemorySnapshots`], a snapshot service over a plain document value. Both are scriptable, so
//! tests can make the journal slow, full or failing.
#![deny(missing_docs)]

pub mod branch;
mod clock;
mod log;
mod sink;
mod snapshots;

pub use clock::{FakeWallClock, SystemWallClock};
pub use log::MemoryHistoryLog;
pub use sink::RecordingSink;
pub use snapshots::MemorySnapshots;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `m`, ignoring poisoning: a panicking test thread must not hide what the mock recorded.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
