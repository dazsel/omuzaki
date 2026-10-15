//! What a failed run reports: where, why, and the output printed before it.

use omuzaki_ops_api::OpError;
use serde::{Deserialize, Serialize};

/// Which limit of the table in `docs/design/scripting.md` section 5 a run hit.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    /// The source is longer than [`Limits::source_bytes`](crate::Limits::source_bytes).
    Source,
    /// More Rhai operations than [`Limits::operations`](crate::Limits::operations).
    Operations,
    /// More inner op calls than [`Limits::calls`](crate::Limits::calls).
    Calls,
    /// Call depth or expression depth.
    Depth,
    /// A string longer than [`Limits::string_bytes`](crate::Limits::string_bytes).
    StringSize,
    /// An array longer than [`Limits::array_len`](crate::Limits::array_len).
    ArraySize,
    /// A map larger than [`Limits::map_len`](crate::Limits::map_len).
    MapSize,
    /// The wall-clock cap, read from the host's clock.
    Time,
    /// The result is larger than [`Limits::result_bytes`](crate::Limits::result_bytes) of JSON.
    Result,
}

/// The class of a [`ScriptError`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptErrorKind {
    /// The source did not parse, or used a symbol the sandbox disables.
    Parse,
    /// An inner op call failed; [`ScriptError::op_error`] says how.
    Op,
    /// The script itself failed (a type error, an unknown function, `throw`).
    Runtime,
    /// A limit stopped the run.
    Limit(LimitKind),
    /// The [`Cancel`](crate::Cancel) was set.
    Cancelled,
    /// A request the run was not allowed to make, such as one that joins a transaction.
    Refused,
}

/// Whether an output line came from `print` or from `log`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    /// A `print(x)` line.
    Print,
    /// A `log(x)` line (a note), or the "output truncated" marker.
    Note,
}

/// One line of a run's output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputLine {
    /// `print` or `log`.
    pub kind: OutputKind,
    /// The text, without a line break.
    pub text: String,
}

/// Why a run failed. Always rolled back; the output printed before the failure is kept.
#[derive(Clone, Debug, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[error("{message} (line {line}, column {column})")]
pub struct ScriptError {
    /// The class of failure.
    pub kind: ScriptErrorKind,
    /// 1-based line; 1 when the failure has no place in the source (a source that is too large).
    pub line: u32,
    /// 1-based column; 1 when the failure has no place in the source.
    pub column: u32,
    /// The failed op call, for [`ScriptErrorKind::Op`].
    pub op_error: Option<OpError>,
    /// What went wrong, in a sentence.
    pub message: String,
    /// What the script printed before it failed.
    pub output: Vec<OutputLine>,
}
