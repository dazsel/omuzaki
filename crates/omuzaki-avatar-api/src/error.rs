//! Why an imported avatar, or its document node, is not valid.
//!
//! Errors are values (ADR 0054 §4): each struct-like variant is `#[non_exhaustive]` and has a constructor, so a field
//! can be added without breaking a crate that builds or matches one.

use omuzaki_anim_api::{Channel, ConstraintError, SpringError};
use omuzaki_char_api::{JointRef, MaterialName, TextureName};
use omuzaki_core::mesh::MeshError;
use omuzaki_render_api::RenderError;

use crate::expression::ExprIx;

/// The first broken rule of an [`ImportedAvatar`](crate::avatar::ImportedAvatar) or an
/// [`AvatarDef`](crate::def::AvatarDef).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum AvatarError {
    /// A mesh or an anchor follows a bone the skeleton does not have.
    #[error("{at} follows bone {joint:?}, which the skeleton does not have")]
    #[non_exhaustive]
    UnresolvedBone {
        /// What refers to it: `mesh 3` or `anchor head-top`.
        at: String,
        /// The bone it names.
        joint: JointRef,
    },
    /// A mesh's inverse bind matrices do not line up with its joint list.
    #[error("mesh {mesh} has {inverse_bind} inverse bind matrices for {joint_bones} joints")]
    #[non_exhaustive]
    InverseBindLength {
        /// The mesh index.
        mesh: usize,
        /// How many matrices it has.
        inverse_bind: usize,
        /// How many joints it lists.
        joint_bones: usize,
    },
    /// A morph bind names a mesh that does not exist, or a target that mesh does not have.
    #[error("expression {expression} moves target {target} of mesh {mesh}, which does not exist")]
    #[non_exhaustive]
    MorphOutOfRange {
        /// The expression index.
        expression: usize,
        /// The mesh it names.
        mesh: u16,
        /// The target it names.
        target: u16,
    },
    /// A channel drives an expression that does not exist.
    #[error("channel {channel} drives expression {expression}, but the avatar has {len} expressions")]
    #[non_exhaustive]
    DriverOutOfRange {
        /// The channel.
        channel: Channel,
        /// The expression it names.
        expression: u16,
        /// How many expressions there are.
        len: usize,
    },
    /// An `eyeLook*` channel drives an expression although the gaze route is not native `ARKit`: the route would
    /// reach that direction twice.
    #[error("channel {channel} drives an expression, but only the native ARKit gaze route drives gaze expressions")]
    #[non_exhaustive]
    GazeDriverOffRoute {
        /// The channel.
        channel: Channel,
    },
    /// A mesh, colour bind or UV bind names a material the avatar does not have.
    #[error("{at} uses material '{material}', which the avatar does not have")]
    #[non_exhaustive]
    UnknownMaterial {
        /// What refers to it: `mesh 2` or `expression 4`.
        at: String,
        /// The material it names.
        material: MaterialName,
    },
    /// More meshes or expressions than a `u16` index can name.
    #[error("the avatar has {count} {what}; at most {} can be indexed", crate::avatar::MAX_INDEXED)]
    #[non_exhaustive]
    TooMany {
        /// `meshes` or `expressions`.
        what: &'static str,
        /// How many it has.
        count: usize,
    },
    /// A mesh's geometry or skin breaks a mesh rule.
    #[error("mesh {mesh}: {why}")]
    #[non_exhaustive]
    Mesh {
        /// The mesh index.
        mesh: usize,
        /// The broken rule.
        why: MeshError,
    },
    /// A mesh's morph targets do not fit its geometry: an index past its vertices, or deltas that don't line up.
    #[error("mesh {mesh}'s morph targets: {why}")]
    #[non_exhaustive]
    Morphs {
        /// The mesh index.
        mesh: usize,
        /// The renderer's own morph rule that is broken.
        why: RenderError,
    },
    /// A material breaks a renderer rule (a range, a non-finite number), so upload would refuse it.
    #[error("material '{material}': {why}")]
    #[non_exhaustive]
    Material {
        /// The material.
        material: MaterialName,
        /// The renderer rule it breaks.
        why: RenderError,
    },
    /// A material draws a texture the avatar does not have.
    #[error("material '{material}' draws texture '{texture}', which the avatar does not have")]
    #[non_exhaustive]
    UnknownTexture {
        /// The material.
        material: MaterialName,
        /// The texture it names.
        texture: TextureName,
    },
    /// A material composites face layers, which only a built character's painted face provides.
    #[error("material '{material}' composites face layers, but an imported avatar has no painted face")]
    #[non_exhaustive]
    FaceOverlay {
        /// The material.
        material: MaterialName,
    },
    /// A texture's pixels don't match its description.
    #[error("texture '{texture}': {why}")]
    #[non_exhaustive]
    Texture {
        /// The texture.
        texture: TextureName,
        /// What is wrong.
        why: String,
    },
    /// A retained source mesh's morph-target names don't match the targets of a runtime mesh made from it.
    #[error("source mesh {source_mesh} names {names} morph targets, but a mesh made from it has {targets}")]
    #[non_exhaustive]
    MorphNames {
        /// The source mesh, an index into `Retained::meshes`.
        source_mesh: usize,
        /// How many names it keeps.
        names: usize,
        /// How many targets the runtime mesh has.
        targets: usize,
    },
    /// An instance of a retained source mesh lists a runtime mesh the avatar does not have.
    #[error("source mesh {source_mesh} lists mesh {mesh}, which the avatar does not have")]
    #[non_exhaustive]
    UnknownPrimitive {
        /// The source mesh, an index into `Retained::meshes`.
        source_mesh: usize,
        /// The runtime mesh it lists.
        mesh: usize,
    },
    /// A runtime mesh is listed by no instance of a retained source mesh, or by more than one.
    #[error("mesh {mesh} is listed by {groups} source-mesh instances; every mesh comes from exactly one")]
    #[non_exhaustive]
    MeshGrouping {
        /// The runtime mesh.
        mesh: usize,
        /// How many instances list it.
        groups: usize,
    },
    /// An instance of a retained source mesh does not list one runtime mesh per primitive.
    #[error("source mesh {source_mesh}'s instance at node {node} lists {meshes} meshes for {primitives} primitives")]
    #[non_exhaustive]
    InstancePrimitives {
        /// The source mesh, an index into `Retained::meshes`.
        source_mesh: usize,
        /// The drawing node, an index into `Retained::nodes`.
        node: u32,
        /// How many runtime meshes the instance lists.
        meshes: usize,
        /// How many primitives the source mesh has.
        primitives: usize,
    },
    /// A retained source mesh lists an instance at a node that does not draw it.
    #[error("source mesh {source_mesh} lists an instance at node {node}, which does not draw it")]
    #[non_exhaustive]
    InstanceNode {
        /// The source mesh, an index into `Retained::meshes`.
        source_mesh: usize,
        /// The node, an index into `Retained::nodes`.
        node: u32,
    },
    /// A node draws a retained source mesh that lists it as an instance other than once.
    #[error(
        "node {node} draws source mesh {source_mesh}, which lists it {instances} times; it lists each drawing node once"
    )]
    #[non_exhaustive]
    NodeInstances {
        /// The node, an index into `Retained::nodes`.
        node: usize,
        /// The source mesh it draws.
        source_mesh: usize,
        /// How many instances at that node the source mesh lists.
        instances: usize,
    },
    /// A morph bind names a source node whose instance does not include the runtime mesh it moves.
    #[error(
        "expression {expression} moves mesh {mesh} and names node {node}, whose instance does not include that mesh"
    )]
    #[non_exhaustive]
    MorphBindInstance {
        /// The expression index.
        expression: usize,
        /// The runtime mesh the bind moves.
        mesh: u16,
        /// The source node it names.
        node: u32,
    },
    /// A retained spring record names a runtime chain or group the spring set does not have.
    #[error("a retained {kind} became the spring set's {kind} '{name}', which the set does not have")]
    #[non_exhaustive]
    UnknownRuntimeSpring {
        /// `chain` or `collider group`.
        kind: &'static str,
        /// The runtime name it gives.
        name: String,
    },
    /// Two retained spring records name the same runtime chain or group: each file record becomes its own.
    #[error("two retained {kind}s became the spring set's {kind} '{name}'; each becomes its own")]
    #[non_exhaustive]
    SharedRuntimeSpring {
        /// `chain` or `collider group`.
        kind: &'static str,
        /// The runtime name they share.
        name: String,
    },
    /// A retained record indexes past the list it points into.
    #[error("{at} is {index}, but there are only {len}")]
    #[non_exhaustive]
    RetainedIndex {
        /// What holds the index: `node 3's child`, `skin 0's joint`.
        at: String,
        /// The index it holds.
        index: usize,
        /// How long the list is.
        len: usize,
    },
    /// A retained skin's inverse bind matrices don't line up with its joints.
    #[error("skin {skin} has {inverse_bind} inverse bind matrices for {joints} joints")]
    #[non_exhaustive]
    SkinInverseBind {
        /// The skin, an index into `Retained::skins`.
        skin: usize,
        /// How many matrices it has.
        inverse_bind: usize,
        /// How many joints it has.
        joints: usize,
    },
    /// The spring set does not fit the skeleton.
    #[error("springs: {0}")]
    Springs(SpringError),
    /// The constraint set does not fit the skeleton.
    #[error("constraints: {0}")]
    Constraints(ConstraintError),
    /// The avatar node was written by a layout this build doesn't read.
    #[error("the avatar node has schema {found}; this build reads schema {}", crate::def::SCHEMA)]
    #[non_exhaustive]
    UnknownSchema {
        /// The schema it says.
        found: u32,
    },
}

impl AvatarError {
    /// [`AvatarError::UnresolvedBone`].
    pub fn unresolved_bone(at: impl Into<String>, joint: JointRef) -> Self {
        Self::UnresolvedBone { at: at.into(), joint }
    }

    /// [`AvatarError::InverseBindLength`].
    pub const fn inverse_bind_length(mesh: usize, inverse_bind: usize, joint_bones: usize) -> Self {
        Self::InverseBindLength { mesh, inverse_bind, joint_bones }
    }

    /// [`AvatarError::MorphOutOfRange`].
    pub const fn morph_out_of_range(expression: usize, mesh: u16, target: u16) -> Self {
        Self::MorphOutOfRange { expression, mesh, target }
    }

    /// [`AvatarError::DriverOutOfRange`].
    pub const fn driver_out_of_range(channel: Channel, expression: ExprIx, len: usize) -> Self {
        Self::DriverOutOfRange { channel, expression: expression.0, len }
    }

    /// [`AvatarError::GazeDriverOffRoute`].
    pub const fn gaze_driver_off_route(channel: Channel) -> Self {
        Self::GazeDriverOffRoute { channel }
    }

    /// [`AvatarError::UnknownMaterial`].
    pub fn unknown_material(at: impl Into<String>, material: MaterialName) -> Self {
        Self::UnknownMaterial { at: at.into(), material }
    }

    /// [`AvatarError::TooMany`].
    pub const fn too_many(what: &'static str, count: usize) -> Self {
        Self::TooMany { what, count }
    }

    /// [`AvatarError::Mesh`].
    pub const fn mesh(mesh: usize, why: MeshError) -> Self {
        Self::Mesh { mesh, why }
    }

    /// [`AvatarError::Morphs`].
    pub fn morphs(mesh: usize, why: RenderError) -> Self {
        Self::Morphs { mesh, why }
    }

    /// [`AvatarError::Material`].
    pub fn material(material: MaterialName, why: RenderError) -> Self {
        Self::Material { material, why }
    }

    /// [`AvatarError::UnknownTexture`].
    pub fn unknown_texture(material: MaterialName, texture: TextureName) -> Self {
        Self::UnknownTexture { material, texture }
    }

    /// [`AvatarError::FaceOverlay`].
    pub fn face_overlay(material: MaterialName) -> Self {
        Self::FaceOverlay { material }
    }

    /// [`AvatarError::Texture`].
    pub fn texture(texture: TextureName, why: impl Into<String>) -> Self {
        Self::Texture { texture, why: why.into() }
    }

    /// [`AvatarError::MorphNames`].
    pub const fn morph_names(source_mesh: usize, names: usize, targets: usize) -> Self {
        Self::MorphNames { source_mesh, names, targets }
    }

    /// [`AvatarError::UnknownPrimitive`].
    pub const fn unknown_primitive(source_mesh: usize, mesh: usize) -> Self {
        Self::UnknownPrimitive { source_mesh, mesh }
    }

    /// [`AvatarError::MeshGrouping`].
    pub const fn mesh_grouping(mesh: usize, groups: usize) -> Self {
        Self::MeshGrouping { mesh, groups }
    }

    /// [`AvatarError::InstancePrimitives`].
    pub const fn instance_primitives(source_mesh: usize, node: u32, meshes: usize, primitives: usize) -> Self {
        Self::InstancePrimitives { source_mesh, node, meshes, primitives }
    }

    /// [`AvatarError::InstanceNode`].
    pub const fn instance_node(source_mesh: usize, node: u32) -> Self {
        Self::InstanceNode { source_mesh, node }
    }

    /// [`AvatarError::NodeInstances`].
    pub const fn node_instances(node: usize, source_mesh: usize, instances: usize) -> Self {
        Self::NodeInstances { node, source_mesh, instances }
    }

    /// [`AvatarError::MorphBindInstance`].
    pub const fn morph_bind_instance(expression: usize, mesh: u16, node: u32) -> Self {
        Self::MorphBindInstance { expression, mesh, node }
    }

    /// [`AvatarError::UnknownRuntimeSpring`].
    pub fn unknown_runtime_spring(kind: &'static str, name: impl Into<String>) -> Self {
        Self::UnknownRuntimeSpring { kind, name: name.into() }
    }

    /// [`AvatarError::SharedRuntimeSpring`].
    pub fn shared_runtime_spring(kind: &'static str, name: impl Into<String>) -> Self {
        Self::SharedRuntimeSpring { kind, name: name.into() }
    }

    /// [`AvatarError::RetainedIndex`].
    pub fn retained_index(at: impl Into<String>, index: usize, len: usize) -> Self {
        Self::RetainedIndex { at: at.into(), index, len }
    }

    /// [`AvatarError::SkinInverseBind`].
    pub const fn skin_inverse_bind(skin: usize, inverse_bind: usize, joints: usize) -> Self {
        Self::SkinInverseBind { skin, inverse_bind, joints }
    }

    /// [`AvatarError::UnknownSchema`].
    pub const fn unknown_schema(found: u32) -> Self {
        Self::UnknownSchema { found }
    }
}
