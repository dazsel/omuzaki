//! Contract for the write-ahead journal and project snapshots (spec v3 §6, §7, §29; ADR 0021; design in
//! `docs/design/journal.md`).
//!
//! The executor (`omuzaki-ops`) hands every change it makes to a [`JournalSink`] as a [`HistoryEntry`], under its lock,
//! in history order. The sink is implemented by `omuzaki-doc`, which writes the entries ahead of the project files and
//! replays them after a crash. A snapshot carries the executor's [`HistoryExport`], so undo and redo survive a
//! restart. Ops reach snapshots through the [`Snapshots`] service. This crate holds only the types and traits:
//! `omuzaki-history-mock` has in-memory stand-ins.
//!
//! Projects keep regenerable caches and renders out of snapshots by declaring them with [`register_derived_path!`]
//! next to the code that writes them.
#![deny(missing_docs)]

mod derived;
mod entry;
mod export;
mod sink;
mod snapshot;
mod wall;

pub use derived::{DerivedPath, derived_paths, is_derived, matches_glob};
pub use entry::{HistoryEntry, Lsn, Stack};
pub use export::{ActorHistory, HISTORY_EXPORT_FORMAT, HistoryExport, OpenTxExport, StepExport};
pub use sink::{JournalSink, JournalStatus, SyncError};
pub use snapshot::{
    Footprint, SnapshotError, SnapshotId, SnapshotInfo, Snapshots, Trigger, Usage, Verification, VerifyHow,
};
pub use wall::{WallClock, WallTime};

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
