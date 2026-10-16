//! Limit tables as data, for any platform (ADR 0047).
//!
//! A platform's published limits live in a TOML file, not in code. [`LimitTable::parse`] reads one (refusing a
//! malformed table with an error that names the file and key), [`evaluate`] puts an avatar's [`AvatarStats`] against
//! it, and [`findings`] turns the result into a validation [`Report`](omuzaki_io_api::Report) whose hints name the fit
//! stage that would help. [`stale`] says when a table has gone too long without being re-read from its page.
//!
//! A *tier* is one column of a platform's table (`Excellent`, `Good`, ...), best first. A platform with a single hard
//! limit has one tier, `accept`. A platform's table is the `[limits]` section of its data file, `data/platforms/<id>.toml`; the tables built in are
//! `VRChat`'s two (`vrchat-pc`, `vrchat-quest`), which `crate::vrchat_rank` registers and wraps.

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

/// A built-in table: the platform id and the text of its data file, `data/platforms/<id>.toml`. A platform registers
/// its own with [`register_limit_table!`](crate::register_limit_table) next to the code that uses it, so no list here
/// names the platforms.
pub struct BuiltinTable {
    /// The platform id, which must equal the file stem and the table's `platform`.
    pub id: &'static str,
    /// The data file's text.
    pub text: &'static str,
}

inventory::collect!(BuiltinTable);

/// Registers a built-in limit table: `register_limit_table!("vrchat-pc", "../data/platforms/vrchat-pc.toml")`. The path
/// is relative to the file the macro is written in, as `include_str!`'s is. Like every `register_*!` macro it also
/// records the registration, so the link gate (ADR 0015) can prove the binary holds it.
#[macro_export]
macro_rules! register_limit_table {
    ($id:literal, $path:literal) => {
        ::omuzaki_core::__record_registration!("limit_table", $id);
        inventory::submit! {
            $crate::limits::BuiltinTable { id: $id, text: include_str!($path) }
        }
    };
}

/// The data file a table is read from, for the errors that name it.
fn data_file(id: &str) -> String {
    format!("data/platforms/{id}.toml")
}

/// The built-in tables, parsed once and sorted by id. A broken file or a repeated id stays an error here, so a caller
/// can report it.
static TABLES: LazyLock<Vec<(&'static str, Result<LimitTable, LimitsError>)>> = LazyLock::new(|| {
    let mut registered: Vec<&BuiltinTable> = inventory::iter::<BuiltinTable>.into_iter().collect();
    registered.sort_by_key(|entry| entry.id);
    let repeated: Vec<&str> =
        registered.windows(2).filter(|pair| pair[0].id == pair[1].id).map(|pair| pair[0].id).collect();
    registered
        .iter()
        .map(|entry| {
            let id = entry.id;
            let file = data_file(id);
            let table = if repeated.contains(&id) {
                Err(LimitsError::new(&file, Some("platform"), "is registered more than once"))
            } else {
                LimitTable::parse_named(&file, entry.text).and_then(|table| {
                    if table.platform == id {
                        Ok(table)
                    } else {
                        Err(LimitsError::new(&file, Some("platform"), "must equal the file stem"))
                    }
                })
            };
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
        None => Err(LimitsError::new("data/platforms", Some(id), "is not a built-in limit table")),
    }
}

/// The ids of the built-in tables, in a fixed order.
pub fn builtin_ids() -> impl Iterator<Item = &'static str> {
    TABLES.iter().map(|&(id, _)| id)
}
