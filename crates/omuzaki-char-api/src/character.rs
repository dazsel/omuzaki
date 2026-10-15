//! The character as stored in the project (spec §7): a flat map of parts that forms a tree, a palette and a face.
//!
//! Why a map keyed by [`PartId`] with `parent` links instead of nested arrays: the project must merge cleanly and
//! patch exactly (`omuzaki-doc-api` arrays only change in place or at the end). A map gives every part a stable
//! address (`characters/<id>/parts/<part>/params/...`), and re-parenting or reordering is one field change.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_core::text::edit_distance;
use omuzaki_doc_api::Value;
use omuzaki_face_api::{FaceProfile, Palette};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::doc_key;
use crate::ids::{AnchorId, BodyTemplateId, JointRef, PartId, PartKindId, SkinToneId};
use crate::skin::WeightBrush;

/// Where characters live in the document tree, as typed paths. Ops build patches against these; nothing else
/// should spell a character path by hand.
pub mod paths {
    use omuzaki_doc_api::DocPath;

    use crate::ids::{CharId, ColourwayId, PartId, VariantId};

    /// `characters/<id>`: the whole [`CharacterDef`](super::CharacterDef) (stored across `character.toml`, `face.toml`).
    pub fn character(id: &CharId) -> DocPath {
        DocPath::root().join("characters").join(id.as_str())
    }

    /// `characters/<id>/parts/<part>`: one [`PartDef`](super::PartDef).
    pub fn part(id: &CharId, part: &PartId) -> DocPath {
        character(id).join("parts").join(part.as_str())
    }

    /// `characters/<id>/parts/<part>/params`: a part's parameters.
    pub fn part_params(id: &CharId, part_id: &PartId) -> DocPath {
        part(id, part_id).join("params")
    }

    /// `characters/<id>/face`: the [`FaceProfile`](omuzaki_face_api::FaceProfile) (`face.toml`).
    pub fn face(id: &CharId) -> DocPath {
        character(id).join("face")
    }

    /// `characters/<id>/weight_brushes`: every part's [`WeightBrush`](crate::WeightBrush) list.
    pub fn weight_brushes(id: &CharId) -> DocPath {
        character(id).join("weight_brushes")
    }

    /// `characters/<id>/palette`.
    pub fn palette(id: &CharId) -> DocPath {
        character(id).join("palette")
    }

    /// `characters/<id>/colourways`: every saved colourway (LK-03).
    pub fn colourways(id: &CharId) -> DocPath {
        character(id).join("colourways")
    }

    /// `characters/<id>/colourways/<name>`: one colourway, a whole [`Palette`](omuzaki_face_api::Palette) kept beside the
    /// live one so a swap is one patch (`palette` takes the colours of the colourway, slot for slot).
    ///
    /// Parts refer to colours by slot, so a colourway recolours a character by setting these standard slots:
    ///
    /// | Slots | Used for |
    /// |---|---|
    /// | `skin` `skin_shade` `skin_line` `skin_rim` | skin: lit, shadowed, outline, rim light (head, body, ears) |
    /// | `blush` `lip` `eyes` | the face |
    /// | `hair_root` `hair_mid` `hair_tip` | the hair gradient |
    /// | `outfit_a` `outfit_b` `trim` | garments: main and second colour, and the accent |
    pub fn colourway(id: &CharId, name: &ColourwayId) -> DocPath {
        colourways(id).join(name.as_str())
    }

    /// `characters/<id>/look`: the [`LookDef`](crate::LookDef) (`look.toml`), the preset and the person's overrides.
    pub fn look(id: &CharId) -> DocPath {
        character(id).join("look")
    }

    /// `characters/<id>/variants`: every [`VariantDef`](crate::VariantDef) (`variants/<name>.toml`).
    pub fn variants(id: &CharId) -> DocPath {
        character(id).join("variants")
    }

    /// `characters/<id>/variants/<variant>`.
    pub fn variant(id: &CharId, v: &VariantId) -> DocPath {
        variants(id).join(v.as_str())
    }

    /// `characters/<id>/applied`: the library application records, one per applied item (the records are the lock,
    /// ADR 0019).
    pub fn applied(id: &CharId) -> DocPath {
        character(id).join("applied")
    }

    /// `characters/<id>/applied/<app>`: one library application record, under its application key (the slot or a
    /// generated key, library design D6), for example `hairstyle`, `poses-wave` or `headphones-2`.
    pub fn application(id: &CharId, app: &str) -> DocPath {
        applied(id).join(app)
    }

    /// `characters/<id>/poses`: the character's pose set.
    pub fn poses(id: &CharId) -> DocPath {
        character(id).join("poses")
    }

    /// `characters/<id>/poses/<name>`: one pose, named by the pose.
    pub fn pose(id: &CharId, name: &str) -> DocPath {
        poses(id).join(name)
    }

    /// `characters/<id>/emotes`: the character's emote set.
    pub fn emotes(id: &CharId) -> DocPath {
        character(id).join("emotes")
    }

    /// `characters/<id>/emotes/<name>`: one emote, named by the emote.
    pub fn emote(id: &CharId, name: &str) -> DocPath {
        emotes(id).join(name)
    }
}

/// An offset for attachments: translation, XYZ Euler rotation in degrees, uniform scale. (Degrees and a plain scale, so
/// people and agents can edit it; the rig's `Transform` is a quaternion.)
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AttachOffset {
    /// Offset in metres, in the target's frame.
    pub translation: [f64; 3],
    /// Rotation in degrees about x, y, z (applied in that order).
    pub rotation_deg: [f64; 3],
    /// Uniform scale.
    pub scale: f64,
}

impl Default for AttachOffset {
    fn default() -> Self {
        Self { translation: [0.0; 3], rotation_deg: [0.0; 3], scale: 1.0 }
    }
}

/// Where an accessory is fixed (CB-43): a bone or another part's anchor, plus an offset from it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "to", rename_all = "snake_case", deny_unknown_fields)]
pub enum Attach {
    /// A bone of the skeleton.
    Bone {
        /// The bone: a humanoid role or an extra bone's name.
        bone: JointRef,
        /// Offset from the bone.
        #[serde(default)]
        offset: AttachOffset,
    },
    /// An anchor another part publishes (the `top` of the head, the `tip` of a braid).
    Anchor {
        /// The part that publishes it. It builds before this part.
        part: PartId,
        /// The anchor's name.
        anchor: AnchorId,
        /// Offset from the anchor.
        #[serde(default)]
        offset: AttachOffset,
    },
}

impl Attach {
    /// The offset from whatever the part is fixed to.
    pub fn offset(&self) -> &AttachOffset {
        match self {
            Self::Bone { offset, .. } | Self::Anchor { offset, .. } => offset,
        }
    }
}

fn enabled_by_default() -> bool {
    true
}

fn empty_params() -> Value {
    Value::Object(serde_json::Map::new())
}

/// One part of a character.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartDef {
    /// Which generator builds it; selects the parameter schema.
    pub kind: PartKindId,
    /// The part this one belongs to (a hood to its robe, a clasp to its robe). A disabled parent disables its children.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent: Option<PartId>,
    /// Disabled parts are skipped entirely (CB-91 variants of a part: hair A, B, C side by side).
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// Layering order, inner to outer: parts with a lower layer build first and garments fit over them. Convention:
    /// body and head 0, underwear 10, shirt 20, vest 30, outer garments 40+, accessories 50+.
    #[serde(default)]
    pub layer: i32,
    /// Where an accessory is fixed; `None` for parts skinned to the template skeleton.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub attach: Option<Attach>,
    /// The kind's parameters, validated against its schema (`PartKindRegistration::check`). Absent keys mean defaults.
    #[serde(default = "empty_params")]
    pub params: Value,
}

/// A character: everything in `character.toml` and `face.toml`. Rig, look and variants live in their own files
/// (`rig.toml`, `look.toml`, `variants/<name>.toml`) and their own contracts.
///
/// `#[non_exhaustive]`: other crates make one with [`CharacterDef::new`] and the `with_*` methods (or assign the public
/// fields), never a struct literal or an exhaustive pattern, so a field added here edits only this crate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct CharacterDef {
    /// Display name (free text).
    pub name: String,
    /// The body template, which fixes the skeleton.
    pub template: BodyTemplateId,
    /// Seeds every procedural part, so a rebuild is identical. (32 bits: safe as a JSON number everywhere.)
    #[serde(default)]
    pub seed: u32,
    /// Named colours that parts refer to by slot.
    #[serde(default)]
    pub palette: Palette,
    /// The skin tone chosen from the library. Informational, like `HeadParams::preset`: the colours that build are the
    /// explicit `skin*` palette slots, so a later library edit never changes this character.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub skin_tone: Option<SkinToneId>,
    /// The parts, by id.
    #[serde(default)]
    pub parts: BTreeMap<PartId, PartDef>,
    /// The face design (`face.toml`).
    #[serde(default)]
    pub face: FaceProfile,
    /// Skin-weight corrections (RG-02) the builder applies to each part's skinned meshes after the part builds.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub weight_brushes: BTreeMap<PartId, Vec<WeightBrush>>,
    /// Growth probe (feature `growth-probe`): a field no other crate knows about. See `tests/growth_probe.rs`.
    #[cfg(feature = "growth-probe")]
    #[serde(skip)]
    // a saved field would also need the closed document-key lists (ADR 0054 §5), which this probe doesn't test
    pub growth_probe: u8,
}

impl CharacterDef {
    /// A character with a name and a body template: seed 0, the empty palette, no parts, the default face.
    pub fn new(name: impl Into<String>, template: BodyTemplateId) -> Self {
        Self {
            name: name.into(),
            template,
            seed: 0,
            palette: Palette::default(),
            skin_tone: None,
            parts: BTreeMap::new(),
            face: FaceProfile::default(),
            weight_brushes: BTreeMap::new(),
            #[cfg(feature = "growth-probe")]
            growth_probe: 0,
        }
    }

    /// Sets the seed.
    #[must_use]
    pub fn with_seed(mut self, seed: u32) -> Self {
        self.seed = seed;
        self
    }

    /// Sets the palette.
    #[must_use]
    pub fn with_palette(mut self, palette: Palette) -> Self {
        self.palette = palette;
        self
    }

    /// Sets the skin tone chosen from the library.
    #[must_use]
    pub fn with_skin_tone(mut self, skin_tone: Option<SkinToneId>) -> Self {
        self.skin_tone = skin_tone;
        self
    }

    /// Sets the parts.
    #[must_use]
    pub fn with_parts(mut self, parts: BTreeMap<PartId, PartDef>) -> Self {
        self.parts = parts;
        self
    }

    /// Sets the face design.
    #[must_use]
    pub fn with_face(mut self, face: FaceProfile) -> Self {
        self.face = face;
        self
    }

    /// Sets the skin-weight corrections.
    #[must_use]
    pub fn with_weight_brushes(mut self, weight_brushes: BTreeMap<PartId, Vec<WeightBrush>>) -> Self {
        self.weight_brushes = weight_brushes;
        self
    }
}

/// Why the parts don't form a buildable tree.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    /// A part names a parent that doesn't exist.
    #[error("part '{part}' has parent '{parent}', which isn't in the character")]
    MissingParent {
        /// The child.
        part: PartId,
        /// The missing parent.
        parent: PartId,
    },
    /// Parts wait on each other (through parents, layers or kind requirements), so none can build first.
    #[error("these parts depend on each other in a loop: {}", .0.iter().map(PartId::as_str).collect::<Vec<_>>().join(", "))]
    Cycle(Vec<PartId>),
}

/// The message for a key of a character node that no file of the character owns: the key, and the nearest valid one if
/// it is close enough to be a typo, otherwise the definition keys.
fn unknown_key_message(key: &str) -> String {
    if let Err(collision) = doc_key::check(key) {
        return format!("{collision}, so the character cannot use it");
    }
    let node_keys = &CharacterDef::NODE_KEYS;
    match nearest_name(key, node_keys.iter().copied().chain(doc_key::sibling_keys().iter().copied())) {
        Some(near) => format!("unknown field `{key}` in the character, did you mean `{near}`?"),
        None => format!(
            "unknown field `{key}` in the character, expected one of {}",
            node_keys.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// The candidate nearest to `key` if it is close enough to be a typo (at most a third of `key`'s length plus one edit
/// away); ties go to the first name in sort order. The did-you-mean of character keys and of look presets and
/// parameters, on `omuzaki-core`'s edit distance.
pub(crate) fn nearest_name<'a>(key: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let close = key.chars().count() / 3 + 1;
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(key, candidate), candidate))
        .min()
        .filter(|(distance, _)| *distance <= close)
        .map(|(_, near)| near)
}

impl CharacterDef {
    /// The keys of `characters/<id>` that make up a [`CharacterDef`] (`character.toml` and `face.toml`).
    pub const NODE_KEYS: [&'static str; 8] = doc_key::BUILT_IN_NODE_KEYS;

    /// The other keys of `characters/<id>`: the character's other files, each a sub-tree with its own contract
    /// (`variants`, `colourways`, `rig.toml`, `look.toml`, the `licence` record, the per-part `sources` rows, the
    /// `applied` library records, the `poses` and the `emotes` sets). They are not part of the definition;
    /// [`from_node`](Self::from_node) skips them and [`to_node`](Self::to_node) leaves them alone.
    ///
    /// This is the built-in list; a new file of the character registers its key with
    /// [`register_doc_key!`](crate::register_doc_key), and [`doc_key::sibling_keys`] lists both.
    pub const SIBLING_KEYS: [&'static str; 9] = doc_key::BUILT_IN_SIBLING_KEYS;

    /// The definition stored in the character node `node` (`characters/<id>`): reads [`NODE_KEYS`](Self::NODE_KEYS) and
    /// skips the [`SIBLING_KEYS`](Self::SIBLING_KEYS), so a character with variants, colourways, a rig or a look reads
    /// like any other. Files written before a field existed still read (every optional field has a default).
    ///
    /// A key in neither list is refused, as `deny_unknown_fields` refuses it inside the definition: a misspelt
    /// `skintone` must not read as an absent `skin_tone` and be lost on the next save.
    ///
    /// # Errors
    /// The [`serde_json::Error`] naming the unknown key (with the nearest valid key when one is close) or the field
    /// that does not match the character format, or a node that is not a table.
    pub fn from_node(node: &Value) -> Result<Self, serde_json::Error> {
        let own = match node {
            Value::Object(map) => {
                if let Some(unknown) = map.keys().find(|key| doc_key::check(key).is_err() || !Self::is_node_key(key)) {
                    return Err(<serde_json::Error as serde::de::Error>::custom(unknown_key_message(unknown)));
                }
                Value::Object(
                    map.iter()
                        .filter(|(key, _)| Self::NODE_KEYS.contains(&key.as_str()))
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                )
            }
            other => other.clone(),
        };
        serde_json::from_value(own)
    }

    /// True for a key a character node may hold: a definition key or the key of another file.
    fn is_node_key(key: &str) -> bool {
        doc_key::is_node_key(key) || doc_key::is_sibling_key(key)
    }

    /// Writes this definition into the character node `into`: sets the [`NODE_KEYS`](Self::NODE_KEYS) it has a value
    /// for, removes the ones it has none for (an absent skin tone leaves no key, never `null`), and keeps every other
    /// key (the sibling sub-trees) as it was. Keys already in the node keep their place. A node that is not a table (a
    /// new character starts as `null`) becomes one.
    ///
    /// Reading the node back with [`from_node`](Self::from_node) gives an equal definition.
    ///
    /// # Errors
    /// The [`serde_json::Error`] if the definition cannot be turned into a table (it can for any value the type
    /// holds).
    pub fn to_node(&self, into: &mut Value) -> Result<(), serde_json::Error> {
        let Value::Object(written) = serde_json::to_value(self)? else {
            return Err(<serde_json::Error as serde::ser::Error>::custom("a character definition is a table"));
        };
        match into {
            Value::Object(node) => {
                node.retain(|key, _| !Self::NODE_KEYS.contains(&key.as_str()) || written.contains_key(key));
                node.extend(written);
            }
            other => *other = Value::Object(written),
        }
        Ok(())
    }

    /// The enabled children of `parent`, in id order.
    pub fn children<'a>(&'a self, parent: &'a PartId) -> impl Iterator<Item = &'a PartId> + 'a {
        self.parts.iter().filter(move |(_, p)| p.enabled && p.parent.as_ref() == Some(parent)).map(|(id, _)| id)
    }

    /// The parts that will build (enabled, and every ancestor enabled), in id order.
    ///
    /// # Errors
    /// [`TreeError::MissingParent`] if an enabled part names a parent that doesn't exist.
    pub fn active_parts(&self) -> Result<BTreeSet<PartId>, TreeError> {
        let mut active = BTreeSet::new();
        for (id, part) in &self.parts {
            if !part.enabled {
                continue;
            }
            let (mut cur, mut on, mut hops) = (part, true, 0);
            while let Some(parent) = &cur.parent {
                let Some(p) = self.parts.get(parent) else {
                    return Err(TreeError::MissingParent { part: id.clone(), parent: parent.clone() });
                };
                on &= p.enabled;
                cur = p;
                hops += 1;
                if hops > self.parts.len() {
                    // A parent loop; reported as a cycle by `build_order`, which has the loop's members.
                    break;
                }
            }
            if on {
                active.insert(id.clone());
            }
        }
        Ok(active)
    }

    /// The order to build the active parts in. A part builds after:
    /// 1. its parent,
    /// 2. every active part with a lower `layer` (so garments fit over what is inside them),
    /// 3. every active part whose kind its own kind `requires` (the face host before the hair that reads it).
    ///
    /// Among parts that could go next, the lowest `(layer, id)` goes first, so the order is deterministic.
    /// `requires` maps a kind to the kinds that must build before it (the registry supplies it; tests pass a closure).
    ///
    /// # Errors
    /// [`TreeError::MissingParent`] for a dangling parent, [`TreeError::Cycle`] if the rules above contradict each other.
    pub fn build_order(&self, requires: &dyn Fn(&PartKindId) -> Vec<PartKindId>) -> Result<Vec<PartId>, TreeError> {
        let active = self.active_parts()?;
        let mut waits: BTreeMap<PartId, BTreeSet<PartId>> = BTreeMap::new();
        for id in &active {
            let part = &self.parts[id];
            let needs = requires(&part.kind);
            let deps = active
                .iter()
                .filter(|other| {
                    let o = &self.parts[*other];
                    *other != id
                        && (part.parent.as_ref() == Some(*other) || o.layer < part.layer || needs.contains(&o.kind))
                })
                .cloned()
                .collect();
            waits.insert(id.clone(), deps);
        }
        let mut order = Vec::with_capacity(active.len());
        while !waits.is_empty() {
            let next = waits
                .iter()
                .filter(|(_, d)| d.is_empty())
                .min_by_key(|(id, _)| (self.parts[*id].layer, (*id).clone()))
                .map(|(id, _)| id.clone());
            let Some(next) = next else {
                return Err(TreeError::Cycle(waits.into_keys().collect()));
            };
            waits.remove(&next);
            for deps in waits.values_mut() {
                deps.remove(&next);
            }
            order.push(next);
        }
        Ok(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> PartId {
        PartId::new(s).unwrap()
    }

    fn part(kind: &str, layer: i32, parent: Option<&str>) -> PartDef {
        PartDef {
            kind: PartKindId::new(kind).unwrap(),
            parent: parent.map(id),
            enabled: true,
            layer,
            attach: None,
            params: empty_params(),
        }
    }

    fn def(parts: &[(&str, PartDef)]) -> CharacterDef {
        CharacterDef::new("Test", BodyTemplateId::new("soft-sd").unwrap())
            .with_seed(1)
            .with_parts(parts.iter().map(|(n, p)| (id(n), p.clone())).collect())
    }

    fn no_requirements(_: &PartKindId) -> Vec<PartKindId> {
        Vec::new()
    }

    #[test]
    fn lower_layers_build_first_then_ids_break_ties() {
        let c = def(&[
            ("vest", part("vest", 30, None)),
            ("shirt", part("shirt", 20, None)),
            ("body", part("body", 0, None)),
            ("head", part("head", 0, None)),
        ]);
        let order: Vec<_> = c.build_order(&no_requirements).unwrap().iter().map(ToString::to_string).collect();
        assert_eq!(order, ["body", "head", "shirt", "vest"]);
    }

    #[test]
    fn kind_requirements_and_parents_order_same_layer_parts() {
        let c = def(&[
            ("hair", part("hair", 0, None)),
            ("head", part("head", 0, None)),
            ("hood", part("hood", 0, Some("robe"))),
            ("robe", part("robe", 0, None)),
        ]);
        let needs = |k: &PartKindId| if k.as_str() == "hair" { vec![PartKindId::new("head").unwrap()] } else { vec![] };
        let order: Vec<_> = c.build_order(&needs).unwrap().iter().map(ToString::to_string).collect();
        assert_eq!(order, ["head", "hair", "robe", "hood"], "hair after head; hood after its parent robe");
    }

    #[test]
    fn disabled_parts_and_their_children_are_skipped() {
        let mut c = def(&[
            ("robe", part("robe", 40, None)),
            ("hood", part("hood", 40, Some("robe"))),
            ("head", part("head", 0, None)),
        ]);
        c.parts.get_mut(&id("robe")).unwrap().enabled = false;
        let order: Vec<_> = c.build_order(&no_requirements).unwrap().iter().map(ToString::to_string).collect();
        assert_eq!(order, ["head"]);
        assert_eq!(c.children(&id("robe")).count(), 1, "the child is still listed; it just doesn't build");
    }

    #[test]
    fn contradictory_rules_are_a_named_cycle() {
        // The body (layer 20) is required by the head (layer 0), but layers say the head builds first.
        let c = def(&[("body", part("body", 20, None)), ("head", part("head", 0, None))]);
        let needs = |k: &PartKindId| if k.as_str() == "head" { vec![PartKindId::new("body").unwrap()] } else { vec![] };
        let e = c.build_order(&needs).unwrap_err();
        assert_eq!(e, TreeError::Cycle(vec![id("body"), id("head")]));
        assert!(e.to_string().contains("body, head"));
    }

    #[test]
    fn a_dangling_parent_is_an_error() {
        let c = def(&[("hood", part("hood", 0, Some("robe")))]);
        assert_eq!(
            c.build_order(&no_requirements),
            Err(TreeError::MissingParent { part: id("hood"), parent: id("robe") })
        );
    }

    #[test]
    fn a_character_round_trips_and_fills_defaults() {
        let json = r#"{"name":"Test","template":"soft-sd","parts":{"head":{"kind":"head"}}}"#;
        let c: CharacterDef = serde_json::from_str(json).unwrap();
        assert!(c.parts[&id("head")].enabled && c.parts[&id("head")].params == empty_params());
        assert_eq!((c.seed, c.skin_tone.clone()), (0, None));
        assert!(c.weight_brushes.is_empty(), "files written before weight brushes still parse");
        assert!(!serde_json::to_string(&c).unwrap().contains("weight_brushes"), "and stay unchanged when saved");
        let back: CharacterDef = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        assert!(serde_json::from_str::<CharacterDef>(r#"{"name":"x","template":"t","partz":{}}"#).is_err());
    }

    /// A character node as the document holds it: the definition, and the other files' sub-trees beside it.
    fn node_with_siblings() -> Value {
        serde_json::json!({
            "name": "Mira", "template": "soft-sd", "seed": 7, "skin_tone": "tone-03-warm",
            "parts": {"head": {"kind": "head", "layer": 2}},
            "variants": {"close-up": {"description": "face only", "overrides": {"parts/head/enabled": true}}},
            "colourways": {"winter": {"skin": "#ffffff"}},
            "rig": {"extra_bones": []},
            "look": {"light": "studio"},
            "licence": {"owner": "mira"},
            "sources": {"head": {"schema": 1}},
            "applied": {"hairstyle": {"item": "hair-bun"}},
            "poses": {"wave": {"arm_r": [0.0, 1.0, 0.0]}},
            "emotes": {"hi": {"clip": "wave-short"}},
        })
    }

    #[test]
    fn a_node_reads_as_its_definition_whatever_sits_beside_it() {
        let def = CharacterDef::from_node(&node_with_siblings()).unwrap();
        assert_eq!((def.name.as_str(), def.seed), ("Mira", 7));
        assert_eq!(def.skin_tone.as_ref().map(SkinToneId::as_str), Some("tone-03-warm"));
        assert_eq!(def.parts[&id("head")].layer, 2);
        let mut bare = node_with_siblings();
        for sibling in CharacterDef::SIBLING_KEYS {
            bare.as_object_mut().unwrap().remove(sibling);
        }
        assert_eq!(CharacterDef::from_node(&bare).unwrap(), def, "the siblings change nothing");
    }

    #[test]
    fn a_misspelt_definition_key_is_refused_with_the_nearest_valid_key() {
        let mut node = node_with_siblings();
        let skin_tone = node.as_object_mut().unwrap().remove("skin_tone").unwrap();
        node["skintone"] = skin_tone;
        let e = CharacterDef::from_node(&node).unwrap_err().to_string();
        assert!(e.contains("unknown field `skintone`") && e.contains("did you mean `skin_tone`?"), "{e}");
        node.as_object_mut().unwrap().remove("skintone");
        node["variant"] = serde_json::json!({});
        let e = CharacterDef::from_node(&node).unwrap_err().to_string();
        assert!(e.contains("did you mean `variants`?"), "a sibling's name is suggested too: {e}");
    }

    #[test]
    fn an_unrelated_key_is_refused_with_the_definition_keys() {
        let mut node = node_with_siblings();
        node["zzzzzz"] = serde_json::json!(1);
        let e = CharacterDef::from_node(&node).unwrap_err().to_string();
        assert!(e.contains("unknown field `zzzzzz`") && !e.contains("did you mean"), "{e}");
        assert!(e.contains("`skin_tone`") && e.contains("`weight_brushes`"), "it lists what is valid: {e}");
    }

    #[test]
    fn the_definition_and_the_other_files_share_no_key() {
        assert!(CharacterDef::SIBLING_KEYS.iter().all(|key| !CharacterDef::NODE_KEYS.contains(key)));
    }

    #[test]
    fn applied_poses_and_emotes_read_and_write_back_unchanged() {
        let node = node_with_siblings();
        let def = CharacterDef::from_node(&node).unwrap();
        // The first write fills in the definition's defaults; after that a read and write is the identity.
        let mut written = node.clone();
        def.to_node(&mut written).unwrap();
        for sibling in ["applied", "poses", "emotes"] {
            assert_eq!(written[sibling], node[sibling], "{sibling} is kept as it was");
        }
        let mut again = written.clone();
        CharacterDef::from_node(&written).unwrap().to_node(&mut again).unwrap();
        assert_eq!(again, written, "reading and writing back is unchanged");
        let mut misspelt = node_with_siblings();
        let applied = misspelt.as_object_mut().unwrap().remove("applied").unwrap();
        misspelt["aplied"] = applied;
        let e = CharacterDef::from_node(&misspelt).unwrap_err().to_string();
        assert!(e.contains("did you mean `applied`?"), "{e}");
    }

    #[test]
    fn a_node_that_does_not_match_the_format_is_an_error_naming_the_field() {
        let mut node = node_with_siblings();
        node["parts"]["head"]["layer"] = serde_json::json!("high");
        let e = CharacterDef::from_node(&node).unwrap_err().to_string();
        assert!(e.contains("invalid type: string"), "{e}");
        assert!(CharacterDef::from_node(&Value::Null).is_err());
    }

    #[test]
    fn writing_a_definition_keeps_the_siblings_and_leaves_no_key_for_an_absent_option() {
        let mut node = node_with_siblings();
        let mut def = CharacterDef::from_node(&node).unwrap();
        def.to_node(&mut node).unwrap();
        let before = node.clone();
        assert_eq!(before["variants"], node_with_siblings()["variants"], "the first write only fills defaults in");
        def.to_node(&mut node).unwrap();
        assert_eq!(node, before, "writing back what was read changes nothing");

        def.name = "Mira II".to_owned();
        def.skin_tone = None;
        def.to_node(&mut node).unwrap();
        assert_eq!(node["name"], "Mira II");
        assert!(node.get("skin_tone").is_none(), "an absent option is no key, not null: {node}");
        assert!(!node.to_string().contains("null"), "{node}");
        for sibling in CharacterDef::SIBLING_KEYS {
            assert_eq!(node[sibling], before[sibling], "{sibling} is another file's");
        }
        assert_eq!(CharacterDef::from_node(&node).unwrap(), def);
    }

    #[test]
    fn writing_into_a_new_node_makes_a_table_that_reads_back_equal() {
        let def = def(&[("head", part("head", 0, None)), ("hood", part("hood", 40, Some("head")))]);
        let mut node = Value::Null;
        def.to_node(&mut node).unwrap();
        assert!(node.is_object() && node.get("skin_tone").is_none());
        assert_eq!(CharacterDef::from_node(&node).unwrap(), def);
        let mut elsewhere = serde_json::json!("not a table");
        def.to_node(&mut elsewhere).unwrap();
        assert_eq!(elsewhere, node);
    }

    #[test]
    fn the_node_keys_are_exactly_the_fields_of_the_character_format() {
        let schema = serde_json::to_value(schemars::schema_for!(CharacterDef)).unwrap();
        let mut fields: Vec<&str> = schema["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        fields.sort_unstable();
        let mut keys = CharacterDef::NODE_KEYS.to_vec();
        keys.sort_unstable();
        assert_eq!(keys, fields, "a field added to CharacterDef must be listed in NODE_KEYS");
    }

    #[test]
    fn absent_options_are_not_stored_and_present_ones_are() {
        let bare = serde_json::to_value(part("hood", 40, None)).unwrap();
        assert_eq!(bare, serde_json::json!({"kind": "hood", "enabled": true, "layer": 40, "params": {}}));
        let mut worn = part("hood", 40, Some("head"));
        worn.attach = Some(Attach::Bone {
            bone: serde_json::from_str(r#"{"humanoid":"head"}"#).unwrap(),
            offset: AttachOffset::default(),
        });
        let json = serde_json::to_value(&worn).unwrap();
        assert_eq!((json["parent"].as_str(), json["attach"]["to"].as_str()), (Some("head"), Some("bone")));
        let back: PartDef = serde_json::from_value(json).unwrap();
        assert_eq!(back, worn);
        let old_file: PartDef = serde_json::from_str(r#"{"kind":"hood","parent":null,"attach":null}"#).unwrap();
        assert_eq!((old_file.parent, old_file.attach), (None, None), "files that stored null still read");
    }

    #[test]
    fn attachments_name_a_bone_or_an_anchor() {
        let a: Attach =
            serde_json::from_str(r#"{"to":"anchor","part":"head","anchor":"top","offset":{"translation":[0,0.01,0]}}"#)
                .unwrap();
        assert!(matches!(a, Attach::Anchor { .. }));
        assert_eq!((a.offset().scale, a.offset().translation), (1.0, [0.0, 0.01, 0.0]));
        let b: Attach = serde_json::from_str(r#"{"to":"bone","bone":{"humanoid":"head"}}"#).unwrap();
        assert_eq!(*b.offset(), AttachOffset::default());
        assert!(serde_json::from_str::<Attach>(r#"{"to":"bone","bone":{"humanoid":"head"},"bonee":"x"}"#).is_err());
    }

    #[test]
    fn paths_follow_the_documented_layout() {
        let (c, p) = (crate::ids::CharId::new("mira").unwrap(), id("hair"));
        assert_eq!(paths::part_params(&c, &p).to_string(), "characters/mira/parts/hair/params");
        assert_eq!(paths::face(&c).to_string(), "characters/mira/face");
        assert_eq!(paths::weight_brushes(&c).to_string(), "characters/mira/weight_brushes");
        let v = crate::ids::VariantId::new("close-up").unwrap();
        assert_eq!(paths::variant(&c, &v).to_string(), "characters/mira/variants/close-up");
        let w = crate::ids::ColourwayId::new("winter").unwrap();
        assert_eq!(paths::colourways(&c).to_string(), "characters/mira/colourways");
        assert_eq!(paths::colourway(&c, &w).to_string(), "characters/mira/colourways/winter");
        assert_eq!(paths::applied(&c).to_string(), "characters/mira/applied");
        assert_eq!(paths::application(&c, "hairstyle").to_string(), "characters/mira/applied/hairstyle");
        assert_eq!(paths::poses(&c).to_string(), "characters/mira/poses");
        assert_eq!(paths::pose(&c, "wave").to_string(), "characters/mira/poses/wave");
        assert_eq!(paths::emotes(&c).to_string(), "characters/mira/emotes");
        assert_eq!(paths::emote(&c, "hi").to_string(), "characters/mira/emotes/hi");
    }
}
