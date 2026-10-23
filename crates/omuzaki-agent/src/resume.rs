//! Continuing a stored conversation: the model's messages rebuilt from the stored entries (D7, #1395).

use std::collections::HashMap;

use omuzaki_agent_api::entry::{AnswerId, Entry, StoredBlock};
use omuzaki_llm_api::message::{ContentBlock, ImageOrigin, Message, Role};

use crate::context::person_text;
use crate::cut::{assistant_blocks_mut, assistant_text_bytes, cut_history, cut_text_blocks};
use crate::sink::EntrySink;

/// The result a trailing tool use gets when the studio closed before the call finished.
const INTERRUPTED: &str = "interrupted: the studio closed";
/// The result a call gets when the answer was stopped before it ran.
const STOPPED_BEFORE_RUN: &str = "not run: stopped by the person";

/// Rebuilds the model's messages from stored entries: each message with the context block it was given (its text; the
/// images of the UI context are not stored), images of results come back as a note, since this variant reads no blobs.
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
    /// Where the answer in progress starts in `messages`, for a Stop to cut its text.
    answer_from: usize,
    /// Bytes of the answer's text that sit before a compaction, so are not in `messages`.
    answer_skip: usize,
    /// The answer in progress, as its `User` entry names it.
    answer: Option<AnswerId>,
    /// Where the person's text of the answer in progress is: its message and block, for its context to go before it.
    person_at: Option<(usize, usize)>,
}

impl<'a> Rebuild<'a> {
    fn new(sink: Option<&'a dyn EntrySink>) -> Self {
        Self {
            sink,
            messages: Vec::new(),
            open: Vec::new(),
            results: HashMap::new(),
            answer_from: 0,
            answer_skip: 0,
            answer: None,
            person_at: None,
        }
    }

    fn run(mut self, entries: &[Entry]) -> Vec<Message> {
        let start = entries.iter().rposition(|e| matches!(e, Entry::Compacted { .. }));
        let tail = match start {
            Some(at) => {
                if let Entry::Compacted { message, render, .. } = &entries[at] {
                    let mut blocks = vec![ContentBlock::text(message.clone())];
                    // The render the rebuilt message kept comes back from its blob, after the text.
                    blocks.extend(self.content(&render.iter().cloned().collect::<Vec<_>>()));
                    self.push(Role::User, blocks);
                    self.answer_from = self.messages.len();
                    // Stop counted the answer's text from its start, which may be before this compaction.
                    let user = entries[..at].iter().rposition(|e| matches!(e, Entry::User { .. }));
                    self.answer = user.and_then(|u| entries[u].answer_id());
                    self.answer_skip = assistant_text_bytes(&entries[user.map_or(0, |u| u + 1)..at]);
                }
                &entries[at + 1..]
            }
            None => entries,
        };
        for (at, entry) in tail.iter().enumerate() {
            match entry {
                Entry::User { text, chips, answer, .. } => {
                    self.close_open(false);
                    self.push(Role::User, vec![ContentBlock::text(person_text(text, chips))]);
                    self.person_at =
                        self.messages.len().checked_sub(1).map(|m| (m, self.messages[m].content.len() - 1));
                    self.answer_from = self.messages.len();
                    self.answer_skip = 0;
                    self.answer = *answer;
                }
                // The context the message was given goes before the person's text, as it was sent.
                Entry::Context { text, answer, .. } if !text.is_empty() && *answer == self.answer => {
                    if let Some((m, b)) = self.person_at.take() {
                        self.messages[m].content.insert(b, ContentBlock::text(text.clone()));
                    }
                }
                // Stop cut its answer's text to what the person had seen; the turns stored before it were whole.
                Entry::Stopped { text_read, answer, .. } => {
                    self.close_open(true);
                    if let Some(read) = text_read
                        && stop_applies(*answer, self.answer)
                    {
                        cut_history(&mut self.messages, self.answer_from, *read, self.answer_skip);
                    }
                }
                Entry::Assistant { blocks, .. } => {
                    self.close_open(stopped_before(tail, at));
                    self.open = blocks
                        .iter()
                        .filter_map(|b| if let ContentBlock::ToolUse { id, .. } = b { Some(id.clone()) } else { None })
                        .collect();
                    self.messages.push(Message::new(Role::Assistant, blocks.clone()));
                }
                // A result that arrived after Stop (its id ends in `:late`) answers nothing; it is kept in the log only.
                Entry::ToolResult { id, ok, blocks, .. } if !id.ends_with(":late") => {
                    self.results.insert(id.clone(), (*ok, blocks.clone()));
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
                StoredBlock::Image { mime, blob, .. } => {
                    self.sink.and_then(|sink| sink.read_blob(blob, *mime)).map_or_else(
                        || ContentBlock::text("[image no longer stored]"),
                        |bytes| ContentBlock::image(*mime, bytes, ImageOrigin::Render),
                    )
                }
                _ => ContentBlock::text("[content omitted]"),
            })
            .collect()
    }
}

/// Whether Stop was pressed between the previous turn and position `at`.
fn stopped_before(tail: &[Entry], at: usize) -> bool {
    tail[..at]
        .iter()
        .rev()
        .take_while(|e| !matches!(e, Entry::User { .. } | Entry::Assistant { .. }))
        .any(|e| matches!(e, Entry::Stopped { .. }))
}

/// Whether a `Stopped` entry naming answer `stopped` applies to the answer in progress, `current` (the one the latest
/// `User` entry names). A numbered cutoff applies to its own answer only, so one recorded for an answer must never cut
/// another; an unnumbered one (a log written before answers were numbered) applies to the latest answer.
pub(crate) fn stop_applies(stopped: Option<AnswerId>, current: Option<AnswerId>) -> bool {
    stopped.is_none() || stopped == current
}

/// The number of answers in `entries`: the highest an entry names, or the count of the person's messages if that is
/// more (a log written before answers were numbered). The next answer is numbered one higher.
pub(crate) fn answers_in(entries: &[Entry]) -> u64 {
    let numbered = entries.iter().filter_map(Entry::answer_id).map(|a| a.0).max().unwrap_or(0);
    let messages = entries.iter().filter(|e| matches!(e, Entry::User { .. })).count();
    numbered.max(u64::try_from(messages).unwrap_or(u64::MAX))
}

/// The entries as the person saw them: where a Stop recorded how much of its answer was read, the text of that answer's
/// earlier turns (stored whole, before the Stop) is cut to it.
pub(crate) fn apply_stop_cuts(mut entries: Vec<Entry>) -> Vec<Entry> {
    let mut answer_from = 0;
    let mut current = None;
    for at in 0..entries.len() {
        match &entries[at] {
            Entry::User { answer, .. } => {
                answer_from = at + 1;
                current = *answer;
            }
            Entry::Stopped { text_read: Some(read), answer, .. } if stop_applies(*answer, current) => {
                let read = *read;
                cut_text_blocks(assistant_blocks_mut(&mut entries[answer_from..at]), read);
            }
            _ => {}
        }
    }
    entries
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
            .filter_map(|b| {
                if let ContentBlock::ToolResult { id, is_error, .. } = b {
                    Some((id.as_str(), *is_error))
                } else {
                    None
                }
            })
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
            Entry::stopped(None),
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

    struct Blobs;

    impl EntrySink for Blobs {
        fn append(&self, _entry: &Entry) -> Result<(), omuzaki_agent_api::error::HostError> {
            Ok(())
        }

        fn read_blob(&self, hash: &str, _mime: omuzaki_llm_api::message::ImageMime) -> Option<Vec<u8>> {
            (hash == "aa").then(|| vec![137, 80, 78, 71])
        }
    }

    #[test]
    fn a_compaction_brings_its_render_back_from_the_blob() {
        use omuzaki_llm_api::message::ImageMime;
        let entries = vec![
            Entry::compacted_with_render("summary", StoredBlock::image(ImageMime::Png, "aa")),
            Entry::user("next", vec![]),
        ];
        let messages = messages_from_entries_with(&entries, &Blobs);
        let kinds: Vec<&str> = messages[0]
            .content
            .iter()
            .map(|b| match b {
                ContentBlock::Text(_) => "text",
                ContentBlock::Image { .. } => "image",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["text", "image", "text"]);
    }

    fn assistant_text(messages: &[Message]) -> String {
        messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .flat_map(|m| &m.content)
            .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
            .collect()
    }

    #[test]
    fn a_cutoff_cuts_only_the_answer_it_names() {
        // A cutoff written for answer 2 before answer 2's message (as a Stop during the close of answer 1's changes
        // once was) must not touch answer 1.
        let entries = vec![
            Entry::user("one", vec![]).in_answer(AnswerId(1)),
            Entry::assistant(vec![ContentBlock::text("first answer")]),
            Entry::stopped(Some(0)).in_answer(AnswerId(2)),
            Entry::user("two", vec![]).in_answer(AnswerId(2)),
            Entry::stopped(Some(0)).in_answer(AnswerId(2)),
        ];
        assert_eq!(assistant_text(&messages_from_entries(&entries)), "first answer");
        let shown = apply_stop_cuts(entries.clone());
        assert_eq!(shown[1], entries[1], "the view keeps it too");
    }

    #[test]
    fn an_unnumbered_cutoff_cuts_the_latest_answer_as_before() {
        let entries = vec![
            Entry::user("one", vec![]),
            Entry::assistant(vec![ContentBlock::text("abcdef")]),
            Entry::stopped(Some(3)),
        ];
        assert_eq!(assistant_text(&messages_from_entries(&entries)), "abc");
        assert_eq!(apply_stop_cuts(entries)[1], Entry::assistant(vec![ContentBlock::text("abc")]));
    }

    #[test]
    fn answers_are_counted_from_the_numbers_entries_carry_or_the_messages() {
        let numbered = [Entry::user("a", vec![]).in_answer(AnswerId(7)), Entry::stopped(None).in_answer(AnswerId(7))];
        assert_eq!(answers_in(&numbered), 7);
        assert_eq!(answers_in(&[Entry::user("a", vec![]), Entry::user("b", vec![])]), 2, "an older log");
        assert_eq!(answers_in(&[]), 0);
    }

    #[test]
    fn a_stop_cuts_the_text_of_the_answers_earlier_turns_to_what_was_read() {
        let entries = vec![
            Entry::user("go", vec![]),
            Entry::assistant(vec![ContentBlock::text("abcd"), ContentBlock::tool_use("c", "t", json!({}))]),
            Entry::tool_result("c", true, vec![StoredBlock::Text("r".into())]),
            Entry::assistant(vec![ContentBlock::text("efgh")]),
            Entry::stopped(Some(6)),
        ];
        let text: String = messages_from_entries(&entries)
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .flat_map(|m| &m.content)
            .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
            .collect();
        assert_eq!(text, "abcdef");
        let shown: String = apply_stop_cuts(entries)
            .iter()
            .filter_map(|e| if let Entry::Assistant { blocks, .. } = e { Some(blocks) } else { None })
            .flatten()
            .filter_map(|b| if let ContentBlock::Text(t) = b { Some(t.as_str()) } else { None })
            .collect();
        assert_eq!(shown, "abcdef", "the view agrees with the history");
    }

    #[test]
    fn an_opaque_block_is_kept_byte_for_byte() {
        let thinking =
            ContentBlock::opaque("anthropic", json!({"type": "thinking", "signature": "s", "thinking": "…"}));
        let entries = vec![Entry::user("x", vec![]), Entry::assistant(vec![thinking.clone(), ContentBlock::text("y")])];
        assert_eq!(messages_from_entries(&entries)[1].content[0], thinking);
    }
}
