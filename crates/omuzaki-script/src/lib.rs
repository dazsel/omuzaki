//! The script runtime (PL-02): the Rhai engine, its limits, cancel, output and errors, and the [`Calls`] handle a run
//! sends its inner op requests through (`docs/design/scripting.md` sections 2, 4 and 5).
//!
//! # Module map
//!
//! | Where | Holds |
//! |---|---|
//! | [`ScriptHost`] | the engine: [`run`](ScriptHost::run) a script, [`check`](ScriptHost::check) one without running it |
//! | `bindings` (private), [`script_call`] | the table of registered ops, the `ops::area::verb` modules generated from it, `try_op` and `get`; how a script spells a call |
//! | [`CheckReport`], [`effective_grants`] | the static check, and the grants a run gets from it |
//! | [`Calls`], [`RunnerCalls`], [`JobScript`] | where a run's requests go: a `Runner` (console, CLI) or a driver job (`script.run`) |
//! | [`Macro`], [`MacroRecorder`], [`play`], [`to_jsonl`] | macros: record a person's edits, replay them as one transaction, the file |
//! | [`ops`] | the `script.check`, `script.run`, `script.save`, `script.delete`, `script.save_mine`, `script.delete_mine` and `script.list` ops |
//! | [`run_script`], [`check_script`] | the two commands behind `omuzaki script run` and `check`, as pure functions |
//! | [`ScriptStore`], [`MemScriptStore`] | My scripts, the person's own scripts across projects |
//!
//! The ops register themselves with `register_op!`; a binary keeps them only if it calls [`link`] (ADR 0015).
//! The user guide is `docs/guide/scripting.md`.

#![deny(missing_docs)]

mod bindings;
mod budget;
mod calls;
mod check;
mod closures;
mod commands;
mod convert;
mod directive;
mod error;
mod guards;
mod host;
mod job;
mod limits;
mod loops;
mod macros;
pub mod ops;
pub(crate) mod registered;
mod saved;
mod store;
#[doc(hidden)]
pub mod testing;

pub use bindings::script_call;
pub use budget::MemoryBudget;
pub use calls::{Calls, Cancel, PlannedCall, RunnerCalls};
pub use check::{CheckReport, Diagnostic, FieldError, FieldErrorKind, OpUse, Refused, effective_grants};
pub use commands::{CheckOutput, RunOptions, check_script, run_script};
pub use error::{LimitKind, OutputKind, OutputLine, ScriptError, ScriptErrorKind};
pub use host::{RunRequest, ScriptHost, ScriptOutcome};
pub use job::JobScript;
pub use limits::Limits;
pub use macros::{
    FileError, Macro, MacroRecorder, Overflowed, PlayError, PlayReport, RecordedCall, from_jsonl, play, play_within,
    to_jsonl,
};
pub use omuzaki_core::cancel::{CancelFlag, NeverCancel};
pub use registered::{registered_host, registered_ops};
pub use saved::{MAX_MACRO_BYTES, MAX_NAME_CHARS, Place, content_hash};
pub use store::{MemScriptStore, ScriptKind, ScriptStore, StoredScript};

omuzaki_core::define_link!();

// The crate's own unit tests run scripts, and a run refuses to start without the budget allocator.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: MemoryBudget = MemoryBudget::system();
