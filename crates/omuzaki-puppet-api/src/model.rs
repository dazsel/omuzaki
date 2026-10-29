//! The puppet: its node tree, the meshes and textures of its parts, and the facts about who made it.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::param::Param;
use crate::physics::{Pendulum, PhysicsWorld};

/// A node's position in [`PuppetDef::nodes`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct NodeIx(pub u32);

impl NodeIx {
    /// The index as a `usize`.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// A texture's position in [`PuppetDef::textures`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct TexIx(pub u32);

impl TexIx {
    /// The index as a `usize`.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// A flat 2D puppet: everything but the texture pixels, which travel beside it (named by [`TextureRef::file`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PuppetDef {
    /// Who made it, and what they allow.
    #[serde(default)]
    pub meta: PuppetMeta,
    /// The art's canvas in model units (pixels): the box a camera frames to show the whole puppet.
    pub canvas: [f32; 2],
    /// The textures, in the order parts name them.
    pub textures: Vec<TextureRef>,
    /// The nodes, every parent before its children; index 0 is the root.
    pub nodes: Vec<Node>,
    /// The parameters, in the order the evaluator applies them.
    #[serde(default)]
    pub params: Vec<Param>,
    /// The pendulums that write parameters each physics step.
    #[serde(default)]
    pub physics: Vec<Pendulum>,
    /// Gravity and scale for the pendulums.
    #[serde(default)]
    pub world: PhysicsWorld,
}

impl PuppetDef {
    /// The node called `name`, the first one in tree order.
    #[must_use]
    pub fn node(&self, name: &str) -> Option<NodeIx> {
        self.nodes.iter().position(|n| n.name == name).and_then(|i| u32::try_from(i).ok()).map(NodeIx)
    }

    /// The parameter called `name`.
    #[must_use]
    pub fn param(&self, name: &str) -> Option<crate::param::ParamIx> {
        self.params.iter().position(|p| p.name == name).and_then(|i| u32::try_from(i).ok()).map(crate::param::ParamIx)
    }

    /// The children of `node`, in tree order.
    pub fn children(&self, node: NodeIx) -> impl Iterator<Item = NodeIx> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .filter(move |(_, n)| n.parent == Some(node))
            .filter_map(|(i, _)| u32::try_from(i).ok().map(NodeIx))
    }
}

/// Who made a puppet, kept verbatim from its source; unknown stays unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PuppetMeta {
    /// The puppet's name.
    #[serde(default)]
    pub name: String,
    /// The artist, if the source says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// The rigger, if the source says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rigger: Option<String>,
    /// The copyright line, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
    /// Where the licence is, if the source says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence_url: Option<String>,
    /// The source's usage-rights fields, verbatim (`allow_commercial = "true"`, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rights: BTreeMap<String, String>,
    /// What made this puppet: `"inochi2d"`, `"layers"`, `"builder"`.
    #[serde(default)]
    pub source: String,
}

/// A texture a part samples: premultiplied-alpha RGBA, sRGB colour.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TextureRef {
    /// Its file beside the puppet (`textures/0.png`), or its name in the store.
    pub file: String,
    /// Its size in pixels.
    pub size: [u32; 2],
}

/// A node's own transform, relative to its parent: scale, then rotation, then translation.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Transform2 {
    /// Translation, model units.
    #[serde(default)]
    pub translation: [f32; 2],
    /// Rotation in the plane, radians (clockwise on screen).
    #[serde(default)]
    pub rotation: f32,
    /// Scale.
    #[serde(default = "one2")]
    pub scale: [f32; 2],
}

const fn one2() -> [f32; 2] {
    [1.0, 1.0]
}

impl Default for Transform2 {
    fn default() -> Self {
        Self { translation: [0.0, 0.0], rotation: 0.0, scale: one2() }
    }
}

/// One node of the tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Node {
    /// Its name (unique names make mappings and ops readable, but are not required).
    pub name: String,
    /// Its parent; `None` only for the root.
    pub parent: Option<NodeIx>,
    /// Whether it and its subtree draw.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Its transform relative to the parent.
    #[serde(default)]
    pub transform: Transform2,
    /// Its draw-order offset, added to the parent's: a larger total draws further back.
    #[serde(default)]
    pub zsort: f32,
    /// What it is.
    pub kind: NodeKind,
}

const fn yes() -> bool {
    true
}

/// What a node is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum NodeKind {
    /// A plain node: it only carries a transform (a rotation pivot, a group).
    Group,
    /// A textured mesh: one layer of the art.
    Part(Part),
    /// A mesh drawn only as a mask source, never on its own.
    Mask(Mesh),
    /// A group drawn into its own layer, then blended as one (group opacity, a blend mode over the whole group).
    Composite(Composite),
    /// A regular grid that warps everything under it: move its points and the children follow, smoothly.
    Lattice(Lattice),
    /// A triangle mesh that warps everything under it (the `MeshGroup` of `Inochi2D`).
    MeshDeformer(Mesh),
    /// A node of a type this version doesn't know, kept so the tree's shape survives; it acts as a [`NodeKind::Group`].
    Unknown {
        /// The source's type name.
        type_name: String,
    },
}

impl NodeKind {
    /// The deformable points of this node, if a `deform` binding can move them.
    #[must_use]
    pub fn deform_points(&self) -> Option<usize> {
        match self {
            Self::Part(part) => Some(part.mesh.vertices.len()),
            Self::Mask(mesh) | Self::MeshDeformer(mesh) => Some(mesh.vertices.len()),
            Self::Lattice(lattice) => Some(lattice.points()),
            Self::Group | Self::Composite(_) | Self::Unknown { .. } => None,
        }
    }
}

/// A triangle mesh in its node's local space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Mesh {
    /// Vertex positions, model units, relative to the node.
    pub vertices: Vec<[f32; 2]>,
    /// Texture coordinates, `0..1` across the texture, y down (empty for a deformer or a mask).
    #[serde(default)]
    pub uvs: Vec<[f32; 2]>,
    /// Triangles, as vertex indices.
    pub triangles: Vec<[u32; 3]>,
}

/// How a layer combines with what is under it. Formulas: `docs/design/puppet2d.md` and `omuzaki_puppet::blend`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlendMode {
    /// Source over.
    #[default]
    Normal,
    /// Darkens: the colours multiply.
    Multiply,
    /// Lightens: the inverses multiply.
    Screen,
    /// Multiply under half grey, screen above it (by the lower layer).
    Overlay,
    /// The darker of the two, per channel.
    Darken,
    /// The lighter of the two, per channel.
    Lighten,
    /// Brightens the lower layer by dividing by the inverse of the upper.
    ColorDodge,
    /// Adds the upper layer (glows, highlights).
    LinearDodge,
    /// Darkens the lower layer by dividing its inverse by the upper.
    ColorBurn,
    /// Overlay with the layers swapped.
    HardLight,
    /// A gentle hard light.
    SoftLight,
    /// The absolute difference.
    Difference,
    /// A softer difference.
    Exclusion,
    /// Subtracts the upper layer.
    Subtract,
    /// Draws only where the lower layer is, keeping its alpha (a clipping layer).
    ClipToLower,
    /// Cuts the upper layer's shape out of the lower one.
    SliceFromLower,
}

/// How a mask source shapes a drawable.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaskMode {
    /// Draw only inside the source.
    #[default]
    Mask,
    /// Draw only outside the source.
    Dodge,
}

/// One mask on a drawable.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaskRef {
    /// The node whose shape masks (a part, by its alpha, or a mask mesh).
    pub source: NodeIx,
    /// Inside or outside.
    #[serde(default)]
    pub mode: MaskMode,
}

/// How a part or composite draws.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Drawable {
    /// How it blends.
    #[serde(default)]
    pub blend: BlendMode,
    /// Its opacity, `0..1`.
    #[serde(default = "one")]
    pub opacity: f32,
    /// A multiplied tint, linear `0..1` per channel.
    #[serde(default = "white")]
    pub tint: [f32; 3],
    /// A screened tint (lifts the colour toward it).
    #[serde(default)]
    pub screen_tint: [f32; 3],
    /// Its masks: with several, a pixel draws where any `Mask` source covers and no `Dodge` source does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<MaskRef>,
    /// The source alpha above which a mask counts as covering.
    #[serde(default = "half")]
    pub mask_threshold: f32,
}

const fn one() -> f32 {
    1.0
}
const fn half() -> f32 {
    0.5
}
const fn white() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

impl Default for Drawable {
    fn default() -> Self {
        Self {
            blend: BlendMode::Normal,
            opacity: 1.0,
            tint: white(),
            screen_tint: [0.0; 3],
            masks: Vec::new(),
            mask_threshold: half(),
        }
    }
}

/// A textured mesh.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Part {
    /// The mesh.
    pub mesh: Mesh,
    /// Its texture.
    pub texture: TexIx,
    /// How it draws.
    #[serde(default)]
    pub draw: Drawable,
}

/// A group drawn as one layer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Composite {
    /// How the group's layer draws.
    #[serde(default)]
    pub draw: Drawable,
}

/// A regular grid deformer, in its node's local space. Its points are row-major: point `(col, row)` is at index
/// `row * cols + col`, at rest at `origin + size * (col / (cols - 1), row / (rows - 1))`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Lattice {
    /// Points across (at least 2).
    pub cols: u32,
    /// Points down (at least 2).
    pub rows: u32,
    /// The top-left point at rest.
    pub origin: [f32; 2],
    /// The grid's extent at rest.
    pub size: [f32; 2],
}

impl Lattice {
    /// How many points the grid has.
    #[must_use]
    pub const fn points(&self) -> usize {
        (self.cols as usize) * (self.rows as usize)
    }

    /// Point `(col, row)` at rest.
    #[must_use]
    #[allow(clippy::cast_precision_loss, reason = "lattices are at most a few dozen points across")]
    pub fn rest(&self, col: u32, row: u32) -> [f32; 2] {
        let fx = col as f32 / (self.cols.max(2) - 1) as f32;
        let fy = row as f32 / (self.rows.max(2) - 1) as f32;
        [self.origin[0] + self.size[0] * fx, self.origin[1] + self.size[1] * fy]
    }
}
