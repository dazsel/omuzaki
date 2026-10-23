//! The executor's history as data, so a snapshot can carry it and a restart can restore it.

use std::collections::BTreeSet;
use std::sync::Arc;

use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Actor, Effect, Scope, TxId};
use serde::{Deserialize, Serialize};

use crate::WallTime;

/// The current [`HistoryExport::format`]. A reader refuses a larger one; a new field is added with a serde default
/// instead of bumping this.
pub const HISTORY_EXPORT_FORMAT: u32 = 1;

/// One undo or redo step: a committed transaction's combined patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StepExport {
    /// The transaction.
    pub tx: TxId,
    /// Its label.
    pub label: String,
    /// The combined patch of the transaction. Shared with the executor's step, so exporting copies no patch.
    pub patch: Arc<Patch>,
    /// The scopes the transaction needed, re-checked when the step is undone or redone.
    pub scopes: BTreeSet<Scope>,
    /// The effects of the transaction's ops outside the document, in apply order, so the step stays undoable and
    /// redoable after a snapshot and a restart. Empty in exports made before effects were recorded.
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Its commit number, counted across every actor: a person's undo takes the newest step among their own and
    /// their agents' histories, so the order steps were committed in survives an export (#2241). `0` in exports made
    /// before it was recorded, which are numbered in the order they are read.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub order: u64,
    /// Its undo number, counted across every actor, for a step on a redo stack (a person's redo takes the most
    /// recently undone); `0` on an undo stack and in older exports.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub undone: u64,
}

#[expect(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if passes a reference")]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// One actor's history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ActorHistory {
    /// Whose history.
    pub actor: Actor,
    /// Steps that can be undone, oldest first.
    pub undo: Vec<StepExport>,
    /// Steps that can be redone, in the order they will be redone.
    pub redo: Vec<StepExport>,
    /// When the actor last acted, in wall time; snapshots drop the history of actors not seen for a long
    /// time (design section 9).
    pub last_seen: WallTime,
    /// The person whose undo reaches these steps, for an agent working for them: kept with the history, so after a
    /// restart the person can still undo what their agent did once its session is gone. `None` for a person (their
    /// own undo reaches it), an automation and the system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<Actor>,
}

/// A transaction that was still open when the export was made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OpenTxExport {
    /// The actor that opened it.
    pub owner: Actor,
    /// The transaction.
    pub tx: TxId,
    /// Its label.
    pub label: String,
    /// The patch of its ops so far.
    pub patch: Patch,
    /// The scopes its ops needed so far.
    pub scopes: BTreeSet<Scope>,
    /// The effects of its ops outside the document so far, in apply order. Empty in exports made before effects were
    /// recorded.
    #[serde(default)]
    pub effects: Vec<Effect>,
}

/// The executor's whole history at one journal position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HistoryExport {
    /// Version of this layout ([`HISTORY_EXPORT_FORMAT`]).
    pub format: u32,
    /// Every actor's stacks.
    pub actors: Vec<ActorHistory>,
    /// Transactions open at this point.
    pub open: Vec<OpenTxExport>,
    /// The next transaction id, so ids never repeat across sessions.
    pub next_tx: TxId,
    /// The next op record `seq`, so sequence numbers never repeat across sessions.
    pub next_seq: u64,
}

impl StepExport {
    /// A step with these facts.
    pub fn new(tx: TxId, label: impl Into<String>, patch: Arc<Patch>, scopes: BTreeSet<Scope>) -> Self {
        Self { tx, label: label.into(), patch, scopes, effects: Vec::new(), order: 0, undone: 0 }
    }

    /// This step with the effects of its ops, in apply order.
    #[must_use]
    pub fn with_effects(self, effects: Vec<Effect>) -> Self {
        Self { effects, ..self }
    }

    /// This step with its commit number and, on a redo stack, its undo number (see [`StepExport::order`]).
    #[must_use]
    pub fn with_order(self, order: u64, undone: u64) -> Self {
        Self { order, undone, ..self }
    }
}

impl ActorHistory {
    /// An actor with empty stacks, last seen at `last_seen`; fill `undo` and `redo` afterwards.
    pub fn new(actor: Actor, last_seen: WallTime) -> Self {
        Self { actor, undo: Vec::new(), redo: Vec::new(), last_seen, principal: None }
    }

    /// This history with the person whose undo reaches it.
    #[must_use]
    pub fn with_principal(mut self, principal: Option<Actor>) -> Self {
        self.principal = principal;
        self
    }
}

impl OpenTxExport {
    /// An open transaction with these facts.
    pub fn new(owner: Actor, tx: TxId, label: impl Into<String>, patch: Patch, scopes: BTreeSet<Scope>) -> Self {
        Self { owner, tx, label: label.into(), patch, scopes, effects: Vec::new() }
    }

    /// This transaction with the effects of its ops so far, in apply order.
    #[must_use]
    pub fn with_effects(self, effects: Vec<Effect>) -> Self {
        Self { effects, ..self }
    }
}

impl HistoryExport {
    /// An export of an empty history: no actors, no open transactions, ids from 1.
    ///
    /// ```
    /// use omuzaki_history_api::{HISTORY_EXPORT_FORMAT, HistoryExport};
    ///
    /// let empty = HistoryExport::empty();
    /// assert_eq!(empty.format, HISTORY_EXPORT_FORMAT);
    /// assert!(empty.actors.is_empty() && empty.open.is_empty());
    /// ```
    pub fn empty() -> Self {
        Self { format: HISTORY_EXPORT_FORMAT, actors: Vec::new(), open: Vec::new(), next_tx: TxId(1), next_seq: 1 }
    }
}
