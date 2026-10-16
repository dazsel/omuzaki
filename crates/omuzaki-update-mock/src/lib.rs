//! Deterministic stand-ins for `omuzaki-update-api` (SE-08): the test oracle that `omuzaki-update`, the launcher and
//! the apps test against. Nothing here touches the network, the real disk or the system clock.
//!
//! | Type | Stands in for |
//! |---|---|
//! | [`updater::MockUpdater`] | the `Updater`: scripted results, a real hold registry, events |
//! | [`server::FakeServer`] | the feed's HTTPS client (`Fetcher`), with faults on cue |
//! | [`disk::FakeDisk`] | the install folder's `Disk`, failing on cue and losing unsynced files on `revive` |
//! | [`clock::FakeWallClock`] | the calendar clock, set by the test |
#![deny(missing_docs)]

pub mod clock;
pub mod disk;
pub mod server;
pub mod updater;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `mutex`. A panic in another test thread leaves plain data behind, so a poisoned lock is still usable.
pub(crate) fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
