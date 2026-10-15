//! The answer's transaction: one user message is one undo step (D2.5).

use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::AssistantEvent;
use serde_json::json;

use crate::link::{ToolInfo, ToolKind, ToolReply};
use crate::run::{AnswerRun, Flow};
use crate::worker::Worker;

/// The label prefix of an assistant step in the history.
const LABEL_PREFIX: &str = "Assistant: ";
/// The most characters of the person's first line in the label.
pub(crate) const LABEL_CHARS: usize = 60;
/// The most characters of the person's first line in the cause.
pub(crate) const CAUSE_CHARS: usize = 200;

/// What a call needs of the transaction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum TxUse {
    /// Opens the answer's transaction if it is not open, and joins it.
    Join,
    /// Never in a transaction.
    Outside,
}

/// A tool's kind. A tool with no kind (an older server) counts as an edit, or as a query when it says it is read-only.
pub(crate) fn kind_of(info: Option<&ToolInfo>) -> ToolKind {
    match info {
        None => ToolKind::Edit,
        Some(t) if t.kind == ToolKind::Unknown => {
            if t.read_only {
                ToolKind::Query
            } else {
                ToolKind::Edit
            }
        }
        Some(t) => t.kind,
    }
}

/// How a call of this kind relates to the answer's transaction.
pub(crate) fn tx_use(kind: ToolKind) -> TxUse {
    match kind {
        ToolKind::Edit | ToolKind::DriverJob => TxUse::Join,
        ToolKind::Query | ToolKind::Job | ToolKind::Builtin | ToolKind::Unknown => TxUse::Outside,
    }
}

/// The first line of the person's message, trimmed.
pub(crate) fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

fn chars(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

/// `Assistant: <first line, 60 chars>`.
pub(crate) fn label(first_line: &str) -> String {
    format!("{LABEL_PREFIX}{}", chars(first_line, LABEL_CHARS))
}

/// The first line, 200 chars: the cause every request in the transaction records.
pub(crate) fn cause(first_line: &str) -> String {
    chars(first_line, CAUSE_CHARS)
}

/// The text the model gets for `history_undo` or `history_redo` while the answer's transaction is open (D2.5).
pub(crate) const UNDO_WHILE_OPEN: &str = "your changes in this answer are not a step yet; use `discard_changes`";

impl Worker {
    /// Opens the answer's transaction with `tx_begin`. A refusal comes back for the model, as the result of the call that
    /// needed the transaction.
    pub(crate) fn begin_tx(&mut self, run: &mut AnswerRun) -> Result<Option<ToolReply>, Flow> {
        let first = run.first_line.clone();
        let args = json!({"label": label(&first), "cause": cause(&first)});
        let reply = self.call_plain("tx_begin", args.as_object().cloned())?;
        if reply.is_error {
            return Ok(Some(reply));
        }
        run.tx = self.session_info().and_then(|info| info.open_tx);
        run.tx_open = true;
        Ok(None)
    }

    /// Commits the answer's transaction at its end. After Stop the executor commits it, so this is not called then.
    pub(crate) fn commit_tx(&mut self, run: &mut AnswerRun) {
        if !run.tx_open {
            return;
        }
        run.tx_open = false;
        let error = match self.call_plain("tx_commit", None) {
            Ok(reply) if !reply.is_error => return,
            Ok(reply) => HostError::Mcp(crate::content::plain_text(&reply.blocks)),
            Err(_) => HostError::SessionEnded,
        };
        self.shared.emit(AssistantEvent::Error(error));
    }

    /// `discard_changes`: rolls the answer's transaction back. `None` means none was open; a refusal comes back for the
    /// model.
    pub(crate) fn rollback_tx(&mut self, run: &mut AnswerRun) -> Result<Option<ToolReply>, Flow> {
        if !run.tx_open {
            return Ok(None);
        }
        let reply = self.call_plain("tx_rollback", None)?;
        if !reply.is_error {
            run.tx_open = false;
            run.tx = None;
        }
        Ok(Some(reply))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(kind: ToolKind, read_only: bool) -> ToolInfo {
        ToolInfo { name: "t".into(), title: None, description: String::new(), input_schema: json!({}), kind, read_only }
    }

    #[test]
    fn a_seventy_character_line_gives_a_sixty_character_label_part_and_the_whole_cause() {
        let line = "x".repeat(70);
        assert_eq!(label(&line), format!("Assistant: {}", "x".repeat(60)));
        assert_eq!(cause(&line), line);
    }

    #[test]
    fn the_label_counts_characters_not_bytes() {
        let line = "é".repeat(80);
        assert_eq!(label(&line).chars().count(), LABEL_PREFIX.len() + 60);
    }

    #[test]
    fn kinds_decide_who_joins_the_transaction() {
        assert_eq!(tx_use(ToolKind::Edit), TxUse::Join);
        assert_eq!(tx_use(ToolKind::DriverJob), TxUse::Join);
        for kind in [ToolKind::Query, ToolKind::Job, ToolKind::Builtin] {
            assert_eq!(tx_use(kind), TxUse::Outside);
        }
    }

    #[test]
    fn a_tool_without_a_kind_is_an_edit_unless_it_is_read_only() {
        assert_eq!(kind_of(Some(&tool(ToolKind::Unknown, false))), ToolKind::Edit);
        assert_eq!(kind_of(Some(&tool(ToolKind::Unknown, true))), ToolKind::Query);
        assert_eq!(kind_of(None), ToolKind::Edit);
        assert_eq!(kind_of(Some(&tool(ToolKind::Job, false))), ToolKind::Job);
    }

    #[test]
    fn the_first_line_skips_blank_lines() {
        assert_eq!(first_line("\n  make her face more oval \nmore"), "make her face more oval");
    }
}
