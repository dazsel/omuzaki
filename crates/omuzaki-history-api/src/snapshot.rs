//! Snapshots: the content of the project folder at one journal position, and the service ops reach them through
//! (design sections 7 and 10).

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use omuzaki_doc_api::{Patch, Value};
use omuzaki_ops_api::Service;
use omuzaki_store_api::{ContentHash, HashParseError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::branch::BranchId;
use crate::state::StateRef;
use crate::{Lsn, WallTime};

/// The id of a snapshot: the BLAKE3 hash of its manifest, so identical content always has the same id. Several refs
/// may share one id.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
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
    /// The state a recording or replay save pins (ADR 0052). Takes its stem through `Snapshots::take`, which stores it
    /// in [`SnapshotInfo::stem`].
    Take,
    /// A point on a branch (VC-02): the state a fork, checkpoint, review request, switch or lane close records. The
    /// snapshot's ref names the branch ([`SnapshotInfo::branch`]).
    Point,
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
    /// The take's stem; take snapshots only (ADR 0052). Kept apart from `name`, because a name pins a named version
    /// (VC-01) and a take's pin is its own.
    #[serde(default)]
    pub stem: Option<String>,
    /// A pin: retention keeps the snapshot until this time. Absent: retention decides.
    #[serde(default)]
    pub keep_until: Option<WallTime>,
    /// The latest check, if it has been checked.
    #[serde(default)]
    pub verified: Option<Verification>,
    /// The highest job id the journal had recorded at `lsn`, so a reopen numbers new jobs after it and never reuses
    /// an id (ADR 0036 decision 10). 0: none, or a ref written before jobs were counted.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_job: u64,
    /// The jobs that had started and not ended at `lsn`. A reopen marks those that end nowhere in the journal after it
    /// as interrupted (journal.md 8.3): a job is never re-run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_jobs: Vec<RunningJob>,
    /// The branch whose lane took the snapshot (VC-02). Refs written before branches existed read as `main`.
    #[serde(default)]
    pub branch: BranchId,
}

/// A job that was running when a snapshot was taken.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunningJob {
    /// The job's id.
    pub job: u64,
    /// The op it runs.
    pub op: String,
}

impl RunningJob {
    /// Job `job`, running `op`.
    pub fn new(job: u64, op: impl Into<String>) -> Self {
        Self { job, op: op.into() }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if passes a reference")]
fn is_zero(n: &u64) -> bool {
    *n == 0
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
            stem: None,
            keep_until: None,
            verified: None,
            last_job: 0,
            running_jobs: Vec::new(),
            branch: BranchId::main(),
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

    /// Takes a snapshot now and returns its id. `name` is the version's name for [`Trigger::Named`] (required there),
    /// and the take's stem for [`Trigger::Take`] (optional; stored in [`SnapshotInfo::stem`]). No other trigger takes
    /// a value.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Invalid`] for a missing name on `Named`, or a value on any other trigger,
    /// [`SnapshotError::Io`] when storing fails.
    fn take(&self, trigger: Trigger, name: Option<&str>) -> Result<SnapshotId, SnapshotError>;

    /// Takes a snapshot of any `state` of this lane's branch, not only the current one (named versions of past
    /// transactions, branching from history; design 9), and returns its id. `trigger` and `name` follow
    /// [`take`](Self::take).
    ///
    /// The default refuses, so services written before branches keep compiling; `take(trigger, name)` is
    /// `take_at(&StateRef::Now, trigger, name)` for those that implement it.
    ///
    /// # Errors
    /// [`SnapshotError::Invalid`] for a name or trigger that does not fit, or a state this service cannot resolve;
    /// [`SnapshotError::NotFound`] or [`SnapshotError::Corrupt`] when what `state` names is missing or damaged;
    /// [`SnapshotError::Io`] when storing fails.
    fn take_at(&self, state: &StateRef, trigger: Trigger, name: Option<&str>) -> Result<SnapshotId, SnapshotError> {
        let _ = (state, trigger, name);
        Err(SnapshotError::Invalid(
            "this snapshot service cannot snapshot a state other than the current one".to_owned(),
        ))
    }

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

    /// What `project.reload` takes in: with no `paths`, every file changed outside Omuzaki since it was last read or
    /// written; otherwise those files. Says which files they are and which of them the document also has unsaved
    /// changes in (clashes, which the reload replaces and reports, never merges). The default reads
    /// [`reload_patch`](Self::reload_patch) and names no files.
    ///
    /// # Errors
    /// As [`reload_patch`](Self::reload_patch).
    fn reload(&self, paths: &[String]) -> Result<Reload, SnapshotError> {
        self.reload_patch(paths).map(Reload::new)
    }

    /// Bytes and counts of the journal, the archive and the snapshot store.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Io`] when the folders cannot be read.
    fn usage(&self) -> Result<Usage, SnapshotError>;
}

/// What [`Snapshots::reload`] found.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Reload {
    /// From the document to the files; the op fills each `before` from the document it plans against.
    pub patch: Patch,
    /// The files it reads, relative to the project folder.
    pub files: Vec<String>,
    /// Those the document also has unsaved changes in.
    pub clashes: Vec<String>,
}

impl Reload {
    /// A reload of `patch`, naming no files.
    pub fn new(patch: Patch) -> Self {
        Self { patch, ..Self::default() }
    }

    /// This reload, naming `files` and the `clashes` among them.
    #[must_use]
    pub fn with_files(mut self, files: Vec<String>, clashes: Vec<String>) -> Self {
        self.files = files;
        self.clashes = clashes;
        self
    }
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
    fn a_take_trigger_round_trips_as_take() {
        let text = serde_json::to_string(&Trigger::Take).unwrap_or_default();
        assert_eq!(text, "\"take\"");
        let back: Result<Trigger, _> = serde_json::from_str(&text);
        assert_eq!(back.ok(), Some(Trigger::Take));
    }

    #[test]
    fn a_ref_missing_optional_facts_still_reads() {
        let info = SnapshotInfo::new(id(), Lsn(5), "s", WallTime(1_760_279_525_000), Trigger::Auto);
        let mut json = serde_json::to_value(&info).unwrap_or_default();
        for key in ["label", "history", "thumbnail", "name", "stem", "keep_until", "verified", "branch"] {
            if let Some(map) = json.as_object_mut() {
                map.remove(key);
            }
        }
        let back: Result<SnapshotInfo, _> = serde_json::from_value(json);
        assert_eq!(back.ok(), Some(info));
    }

    #[test]
    fn a_ref_from_before_branches_reads_as_main_and_the_point_trigger_is_kebab_case() {
        let info = SnapshotInfo::new(id(), Lsn(5), "s", WallTime(1), Trigger::Point);
        assert!(info.branch.is_main());
        let text = serde_json::to_string(&info.trigger).unwrap_or_default();
        assert_eq!(text, "\"point\"");
        let mut json = serde_json::to_value(&info).unwrap_or_default();
        assert_eq!(json["branch"], "main");
        if let Some(map) = json.as_object_mut() {
            map.remove("branch");
        }
        let back: Result<SnapshotInfo, _> = serde_json::from_value(json);
        assert_eq!(back.ok(), Some(info));
    }

    /// A service written before branches: it implements only what the trait required then.
    struct Old;

    impl Snapshots for Old {
        fn list(&self) -> Result<Vec<SnapshotInfo>, SnapshotError> {
            Ok(Vec::new())
        }
        fn take(&self, _: Trigger, _: Option<&str>) -> Result<SnapshotId, SnapshotError> {
            Ok(id())
        }
        fn tree(&self, _: &SnapshotId) -> Result<Value, SnapshotError> {
            Ok(Value::Null)
        }
        fn restore_outside_tree(&self, _: &SnapshotId) -> Result<(), SnapshotError> {
            Ok(())
        }
        fn save_files(&self, _: Duration) -> Result<(), SnapshotError> {
            Ok(())
        }
        fn reload_patch(&self, _: &[String]) -> Result<Patch, SnapshotError> {
            Ok(Patch::default())
        }
        fn usage(&self) -> Result<Usage, SnapshotError> {
            Ok(Usage::default())
        }
    }

    #[test]
    fn take_at_refuses_by_default_so_older_services_keep_compiling() {
        let refused = Old.take_at(&StateRef::Now, Trigger::Point, None);
        assert!(matches!(refused, Err(SnapshotError::Invalid(_))), "{refused:?}");
    }

    #[test]
    fn the_id_displays_and_parses_as_the_hash() {
        let shown = id().to_string();
        assert_eq!(shown.len(), 64);
        assert_eq!(shown.parse::<SnapshotId>(), Ok(id()));
    }
}
