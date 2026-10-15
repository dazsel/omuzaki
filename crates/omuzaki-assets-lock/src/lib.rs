//! The project's `assets.lock` file (spec v3 §7, git design §3.2): the list of assets a project uses, one row per
//! content hash, with the provenance a reader needs to check licences without opening the asset store.
//!
//! This crate is the file's only reader and only canonical writer. The project loader, the commit view, the push
//! advisory, the snapshot manifest and the asset sweep all go through [`AssetsLock::parse`] and
//! [`AssetsLock::to_text`], so the format cannot drift between them. It does no I/O: callers read and write the text.
//!
//! Rows are sorted by hash, so two people adding assets change different lines and git merges them cleanly. The
//! writer owns the whole file: unknown keys in an input file are dropped on the next write, and a caller that must
//! keep CRLF line endings converts the text after writing it.
#![deny(missing_docs)]

use std::collections::HashSet;
use std::fmt::Write as _;

use omuzaki_store_api::{Commercial, ContentHash};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The project's asset list: one row per asset the project uses, keyed by content hash. No I/O: callers read and write
/// the text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AssetsLock {
    /// The rows, sorted by hash after [`AssetsLock::parse`]. [`AssetsLock::to_text`] sorts them itself, so the order
    /// here never changes the file.
    pub rows: Vec<LockRow>,
}

/// One asset the project uses: its hash in the store and the provenance that travels with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LockRow {
    /// The asset's content hash: the key the store and the project both use.
    pub hash: ContentHash,
    /// The file name the asset was added under, for people reading the lock.
    pub name: String,
    /// SPDX licence expression, or `LicenseRef-<name>` for a custom record.
    pub licence: String,
    /// Commercial status, as the licence gate reads it (`ok`, `no` or `unknown` in the file).
    pub commercial: Commercial,
    /// Where the asset came from, in words.
    pub source: String,
    /// Test material that must never ship or be published, such as a private character's reference. Absent in the
    /// file means `false`, and the writer omits it when `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private_test: bool,
}

/// Why a lock file can't be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LockError {
    /// The text isn't TOML, or its `asset` key isn't a list (the whole document has the wrong shape). There is no row
    /// to point at, so the message says what shape was expected.
    #[error("assets.lock is not valid TOML or does not have the expected shape: {0}")]
    Toml(String),
    /// Row `index` (counted from 0 in file order) is malformed or repeats an earlier hash.
    #[error("assets.lock row {index}: {reason}")]
    Row {
        /// The row's position in file order, counted from 0.
        index: usize,
        /// What is wrong with it.
        reason: String,
    },
}

/// The file's top level: a list of `[[asset]]` entries. Each entry is kept as a raw value until it is checked, so a bad
/// entry is reported with its own index instead of failing the whole file.
#[derive(Deserialize)]
struct Document {
    #[serde(default)]
    asset: Vec<toml::Value>,
}

impl AssetsLock {
    /// Reads a lock file. Accepts LF and CRLF line endings, ignores keys it doesn't know, and treats an empty or
    /// whitespace-only file as zero rows. Rows come back sorted by hash.
    ///
    /// A row is refused when an entry isn't a table, a key is missing, a value has the wrong type, the hash isn't 64
    /// lowercase hex characters, `commercial` isn't `ok`, `no` or `unknown`, or the hash repeats an earlier row's hash.
    ///
    /// # Errors
    ///
    /// [`LockError::Toml`] when the text isn't TOML or `asset` isn't a list; [`LockError::Row`] for the first entry
    /// that is not a table, is malformed, or repeats an earlier hash.
    pub fn parse(text: &str) -> Result<AssetsLock, LockError> {
        let document: Document = toml::from_str(text).map_err(|error| LockError::Toml(error.to_string()))?;
        let mut rows = Vec::with_capacity(document.asset.len());
        let mut seen = HashSet::with_capacity(document.asset.len());
        for (index, entry) in document.asset.into_iter().enumerate() {
            let toml::Value::Table(table) = entry else {
                return Err(LockError::Row { index, reason: "not a table".to_owned() });
            };
            let row: LockRow = table
                .try_into()
                .map_err(|error: toml::de::Error| LockError::Row { index, reason: error.to_string() })?;
            if !seen.insert(row.hash) {
                return Err(LockError::Row { index, reason: format!("hash {} is listed twice", row.hash) });
            }
            rows.push(row);
        }
        rows.sort_by_key(|row| row.hash);
        Ok(AssetsLock { rows })
    }

    /// The canonical text of this lock: rows sorted by hash, each written as an `[[asset]]` table with its keys in a
    /// fixed order (`private_test` only when true), one blank line between rows, `\n` endings and one trailing newline.
    /// Zero rows give the empty string. The same rows in any order give the same text, and [`AssetsLock::parse`]
    /// reads it back to the same rows.
    ///
    /// Rows should have distinct hashes, as a file does: a repeated hash is written as given, and `parse` refuses it.
    pub fn to_text(&self) -> String {
        let mut rows: Vec<&LockRow> = self.rows.iter().collect();
        rows.sort_by_key(|row| row.hash);
        let mut out = String::new();
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            write_row(&mut out, row);
        }
        out
    }
}

/// Writes one `[[asset]]` table in the canonical key order.
fn write_row(out: &mut String, row: &LockRow) {
    out.push_str("[[asset]]\n");
    write_string_key(out, "hash", &row.hash.to_string());
    write_string_key(out, "name", &row.name);
    write_string_key(out, "licence", &row.licence);
    write_string_key(out, "commercial", commercial_text(row.commercial));
    write_string_key(out, "source", &row.source);
    if row.private_test {
        out.push_str("private_test = true\n");
    }
}

/// The file's spelling of a commercial status. It matches the serde names of [`Commercial`], which the reader uses.
fn commercial_text(commercial: Commercial) -> &'static str {
    match commercial {
        Commercial::Ok => "ok",
        Commercial::No => "no",
        Commercial::Unknown => "unknown",
    }
}

/// Writes `key = "value"` with the value as a TOML basic string.
fn write_string_key(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = ");
    write_basic_string(out, value);
    out.push('\n');
}

/// Writes `value` as a TOML basic string. Quotes, backslashes and every control character are escaped (TOML forbids
/// raw control characters, and a raw newline would split the line); other text, including non-ASCII, is written as
/// it is.
fn write_basic_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c.is_control() => {
                // `\uXXXX` takes exactly four hex digits; `write!` into a String cannot fail, so the result is ignored.
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
