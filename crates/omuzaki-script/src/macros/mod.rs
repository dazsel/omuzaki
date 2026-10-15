//! Macros: the op calls a person made, recorded from the runner's events and played back as one transaction.
//!
//! A macro is language-free data (`{op, args}` lines, the step shape of eval fixtures), so it doesn't depend on the
//! scripting language (`docs/design/scripting.md` section 7).

mod file;
mod play;
mod record;

use omuzaki_doc_api::Value;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use file::{FileError, from_jsonl, to_jsonl};
pub use play::{PlayError, PlayReport, play};
pub use record::MacroRecorder;

/// One op call of a macro, as the person made it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecordedCall {
    /// The op's registered name.
    pub op: String,
    /// Its arguments.
    pub args: Value,
}

/// A named list of op calls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Macro {
    /// The name shown in the macro list and used as the transaction label and the cause text when it plays.
    pub name: String,
    /// The calls, in the order they are played.
    pub steps: Vec<RecordedCall>,
}
