//! The journal's records (journal.md 4.4 and 4.5): what a frame's payload holds, and the code that turns records into
//! frames and back.
//!
//! Every payload is one JSON object. A history entry is the [`HistoryEntry`] itself, tagged by `entry`; the tag must
//! agree with the frame's kind, so a frame whose kind and payload disagree reads as a bad payload, not as a record.
//!
//! Kinds 12 to 14 (`job_started`, `job_ended`, `tx_revert`) are not in the 4.4 table yet. The Runtime lead approved them
//! on #253 as required kinds, not skippable: replay must apply a `TxRevert`, so a reader that doesn't know them stops
//! with `TooNew` instead of skipping them.
//!
//! Kind 11, the audit record (#1058), is the one kind written [`SKIPPABLE`]: replay never needs it, so a reader that
//! predates it reads past it and replays the rest the same.

use omuzaki_core::time::Seconds;
use omuzaki_history_api::{HistoryEntry, Lsn, SnapshotId, WallTime};
use serde::{Deserialize, Serialize};

use super::{FORMAT_VERSION, Frame, JournalError, SKIPPABLE, write_frame};

/// The record kinds (4.4).
pub mod kind {
    /// A session starts.
    pub const SESSION: u8 = 1;
    /// A transaction opened.
    pub const BEGIN: u8 = 2;
    /// An op was applied.
    pub const OP: u8 = 3;
    /// A transaction was committed.
    pub const COMMIT: u8 = 4;
    /// A transaction was rolled back.
    pub const ROLLBACK: u8 = 5;
    /// A step was undone.
    pub const UNDO: u8 = 6;
    /// A step was redone.
    pub const REDO: u8 = 7;
    /// A step was dropped after a conflict.
    pub const DROPPED: u8 = 8;
    /// A clean shutdown, the last record of a session.
    pub const CLOSE: u8 = 9;
    /// Reserved for a branch switch (section 9); never written.
    pub const RESERVED_BRANCH: u8 = 10;
    /// The audit record (skippable), #1058's.
    pub const AUDIT: u8 = 11;
    /// A job started. Required: recovery must know of a job the crash interrupted.
    pub const JOB_STARTED: u8 = 12;
    /// A job ended. Required.
    pub const JOB_ENDED: u8 = 13;
    /// A joined driver's records left an open transaction. Required: replay must apply it.
    pub const TX_REVERT: u8 = 14;
}

/// What a journal record holds.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Record {
    /// Kind 1: a session starts.
    Session(SessionInfo),
    /// A history entry (kinds 2 to 8, and 12 to 14). Never `HistoryEntry::Session` (that is [`Record::Session`])
    /// and never `HistoryEntry::Mark` (never written).
    Entry(HistoryEntry),
    /// Kind 9: a clean shutdown, the last record of a session.
    Close {
        /// The snapshot taken at shutdown.
        snapshot: SnapshotId,
    },
}

/// The `session` record's payload (4.4 kind 1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SessionInfo {
    /// From the executor's `HistoryEntry::Session`.
    pub session: String,
    /// The wall time at runner-clock zero.
    pub started: WallTime,
    /// The version of the application that wrote the session.
    pub app_version: String,
    /// The document schema the session's patches are against.
    pub schema: u32,
    /// The base snapshot; `None` when the session starts from nothing.
    #[serde(default)]
    pub base: Option<SnapshotId>,
    /// The LSN the base snapshot covers.
    #[serde(default)]
    pub base_lsn: Lsn,
    /// The process id that wrote the session.
    #[serde(default)]
    pub pid: u32,
    /// The host that wrote the session.
    #[serde(default)]
    pub host: String,
}

impl SessionInfo {
    /// A session with no base snapshot and no process facts; add them with [`with_base`](Self::with_base) and
    /// [`with_process`](Self::with_process).
    #[must_use]
    pub fn new(session: String, started: WallTime, app_version: String, schema: u32) -> Self {
        Self { session, started, app_version, schema, base: None, base_lsn: Lsn(0), pid: 0, host: String::new() }
    }

    /// This session, based on `snapshot`, which covers the journal up to `lsn`.
    #[must_use]
    pub fn with_base(mut self, snapshot: SnapshotId, lsn: Lsn) -> Self {
        self.base = Some(snapshot);
        self.base_lsn = lsn;
        self
    }

    /// This session, written by process `pid` on `host`.
    #[must_use]
    pub fn with_process(mut self, pid: u32, host: String) -> Self {
        self.pid = pid;
        self.host = host;
        self
    }
}

/// The `close` payload.
#[derive(Serialize, Deserialize)]
struct ClosePayload {
    snapshot: SnapshotId,
}

/// The frame kind of a history entry; `None` for the two entries that are never a record of their own kind.
fn entry_kind(entry: &HistoryEntry) -> Option<u8> {
    Some(match entry {
        HistoryEntry::Begin { .. } => kind::BEGIN,
        HistoryEntry::Op { .. } => kind::OP,
        HistoryEntry::Commit { .. } => kind::COMMIT,
        HistoryEntry::Rollback { .. } => kind::ROLLBACK,
        HistoryEntry::Undo { .. } => kind::UNDO,
        HistoryEntry::Redo { .. } => kind::REDO,
        HistoryEntry::Dropped { .. } => kind::DROPPED,
        HistoryEntry::JobStarted { .. } => kind::JOB_STARTED,
        HistoryEntry::JobEnded { .. } => kind::JOB_ENDED,
        HistoryEntry::TxRevert { .. } => kind::TX_REVERT,
        HistoryEntry::Audit { .. } => kind::AUDIT,
        HistoryEntry::Session { .. } | HistoryEntry::Mark { .. } => return None,
    })
}

/// The runner-clock time an entry carries, which JSON would write as `null` if it were not finite.
fn entry_time(entry: &HistoryEntry) -> Option<Seconds> {
    match entry {
        HistoryEntry::Begin { at, .. } | HistoryEntry::JobStarted { at, .. } | HistoryEntry::Audit { at, .. } => {
            Some(*at)
        }
        HistoryEntry::Op { record, .. } => Some(record.at),
        _ => None,
    }
}

/// The deepest nesting of JSON arrays and objects a payload may have: 127, one less than the recursion limit of 128 that
/// `serde_json` enforces when it reads a document. `encode` refuses a deeper payload, so every record it accepts is
/// one [`decode`] reads back.
pub const MAX_DEPTH: usize = 127;

/// How deeply the JSON text `bytes` nests arrays and objects; 0 for a scalar. Brackets inside strings don't count.
fn json_depth(bytes: &[u8]) -> usize {
    let (mut depth, mut deepest, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for &b in bytes {
        if in_string {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// The kind and JSON payload of one record. The payload is checked to read back as the same record, so what `encode`
/// accepts is what [`decode`] returns.
fn payload_of(lsn: Lsn, record: &Record) -> Result<(u8, Vec<u8>), JournalError> {
    let unencodable = |reason: String| JournalError::Unencodable { lsn: lsn.0, reason };
    let json = |r: serde_json::Result<Vec<u8>>| r.map_err(|e| unencodable(format!("the payload doesn't encode: {e}")));
    let (k, payload) = match record {
        Record::Session(info) => (kind::SESSION, json(serde_json::to_vec(info))?),
        Record::Close { snapshot } => (kind::CLOSE, json(serde_json::to_vec(&ClosePayload { snapshot: *snapshot }))?),
        Record::Entry(entry) => {
            let Some(k) = entry_kind(entry) else {
                return Err(unencodable(
                    "a session entry is Record::Session, and a mark entry is never written".to_string(),
                ));
            };
            if entry_time(entry).is_some_and(|at| !at.0.is_finite()) {
                return Err(unencodable(
                    "a runner-clock time isn't finite, and JSON would write it as null".to_string(),
                ));
            }
            (k, json(serde_json::to_vec(entry))?)
        }
    };
    let depth = json_depth(&payload);
    if depth > MAX_DEPTH {
        return Err(unencodable(format!("the payload nests {depth} levels deep, over the {MAX_DEPTH} level cap")));
    }
    let frame = Frame { lsn, kind: k, flags: flags_of(k), payload: &payload };
    match decode(&frame) {
        Ok(Some(back)) if back == *record => Ok((k, payload)),
        Ok(_) => Err(unencodable("the payload doesn't read back as the same record".to_string())),
        Err(e) => Err(unencodable(format!("the payload doesn't read back: {e}"))),
    }
}

/// The flags a record of kind `k` is written with: [`SKIPPABLE`] for the audit record, which replay never needs,
/// and 0 for every other kind.
fn flags_of(k: u8) -> u8 {
    if k == kind::AUDIT { SKIPPABLE } else { 0 }
}

/// Appends `records` as frames, in order (no segment header; the writer writes that), each with [`flags_of`] its
/// kind.
///
/// # Errors
/// [`JournalError::Unencodable`] for a `Mark` or `HistoryEntry::Session` entry, a non-finite runner-clock time
/// (`Begin::at`, `OpRecord::at`, `JobStarted::at`), a payload over [`MAX_PAYLOAD`](super::MAX_PAYLOAD) or nested deeper than [`MAX_DEPTH`], a
/// value JSON can't hold exactly (so the payload doesn't read back as the same record), or JSON that fails. `out` is
/// left as it was.
pub fn encode(records: &[(Lsn, Record)], out: &mut Vec<u8>) -> Result<(), JournalError> {
    let start = out.len();
    let result = records.iter().try_for_each(|(lsn, record)| {
        let (kind, payload) = payload_of(*lsn, record)?;
        write_frame(*lsn, kind, flags_of(kind), &payload, out)
    });
    if result.is_err() {
        out.truncate(start);
    }
    result
}

/// The record a frame holds; `Ok(None)` for an unknown kind flagged skippable.
///
/// # Errors
/// [`JournalError::TooNew`] with `kind: Some(k)` for an unknown kind that isn't skippable (the reserved kind 10
/// included). [`JournalError::BadPayload`] for JSON that doesn't read as the kind's payload, or a [`HistoryEntry`]
/// whose `entry` tag disagrees with the frame's kind.
pub fn decode(frame: &Frame<'_>) -> Result<Option<Record>, JournalError> {
    let bad = |reason: String| JournalError::BadPayload { lsn: frame.lsn.0, kind: frame.kind, reason };
    match frame.kind {
        kind::SESSION => {
            let info = serde_json::from_slice(frame.payload).map_err(|e| bad(e.to_string()))?;
            Ok(Some(Record::Session(info)))
        }
        kind::CLOSE => {
            let close: ClosePayload = serde_json::from_slice(frame.payload).map_err(|e| bad(e.to_string()))?;
            Ok(Some(Record::Close { snapshot: close.snapshot }))
        }
        kind::BEGIN
        | kind::OP
        | kind::COMMIT
        | kind::ROLLBACK
        | kind::UNDO
        | kind::REDO
        | kind::DROPPED
        | kind::JOB_STARTED
        | kind::JOB_ENDED
        | kind::TX_REVERT => {
            let entry: HistoryEntry = serde_json::from_slice(frame.payload).map_err(|e| bad(e.to_string()))?;
            if entry_kind(&entry) != Some(frame.kind) {
                return Err(bad("the entry tag disagrees with the record kind".to_string()));
            }
            Ok(Some(Record::Entry(entry)))
        }
        // An audit record this build can't read (a fact a later build added) is skipped like an unknown kind: replay
        // never needs one. Only a flagged one: an unflagged kind 11 frame is no record this format writes.
        kind::AUDIT => match serde_json::from_slice::<HistoryEntry>(frame.payload) {
            Ok(entry @ HistoryEntry::Audit { .. }) => Ok(Some(Record::Entry(entry))),
            _ if frame.skippable() => Ok(None),
            Ok(_) => Err(bad("the entry tag disagrees with the record kind".to_string())),
            Err(e) => Err(bad(e.to_string())),
        },
        _ if frame.skippable() => Ok(None),
        unknown => Err(JournalError::TooNew { version: FORMAT_VERSION, kind: Some(unknown) }),
    }
}
