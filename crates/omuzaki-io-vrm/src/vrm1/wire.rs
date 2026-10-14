//! The JSON shapes of the VRM 1.0 extensions, as serde types: `VRMC_vrm` (`VRMC_vrm-1.0/*.md` and
//! `schema/VRMC_vrm.*.schema.json`), `VRMC_springBone-1.0` and `VRMC_node_constraint-1.0`.
//!
//! One type per JSON object serves reading and writing. Fields the spec gives a default are `Option`s here: the reader
//! applies the default, the writer sets every one explicitly. Names of the same kind (`type`, `source`) follow the
//! schemas. Unknown keys are ignored on read.

use omuzaki_io_api::VrmMeta;
use serde::{Deserialize, Serialize};

use omuzaki_gltf::schema::Extensions;

/// `extensions.VRMC_vrm`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireVrm {
    pub spec_version: String,
    pub meta: VrmMeta,
    pub humanoid: WireHumanoid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_person: Option<WireFirstPerson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_at: Option<WireLookAt>,
    pub expressions: WireExpressions,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireHumanoid {
    pub human_bones: std::collections::BTreeMap<String, WireNodeRef>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireNodeRef {
    pub node: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireFirstPerson {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mesh_annotations: Vec<WireAnnotation>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireAnnotation {
    pub node: usize,
    #[serde(rename = "type")]
    pub kind: WireFirstPersonType,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WireFirstPersonType {
    #[default]
    Auto,
    Both,
    ThirdPersonOnly,
    FirstPersonOnly,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WireLookAtType {
    #[default]
    Bone,
    Expression,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireLookAt {
    #[serde(rename = "type")]
    pub kind: WireLookAtType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_from_head_bone: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_map_horizontal_inner: Option<WireRangeMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_map_horizontal_outer: Option<WireRangeMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_map_vertical_down: Option<WireRangeMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_map_vertical_up: Option<WireRangeMap>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireRangeMap {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_max_value: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_scale: Option<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireExpressions {
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub preset: std::collections::BTreeMap<String, WireExpression>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub custom: std::collections::BTreeMap<String, WireExpression>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WireOverride {
    #[default]
    None,
    Block,
    Blend,
}

/// An expression. The bind arrays are omitted when empty because the schema requires at least one entry when the
/// array is present.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireExpression {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub morph_target_binds: Vec<WireMorphBind>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub material_color_binds: Vec<WireColorBind>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub texture_transform_binds: Vec<WireUvBind>,
    pub is_binary: bool,
    pub override_blink: WireOverride,
    pub override_look_at: WireOverride,
    pub override_mouth: WireOverride,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireMorphBind {
    pub node: usize,
    pub index: usize,
    pub weight: f32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WireColorType {
    #[default]
    Color,
    EmissionColor,
    ShadeColor,
    MatcapColor,
    RimColor,
    OutlineColor,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireColorBind {
    pub material: usize,
    #[serde(rename = "type")]
    pub kind: WireColorType,
    pub target_value: [f32; 4],
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireUvBind {
    pub material: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<[f32; 2]>,
}

// ---------------------------------------------------------------------------------------------------------------
// VRMC_springBone

/// `extensions.VRMC_springBone`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireSpringBone {
    pub spec_version: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub colliders: Vec<WireCollider>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub collider_groups: Vec<WireColliderGroup>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub springs: Vec<WireSpring>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireCollider {
    pub node: usize,
    /// Absent when the collider is described only by `VRMC_springBone_extended_collider`, which this crate drops.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<WireShape>,
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireShape {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sphere: Option<WireSphere>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capsule: Option<WireCapsule>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireSphere {
    pub offset: [f32; 3],
    pub radius: f32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireCapsule {
    pub offset: [f32; 3],
    pub radius: f32,
    pub tail: [f32; 3],
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireColliderGroup {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub colliders: Vec<usize>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireSpring {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub joints: Vec<WireJoint>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub collider_groups: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<usize>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireJoint {
    pub node: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stiffness: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gravity_power: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gravity_dir: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drag_force: Option<f32>,
    /// `VRMC_springBone_limit` lives here; this crate drops it and says so.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
}

// ---------------------------------------------------------------------------------------------------------------
// VRMC_node_constraint

/// `nodes[*].extensions.VRMC_node_constraint`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireNodeConstraint {
    pub spec_version: String,
    pub constraint: WireConstraint,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireConstraint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roll: Option<WireRoll>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim: Option<WireAim>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<WireRotation>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum WireRollAxis {
    #[default]
    X,
    Y,
    Z,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum WireAimAxis {
    #[default]
    PositiveX,
    NegativeX,
    PositiveY,
    NegativeY,
    PositiveZ,
    NegativeZ,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireRoll {
    pub source: usize,
    pub roll_axis: WireRollAxis,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct WireAim {
    pub source: usize,
    pub aim_axis: WireAimAxis,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WireRotation {
    pub source: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
}
