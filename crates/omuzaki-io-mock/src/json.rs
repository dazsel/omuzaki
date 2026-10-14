//! The slice of the glTF 2.0 JSON schema the mock reads and writes, as typed serde structs. Field names follow the
//! glTF 2.0 specification (`KhronosGroup/glTF`, `specification/2.0`), converted to camelCase by serde.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// glTF component type constants (spec, "Accessor Data Types").
pub(crate) mod component {
    pub(crate) const U8: u32 = 5121;
    pub(crate) const U16: u32 = 5123;
    pub(crate) const U32: u32 = 5125;
    pub(crate) const F32: u32 = 5126;
}

/// `bufferView.target` values.
pub(crate) const ARRAY_BUFFER: u32 = 34962;
pub(crate) const ELEMENT_ARRAY_BUFFER: u32 = 34963;

/// The glTF root object.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Gltf {
    pub asset: Asset,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scenes: Vec<GScene>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<GNode>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub meshes: Vec<GMesh>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skins: Vec<GSkin>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub materials: Vec<GMaterial>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub accessors: Vec<Accessor>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub buffer_views: Vec<BufferView>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub buffers: Vec<Buffer>,
    /// Read only: the mock drops images and says so.
    #[serde(skip_serializing)]
    pub images: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_used: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_required: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Asset {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GScene {
    pub nodes: Vec<usize>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GNode {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skin: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 3]>,
    /// Read only, to refuse it: the mock doesn't decompose matrices.
    #[serde(skip_serializing)]
    pub matrix: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GMesh {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub primitives: Vec<GPrimitive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<MeshExtras>,
}

/// The de-facto morph target name convention (glTF 2.0, Morph Targets, implementation note).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct MeshExtras {
    pub target_names: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GPrimitive {
    pub attributes: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indices: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<BTreeMap<String, usize>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct GSkin {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub joints: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inverse_bind_matrices: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<usize>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct GMaterial {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub pbr_metallic_roughness: Pbr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive_factor: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha_cutoff: Option<f32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub double_sided: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

// The `*_factor` names are the glTF spec's.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Pbr {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_color_factor: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metallic_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roughness_factor: Option<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Accessor {
    pub buffer_view: Option<usize>,
    #[serde(skip_serializing_if = "is_zero")]
    pub byte_offset: usize,
    pub component_type: u32,
    pub count: usize,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<Vec<f32>>,
    /// Read only, to refuse it.
    #[serde(skip_serializing)]
    pub sparse: Option<Value>,
}

// serde's `skip_serializing_if` hands over a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct BufferView {
    pub buffer: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub byte_offset: usize,
    pub byte_length: usize,
    /// Read only, to refuse interleaved data.
    #[serde(skip_serializing)]
    pub byte_stride: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<u32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Buffer {
    pub byte_length: usize,
    #[serde(skip_serializing)]
    pub uri: Option<String>,
}
