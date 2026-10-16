//! A platform's limit table as data: the types, the parser that refuses a malformed table, and the staleness check.
//!
//! A table is the `[limits]` section of a platform's data file, `data/platforms/<id>.toml` (the file's `[profile]`
//! section belongs to the platform profile registry and is not read here). It holds a header (`platform`, `version`,
//! `source`, `retrieved`, `tiers`, `beyond`, and optionally `advisory` and `unsupported_note`), a `[limits.texture]`
//! estimate and a `[limits.stats]` table with one row per capped stat. A row is a limit per
//! tier (a number, or `"-"` for unlimited) or the word `"unsupported"` for a stat the platform strips. A stat that has
//! no row is not capped. Parsing never guesses: a missing header field, an unknown key, a row of the wrong length or
//! a limit that falls from one tier to the next is an error that names the file and the key.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::LimitsError;
use super::stats::{STATS, stat_def};

/// The word that marks a stat the platform strips.
const UNSUPPORTED: &str = "unsupported";
/// The cell that marks a tier with no limit.
const UNLIMITED: &str = "-";
/// A full mip chain adds a third on top of the base level.
const MIP_FACTOR: f64 = 4.0 / 3.0;
const BITS_PER_BYTE: f64 = 8.0;
const MIB: f64 = 1024.0 * 1024.0;

/// The unit a stat's limits and values are in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Unit {
    /// A plain number: triangles, bones, components.
    Count,
    /// Metres along one axis (bounds are three stats, one per axis).
    Metres,
    /// Mebibytes of texture memory (1024 * 1024 bytes), under the table's [`TextureEstimate`].
    Mib,
}

impl Unit {
    /// The suffix a message writes after a value of this unit, with its leading space (or none for a count).
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Count => "",
            Self::Metres => " m",
            Self::Mib => " MiB",
        }
    }
}

/// How a platform's texture memory is estimated from pixel counts.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextureEstimate {
    /// Bits per pixel for an image without an alpha channel.
    pub opaque_bits: f64,
    /// Bits per pixel for an image with an alpha channel.
    pub alpha_bits: f64,
    /// Whether a full mip chain is counted, which adds a third.
    pub mips: bool,
}

impl TextureEstimate {
    /// The memory for the given pixel counts, in MiB.
    pub(super) fn mib(&self, px_opaque: f64, px_alpha: f64) -> f64 {
        let bytes = px_opaque * (self.opaque_bits / BITS_PER_BYTE) + px_alpha * (self.alpha_bits / BITS_PER_BYTE);
        let mips = if self.mips { MIP_FACTOR } else { 1.0 };
        bytes * mips / MIB
    }
}

/// One stat's limits.
#[derive(Clone, Debug, PartialEq)]
pub struct StatLimit {
    /// The stat's key (see [`STATS`]).
    pub key: String,
    /// The unit of the limits.
    pub unit: Unit,
    /// The limit per tier, in the table's tier order. `None` is unlimited. Empty when `unsupported`.
    pub per_tier: Vec<Option<f64>>,
    /// The platform strips this component: it is never counted, and a value is only reported.
    pub unsupported: bool,
}

/// A platform's published limits.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitTable {
    /// The platform's id, a lower-case slug that equals the file stem (`vrchat-quest`). Rule ids use it.
    pub platform: String,
    /// The platform's own version, or the date of the page the numbers came from.
    pub version: String,
    /// The URL of the page the numbers were read from.
    pub source: String,
    /// The day the page was read, `YYYY-MM-DD`.
    pub retrieved: String,
    /// The tier names, best first. A platform with hard limits has one tier, `accept`.
    pub tiers: Vec<String>,
    /// What the platform calls an avatar past the last tier (`Very Poor`, `refused`). It is not a tier: it is
    /// unbounded, so it can never be a target.
    pub beyond: String,
    /// The tiers that are recommendations, not limits (Resonite's recommended sizes). A stat past an advisory tier
    /// but within the target is a warning, never an error. Empty for a platform whose tiers are all limits.
    pub advisory: Vec<String>,
    /// The platform's wording for a stripped component, shown with an unsupported stat that has a value.
    pub unsupported_note: Option<String>,
    /// How this platform's texture memory is estimated.
    pub texture: TextureEstimate,
    /// The capped stats, in [`STATS`] order whatever order the file wrote them in.
    pub stats: Vec<StatLimit>,
}

/// The data file's shape: the `[limits]` section this module reads, and a `[profile]` section it leaves alone (the
/// platform profile registry parses it), kept as it was written.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    limits: RawTable,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<toml::Value>,
}

/// The `[limits]` section's shape, before the rows are checked.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTable {
    platform: String,
    version: String,
    source: String,
    retrieved: String,
    tiers: Vec<String>,
    beyond: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    advisory: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unsupported_note: Option<String>,
    texture: TextureEstimate,
    stats: toml::Table,
}

impl LimitTable {
    /// Parses a table from the TOML text of a platform data file (its `[limits]` section). Errors name `limit table`
    /// as the file; use [`LimitTable::parse_named`] to name the real one.
    ///
    /// # Errors
    ///
    /// Fails, naming the key, if the text is not a valid table (see the module docs).
    pub fn parse(text: &str) -> Result<Self, LimitsError> {
        Self::parse_named("limit table", text)
    }

    /// Parses a table from TOML text read from `file`, which the errors name.
    ///
    /// # Errors
    ///
    /// Fails, naming `file` and the key, if the text is not a valid table (see the module docs).
    pub fn parse_named(file: &str, text: &str) -> Result<Self, LimitsError> {
        let raw: RawFile = toml::from_str(text).map_err(|e| LimitsError::from_toml(file, e))?;
        let raw = raw.limits;
        let fail = |key: &str, reason: String| LimitsError::new(file, Some(key), reason);
        for (key, value) in [
            ("platform", &raw.platform),
            ("version", &raw.version),
            ("source", &raw.source),
            ("retrieved", &raw.retrieved),
            ("beyond", &raw.beyond),
        ] {
            if value.trim().is_empty() {
                return Err(fail(key, "is empty".into()));
            }
        }
        if !is_slug(&raw.platform) {
            return Err(fail("platform", "must be a lower-case slug (a-z, 0-9, '-')".into()));
        }
        if parse_date(&raw.retrieved).is_none() {
            return Err(fail("retrieved", format!("'{}' is not a YYYY-MM-DD date", raw.retrieved)));
        }
        check_tiers(&raw.tiers).map_err(|reason| fail("tiers", reason))?;
        if raw.tiers.contains(&raw.beyond) {
            return Err(fail("beyond", format!("'{}' is also a tier; it names what is past the last one", raw.beyond)));
        }
        check_advisory(&raw.advisory, &raw.tiers).map_err(|reason| fail("advisory", reason))?;
        let texture = raw.texture;
        for (key, bits) in [("texture.opaque_bits", texture.opaque_bits), ("texture.alpha_bits", texture.alpha_bits)] {
            if !bits.is_finite() || bits < 0.0 {
                return Err(fail(key, format!("{bits} is not a non-negative number")));
            }
        }
        if let Some(key) = raw.stats.keys().find(|key| stat_def(key).is_none()) {
            return Err(fail(&format!("stats.{key}"), "is not a stat (see the stat registry)".into()));
        }
        let mut stats = Vec::with_capacity(raw.stats.len());
        for def in STATS {
            let Some(value) = raw.stats.get(def.key) else {
                continue;
            };
            let key = format!("stats.{}", def.key);
            let (per_tier, unsupported) = parse_row(value, raw.tiers.len()).map_err(|reason| fail(&key, reason))?;
            stats.push(StatLimit { key: def.key.to_string(), unit: def.unit, per_tier, unsupported });
        }
        Ok(Self {
            platform: raw.platform,
            version: raw.version,
            source: raw.source,
            retrieved: raw.retrieved,
            tiers: raw.tiers,
            beyond: raw.beyond,
            advisory: raw.advisory,
            unsupported_note: raw.unsupported_note,
            texture,
            stats,
        })
    }

    /// The table as TOML text that [`LimitTable::parse`] reads back to an equal table.
    ///
    /// # Errors
    ///
    /// Fails if a limit is not a finite number, which no parsed table holds.
    pub fn to_toml(&self) -> Result<String, LimitsError> {
        let mut stats = toml::Table::new();
        for stat in &self.stats {
            let value = if stat.unsupported {
                toml::Value::String(UNSUPPORTED.into())
            } else {
                toml::Value::Array(
                    stat.per_tier
                        .iter()
                        .map(|limit| limit.map_or_else(|| toml::Value::String(UNLIMITED.into()), toml::Value::Float))
                        .collect(),
                )
            };
            stats.insert(stat.key.clone(), value);
        }
        let raw = RawFile {
            limits: RawTable {
                platform: self.platform.clone(),
                version: self.version.clone(),
                source: self.source.clone(),
                retrieved: self.retrieved.clone(),
                tiers: self.tiers.clone(),
                beyond: self.beyond.clone(),
                advisory: self.advisory.clone(),
                unsupported_note: self.unsupported_note.clone(),
                texture: self.texture,
                stats,
            },
            profile: None,
        };
        toml::to_string(&raw).map_err(|e| LimitsError::new("limit table", None, e.to_string()))
    }

    /// The limits of one stat, or `None` if the table does not cap it.
    pub fn stat(&self, key: &str) -> Option<&StatLimit> {
        self.stats.iter().find(|stat| stat.key == key)
    }

    /// The built-in table for a platform id (`vrchat-pc`, `vrchat-quest`), or `None` if there is none or its data
    /// file is broken. [`super::builtin_checked`] says which.
    pub fn builtin(id: &str) -> Option<&'static Self> {
        super::builtin_checked(id).ok()
    }
}

/// True if the table was read more than `max_age_days` days before `today` (`YYYY-MM-DD`). A `today` that is not a
/// date counts as stale, so a bad clock never hides an old table.
pub fn stale(table: &LimitTable, today: &str, max_age_days: u32) -> bool {
    match (parse_date(today), parse_date(&table.retrieved)) {
        (Some(today), Some(retrieved)) => today - retrieved > i64::from(max_age_days),
        _ => true,
    }
}

fn is_slug(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn check_tiers(tiers: &[String]) -> Result<(), String> {
    if tiers.is_empty() {
        return Err("needs at least one tier".into());
    }
    let mut seen = BTreeSet::new();
    for tier in tiers {
        if tier.trim().is_empty() {
            return Err("has an empty tier name".into());
        }
        if !seen.insert(tier) {
            return Err(format!("names tier '{tier}' twice"));
        }
    }
    Ok(())
}

/// The advisory tiers are tiers of the table, each named once.
fn check_advisory(advisory: &[String], tiers: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for name in advisory {
        if !tiers.contains(name) {
            return Err(format!("names '{name}', which is not a tier"));
        }
        if !seen.insert(name) {
            return Err(format!("names tier '{name}' twice"));
        }
    }
    Ok(())
}

/// One row: the limits per tier and whether the stat is marked unsupported.
fn parse_row(value: &toml::Value, tiers: usize) -> Result<(Vec<Option<f64>>, bool), String> {
    let cells = match value {
        toml::Value::String(word) if word == UNSUPPORTED => return Ok((Vec::new(), true)),
        toml::Value::String(word) => {
            return Err(format!("is the word '{word}'; the only word is '{UNSUPPORTED}', or write one limit per tier"));
        }
        toml::Value::Array(cells) => cells,
        other => return Err(format!("must be an array of limits or '{UNSUPPORTED}', not a {}", other.type_str())),
    };
    if cells.len() != tiers {
        return Err(format!("has {} limits for {tiers} tiers", cells.len()));
    }
    let mut limits: Vec<Option<f64>> = Vec::with_capacity(cells.len());
    for cell in cells {
        let limit = match cell {
            toml::Value::Float(n) => Some(*n),
            toml::Value::Integer(n) => Some(int_to_f64(*n)),
            toml::Value::String(word) if word == UNLIMITED => None,
            other => return Err(format!("has a {} where a number or '{UNLIMITED}' belongs", other.type_str())),
        };
        if let Some(n) = limit.filter(|n| !n.is_finite() || *n < 0.0) {
            return Err(format!("has {n}, which is not a non-negative number"));
        }
        limits.push(limit);
    }
    check_order(&limits)?;
    Ok((limits, false))
}

// Limits are far below 2^53, where f64 stops representing every integer exactly.
#[allow(clippy::cast_precision_loss)]
fn int_to_f64(n: i64) -> f64 {
    n as f64
}

/// A tier never allows less than the tier before it, and nothing after an unlimited tier is limited.
fn check_order(limits: &[Option<f64>]) -> Result<(), String> {
    for (i, pair) in limits.windows(2).enumerate() {
        let tighter_later = match (pair[0], pair[1]) {
            (Some(a), Some(b)) => b < a,
            (None, Some(_)) => true,
            _ => false,
        };
        if tighter_later {
            return Err(format!("limits fall from tier {} to tier {}", i + 1, i + 2));
        }
    }
    Ok(())
}

/// Days since 1970-01-01 for a `YYYY-MM-DD` date, or `None` if the text is not a real calendar date.
fn parse_date(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let shape_ok = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes.iter().enumerate().all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    if !shape_ok {
        return None;
    }
    let year: i64 = text[0..4].parse().ok()?;
    let month: i64 = text[5..7].parse().ok()?;
    let day: i64 = text[8..10].parse().ok()?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=month_days).contains(&day) {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm), with March as the first month so the leap day ends the year.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}
