//! The op executor (spec v3 §6): implements [`omuzaki_ops_api::Runner`], the one entry point MCP, the CLI, scripts, the
//! command palette and the UI use to change the project.
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
//! - **Transactions** group ops into one undo step. An op without a transaction is its own one-op transaction. Ops in
//!   an open transaction apply immediately (later ops see earlier ones); rollback applies the combined inverse in one
//!   atomic patch. A transaction belongs to the actor that began it: only that actor can add ops to it, commit it or
//!   roll it back.
//! - **Undo/redo are per actor.** Each actor has its own stacks of its committed transactions, so an agent never
//!   undoes a person's edit. Undo applies the step's combined inverse patch, so it works after later edits to *other*
//!   paths. If others changed the same paths since, the step is *dropped* from the actor's history and a `Conflict`
//!   names the path and who changed it last (from the op log); the next undo reaches the step before, so history
//!   can't wedge. A step that changed nothing (a query, a no-op edit) is not an undo step and does not clear redo; a
//!   commit clears only its own actor's redo. Undoing or redoing needs the scopes of the ops it reverts; a refusal for
//!   missing scopes keeps the step. Conflict messages from undo, redo and rollback never contain document values.
//! - **Queries** are checked against the scope, never change the document, and are not logged: the log is the history
//!   of changes. A query that plans a patch is a bug and fails with `Internal`.
//! - **Events** go to every subscriber on its own channel, in log order, from outside the state lock.
//! - **Concurrency:** one mutex covers the document, log and history, so plan-then-apply is atomic. This is the
//!   authoring plane, not the real-time one.
//!
//! The log and the undo/redo stacks live in memory without a cap: persisting and trimming them is the store's job.
//!
//! `Job` ops are planned and applied like edits here; running their long work is the job manager's concern.
//! Undo and redo refuse a step that holds an op marked [`Undo::Explicit`](omuzaki_ops_api::Undo::Explicit): the executor
//! has no hook to revert that op's effects outside the document, so it drops the step and leaves the document alone
//! (ADR 0036 decision 9). Rollback doesn't check for such ops yet: its effects arrive with the effects task.
#![deny(missing_docs)]

mod approvals;
mod budget;
mod bus;
mod controls;
mod error;
mod executor;
mod fence;
mod gate;
mod index;
mod jobs;
mod limits;
mod sessions;
mod state;
mod sync;

pub use error::ExecutorError;
pub use executor::Executor;
