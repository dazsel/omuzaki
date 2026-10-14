use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_doc_api::{Patch, Value};
use omuzaki_history_api::{
    Footprint, Lsn, SnapshotError, SnapshotId, SnapshotInfo, Snapshots, Trigger, Usage, WallClock,
};
use omuzaki_store_api::ContentHash;

use crate::lock;

struct Stored {
    info: SnapshotInfo,
    tree: Value,
}

struct State {
    tree: Value,
    lsn: Lsn,
    snapshots: Vec<Stored>,
    restored: Vec<SnapshotId>,
    saves: usize,
    reload: Patch,
    reloaded: Vec<Vec<String>>,
}

/// A [`Snapshots`] service over a document value the test sets; `created` comes from the injected clock. `take` stores a copy of the current value; its id is
/// the hash of that value, so equal documents get equal ids, as with a real manifest.
pub struct MemorySnapshots {
    clock: Arc<dyn WallClock>,
    state: Mutex<State>,
}

impl MemorySnapshots {
    /// A service whose document is `tree`, at LSN 0, taking times from `clock`.
    pub fn new(tree: Value, clock: Arc<dyn WallClock>) -> Self {
        Self {
            clock,
            state: Mutex::new(State {
                tree,
                lsn: Lsn(0),
                snapshots: Vec::new(),
                restored: Vec::new(),
                saves: 0,
                reload: Patch::default(),
                reloaded: Vec::new(),
            }),
        }
    }

    /// Replaces the current document and the LSN the next snapshot records.
    pub fn set_document(&self, tree: Value, lsn: Lsn) {
        let mut state = lock(&self.state);
        state.tree = tree;
        state.lsn = lsn;
    }

    /// Sets the patch `reload_patch` returns.
    pub fn script_reload(&self, patch: Patch) {
        lock(&self.state).reload = patch;
    }

    /// The ids `restore_outside_tree` was called with, in order.
    pub fn restored(&self) -> Vec<SnapshotId> {
        lock(&self.state).restored.clone()
    }

    /// How many times `save_files` was called.
    pub fn saves(&self) -> usize {
        lock(&self.state).saves
    }

    /// The path lists `reload_patch` was called with, in order.
    pub fn reloaded(&self) -> Vec<Vec<String>> {
        lock(&self.state).reloaded.clone()
    }
}

impl Snapshots for MemorySnapshots {
    fn list(&self) -> Result<Vec<SnapshotInfo>, SnapshotError> {
        Ok(lock(&self.state).snapshots.iter().rev().map(|s| s.info.clone()).collect())
    }

    fn take(&self, trigger: Trigger, name: Option<&str>) -> Result<SnapshotId, SnapshotError> {
        if name.is_some() != matches!(trigger, Trigger::Named) {
            return Err(SnapshotError::Invalid("a name goes with Trigger::Named, and only with it".to_owned()));
        }
        let mut state = lock(&self.state);
        let bytes = serde_json::to_vec(&state.tree).map_err(|e| SnapshotError::Io(e.to_string()))?;
        let id = SnapshotId(ContentHash::of(&bytes));
        let mut info = SnapshotInfo::new(id, state.lsn, "mock-session", self.clock.now(), trigger);
        info.name = name.map(str::to_owned);
        let tree = state.tree.clone();
        state.snapshots.push(Stored { info, tree });
        Ok(id)
    }

    fn tree(&self, id: &SnapshotId) -> Result<Value, SnapshotError> {
        let state = lock(&self.state);
        let found = state.snapshots.iter().find(|s| s.info.id == *id);
        found.map(|s| s.tree.clone()).ok_or(SnapshotError::NotFound(*id))
    }

    fn restore_outside_tree(&self, id: &SnapshotId) -> Result<(), SnapshotError> {
        let mut state = lock(&self.state);
        if !state.snapshots.iter().any(|s| s.info.id == *id) {
            return Err(SnapshotError::NotFound(*id));
        }
        state.restored.push(*id);
        Ok(())
    }

    fn save_files(&self, _timeout: Duration) -> Result<(), SnapshotError> {
        lock(&self.state).saves += 1;
        Ok(())
    }

    fn reload_patch(&self, paths: &[String]) -> Result<Patch, SnapshotError> {
        let mut state = lock(&self.state);
        state.reloaded.push(paths.to_vec());
        Ok(state.reload.clone())
    }

    fn usage(&self) -> Result<Usage, SnapshotError> {
        let state = lock(&self.state);
        let bytes = state.snapshots.iter().map(|s| serde_json::to_vec(&s.tree).map_or(0, |v| v.len() as u64)).sum();
        let mut usage = Usage::default();
        usage.snapshots = Footprint::new(bytes, state.snapshots.len() as u64);
        Ok(usage)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::FakeWallClock;
    use omuzaki_history_api::WallTime;

    fn service(tree: Value) -> (MemorySnapshots, Arc<FakeWallClock>) {
        let clock = Arc::new(FakeWallClock::new(WallTime(1_000)));
        (MemorySnapshots::new(tree, clock.clone()), clock)
    }

    #[test]
    fn list_is_newest_first_and_ids_follow_content() {
        let (snaps, _) = service(json!({"a": 1}));
        let first = snaps.take(Trigger::Manual, None);
        snaps.set_document(json!({"a": 2}), Lsn(9));
        let second = snaps.take(Trigger::Named, Some("v2"));
        assert_ne!(first, second);
        let listed = snaps.list().unwrap_or_default();
        assert_eq!(listed.iter().map(|i| i.lsn).collect::<Vec<_>>(), vec![Lsn(9), Lsn(0)]);
        assert_eq!(listed[0].name.as_deref(), Some("v2"));
        let again = snaps.take(Trigger::Auto, None);
        assert_eq!(again, second);
    }

    #[test]
    fn created_comes_from_the_clock() {
        let (snaps, clock) = service(json!({}));
        clock.advance(500);
        let _ = snaps.take(Trigger::Manual, None);
        assert_eq!(snaps.list().map(|l| l[0].created), Ok(WallTime(1_500)));
    }

    #[test]
    fn tree_returns_the_document_as_it_was() {
        let (snaps, _) = service(json!({"a": 1}));
        let id = snaps.take(Trigger::Manual, None).ok();
        snaps.set_document(json!({"a": 2}), Lsn(1));
        assert_eq!(id.and_then(|id| snaps.tree(&id).ok()), Some(json!({"a": 1})));
    }

    #[test]
    fn unknown_ids_and_misplaced_names_are_refused() {
        let (snaps, _) = service(json!({}));
        let missing = SnapshotId(ContentHash::of(b"nope"));
        assert_eq!(snaps.tree(&missing), Err(SnapshotError::NotFound(missing)));
        assert_eq!(snaps.restore_outside_tree(&missing), Err(SnapshotError::NotFound(missing)));
        assert!(matches!(snaps.take(Trigger::Manual, Some("x")), Err(SnapshotError::Invalid(_))));
        assert!(matches!(snaps.take(Trigger::Named, None), Err(SnapshotError::Invalid(_))));
    }

    #[test]
    fn calls_are_recorded_and_usage_counts_snapshots() {
        let (snaps, _) = service(json!({"a": 1}));
        let id = snaps.take(Trigger::Manual, None).ok();
        assert!(snaps.save_files(Duration::ZERO).is_ok());
        assert!(snaps.reload_patch(&["takes/1.track".to_owned()]).is_ok());
        if let Some(id) = id {
            assert!(snaps.restore_outside_tree(&id).is_ok());
            assert_eq!(snaps.restored(), vec![id]);
        }
        assert_eq!(snaps.saves(), 1);
        assert_eq!(snaps.reloaded(), vec![vec!["takes/1.track".to_owned()]]);
        assert_eq!(snaps.usage().map(|u| u.snapshots.count), Ok(1));
    }
}
