//! The small record stored beside a blob: the `(size, mtime)` it had when it was verified. A later `get` that finds
//! the same size and mtime trusts the blob without hashing it again; anything else is re-hashed (ADR 0017).

use std::fs::{self, Metadata};
use std::path::Path;
use std::time::UNIX_EPOCH;

/// What a blob looked like when we verified and stored it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct Record {
    size: u64,
    mtime_nanos: u128,
}

impl Record {
    /// The record of a file as it is now.
    pub(super) fn of(meta: &Metadata) -> Self {
        // A clock before 1970 or an OS without mtimes gives 0, which still compares equal to itself.
        let mtime_nanos =
            meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
        Self { size: meta.len(), mtime_nanos }
    }

    /// The text form: `<size> <mtime nanoseconds>`.
    pub(super) fn to_text(self) -> String {
        format!("{} {}\n", self.size, self.mtime_nanos)
    }

    /// Reads the record at `path`; `None` if it is missing or not a record (then the blob is simply re-hashed).
    pub(super) fn read(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        let (size, mtime) = text.trim().split_once(' ')?;
        Some(Self { size: size.parse().ok()?, mtime_nanos: mtime.parse().ok()? })
    }
}
