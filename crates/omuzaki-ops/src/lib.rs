//! The op executor (spec v3 §6): implements [`omuzaki_ops_api::Runner`], the one entry point MCP, the CLI, scripts, the
//! command palette and the UI use to change the project, and [`Sessions`](omuzaki_ops_api::Sessions) and
//! [`Jobs`](omuzaki_ops_api::Jobs) beside it.
//!
//! An [`Executor`] owns the document. Running an op is: check the actor holds `read`, find the op by name, decode its
//! arguments, **plan** it against the current document (no mutation), check the op's scope, **apply** the resulting
//! patch, append an [`OpRecord`](omuzaki_ops_api::OpRecord) and emit events. Because every change is a patch, the rest
//! follows:
//!
//! - **Permissions.** Every `run` needs `read`, because a plan or a conflict message carries document values. A
//!   dry-run needs only `read` (it is how an actor shows what it would ask permission for), except for a query, whose
//!   plan *is* its answer and so also needs the op's scope. Applying needs `read` and the op's scope.
//! - **Dry-run** returns the plan and stops; nothing changes, nothing is logged.
//! - **Transactions** group ops into one undo step. An op without a transaction is its own one-op transaction, labelled
//!   by the op's summary. Ops in an open transaction apply immediately (later ops see earlier ones); rollback applies the
//!   combined inverse in one atomic patch. A transaction belongs to the actor that began it: only that actor can add ops
//!   to it, commit it or roll it back.
//! - **Undo/redo are per actor, and per principal for a person** (design `agent-sessions.md` §5.4). Each actor has its
//!   own stacks, so an agent never undoes a person's edit; a person's undo takes the newest step among their own and
//!   their agents' histories (pausing a running agent first), and any of them committing clears the person's redo.
//!   Undo applies the step's combined inverse patch, so it works after later edits to *other* paths. If others changed
//!   the same paths since, the step is *dropped* and a `Conflict` names the path and who changed it last; the next undo
//!   reaches the step before, so history can't wedge. A step that changed nothing is not an undo step. Undoing or
//!   redoing needs the scopes of the ops it reverts; a refusal for missing scopes keeps the step. Conflict messages from
//!   undo, redo and rollback never contain document values. Each actor keeps at most [`HistoryLimits::undo_depth`]
//!   steps, and the log the newest [`HistoryLimits::log_records`]; the journal keeps the rest.
//! - **Effects outside the document** (ADR 0036 decision 8): an `Undo::Explicit` op records a pair of inverse calls,
//!   which undo, redo, rollback, a session's end and undo-session run (tried on a scratch copy first, compensated on
//!   failure). An edit with `Undo::None` is a **live action**: logged and announced, never in an undo history, and
//!   refused if it plans a patch.
//! - **Queries** are checked against the scope, never change the document, and are not logged: the log is the history
//!   of changes. A query that plans a patch is a bug and fails with `Internal`.
//! - **Sessions** (ADR 0020): agents act in sessions whose grants never exceed their person's, with rate limits, a
//!   budget, a fence and the stale-write guard; a request a person could allow waits in the approval queue instead of
//!   failing (see `gate.rs`). Ending a session abandons its open transactions and cancels its jobs; a person can undo a
//!   whole session as one step, or stop it.
//! - **Jobs** (ADR 0036): long work runs on job threads without the lock; a driver job edits through inner calls as one
//!   transaction, joining the caller's when asked.
//! - **The journal** ([`Executor::with_journal`]): every change is handed to the sink as a physical entry, in history
//!   order, and [`Executor::restore`] and [`Executor::replay`] bring the history back after a restart.
//! - **Events** go to every subscriber on its own channel, in log order, from outside the state lock.
//! - **Concurrency:** one mutex covers the document, log, history, sessions and approvals, so plan-then-apply is atomic
//!   and a control (a pause, an end, a budget pause) is never split from the request it stops. This is the authoring
//!   plane, not the real-time one.
#![deny(missing_docs)]

mod approvals;
mod budget;
mod bus;
mod controls;
mod effects;
mod error;
mod executor;
mod fence;
mod gate;
mod guard;
mod history;
mod index;
mod jobs;
mod journal;
mod limits;
mod replay;
mod sessions;
mod simulate;
mod state;
mod sync;
mod travel;

pub use error::ExecutorError;

/// The committed state of an executor's document ([`Executor::committed`]): what a branch point records.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Committed {
    /// The tree without the open transactions' changes.
    pub tree: omuzaki_doc_api::Value,
    /// The history export, when no transaction was open.
    pub history: Option<omuzaki_history_api::HistoryExport>,
    /// The open transactions taken out: id, owner and label.
    pub open: Vec<(omuzaki_ops_api::TxId, omuzaki_ops_api::Actor, String)>,
}

pub use executor::Executor;
pub use history::HistoryLimits;
pub use replay::{ReplayError, ReplayReport};
