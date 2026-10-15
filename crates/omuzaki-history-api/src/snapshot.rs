//! Snapshots: the content of the project folder at one journal position, and the service ops reach them through
//! (design sections 7 and 10).

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use omuzaki_doc_api::{Patch, Value};
use omuzaki_ops_api::Service;
use omuzaki_store_api::{ContentHash, HashParseError};
use serde::{Deserialize, Serialize};

use crate::{Lsn, WallTime};

/// The id of a snapshot: the BLAKE3 hash of its manifest, so identical content always has the same id. Several refs
/// may share one id.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotId(pub ContentHash);

impl fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for SnapshotId {
    type Err = HashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

/// Why a snapshot was taken. Serialised as the ref's `trigger` string (`pre-migration`).
///
/// Open to new variants: matching code needs a wildcard arm.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Trigger {
    /// The files as opened, when a session starts.
    Session,
    /// The editing cadence: every 10 minutes or 5,000 records.
    Auto,
    /// The final state of a clean close.
    Close,
    /// The raw files, before a migration.
    PreMigration,
    /// The current state, before a backup restore replaces anything.
    PreRestore,
    /// The raw files, before crash recovery writes anything.
    PreRecovery,
    /// Before a bulk agent session (`backup.snapshot`).
    PreAgentSession,
    /// Requested by the person or a script, without a name.
    Manual,
    /// A named version (VC-01), pinned until the name is removed.
    Named,
    /// The state a recording or replay save pins (ADR 0052).
    Take,
}

/// How a snapshot was checked.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum VerifyHow {
    /// Every object the manifest names exists and hashes right.
    Hash,
    /// A project loaded from the manifest in memory, running the migrations as a dry run (VC-09).
    TrialOpen,
}

/// The result of checking a snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Verification {
    /// When.
    pub at: WallTime,
    /// How.
    pub how: VerifyHow,
    /// Whether it passed.
    pub ok: bool,
}

/// The facts of one snapshot ref (design section 7.1).
///
/// Non-exhaustive so later refs can add fields (branches) without breaking implementations: build one with
/// [`SnapshotInfo::new`] and set the optional fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SnapshotInfo {
    /// The manifest's hash.
    pub id: SnapshotId,
    /// The journal position it represents: the persistent document version, since `DocVersion` restarts at 0 whenever
    /// a project opens.
    pub lsn: Lsn,
    /// The session it was taken in.
    pub session: String,
    /// When.
    pub created: WallTime,
    /// Why.
    pub trigger: Trigger,
    /// The last transaction's label.
    #[serde(default)]
    pub label: String,
    /// The history export at `lsn` (local only; not mirrored to a second location by default).
    #[serde(default)]
    pub history: Option<ContentHash>,
    /// The last transaction's thumbnail, for the restore preview.
    #[serde(default)]
    pub thumbnail: Option<ContentHash>,
    /// The version's name; named versions only.
    #[serde(default)]
    pub name: Option<String>,
    /// A pin: retention keeps the snapshot until this time. Absent: retention decides.
    #[serde(default)]
    pub keep_until: Option<WallTime>,
    /// The latest check, if it has been checked.
    #[serde(default)]
    pub verified: Option<Verification>,
}

impl SnapshotInfo {
    /// A ref with the mandatory facts; the optional ones start empty.
    pub fn new(id: SnapshotId, lsn: Lsn, session: impl Into<String>, created: WallTime, trigger: Trigger) -> Self {
        Self {
            id,
            lsn,
            session: session.into(),
            created,
            trigger,
            label: String::new(),
            history: None,
            thumbnail: None,
            name: None,
            keep_until: None,
            verified: None,
        }
    }
}

/// Why a snapshot call failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// No ref names this snapshot.
    #[error("no snapshot {0}")]
    NotFound(SnapshotId),
    /// The snapshot's data is missing or does not hash right.
    #[error("snapshot {id} is damaged: {reason}")]
    Corrupt {
        /// Which.
        id: SnapshotId,
        /// What is wrong.
        reason: String,
    },
    /// A call argument is wrong (a path outside the project, an empty name).
    #[error("invalid request: {0}")]
    Invalid(String),
    /// Waiting for the files to be written took longer than the timeout.
    #[error("timed out waiting for the project files to be saved")]
    Timeout,
    /// The disk or the store failed.
    #[error("snapshot storage failed: {0}")]
    Io(String),
}

/// How much one kind of data takes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Footprint {
    /// Bytes on disk.
    pub bytes: u64,
    /// Files (journal, archive) or objects (snapshots).
    pub count: u64,
}

impl Verification {
    /// A check made at `at`.
    pub fn new(at: WallTime, how: VerifyHow, ok: bool) -> Self {
        Self { at, how, ok }
    }
}

impl Footprint {
    /// `bytes` in `count` files or objects.
    pub fn new(bytes: u64, count: u64) -> Self {
        Self { bytes, count }
    }
}

/// Disk use of a project's history, shown in Settings. Start from `Usage::default()` and set the parts.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Usage {
    /// The live journal segments.
    pub journal: Footprint,
    /// The sealed segments: the op log archive.
    pub archive: Footprint,
    /// The snapshot store.
    pub snapshots: Footprint,
}

/// The snapshot service, implemented by `omuzaki-doc`. The `backup.*` and `project.save` ops reach it through
/// `OpCtx::service` and are the only callers.
pub trait Snapshots: Service {
    /// Every snapshot ref, newest first.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Io`] when the refs cannot be read.
    fn list(&self) -> Result<Vec<SnapshotInfo>, SnapshotError>;

    /// Takes a snapshot now and returns its id. `name` is for [`Trigger::Named`].
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Invalid`] for a name on a trigger that takes none, [`SnapshotError::Io`] when storing fails.
    fn take(&self, trigger: Trigger, name: Option<&str>) -> Result<SnapshotId, SnapshotError>;

    /// The document as of snapshot `id`.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::NotFound`] or [`SnapshotError::Corrupt`].
    fn tree(&self, id: &SnapshotId) -> Result<Value, SnapshotError>;

    /// Writes the files of snapshot `id` that live outside the document tree (locks, takes) back into the project.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::NotFound`], [`SnapshotError::Corrupt`] or [`SnapshotError::Io`].
    fn restore_outside_tree(&self, id: &SnapshotId) -> Result<(), SnapshotError>;

    /// Waits until the project files match the document (`project.save`).
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Timeout`] when `timeout` passes first, [`SnapshotError::Io`] when writing fails.
    fn save_files(&self, timeout: Duration) -> Result<(), SnapshotError>;

    /// The patch that brings the document in line with the files at `paths` on disk (`project.reload`).
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Invalid`] for a path outside the project, [`SnapshotError::Io`] when a file cannot be read.
    fn reload_patch(&self, paths: &[String]) -> Result<Patch, SnapshotError>;

    /// Bytes and counts of the journal, the archive and the snapshot store.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Io`] when the folders cannot be read.
    fn usage(&self) -> Result<Usage, SnapshotError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> SnapshotId {
        SnapshotId(ContentHash::of(b"manifest"))
    }

    #[test]
    fn triggers_serialise_as_the_ref_strings() {
        let text = serde_json::to_string(&Trigger::PreAgentSession).unwrap_or_default();
        assert_eq!(text, "\"pre-agent-session\"");
    }

    #[test]
    fn the_take_trigger_round_trips_as_take() {
        let text = serde_json::to_string(&Trigger::Take).unwrap_or_default();
        assert_eq!(text, "\"take\"");
        let back: Result<Trigger, _> = serde_json::from_str(&text);
        assert_eq!(back.ok(), Some(Trigger::Take));
    }

    #[test]
    fn a_ref_missing_optional_facts_still_reads() {
        let info = SnapshotInfo::new(id(), Lsn(5), "s", WallTime(1_760_279_525_000), Trigger::Auto);
        let mut json = serde_json::to_value(&info).unwrap_or_default();
        for key in ["label", "history", "thumbnail", "name", "keep_until", "verified"] {
            if let Some(map) = json.as_object_mut() {
                map.remove(key);
            }
        }
        let back: Result<SnapshotInfo, _> = serde_json::from_value(json);
        assert_eq!(back.ok(), Some(info));
    }

    #[test]
    fn the_id_displays_and_parses_as_the_hash() {
        let shown = id().to_string();
        assert_eq!(shown.len(), 64);
        assert_eq!(shown.parse::<SnapshotId>(), Ok(id()));
    }
}
