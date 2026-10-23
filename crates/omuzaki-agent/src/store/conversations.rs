//! The panel's and Settings > Privacy's view of the store: [`Conversations`] over [`ConversationStore`].

use std::path::Path;

use omuzaki_agent_api::conversations::{ConversationId, ConversationInfo, Conversations, Resumed};
use omuzaki_agent_api::error::HostError;

use super::ConversationStore;

impl Conversations for ConversationStore {
    fn list(&self) -> Result<Vec<ConversationInfo>, HostError> {
        ConversationStore::list(self)
    }

    /// Opening is resuming: the entries and whether the last answer was cut short.
    fn open(&self, id: ConversationId) -> Result<Resumed, HostError> {
        self.resume(id)
    }

    fn delete(&self, id: ConversationId) -> Result<(), HostError> {
        ConversationStore::delete(self, id)
    }

    fn delete_all(&self) -> Result<(), HostError> {
        ConversationStore::delete_all(self)
    }

    fn export(&self, id: ConversationId, dest: &Path) -> Result<(), HostError> {
        ConversationStore::export(self, id, dest)
    }
}
