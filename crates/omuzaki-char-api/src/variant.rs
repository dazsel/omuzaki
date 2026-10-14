//! Per-use variants (spec CB-13, §26 SH-10): a sheet-faithful "game" face, a longer-chin "close-up" face with bangs,
//! a thick-lined "emote".
//!
//! A variant is **a set of overrides**, each a document path (relative to the character) and a new value:
//!
//! ```toml
//! # characters/mira/variants/close-up.toml
//! description = "Longer lower face, performing eyes, bangs on"
//! extends = "game"                      # optional: start from another variant
//! [shot]
//! min = 0.35                            # use when the head fills 35% to 100% of the frame height
//! max = 1.0
//! [overrides]
//! "parts/head/params/outline/chin" = { y = -0.3, stretch_top = -0.08, min_ratio = 0.6 }
//! "face/layout/mouth_y" = -0.2
//! "face/mapping/rest/eye_open" = 1.3
//! "parts/bangs/enabled" = true
//! ```
//!
//! Why paths and not a second copy of the character: a variant stays a few readable lines that merge cleanly, only the
//! differences are stored (so editing the base flows into every variant), and the same mechanism that edits the
//! document (`omuzaki-doc-api` paths) addresses them. Resolution applies the overrides to a copy of the character and
//! re-validates it through the typed structs, so a mistyped path or value is an error naming the override, never a
//! silent no-op.

use std::collections::BTreeMap;

use omuzaki_doc_api::{DocPath, Value};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::character::CharacterDef;
use crate::ids::VariantId;

/// How big the head is in the frame: the head's height over the frame height (about 0.1 for a full-body shot,
/// 0.5 for a portrait, 1 and above for an extreme close-up). Independent of lens and aspect ratio, so the same ranges
/// work in every shot (SH-10).
pub type HeadCoverage = f64;

/// Extra coverage on each side of the current variant's range before the selection switches away, so a slow zoom
/// doesn't flicker between two faces at the boundary.
pub const SHOT_HYSTERESIS: HeadCoverage = 0.04;

/// The shots a variant is meant for.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShotRange {
    /// Smallest [`HeadCoverage`] (inclusive).
    pub min: HeadCoverage,
    /// Largest [`HeadCoverage`] (exclusive).
    pub max: HeadCoverage,
}

impl ShotRange {
    /// True if `coverage` is inside `[min, max)`.
    pub fn contains(&self, coverage: HeadCoverage) -> bool {
        coverage >= self.min && coverage < self.max
    }
}

/// One variant file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct VariantDef {
    /// What it is for, in a sentence.
    pub description: String,
    /// Another variant to start from; its overrides apply first.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub extends: Option<VariantId>,
    /// Document path (from the character root) to new value.
    pub overrides: BTreeMap<DocPath, Value>,
    /// The shots it is for; `None` means it is only chosen by name.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shot: Option<ShotRange>,
}

/// All variants of a character, by id.
pub type VariantSet = BTreeMap<VariantId, VariantDef>;

/// Why a variant can't be applied.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum VariantError {
    /// No such variant.
    #[error("no variant '{0}': variants are listed under characters/<id>/variants")]
    Unknown(VariantId),
    /// `extends` chains loop.
    #[error("variants extend each other in a loop: {}", .0.iter().map(VariantId::as_str).collect::<Vec<_>>().join(" -> "))]
    Cycle(Vec<VariantId>),
    /// One override can't be applied.
    #[error("variant '{variant}': override '{path}' {reason}")]
    Override {
        /// The variant.
        variant: VariantId,
        /// The override's path.
        path: DocPath,
        /// Why it fails.
        reason: String,
    },
    /// The base character can't be turned into a document tree.
    #[error("the character can't be serialised: {0}")]
    Serialise(String),
}

/// `base` with the variant `id` (and the variants it extends, oldest first) applied.
///
/// Overrides write into the character's serialised tree: an existing key is replaced; a missing key is created when
/// its parent is an object (which is how a sparse `params` table gains a field); array elements are replaced in place
/// by index. The result must still be a valid [`CharacterDef`], so a wrong type or an unknown key in the typed parts
/// is an error that names the override that caused it. Part parameters are checked when the part builds (their types
/// belong to the part kinds), where an unknown key is an error too.
///
/// # Errors
/// [`VariantError::Unknown`] for a missing variant, [`VariantError::Cycle`] for looping `extends`, and
/// [`VariantError::Override`] for the first override that can't be applied or breaks the character.
pub fn resolve_variant(
    base: &CharacterDef,
    variants: &VariantSet,
    id: &VariantId,
) -> Result<CharacterDef, VariantError> {
    let mut chain = vec![id.clone()];
    let mut cur = variants.get(id).ok_or_else(|| VariantError::Unknown(id.clone()))?;
    while let Some(parent) = &cur.extends {
        if chain.contains(parent) {
            chain.push(parent.clone());
            return Err(VariantError::Cycle(chain));
        }
        cur = variants.get(parent).ok_or_else(|| VariantError::Unknown(parent.clone()))?;
        chain.push(parent.clone());
    }
    let mut tree = serde_json::to_value(base).map_err(|e| VariantError::Serialise(e.to_string()))?;
    for variant in chain.iter().rev() {
        for (path, value) in &variants[variant].overrides {
            let fail = |reason: String| VariantError::Override { variant: variant.clone(), path: path.clone(), reason };
            set_at(&mut tree, path, value.clone()).map_err(fail)?;
            // Typed check after each override, so the culprit is named exactly.
            serde_json::from_value::<CharacterDef>(tree.clone())
                .map_err(|e| fail(format!("makes the character invalid: {e}")))?;
        }
    }
    serde_json::from_value(tree).map_err(|e| VariantError::Serialise(e.to_string()))
}

/// Sets the value at `path`, creating missing object keys but never array slots.
fn set_at(root: &mut Value, path: &DocPath, value: Value) -> Result<(), String> {
    let Some((parent, last)) = path.split_last() else { return Err("is empty: name a field to override".to_owned()) };
    let mut cur = root;
    for seg in parent.segments() {
        cur = match cur {
            Value::Object(map) => map.entry(seg.clone()).or_insert_with(|| Value::Object(serde_json::Map::new())),
            Value::Array(items) => {
                let i: usize = seg.parse().map_err(|_| format!("has '{seg}' where an array index is expected"))?;
                items.get_mut(i).ok_or_else(|| format!("has index {i}, past the end of the list"))?
            }
            _ => return Err(format!("goes through '{seg}', which is a plain value, not a table")),
        };
    }
    match cur {
        Value::Object(map) => {
            map.insert(last.to_owned(), value);
            Ok(())
        }
        Value::Array(items) => {
            let i: usize = last.parse().map_err(|_| format!("has '{last}' where an array index is expected"))?;
            let slot = items.get_mut(i).ok_or_else(|| format!("has index {i}, past the end of the list"))?;
            *slot = value;
            Ok(())
        }
        _ => Err("targets something inside a plain value".to_owned()),
    }
}

/// The variant to use at `coverage`: the one with the narrowest [`ShotRange`] containing it. If `current` is still
/// within its range widened by [`SHOT_HYSTERESIS`], it stays selected.
pub fn select_variant<'a>(
    variants: &'a VariantSet,
    coverage: HeadCoverage,
    current: Option<&VariantId>,
) -> Option<&'a VariantId> {
    if let Some((id, range)) =
        current.and_then(|c| variants.get_key_value(c)).and_then(|(id, v)| v.shot.map(|r| (id, r)))
        && coverage >= range.min - SHOT_HYSTERESIS
        && coverage < range.max + SHOT_HYSTERESIS
    {
        return Some(id);
    }
    variants
        .iter()
        .filter_map(|(id, v)| v.shot.filter(|r| r.contains(coverage)).map(|r| (id, r.max - r.min)))
        .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(b.0)))
        .map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::PartDef;
    use crate::ids::{BodyTemplateId, PartId, PartKindId};
    use omuzaki_face_api::{FaceProfile, Palette};
    use serde_json::json;

    fn vid(s: &str) -> VariantId {
        VariantId::new(s).unwrap()
    }

    fn path(s: &str) -> DocPath {
        DocPath::parse(s).unwrap()
    }

    fn base() -> CharacterDef {
        let head = PartDef {
            kind: PartKindId::new("head").unwrap(),
            parent: None,
            enabled: true,
            layer: 0,
            attach: None,
            params: json!({"outline": {"exponent": 1.9}}),
        };
        let bangs = PartDef {
            kind: PartKindId::new("hair").unwrap(),
            parent: None,
            enabled: false,
            layer: 0,
            attach: None,
            params: json!({}),
        };
        CharacterDef {
            name: "Test".to_owned(),
            template: BodyTemplateId::new("soft-sd").unwrap(),
            seed: 3,
            palette: Palette::default(),
            skin_tone: None,
            parts: [(PartId::new("head").unwrap(), head), (PartId::new("bangs").unwrap(), bangs)].into(),
            face: FaceProfile::default(),
            weight_brushes: BTreeMap::new(),
        }
    }

    fn variant(extends: Option<&str>, overrides: &[(&str, Value)], shot: Option<(f64, f64)>) -> VariantDef {
        VariantDef {
            description: String::new(),
            extends: extends.map(vid),
            overrides: overrides.iter().map(|(p, v)| (path(p), v.clone())).collect(),
            shot: shot.map(|(min, max)| ShotRange { min, max }),
        }
    }

    #[test]
    fn overrides_replace_typed_fields_and_create_keys_in_sparse_params() {
        let vs: VariantSet = [(
            vid("close-up"),
            variant(
                None,
                &[
                    ("face/layout/mouth_y", json!(-0.2)),
                    ("parts/head/params/outline/chin", json!({"y": -0.3})),
                    ("parts/bangs/enabled", json!(true)),
                    ("face/mapping/rest/eye_open", json!(1.3)),
                ],
                None,
            ),
        )]
        .into();
        let out = resolve_variant(&base(), &vs, &vid("close-up")).unwrap();
        assert_eq!(out.face.layout.mouth_y, -0.2);
        assert_eq!(out.face.mapping.rest.eye_open, 1.3);
        assert!(out.parts[&PartId::new("bangs").unwrap()].enabled);
        assert_eq!(
            out.parts[&PartId::new("head").unwrap()].params,
            json!({"outline": {"exponent": 1.9, "chin": {"y": -0.3}}})
        );
        assert_eq!(base().face.layout.mouth_y, FaceProfile::default().layout.mouth_y, "the base is not touched");
    }

    #[test]
    fn extends_applies_the_parent_first_so_the_child_wins() {
        let vs: VariantSet = [
            (
                vid("game"),
                variant(None, &[("face/layout/mouth_y", json!(-0.16)), ("face/layout/nose_y", json!(-0.12))], None),
            ),
            (vid("close-up"), variant(Some("game"), &[("face/layout/mouth_y", json!(-0.2))], None)),
        ]
        .into();
        let out = resolve_variant(&base(), &vs, &vid("close-up")).unwrap();
        assert_eq!((out.face.layout.mouth_y, out.face.layout.nose_y), (-0.2, -0.12));
    }

    #[test]
    fn mistakes_name_the_variant_and_the_override() {
        let vs: VariantSet = [
            (vid("typo"), variant(None, &[("face/layoutt/mouth_y", json!(-0.2))], None)),
            (vid("wrong-type"), variant(None, &[("face/layout/mouth_y", json!("low"))], None)),
            (vid("past-end"), variant(None, &[("face/marks/3", json!({}))], None)),
            (vid("inside-value"), variant(None, &[("name/x", json!(1))], None)),
        ]
        .into();
        for (v, needle) in [
            ("typo", "layoutt"),
            ("wrong-type", "invalid type"),
            ("past-end", "past the end"),
            ("inside-value", "plain value"),
        ] {
            let e = resolve_variant(&base(), &vs, &vid(v)).unwrap_err().to_string();
            assert!(e.contains(&format!("variant '{v}'")) && e.contains(needle), "{e}");
        }
    }

    #[test]
    fn loops_and_unknown_names_are_reported() {
        let vs: VariantSet = [
            (vid("a"), variant(Some("b"), &[], None)),
            (vid("b"), variant(Some("a"), &[], None)),
            (vid("c"), variant(Some("zzz"), &[], None)),
        ]
        .into();
        assert_eq!(
            resolve_variant(&base(), &vs, &vid("a")),
            Err(VariantError::Cycle(vec![vid("a"), vid("b"), vid("a")]))
        );
        assert_eq!(resolve_variant(&base(), &vs, &vid("c")), Err(VariantError::Unknown(vid("zzz"))));
        assert_eq!(resolve_variant(&base(), &vs, &vid("nope")), Err(VariantError::Unknown(vid("nope"))));
    }

    #[test]
    fn selection_picks_the_narrowest_range_and_holds_with_hysteresis() {
        let vs: VariantSet = [
            (vid("wide"), variant(None, &[], Some((0.0, 0.3)))),
            (vid("game"), variant(None, &[], Some((0.0, 0.6)))),
            (vid("close-up"), variant(None, &[], Some((0.3, 5.0)))),
            (vid("manual"), variant(None, &[], None)),
        ]
        .into();
        let pick = |c: f64, cur: Option<&str>| select_variant(&vs, c, cur.map(vid).as_ref()).map(VariantId::to_string);
        assert_eq!(pick(0.1, None).as_deref(), Some("wide"), "narrowest of wide and game");
        assert_eq!(pick(0.45, None).as_deref(), Some("game"));
        assert_eq!(pick(2.0, None).as_deref(), Some("close-up"));
        assert_eq!(pick(6.0, None), None, "nothing covers it");
        // At 0.31 the narrowest match is game; but a held `wide` stays until 0.34.
        assert_eq!(pick(0.31, None).as_deref(), Some("game"));
        assert_eq!(pick(0.31, Some("wide")).as_deref(), Some("wide"));
        assert_eq!(pick(0.35, Some("wide")).as_deref(), Some("game"));
        assert_eq!(pick(0.5, Some("manual")).as_deref(), Some("game"), "a variant without a range never holds");
    }

    #[test]
    fn a_variant_file_round_trips() {
        let json = r#"{"description":"d","extends":"game","overrides":{"face/layout/mouth_y":-0.2},"shot":{"min":0.3,"max":1.0}}"#;
        let v: VariantDef = serde_json::from_str(json).unwrap();
        assert_eq!(v.overrides[&path("face/layout/mouth_y")], json!(-0.2));
        assert_eq!(serde_json::from_str::<VariantDef>(&serde_json::to_string(&v).unwrap()).unwrap(), v);
        assert!(serde_json::from_str::<VariantDef>(r#"{"override":{}}"#).is_err());
    }
}
