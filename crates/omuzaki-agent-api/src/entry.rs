//! [`Entry`]: the kinds a conversation is stored as and `transcript()` returns (D7). One variant per fact.

use omuzaki_llm_api::message::{ContentBlock, ImageMime};
use omuzaki_llm_api::usage::Usage;
use omuzaki_ops_api::{ApprovalId, EndReason, Need, Resolution, SessionId, TxId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::QuestionId;
use crate::message::ContextChip;

/// A block of a stored tool result: text, or an image kept as a blob so the log stays small.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum StoredBlock {
    /// Text.
    Text(String),
    /// An image, named by the BLAKE3 hash of its bytes.
    #[non_exhaustive]
    Image {
        /// Its encoding.
        mime: ImageMime,
        /// The hash in lower-case hex; the bytes are in the project's `blobs/` folder, shared by its conversations.
        blob: String,
    },
}

impl StoredBlock {
    /// An image kept as the blob `blob`.
    pub fn image(mime: ImageMime, blob: impl Into<String>) -> Self {
        Self::Image { mime, blob: blob.into() }
    }
}

/// One answer of a conversation: the person's messages counted from 1 in the order they were sent. The entries that
/// belong to one answer and change how it reads (the message itself, and Stop's cutoff) name it, so a replay applies
/// each to that answer and to no other.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct AnswerId(pub u64);

/// What happened to the session a conversation runs in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum SessionNote {
    /// A session opened.
    Opened,
    /// The session ended, and why. A revoked one shows in the conversation list.
    Ended(EndReason),
}

// One entry is built per fact and then stored: boxing the approval would only change the contract's shape.
#[allow(clippy::large_enum_variant)]
/// One fact of a conversation. Entries are appended in the order they happened and never edited.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Entry {
    /// The person's message.
    #[non_exhaustive]
    User {
        /// What they typed.
        text: String,
        /// What they attached.
        chips: Vec<ContextChip>,
        /// The answer this message starts; `None` in logs written before answers were numbered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<AnswerId>,
    },
    /// The model's final content blocks for one turn; opaque ones byte for byte.
    #[non_exhaustive]
    Assistant {
        /// The blocks.
        blocks: Vec<ContentBlock>,
    },
    /// A tool call the model made.
    #[non_exhaustive]
    ToolCall {
        /// The call id.
        id: String,
        /// The tool.
        tool: String,
        /// The row the panel showed.
        summary: String,
    },
    /// What a call returned.
    #[non_exhaustive]
    ToolResult {
        /// The call id.
        id: String,
        /// Whether it succeeded.
        ok: bool,
        /// The result, images as blob hashes.
        blocks: Vec<StoredBlock>,
    },
    /// An approval the answer waited on.
    #[non_exhaustive]
    Approval {
        /// Which approval.
        id: ApprovalId,
        /// What it asked for.
        need: Need,
        /// How it ended; `None` while pending.
        resolution: Option<Resolution>,
    },
    /// A question put to the person.
    #[non_exhaustive]
    Question {
        /// Which question.
        id: QuestionId,
        /// The question.
        text: String,
        /// Suggested answers.
        choices: Vec<String>,
    },
    /// The person's answer.
    #[non_exhaustive]
    Answer {
        /// Which question.
        id: QuestionId,
        /// The reply.
        text: String,
    },
    /// What one turn cost.
    #[non_exhaustive]
    Charge {
        /// The final usage.
        usage: Usage,
        /// The charge in micro-dollars.
        usd_micros: u64,
    },
    /// An undo step the answer made, with the rationale for Explain this (D7).
    #[non_exhaustive]
    Step {
        /// The transaction.
        tx: TxId,
        /// Its label.
        label: String,
        /// The text the model wrote during the answer, cut to 2,000 characters.
        rationale: String,
    },
    /// A session opened or ended.
    #[non_exhaustive]
    Session {
        /// Which session.
        id: SessionId,
        /// What happened.
        note: SessionNote,
    },
    /// The history was compacted: the one rebuilt first user message (the summary and the kept set), so a resume
    /// can start from it (D5).
    #[non_exhaustive]
    Compacted {
        /// The rebuilt message.
        message: String,
        /// The latest render the rebuilt message kept, as a blob, so a resume can put it back.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        render: Option<StoredBlock>,
    },
    /// The person pressed Stop.
    #[non_exhaustive]
    Stopped {
        /// How many bytes of the answer's text the person had been shown. The answer's earlier turns were stored
        /// whole, so a replay cuts their text to this; `None` when it is not known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text_read: Option<usize>,
        /// The answer that was stopped. A replay cuts only that answer, and only while it is the latest one; `None`
        /// (logs written before answers were numbered) means the answer of the latest `User` entry before it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<AnswerId>,
    },
    /// The context block the host gave the model before the person's text (D5): the UI context, the project summary,
    /// what others changed, results that arrived after a Stop. Stored before anything it carries counts as told, so a
    /// compaction and a continued conversation give the model what it was given.
    #[non_exhaustive]
    Context {
        /// The answer it opened.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<AnswerId>,
        /// Its text block, as sent; images are not kept.
        text: String,
    },
    /// Changes by others the model is owed: a heads-up of a result it did not receive, or what the host's own read of
    /// the op log returned that no heads-up had told. Owed until the next `Context` entry, which carries it.
    #[non_exhaustive]
    Notice {
        /// The op-log events it accounts for: numbered above `after`, up to `upto`.
        after: u64,
        /// See `after`.
        upto: u64,
        /// What the model is told.
        text: String,
    },
    /// A transaction the executor confirmed closed whose outcome the op log has not shown yet. It becomes a `Step` when
    /// the log shows it committed, or is `Settled` when it shows it rolled back. Stored so a host reopened before then
    /// still reads it.
    #[non_exhaustive]
    Retained {
        /// The transaction.
        tx: TxId,
        /// The label of the step it would be.
        label: String,
        /// The rationale of the step it would be, cut like a step's.
        rationale: String,
        /// Its outcome is in the op log above this event number.
        after: u64,
    },
    /// A retained transaction that did not become a step: the log showed it rolled back, or can no longer show it.
    #[non_exhaustive]
    Settled {
        /// The transaction.
        tx: TxId,
    },
}

impl Entry {
    /// The person's message.
    pub fn user(text: impl Into<String>, chips: Vec<ContextChip>) -> Self {
        Self::User { text: text.into(), chips, answer: None }
    }

    /// The model's blocks for one turn.
    pub fn assistant(blocks: Vec<ContentBlock>) -> Self {
        Self::Assistant { blocks }
    }

    /// A tool call.
    pub fn tool_call(id: impl Into<String>, tool: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::ToolCall { id: id.into(), tool: tool.into(), summary: summary.into() }
    }

    /// A tool result.
    pub fn tool_result(id: impl Into<String>, ok: bool, blocks: Vec<StoredBlock>) -> Self {
        Self::ToolResult { id: id.into(), ok, blocks }
    }

    /// An approval and how it ended.
    pub fn approval(id: ApprovalId, need: Need, resolution: Option<Resolution>) -> Self {
        Self::Approval { id, need, resolution }
    }

    /// A question.
    pub fn question(id: QuestionId, text: impl Into<String>, choices: Vec<String>) -> Self {
        Self::Question { id, text: text.into(), choices }
    }

    /// An answer.
    pub fn answer(id: QuestionId, text: impl Into<String>) -> Self {
        Self::Answer { id, text: text.into() }
    }

    /// What a turn cost.
    pub fn charge(usage: Usage, usd_micros: u64) -> Self {
        Self::Charge { usage, usd_micros }
    }

    /// An undo step with its rationale.
    pub fn step(tx: TxId, label: impl Into<String>, rationale: impl Into<String>) -> Self {
        Self::Step { tx, label: label.into(), rationale: rationale.into() }
    }

    /// A session event.
    pub fn session(id: SessionId, note: SessionNote) -> Self {
        Self::Session { id, note }
    }

    /// A compaction and the message it rebuilt.
    pub fn compacted(message: impl Into<String>) -> Self {
        Self::Compacted { message: message.into(), render: None }
    }

    /// A compaction whose rebuilt message kept a render, stored as the blob `render`.
    pub fn compacted_with_render(message: impl Into<String>, render: StoredBlock) -> Self {
        Self::Compacted { message: message.into(), render: Some(render) }
    }

    /// The person pressed Stop after being shown `text_read` bytes of the answer's text.
    pub fn stopped(text_read: Option<usize>) -> Self {
        Self::Stopped { text_read, answer: None }
    }

    /// The context block of a message.
    pub fn context(text: impl Into<String>) -> Self {
        Self::Context { answer: None, text: text.into() }
    }

    /// Changes by others owed to the model, for the events numbered above `after` up to `upto`.
    pub fn notice(after: u64, upto: u64, text: impl Into<String>) -> Self {
        Self::Notice { after, upto, text: text.into() }
    }

    /// A transaction whose outcome the op log will show above event `after`.
    pub fn retained(tx: TxId, label: impl Into<String>, rationale: impl Into<String>, after: u64) -> Self {
        Self::Retained { tx, label: label.into(), rationale: rationale.into(), after }
    }

    /// A retained transaction that made no step.
    pub fn settled(tx: TxId) -> Self {
        Self::Settled { tx }
    }

    /// The same entry, naming `id` as its answer. Only a `User`, a `Stopped` and a `Context` entry carry one; others are
    /// unchanged.
    #[must_use]
    pub fn in_answer(mut self, id: AnswerId) -> Self {
        match &mut self {
            Self::User { answer, .. } | Self::Stopped { answer, .. } | Self::Context { answer, .. } => {
                *answer = Some(id);
            }
            _ => {}
        }
        self
    }

    /// The answer a `User`, `Stopped` or `Context` entry names, if it names one.
    pub fn answer_id(&self) -> Option<AnswerId> {
        match self {
            Self::User { answer, .. } | Self::Stopped { answer, .. } | Self::Context { answer, .. } => *answer,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::time::Seconds;
    use omuzaki_ops_api::{Actor, Resolved};
    use serde_json::json;

    use super::*;

    fn every_entry() -> Vec<Entry> {
        let resolution = Resolution {
            id: ApprovalId(7),
            by: None,
            result: Resolved::Rejected { comment: Some("no".into()) },
            at: Seconds(3.0),
        };
        vec![
            Entry::user("make her face more oval", vec![ContextChip::Viewport]).in_answer(AnswerId(3)),
            Entry::assistant(vec![
                ContentBlock::text("Let me look."),
                ContentBlock::opaque("anthropic", json!({"type": "thinking", "signature": "abc"})),
                ContentBlock::tool_use("c1", "render_preview", json!({"view": "front"})),
            ]),
            Entry::tool_call("c1", "render_preview", "Preview: front"),
            Entry::tool_result(
                "c1",
                true,
                vec![StoredBlock::Text("ok".into()), StoredBlock::image(ImageMime::Png, "ab".repeat(32))],
            ),
            Entry::approval(ApprovalId(7), Need::Person { reason: "model switch".into() }, Some(resolution)),
            Entry::question(QuestionId(1), "Which?", vec!["a".into()]),
            Entry::answer(QuestionId(1), "a"),
            Entry::charge(Usage::new(4, 61).with_cache_write(6_262), 814),
            Entry::step(TxId(31), "Assistant: make her face more oval", "the jaw reads round because"),
            Entry::session(SessionId(2), SessionNote::Opened),
            Entry::session(
                SessionId(2),
                SessionNote::Ended(EndReason::Revoked { by: Actor::Person { name: "sam".into() }, credential: true }),
            ),
            Entry::compacted("Summary: ..."),
            Entry::compacted_with_render("Summary: ...", StoredBlock::image(ImageMime::Png, "cd".repeat(32))),
            Entry::stopped(None),
            Entry::stopped(Some(120)).in_answer(AnswerId(3)),
            Entry::context("Context for this message: ...").in_answer(AnswerId(3)),
            Entry::notice(4, 9, "hair.set_colour by Sam"),
            Entry::retained(TxId(31), "Assistant: oval (stopped)", "the jaw reads round", 12),
            Entry::settled(TxId(32)),
        ]
    }

    #[test]
    fn every_entry_survives_json_and_names_its_kind() {
        let kinds = [
            "user",
            "assistant",
            "tool_call",
            "tool_result",
            "approval",
            "question",
            "answer",
            "charge",
            "step",
            "session",
            "session",
            "compacted",
            "compacted",
            "stopped",
            "stopped",
            "context",
            "notice",
            "retained",
            "settled",
        ];
        for (entry, kind) in every_entry().into_iter().zip(kinds) {
            let value = serde_json::to_value(&entry).unwrap();
            assert_eq!(value["kind"], kind);
            assert_eq!(serde_json::from_value::<Entry>(value).unwrap(), entry);
        }
    }

    #[test]
    fn an_entry_without_an_answer_reads_back_as_before_and_one_with_names_it() {
        let old = serde_json::to_value(Entry::stopped(Some(5))).unwrap();
        assert_eq!(old, json!({"kind": "stopped", "text_read": 5}), "nothing new is written for an unnumbered entry");
        let numbered = Entry::stopped(Some(5)).in_answer(AnswerId(2));
        assert_eq!(serde_json::to_value(&numbered).unwrap()["answer"], 2);
        assert_eq!(numbered.answer_id(), Some(AnswerId(2)));
        assert_eq!(Entry::charge(Usage::new(1, 1), 1).in_answer(AnswerId(2)).answer_id(), None, "not a charge");
        assert_eq!(Entry::context("c").in_answer(AnswerId(2)).answer_id(), Some(AnswerId(2)));
    }
}
