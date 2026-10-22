//! [`MemoryVariables`]: a [`VariableStore`] in memory, counting saves, for the engine's persistence tests.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_live_api::ids::VarName;
use omuzaki_live_api::services::{PersistScope, VarStoreError, VariableStore};
use serde_json::Value;

/// Stored values per scope.
type Stored = BTreeMap<PersistScope, BTreeMap<VarName, Value>>;

/// Values per scope, in memory.
#[derive(Debug, Default)]
pub struct MemoryVariables {
    values: Mutex<Stored>,
    saves: AtomicU64,
}

impl MemoryVariables {
    /// Nothing stored.
    pub fn new() -> Self {
        Self::default()
    }

    /// With `values` already stored for `scope`, as if a previous run saved them.
    #[must_use]
    pub fn with(self, scope: PersistScope, values: BTreeMap<VarName, Value>) -> Self {
        self.lock().insert(scope, values);
        self
    }

    /// How many times [`save`](VariableStore::save) was called: the debounce test reads it.
    pub fn saves(&self) -> u64 {
        self.saves.load(Ordering::Acquire)
    }

    /// What is stored for `scope`.
    pub fn stored(&self, scope: &PersistScope) -> BTreeMap<VarName, Value> {
        self.lock().get(scope).cloned().unwrap_or_default()
    }

    // A panic in another test thread can't leave the map half-written (every write is one insert), so a poisoned
    // lock's data is still good.
    fn lock(&self) -> MutexGuard<'_, Stored> {
        self.values.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl VariableStore for MemoryVariables {
    fn load(&self, scope: &PersistScope) -> Result<BTreeMap<VarName, Value>, VarStoreError> {
        Ok(self.stored(scope))
    }

    fn save(&self, scope: &PersistScope, values: &BTreeMap<VarName, Value>) -> Result<(), VarStoreError> {
        self.lock().insert(scope.clone(), values.clone());
        self.saves.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn saves_are_stored_and_counted() {
        let bonks = VarName::new("bonks").unwrap();
        let store = MemoryVariables::new().with(PersistScope::Global, BTreeMap::from([(bonks.clone(), json!(3))]));
        assert_eq!(store.load(&PersistScope::Global).unwrap()[&bonks], json!(3));
        store.save(&PersistScope::Global, &BTreeMap::from([(bonks.clone(), json!(4))])).unwrap();
        assert_eq!((store.saves(), store.stored(&PersistScope::Global)[&bonks].clone()), (1, json!(4)));
    }
}
