//! The JSON shapes of the VRM 0.x `VRM` extension (`vrm-specification/specification/0.0/README.md` and its
//! `schema/*.json`), as serde types. Indices are `i64` because 0.x uses `-1` for "none"; vectors are the 0.x
//! `{x, y, z}` objects. Unknown keys are ignored on read.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `extensions.VRM`. The meta is kept as raw JSON so it can be cleaned before it is parsed (see `meta`).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireVrm0 {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exporter_version: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub spec_version: String,
    pub meta: Value,
    pub humanoid: WireHumanoid0,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_person: Option<WireFirstPerson0>,
    pub blend_shape_master: WireBlendShapeMaster,
    pub secondary_animation: WireSecondary,
    pub material_properties: Vec<WireMaterial0>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireHumanoid0 {
    pub human_bones: Vec<WireHumanBone0>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arm_stretch: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leg_stretch: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upper_arm_twist: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lower_arm_twist: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upper_leg_twist: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lower_leg_twist: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feet_spacing: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "hasTranslationDoF")]
    pub has_translation_dof: Option<bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireHumanBone0 {
    pub bone: String,
    pub node: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_default_values: Option<bool>,
}

/// An `{x, y, z}` vector as 0.x writes them.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireVec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl WireVec3 {
    pub(crate) fn from_array(a: [f32; 3]) -> Self {
        Self { x: a[0], y: a[1], z: a[2] }
    }

    pub(crate) fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireFirstPerson0 {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_person_bone: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_person_bone_offset: Option<WireVec3>,
    pub mesh_annotations: Vec<WireAnnotation0>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_at_type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_at_horizontal_inner: Option<WireDegreeMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_at_horizontal_outer: Option<WireDegreeMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_at_vertical_down: Option<WireDegreeMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_at_vertical_up: Option<WireDegreeMap>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireAnnotation0 {
    pub mesh: i64,
    pub first_person_flag: String,
}

/// A look-at angle map: a Unity animation curve (kept only so a written file has one) and its input and output ranges.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireDegreeMap {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curve: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_range: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y_range: Option<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireBlendShapeMaster {
    pub blend_shape_groups: Vec<WireGroup>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireGroup {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_name: Option<String>,
    pub binds: Vec<WireBind0>,
    pub material_values: Vec<WireMaterialValue>,
    pub is_binary: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireBind0 {
    pub mesh: i64,
    pub index: i64,
    pub weight: f32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireMaterialValue {
    pub material_name: String,
    pub property_name: String,
    pub target_value: Vec<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireSecondary {
    pub bone_groups: Vec<WireBoneGroup>,
    pub collider_groups: Vec<WireColliderGroup0>,
}

/// A spring bone group. `stiffiness` is the 0.x spec's own spelling.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireBoneGroup {
    pub comment: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stiffiness: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gravity_power: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gravity_dir: Option<WireVec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drag_force: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_radius: Option<f32>,
    pub bones: Vec<i64>,
    pub collider_groups: Vec<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireColliderGroup0 {
    pub node: i64,
    pub colliders: Vec<WireSphere0>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireSphere0 {
    pub offset: WireVec3,
    pub radius: f32,
}

/// One entry of `materialProperties`: a Unity shader name and its properties.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireMaterial0 {
    pub name: String,
    pub shader: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_queue: Option<i64>,
    pub float_properties: BTreeMap<String, f32>,
    pub vector_properties: BTreeMap<String, Vec<f32>>,
    pub texture_properties: BTreeMap<String, i64>,
    pub keyword_map: BTreeMap<String, bool>,
    pub tag_map: BTreeMap<String, String>,
}
