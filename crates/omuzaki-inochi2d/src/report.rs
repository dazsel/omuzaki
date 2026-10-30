//! The import report: what was mapped, what was made up, what was left behind, and why.

use serde::{Deserialize, Serialize};

/// Stable rule ids, `inochi2d.<rule>`, for errors and findings. Tests and the UI match on these, never on text.
pub mod rule {
    /// The magic bytes are neither INP1 nor INP2.
    pub const MAGIC: &str = "inochi2d.magic";
    /// A declared length runs past the end of the file.
    pub const TRUNCATED: &str = "inochi2d.truncated";
    /// A container section is repeated or out of shape.
    pub const SECTION: &str = "inochi2d.section";
    /// The INP1 payload is not the JSON an `Inochi2D` puppet is.
    pub const JSON: &str = "inochi2d.json";
    /// Text that should be UTF-8 is not.
    pub const UTF8: &str = "inochi2d.utf8";
    /// The INP2 tagged tree is malformed (a bad tag, alignment, checksum or sentinel).
    pub const INP2: &str = "inochi2d.inp2";
    /// INP2's payload (the `Inochi2D` 0.9 schema) is not mapped yet.
    pub const INP2_PAYLOAD: &str = "inochi2d.inp2_payload";
    /// The node tree has no root.
    pub const TREE: &str = "inochi2d.tree";
    /// A node has the uuid of one of its ancestors: references to it would loop.
    pub const TREE_CYCLE: &str = "inochi2d.tree_cycle";
    /// A mesh is out of shape: odd coordinate lists, uvs that don't match, an index past the vertices.
    pub const MESH: &str = "inochi2d.mesh";
    /// A number that must be finite is not (an overflowing literal).
    pub const NUMBER: &str = "inochi2d.number";
    /// A part names a texture slot the file doesn't have.
    pub const TEXTURE_REF: &str = "inochi2d.texture_ref";
    /// A texture's bytes don't decode.
    pub const TEXTURE_DECODE: &str = "inochi2d.texture_decode";
    /// A mask names a node that doesn't exist.
    pub const MASK_SOURCE: &str = "inochi2d.mask_source";
    /// A parameter's axes are malformed.
    pub const PARAM: &str = "inochi2d.param";
    /// A binding's values don't match its parameter's key grid or its node's points.
    pub const BINDING_VALUES: &str = "inochi2d.binding_values";
    /// The mapped puppet fails the contract's checks (a bug in the mapping, or a case it doesn't guard).
    pub const CHECK: &str = "inochi2d.check";
    /// A [`crate::Limits`] bound was exceeded.
    pub const LIMIT: &str = "inochi2d.limit";

    /// A node type this importer doesn't know: kept as `NodeKind::Unknown`.
    pub const UNKNOWN_NODE: &str = "inochi2d.unknown_node";
    /// `lockToRoot` is set: the node follows the root, skipping its parents; not modelled.
    pub const LOCK_TO_ROOT: &str = "inochi2d.lock_to_root";
    /// A z translation, dropped: draw order comes from `zsort`, and the camera is orthographic.
    pub const TRANSLATE_Z: &str = "inochi2d.translate_z";
    /// An x or y rotation folded into scale; exact only for multiples of pi.
    pub const ROTATION_XY: &str = "inochi2d.rotation_xy";
    /// Two nodes share a uuid; references go to the first in tree order, as `Inochi2D`'s lookup does.
    pub const DUPLICATE_UUID: &str = "inochi2d.duplicate_uuid";
    /// A part with no albedo texture: kept as a mask mesh, which never draws.
    pub const PART_NO_TEXTURE: &str = "inochi2d.part_no_texture";
    /// Emission and bump-map textures and emission strength: not modelled.
    pub const PART_EXTRA_TEXTURES: &str = "inochi2d.part_extra_textures";
    /// An index list that is not whole triangles: `Inochi2D` doesn't draw such a mesh, so its triangles are dropped.
    pub const INDICES: &str = "inochi2d.indices";
    /// A blend mode mapped to the nearest one we have, or an unknown one read as normal.
    pub const BLEND_MODE: &str = "inochi2d.blend_mode";
    /// An unknown mask mode, read as `Mask`.
    pub const MASK_MODE: &str = "inochi2d.mask_mode";
    /// A mask whose source can't mask in our model (a mesh group, a composite, itself): dropped.
    pub const MASK_SOURCE_KIND: &str = "inochi2d.mask_source_kind";
    /// A mesh group with no triangles deforms nothing: kept as a plain group.
    pub const MESHGROUP_EMPTY: &str = "inochi2d.meshgroup_empty";
    /// A mesh group's `translate_children` or `dynamic_deformation`: our deformer carries every point under it.
    pub const MESHGROUP_MODE: &str = "inochi2d.meshgroup_mode";
    /// A parameter whose min equals its max: its key positions are spread over a unit range instead.
    pub const PARAM_RANGE: &str = "inochi2d.param_range";
    /// A one-axis parameter with more than one y key point: only the first is used, as `Inochi2D` does.
    pub const PARAM_AXIS: &str = "inochi2d.param_axis";
    /// A parameter merge mode other than additive: not modelled.
    pub const PARAM_MERGE_MODE: &str = "inochi2d.param_merge_mode";
    /// A binding to a node that doesn't exist: dropped, as `Inochi2D` drops it.
    pub const BINDING_NODE: &str = "inochi2d.binding_node";
    /// A binding target we don't model, or one its node can't take: dropped.
    pub const BINDING_TARGET: &str = "inochi2d.binding_target";
    /// An interpolation mode mapped to the nearest one we have.
    pub const INTERPOLATION: &str = "inochi2d.interpolation";
    /// A binding without `isSet`: every key point is taken as set.
    pub const BINDING_ISSET: &str = "inochi2d.binding_isset";
    /// A physics node whose parameter doesn't exist: no pendulum.
    pub const PHYSICS_PARAM: &str = "inochi2d.physics_param";
    /// A physics node with a non-positive length or frequency: dropped or repaired.
    pub const PHYSICS_INVALID: &str = "inochi2d.physics_invalid";
    /// An unknown physics model or map mode, read as the default.
    pub const PHYSICS_MODE: &str = "inochi2d.physics_mode";
    /// The puppet's physics scale or gravity is unusable: the defaults are used.
    pub const WORLD: &str = "inochi2d.world";
    /// The file states no usage rights. Unknown means unknown: nothing is assumed granted.
    pub const RIGHTS_UNKNOWN: &str = "inochi2d.rights_unknown";
    /// Metadata we keep only in the report (contact, reference, version, thumbnail, pixel filtering).
    pub const META: &str = "inochi2d.meta";
    /// Automation (`Inochi2D`'s parameter drivers other than physics): not modelled.
    pub const AUTOMATION: &str = "inochi2d.automation";
    /// Animations: not modelled.
    pub const ANIMATION: &str = "inochi2d.animation";
    /// `Inochi2D` has no canvas: one is computed from the parts at rest.
    pub const CANVAS: &str = "inochi2d.canvas";
    /// An empty texture slot: a transparent 1x1 texture stands in.
    pub const TEXTURE_EMPTY: &str = "inochi2d.texture_empty";
    /// A BC7 texture: no permissive pure-Rust decoder is in the workspace, so a transparent 1x1 texture stands in.
    pub const TEXTURE_BC7: &str = "inochi2d.texture_bc7";
    /// An unknown texture encoding: a transparent 1x1 texture stands in.
    pub const TEXTURE_ENCODING: &str = "inochi2d.texture_encoding";
    /// The file has no texture section.
    pub const NO_TEXTURES: &str = "inochi2d.no_textures";
    /// Vendor data (`EXT_SECT`): kept, uninterpreted.
    pub const VENDOR_DATA: &str = "inochi2d.vendor_data";
    /// Bytes after the known sections: kept, uninterpreted.
    pub const UNKNOWN_SECTION: &str = "inochi2d.unknown_section";
}

/// What a finding records.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// We made up a value the file doesn't have.
    Synthesised,
    /// Something in the file is not in the puppet.
    Dropped,
    /// Something mapped to the nearest thing we have; the picture may differ.
    Approximated,
    /// Something we can't read at all.
    Unsupported,
    /// Kept, or only worth knowing.
    Note,
}

/// One finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// The rule, `inochi2d.<rule>` (see [`rule`]).
    pub id: String,
    /// What it records.
    pub kind: FindingKind,
    /// Where: a JSON path in the payload (`nodes/children/2/mesh`), or a container section (`TEX_SECT/3`).
    pub path: String,
    /// What happened.
    pub detail: String,
}

/// How many of each thing reached the puppet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mapped {
    /// Nodes, of every kind.
    pub nodes: usize,
    /// Plain nodes (and physics nodes, which become groups).
    pub groups: usize,
    /// Textured parts.
    pub parts: usize,
    /// Mask meshes.
    pub masks: usize,
    /// Composites.
    pub composites: usize,
    /// Mesh groups, as mesh deformers.
    pub mesh_deformers: usize,
    /// Nodes of unknown type.
    pub unknown: usize,
    /// Parameters.
    pub params: usize,
    /// Bindings.
    pub bindings: usize,
    /// Pendulums.
    pub pendulums: usize,
    /// Texture slots.
    pub textures: usize,
}

/// The report of one import.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    /// What reached the puppet.
    pub mapped: Mapped,
    /// Everything synthesised, dropped, approximated or noted, in file order.
    pub findings: Vec<Finding>,
}

impl ImportReport {
    pub(crate) fn add(&mut self, id: &str, kind: FindingKind, path: impl Into<String>, detail: impl Into<String>) {
        self.findings.push(Finding { id: id.to_owned(), kind, path: path.into(), detail: detail.into() });
    }

    /// The findings with rule `id`.
    pub fn with_id<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Finding> + 'a {
        self.findings.iter().filter(move |f| f.id == id)
    }

    /// Whether any finding has rule `id`.
    #[must_use]
    pub fn has(&self, id: &str) -> bool {
        self.with_id(id).next().is_some()
    }

    /// The findings of one kind.
    pub fn of_kind(&self, kind: FindingKind) -> impl Iterator<Item = &Finding> + '_ {
        self.findings.iter().filter(move |f| f.kind == kind)
    }
}
