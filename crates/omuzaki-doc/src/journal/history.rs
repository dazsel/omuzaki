//! The project's history from the journal and its archive (journal.md 7.3 and 9; #429): every record of every
//! session, read in LSN order, and the transactions they describe, for `history.list`, the History panel and the
//! history view that rebuilds any past state.
//!
//! The archive holds the sealed segments [`archive_before`](super::archive_before) moved out of the live folder; the
//! live folder continues it. A summary is rebuilt from the records, never stored, so it can't disagree with them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_history_api::state::HistoryError;
use omuzaki_history_api::{
    HistoryEntry, HistoryLog, HistoryPage, HistoryQuery, JournalSink, Lsn, Trigger, TxOutcome, TxSummary, WallTime,
};
use omuzaki_ops_api::TxId;

use super::reader::Reader;
use super::{HEADER_LEN, JournalError, Record, SegmentHeader, archive_dir, journal_dir, segment_seq};
use crate::snapshot::Snapshotter;

/// How long a read waits for the journal to make what was appended before it durable.
const SYNC_WAIT: Duration = Duration::from_secs(10);

/// Every record of the project in `project_dir`, the archive's then the live journal's, in LSN order. A folder that
/// doesn't exist holds none.
///
/// # Errors
/// As [`Reader::scan`].
pub fn read_all(project_dir: &Path) -> Result<Vec<(Lsn, Record)>, JournalError> {
    let mut records = Vec::new();
    for dir in [archive_dir(project_dir), journal_dir(project_dir)] {
        let after = records.last().map_or(Lsn(0), |(lsn, _): &(Lsn, Record)| *lsn);
        let Some(first) = first_lsn(&dir) else { continue };
        // Start where this folder starts when what came before it is gone (an archive that was pruned).
        let from = if after.0 == 0 || first.0 > after.0 + 1 { Lsn(first.0.saturating_sub(1)) } else { after };
        let scan = Reader::scan(&dir, from)?;
        records.extend(scan.records.into_iter().filter(|(lsn, _)| lsn.0 > after.0));
    }
    Ok(records)
}

/// Whether the project in `project_dir` was left open: its live journal has records and the last one is not a `close`
/// (Omuzaki stopped without closing it: a crash, a kill, a power cut). This is the startup plan's recovery check
/// (settings.md D9): it reads only the live journal, never opens the project and writes nothing. A project never
/// journalled, or one whose journal does not read, is not reported; opening it says why.
pub fn left_open(project_dir: &Path) -> bool {
    let dir = journal_dir(project_dir);
    let Some(first) = first_lsn(&dir) else { return false };
    Reader::scan(&dir, Lsn(first.0.saturating_sub(1)))
        .is_ok_and(|scan| scan.records.last().is_some_and(|(_, record)| !matches!(record, Record::Close { .. })))
}

/// The first LSN of the first used segment in `dir`, from its header; `None` when there is none.
fn first_lsn(dir: &Path) -> Option<Lsn> {
    let mut segments: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| Some((segment_seq(e.file_name().to_str()?)?, e.path())))
        .collect();
    segments.sort();
    segments.into_iter().find_map(|(_, path)| {
        let mut header = [0u8; HEADER_LEN];
        std::io::Read::read_exact(&mut std::fs::File::open(path).ok()?, &mut header).ok()?;
        SegmentHeader::decode(&header).ok().filter(|h| !h.is_prepared()).map(|h| h.first)
    })
}

/// The transactions `records` describe, oldest first (by their first record). A transaction that changed nothing in
/// the document (a save, a named version, a no-op edit) is left out: there is no state of its own to go back to.
pub fn summarise(records: &[(Lsn, Record)]) -> Vec<TxSummary> {
    let mut items: Vec<TxSummary> = Vec::new();
    let mut changed: Vec<bool> = Vec::new();
    let mut index: HashMap<TxId, usize> = HashMap::new();
    let (mut session, mut started) = (String::new(), None::<WallTime>);
    for (lsn, record) in records {
        let entry = match record {
            Record::Session(info) => {
                session.clone_from(&info.session);
                started = Some(info.started);
                continue;
            }
            Record::Entry(entry) => entry,
            Record::Close { .. } => continue,
        };
        match entry {
            HistoryEntry::Begin { tx, actor, label, at } => {
                let mut item = TxSummary::new(*tx, session.clone(), label.clone(), actor.clone(), *lsn);
                item.time = started.and_then(|s| s.after(*at));
                index.insert(*tx, items.len());
                items.push(item);
                changed.push(false);
            }
            HistoryEntry::Op { record: op, commits } => {
                let at = match index.get(&op.tx) {
                    Some(&at) if !*commits => at,
                    _ => {
                        let mut item = TxSummary::new(op.tx, session.clone(), op.op.clone(), op.actor.clone(), *lsn);
                        item.time = started.and_then(|s| s.after(op.at));
                        if *commits {
                            item.outcome = TxOutcome::Committed;
                        }
                        index.insert(op.tx, items.len());
                        items.push(item);
                        changed.push(false);
                        items.len() - 1
                    }
                };
                changed[at] |= !op.patch.is_empty();
                let item = &mut items[at];
                item.ops += 1;
                item.last = *lsn;
                if !item.op_names.contains(&op.op) {
                    item.op_names.push(op.op.clone());
                }
                if item.cause.is_none() {
                    item.cause.clone_from(&op.cause);
                }
            }
            HistoryEntry::Commit { tx } => settle(&mut items, &index, *tx, Some(*lsn), TxOutcome::Committed),
            HistoryEntry::Rollback { tx, .. } => settle(&mut items, &index, *tx, Some(*lsn), TxOutcome::RolledBack),
            HistoryEntry::Undo { tx, .. } => settle(&mut items, &index, *tx, None, TxOutcome::Undone),
            HistoryEntry::Redo { tx, .. } => settle(&mut items, &index, *tx, None, TxOutcome::Committed),
            _ => {}
        }
    }
    items.into_iter().zip(changed).filter_map(|(item, changed)| changed.then_some(item)).collect()
}

fn settle(items: &mut [TxSummary], index: &HashMap<TxId, usize>, tx: TxId, last: Option<Lsn>, outcome: TxOutcome) {
    if let Some(item) = index.get(&tx).and_then(|&at| items.get_mut(at)) {
        item.outcome = outcome;
        if let Some(lsn) = last {
            item.last = lsn;
        }
    }
}

/// [`HistoryLog`] over a project's journal and archive, with the named versions from its snapshots.
pub struct JournalHistory {
    dir: PathBuf,
    snapshots: Arc<Snapshotter>,
    journal: Mutex<Option<Arc<dyn JournalSink>>>,
}

impl JournalHistory {
    /// The history of the project in `dir`, whose snapshots `snapshots` keeps.
    pub fn new(dir: impl Into<PathBuf>, snapshots: Arc<Snapshotter>) -> Self {
        Self { dir: dir.into(), snapshots, journal: Mutex::new(None) }
    }

    /// Reads after making `journal`'s appends durable, so the newest commits are listed.
    pub fn bind(&self, journal: Arc<dyn JournalSink>) {
        *self.journal.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(journal);
    }

    /// Every record, after a sync of the bound journal.
    ///
    /// # Errors
    /// [`HistoryError::Io`] when the journal can't be made durable or read.
    pub fn records(&self) -> Result<Vec<(Lsn, Record)>, HistoryError> {
        let journal = self.journal.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(journal) = journal {
            journal.sync(SYNC_WAIT).map_err(|e| HistoryError::Io(e.to_string()))?;
        }
        read_all(&self.dir).map_err(|e| HistoryError::Io(e.to_string()))
    }

    /// Every transaction, oldest first, with the named versions taken of the state after each.
    ///
    /// # Errors
    /// As [`records`](Self::records).
    pub fn summaries(&self) -> Result<Vec<TxSummary>, HistoryError> {
        let mut items = summarise(&self.records()?);
        let refs = self.snapshots.refs().map_err(|e| HistoryError::Io(e.to_string()))?;
        for info in refs.iter().filter(|info| info.trigger == Trigger::Named) {
            let Some(name) = &info.name else { continue };
            // The newest transaction settled at or before the snapshot: the state it holds.
            let named = items
                .iter_mut()
                .filter(|item| item.outcome != TxOutcome::RolledBack && item.last <= info.lsn)
                .max_by_key(|item| item.last);
            if let Some(item) = named {
                item.names.push(name.clone());
            }
        }
        Ok(items)
    }
}

impl HistoryLog for JournalHistory {
    fn list(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError> {
        let matching = self
            .summaries()?
            .into_iter()
            .filter(|item| query.rolled_back || item.outcome != TxOutcome::RolledBack)
            .filter(|item| query.actor.as_ref().is_none_or(|actor| &item.actor == actor))
            .collect();
        Ok(HistoryPage::of(matching, query))
    }

    fn get(&self, tx: TxId) -> Result<TxSummary, HistoryError> {
        self.summaries()?.into_iter().find(|item| item.tx == tx).ok_or(HistoryError::UnknownTx(tx))
    }
}
