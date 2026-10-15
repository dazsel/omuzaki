//! Continuing a stored conversation: the model's messages rebuilt from the stored entries (D7, #1395).

use std::collections::HashMap;

use omuzaki_agent_api::entry::{Entry, StoredBlock};
use omuzaki_llm_api::message::{ContentBlock, ImageOrigin, Message, Role};

use crate::context::person_text;
use crate::sink::EntrySink;

/// The result a trailing tool use gets when the studio closed before the call finished.
const INTERRUPTED: &str = "interrupted: the studio closed";
/// The result a call gets when the answer was stopped before it ran.
const STOPPED_BEFORE_RUN: &str = "not run: stopped by the person";

/// Rebuilds the model's messages from stored entries. The per-message context blocks were not stored and are not
/// recreated; images come back as a note, since this variant reads no blobs.
#[allow(dead_code)]
pub(crate) fn messages_from_entries(entries: &[Entry]) -> Vec<Message> {
    Rebuild::new(None).run(entries)
}

/// As [`messages_from_entries`], reading images back from `sink`'s blobs.
pub(crate) fn messages_from_entries_with(entries: &[Entry], sink: &dyn EntrySink) -> Vec<Message> {
    Rebuild::new(Some(sink)).run(entries)
}

struct Rebuild<'a> {
    sink: Option<&'a dyn EntrySink>,
    messages: Vec<Message>,
    /// The tool-use ids of the last assistant message that still wait for their results, in order.
    open: Vec<String>,
    results: HashMap<String, (bool, Vec<StoredBlock>)>,
}

impl<'a> Rebuild<'a> {
    fn new(sink: Option<&'a dyn EntrySink>) -> Self {
        Self { sink, messages: Vec::new(), open: Vec::new(), results: HashMap::new() }
    }

    fn run(mut self, entries: &[Entry]) -> Vec<Message> {
        let start = entries.iter().rposition(|e| matches!(e, Entry::Compacted { .. }));
        let tail = match start {
            Some(at) => {
                if let Entry::Compacted { message, .. } = &entries[at] {
                    self.push(Role::User, vec![ContentBlock::text(message.clone())]);
                }
                &entries[at + 1..]
            }
            None => entries,
        };
        for (at, entry) in tail.iter().enumerate() {
            match entry {
                Entry::User { text, chips, .. } => {
                    self.close_open(false);
                    self.push(Role::User, vec![ContentBlock::text(person_text(text, chips))]);
                }
                Entry::Assistant { blocks, .. } => {
                    self.close_open(stopped_before(tail, at));
                    self.open = blocks
                        .iter()
                        .filter_map(|b| if let ContentBlock::ToolUse { id, .. } = b { Some(id.clone()) } else { None })
                        .collect();
                    self.messages.push(Message::new(Role::Assistant, blocks.clone()));
                }
                Entry::ToolResult { id, ok, blocks, .. } => {
                    // A result that arrived after Stop answers nothing; it is kept in the log only.
                    if !id.ends_with(":late") {
                        self.results.insert(id.clone(), (*ok, blocks.clone()));
                    }
                }
                _ => {}
            }
        }
        self.close_open(stopped_before(tail, tail.len()));
        self.messages
    }

    fn push(&mut self, role: Role, mut blocks: Vec<ContentBlock>) {
        match self.messages.last_mut() {
            Some(last) if last.role == role => last.content.append(&mut blocks),
            _ => self.messages.push(Message::new(role, blocks)),
        }
    }

    /// Answers the open tool uses with their stored results, or with a synthetic error for one that has none:
    /// providers refuse a tool use with no result.
    fn close_open(&mut self, stopped: bool) {
        if self.open.is_empty() {
            return;
        }
        let missing = if stopped { STOPPED_BEFORE_RUN } else { INTERRUPTED };
        let results: Vec<ContentBlock> = std::mem::take(&mut self.open)
            .into_iter()
            .map(|id| match self.results.remove(&id) {
                Some((true, blocks)) => ContentBlock::tool_result(id, self.content(&blocks)),
                Some((false, blocks)) => ContentBlock::tool_error(id, self.content(&blocks)),
                None => ContentBlock::tool_error(id, vec![ContentBlock::text(missing)]),
            })
            .collect();
        self.push(Role::User, results);
    }

    fn content(&self, blocks: &[StoredBlock]) -> Vec<ContentBlock> {
        blocks
            .iter()
            .map(|block| match block {
                StoredBlock::Text(text) => ContentBlock::text(text.clone()),
                StoredBlock::Image { mime, blob, .. } => self
                    .sink
                    .and_then(|sink| sink.read_blob(blob, *mime))
                    .map_or_else(
                        || ContentBlock::text("[image no longer stored]"),
                        |bytes| ContentBlock::image(*mime, bytes, ImageOrigin::Render),
                    ),
                _ => ContentBlock::text("[content omitted]"),
            })
            .collect()
    }
}

/// Whether Stop was pressed between the previous turn and position `at`.
fn stopped_before(tail: &[Entry], at: usize) -> bool {
    tail[..at].iter().rev().take_while(|e| !matches!(e, Entry::User { .. } | Entry::Assistant { .. })).any(|e| matches!(e, Entry::Stopped))
}

#[cfg(test)]
mod tests {
    use omuzaki_agent_api::message::ContextChip;
    use serde_json::json;

    use super::*;

    fn text_of(message: &Message) -> String {
        message
            .content
            .iter()
            .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn a_plain_exchange_rebuilds_as_alternating_messages() {
        let entries = vec![
            Entry::user("hi", vec![ContextChip::Viewport]),
            Entry::assistant(vec![ContentBlock::text("hello")]),
            Entry::charge(omuzaki_llm_api::usage::Usage::new(1, 1), 1),
        ];
        let messages = messages_from_entries(&entries);
        assert_eq!(messages.len(), 2);
        assert_eq!(text_of(&messages[0]), "hi\n[attached: the viewport]");
        assert_eq!(messages[1].role, Role::Assistant);
    }

    #[test]
    fn tool_results_become_one_message_in_the_order_of_the_tool_uses() {
        let entries = vec![
            Entry::user("go", vec![]),
            Entry::assistant(vec![
                ContentBlock::tool_use("c1", "a", json!({})),
                ContentBlock::tool_use("c2", "b", json!({})),
            ]),
            Entry::tool_result("c2", true, vec![StoredBlock::Text("two".into())]),
            Entry::tool_result("c1", false, vec![StoredBlock::Text("one".into())]),
            Entry::assistant(vec![ContentBlock::text("done")]),
        ];
        let messages = messages_from_entries(&entries);
        assert_eq!(messages.len(), 4);
        let ids: Vec<(&str, bool)> = messages[2]
            .content
            .iter()
            .filter_map(|b| if let ContentBlock::ToolResult { id, is_error, .. } = b { Some((id.as_str(), *is_error)) } else { None })
            .collect();
        assert_eq!(ids, [("c1", true), ("c2", false)]);
    }

    #[test]
    fn a_trailing_tool_use_gets_one_synthetic_error() {
        let entries = vec![
            Entry::user("go", vec![]),
            Entry::assistant(vec![ContentBlock::tool_use("c1", "a", json!({}))]),
            Entry::tool_call("c1", "a", "A"),
        ];
        let messages = messages_from_entries(&entries);
        let last = messages.last().unwrap();
        assert_eq!(last.role, Role::User);
        assert_eq!(last.content, vec![ContentBlock::tool_error("c1", vec![ContentBlock::text(INTERRUPTED)])]);
    }

    #[test]
    fn a_late_result_answers_nothing() {
        let entries = vec![
            Entry::user("go", vec![]),
            Entry::assistant(vec![ContentBlock::tool_use("c1", "a", json!({}))]),
            Entry::tool_result("c1", false, vec![StoredBlock::Text("stopped".into())]),
            Entry::Stopped,
            Entry::tool_result("c1:late", true, vec![StoredBlock::Text("applied".into())]),
        ];
        let messages = messages_from_entries(&entries);
        assert_eq!(messages.len(), 3);
        assert!(matches!(&messages[2].content[0], ContentBlock::ToolResult { is_error: true, .. }));
    }

    #[test]
    fn history_starts_at_the_last_compaction() {
        let entries = vec![
            Entry::user("old", vec![]),
            Entry::assistant(vec![ContentBlock::text("old answer")]),
            Entry::compacted("summary one"),
            Entry::user("middle", vec![]),
            Entry::compacted("summary two"),
            Entry::user("new", vec![]),
        ];
        let messages = messages_from_entries(&entries);
        assert_eq!(messages.len(), 1, "the summary and the new message are one user message");
        assert_eq!(text_of(&messages[0]), "summary two|new");
    }

    #[test]
    fn an_opaque_block_is_kept_byte_for_byte() {
        let thinking = ContentBlock::opaque("anthropic", json!({"type": "thinking", "signature": "s", "thinking": "…"}));
        let entries = vec![Entry::user("x", vec![]), Entry::assistant(vec![thinking.clone(), ContentBlock::text("y")])];
        assert_eq!(messages_from_entries(&entries)[1].content[0], thinking);
    }
}
