//! Exaggeration profiles (spec FE-06): one dial from a realistic face through the anime defaults to a chibi extreme, and
//! the subtle-performance expander.
//!
//! **The dial.** `amount` runs from -1 (`realistic`) through 0 (`anime`, the contract's default coefficients) to +1
//! (`chibi-extreme`). The two ends are factor tables in `library/exaggeration/`: each factor multiplies the default
//! coefficient at its path of the [`FaceMapping`]. Between an end and the centre the factor moves in a straight line, so
//! amount 0 gives the defaults bit for bit, and since every realistic factor is at most 1 and every chibi factor at least
//! 1, a larger amount never gives a smaller coefficient. The dial sets gains only: the resting face, the thresholds and
//! the fractions are the character's design, and the dial always starts from the defaults, so it never compounds.
//!
//! **The expander** is [`omuzaki_face_api::Amplify`], stored in the mapping: below a knee each `ARKit` channel is
//! multiplied by a gain, so the small movements a webcam reads from a calm face show.
//!
//! **What this is not.** Per-channel response curves (a performer's range on one blendshape, dead zones, calibration)
//! are retarget calibration (spec PF-22, `omuzaki-anim`). They run before the bus reaches the face and belong to the
//! performer; this module shapes how a character's face reacts, and belongs to the character.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use omuzaki_face_api::FaceMapping;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The realistic end of the dial.
const REALISTIC: &str = include_str!("../../library/exaggeration/realistic.toml");
/// The chibi-extreme end of the dial.
const CHIBI: &str = include_str!("../../library/exaggeration/chibi-extreme.toml");

/// A named point on the dial.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// Small eye-opening travel, flatter brows, a quieter mouth: amount -1.
    Realistic,
    /// The contract's default coefficients: amount 0.
    Anime,
    /// Wide eyes, big brow swings, large smiles: amount +1.
    ChibiExtreme,
}

impl Preset {
    /// Every preset, from the realistic end up.
    pub const ALL: [Self; 3] = [Self::Realistic, Self::Anime, Self::ChibiExtreme];

    /// The preset's point on the dial.
    pub fn amount(self) -> f64 {
        match self {
            Self::Realistic => -1.0,
            Self::Anime => 0.0,
            Self::ChibiExtreme => 1.0,
        }
    }

    /// The preset's name, as ops and files spell it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Realistic => "realistic",
            Self::Anime => "anime",
            Self::ChibiExtreme => "chibi-extreme",
        }
    }
}

/// Factors by mapping path (`eyes/wide`).
pub type Factors = BTreeMap<String, f64>;

/// The two ends of the dial.
#[derive(Clone, Debug, PartialEq)]
pub struct Ends {
    /// At amount -1; every factor `0 < f <= 1`.
    pub realistic: Factors,
    /// At amount +1; every factor `f >= 1`.
    pub chibi: Factors,
}

/// The embedded ends, parsed and checked once.
///
/// # Errors
/// A table doesn't parse, names a path the mapping doesn't have or one the dial may not touch, or has a factor on the
/// wrong side of 1: a bug in the embedded data, which the crate's tests catch.
pub fn ends() -> Result<&'static Ends, String> {
    static ENDS: OnceLock<Result<Ends, String>> = OnceLock::new();
    ENDS.get_or_init(|| {
        Ok(Ends { realistic: parse("realistic", REALISTIC, false)?, chibi: parse("chibi-extreme", CHIBI, true)? })
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// Parses one end: sections of the mapping, each a table of factors. `up` says which side of 1 the factors lie on.
///
/// # Errors
/// See [`ends`].
pub fn parse(name: &str, text: &str, up: bool) -> Result<Factors, String> {
    let sections: BTreeMap<String, BTreeMap<String, f64>> =
        toml::from_str(text).map_err(|e| format!("the {name} exaggeration table: {e}"))?;
    let defaults = defaults()?;
    let mut factors = Factors::new();
    for (section, entries) in sections {
        if matches!(section.as_str(), "rest" | "amplify") {
            return Err(format!("{name}: the dial doesn't touch '{section}', the character's own design"));
        }
        for (key, factor) in entries {
            let path = format!("{section}/{key}");
            if !defaults.contains_key(&path) {
                return Err(format!("{name}: the mapping has no coefficient '{path}'"));
            }
            let on_side = if up { factor >= 1.0 } else { factor > 0.0 && factor <= 1.0 };
            if !(factor.is_finite() && on_side) {
                let want = if up { "at least 1" } else { "above 0 and at most 1" };
                return Err(format!("{name}: {path} has factor {factor}, which must be {want}"));
            }
            factors.insert(path, factor);
        }
    }
    Ok(factors)
}

/// The default mapping's coefficients by path.
fn defaults() -> Result<BTreeMap<String, f64>, String> {
    let value = serde_json::to_value(FaceMapping::default()).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    for (section, entries) in value.as_object().into_iter().flatten() {
        for (key, number) in entries.as_object().into_iter().flatten() {
            if let Some(n) = number.as_f64() {
                out.insert(format!("{section}/{key}"), n);
            }
        }
    }
    Ok(out)
}

/// The factor at `amount` for a path whose ends are `low` (at -1) and `high` (at +1): 1 at the centre, a straight line to
/// each end.
fn factor(amount: f64, low: f64, high: f64) -> f64 {
    if amount >= 0.0 { 1.0 + amount * (high - 1.0) } else { 1.0 - amount * (low - 1.0) }
}

/// The coefficients the dial sets at `amount` (clamped to `-1..=1`), by path: every path either end names, as the
/// default times its factor, to four decimals. At 0 these are the defaults exactly.
///
/// # Errors
/// See [`ends`].
pub fn gains(amount: f64) -> Result<BTreeMap<String, f64>, String> {
    let amount = if amount.is_finite() { amount.clamp(-1.0, 1.0) } else { 0.0 };
    let ends = ends()?;
    let defaults = defaults()?;
    let paths = ends.realistic.keys().chain(ends.chibi.keys());
    let mut out = BTreeMap::new();
    for path in paths {
        let default = *defaults.get(path).ok_or_else(|| format!("no default for {path}"))?;
        let low = ends.realistic.get(path).copied().unwrap_or(1.0);
        let high = ends.chibi.get(path).copied().unwrap_or(1.0);
        let f = factor(amount, low, high);
        // Rounding is monotone, so the dial stays monotone; at f = 1 the default comes back as it was written.
        #[allow(clippy::float_cmp, reason = "exactly 1 is the identity, which keeps the default bit for bit")]
        let value = if f == 1.0 { default } else { (default * f * 1e4).round() / 1e4 };
        out.insert(path.clone(), value);
    }
    Ok(out)
}

/// The dial's coefficients as a nested object (`{"eyes": {"wide": ...}}`), the shape a mapping edit merges.
///
/// # Errors
/// See [`ends`].
pub fn gains_object(amount: f64) -> Result<Value, String> {
    let mut object = serde_json::Map::new();
    for (path, value) in gains(amount)? {
        let (section, key) = path.split_once('/').ok_or_else(|| format!("'{path}' is not section/key"))?;
        let number = serde_json::Number::from_f64(value).ok_or_else(|| format!("{path} is not finite"))?;
        let entry = object.entry(section.to_owned()).or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Value::Object(map) = entry {
            map.insert(key.to_owned(), Value::Number(number));
        }
    }
    Ok(Value::Object(object))
}

/// `base` with the dial's coefficients at `amount` written over it.
///
/// # Errors
/// See [`ends`]; also a mapping that doesn't round-trip through JSON.
pub fn at(base: &FaceMapping, amount: f64) -> Result<FaceMapping, String> {
    let mut value = serde_json::to_value(base).map_err(|e| e.to_string())?;
    let gains = gains_object(amount)?;
    for (section, entries) in gains.as_object().into_iter().flatten() {
        for (key, number) in entries.as_object().into_iter().flatten() {
            if let Some(slot) = value.get_mut(section).and_then(|s| s.get_mut(key)) {
                *slot = number.clone();
            }
        }
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}
