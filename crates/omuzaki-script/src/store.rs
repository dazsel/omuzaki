//! My scripts: the person's own scripts and macros, kept outside any project so they travel across projects like
//! layouts do. This crate defines the contract and an in-memory implementation for tests; the on-disk one
//! (`<config dir>/omuzaki/scripts/<id>.rhai` and `<id>.macro.jsonl`) belongs to the app that wires it.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use omuzaki_ops_api::OpError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a saved item is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScriptKind {
    /// Rhai source.
    Script,
    /// Recorded op calls, as the macro file's JSON lines.
    Macro,
}

/// One saved script or macro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredScript {
    /// The id it is saved under (a slug, so it is safe as a file name).
    pub id: String,
    /// The name shown in lists.
    pub name: String,
    /// Script or macro.
    pub kind: ScriptKind,
    /// The Rhai source, or the macro file's text ([`to_jsonl`](crate::to_jsonl)).
    pub body: String,
    /// The content hash of the body ([`content_hash`](crate::content_hash)).
    pub hash: String,
}

/// Where My scripts live. Taken by the `script.*` ops with `ctx.service::<dyn ScriptStore>()`; without one wired,
/// the `*_mine` ops are `Unavailable`.
pub trait ScriptStore: Send + Sync {
    /// Every saved item, sorted by id.
    ///
    /// # Errors
    /// Whatever the storage fails with.
    fn list(&self) -> Result<Vec<StoredScript>, OpError>;

    /// The item saved under `id`, if there is one.
    ///
    /// # Errors
    /// Whatever the storage fails with.
    fn get(&self, id: &str) -> Result<Option<StoredScript>, OpError>;

    /// Saves `script`, replacing any item with the same id.
    ///
    /// # Errors
    /// Whatever the storage fails with.
    fn put(&self, script: StoredScript) -> Result<(), OpError>;

    /// Removes the item saved under `id`; says whether there was one.
    ///
    /// # Errors
    /// Whatever the storage fails with.
    fn remove(&self, id: &str) -> Result<bool, OpError>;
}

/// A [`ScriptStore`] in memory, for tests and for apps with nowhere to keep scripts yet.
#[derive(Debug, Default)]
pub struct MemScriptStore {
    items: Mutex<BTreeMap<String, StoredScript>>,
}

impl MemScriptStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn items(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, StoredScript>> {
        // A panic while the lock was held can't leave the map half-updated: every method makes one call on it.
        self.items.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ScriptStore for MemScriptStore {
    fn list(&self) -> Result<Vec<StoredScript>, OpError> {
        Ok(self.items().values().cloned().collect())
    }

    fn get(&self, id: &str) -> Result<Option<StoredScript>, OpError> {
        Ok(self.items().get(id).cloned())
    }

    fn put(&self, script: StoredScript) -> Result<(), OpError> {
        self.items().insert(script.id.clone(), script);
        Ok(())
    }

    fn remove(&self, id: &str) -> Result<bool, OpError> {
        Ok(self.items().remove(id).is_some())
    }
}
