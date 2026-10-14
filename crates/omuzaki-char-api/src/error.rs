//! Why building a character fails. Every variant names the part and says what to fix (spec §6: errors are
//! structured and actionable), so an agent can repair the character without reading code.

use omuzaki_anim_api::{SkeletonError, SpringError};
use omuzaki_core::mesh::MeshError;
use omuzaki_face_api::{ColourError, ParamErrors};

use crate::character::TreeError;
use crate::drivers::DriverError;
use crate::ids::{BoneName, JointRef, MaterialName, PartId, PartKindId};
use crate::rig::joint_name;
use crate::services::AssetError;

/// A character (or one of its parts) could not be built. Nothing is partially built: the result is all or nothing.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BuildError {
    /// The parts don't form a buildable tree.
    #[error(transparent)]
    Tree(#[from] TreeError),
    /// No generator is registered for the part's kind.
    #[error(
        "part '{part}' has kind '{kind}', which no generator provides: use one of the registered kinds (PartKindRegistration)"
    )]
    UnknownKind {
        /// The part.
        part: PartId,
        /// Its kind.
        kind: PartKindId,
    },
    /// The part's parameters are invalid for its kind.
    #[error("part '{part}' has invalid parameters: {errors}")]
    Params {
        /// The part.
        part: PartId,
        /// Every invalid field, with its path under `params`.
        errors: ParamErrors,
    },
    /// The face profile is invalid.
    #[error("the face profile is invalid: {0}")]
    Face(ParamErrors),
    /// A part needs something an earlier part should have published.
    #[error("part '{part}' needs {needs}, but no part published it: add the part that provides it, or enable it")]
    Missing {
        /// The part that needs it.
        part: PartId,
        /// What is missing (`a head surface`).
        needs: &'static str,
    },
    /// A part returned a mesh that breaks the mesh invariants.
    #[error("part '{part}' returned a broken mesh '{mesh}': {source}")]
    Mesh {
        /// The part.
        part: PartId,
        /// The mesh's name.
        mesh: String,
        /// What is wrong.
        source: MeshError,
    },
    /// Two parts registered the same material or texture id.
    #[error("{what} id '{id}' is defined twice: prefix ids with the part's id")]
    Duplicate {
        /// `material` or `texture`.
        what: &'static str,
        /// The id.
        id: String,
    },
    /// A mesh names a material the character doesn't have.
    #[error("part '{part}': mesh '{mesh}' uses material '{material}', which no part defines")]
    UnknownMaterial {
        /// The part.
        part: PartId,
        /// The mesh.
        mesh: String,
        /// The missing material.
        material: MaterialName,
    },
    /// A colour slot could not be resolved while building.
    #[error("part '{part}': {error}")]
    Colour {
        /// The part.
        part: PartId,
        /// What is wrong.
        error: ColourError,
    },
    /// A part read a store asset it needs and could not get it. Kept typed, so callers can match `NotFound(hash)`
    /// and offer the fix (import the asset) without parsing a message.
    #[error("part '{part}' could not read an asset: {error}")]
    Asset {
        /// The part.
        part: PartId,
        /// Why the read failed (names the hash and what to fix).
        error: AssetError,
    },
    /// The joints and bones parts published don't make a valid skeleton. Only failures no single part caused reach this
    /// (a required humanoid joint nobody published, or a creature with no bones); a bad bone a part contributed is a [`BuildError::Part`] naming the part.
    #[error(
        "the character has no valid skeleton: {0}; a humanoid body part must publish the joints VRM requires through BodyFit::joint, and a creature must contribute its bones through `rig.bones`"
    )]
    Skeleton(SkeletonError),
    /// An extra bone hangs from a bone the rig doesn't have (yet).
    #[error(
        "part '{part}': extra bone '{bone}' hangs from '{}', which the skeleton lacks: publish that humanoid joint, or list the parent bone earlier",
        joint_name(.parent)
    )]
    MissingParentBone {
        /// The part that made the bone.
        part: PartId,
        /// The bone.
        bone: BoneName,
        /// Its parent.
        parent: JointRef,
    },
    /// Two extra bones (or an extra bone and a humanoid bone) share a name.
    #[error(
        "part '{part}': a bone named '{bone}' already exists: prefix extra bones with the part's id (`hair_<part>_<i>`)"
    )]
    DuplicateBone {
        /// The part that made the second bone.
        part: PartId,
        /// The name.
        bone: BoneName,
    },
    /// A mesh is skinned to a bone the skeleton lacks.
    #[error(
        "part '{part}': mesh '{mesh}' is skinned to '{}', which the skeleton lacks: publish that humanoid joint, or add the bone with `rig.bones` (a creature's skeleton has no humanoid joints, so skin to its bones by name)",
        joint_name(.bone)
    )]
    UnknownMeshBone {
        /// The part.
        part: PartId,
        /// The mesh.
        mesh: String,
        /// The missing bone.
        bone: JointRef,
    },
    /// A spring chain or collider names a bone the rig doesn't have.
    #[error(
        "part '{part}': {what} names a bone the rig doesn't have: use `humanoid_id` for a bone the body publishes, `extra_id` (or `RigContribution::add_bone`) for the part's own bones"
    )]
    UnknownRigBone {
        /// The part.
        part: PartId,
        /// Where the bad id is (`joint 2 of chain 'hair_fringe'`).
        what: String,
    },
    /// Two parts (or one part twice) gave a spring chain or a collider group the same name.
    #[error(
        "part '{part}': the {kind} name '{name}' is already used by part '{earlier}': name chains and collider groups `<kind>_<part>`"
    )]
    DuplicateSpringName {
        /// The part that repeated the name.
        part: PartId,
        /// The part that used it first.
        earlier: PartId,
        /// `spring chain` or `collider group`.
        kind: &'static str,
        /// The name.
        name: String,
    },
    /// A part's spring chains or collider groups don't fit the skeleton.
    #[error("part '{part}': its spring data doesn't fit the skeleton: {source}")]
    Springs {
        /// The part whose chain or group was the first that failed.
        part: PartId,
        /// What is wrong.
        source: SpringError,
    },
    /// A part's bone drivers don't fit the skeleton.
    #[error("part '{part}': its bone drivers don't fit the skeleton: {source}")]
    Drivers {
        /// The part whose drivers failed (drivers are validated after each part).
        part: PartId,
        /// What is wrong.
        source: DriverError,
    },
    /// The character's weight brushes are invalid.
    #[error("weight brushes of part '{part}' are invalid: {errors}")]
    Brush {
        /// The part they belong to.
        part: PartId,
        /// Every invalid field, with its path under `weight_brushes/<part>`.
        errors: ParamErrors,
    },
    /// A generator failed for a reason of its own.
    #[error("part '{part}' could not be built: {message}")]
    Part {
        /// The part.
        part: PartId,
        /// What happened.
        message: String,
    },
}
