//! The script runtime (PL-02): the Rhai engine, its limits, cancel, output and errors, and the [`Calls`] handle a run
//! sends its inner op requests through (`docs/design/scripting.md` sections 2, 4 and 5).

#![deny(missing_docs)]

mod bindings;
mod calls;
mod check;
mod convert;
mod error;
mod host;
mod limits;
mod macros;

pub use calls::{Calls, Cancel, PlannedCall, RunnerCalls};
pub use check::{CheckReport, Diagnostic, FieldError, FieldErrorKind, OpUse, Refused, effective_grants};
pub use error::{LimitKind, OutputKind, OutputLine, ScriptError, ScriptErrorKind};
pub use host::{RunRequest, ScriptHost, ScriptOutcome};
pub use limits::Limits;
pub use macros::{FileError, Macro, MacroRecorder, PlayError, PlayReport, RecordedCall, from_jsonl, play, to_jsonl};
