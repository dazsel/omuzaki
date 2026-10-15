//! Resume: replay a conversation into the view and say whether its last answer was cut short.

use omuzaki_agent_api::conversations::{ConversationId, Resumed};
use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_llm_api::message::ContentBlock;

use super::ConversationStore;

impl ConversationStore {
    /// Reads a conversation back for the panel. Live sessions never survive a restart, so the next message opens a
    /// new one; `interrupted` tells the panel to show "interrupted: the studio closed".
    ///
    /// # Errors
    /// [`HostError::Store`] for an unknown id or an unreadable log.
    pub fn resume(&self, id: ConversationId) -> Result<Resumed, HostError> {
        let entries = self.read(id)?;
        let interrupted = is_interrupted(&entries);
        Ok(Resumed::new(entries, interrupted))
    }
}

/// D7 has no "end of answer" entry, so the rule is: an answer is closed when the last entry is a `Charge`, `Step`,
/// `Stopped` or `Compacted` and the last `Assistant` entry has no tool use. Anything else, including a log that ends
/// on the person's message, is interrupted. A conversation with no entries has no answer to interrupt.
fn is_interrupted(entries: &[Entry]) -> bool {
    let Some(last) = entries.last() else {
        return false;
    };
    let ends_an_answer =
        matches!(last, Entry::Charge { .. } | Entry::Step { .. } | Entry::Stopped | Entry::Compacted { .. });
    !ends_an_answer || last_assistant_calls_a_tool(entries)
}

fn last_assistant_calls_a_tool(entries: &[Entry]) -> bool {
    entries.iter().rev().find_map(|entry| match entry {
        Entry::Assistant { blocks, .. } => Some(blocks.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }))),
        _ => None,
    }) == Some(true)
}

#[cfg(test)]
mod tests {
    use omuzaki_llm_api::usage::Usage;
    use serde_json::json;

    use super::*;

    fn text() -> Entry {
        Entry::assistant(vec![ContentBlock::text("done")])
    }

    fn calls() -> Entry {
        Entry::assistant(vec![ContentBlock::tool_use("c", "t", json!({}))])
    }

    fn charge() -> Entry {
        Entry::charge(Usage::new(1, 1), 1)
    }

    #[test]
    fn closed_answers_are_not_interrupted() {
        for last in [charge(), Entry::step(omuzaki_ops_api::TxId(1), "l", "r"), Entry::Stopped, Entry::compacted("s")] {
            assert!(!is_interrupted(&[Entry::user("hi", Vec::new()), text(), last]));
        }
        assert!(!is_interrupted(&[]));
    }

    #[test]
    fn everything_else_is_interrupted() {
        let user = Entry::user("hi", Vec::new());
        assert!(is_interrupted(std::slice::from_ref(&user)));
        assert!(is_interrupted(&[user.clone(), calls(), Entry::tool_call("c", "t", "s")]));
        assert!(is_interrupted(&[user.clone(), calls(), Entry::tool_result("c", true, Vec::new())]));
        // A charge after a turn that ended in a tool call: the answer continues, so it is not closed.
        assert!(is_interrupted(&[user, calls(), Entry::tool_result("c", true, Vec::new()), charge()]));
    }
}
