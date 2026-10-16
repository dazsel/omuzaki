//! Errors: the executor's own startup error, plus the constructors that turn lower-level failures into the
//! structured, actionable [`OpError`]s the contract promises (spec §6).

use omuzaki_doc_api::{DocPath, PatchError};
use omuzaki_ops_api::{Actor, OpError, TxId};

/// Why an [`Executor`](crate::Executor) couldn't be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ExecutorError {
    /// Two op types registered under one name. Names are the MCP tool name and the CLI path, so they must be unique.
    /// Both registrations are named by summary, since their names are identical.
    #[error("op {name:?} is registered twice: first as {first:?}, then as {second:?}; rename one of them")]
    DuplicateOp {
        /// The clashing name.
        name: &'static str,
        /// The summary of the registration seen first.
        first: &'static str,
        /// The summary of the registration seen second.
        second: &'static str,
    },
    /// The history to restore was written by a newer Omuzaki, in a layout this one cannot read.
    #[error(
        "the history is in format {found}, newer than the {supported} this build reads; open it with a newer Omuzaki"
    )]
    HistoryFormat {
        /// The export's format.
        found: u32,
        /// The newest format this build reads.
        supported: u32,
    },
}

/// A human-readable name for an actor, for messages.
pub(crate) fn who(actor: &Actor) -> String {
    match actor {
        Actor::Person { name } => format!("person {name}"),
        Actor::Agent { id, .. } => format!("agent {id}"),
        Actor::Plugin { id } => format!("plugin {id}"),
        Actor::System => "the system".to_owned(),
    }
}

/// Applying an op's own patch failed. A conflict means the document no longer matches what the op planned against.
pub(crate) fn apply_failed(op: &str, error: PatchError) -> OpError {
    match error {
        PatchError::Conflict { path, expected, found } => OpError::conflict(format!(
            "{op}: {path} changed since the op was planned (expected {expected}, found {found})"
        ))
        .hint("re-plan and retry"),
        PatchError::Unreachable { path, reason } => {
            OpError::internal(format!("{op} planned a change at {path} that can't be applied: {reason}"))
                .hint("this is a bug in the op: report it")
        }
    }
}

/// The path a [`PatchError`] is about.
pub(crate) fn patch_error_path(error: &PatchError) -> &DocPath {
    match error {
        PatchError::Conflict { path, .. } | PatchError::Unreachable { path, .. } => path,
    }
}

/// Applying an inverse (or redo) patch failed because others changed the document since. Whatever the exact cause
/// (a different value, a vanished parent), it's a conflict.
///
/// The message names the path and who changed it last (or that it changed before this session), but never document
/// *values*: the callers of undo, redo and rollback aren't required to hold `read`, so values would leak.
pub(crate) fn revert_failed(what: &str, error: &PatchError, changed_by: Option<&Actor>, hint: &str) -> OpError {
    // The executor knows every change since it started; one it can't name came before (a restored history, a file
    // edited while the project was closed), which the journal's archive keeps.
    let by = changed_by.map_or_else(
        || " (before this session: see the history for who)".to_owned(),
        |actor| format!(" (last by {})", who(actor)),
    );
    OpError::conflict(format!("can't {what}: {} was changed since{by}", patch_error_path(error))).hint(hint)
}

/// `tx` isn't an open transaction.
pub(crate) fn tx_not_open(tx: TxId) -> OpError {
    OpError::not_found(format!("transaction {} isn't open", tx.0)).hint("begin a transaction first, or omit `tx`")
}

/// `tx` is open but belongs to someone else.
pub(crate) fn tx_not_yours(tx: TxId, owner: &Actor) -> OpError {
    OpError::denied(format!("transaction {} belongs to {}", tx.0, who(owner)))
        .hint("begin your own transaction, or omit `tx`")
}
