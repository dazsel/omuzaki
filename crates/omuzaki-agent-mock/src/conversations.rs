//! [`ScriptedConversations`]: an in-memory `Conversations`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_agent_api::conversations::{ConversationId, ConversationInfo, Conversations, Resumed};
use omuzaki_agent_api::error::HostError;

#[derive(Default)]
struct State {
    conversations: Vec<(ConversationInfo, Resumed)>,
    deleted: Vec<ConversationId>,
    exported: Vec<(ConversationId, PathBuf)>,
}

/// A scripted `Conversations`: an in-memory list, with the deletes and exports it was asked for recorded. It never
/// touches the disk.
pub struct ScriptedConversations {
    state: Mutex<State>,
}

impl ScriptedConversations {
    /// A store holding `conversations`.
    pub fn new(conversations: Vec<(ConversationInfo, Resumed)>) -> Self {
        Self { state: Mutex::new(State { conversations, ..State::default() }) }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every id deleted so far, in order. `delete_all` records each id it removed, in list order.
    pub fn deleted(&self) -> Vec<ConversationId> {
        self.lock().deleted.clone()
    }

    /// Every `(id, destination)` exported so far, in order.
    pub fn exported(&self) -> Vec<(ConversationId, PathBuf)> {
        self.lock().exported.clone()
    }
}

fn unknown(id: ConversationId) -> HostError {
    HostError::Store(format!("no conversation {}", id.0))
}

impl Conversations for ScriptedConversations {
    /// Newest first by `last_entry`, compared as strings (RFC 3339 sorts as time); ties keep the stored order.
    fn list(&self) -> Result<Vec<ConversationInfo>, HostError> {
        let mut rows: Vec<ConversationInfo> = self.lock().conversations.iter().map(|(info, _)| info.clone()).collect();
        rows.sort_by(|a, b| b.last_entry.cmp(&a.last_entry));
        Ok(rows)
    }

    fn open(&self, id: ConversationId) -> Result<Resumed, HostError> {
        let state = self.lock();
        state
            .conversations
            .iter()
            .find(|(info, _)| info.id == id)
            .map(|(_, resumed)| resumed.clone())
            .ok_or_else(|| unknown(id))
    }

    fn delete(&self, id: ConversationId) -> Result<(), HostError> {
        let mut state = self.lock();
        let at = state.conversations.iter().position(|(info, _)| info.id == id).ok_or_else(|| unknown(id))?;
        state.conversations.remove(at);
        state.deleted.push(id);
        Ok(())
    }

    fn delete_all(&self) -> Result<(), HostError> {
        let mut state = self.lock();
        let removed: Vec<ConversationId> = state.conversations.drain(..).map(|(info, _)| info.id).collect();
        state.deleted.extend(removed);
        Ok(())
    }

    fn export(&self, id: ConversationId, dest: &Path) -> Result<(), HostError> {
        let mut state = self.lock();
        if !state.conversations.iter().any(|(info, _)| info.id == id) {
            return Err(unknown(id));
        }
        state.exported.push((id, dest.to_path_buf()));
        Ok(())
    }
}
