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
        Ok(Resumed::new(crate::resume::apply_stop_cuts(entries), interrupted))
    }
}

/// D7 has no "end of answer" entry, so the rule is: an answer is closed when the last entry is a `Charge`, `Step` or
/// `Compacted` and the last `Assistant` entry has no tool use, or when it is `Stopped` (a stop closes the answer
/// even with a call in flight, and whatever the host wrote after it). Anything else, including a log that ends on the person's message, is interrupted.
/// Session notes after the last answer are not part of it. A conversation with no entries has no answer to interrupt.
fn is_interrupted(entries: &[Entry]) -> bool {
    // A Stop closes its answer, whatever bookkeeping follows it: the stopped step, a late result, session notes.
    let start = entries.iter().rposition(|e| matches!(e, Entry::User { .. })).map_or(0, |at| at + 1);
    if entries[start..].iter().any(|e| matches!(e, Entry::Stopped { .. })) {
        return false;
    }
    let Some(last) = entries.iter().rev().find(|e| !matches!(e, Entry::Session { .. })) else {
        return false;
    };
    match last {
        Entry::Stopped { .. } => false,
        Entry::Charge { .. } | Entry::Step { .. } | Entry::Compacted { .. } => last_assistant_calls_a_tool(entries),
        _ => true,
    }
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
        for last in
            [charge(), Entry::step(omuzaki_ops_api::TxId(1), "l", "r"), Entry::stopped(None), Entry::compacted("s")]
        {
            assert!(!is_interrupted(&[Entry::user("hi", Vec::new()), text(), last]));
        }
        assert!(!is_interrupted(&[]));
    }

    #[test]
    fn the_order_the_host_writes_is_a_closed_answer() {
        // The turn, then what it cost.
        assert!(!is_interrupted(&[Entry::user("hi", Vec::new()), text(), charge()]));
        // A session note after the last answer is not part of it.
        let ended = Entry::session(
            omuzaki_ops_api::SessionId(1),
            omuzaki_agent_api::entry::SessionNote::Ended(omuzaki_ops_api::EndReason::Closed),
        );
        assert!(!is_interrupted(&[Entry::user("hi", Vec::new()), text(), charge(), ended]));
    }

    #[test]
    fn a_stopped_answer_stays_closed_after_its_step_and_a_late_result() {
        let log = [
            Entry::user("hi", Vec::new()),
            calls(),
            Entry::stopped(Some(0)),
            Entry::step(omuzaki_ops_api::TxId(1), "l (stopped)", "r"),
            Entry::tool_result("c:late", true, vec![]),
        ];
        assert!(!is_interrupted(&log));
        // The next message starts a new answer, which is judged on its own.
        let mut next = log.to_vec();
        next.push(Entry::user("again", Vec::new()));
        assert!(is_interrupted(&next));
    }

    #[test]
    fn a_stop_closes_the_answer_even_with_a_call_in_flight() {
        let log = [
            Entry::user("hi", Vec::new()),
            calls(),
            Entry::tool_result("c", false, Vec::new()),
            Entry::stopped(Some(0)),
        ];
        assert!(!is_interrupted(&log));
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
