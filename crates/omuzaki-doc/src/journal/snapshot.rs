//! Snapshots, first form: the whole document tree and the executor's history at one LSN, as one JSON file.
//!
//! Recovery starts from the newest snapshot and replays the journal after it (journal.md 8.3). The design's snapshot
//! (section 7: a manifest of file hashes and packs in a content-addressed store, with refs) is what backups, named
//! versions and retention need; this form gives recovery and persisted history the same guarantees now and is
//! replaced by it without a journal change, because a session record names its base only by id and LSN.
//!
//! - **Where:** `.omuzaki/snapshots/<lsn, 20 digits>.json`, so name order is LSN order.
//! - **Atomic:** written with [`write_atomic`] (a temporary file, flushed, renamed, folder flushed), so a snapshot is
//!   either whole or absent. A file that doesn't read is skipped by [`newest`] and the one before it is used.
//! - **Id:** the BLAKE3 hash of the file's bytes, which is what a `close` record names.
//! - **Kept:** the newest [`KEEP`]; older ones are deleted once a newer one is durable.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use omuzaki_core::fsx::write_atomic;
use omuzaki_history_api::{HistoryExport, Lsn, SnapshotId, Trigger, WallTime};
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::JournalError;

/// The folder inside `.omuzaki/` that holds snapshots.
pub const SNAPSHOT_DIR: &str = "snapshots";

/// The layout version of a snapshot file. A reader skips a newer one rather than misread it.
pub const SNAPSHOT_FORMAT: u32 = 1;

/// How many snapshots are kept. More than one, so a damaged newest snapshot still leaves one to recover from.
pub const KEEP: usize = 3;

/// The digits of the LSN in a snapshot's file name: enough for any `u64`, so name order is LSN order.
const LSN_DIGITS: usize = 20;

/// The file name suffix.
const SUFFIX: &str = ".json";

/// One snapshot: the document and the history at `lsn`, the journal position it covers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Snapshot {
    /// [`SNAPSHOT_FORMAT`] when written.
    pub format: u32,
    /// The last journal record it covers: recovery replays the records after it.
    pub lsn: Lsn,
    /// The session it was taken in.
    pub session: String,
    /// Why it was taken.
    pub trigger: Trigger,
    /// When, in wall time.
    pub created: WallTime,
    /// The whole document tree.
    pub root: Value,
    /// The executor's history at `lsn`: undo and redo survive a restart through it.
    pub history: HistoryExport,
}

impl Snapshot {
    /// A snapshot of `root` and `history` at `lsn`.
    pub fn new(
        lsn: Lsn,
        session: impl Into<String>,
        trigger: Trigger,
        created: WallTime,
        root: Value,
        history: HistoryExport,
    ) -> Self {
        Self { format: SNAPSHOT_FORMAT, lsn, session: session.into(), trigger, created, root, history }
    }
}

/// The snapshot folder of the project in `project_dir`.
pub fn snapshot_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".omuzaki").join(SNAPSHOT_DIR)
}

/// Writes `snapshot` durably into `dir` and returns its id, then deletes all but the newest [`KEEP`].
///
/// # Errors
/// [`JournalError::Io`] if it can't be written, [`JournalError::Unencodable`] if it doesn't encode (a value JSON can't
/// hold). Pruning failures are not errors: an old snapshot left behind costs only space.
pub fn write(dir: &Path, snapshot: &Snapshot) -> Result<SnapshotId, JournalError> {
    let text = serde_json::to_string(snapshot)
        .map_err(|e| JournalError::Unencodable { lsn: snapshot.lsn.0, reason: e.to_string() })?;
    let path = dir.join(file_name(snapshot.lsn));
    write_atomic(&path, &text).map_err(|source| JournalError::Io { path: path.clone(), source })?;
    prune(dir);
    Ok(SnapshotId(ContentHash::of(text.as_bytes())))
}

/// The newest snapshot in `dir` that reads, with its id, or `None` if there is none. Files that don't read (damaged,
/// or written by a newer build) are skipped, so recovery falls back to the one before.
///
/// # Errors
/// [`JournalError::Io`] if the folder exists but can't be listed.
pub fn newest(dir: &Path) -> Result<Option<(SnapshotId, Snapshot)>, JournalError> {
    for path in list(dir)?.into_iter().rev() {
        let Ok(bytes) = fs::read(&path) else { continue };
        match serde_json::from_slice::<Snapshot>(&bytes) {
            Ok(snapshot) if snapshot.format <= SNAPSHOT_FORMAT => {
                return Ok(Some((SnapshotId(ContentHash::of(&bytes)), snapshot)));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// The snapshot files in `dir`, oldest first. A missing folder holds none.
fn list(dir: &Path) -> Result<Vec<PathBuf>, JournalError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(JournalError::Io { path: dir.to_path_buf(), source }),
    };
    let mut found: Vec<(u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            lsn_of(name.to_str()?).map(|lsn| (lsn, entry.path()))
        })
        .collect();
    found.sort();
    Ok(found.into_iter().map(|(_, path)| path).collect())
}

/// Deletes all but the newest [`KEEP`] snapshots, best effort.
fn prune(dir: &Path) {
    let Ok(all) = list(dir) else { return };
    for path in all.iter().take(all.len().saturating_sub(KEEP)) {
        // An old snapshot that can't be deleted only costs space; the next write tries again.
        drop(fs::remove_file(path));
    }
}

fn file_name(lsn: Lsn) -> String {
    format!("{:0width$}{SUFFIX}", lsn.0, width = LSN_DIGITS)
}

fn lsn_of(name: &str) -> Option<u64> {
    let digits = name.strip_suffix(SUFFIX)?;
    (digits.len() == LSN_DIGITS && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok()).flatten()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn snapshot(lsn: u64) -> Snapshot {
        Snapshot::new(
            Lsn(lsn),
            "s",
            Trigger::Auto,
            WallTime(1),
            json!({"project": {"name": "x"}}),
            HistoryExport::empty(),
        )
    }

    #[test]
    fn the_newest_readable_snapshot_wins_and_only_the_newest_few_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("snapshots");
        assert_eq!(newest(&dir).unwrap(), None, "no folder, no snapshot");
        let mut ids = Vec::new();
        for lsn in [3, 10, 25, 40] {
            ids.push(write(&dir, &snapshot(lsn)).unwrap());
        }
        assert_eq!(list(&dir).unwrap().len(), KEEP);
        let (id, found) = newest(&dir).unwrap().unwrap();
        assert_eq!((id, found.lsn), (ids[3], Lsn(40)));

        fs::write(dir.join(file_name(Lsn(40))), b"{ torn").unwrap();
        assert_eq!(newest(&dir).unwrap().unwrap().1.lsn, Lsn(25), "a damaged newest falls back to the one before");
    }

    #[test]
    fn names_sort_in_lsn_order_and_only_snapshot_names_count() {
        assert_eq!(lsn_of(&file_name(Lsn(7))), Some(7));
        assert_eq!(lsn_of(&file_name(Lsn(u64::MAX))), Some(u64::MAX));
        assert!(file_name(Lsn(9)) < file_name(Lsn(10)));
        for other in ["7.json", "00000000000000000007.toml", "0000000000000000000x.json", ".tmp"] {
            assert_eq!(lsn_of(other), None, "{other}");
        }
    }
}
