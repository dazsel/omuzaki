//! [`AvatarDef`]: the document node of an imported character, `characters/<id>/avatar` (design D5).
//!
//! The avatar is rebuilt from the original file plus this node on every load (import is deterministic), so the node
//! holds only what a person decided: the source, the overrides of what prepare proposed (humanoid map, channel
//! drivers), material edits, the rights acknowledgement, and attachments. Every table refuses unknown keys, so a
//! misspelt override is an error, never a silently ignored line.
//!
//! ```toml
//! schema = 1
//! source = { hash = "…", format = "vrm1", file = "Ava.vrm" }
//! humanoid = { leftHand = { node = 41, name = "J_Bip_L_Hand" } }
//! channels = { cheekPuff = "Puff", custom00 = "happy", tongueOut = false }
//! materials = { body_00 = { shade = [0.8, 0.6, 0.6] } }
//! acknowledged = { perform = "2026-10-13", by = "owner" }
//! ```

use std::collections::BTreeMap;

use omuzaki_anim_api::{Channel, HumanoidBone};
use omuzaki_char_api::{MaterialName, PartDef, PartId};
use omuzaki_core::glam::Vec4;
use omuzaki_io_api::ColorBindKind;
use omuzaki_store_api::ContentHash;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::AvatarError;

/// The node layout this build reads and writes (`schema = 1`).
pub const SCHEMA: u32 = 1;

/// The file format of the original.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SourceFormat {
    /// VRM 1.0.
    Vrm1,
    /// VRM 0.x.
    Vrm0,
    /// Plain glTF 2.0 (`.gltf` or `.glb`).
    Gltf,
}

/// The original file: what the avatar is rebuilt from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct AvatarSource {
    /// Its bytes, unchanged, in the store.
    pub hash: ContentHash,
    /// Its format.
    pub format: SourceFormat,
    /// The file name it was imported from, for people (`Ava.vrm`).
    pub file: String,
}

impl AvatarSource {
    /// The file `file` of `format`, stored under `hash`.
    pub fn new(hash: ContentHash, format: SourceFormat, file: impl Into<String>) -> Self {
        Self { hash, format, file: file.into() }
    }
}

/// A person's choice of node for a humanoid bone (D4): the index is authoritative for the stored source; the name lets
/// a re-import of an updated file find the node again, and say so when it cannot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct NodeChoice {
    /// The node's index in the file.
    pub node: u32,
    /// The node's name in the file.
    pub name: String,
}

impl NodeChoice {
    /// The node at `node`, called `name`.
    pub fn new(node: u32, name: impl Into<String>) -> Self {
        Self { node, name: name.into() }
    }
}

/// A person's choice of what a channel drives (D3, `avatar.map_channel`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriverChoice {
    /// The expression of this name in the file (`"Puff"`, `"happy"`). Written as the name.
    Expression(String),
    /// Nothing: the channel is left inert on this model, whatever prepare proposed. Written as `false`.
    Nothing,
}

impl Serialize for DriverChoice {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Expression(name) => serializer.serialize_str(name),
            Self::Nothing => serializer.serialize_bool(false),
        }
    }
}

impl<'de> Deserialize<'de> for DriverChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Name(String),
            Flag(bool),
        }
        match Written::deserialize(deserializer)? {
            Written::Name(name) if !name.is_empty() => Ok(Self::Expression(name)),
            Written::Flag(false) => Ok(Self::Nothing),
            Written::Name(_) | Written::Flag(true) => Err(serde::de::Error::custom(
                "a channel drives an expression, named by a non-empty string, or nothing, written `false`",
            )),
        }
    }
}

impl JsonSchema for DriverChoice {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DriverChoice".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "The expression a channel drives, by its name in the file, or `false` for nothing.",
            "oneOf": [{ "type": "string", "minLength": 1 }, { "const": false }]
        })
    }
}

/// A person's edits to one material: one field per colour a colour bind can move, linear f32 as the file stores
/// them. A field left out keeps the file's value (D8).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct MaterialEdit {
    /// The base colour factor, linear RGBA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<[f32; 4]>,
    /// The emissive factor, linear RGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emission: Option<[f32; 3]>,
    /// The `MToon` shade colour, linear RGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shade: Option<[f32; 3]>,
    /// The `MToon` matcap colour, linear RGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcap: Option<[f32; 3]>,
    /// The `MToon` rim colour, linear RGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rim: Option<[f32; 3]>,
    /// The `MToon` outline colour, linear RGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<[f32; 3]>,
}

impl MaterialEdit {
    /// No edit.
    pub fn new() -> Self {
        Self::default()
    }

    /// The edited value of the colour a colour bind of `kind` moves, as linear RGBA (alpha 1 for the colours that have
    /// none), or `None` if it is not edited.
    pub fn colour(&self, kind: ColorBindKind) -> Option<Vec4> {
        let rgb = |c: Option<[f32; 3]>| c.map(|[r, g, b]| Vec4::new(r, g, b, 1.0));
        match kind {
            ColorBindKind::Color => self.base.map(Vec4::from_array),
            ColorBindKind::EmissionColor => rgb(self.emission),
            ColorBindKind::ShadeColor => rgb(self.shade),
            ColorBindKind::MatcapColor => rgb(self.matcap),
            ColorBindKind::RimColor => rgb(self.rim),
            ColorBindKind::OutlineColor => rgb(self.outline),
        }
    }

    /// True if nothing is edited.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The confirm step's rights acknowledgement (D5, `rights.avatar_permission`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Acknowledgement {
    /// The date the person acknowledged they may perform as the avatar, `YYYY-MM-DD`.
    pub perform: String,
    /// Who acknowledged it.
    pub by: String,
}

impl Acknowledgement {
    /// Acknowledged on `perform` by `by`.
    pub fn new(perform: impl Into<String>, by: impl Into<String>) -> Self {
        Self { perform: perform.into(), by: by.into() }
    }
}

/// The document node of an imported character.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct AvatarDef {
    /// The layout version, [`SCHEMA`].
    pub schema: u32,
    /// The original file.
    pub source: AvatarSource,
    /// Humanoid map overrides: only the bones a person chose differently from prepare.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub humanoid: BTreeMap<HumanoidBone, NodeChoice>,
    /// Channel driver overrides: only the channels a person mapped differently from prepare.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<Channel, DriverChoice>,
    /// Material edits, by runtime material name, applied field by field over the retained source.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub materials: BTreeMap<MaterialName, MaterialEdit>,
    /// The rights acknowledgement, when the confirm step asked for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged: Option<Acknowledgement>,
    /// Attachments (D1, condition 2), built by the kit against the avatar's anchors.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parts: BTreeMap<PartId, PartDef>,
}

impl AvatarDef {
    /// The node of an avatar imported from `source`, with nothing overridden.
    pub fn new(source: AvatarSource) -> Self {
        Self {
            schema: SCHEMA,
            source,
            humanoid: BTreeMap::new(),
            channels: BTreeMap::new(),
            materials: BTreeMap::new(),
            acknowledged: None,
            parts: BTreeMap::new(),
        }
    }

    /// Checks the layout version, so a node written by a later layout is refused, never misread.
    ///
    /// # Errors
    /// [`AvatarError::UnknownSchema`] when `schema` is not [`SCHEMA`].
    pub fn check_schema(&self) -> Result<(), AvatarError> {
        if self.schema == SCHEMA { Ok(()) } else { Err(AvatarError::unknown_schema(self.schema)) }
    }
}
