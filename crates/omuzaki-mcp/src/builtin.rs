//! The tools that aren't ops: undo/redo, the op log, transactions, sessions, reading the document and finding ops. They
//! call the `Runner` directly, so they exist whatever ops are registered, and their names are reserved.

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde_json::{Value, json};

use crate::catalog::{ToolMode, kind_meta};

/// The longest `cause` `tx_begin` accepts, in characters. The first line of a person's message, which is what a cause
/// is, rarely needs more.
pub(crate) const CAUSE_MAX_CHARS: usize = 200;

/// A built-in tool.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    Undo,
    Redo,
    Log,
    TxBegin,
    TxCommit,
    TxRollback,
    SessionInfo,
    SessionList,
    ApprovalWait,
    DocGet,
    OpsFind,
    OpsDescribe,
    OpsRun,
    ToolsEnable,
}

/// The longest `approval_wait` waits, in seconds, and how long it waits when no timeout is given.
pub(crate) const APPROVAL_WAIT_MAX_SECONDS: u64 = 50;
pub(crate) const APPROVAL_WAIT_DEFAULT_SECONDS: u64 = 30;

impl Builtin {
    pub(crate) const ALL: [Builtin; 14] = [
        Self::Undo,
        Self::Redo,
        Self::Log,
        Self::TxBegin,
        Self::TxCommit,
        Self::TxRollback,
        Self::SessionInfo,
        Self::SessionList,
        Self::ApprovalWait,
        Self::DocGet,
        Self::OpsFind,
        Self::OpsDescribe,
        Self::OpsRun,
        Self::ToolsEnable,
    ];

    /// Whether the tool talks to the executor's session registry, and so exists only when the host wired one.
    pub(crate) const fn needs_registry(self) -> bool {
        matches!(self, Self::SessionInfo | Self::SessionList | Self::ApprovalWait)
    }

    /// Whether the tool exists on a server with (or without) a session registry, listing tools in `mode`. `ops_run`
    /// and `tools_enable` stand in for the op tools a core list leaves out, so only a core list has them.
    pub(crate) fn offered(self, registry: bool, mode: ToolMode) -> bool {
        (registry || !self.needs_registry())
            && (mode == ToolMode::Core || !matches!(self, Self::OpsRun | Self::ToolsEnable))
    }

    /// Whether the tool keeps nothing between calls, so a request over HTTP that stands alone (no session) may call it.
    pub(crate) const fn stateless(self) -> bool {
        matches!(self, Self::Log | Self::DocGet | Self::OpsFind | Self::OpsDescribe)
    }

    /// The dotted name, shown as the tool's title and on the first line of its description.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Undo => "history.undo",
            Self::Redo => "history.redo",
            Self::Log => "history.log",
            Self::TxBegin => "tx.begin",
            Self::TxCommit => "tx.commit",
            Self::TxRollback => "tx.rollback",
            Self::SessionInfo => "session.info",
            Self::SessionList => "session.list",
            Self::ApprovalWait => "approval.wait",
            Self::DocGet => "doc.get",
            Self::OpsFind => "ops.find",
            Self::OpsDescribe => "ops.describe",
            Self::OpsRun => "ops.run",
            Self::ToolsEnable => "tools.enable",
        }
    }

    /// The tool name an agent calls: the dotted name with `.` as `_` (see `names`).
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Undo => "history_undo",
            Self::Redo => "history_redo",
            Self::Log => "history_log",
            Self::TxBegin => "tx_begin",
            Self::TxCommit => "tx_commit",
            Self::TxRollback => "tx_rollback",
            Self::SessionInfo => "session_info",
            Self::SessionList => "session_list",
            Self::ApprovalWait => "approval_wait",
            Self::DocGet => "doc_get",
            Self::OpsFind => "ops_find",
            Self::OpsDescribe => "ops_describe",
            Self::OpsRun => "ops_run",
            Self::ToolsEnable => "tools_enable",
        }
    }

    /// The MCP tool definition.
    pub(crate) fn tool(self) -> Tool {
        let (description, destructive) = match self {
            Self::Undo => (
                "Undo YOUR most recent committed change in this session (one op, or a whole transaction) and report \
                 what was undone. Undo is per actor: it never touches edits made by the user or by other agents. Use \
                 it when a result looks wrong; call it again to step back further. Reverse it with history_redo. \
                 Having nothing of yours to undo is not an error. Fails with `conflict` if someone else has since \
                 changed the same values: that step is then dropped from your history (so undo never gets stuck) and \
                 the error names the path and who changed it; call history_undo again for the step before. Fails \
                 with `denied` if this session lacks the scope of what it would undo.",
                false,
            ),
            Self::Redo => (
                "Redo the change you most recently undid with history_undo (per actor, like undo). Any new edit of \
                 yours clears what could be redone. Having nothing to redo is not an error.",
                false,
            ),
            Self::Log => (
                "Read the op log: the same events as the resource omuzaki://log, for clients that can't read resources. \
                 Each event has a sequence number `n`; `after_n` returns only the events numbered above it, e.g. after_n=37 \
                 for \"what changed since n 37\" from a heads-up. Needs the `read` scope, like the resource.",
                false,
            ),
            Self::TxBegin => (
                "Start a transaction: every edit you apply from now until tx_commit joins it and becomes ONE undo step \
                 named by `label` (history_undo reverts them together). Use it for changes made of several ops, like \
                 'make the face more oval'. Edits still apply immediately and are visible at once; the transaction \
                 only groups them. One transaction per session at a time; previews (dry_run), queries and jobs \
                 (renders, exports) don't join. Pass `cause`, what the person asked for: each edit in the transaction \
                 records it, and it is shown on the step's card. If the connection closes before tx_commit, the \
                 transaction is rolled back.",
                false,
            ),
            Self::TxCommit => (
                "Finish the open transaction: its edits become one undo step. Call it once the group of edits is done \
                 and checked. Fails with `not_found` if no transaction is open (tx_begin starts one).",
                false,
            ),
            Self::TxRollback => (
                "Abandon the open transaction and revert every edit made since tx_begin, leaving a clean slate. Use it \
                 when the approach failed. Fails with `conflict` if the project changed in ways the revert can't \
                 apply, and with `not_found` if no transaction is open.",
                true,
            ),
            Self::SessionInfo => (
                "Show this session's own record: who it acts as and for, how it is connected, its credential and whether \
                 the person has paired it, what it may do (scopes and file roots), its fence (the parts of the project \
                 it may edit; empty means the whole project except what another session holds), its budget and spend, \
                 its rate limits, whether it is active or paused, its open transaction, and the approvals it is waiting \
                 on. Needs `read`. Still answers while the session is paused, including while it waits to be paired: \
                 then approve the client in Omuzaki's window.",
                false,
            ),
            Self::SessionList => (
                "List every live agent session on this project: id, actor, client, model, transport, state, fence and \
                 steps. Your own is marked `you: true`. Use it to see who else is working, and where, before you edit. \
                 Needs `read`. Still answers while the session is paused.",
                false,
            ),
            Self::ApprovalWait => (
                "Wait for the person to decide one of YOUR pending approvals (the `approval` id a `waiting` result \
                 gave, or the pairing approval in session_info), up to `timeout_seconds` (default 30, at most 50). An \
                 approved request returns what the op returned; a failed one its error; a rejected one says so with \
                 the person's comment; a withdrawn one says why. If the time runs out the approval is still pending: \
                 call again, or carry on with something else. This tool never approves anything: only the person can, \
                 in Omuzaki's window. Another session's approval is `not_found`.",
                false,
            ),
            Self::DocGet => (
                "Read one value of the project document by its path, as a result's change lines name it (`characters/mira\
                 /face/blend`; `\"\"` is the whole document). A large object or list comes back as an outline (each \
                 key with its type and size) unless `full` is true, so read deeper paths instead. Needs `read`.",
                false,
            ),
            Self::OpsFind => (
                "Find ops by what they do: every op whose name or summary holds all the words of `query` (any order, \
                 any case), with its kind, scope and summary, best matches first. Narrow it with `area`. Without either \
                 it lists the areas and how many ops each has. Then read one op's arguments with ops_describe.",
                false,
            ),
            Self::OpsDescribe => (
                "Describe one op: its full description, its argument schema with examples, and the call to make. Pass \
                 the op name (`face.set_profile`) or its tool name (`face_set_profile`).",
                false,
            ),
            Self::OpsRun => (
                "Run any op by name, listed as a tool or not: `op` is the op (`face.set_profile`), `args` its \
                 arguments as ops_describe shows them, and `dry_run` previews an edit without applying it. The result \
                 is exactly what calling the op's own tool returns, errors included. Built-in tools are called \
                 directly, not through this.",
                true,
            ),
            Self::ToolsEnable => (
                "List the ops of one or more areas (`face`, `stage`, `output`, ...; ops_find with no query names them all) \
                 as tools of their own, for clients that refresh their tool list (notifications/tools/list_changed). \
                 Pass `disable: true` to take them off the list again. The ops stay callable through ops_run either way.",
                false,
            ),
        };
        let read_only =
            matches!(self, Self::Log | Self::SessionInfo | Self::SessionList | Self::ApprovalWait | Self::DocGet)
                || matches!(self, Self::OpsFind | Self::OpsDescribe);
        let annotations =
            ToolAnnotations::new().read_only(read_only).destructive(destructive).open_world(self == Self::OpsRun);
        let description = format!("{}: {description}", self.title());
        Tool::new(self.name(), description, input_schema(self))
            .with_title(self.title())
            .with_annotations(annotations)
            .with_meta(kind_meta("builtin"))
    }
}

/// `tx_begin` takes a label, `history_log` an optional `after_n`; the others take nothing.
fn input_schema(builtin: Builtin) -> JsonObject {
    let schema = match builtin {
        Builtin::Log => json!({
            "type": "object",
            "properties": {"after_n": {
                "type": "integer",
                "minimum": 0,
                "description": "Return only the events numbered above this (the `n` in a heads-up). Omit for all."
            }},
            "additionalProperties": false
        }),
        Builtin::TxBegin => json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Name of this undo step, shown in history, e.g. 'make the face more oval'."
                },
                "cause": {
                    "type": "string",
                    "maxLength": CAUSE_MAX_CHARS,
                    "description": "What the person asked for in this step (their message, first line), at most 200 \
                                    characters. Each edit in the transaction records it. Omit it when there's no request."
                }
            },
            "required": ["label"],
            "additionalProperties": false
        }),
        Builtin::ApprovalWait => json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "The approval to wait for: the number a `waiting` result or session_info names."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": APPROVAL_WAIT_MAX_SECONDS,
                    "description": "How long to wait. Default 30, at most 50."
                }
            },
            "required": ["id"],
            "additionalProperties": false
        }),
        Builtin::DocGet => json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The value's path, `/` between keys (`characters/mira/face`); `\"\"` for the whole document."
                },
                "full": {
                    "type": "boolean",
                    "default": false,
                    "description": "Return a large object or list whole instead of its outline."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        Builtin::OpsFind => json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Words the op's name or summary holds, e.g. `virtual camera`."},
                "area": {"type": "string", "description": "Only ops of this area, e.g. `output`."}
            },
            "additionalProperties": false
        }),
        Builtin::OpsDescribe => json!({
            "type": "object",
            "properties": {"op": {"type": "string", "description": "The op name (`face.set_profile`) or tool name."}},
            "required": ["op"],
            "additionalProperties": false
        }),
        Builtin::OpsRun => json!({
            "type": "object",
            "properties": {
                "op": {"type": "string", "description": "The op name (`face.set_profile`) or tool name."},
                "args": {"type": "object", "description": "The op's arguments, as ops_describe shows them. Omit for none."},
                "dry_run": {"type": "boolean", "default": false, "description": "Preview an edit without applying it."}
            },
            "required": ["op"],
            "additionalProperties": false
        }),
        Builtin::ToolsEnable => json!({
            "type": "object",
            "properties": {
                "areas": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "description": "The areas to list, e.g. [\"stage\", \"output\"]."
                },
                "disable": {"type": "boolean", "default": false, "description": "Take these areas off the list instead."}
            },
            "required": ["areas"],
            "additionalProperties": false
        }),
        _ => json!({"type": "object", "properties": {}, "additionalProperties": false}),
    };
    match schema {
        Value::Object(map) => map,
        _ => JsonObject::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::tool_name;

    #[test]
    fn every_builtin_has_a_safe_name_a_dotted_title_and_an_object_schema() {
        for b in Builtin::ALL {
            let tool = b.tool();
            assert_eq!(tool.name, b.name());
            assert_eq!(tool.name, tool_name(b.title()), "the name is the title with dots as underscores");
            assert!(tool.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
            assert_eq!(tool.title.as_deref(), Some(b.title()));
            let description = tool.description.as_deref().unwrap();
            assert!(description.starts_with(&format!("{}: ", b.title())) && description.len() > 40);
            assert_eq!(tool.input_schema["type"], "object");
        }
        assert_eq!(Builtin::TxBegin.tool().input_schema["required"], json!(["label"]));
    }

    #[test]
    fn tx_begin_takes_an_optional_cause_of_at_most_200_characters() {
        let schema = Builtin::TxBegin.tool().input_schema;
        assert_eq!(schema["properties"]["cause"]["type"], "string");
        assert_eq!(schema["properties"]["cause"]["maxLength"], CAUSE_MAX_CHARS);
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn only_rollback_and_the_op_gateway_are_destructive() {
        let destructive: Vec<_> =
            Builtin::ALL.into_iter().filter(|b| b.tool().annotations.is_some_and(|a| a.is_destructive())).collect();
        assert_eq!(destructive, [Builtin::TxRollback, Builtin::OpsRun], "ops_run may run an op that can't be undone");
    }
}
