//! The script runtime (PL-02): the Rhai engine, its limits, cancel, output and errors, and the [`Calls`] handle a run
//! sends its inner op requests through (`docs/design/scripting.md` sections 2, 4 and 5).
//!
//! # Module map
//!
//! | Where | Holds |
//! |---|---|
//! | [`ScriptHost`] | the engine: [`run`](ScriptHost::run) a script, [`check`](ScriptHost::check) one without running it |
//! | `bindings` (private) | the table of registered ops, the `ops::area::verb` modules generated from it, `try_op` and `get` |
//! | [`CheckReport`], [`effective_grants`] | the static check, and the grants a run gets from it |
//! | [`Calls`], [`RunnerCalls`], [`JobScript`] | where a run's requests go: a `Runner` (console, CLI) or a driver job (`script.run`) |
//! | [`Macro`], [`MacroRecorder`], [`play`], [`to_jsonl`] | macros: record a person's edits, replay them as one transaction, the file |
//! | [`ops`] | the `script.check`, `script.run`, `script.save`, `script.delete` and `script.list` ops |
//! | [`ScriptStore`], [`MemScriptStore`] | My scripts, the person's own scripts across projects |
//!
//! The ops register themselves with `register_op!`; a binary keeps them only if it calls [`link`] (ADR 0015).
//! The user guide is `docs/guide/scripting.md`.

#![deny(missing_docs)]

mod bindings;
mod calls;
mod check;
mod convert;
mod error;
mod host;
mod job;
mod limits;
mod macros;
pub mod ops;
mod registered;
mod saved;
mod store;

pub use calls::{Calls, Cancel, PlannedCall, RunnerCalls};
pub use check::{CheckReport, Diagnostic, FieldError, FieldErrorKind, OpUse, Refused, effective_grants};
pub use error::{LimitKind, OutputKind, OutputLine, ScriptError, ScriptErrorKind};
pub use host::{RunRequest, ScriptHost, ScriptOutcome};
pub use job::JobScript;
pub use limits::Limits;
pub use macros::{FileError, Macro, MacroRecorder, PlayError, PlayReport, RecordedCall, from_jsonl, play, to_jsonl};
pub use registered::{registered_host, registered_ops};
pub use saved::{MAX_MACRO_BYTES, MAX_NAME_CHARS, Place, content_hash};
pub use store::{MemScriptStore, ScriptKind, ScriptStore, StoredScript};

omuzaki_core::define_link!();
