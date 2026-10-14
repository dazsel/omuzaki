//! `state.toml`: written only by the launcher and the updater, atomically (design section 5). It lives here because
//! both are implementation crates and `lint-deps` lets neither use the other's code.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::clock::Timestamp;
use crate::status::RollbackReason;

/// How far a feed has gone: ordered as the pair, `epoch` first (design section 4), so a key rotation that bumps the
/// epoch outranks any sequence of the old one.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Watermark {
    /// The key epoch.
    pub epoch: u32,
    /// The manifest sequence within the epoch.
    pub sequence: u64,
}

/// Why `state.toml` could not be read.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("state.toml: {0}")]
pub struct StateError(pub String);

/// The install state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct State {
    /// The version the launcher starts.
    pub current: Version,
    /// The version before it; the rollback target.
    #[serde(default)]
    pub previous: Option<Version>,
    /// A staged version the launcher switches to at its next start.
    #[serde(default)]
    pub pending: Option<Version>,
    /// Trial starts that ended before `healthy`.
    #[serde(default)]
    pub attempts: u8,
    /// Whether `current` is still on trial.
    #[serde(default)]
    pub trial: bool,
    /// Versions that failed and are not offered again.
    #[serde(default)]
    pub bad: Vec<Version>,
    /// Set by an automatic or manual rollback; shown once.
    #[serde(default)]
    pub rolled_back_from: Option<(Version, RollbackReason)>,
    /// When the last check finished.
    #[serde(default)]
    pub last_checked: Option<Timestamp>,
    /// Per feed: `stable`, `beta`, `catalogue`.
    #[serde(default)]
    pub watermarks: BTreeMap<String, Watermark>,
    /// The sequence of the embedded-keys list last accepted.
    #[serde(default)]
    pub keys_sequence: u64,
    /// Keys this build does not know, at any depth. Kept on write, as settings D5 keeps unknown keys.
    #[serde(skip)]
    pub unknown: toml::Table,
}

impl State {
    /// Reads `state.toml`, keeping every key this build does not know.
    ///
    /// # Errors
    /// The text is not valid TOML or lacks a valid `current`.
    pub fn from_toml(text: &str) -> Result<Self, StateError> {
        let raw: toml::Table = text.parse().map_err(|e: toml::de::Error| StateError(e.to_string()))?;
        let mut state: Self = raw.clone().try_into().map_err(|e: toml::de::Error| StateError(e.to_string()))?;
        state.unknown = unknown_in(&raw, &state.known_table()?);
        Ok(state)
    }

    /// The `state.toml` text: the known keys, then the unknown ones merged back in. TOML integers are signed 64-bit, so
    /// a sequence above `i64::MAX` is written as `i64::MAX`.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut table = self.known_table().unwrap_or_default();
        merge_missing(&mut table, &self.unknown);
        table.to_string()
    }

    fn known_table(&self) -> Result<toml::Table, StateError> {
        let mut clamped = self.clone();
        let cap = |n: u64| n.min(i64::MAX as u64);
        clamped.keys_sequence = cap(clamped.keys_sequence);
        for mark in clamped.watermarks.values_mut() {
            mark.sequence = cap(mark.sequence);
        }
        toml::Table::try_from(&clamped).map_err(|e| StateError(e.to_string()))
    }
}

/// What `raw` has that `known` lacks, keeping the nesting.
fn unknown_in(raw: &toml::Table, known: &toml::Table) -> toml::Table {
    let mut out = toml::Table::new();
    for (key, value) in raw {
        match (value, known.get(key)) {
            (_, None) => {
                out.insert(key.clone(), value.clone());
            }
            (toml::Value::Table(r), Some(toml::Value::Table(k))) => {
                let inner = unknown_in(r, k);
                if !inner.is_empty() {
                    out.insert(key.clone(), toml::Value::Table(inner));
                }
            }
            _ => {}
        }
    }
    out
}

/// Adds the entries of `extra` that `into` lacks, recursing into tables both have.
fn merge_missing(into: &mut toml::Table, extra: &toml::Table) {
    for (key, value) in extra {
        match (into.get_mut(key), value) {
            (Some(toml::Value::Table(a)), toml::Value::Table(b)) => merge_missing(a, b),
            (Some(_), _) => {}
            (None, _) => {
                into.insert(key.clone(), value.clone());
            }
        }
    }
}
