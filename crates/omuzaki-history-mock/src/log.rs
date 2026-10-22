//! An in-memory [`HistoryLog`] for op and UI tests: the transactions a test puts, listed as the journal would.

use std::sync::Mutex;

use omuzaki_history_api::state::HistoryError;
use omuzaki_history_api::{HistoryLog, HistoryPage, HistoryQuery, TxOutcome, TxSummary};
use omuzaki_ops_api::TxId;

use crate::lock;

/// The transactions a test puts, oldest first.
#[derive(Default)]
pub struct MemoryHistoryLog {
    items: Mutex<Vec<TxSummary>>,
}

impl MemoryHistoryLog {
    /// A log holding `items`, oldest first.
    pub fn new(items: Vec<TxSummary>) -> Self {
        Self { items: Mutex::new(items) }
    }

    /// Appends a transaction.
    pub fn push(&self, item: TxSummary) {
        lock(&self.items).push(item);
    }
}

impl HistoryLog for MemoryHistoryLog {
    fn list(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError> {
        let matching = lock(&self.items)
            .iter()
            .filter(|item| query.rolled_back || item.outcome != TxOutcome::RolledBack)
            .filter(|item| query.actor.as_ref().is_none_or(|actor| &item.actor == actor))
            .cloned()
            .collect();
        Ok(HistoryPage::of(matching, query))
    }

    fn get(&self, tx: TxId) -> Result<TxSummary, HistoryError> {
        lock(&self.items).iter().find(|item| item.tx == tx).cloned().ok_or(HistoryError::UnknownTx(tx))
    }
}
