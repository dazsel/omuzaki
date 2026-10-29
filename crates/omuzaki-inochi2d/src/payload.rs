//! INP1's JSON payload, as `Inochi2D` 0.8 writes it (`core/puppet.d`, `core/nodes/**`, `core/param/**`).
//!
//! The tree is parsed with every node's fields kept as raw JSON text ([`RawValue`]), and only the fields a node's
//! type uses are parsed further, so a node of a type we don't know never fails the file, whatever it carries. The raw
//! text borrows the input; nothing is copied until a field is read.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;

use crate::error::{ImportError, malformed};
use crate::report::rule;

/// The payload's top level.
#[derive(Deserialize)]
pub(crate) struct JPuppet<'a> {
    #[serde(borrow, default)]
    pub meta: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub physics: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub nodes: Option<JNode<'a>>,
    #[serde(borrow, default)]
    pub param: Option<Vec<JParam<'a>>>,
    #[serde(borrow, default)]
    pub automation: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub animations: Option<&'a RawValue>,
}

/// One node: the fields every node has, then the fields of each known type, all still raw.
#[derive(Deserialize)]
#[allow(non_snake_case, reason = "the fields are named as Inochi2D names its keys")]
pub(crate) struct JNode<'a> {
    #[serde(borrow, default)]
    pub uuid: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub name: Option<&'a RawValue>,
    #[serde(borrow, default, rename = "type")]
    pub ty: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub enabled: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub zsort: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub transform: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub lockToRoot: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub children: Option<Vec<JNode<'a>>>,

    // Drawables (Part, Mask, MeshGroup).
    #[serde(borrow, default)]
    pub mesh: Option<&'a RawValue>,
    // Part.
    #[serde(borrow, default)]
    pub textures: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub emissionStrength: Option<&'a RawValue>,
    // Part and Composite.
    #[serde(borrow, default)]
    pub blend_mode: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub tint: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub screenTint: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub masks: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub masked_by: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub mask_mode: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub mask_threshold: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub opacity: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub propagate_meshgroup: Option<&'a RawValue>,
    // MeshGroup.
    #[serde(borrow, default)]
    pub dynamic_deformation: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub translate_children: Option<&'a RawValue>,
    // SimplePhysics.
    #[serde(borrow, default)]
    pub param: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub model_type: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub map_mode: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub gravity: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub length: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub frequency: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub angle_damping: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub length_damping: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub output_scale: Option<&'a RawValue>,
    #[serde(borrow, default)]
    pub local_only: Option<&'a RawValue>,
}

/// A transform: `trans` (x, y, z), `rot` (Euler x, y, z, radians), `scale` (x, y).
#[derive(Deserialize, Default)]
pub(crate) struct JTransform {
    #[serde(default)]
    pub trans: Vec<f32>,
    #[serde(default)]
    pub rot: Vec<f32>,
    #[serde(default)]
    pub scale: Vec<f32>,
}

/// A drawable's mesh: flat coordinate lists.
#[derive(Deserialize)]
pub(crate) struct JMesh {
    #[serde(default)]
    pub verts: Vec<f32>,
    #[serde(default)]
    pub uvs: Option<Vec<f32>>,
    #[serde(default)]
    pub indices: Vec<u32>,
    #[serde(default)]
    pub origin: Vec<f32>,
}

/// One mask binding.
#[derive(Deserialize)]
pub(crate) struct JMask {
    pub source: Option<u32>,
    #[serde(default)]
    pub mode: Option<Named>,
}

/// An enum value, which `Inochi2D` writes by name (and older writers by number).
#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
pub(crate) enum Named {
    Name(String),
    Number(i64),
}

impl Named {
    /// The value as text, for reports.
    pub fn text(&self) -> String {
        match self {
            Self::Name(s) => s.clone(),
            Self::Number(n) => n.to_string(),
        }
    }
}

/// One parameter.
#[derive(Deserialize)]
pub(crate) struct JParam<'a> {
    #[serde(default)]
    pub uuid: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_vec2: Option<bool>,
    #[serde(default)]
    pub min: Option<Vec<f32>>,
    #[serde(default)]
    pub max: Option<Vec<f32>>,
    #[serde(default)]
    pub defaults: Option<Vec<f32>>,
    #[serde(default)]
    pub axis_points: Option<Vec<Vec<f32>>>,
    #[serde(default)]
    pub merge_mode: Option<Named>,
    #[serde(borrow, default)]
    pub bindings: Option<Vec<JBinding<'a>>>,
}

/// One binding, its values still raw (their shape depends on the target).
#[derive(Deserialize)]
#[allow(non_snake_case, reason = "the fields are named as Inochi2D names its keys")]
pub(crate) struct JBinding<'a> {
    #[serde(default)]
    pub node: Option<u32>,
    #[serde(default)]
    pub param_name: Option<String>,
    #[serde(borrow, default)]
    pub values: Option<&'a RawValue>,
    #[serde(default)]
    pub isSet: Option<Vec<Vec<bool>>>,
    #[serde(default)]
    pub interpolate_mode: Option<Named>,
}

/// The puppet's physics settings.
#[derive(Deserialize)]
#[allow(non_snake_case, reason = "the fields are named as Inochi2D names its keys")]
pub(crate) struct JPhysics {
    #[serde(default)]
    pub pixelsPerMeter: Option<f32>,
    #[serde(default)]
    pub gravity: Option<f32>,
}

/// The puppet's metadata. Absent and `null` are both unknown.
#[derive(Deserialize)]
#[allow(non_snake_case, reason = "the fields are named as Inochi2D names its keys")]
pub(crate) struct JMeta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub rigger: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub rights: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub copyright: Option<String>,
    #[serde(default)]
    pub licenseURL: Option<String>,
    #[serde(default)]
    pub contact: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub thumbnailId: Option<u64>,
    #[serde(default)]
    pub preservePixels: Option<bool>,
}

/// Parses the payload's top level.
pub(crate) fn parse_puppet(json: &[u8]) -> Result<JPuppet<'_>, ImportError> {
    serde_json::from_slice(json).map_err(|e| malformed(rule::JSON, "", e.to_string()))
}

/// Parses a raw field as `T`; a wrong shape is malformed at `path`.
pub(crate) fn field<T: DeserializeOwned>(raw: Option<&RawValue>, path: &str) -> Result<Option<T>, ImportError> {
    raw.map(|raw| serde_json::from_str::<Option<T>>(raw.get()).map_err(|e| malformed(rule::JSON, path, e.to_string())))
        .transpose()
        .map(Option::flatten)
}
