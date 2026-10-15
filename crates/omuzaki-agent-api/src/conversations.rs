//! Stored conversations: the list the panel and Settings > Privacy show, and the rationale behind "Explain this" (D7).

use std::path::Path;

use omuzaki_ops_api::TxId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::entry::Entry;
use crate::error::HostError;

/// Identifies one stored conversation within a project.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ConversationId(pub u64);

/// One row of the conversation list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ConversationInfo {
    /// Which.
    pub id: ConversationId,
    /// When it was created, RFC 3339.
    pub created: String,
    /// When its last entry was written, RFC 3339.
    pub last_entry: String,
    /// The model it started with.
    pub model: String,
    /// The person's first message, cut to 80 characters.
    pub first_line: String,
    /// Its session was revoked: continuing it opens a fresh session.
    pub revoked: bool,
}

impl ConversationInfo {
    /// A row of the list.
    pub fn new(
        id: ConversationId,
        created: impl Into<String>,
        last_entry: impl Into<String>,
        model: impl Into<String>,
        first_line: impl Into<String>,
        revoked: bool,
    ) -> Self {
        Self {
            id,
            created: created.into(),
            last_entry: last_entry.into(),
            model: model.into(),
            first_line: first_line.into(),
            revoked,
        }
    }
}

/// A stored conversation read back for the panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Resumed {
    /// Every entry, in order.
    pub entries: Vec<Entry>,
    /// The last answer was cut short by a crash; the panel shows "interrupted: the studio closed".
    pub interrupted: bool,
}

impl Resumed {
    /// A conversation read back.
    pub fn new(entries: Vec<Entry>, interrupted: bool) -> Self {
        Self { entries, interrupted }
    }
}

/// The stored conversations of one project (D7). Answered by the conversation store.
pub trait Conversations: Send + Sync {
    /// This project's conversations, newest first.
    ///
    /// # Errors
    /// [`HostError::Store`] if the store cannot be read.
    fn list(&self) -> Result<Vec<ConversationInfo>, HostError>;

    /// Reads one conversation for display.
    ///
    /// # Errors
    /// [`HostError::Store`] for an unknown id or an unreadable log.
    fn open(&self, id: ConversationId) -> Result<Resumed, HostError>;

    /// Deletes one conversation and the blobs only it used.
    ///
    /// # Errors
    /// [`HostError::Store`] for an unknown id or a failed delete.
    fn delete(&self, id: ConversationId) -> Result<(), HostError>;

    /// Deletes every conversation of this project, and no other project's.
    ///
    /// # Errors
    /// [`HostError::Store`] if a delete fails.
    fn delete_all(&self) -> Result<(), HostError>;

    /// Writes a Markdown transcript with its images beside it, plus the raw `log.jsonl`, into `dest`.
    ///
    /// # Errors
    /// [`HostError::Store`] for an unknown id or a failed write.
    fn export(&self, id: ConversationId, dest: &Path) -> Result<(), HostError>;
}

/// Why the assistant changed what it changed: the text it wrote during the answer that made a step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Rationale {
    /// The text, cut to 2,000 characters by the store.
    pub text: String,
    /// The conversation it comes from.
    pub conversation: ConversationId,
}

impl Rationale {
    /// A rationale from `conversation`.
    pub fn new(text: impl Into<String>, conversation: ConversationId) -> Self {
        Self { text: text.into(), conversation }
    }
}

/// Where the Theatre's step card asks for "Explain this" (TH-08). The rationale quotes private chat, so it lives in the
/// conversation store and never in the journal, which travels with the project.
pub trait RationaleSource: Send + Sync {
    /// The rationale for the step `tx`, if an assistant conversation of this project made it.
    fn rationale(&self, tx: TxId) -> Option<Rationale>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_and_a_resumed_conversation_survive_json() {
        let info =
            ConversationInfo::new(ConversationId(3), "2026-10-14T10:00:00Z", "2026-10-14T10:05:00Z", "m", "hi", true);
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(serde_json::from_str::<ConversationInfo>(&json).unwrap(), info);

        let resumed = Resumed::new(vec![Entry::user("hi", Vec::new()), Entry::Stopped], true);
        let json = serde_json::to_string(&resumed).unwrap();
        assert_eq!(serde_json::from_str::<Resumed>(&json).unwrap(), resumed);
    }
}
