//! The settings store (SE-10, `docs/design/settings.md` D3, D5, D6, ADR 0026).
//!
//! [`Store`] implements the `Settings` service of `omuzaki-settings-api` over two TOML files:
//! `settings.toml` (the `User` keys, in the config folder, which may roam) and `machine.toml` (the `Machine` keys, in
//! the local data folder). [`Paths`] says where; tests pass their own folders.
//!
//! | Module | Holds |
//! |---|---|
//! | `paths` | [`Paths`]: the two folders, from `directories` or `OMUZAKI_CONFIG_DIR` and `OMUZAKI_DATA_DIR` |
//! | `store` | [`Store`]: load, snapshot, events, the change log and the debounced writer thread |
//! | `backend` | the private `Backend` trait and the file backend: lock, merge, atomic replace, quarantine |
//! | `document` | reading keys out of a file and merging edits into its text without touching anything else |
//! | `ephemeral` | session state in memory until the session file store lands |
//! | [`ops`] | the `settings.*` ops over any `Settings` service (#767) |
//!
//! # Choices a reviewer may disagree with
//!
//! - **Start never blocks and never fails.** A file that cannot be read or parsed is a finding and its keys read as
//!   defaults; an unparsable file is renamed `<name>.broken-<utc>` so the next write starts clean.
//! - **A bad value stays in the file.** The key reads as its default and a finding is listed; only a write of that key
//!   replaces it. A value stored at a newer version than this build knows is kept the same way.
//! - **Migrations run in memory.** A migrated value is written back only when its key is next written, and the first
//!   write after a migration copies the file to `<name>.bak-<app version>` (once per version).
//! - **Stale temporary files are removed by age**, and only those named for the file being written, not by asking
//!   whether the owning process is alive, which std cannot do portably. Holding the file lock already proves no live
//!   writer of this file exists; another file's writer in the folder may only be slow.
//! - **Profiles and session state are not stored here yet.** No profile exists and `session()` is in memory.

mod backend;
mod document;
mod ephemeral;
pub mod ops;
mod paths;
mod store;
#[cfg(test)]
mod tests;

pub use paths::{CONFIG_DIR_VAR, DATA_DIR_VAR, Paths};
pub use store::{DEBOUNCE, MAX_DELAY, Store};

use std::sync::{Mutex, MutexGuard, PoisonError};

omuzaki_core::define_link!();

/// Locks `mutex`, going on after a panic elsewhere: settings state is replaced whole, never left half-written.
pub(crate) fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
