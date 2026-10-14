//! The tools that aren't ops: undo/redo, the op log and transactions. They call the `Runner` directly, so they exist whatever ops
//! are registered, and their names are reserved.

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde_json::{Value, json};

use crate::catalog::kind_meta;

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
}

impl Builtin {
    pub(crate) const ALL: [Builtin; 6] =
        [Self::Undo, Self::Redo, Self::Log, Self::TxBegin, Self::TxCommit, Self::TxRollback];

    /// The dotted name, shown as the tool's title and on the first line of its description.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Undo => "history.undo",
            Self::Redo => "history.redo",
            Self::Log => "history.log",
            Self::TxBegin => "tx.begin",
            Self::TxCommit => "tx.commit",
            Self::TxRollback => "tx.rollback",
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
        };
        let annotations =
            ToolAnnotations::new().read_only(self == Self::Log).destructive(destructive).open_world(false);
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
    fn only_rollback_is_destructive() {
        let destructive: Vec<_> =
            Builtin::ALL.into_iter().filter(|b| b.tool().annotations.is_some_and(|a| a.is_destructive())).collect();
        assert_eq!(destructive, [Builtin::TxRollback]);
    }
}
