//! The item manifest (design D1): metadata, rights and a recipe of op requests, as one TOML file.

use std::collections::BTreeMap;

use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::migrations::{self, CURRENT_SCHEMA, SchemaError};
use crate::publisher::Publisher;
use crate::rights::AssetInfo;
use crate::{IdError, ItemId, ItemKind, ItemRights, Section, Target, Version, VersionError};

/// Why a manifest couldn't be read. Semantic problems are not parse errors: [`ItemManifest::check`] reports those,
/// each naming its field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Not valid TOML.
    #[error("not valid TOML: {0}")]
    Toml(String),
    /// Valid TOML, but not a manifest (a missing required key, an unknown key, a wrong type).
    #[error("not a valid manifest: {0}")]
    Shape(String),
    /// The `schema` key is missing or not a whole number.
    #[error("the manifest needs `schema = 1` (a whole number)")]
    NoSchema,
    /// A `nan` or `inf` float, which the canonical JSON form cannot hold.
    #[error("{0}: NaN and infinity are not allowed; write a finite number")]
    NonFinite(String),
    /// The schema can't be read by this build.
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

/// The kinds of body an item can be compatible with.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyKind {
    /// A humanoid skeleton.
    Humanoid,
    /// A creature skeleton.
    Creature,
}

/// What an item may be applied to. Templates and kinds are hard (an incompatible target is refused); styles only warn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compat {
    /// Body kinds; empty means any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<BodyKind>,
    /// Body template ids, or `*` for every template of those kinds; empty means any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<String>,
    /// Look styles it was designed for, or `*`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub styles: Vec<String>,
    /// Part kinds the target must have.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

/// A part a `part` item creates (`creates.<key>`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Creates {
    /// The part kind (`hair`, `skirt`).
    pub kind: String,
    /// The default part id; apply picks the next free `<id>-2`, `<id>-3` on a clash.
    pub id: String,
    /// The part's layer.
    pub layer: i32,
    /// Another created part this one hangs from, written `$parts.<key>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// The type of an item parameter.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    /// A floating-point number, within `min` and `max` if given.
    Number,
    /// A whole number, within `min` and `max` if given.
    Integer,
    /// `true` or `false`.
    Bool,
    /// One of `choices`.
    Choice,
    /// A `#rrggbb` or `#rrggbbaa` string.
    Colour,
}

/// One knob an item offers at apply time (`params.<key>`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamSpec {
    /// The value type.
    #[serde(rename = "type")]
    pub ty: ParamType,
    /// The value used when the caller gives none.
    pub default: Value,
    /// Lower bound for numbers and integers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Upper bound for numbers and integers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// The allowed values of a `choice`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// What it does, for people and agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ParamSpec {
    /// Checks `value` against the type, bounds and choices.
    ///
    /// # Errors
    /// What is wrong, in a sentence.
    pub fn validate(&self, value: &Value) -> Result<(), String> {
        let in_bounds = |n: f64| match (self.min, self.max) {
            (Some(min), _) if n < min => Err(format!("{n} is below the minimum {min}")),
            (_, Some(max)) if n > max => Err(format!("{n} is above the maximum {max}")),
            _ => Ok(()),
        };
        match self.ty {
            ParamType::Number => value.as_f64().ok_or_else(|| "expected a number".to_owned()).and_then(in_bounds),
            #[allow(clippy::cast_precision_loss)] // bounds are f64 in the schema; integers beyond 2^53 are not knobs
            ParamType::Integer => {
                let n = value.as_i64().ok_or_else(|| "expected a whole number".to_owned())?;
                in_bounds(n as f64)
            }
            ParamType::Bool => value.is_boolean().then_some(()).ok_or_else(|| "expected true or false".to_owned()),
            ParamType::Choice => match value.as_str() {
                Some(s) if self.choices.iter().any(|c| c == s) => Ok(()),
                Some(s) => Err(format!("{s:?} is not one of {:?}", self.choices)),
                None => Err("expected one of the choices".to_owned()),
            },
            ParamType::Colour => match value.as_str() {
                Some(s) if is_hex_colour(s) => Ok(()),
                _ => Err("expected a colour like #2a2346 or #2a2346ff".to_owned()),
            },
        }
    }
}

fn is_hex_colour(s: &str) -> bool {
    matches!(s.strip_prefix('#'), Some(hex) if matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A guard on a step: it runs only on these body templates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    /// Template ids, or `*`.
    pub templates: Vec<String>,
}

/// A recipe step that runs one op.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpStep {
    /// The registered op name (`part.add`).
    pub op: String,
    /// Its arguments as JSON, where a string that is exactly a placeholder is replaced at apply time (design D3).
    pub args: Value,
    /// Optional template guard.
    pub when: Option<When>,
}

/// A bundle step that applies another item (design D7).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ItemStep {
    /// The nested item's id.
    pub item: String,
    /// The version range, `^1`.
    pub version: String,
    /// Parameter values for the nested item (placeholders allowed).
    pub params: BTreeMap<String, Value>,
    /// Bindings for the nested apply: `target` and `as`, each a placeholder or a literal.
    pub bind: BTreeMap<String, String>,
    /// Optional template guard.
    pub when: Option<When>,
}

/// One step of the recipe, in the manifest's `[[step]]` order.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Run an op.
    Op(OpStep),
    /// Apply another item.
    Item(ItemStep),
}

/// The flat TOML shape of a step; [`Step`] converts to and from it so a malformed step reads as one clear error.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    op: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    args: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bind: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    when: Option<When>,
}

impl TryFrom<RawStep> for Step {
    type Error = String;
    fn try_from(raw: RawStep) -> Result<Self, String> {
        match (raw.op, raw.item) {
            (Some(op), None) => {
                if raw.version.is_some() || raw.params.is_some() || raw.bind.is_some() {
                    return Err("an `op` step takes `args`, not `version`, `params` or `bind`".to_owned());
                }
                Ok(Step::Op(OpStep {
                    op,
                    args: raw.args.unwrap_or_else(|| Value::Object(serde_json::Map::default())),
                    when: raw.when,
                }))
            }
            (None, Some(item)) => {
                if raw.args.is_some() {
                    return Err("an `item` step takes `params` and `bind`, not `args`".to_owned());
                }
                let version = raw.version.ok_or("an `item` step needs a `version` range, like \"^1\"")?;
                Ok(Step::Item(ItemStep {
                    item,
                    version,
                    params: raw.params.unwrap_or_default(),
                    bind: raw.bind.unwrap_or_default(),
                    when: raw.when,
                }))
            }
            (Some(_), Some(_)) => Err("a step is either `op` or `item`, not both".to_owned()),
            (None, None) => Err("a step needs `op` (run an op) or `item` (apply an item)".to_owned()),
        }
    }
}

impl From<&Step> for RawStep {
    fn from(step: &Step) -> Self {
        let blank = RawStep { op: None, args: None, item: None, version: None, params: None, bind: None, when: None };
        match step {
            Step::Op(s) => {
                RawStep { op: Some(s.op.clone()), args: Some(s.args.clone()), when: s.when.clone(), ..blank }
            }
            Step::Item(s) => RawStep {
                item: Some(s.item.clone()),
                version: Some(s.version.clone()),
                params: (!s.params.is_empty()).then(|| s.params.clone()),
                bind: (!s.bind.is_empty()).then(|| s.bind.clone()),
                when: s.when.clone(),
                ..blank
            },
        }
    }
}

impl Serialize for Step {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        RawStep::from(self).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Step {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Step::try_from(RawStep::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// How an item previews (design D12). Not part of the hash of the render, only of the manifest.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Framing {
    /// Whole body.
    Full,
    /// Head and shoulders.
    Bust,
    /// Face.
    Face,
    /// Hands.
    Hands,
    /// A swatch card.
    Swatch,
    /// A scene.
    Scene,
}

/// Which way the preview character faces.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Turn {
    /// Straight on.
    Front,
    /// Turned a third.
    ThreeQuarter,
}

/// The `preview` key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewSpec {
    /// The framing.
    pub framing: Framing,
    /// The turn, front by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<Turn>,
    /// A pose item to pose the preview character with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pose: Option<String>,
}

/// The `migrate` key: renames from older majors (design D16).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migrate {
    /// Old parameter name to current name.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

fn yes() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
fn is_true(b: &bool) -> bool {
    *b
}

/// An item: one TOML manifest (design D1, ADR 0019). Applying it runs its recipe; it holds no code and no mesh.
///
/// `id`, `version` and `section` are kept as written so that [`check`](Self::check) can name a bad one instead of
/// the whole file failing to parse; [`item_id`](Self::item_id), [`item_version`](Self::item_version) and
/// [`item_section`](Self::item_section) give the typed values once the manifest checks clean.
///
/// The struct is `#[non_exhaustive]` (ADR 0054): build one with [`ItemManifest::new`] and set the optional fields on
/// the result, so a new field never breaks a caller.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ItemManifest {
    /// The manifest format version (`1`).
    pub schema: u32,
    /// `publisher/name`, see [`ItemId`].
    pub id: String,
    /// `MAJOR.MINOR.PATCH`, see [`Version`].
    pub version: String,
    /// The browsing group, see [`Section`].
    pub section: String,
    /// What applying it does.
    pub kind: ItemKind,
    /// What it applies to.
    pub target: Target,
    /// An exclusive slot on the target: another item in the slot replaces this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// What people read.
    pub title: String,
    /// One sentence.
    pub summary: String,
    /// Lowercase search words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Localised titles by locale.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub titles: BTreeMap<String, String>,
    /// Localised tags by locale.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags_i18n: BTreeMap<String, Vec<String>>,
    /// One line on where the values came from; required for first-party items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Rights, see [`ItemRights`].
    pub rights: ItemRights,
    /// What it may be applied to.
    #[serde(default, skip_serializing_if = "is_default_compat")]
    pub compat: Compat,
    /// The parts a `part` item creates, by key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub creates: BTreeMap<String, Creates>,
    /// The part kinds `$target` may be, for items that change an existing part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_part: Vec<String>,
    /// The default member name of a `member` item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The item's own knobs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, ParamSpec>,
    /// Store hashes of binaries the item needs; never in first-party items.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assets: BTreeMap<String, ContentHash>,
    /// What the manifest says about each asset before it is requested: its size and its own rights. When present it
    /// has exactly the keys of `assets` ([`check`](Self::check) says which are missing or extra).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub asset_info: BTreeMap<String, AssetInfo>,
    /// The recipe, in order (`[[step]]`).
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
    /// How to preview it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<PreviewSpec>,
    /// Renames from older majors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrate: Option<Migrate>,
    /// `false` keeps it out of search; it still applies as a dependency.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub listed: bool,
    /// A reason; the item stays resolvable for pins and is hidden from search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// Who published it. Outside the canonical hash; see [`verify`](Self::verify).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<Publisher>,
    /// Hex ed25519 signature over [`signing_message`](Self::signing_message). Outside the canonical hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

fn is_default_compat(c: &Compat) -> bool {
    *c == Compat::default()
}

impl ItemManifest {
    /// A manifest at the current schema with the required fields; every optional field is empty and it is listed. Set
    /// `summary` and add `steps` on the result: [`check`](Self::check) says so until you do.
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        section: impl Into<String>,
        kind: ItemKind,
        target: Target,
        title: impl Into<String>,
        rights: ItemRights,
    ) -> Self {
        Self {
            schema: CURRENT_SCHEMA,
            id: id.into(),
            version: version.into(),
            section: section.into(),
            kind,
            target,
            slot: None,
            title: title.into(),
            summary: String::new(),
            tags: Vec::new(),
            titles: BTreeMap::new(),
            tags_i18n: BTreeMap::new(),
            origin: None,
            rights,
            compat: Compat::default(),
            creates: BTreeMap::new(),
            target_part: Vec::new(),
            name: None,
            params: BTreeMap::new(),
            assets: BTreeMap::new(),
            asset_info: BTreeMap::new(),
            steps: Vec::new(),
            preview: None,
            migrate: None,
            listed: true,
            deprecated: None,
            publisher: None,
            signature: None,
        }
    }

    /// Reads a manifest from TOML. Older schemas are migrated in memory; unknown keys are refused.
    ///
    /// # Errors
    /// A [`ParseError`]. A manifest that parses can still have [`problems`](Self::check).
    pub fn parse(toml: &str) -> Result<Self, ParseError> {
        let raw: toml::Value = toml::from_str(toml).map_err(|e| ParseError::Toml(e.to_string()))?;
        // JSON has no NaN or infinity: converting would turn them into null, silently dropping a bound or merging
        // different values into one canonical form. Refuse them first, naming the field.
        if let Some(field) = non_finite_path(&raw, String::new()) {
            return Err(ParseError::NonFinite(field));
        }
        let mut value: Value = raw.try_into().map_err(|e: toml::de::Error| ParseError::Shape(e.to_string()))?;
        let schema = value
            .get("schema")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(ParseError::NoSchema)?;
        if schema != CURRENT_SCHEMA {
            migrations::to_current(&mut value, schema)?;
        }
        serde_json::from_value(value).map_err(|e| ParseError::Shape(e.to_string()))
    }

    /// The id, once valid.
    ///
    /// # Errors
    /// [`IdError`] if `id` isn't one.
    pub fn item_id(&self) -> Result<ItemId, IdError> {
        ItemId::new(self.id.as_str())
    }

    /// The version, once valid.
    ///
    /// # Errors
    /// [`VersionError`] if `version` isn't one.
    pub fn item_version(&self) -> Result<Version, VersionError> {
        Version::parse(&self.version)
    }

    /// The section, once known.
    ///
    /// # Errors
    /// [`crate::UnknownSection`] if `section` isn't one.
    pub fn item_section(&self) -> Result<Section, crate::UnknownSection> {
        self.section.parse()
    }

    /// The canonical form (design D2): the manifest as JSON with keys sorted at every level, no whitespace and
    /// floats in shortest round-trip form (ADR 0009 rule 3). Defaults are left out, so formatting, comments and
    /// spelling out a default don't change it, and any value does. `publisher` and `signature` are not in it.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.publisher = None;
        unsigned.signature = None;
        // A manifest is plain data with string keys, so serialising it cannot fail; `Null` keeps this total.
        let value = serde_json::to_value(&unsigned).unwrap_or(Value::Null);
        let mut out = String::new();
        write_canonical(&value, &mut out);
        out.into_bytes()
    }

    /// The pin: BLAKE3 of [`canonical_bytes`](Self::canonical_bytes).
    pub fn hash(&self) -> ContentHash {
        ContentHash::of(&self.canonical_bytes())
    }
}

/// The path (`params/tail/max`, arrays by index) of the first non-finite float in `value`.
fn non_finite_path(value: &toml::Value, path: String) -> Option<String> {
    let join = |key: &str| if path.is_empty() { key.to_owned() } else { format!("{path}/{key}") };
    match value {
        toml::Value::Float(f) if !f.is_finite() => Some(path),
        toml::Value::Array(items) => {
            items.iter().enumerate().find_map(|(i, v)| non_finite_path(v, join(&i.to_string())))
        }
        toml::Value::Table(map) => map.iter().find_map(|(k, v)| non_finite_path(v, join(k))),
        _ => None,
    }
}

/// Writes `value` with object keys sorted. Sorted here rather than trusted to `serde_json`'s map type, which keeps
/// insertion order when any crate in the build turns on `preserve_order`.
fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(v, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(v, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    /// A small valid part item (design 4.3, trimmed).
    pub const TWINTAILS: &str = r#"
schema = 1
id = "omuzaki/hair/twintails"
version = "1.0.0"
section = "hair"
kind = "part"
target = "character"
slot = "hairstyle"
title = "Twintails"
summary = "Two high tails tied above the ears, with a light fringe."
tags = ["hair", "twintails"]
origin = "generator defaults, design target 3"
compat = { kinds = ["humanoid"], templates = ["*"], requires = ["head"] }
creates.hair = { kind = "hair", id = "hair", layer = 5 }
params.tail_length = { type = "number", default = 2.2, min = 0.8, max = 3.5, summary = "Tail length in head heights" }

[rights]
author = "Omuzaki"
licence = "CC0-1.0"
commercial = "ok"
owned = true

[[step]]
op = "part.add"
[step.args]
character = "$character"
kind = "hair"
id = "$parts.hair"
layer = 5
[step.args.params.groups.cap.generator.cap]
hairline_front_deg = 18.0
[step.args.params.groups.tails]
mirror = true
generator.tail = { anchor = "crown_side", length = "$params.tail_length" }
"#;
}

#[cfg(test)]
mod tests {
    use super::fixtures::TWINTAILS;
    use super::*;
    use serde_json::json;

    #[test]
    fn a_manifest_parses_and_keeps_its_recipe() {
        let m = ItemManifest::parse(TWINTAILS).unwrap();
        assert_eq!(m.item_id().unwrap().as_str(), "omuzaki/hair/twintails");
        assert_eq!(m.item_section().unwrap(), Section::Hair);
        assert_eq!(m.rights, ItemRights::first_party());
        let [Step::Op(step)] = m.steps.as_slice() else { panic!("one op step") };
        assert_eq!(step.op, "part.add");
        assert_eq!(step.args["params"]["groups"]["tails"]["generator"]["tail"]["length"], json!("$params.tail_length"));
    }

    #[test]
    fn unknown_keys_and_bad_schemas_are_refused() {
        assert!(matches!(ItemManifest::parse("schema = 1\ntypo = 2"), Err(ParseError::Shape(_))));
        assert!(matches!(ItemManifest::parse("id = 'a/b'"), Err(ParseError::NoSchema)));
        assert!(matches!(ItemManifest::parse("schema = 'one'"), Err(ParseError::NoSchema)));
        assert!(matches!(ItemManifest::parse("schema = 2"), Err(ParseError::Schema(SchemaError::Newer { found: 2 }))));
        assert!(matches!(ItemManifest::parse("schema = ["), Err(ParseError::Toml(_))));
        let both = TWINTAILS.replace("op = \"part.add\"", "op = \"part.add\"\nitem = \"a/b\"");
        assert!(ItemManifest::parse(&both).unwrap_err().to_string().contains("not both"));
    }

    #[test]
    fn the_hash_ignores_formatting_comments_and_spelled_out_defaults() {
        let a = ItemManifest::parse(TWINTAILS).unwrap();
        let reformatted = format!("# a comment\n{TWINTAILS}\n\n# another\n");
        let b = ItemManifest::parse(
            &reformatted.replace("tags = [\"hair\", \"twintails\"]", "tags=[ \"hair\",\n \"twintails\" ]"),
        )
        .unwrap();
        let c = ItemManifest::parse(&format!("listed = true\n{TWINTAILS}")).unwrap();
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a.hash(), c.hash());
    }

    #[test]
    fn the_hash_changes_with_values_but_not_with_signature_or_publisher() {
        let a = ItemManifest::parse(TWINTAILS).unwrap();
        let b =
            ItemManifest::parse(&TWINTAILS.replace("hairline_front_deg = 18.0", "hairline_front_deg = 18.5")).unwrap();
        assert_ne!(a.hash(), b.hash());
        let mut signed = a.clone();
        signed.signature = Some("ab".repeat(64));
        signed.publisher = Some(Publisher { name: "Someone".into(), key: "00".repeat(32) });
        assert_eq!(a.hash(), signed.hash());
    }

    #[test]
    fn canonical_bytes_are_sorted_compact_and_keep_float_form() {
        let m = ItemManifest::parse(TWINTAILS).unwrap();
        let text = String::from_utf8(m.canonical_bytes()).unwrap();
        assert!(
            text.starts_with(r#"{"compat":{"kinds":["humanoid"],"requires":["head"],"templates":["*"]},"creates":"#),
            "{text}"
        );
        assert!(text.contains(r#""hairline_front_deg":18.0"#) && text.contains(r#""layer":5"#), "{text}");
        assert!(!text.contains(' ') || text.contains("Two high"), "no layout whitespace outside strings");
        assert!(!text.contains("signature") && !text.contains("\"listed\""));
    }

    #[test]
    fn non_finite_numbers_are_refused_naming_the_field() {
        let cases = [
            ("max = 3.5", "max = nan", "params/tail_length/max"),
            ("min = 0.8", "min = -inf", "params/tail_length/min"),
            (
                "hairline_front_deg = 18.0",
                "hairline_front_deg = inf",
                "step/0/args/params/groups/cap/generator/cap/hairline_front_deg",
            ),
        ];
        for (from, to, field) in cases {
            let err = ItemManifest::parse(&TWINTAILS.replace(from, to)).unwrap_err();
            assert_eq!(err, ParseError::NonFinite(field.to_owned()), "{to}");
        }
    }

    #[test]
    fn param_values_are_validated_by_type_bounds_and_choices() {
        let spec = |ty, default: Value| ParamSpec {
            ty,
            default,
            min: Some(0.0),
            max: Some(2.0),
            choices: vec!["a".into()],
            summary: None,
        };
        assert!(spec(ParamType::Number, json!(1)).validate(&json!(1.5)).is_ok());
        assert!(spec(ParamType::Number, json!(1)).validate(&json!(2.5)).unwrap_err().contains("maximum"));
        assert!(spec(ParamType::Integer, json!(1)).validate(&json!(1.5)).is_err());
        assert!(spec(ParamType::Bool, json!(true)).validate(&json!("yes")).is_err());
        assert!(spec(ParamType::Choice, json!("a")).validate(&json!("b")).is_err());
        assert!(spec(ParamType::Colour, json!("#2a2346")).validate(&json!("#2a2346")).is_ok());
        assert!(spec(ParamType::Colour, json!("#2a2346")).validate(&json!("#2a23")).is_err());
    }
}
