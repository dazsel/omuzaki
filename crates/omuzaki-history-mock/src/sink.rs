use std::sync::Mutex;
use std::time::Duration;

use omuzaki_history_api::{HistoryEntry, JournalSink, JournalStatus, Lsn, SyncError};
use omuzaki_ops_api::OpError;

use crate::lock;

struct State {
    entries: Vec<(Lsn, HistoryEntry)>,
    admit: Result<(), OpError>,
    wants_mark: bool,
    sync_failure: Option<SyncError>,
    status: JournalStatus,
}

/// A [`JournalSink`] that keeps every entry in order. `admit`, `wants_mark`, `sync` and `status` answer what the test
/// scripted; by default it admits everything, wants no mark, syncs at once and is healthy.
pub struct RecordingSink {
    state: Mutex<State>,
}

impl Default for RecordingSink {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingSink {
    /// An empty, healthy sink.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                entries: Vec::new(),
                admit: Ok(()),
                wants_mark: false,
                sync_failure: None,
                status: JournalStatus::Healthy,
            }),
        }
    }

    /// Every entry appended so far, in order.
    pub fn entries(&self) -> Vec<HistoryEntry> {
        lock(&self.state).entries.iter().map(|(_, e)| e.clone()).collect()
    }

    /// Every entry with the LSN it was given.
    pub fn numbered(&self) -> Vec<(Lsn, HistoryEntry)> {
        lock(&self.state).entries.clone()
    }

    /// Makes `admit` answer `result` from now on.
    pub fn script_admit(&self, result: Result<(), OpError>) {
        lock(&self.state).admit = result;
    }

    /// Asks for a mark: `wants_mark` is true until a [`HistoryEntry::Mark`] is appended, as with the real journal.
    pub fn request_mark(&self) {
        lock(&self.state).wants_mark = true;
    }

    /// Makes `sync` fail with `error` (`None` makes it succeed again).
    pub fn script_sync(&self, error: Option<SyncError>) {
        lock(&self.state).sync_failure = error;
    }

    /// Makes `status` answer `status`.
    pub fn script_status(&self, status: JournalStatus) {
        lock(&self.state).status = status;
    }
}

impl JournalSink for RecordingSink {
    fn append(&self, entry: HistoryEntry) -> Lsn {
        let mut state = lock(&self.state);
        if matches!(entry, HistoryEntry::Mark { .. }) {
            state.wants_mark = false;
        }
        let lsn = Lsn(state.entries.len() as u64 + 1);
        state.entries.push((lsn, entry));
        lsn
    }

    fn admit(&self) -> Result<(), OpError> {
        lock(&self.state).admit.clone()
    }

    fn wants_mark(&self) -> bool {
        lock(&self.state).wants_mark
    }

    fn sync(&self, _timeout: Duration) -> Result<Lsn, SyncError> {
        let state = lock(&self.state);
        match &state.sync_failure {
            Some(error) => Err(error.clone()),
            None => Ok(state.entries.last().map_or(Lsn(0), |(lsn, _)| *lsn)),
        }
    }

    fn status(&self) -> JournalStatus {
        lock(&self.state).status.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use omuzaki_doc_api::{Patch, Value};
    use omuzaki_history_api::{HistoryExport, OpenTxExport};
    use omuzaki_ops_api::{Actor, JobId, JobState, TxId};

    use super::*;

    #[test]
    fn entries_keep_their_order_and_get_consecutive_lsns() {
        let sink = RecordingSink::new();
        assert_eq!(sink.append(HistoryEntry::commit(TxId(1))), Lsn(1));
        assert_eq!(sink.append(HistoryEntry::commit(TxId(2))), Lsn(2));
        assert_eq!(sink.entries(), vec![HistoryEntry::commit(TxId(1)), HistoryEntry::commit(TxId(2))]);
        assert_eq!(sink.sync(Duration::ZERO), Ok(Lsn(2)));
    }

    #[test]
    fn a_requested_mark_stays_wanted_until_one_arrives() {
        let sink = RecordingSink::new();
        assert!(!sink.wants_mark());
        sink.request_mark();
        sink.append(HistoryEntry::commit(TxId(1)));
        assert!(sink.wants_mark());
        sink.append(HistoryEntry::Mark { history: Arc::new(HistoryExport::empty()) });
        assert!(!sink.wants_mark());
    }

    #[test]
    fn admit_sync_and_status_follow_the_script() {
        let sink = RecordingSink::new();
        assert!(sink.admit().is_ok());
        sink.script_admit(Err(OpError::unsupported("saving is full")));
        assert!(sink.admit().is_err());
        sink.script_sync(Some(SyncError::Timeout { durable: Lsn(0) }));
        assert_eq!(sink.sync(Duration::ZERO), Err(SyncError::Timeout { durable: Lsn(0) }));
        sink.script_status(JournalStatus::Behind { pending: 3 });
        assert_eq!(sink.status(), JournalStatus::Behind { pending: 3 });
    }

    #[test]
    fn job_and_revert_entries_are_kept_in_order_and_unchanged() {
        let sink = RecordingSink::new();
        let remaining = OpenTxExport::new(Actor::System, TxId(4), "open", Patch::default(), BTreeSet::new());
        // Read from JSON so this crate needs no `omuzaki-core` for `Seconds` (a transparent `f64`).
        let started: HistoryEntry = serde_json::from_value(serde_json::json!({
            "entry": "job_started",
            "job": 1,
            "op": "render",
            "args": Value::from(1),
            "actor": Actor::System,
            "on_behalf_of": null,
            "at": 0.5,
        }))
        .unwrap();
        assert!(matches!(&started, HistoryEntry::JobStarted { job: JobId(1), .. }));
        assert_eq!(serde_json::to_value(&started).unwrap()["at"], 0.5);
        let ended = HistoryEntry::JobEnded { job: JobId(1), state: JobState::done(TxId(5), Some(12)) };
        let revert = HistoryEntry::TxRevert {
            tx: TxId(4),
            seqs: vec![11],
            patch: Patch::default(),
            remaining: remaining.clone(),
        };
        for entry in [started.clone(), ended.clone(), revert.clone()] {
            sink.append(entry);
        }
        assert_eq!(sink.entries(), vec![started, ended, revert]);
        let HistoryEntry::TxRevert { remaining: back, .. } = &sink.entries()[2] else { panic!("not a tx_revert") };
        assert_eq!(back, &remaining);
    }
}
