//! The project's history as a list of transactions (VC-01; `docs/design/journal.md` §7.3 and §9): what the History
//! panel, `history.list` and the CLI show, read from the journal and its archive across every session, not only the
//! executor's capped in-memory log.

use omuzaki_ops_api::{Actor, Cause, Service, TxId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::HistoryError;
use crate::{Lsn, WallTime};

/// The most transactions one page holds.
pub const MAX_PAGE: usize = 500;

/// What became of a transaction, as of the newest record.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TxOutcome {
    /// Committed, and in effect.
    Committed,
    /// Committed, then undone (and not redone since).
    Undone,
    /// Rolled back: it never took effect.
    RolledBack,
    /// Still open (a transaction someone is building), or left open when a session ended without closing it.
    Open,
}

/// One transaction of the project's history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TxSummary {
    /// Its id, unique in the project.
    pub tx: TxId,
    /// The session it ran in.
    pub session: String,
    /// Its label: the transaction's own, or the op's name for a one-op transaction.
    pub label: String,
    /// Who made it.
    pub actor: Actor,
    /// Why, when the first op said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Cause>,
    /// When it started, in milliseconds since the Unix epoch; `None` when the session's start time is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<WallTime>,
    /// The ops it holds.
    pub ops: usize,
    /// The names of those ops, in order, each once.
    pub op_names: Vec<String>,
    /// Its first record.
    pub first: Lsn,
    /// Its commit or rollback record (its only record for a one-op transaction); the newest of its records while open.
    pub last: Lsn,
    /// What became of it.
    pub outcome: TxOutcome,
    /// The named versions taken of the state right after it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

impl TxSummary {
    /// A summary with the required facts; `cause`, `time` and `names` start empty, `outcome` is `Open`.
    pub fn new(tx: TxId, session: impl Into<String>, label: impl Into<String>, actor: Actor, first: Lsn) -> Self {
        Self {
            tx,
            session: session.into(),
            label: label.into(),
            actor,
            cause: None,
            time: None,
            ops: 0,
            op_names: Vec::new(),
            first,
            last: first,
            outcome: TxOutcome::Open,
            names: Vec::new(),
        }
    }
}

/// Which transactions to list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HistoryQuery {
    /// At most this many (clamped to 1 ..= [`MAX_PAGE`]); 0 means 50.
    #[serde(default)]
    pub limit: usize,
    /// Only those older than this transaction (the `next` of the page before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<TxId>,
    /// Only those this actor made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Actor>,
    /// Rolled back transactions too (left out by default: they never took effect).
    #[serde(default)]
    pub rolled_back: bool,
}

impl HistoryQuery {
    /// The page size this query asks for, clamped.
    pub fn page_size(&self) -> usize {
        match self.limit {
            0 => 50,
            n => n.min(MAX_PAGE),
        }
    }
}

/// One page of history, newest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HistoryPage {
    /// The transactions, newest first.
    pub items: Vec<TxSummary>,
    /// Pass as `before` for the next older page; `None` on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<TxId>,
    /// How many transactions match in all.
    pub total: usize,
}

impl HistoryPage {
    /// The newest-first page of `matching` (already filtered, oldest first) that `query` asks for.
    pub fn of(mut matching: Vec<TxSummary>, query: &HistoryQuery) -> Self {
        let total = matching.len();
        matching.reverse();
        if let Some(before) = query.before {
            matching.retain(|item| item.tx < before);
        }
        let size = query.page_size();
        let more = matching.len() > size;
        matching.truncate(size);
        let next = if more { matching.last().map(|item| item.tx) } else { None };
        Self { items: matching, next, total }
    }
}

/// The project's history, read from the journal and its archive (a [`Service`]).
pub trait HistoryLog: Service {
    /// A page of transactions, newest first.
    ///
    /// # Errors
    /// [`HistoryError::Io`] when the journal can't be read.
    fn list(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError>;

    /// The transaction `tx`.
    ///
    /// # Errors
    /// [`HistoryError::UnknownTx`] when the journal holds none; [`HistoryError::Io`].
    fn get(&self, tx: TxId) -> Result<TxSummary, HistoryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(n: u64) -> TxSummary {
        TxSummary::new(TxId(n), "s", format!("t{n}"), Actor::System, Lsn(n))
    }

    #[test]
    fn pages_go_newest_first_and_chain_by_before() {
        let all: Vec<TxSummary> = (1..=7).map(tx).collect();
        let query = HistoryQuery { limit: 3, ..HistoryQuery::default() };
        let first = HistoryPage::of(all.clone(), &query);
        assert_eq!(first.items.iter().map(|t| t.tx.0).collect::<Vec<_>>(), [7, 6, 5]);
        assert_eq!((first.next, first.total), (Some(TxId(5)), 7));
        let second = HistoryPage::of(all.clone(), &HistoryQuery { before: first.next, ..query.clone() });
        assert_eq!(second.items.iter().map(|t| t.tx.0).collect::<Vec<_>>(), [4, 3, 2]);
        let third = HistoryPage::of(all, &HistoryQuery { before: second.next, ..query });
        assert_eq!(third.items.iter().map(|t| t.tx.0).collect::<Vec<_>>(), [1]);
        assert_eq!(third.next, None);
    }

    #[test]
    fn the_page_size_defaults_to_50_and_is_capped() {
        assert_eq!(HistoryQuery::default().page_size(), 50);
        assert_eq!(HistoryQuery { limit: 10_000, ..HistoryQuery::default() }.page_size(), MAX_PAGE);
    }

    #[test]
    fn a_summary_serialises_its_outcome_in_snake_case() {
        let mut item = tx(3);
        item.outcome = TxOutcome::RolledBack;
        let json = serde_json::to_value(&item).unwrap_or_default();
        assert_eq!(json["outcome"], "rolled_back");
        assert!(json.get("cause").is_none(), "{json}");
    }
}
