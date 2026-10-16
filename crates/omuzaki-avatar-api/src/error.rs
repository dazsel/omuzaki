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
    /// The retained morph-target names don't line up with the meshes' targets.
    #[error(
        "the retained morph-target names don't fit mesh {mesh}: {names} names for {targets} targets (one list per          mesh, one name per target)"
    )]
    #[non_exhaustive]
    MorphNames {
        /// The mesh index.
        mesh: usize,
        /// How many names are retained for it.
        names: usize,
        /// How many targets it has.
        targets: usize,
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
    pub const fn morph_names(mesh: usize, names: usize, targets: usize) -> Self {
        Self::MorphNames { mesh, names, targets }
    }

    /// [`AvatarError::UnknownSchema`].
    pub const fn unknown_schema(found: u32) -> Self {
        Self::UnknownSchema { found }
    }
}
