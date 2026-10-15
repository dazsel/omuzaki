//! The transcript (design 7.3) as the `log.*` checks read it: what the server returned for each tool call, never what
//! the model claimed.

use std::collections::BTreeMap;

use omuzaki_mcp::McpServer;
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
    /// `true` when the server's answer carried an application record: the call really changed the project. A query
    /// runs in [`Mode::Apply`] and succeeds without one, so neither the mode nor `ok` says this.
    pub applied: bool,
}

impl ToolCall {
    /// A call of `op` with `args` in `mode`, and whether the server answered without an error. It did not change the
    /// project until [`ToolCall::with_application`] says so.
    pub fn new(op: impl Into<String>, args: Value, mode: Mode, ok: bool) -> Self {
        Self { op: op.into(), args, mode, ok, applied: false }
    }

    /// The same call, whose answer carried an application record.
    #[must_use]
    pub fn with_application(mut self) -> Self {
        self.applied = true;
        self
    }

    /// Whether this call changed the project: the server answered with an application record.
    #[must_use]
    pub fn applied(&self) -> bool {
        self.ok && self.applied
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

/// How a server's tools are named, so a transcript that carries MCP tool names (`face_set_profile`) can be read in the op
/// names a task writes (`face.set_profile`).
///
/// The map comes from the server's own catalog ([`ToolNames::of_server`]: each tool's name and its `title`, the dotted
/// op name), never from replacing characters, so an op whose name already has an underscore (`face.set_profile`'s
/// `set_profile`) keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolNames {
    ops: BTreeMap<String, String>,
}

impl ToolNames {
    /// The map of the tools `server` lists: every op and every built-in (`history_undo` is `history.undo`).
    #[must_use]
    pub fn of_server(server: &McpServer) -> Self {
        let ops = server.tools().iter().filter_map(|tool| Some((tool.name.to_string(), tool.title.clone()?))).collect();
        Self { ops }
    }

    /// A map from `(tool name, op name)` pairs.
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self { ops: pairs.into_iter().collect() }
    }

    /// The op (or built-in) a tool name stands for, if the server lists such a tool.
    #[must_use]
    pub fn op_of(&self, tool: &str) -> Option<&str> {
        self.ops.get(tool).map(String::as_str)
    }

    /// Whether `name` is an op or built-in name the map knows (as a target).
    #[must_use]
    pub fn knows_op(&self, name: &str) -> bool {
        self.ops.values().any(|op| op == name)
    }
}
