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

/// Why `state.toml` could not be read, or a state could not be written as it.
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
    /// The text is not valid TOML, lacks a valid `current`, or has a sequence that is not a `u64`.
    pub fn from_toml(text: &str) -> Result<Self, StateError> {
        let raw: toml::Table = text.parse().map_err(|e: toml::de::Error| StateError(e.to_string()))?;
        // Sequences above `i64::MAX` are strings in the file; serde sees a placeholder and the real value goes back after.
        let mut plain = raw.clone();
        let keys_sequence = take_sequence(&mut plain, "keys_sequence")?;
        let mut sequences = BTreeMap::new();
        if let Some(toml::Value::Table(marks)) = plain.get_mut("watermarks") {
            for (feed, mark) in marks.iter_mut() {
                if let toml::Value::Table(mark) = mark
                    && let Some(n) = take_sequence(mark, "sequence")?
                {
                    sequences.insert(feed.clone(), n);
                }
            }
        }
        let mut state: Self = plain.try_into().map_err(|e: toml::de::Error| StateError(e.to_string()))?;
        if let Some(n) = keys_sequence {
            state.keys_sequence = n;
        }
        for (feed, n) in sequences {
            if let Some(mark) = state.watermarks.get_mut(&feed) {
                mark.sequence = n;
            }
        }
        state.unknown = unknown_in(&raw, &state.known_table()?);
        Ok(state)
    }

    /// The `state.toml` text: the known keys, then the unknown ones merged back in.
    ///
    /// TOML integers are signed 64-bit, so a sequence above `i64::MAX` is written as a decimal string
    /// (`sequence = "18446744073709551615"`); [`State::from_toml`] reads either form, so every `u64` round-trips.
    ///
    /// A known key always wins over an unknown one at the same place. Unknown keys inside a `watermarks` entry belong to
    /// that feed: removing the feed from [`State::watermarks`] removes them too, so a saved file never holds a feed
    /// without its `epoch` and `sequence`. Every other unknown key is kept.
    ///
    /// # Errors
    /// A state TOML can't represent. No field today can fail (every number fits TOML's integers once the sequences
    /// are strings), but a failure is returned rather than written short: a file missing `current`, written atomically
    /// over `state.toml`, would leave the next launch unable to read its state.
    pub fn to_toml(&self) -> Result<String, StateError> {
        let mut table = self.known_table()?;
        let mut unknown = self.unknown.clone();
        if let Some(toml::Value::Table(marks)) = unknown.get_mut("watermarks") {
            marks.retain(|feed, _| self.watermarks.contains_key(feed));
            if marks.is_empty() {
                unknown.remove("watermarks");
            }
        }
        merge_missing(&mut table, &unknown);
        Ok(table.to_string())
    }

    fn known_table(&self) -> Result<toml::Table, StateError> {
        let mut plain = self.clone();
        plain.keys_sequence = 0;
        for mark in plain.watermarks.values_mut() {
            mark.sequence = 0;
        }
        let mut table = toml::Table::try_from(&plain).map_err(|e| StateError(e.to_string()))?;
        table.insert("keys_sequence".into(), sequence_value(self.keys_sequence));
        if let Some(toml::Value::Table(marks)) = table.get_mut("watermarks") {
            for (feed, mark) in &self.watermarks {
                if let Some(toml::Value::Table(entry)) = marks.get_mut(feed) {
                    entry.insert("sequence".into(), sequence_value(mark.sequence));
                }
            }
        }
        Ok(table)
    }
}

/// A sequence as TOML: an integer while it fits in `i64`, else its decimal string.
fn sequence_value(n: u64) -> toml::Value {
    i64::try_from(n).map_or_else(|_| toml::Value::String(n.to_string()), toml::Value::Integer)
}

/// Replaces a string sequence at `key` with a placeholder integer and returns its value; an integer stays for serde.
fn take_sequence(table: &mut toml::Table, key: &str) -> Result<Option<u64>, StateError> {
    let Some(toml::Value::String(text)) = table.get(key) else {
        return Ok(None);
    };
    let n = text.parse::<u64>().map_err(|_| StateError(format!("{key}: {text:?} is not a u64")))?;
    table.insert(key.into(), toml::Value::Integer(0));
    Ok(Some(n))
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
