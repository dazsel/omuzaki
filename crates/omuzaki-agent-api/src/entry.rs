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
        /// The hash in lower-case hex; the bytes are in the conversation's `blobs/` folder.
        blob: String,
    },
}

impl StoredBlock {
    /// An image kept as the blob `blob`.
    pub fn image(mime: ImageMime, blob: impl Into<String>) -> Self {
        Self::Image { mime, blob: blob.into() }
    }
}

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
    },
    /// The person pressed Stop.
    Stopped,
}

impl Entry {
    /// The person's message.
    pub fn user(text: impl Into<String>, chips: Vec<ContextChip>) -> Self {
        Self::User { text: text.into(), chips }
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
        Self::Compacted { message: message.into() }
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
            Entry::user("make her face more oval", vec![ContextChip::Viewport]),
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
            Entry::Stopped,
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
            "stopped",
        ];
        for (entry, kind) in every_entry().into_iter().zip(kinds) {
            let value = serde_json::to_value(&entry).unwrap();
            assert_eq!(value["kind"], kind);
            assert_eq!(serde_json::from_value::<Entry>(value).unwrap(), entry);
        }
    }
}
