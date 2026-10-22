//! Where variable values persist (design D10): [`VariableStore`], per [`PersistScope`].

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ShowKey;
use crate::ids::VarName;

/// Which values file: one show's on this machine, or this OS user's globals.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PersistScope {
    /// `show`-scope values of one show: `<project>/.omuzaki/automation/<show>/values.toml`.
    Show(ShowKey),
    /// `global`-scope values: `<data>/automation/globals.toml`.
    Global,
}

/// Why values couldn't be loaded or saved.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum VarStoreError {
    /// Reading or writing the file failed.
    #[error("couldn't read or write the values file: {0}")]
    Io(String),
    /// The file isn't a values file.
    #[error("the values file is damaged: {0}")]
    Format(String),
}

/// Loads and saves variable values. The engine's writer thread calls `save` at most once a second per file and at
/// the final flush; an implementation writes atomically (a temporary file, then a rename).
pub trait VariableStore: Send + Sync {
    /// The stored values of `scope`; empty when nothing is stored yet.
    ///
    /// # Errors
    /// A [`VarStoreError`] if the file exists but can't be read.
    fn load(&self, scope: &PersistScope) -> Result<BTreeMap<VarName, Value>, VarStoreError>;

    /// Replaces the stored values of `scope`.
    ///
    /// # Errors
    /// A [`VarStoreError`] if they can't be written; what was stored before stays.
    fn save(&self, scope: &PersistScope, values: &BTreeMap<VarName, Value>) -> Result<(), VarStoreError>;
}
