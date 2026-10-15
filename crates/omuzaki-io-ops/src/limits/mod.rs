//! Limit tables as data, for any platform (ADR 0047).
//!
//! A platform's published limits live in a TOML file, not in code. [`LimitTable::parse`] reads one (refusing a
//! malformed table with an error that names the file and key), [`evaluate`] puts an avatar's [`AvatarStats`] against
//! it, and [`findings`] turns the result into a validation [`Report`](omuzaki_io_api::Report) whose hints name the fit
//! stage that would help. [`stale`] says when a table has gone too long without being re-read from its page.
//!
//! A *tier* is one column of a platform's table (`Excellent`, `Good`, ...), best first. A platform with a single hard
//! limit has one tier, `accept`. The tables built in are `VRChat`'s two (`vrchat-pc`, `vrchat-quest`), under
//! `data/limits/`; `crate::vrchat_rank` is a thin wrapper over them.

mod eval;
mod stats;
mod table;

use std::sync::LazyLock;

use thiserror::Error;

pub use eval::{Evaluation, StatTier, StatVerdict, evaluate, findings};
pub use stats::{AvatarStats, Fix, STATS, StatDef, StatsError, stat_def, stats_from_ioscene};
pub use table::{LimitTable, StatLimit, TextureEstimate, Unit, stale};

/// A limit table that cannot be used. The text names the file and, where there is one, the key.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{}", self.message())]
pub struct LimitsError {
    file: String,
    key: Option<String>,
    reason: String,
}

impl LimitsError {
    pub(crate) fn new(file: &str, key: Option<&str>, reason: impl Into<String>) -> Self {
        Self { file: file.to_string(), key: key.map(str::to_string), reason: reason.into() }
    }

    /// The file the table was read from.
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The key in the table that is wrong, if the problem is one key's.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    fn message(&self) -> String {
        match &self.key {
            Some(key) => format!("{}: '{key}' {}", self.file, self.reason),
            None => format!("{}: {}", self.file, self.reason),
        }
    }
}

/// The built-in tables: their ids and the data files they are read from.
const BUILTIN: [(&str, &str); 2] = [
    ("vrchat-pc", include_str!("../../data/limits/vrchat-pc.toml")),
    ("vrchat-quest", include_str!("../../data/limits/vrchat-quest.toml")),
];

/// The built-in tables, parsed once. A broken file stays an error here, so a caller can report it.
static TABLES: LazyLock<Vec<(&'static str, Result<LimitTable, LimitsError>)>> = LazyLock::new(|| {
    BUILTIN
        .iter()
        .map(|&(id, text)| {
            let table = LimitTable::parse_named(&format!("data/limits/{id}.toml"), text).and_then(|table| {
                if table.platform == id {
                    Ok(table)
                } else {
                    Err(LimitsError::new(
                        &format!("data/limits/{id}.toml"),
                        Some("platform"),
                        "must equal the file stem",
                    ))
                }
            });
            (id, table)
        })
        .collect()
});

/// The built-in table for `id`.
///
/// # Errors
///
/// Fails if there is no such table, or its data file is broken (the tests catch that).
pub fn builtin_checked(id: &str) -> Result<&'static LimitTable, LimitsError> {
    match TABLES.iter().find(|(known, _)| *known == id) {
        Some((_, Ok(table))) => Ok(table),
        Some((_, Err(error))) => Err(error.clone()),
        None => Err(LimitsError::new("data/limits", Some(id), "is not a built-in limit table")),
    }
}

/// The ids of the built-in tables, in a fixed order.
pub fn builtin_ids() -> impl Iterator<Item = &'static str> {
    BUILTIN.iter().map(|&(id, _)| id)
}
