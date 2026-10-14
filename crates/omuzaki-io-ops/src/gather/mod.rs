//! Gathering a built character into an `IoScene` for export (`docs/design/io-api.md`, D2 and section 5).
//!
//! The scene is always in glTF 2.0 and VRM 1.0 space, so gathering is where the runtime's skeleton, meshes and
//! materials take their file shape. The VRM avatar block (humanoid map, spring bones, first-person and look-at data)
//! is gathered separately so the geometry and the avatar data can be built and tested on their own.
//!
//! [`gather`] is the geometry half. What it produces, and the promises later steps rely on:
//! - **Bone `i` is node `i`.** The skeleton's bones become the first nodes, in the skeleton's own parents-first order
//!   ([`skeleton::bone_node`]), so the avatar block, the spring bones and the face baker find a bone's node without
//!   a lookup table. Humanoid bones are named like their VRM role (`leftUpperArm`), extra bones (hair, cloth,
//!   accessories) keep their own name, and both kinds are joints of the one skin.
//! - **One skin, one primitive per mesh.** Every skinned mesh uses a single skin whose joint `i` is bone `i`, so the
//!   mesh's own joint list is remapped to skeleton indices. A rigid mesh has no skin. Mesh nodes follow the bones, as
//!   extra scene roots, named `<part>.<mesh>`: a dot cannot occur in a bone name, so a node name always reads back as
//!   a bone or as a mesh, never as both.
//! - **Materials and textures** are the used ones only, in name order, converted by [`crate::convert`].
//! - **Size** is baked in. A character is built at its real-world stature (`BuiltCharacter::stature`), so with no
//!   target height the scene is already life-size and the factor is 1: asking for the stature again changes nothing,
//!   and a target height is applied once, on top of the built size, never twice. A target height scales every vertex,
//!   bone translation and inverse bind matrix by the same factor; no node carries a scale, which VRM runtimes and
//!   spring bones do not expect on a skeleton.
//! - **Orientation** is untouched: the character faces +Z, which is VRM 1.0's convention (the 0.x exporter flips it).
//!
//! Not here: `scene.meta`, `scene.provenance` and `scene.avatar`, which need the project's licence record and the
//! baked face (the avatar half and `io.export`).

use omuzaki_anim_api::NotHumanoid;
use omuzaki_char_api::BuiltCharacter;
use omuzaki_io_api::{IoScene, Report, UnknownBone};
use thiserror::Error;

use crate::convert::material::MissingTexture;
use crate::convert::texture::TextureError;

pub mod avatar;
pub mod scene;
pub mod skeleton;
pub mod springs;

/// What the scene is for. It decides which skeletons are acceptable.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum GatherPurpose {
    /// A plain glTF or GLB: any skeleton, humanoid or not.
    #[default]
    Gltf,
    /// A VRM avatar: the skeleton must be a VRM humanoid.
    Vrm,
}

/// How to gather.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct GatherOptions {
    /// The height in metres the character is scaled to, measured as the extent of its meshes along Y (feet to the top
    /// of the hair). `None` keeps the size the character was built at, which is its real-world stature: leave it
    /// `None` to export life-size, and set it only to deliberately resize (an avatar for a world with its own scale).
    pub target_height: Option<f32>,
    /// What the scene is for.
    pub purpose: GatherPurpose,
}

/// A gathered character.
#[derive(Clone, Debug, PartialEq)]
pub struct Gathered {
    /// The geometry half of the scene: nodes, meshes, skin, materials, textures. `meta`, `provenance` and `avatar` are
    /// left for the steps that know the licence record and the baked face.
    pub scene: IoScene,
    /// What the gather noticed (`gather.*`) and what the material and texture conversions could not carry
    /// (`material.*`, `texture.*`). Locations are scene paths (`materials/2`).
    pub report: Report,
    /// The factor lengths were multiplied by (1 if no target height was asked for). Anything else gathered later that
    /// is a length (spring hit radii, collider offsets, the look-at offset) must be multiplied by it too.
    pub scale: f32,
}

/// Why a character could not be gathered.
#[derive(Debug, Clone, PartialEq, Error)]
#[non_exhaustive]
pub enum GatherError {
    /// A VRM was asked for but the skeleton has no humanoid roles (it was built with `Skeleton::generic`).
    #[error("a VRM needs a humanoid skeleton, and this one is generic: {0}")]
    NotHumanoid(#[from] NotHumanoid),
    /// The runtime has a humanoid bone the file contract lacks.
    #[error(transparent)]
    UnknownBone(#[from] UnknownBone),
    /// An extra bone of a humanoid skeleton is called like a humanoid bone, so its node would read back as that one.
    #[error(
        "bone {0:?} is not a humanoid role but is named like one, so its node would read back as the humanoid bone"
    )]
    ReservedBoneName(String),
    /// Two bones would get one node name.
    #[error("two bones would both be called {0:?} in the scene")]
    DuplicateNodeName(String),
    /// A mesh is skinned to a joint the skeleton lacks.
    #[error("mesh '{mesh}' is skinned to {joint}, which the skeleton doesn't have")]
    MissingBone {
        /// The mesh's name.
        mesh: String,
        /// The joint, as the mesh names it.
        joint: String,
    },
    /// A skinned mesh's vertices use a joint index past its joint list.
    #[error("mesh '{mesh}' uses joint {index}, but its joint list has {len} entries")]
    JointOutOfRange {
        /// The mesh's name.
        mesh: String,
        /// The index a vertex uses.
        index: u16,
        /// How many joints the mesh lists.
        len: usize,
    },
    /// A mesh names a material the character doesn't have.
    #[error("mesh '{mesh}' uses material '{material}', which the character doesn't have")]
    UnknownMaterial {
        /// The mesh's name.
        mesh: String,
        /// The material it names.
        material: String,
    },
    /// A material names a texture the character doesn't have.
    #[error(transparent)]
    MissingTexture(#[from] MissingTexture),
    /// A texture could not be written as PNG.
    #[error(transparent)]
    Texture(#[from] TextureError),
    /// The target height is not a positive, finite number of metres.
    #[error("the target height must be a positive number of metres (got {0})")]
    BadHeight(f32),
    /// A target height was asked for but the meshes have no height to scale.
    #[error("the character has no meshes with a height, so there is nothing to scale to {0} m")]
    NoHeight(f32),
}

/// Gathers `built` into the geometry half of an `IoScene`.
///
/// Deterministic: the same character and options give the same scene. Dynamic meshes (hair, cloth) go out at their
/// rest shape; a character built with `BuildOptions::rig_export` brings its own skinned bake meshes, which are
/// gathered like any other.
///
/// # Errors
/// [`GatherError`] for a generic skeleton when a VRM is asked for, for a character whose meshes name bones, materials
/// or textures it doesn't have, and for an unusable target height.
pub fn gather(built: &BuiltCharacter, options: &GatherOptions) -> Result<Gathered, GatherError> {
    scene::gather(built, options)
}
