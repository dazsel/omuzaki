//! [`MemoryConfig`]: connector settings held in memory.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use omuzaki_connect_api::{ConfigSource, ConnectorConfig, ConnectorId};

/// Settings a test set, by connector; the default (disabled) for one it never set.
#[derive(Debug, Default)]
pub struct MemoryConfig(Mutex<BTreeMap<ConnectorId, ConnectorConfig>>);

impl MemoryConfig {
    /// No settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the settings of `id`.
    pub fn set(&self, id: ConnectorId, config: ConnectorConfig) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(id, config);
    }
}

impl ConfigSource for MemoryConfig {
    fn get(&self, id: &ConnectorId) -> ConnectorConfig {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get(id).cloned().unwrap_or_default()
    }
}
