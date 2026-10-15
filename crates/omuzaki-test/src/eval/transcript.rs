//! The transcript (design 7.3) as the `log.*` checks read it: what the server returned for each tool call, never what
//! the model claimed.

use omuzaki_ops_api::Mode;
use serde_json::Value;

/// One tool call and what the server answered.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// The op name.
    pub op: String,
    /// The arguments as sent.
    pub args: Value,
    /// Whether the call was a dry run or applied.
    pub mode: Mode,
    /// `true` when the server answered with a result, `false` when it answered with an error.
    pub ok: bool,
}

impl ToolCall {
    /// A call of `op` with `args` in `mode`, and whether the server answered without an error.
    pub fn new(op: impl Into<String>, args: Value, mode: Mode, ok: bool) -> Self {
        Self { op: op.into(), args, mode, ok }
    }

    /// Whether this call changed the project: an applied call the server did not refuse.
    #[must_use]
    pub fn applied(&self) -> bool {
        self.mode == Mode::Apply && self.ok
    }
}

/// The tool calls of one attempt, in the order the server saw them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transcript {
    /// The calls, oldest first.
    pub calls: Vec<ToolCall>,
}

impl Transcript {
    /// A transcript of these calls.
    #[must_use]
    pub fn new(calls: Vec<ToolCall>) -> Self {
        Self { calls }
    }
}
