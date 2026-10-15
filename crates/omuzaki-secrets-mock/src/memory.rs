//! [`MemorySecrets`].

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_secrets_api::{SecretError, SecretLabel, SecretStore, SecretValue};

#[derive(Default)]
struct State {
    secrets: BTreeMap<SecretLabel, SecretValue>,
    failure: Option<SecretError>,
}

/// A [`SecretStore`] in memory.
#[derive(Default)]
pub struct MemorySecrets {
    state: Mutex<State>,
}

impl MemorySecrets {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every later call fail with `error` until [`clear_failure`](Self::clear_failure), as a missing or locked
    /// keychain would. Nothing is read or changed while it fails.
    pub fn fail_with(&self, error: SecretError) {
        self.state().failure = Some(error);
    }

    /// Ends [`fail_with`](Self::fail_with).
    pub fn clear_failure(&self) {
        self.state().failure = None;
    }

    // A panic in another test thread cannot leave the map half-updated (each call is one map operation), so a
    // poisoned lock is still safe to use.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn working(&self) -> Result<MutexGuard<'_, State>, SecretError> {
        let state = self.state();
        match &state.failure {
            Some(error) => Err(error.clone()),
            None => Ok(state),
        }
    }
}

impl fmt::Debug for MemorySecrets {
    /// Lists the labels, never the values.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state();
        f.debug_struct("MemorySecrets").field("labels", &state.secrets.keys().collect::<Vec<_>>()).finish()
    }
}

impl SecretStore for MemorySecrets {
    fn get(&self, label: &SecretLabel) -> Result<Option<SecretValue>, SecretError> {
        Ok(self.working()?.secrets.get(label).map(|v| SecretValue::new(v.expose())))
    }

    fn contains(&self, label: &SecretLabel) -> Result<bool, SecretError> {
        Ok(self.working()?.secrets.contains_key(label))
    }

    fn labels(&self) -> Result<Vec<SecretLabel>, SecretError> {
        Ok(self.working()?.secrets.keys().cloned().collect())
    }

    fn set(&self, label: &SecretLabel, value: SecretValue) -> Result<(), SecretError> {
        let mut state = self.working()?;
        value.check_storable()?;
        state.secrets.insert(label.clone(), value);
        Ok(())
    }

    fn delete(&self, label: &SecretLabel) -> Result<(), SecretError> {
        self.working()?.secrets.remove(label);
        Ok(())
    }
}
