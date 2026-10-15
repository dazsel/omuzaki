//! Rig data from parts (RG-01, plan decision G2): extra bones, spring chains and colliders, assembled by the builder
//! into the character's [`Skeleton`] and [`SpringSet`].
//!
//! Implementation crates cannot call each other, so `omuzaki-io-ops`, the stage and `omuzaki-rig` all read the rig from
//! [`BuiltCharacter::rig`](crate::BuiltCharacter) instead of asking the parts again. Parts only **contribute**; the
//! builder owns the order of bones, so two builds of one character give one skeleton (decision 0009).
//!
//! ## How the skeleton is assembled
//!
//! 1. **Humanoid bones.** A bone exists if some published [`BodyFit`] knows its joint ([`Published::joint`]: joint-only
//!    fits first, then envelopes). A part that only moves joints (the head's "head drop" moves `head`, the eyes and the
//!    `jaw`) publishes a [`JointFit`] over the body, and it wins over the body whatever the build order. Rest rotations are identity (the VRM normalised
//!    convention) and a bone's local translation is its joint minus its parent's. [`Skeleton::humanoid`] picks each
//!    bone's parent (the nearest present ancestor).
//!    A body that publishes no humanoid joint at all, and whose parts contribute bones, is a **creature** (a quadruped,
//!    a blob): its skeleton is built with [`Skeleton::generic`] from the extra bones alone. The first bone hangs from a
//!    [`JointRef::Named`] joint the body publishes ([`BodyFit::joint`]), which makes it a root. A mesh or bone bound to
//!    a [`JointRef::Humanoid`] role is then a build error naming the part ([`BuildError::MissingParentBone`],
//!    [`BuildError::UnknownMeshBone`]): a creature has no humanoid roles to resolve.
//! 2. **Extra bones** ([`ExtraBone`]) are appended in build order, then in the order each part listed them, so a parent
//!    always comes first. Their `rest` is in **bind space** like every other position a part authors; the builder makes
//!    it parent-relative.
//! 3. **Springs.** Every contribution's chains and collider groups are rebased onto the final bone ids and checked with
//!    [`SpringSet::validate`].
//!
//! ## Rest and bind
//!
//! A character **rests** in the VRM T-pose (identity rotations on the humanoid bones, so a [`HumanoidPose`] means the
//! same thing on every character) and is **bound** in the pose its meshes were sculpted in, which is
//! [`CharacterRig::bind`] (decision 0014). The inverse bind matrices come from the bind pose
//! ([`CharacterRig::inverse_bind`]), never from the rest, so a mesh sculpted in an A-pose is not re-posed (that is lossy
//! where weights blend) and posing the bones at `bind` reproduces it exactly.
//!
//! ## Bone ids inside a contribution
//!
//! A part builds before the skeleton exists, so the [`BoneId`]s in its [`SpringChain`]s and [`Collider`]s are
//! **provisional**: [`humanoid_id`] names a VRM bone (whatever index it ends up with) and [`extra_id`] (or
//! [`RigContribution::add_bone`]) names the part's own `i`th extra bone. The builder rebases them; a provisional id with
//! nothing behind it is [`BuildError::UnknownRigBone`]. Chains name colliders by group name, and any part may use a
//! group another part contributed.

use std::collections::BTreeMap;

use omuzaki_anim_api::{
    BoneDef, BoneId, Collider, ColliderGroup, HUMANOID_COUNT, HumanoidBone, HumanoidPose, MAX_BONES, Pose, Skeleton,
    SkeletonError, SpringChain, SpringSet, Transform,
};
use omuzaki_core::glam::{DQuat, DVec3, Mat4, Quat, Vec3, Vec4};
use serde::{Deserialize, Serialize};

use crate::build::BuiltMesh;
use crate::drivers::{self, BoneDriver, Drive, DriverError, DriverPlan};
use crate::error::BuildError;
use crate::ids::{BoneName, JointRef, PartId};
use crate::kind::{BodyFit, BodyRegion, Published, Section};

/// A bone a part adds to the skeleton: hair, cloth and accessory bones (VRM calls them non-humanoid).
#[derive(Clone, Debug, PartialEq)]
pub struct ExtraBone {
    /// The bone's name, unique in the character. Convention: `hair_<part>_<i>`.
    pub name: BoneName,
    /// The bone it hangs from: a humanoid role the body publishes, or an extra bone listed **earlier** (by this part or
    /// a part built before it). On a creature (no humanoid joints), a [`JointRef::Named`] joint the body publishes with
    /// no earlier bone of that name makes this bone a root; its `rest` then stays in bind space.
    pub parent: JointRef,
    /// Where the bone rests, in **bind space** (not relative to the parent: the builder does that).
    pub rest: Transform,
}

/// What one part adds to the rig. Empty for parts that only skin to the body's bones.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RigContribution {
    /// Extra bones, parents first.
    pub bones: Vec<ExtraBone>,
    /// Spring chains over those bones (and humanoid ones). Bone ids are provisional: see the module docs. Chain names
    /// must be unique in the character: use `<kind>_<part>` (`hair_fringe`).
    pub chains: Vec<SpringChain>,
    /// Collider groups for the rig. Bone ids are provisional. Group names are unique in the character too, and named
    /// `<kind>_<part>`.
    pub collider_groups: Vec<ColliderGroup>,
    /// Bone drivers over the part's own extra bones (wings that fold). Bone ids, driven and source, are provisional.
    pub drivers: Vec<BoneDriver>,
}

impl RigContribution {
    /// True if the part adds nothing to the rig.
    pub fn is_empty(&self) -> bool {
        self.bones.is_empty() && self.chains.is_empty() && self.collider_groups.is_empty() && self.drivers.is_empty()
    }

    /// Adds `bone` and returns the provisional id to use for it in chains and colliders.
    pub fn add_bone(&mut self, bone: ExtraBone) -> BoneId {
        let id = extra_id(self.bones.len());
        self.bones.push(bone);
        id
    }
}

/// The provisional id of a humanoid bone in a [`RigContribution`].
#[allow(clippy::cast_possible_truncation)] // humanoid indices are below HUMANOID_COUNT (55)
pub const fn humanoid_id(bone: HumanoidBone) -> BoneId {
    BoneId(bone.index() as u16)
}

/// The humanoid bone a provisional id names, or `None` for an id that names an extra bone. This is how a part reads
/// the colliders published to it ([`BuildCtx::colliders`](crate::BuildCtx::colliders)).
pub fn humanoid_of(id: BoneId) -> Option<HumanoidBone> {
    HumanoidBone::ALL.get(id.index()).copied()
}

/// The provisional id of the part's `index`th extra bone ([`RigContribution::bones`]). An index too large for an id
/// gives an id that resolves to nothing, which the builder reports.
pub fn extra_id(index: usize) -> BoneId {
    BoneId(u16::try_from(HUMANOID_COUNT + index).unwrap_or(u16::MAX))
}

/// The assembled rig of a built character.
///
/// Serialises with `bind` left out while it is the T-pose, and a rig without one reads back as bound at rest. Reading
/// checks the shape, not the numbers: run [`CharacterRig::validate_bind`] on a rig that came from outside the builder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CharacterRig {
    /// Humanoid bones, then extra bones; parents first; rest rotations of humanoid bones are identity (the VRM T-pose).
    pub skeleton: Skeleton,
    /// Every spring chain and collider group, on the skeleton's bone ids; validated against `skeleton`.
    pub springs: SpringSet,
    /// Extra bones posed by a channel or by another bone, in the order parts listed them; validated against
    /// `skeleton`. Run them each frame through [`CharacterRig::driver_plan`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drivers: Vec<BoneDriver>,
    /// The pose the meshes were built in, in the normalised space of [`HumanoidPose`] (identity, the default, when they
    /// are built in the T-pose). A character sculpted in an A-pose records the arms' hang here; the skeleton still
    /// rests in the T. The stage shows a character in this pose when nothing animates it, and the inverse bind matrices
    /// ([`CharacterRig::inverse_bind`]) are those of this pose. Rotations must be finite and normalised
    /// ([`CharacterRig::validate_bind`]).
    #[serde(default, skip_serializing_if = "is_rest")]
    pub bind: HumanoidPose,
}

/// Why a [`CharacterRig::bind`] pose can't be used. `field` is the offending field's path in the serialised rig, so the
/// message points at the line to fix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BindError {
    /// A number is NaN or infinite.
    #[error("{field} is not finite")]
    NotFinite {
        /// The field, for example `bind.rotations.leftUpperArm` or `bind.hips_offset`.
        field: String,
    },
    /// A rotation is finite but not of unit length.
    #[error("{field} is not a unit rotation (a quaternion of length 1)")]
    NotNormalised {
        /// The field, for example `bind.rotations.leftUpperArm`.
        field: String,
    },
}

fn is_rest(pose: &HumanoidPose) -> bool {
    *pose == HumanoidPose::REST
}

/// [`CharacterRig::validate_bind`] for a pose that has no rig yet (a fit's [`BodyFit::bind`]).
fn check_bind(bind: &HumanoidPose) -> Result<(), BindError> {
    if !bind.hips_offset.is_finite() {
        return Err(BindError::NotFinite { field: "bind.hips_offset".to_owned() });
    }
    for bone in HumanoidBone::ALL {
        let rotation = bind.get(bone);
        let field = || format!("bind.rotations.{bone}");
        if !rotation.is_finite() {
            return Err(BindError::NotFinite { field: field() });
        }
        if !rotation.is_normalized() {
            return Err(BindError::NotNormalised { field: field() });
        }
    }
    Ok(())
}

impl CharacterRig {
    /// The bone a mesh or anchor means by `joint`, if the skeleton has it.
    pub fn bone(&self, joint: &JointRef) -> Option<BoneId> {
        match joint {
            JointRef::Humanoid(h) => self.skeleton.humanoid_bone(*h),
            JointRef::Named(name) => self.skeleton.find(name.as_str()),
        }
    }

    /// The drivers compiled for the frame loop: validated, ordered and allocation-free to run
    /// ([`DriverPlan::apply`]). Cold; compile once per character.
    ///
    /// # Errors
    /// A [`DriverError`] for a rig that came from outside the builder and has bad drivers.
    pub fn driver_plan(&self) -> Result<DriverPlan, DriverError> {
        DriverPlan::compile(&self.skeleton, &self.drivers)
    }

    /// Checks that `bind` is a pose the rig can be bound in: a finite hips offset, and finite rotations of unit length.
    ///
    /// # Errors
    /// A [`BindError`] naming the first bad field, hips first, then bones in [`HumanoidBone::ALL`] order.
    pub fn validate_bind(&self) -> Result<(), BindError> {
        check_bind(&self.bind)
    }

    /// The inverse bind matrices of the skinned meshes, one per bone of `skeleton`: the bones at [`bind`](Self::bind)
    /// (humanoid bones turned by it, hair, cloth and accessory bones at their rest relative to their parent), inverted.
    /// Give them to the rig (`RigSkeleton::with_inverse_bind`) and to exporters; never derive them from the rest pose
    /// (decision 0014).
    ///
    /// An identity `bind` gives exactly [`Skeleton::inverse_bind`]. A generic skeleton has no humanoid bones for `bind`
    /// to turn. A rotation that is not finite leaves its bone at rest, as the rig's solve does; run
    /// [`validate_bind`](Self::validate_bind) to refuse it instead. Cold: allocates, and builds two 16 KiB poses.
    pub fn inverse_bind(&self) -> Vec<Mat4> {
        if is_rest(&self.bind) {
            return self.skeleton.inverse_bind();
        }
        self.skeleton.inverse_bind_for(&self.bind_pose())
    }

    /// The skeleton's local pose with `bind` applied through the humanoid map.
    ///
    /// A normalised rotation `q` becomes the local rotation `Wp⁻¹ · q · Wb`, with `Wp` and `Wb` the skeleton-space rest
    /// rotations of the bone's parent and of the bone. This is the conversion the rig's solve (`HumanoidRig`) applies to
    /// every frame, so a skeleton posed at `bind` is exactly the one the inverse bind matrices describe. It is the
    /// identity conversion (`local = q`) for the identity rest rotations the builder produces. The hips offset moves the
    /// hips in the same world-aligned axes.
    fn bind_pose(&self) -> Pose {
        let mut pose = self.skeleton.rest_pose();
        let Some(map) = self.skeleton.humanoid_map() else { return pose };
        let mut rest_world = Pose::IDENTITY;
        self.skeleton.world_pose(&pose, &mut rest_world);
        let world_rotation = |id: BoneId| rest_world.local[id.index()].rotation.normalize();
        let parent_rotation =
            |id: BoneId| self.skeleton.get(id).and_then(|def| def.parent).map_or(Quat::IDENTITY, world_rotation);
        for (bone, id) in map.iter() {
            let local = parent_rotation(id).conjugate() * self.bind.get(bone) * world_rotation(id);
            if let Some(rotation) = Vec4::from(local).try_normalize().map(Quat::from_vec4) {
                pose.local[id.index()].rotation = rotation;
            }
        }
        if self.bind.hips_offset.is_finite() {
            let hips = map.hips();
            pose.local[hips.index()].translation += parent_rotation(hips).conjugate() * self.bind.hips_offset;
        }
        pose
    }
}

/// A fit that only reports joint positions: what a part publishes to move or add humanoid joints without wrapping
/// anything. Garments skip it ([`BodyFit::envelope`]), so a head part can publish one after the body.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JointFit {
    joints: BTreeMap<JointRef, Vec3>,
}

impl JointFit {
    /// A fit with these bind-space joint positions.
    pub fn new(joints: impl IntoIterator<Item = (JointRef, Vec3)>) -> Self {
        Self { joints: joints.into_iter().collect() }
    }
}

impl BodyFit for JointFit {
    fn section(&self, _: BodyRegion, _: f32) -> Option<Section> {
        None
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        self.joints.get(joint).copied()
    }

    fn envelope(&self) -> bool {
        false
    }
}

/// The name of a joint for messages.
pub(crate) fn joint_name(joint: &JointRef) -> &str {
    match joint {
        JointRef::Humanoid(h) => h.name(),
        JointRef::Named(n) => n.as_str(),
    }
}

/// Colliders a part publishes to later parts must sit on humanoid bones: those are the only bones another part can
/// locate (through [`BodyFit::joint`]).
pub(crate) fn check_published_colliders(part: &PartId, groups: &[ColliderGroup]) -> Result<(), BuildError> {
    for group in groups {
        if group.colliders.iter().any(|c| humanoid_of(c.bone).is_none()) {
            return Err(BuildError::Part {
                part: part.clone(),
                message: format!(
                    "published collider group '{}' has a collider on a bone that is not a humanoid role: later parts \
                     can only locate humanoid bones, so put colliders on your own bones in `rig.collider_groups`",
                    group.name
                ),
            });
        }
    }
    Ok(())
}

/// A fit a part publishes must give finite joints; checked when the part publishes it, while the part is known.
pub(crate) fn check_published_joints(part: &PartId, fit: Option<&dyn BodyFit>) -> Result<(), BuildError> {
    let Some(fit) = fit else { return Ok(()) };
    check_bind(&fit.bind()).map_err(|e| BuildError::Part {
        part: part.clone(),
        message: format!("the published bind pose is unusable: {e}"),
    })?;
    for bone in HumanoidBone::ALL {
        if fit.joint(&JointRef::Humanoid(bone)).is_some_and(|at| !at.is_finite()) {
            return Err(BuildError::Part {
                part: part.clone(),
                message: format!("the published joint '{bone}' is not a finite position"),
            });
        }
    }
    Ok(())
}

/// Joint positions of the humanoid bones, indexed by [`HumanoidBone::index`]; `None` where no fit knows the bone.
type JointWorld = [Option<Vec3>; HUMANOID_COUNT];

/// Assembles the rig from what the parts published and contributed, in build order.
pub(crate) fn assemble(published: &Published, parts: &[(PartId, RigContribution)]) -> Result<CharacterRig, BuildError> {
    let world: JointWorld = std::array::from_fn(|i| published.joint(&JointRef::Humanoid(HumanoidBone::ALL[i])));
    let bind = published.bind();
    // No humanoid joint at all and bones to build from: a creature (quadruped, blob). A body that publishes only some
    // humanoid joints is still a humanoid with joints missing, and `humanoid_skeleton` names what it lacks.
    let generic = world.iter().all(Option::is_none) && parts.iter().any(|(_, rig)| !rig.bones.is_empty());
    let humanoid = if generic { None } else { Some(humanoid_skeleton(&world, &bind).map_err(BuildError::Skeleton)?) };
    let (skeleton, extra_ids) = with_extra_bones(humanoid.as_ref(), published, &world, &bind, parts)?;
    let springs = spring_set(parts, &skeleton, &extra_ids)?;
    let drivers = driver_list(parts, &skeleton, &extra_ids)?;
    Ok(CharacterRig { skeleton, springs, drivers, bind })
}

/// The humanoid skeleton of joints published in the pose `bind`, resting in the VRM T-pose (decision 0014,
/// `docs/decisions/0014-rest-in-t-bind-in-a.md`).
///
/// Takes `joints`, the humanoid joints in bind space in metres (a later entry for a bone wins; a bone with no entry is
/// absent and its children hang from the nearest present ancestor), and `bind`, the normalised pose the joints are in
/// (the identity when they are the T-pose). Returns the skeleton: identity rest rotations, parents per
/// [`HumanoidBone::parent`], and for each bone the offset from its parent's joint turned back through the parent's
/// bound rotation. Posing that skeleton at `bind` puts every joint where it was published, and posing it at the identity
/// shows the T-pose. The hips offset of `bind` lowers the root by that much. This is the rule the builder applies to
/// the joints parts publish, so a template can build the same skeleton without a builder.
///
/// # Errors
/// [`SkeletonError`] if the joints lack a bone VRM requires or are not finite.
pub fn rest_skeleton(joints: &[(HumanoidBone, Vec3)], bind: &HumanoidPose) -> Result<Skeleton, SkeletonError> {
    let mut world: JointWorld = [None; HUMANOID_COUNT];
    for &(bone, at) in joints {
        world[bone.index()] = Some(at);
    }
    humanoid_skeleton(&world, bind)
}

/// The nearest present ancestor of `bone`, the parent rule `Skeleton::humanoid` applies.
fn present_parent(world: &JointWorld, bone: HumanoidBone) -> Option<HumanoidBone> {
    std::iter::successors(bone.parent(), |p| p.parent()).find(|p| world[p.index()].is_some())
}

/// The world rotation of every present bone when the skeleton is posed at `bind`; the identity for an absent bone.
///
/// Kept in `f64`: the rest offsets are turned back through these, and a chain of `f32` products (a fingertip is ten
/// bones from the hips) would add up to a micron of error.
fn bind_rotations(world: &JointWorld, bind: &HumanoidPose) -> [DQuat; HUMANOID_COUNT] {
    let mut rotations = [DQuat::IDENTITY; HUMANOID_COUNT];
    // Parents come before children in `HumanoidBone::ALL`.
    for bone in HumanoidBone::ALL.into_iter().filter(|b| world[b.index()].is_some()) {
        let parent = present_parent(world, bone).map_or(DQuat::IDENTITY, |p| rotations[p.index()]);
        // A rotation the solve would refuse (not finite) leaves the bone at rest, as `bind_pose` does.
        let local = Vec4::from(bind.get(bone)).try_normalize().map_or(Quat::IDENTITY, Quat::from_vec4);
        // Renormalised: an `f32` quaternion is a hair off unit length, and a rotation that is not unit scales lengths.
        rotations[bone.index()] = (parent * local.as_dquat()).normalize();
    }
    rotations
}

fn humanoid_skeleton(world: &JointWorld, bind: &HumanoidPose) -> Result<Skeleton, SkeletonError> {
    let present: Vec<HumanoidBone> = HumanoidBone::ALL.into_iter().filter(|b| world[b.index()].is_some()).collect();
    let rotations = bind_rotations(world, bind);
    let at = |bone: HumanoidBone| world[bone.index()].unwrap_or_default().as_dvec3();
    let hips_offset = if bind.hips_offset.is_finite() { bind.hips_offset.as_dvec3() } else { DVec3::ZERO };
    // Each offset is aimed at the joint from where the already-rounded bones before it actually put the parent, so the
    // rounding of an `f32` offset is not carried down the chain: posed at the bind, a fingertip is as close to its
    // joint as any single offset can be.
    let mut posed = [DVec3::ZERO; HUMANOID_COUNT];
    let mut offsets = [Vec3::ZERO; HUMANOID_COUNT];
    for &bone in &present {
        let offset = match present_parent(world, bone) {
            Some(parent) => rotations[parent.index()].inverse() * (at(bone) - posed[parent.index()]),
            // The root: the bind pose lifts it by the hips offset, so the rest sits that much lower.
            None => at(bone) - hips_offset,
        }
        .as_vec3();
        offsets[bone.index()] = offset;
        posed[bone.index()] = match present_parent(world, bone) {
            Some(parent) => posed[parent.index()] + rotations[parent.index()] * offset.as_dvec3(),
            None => offset.as_dvec3() + hips_offset,
        };
    }
    Skeleton::humanoid(&present, |bone| Transform::from_translation(offsets[bone.index()]))
}

/// A bone placed in the skeleton being assembled: its final id and bind-space transform (the pose the meshes are in).
#[derive(Copy, Clone)]
struct Placed {
    /// `None` for a joint the body published that has no bone yet: a bone hung from it is a root of a generic skeleton.
    id: Option<BoneId>,
    world: Transform,
}

/// `world` expressed relative to `parent` (both in bind space).
fn relative_to(parent: Transform, world: Transform) -> Transform {
    let inverse = parent.rotation.inverse();
    Transform { translation: inverse * (world.translation - parent.translation), rotation: inverse * world.rotation }
}

/// Appends every part's extra bones to the humanoid skeleton, or builds a generic skeleton from them when there is
/// none (`humanoid` is `None`): then a bone whose parent is a [`JointRef::Named`] joint the body published, and not an
/// earlier bone, is a root, and `Skeleton::generic` checks the rest. Also returns, per part, the final id of each of its
/// extra bones (what provisional [`extra_id`]s resolve to).
fn with_extra_bones(
    humanoid: Option<&Skeleton>,
    published: &Published,
    world: &JointWorld,
    bind: &HumanoidPose,
    parts: &[(PartId, RigContribution)],
) -> Result<(Skeleton, Vec<Vec<BoneId>>), BuildError> {
    // An extra bone hangs from its parent as the parent is *bound*: its offset is taken in the parent's bind-pose frame.
    let rotations = bind_rotations(world, bind);
    let mut defs: Vec<BoneDef> = humanoid.map_or_else(Vec::new, |h| h.bones().to_vec());
    let mut placed: BTreeMap<&BoneName, Placed> = BTreeMap::new();
    let mut ids = Vec::with_capacity(parts.len());
    for (part, rig) in parts {
        let mut own = Vec::with_capacity(rig.bones.len());
        for bone in &rig.bones {
            check_extra_bone(part, bone, defs.len())?;
            if placed.contains_key(&bone.name) || humanoid.is_some_and(|h| h.find(bone.name.as_str()).is_some()) {
                return Err(BuildError::DuplicateBone { part: part.clone(), bone: bone.name.clone() });
            }
            let parent = match &bone.parent {
                JointRef::Humanoid(h) => {
                    humanoid.and_then(|s| s.humanoid_bone(*h)).zip(world[h.index()]).map(|(id, at)| Placed {
                        id: Some(id),
                        world: Transform { translation: at, rotation: rotations[h.index()].as_quat() },
                    })
                }
                JointRef::Named(name) => placed.get(name).copied().or_else(|| {
                    // Only a generic skeleton starts from a published joint; a humanoid always has its hips as root.
                    humanoid
                        .is_none()
                        .then(|| published.joint(&bone.parent))
                        .flatten()
                        .map(|at| Placed { id: None, world: Transform::from_translation(at) })
                }),
            }
            .ok_or_else(|| BuildError::MissingParentBone {
                part: part.clone(),
                bone: bone.name.clone(),
                parent: bone.parent.clone(),
            })?;
            let id = BoneId(u16::try_from(defs.len()).unwrap_or(u16::MAX)); // past MAX_BONES `Skeleton::new` refuses
            defs.push(BoneDef {
                name: bone.name.as_str().to_owned(),
                parent: parent.id,
                // A root has no parent to be relative to: its rest is its bind-space transform.
                rest: if parent.id.is_some() { relative_to(parent.world, bone.rest) } else { bone.rest },
                humanoid: None,
            });
            placed.insert(&bone.name, Placed { id: Some(id), world: bone.rest });
            own.push(id);
        }
        ids.push(own);
    }
    let skeleton = if humanoid.is_some() { Skeleton::new(defs) } else { Skeleton::generic(defs) };
    Ok((skeleton.map_err(BuildError::Skeleton)?, ids))
}

/// Rejects what `Skeleton::new` would refuse about an extra bone, while the part that made it is still known.
fn check_extra_bone(part: &PartId, bone: &ExtraBone, bones_so_far: usize) -> Result<(), BuildError> {
    let rest = bone.rest;
    if !(rest.translation.is_finite() && rest.rotation.is_finite() && rest.rotation.is_normalized()) {
        return Err(BuildError::Part {
            part: part.clone(),
            message: format!("extra bone '{}' has a rest transform that isn't finite with a unit rotation", bone.name),
        });
    }
    if bones_so_far >= MAX_BONES {
        return Err(BuildError::Part {
            part: part.clone(),
            message: format!(
                "extra bone '{}' would be bone {} of at most {MAX_BONES}: use fewer bones or shorter chains",
                bone.name,
                bones_so_far + 1
            ),
        });
    }
    Ok(())
}

/// Resolves provisional ids of one contribution to final ones.
struct Ids<'a> {
    skeleton: &'a Skeleton,
    extras: &'a [BoneId],
}

impl Ids<'_> {
    fn resolve(&self, provisional: BoneId) -> Option<BoneId> {
        match provisional.index().checked_sub(HUMANOID_COUNT) {
            None => humanoid_of(provisional).and_then(|h| self.skeleton.humanoid_bone(h)),
            Some(i) => self.extras.get(i).copied(),
        }
    }

    fn chain(&self, chain: &SpringChain) -> Result<SpringChain, String> {
        let mut out = chain.clone();
        for (i, joint) in out.joints.iter_mut().enumerate() {
            joint.bone = self.resolve(joint.bone).ok_or_else(|| format!("joint {i} of chain '{}'", chain.name))?;
        }
        if let Some(center) = &mut out.center {
            *center = self.resolve(*center).ok_or_else(|| format!("the center of chain '{}'", chain.name))?;
        }
        Ok(out)
    }

    fn driver(&self, driver: &BoneDriver) -> Result<BoneDriver, String> {
        let bone = self.resolve(driver.bone).ok_or("the driven bone")?;
        let drive = match driver.drive {
            Drive::Follow { source, axis, weight } => {
                let source = self.resolve(source).ok_or("the source bone")?;
                Drive::Follow { source, axis, weight }
            }
            channel @ Drive::Channel { .. } => channel,
        };
        Ok(BoneDriver { bone, drive })
    }

    fn group(&self, group: &ColliderGroup) -> Result<ColliderGroup, String> {
        let colliders = group
            .colliders
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let bone = self.resolve(c.bone).ok_or_else(|| format!("collider {i} of group '{}'", group.name))?;
                Ok(Collider { bone, shape: c.shape })
            })
            .collect::<Result<_, String>>()?;
        Ok(ColliderGroup { name: group.name.clone(), colliders })
    }
}

/// Which part first used each chain and collider group name, so a repeated name can say who has it.
#[derive(Default)]
struct Names<'a> {
    owners: BTreeMap<(&'static str, &'a str), &'a PartId>,
}

impl<'a> Names<'a> {
    fn claim(&mut self, part: &'a PartId, kind: &'static str, name: &'a str) -> Result<(), BuildError> {
        match self.owners.insert((kind, name), part) {
            Some(earlier) => Err(BuildError::DuplicateSpringName {
                part: part.clone(),
                earlier: earlier.clone(),
                kind,
                name: name.to_owned(),
            }),
            None => Ok(()),
        }
    }
}

fn unknown_rig_bone(part: &PartId, what: String) -> BuildError {
    BuildError::UnknownRigBone { part: part.clone(), what }
}

/// Validates the set so far; the part that was just added is the one to blame for a failure.
fn check_springs(set: &SpringSet, skeleton: &Skeleton, part: &PartId) -> Result<(), BuildError> {
    set.validate(skeleton).map_err(|source| BuildError::Springs { part: part.clone(), source })
}

/// Builds the spring set from every contribution, validating after each part so an error names the part that caused it:
/// all collider groups first (a chain may use any part's group), then the chains.
fn spring_set(
    parts: &[(PartId, RigContribution)],
    skeleton: &Skeleton,
    extra_ids: &[Vec<BoneId>],
) -> Result<SpringSet, BuildError> {
    let (mut set, mut names) = (SpringSet::default(), Names::default());
    for ((part, rig), extras) in parts.iter().zip(extra_ids) {
        let ids = Ids { skeleton, extras };
        for group in &rig.collider_groups {
            names.claim(part, "collider group", &group.name)?;
            set.groups.push(ids.group(group).map_err(|what| unknown_rig_bone(part, what))?);
        }
        if !rig.collider_groups.is_empty() {
            check_springs(&set, skeleton, part)?;
        }
    }
    for ((part, rig), extras) in parts.iter().zip(extra_ids) {
        let ids = Ids { skeleton, extras };
        for chain in &rig.chains {
            names.claim(part, "spring chain", &chain.name)?;
            set.chains.push(ids.chain(chain).map_err(|what| unknown_rig_bone(part, what))?);
        }
        if !rig.chains.is_empty() {
            check_springs(&set, skeleton, part)?;
        }
    }
    Ok(set)
}

/// Rebases every part's drivers onto final bone ids, validating after each part so an error names the part.
fn driver_list(
    parts: &[(PartId, RigContribution)],
    skeleton: &Skeleton,
    extra_ids: &[Vec<BoneId>],
) -> Result<Vec<BoneDriver>, BuildError> {
    let mut all = Vec::new();
    for ((part, rig), extras) in parts.iter().zip(extra_ids) {
        let ids = Ids { skeleton, extras };
        for driver in &rig.drivers {
            all.push(ids.driver(driver).map_err(|what| unknown_rig_bone(part, format!("{what} of a bone driver")))?);
        }
        if !rig.drivers.is_empty() {
            drivers::validate(skeleton, &all).map_err(|source| BuildError::Drivers { part: part.clone(), source })?;
        }
    }
    Ok(all)
}

/// Checks that every bone a mesh is skinned to exists in the rig, so the stage never meets a joint it cannot resolve.
pub(crate) fn check_mesh_bones(rig: &CharacterRig, meshes: &[BuiltMesh]) -> Result<(), BuildError> {
    for built in meshes {
        if let Some(bone) = built.mesh.joint_bones.iter().find(|j| rig.bone(j).is_none()) {
            return Err(BuildError::UnknownMeshBone {
                part: built.part.clone(),
                mesh: built.mesh.name.clone(),
                bone: bone.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;
    use std::sync::Arc;

    use crate::build::BuiltMesh;
    use crate::kind::{FitFact, Publishes};

    use super::*;

    #[test]
    fn provisional_ids_separate_humanoid_roles_from_extra_bones() {
        assert_eq!(humanoid_of(humanoid_id(HumanoidBone::Head)), Some(HumanoidBone::Head));
        assert_eq!(humanoid_of(extra_id(0)), None);
        assert_eq!(extra_id(0).index(), HUMANOID_COUNT);
        assert_eq!(extra_id(3).index(), HUMANOID_COUNT + 3);
        let mut rig = RigContribution::default();
        let name = BoneName::new("hair_a_0").unwrap();
        let id = rig.add_bone(ExtraBone { name, parent: JointRef::HEAD, rest: Transform::IDENTITY });
        assert_eq!(id, extra_id(0));
        assert!(!rig.is_empty() && RigContribution::default().is_empty());
    }

    #[test]
    fn a_child_is_placed_relative_to_a_rotated_parent() {
        let parent = Transform { translation: Vec3::new(1.0, 2.0, 0.0), rotation: Quat::from_rotation_z(FRAC_PI_2) };
        let world = Transform { translation: Vec3::new(1.0, 3.0, 0.0), rotation: parent.rotation };
        let local = relative_to(parent, world);
        // One metre above the parent, which is turned a quarter about Z: that is one metre along the parent's +x.
        assert!((local.translation - Vec3::X).length() < 1e-6, "{local:?}");
        assert!(local.rotation.abs_diff_eq(Quat::IDENTITY, 1e-6));
        let back = parent * local;
        assert!((back.translation - world.translation).length() < 1e-6);
    }

    // ---- rest and bind (decision 0014)

    /// The joints of a character laid out in the T-pose: arms along the X axis at shoulder height.
    fn t_joint(bone: HumanoidBone) -> Option<Vec3> {
        use HumanoidBone as B;
        let side = if bone.name().starts_with("left") { 1.0 } else { -1.0 };
        Some(match bone {
            B::Hips => Vec3::new(0.0, 1.0, 0.0),
            B::Spine => Vec3::new(0.0, 1.2, 0.0),
            B::Head => Vec3::new(0.0, 1.6, 0.0),
            B::LeftUpperLeg | B::RightUpperLeg => Vec3::new(side * 0.1, 0.95, 0.0),
            B::LeftLowerLeg | B::RightLowerLeg => Vec3::new(side * 0.1, 0.5, 0.0),
            B::LeftFoot | B::RightFoot => Vec3::new(side * 0.1, 0.05, 0.0),
            B::LeftUpperArm | B::RightUpperArm => Vec3::new(side * 0.2, 1.45, 0.0),
            B::LeftLowerArm | B::RightLowerArm => Vec3::new(side * 0.45, 1.45, 0.0),
            B::LeftHand | B::RightHand => Vec3::new(side * 0.7, 1.45, 0.0),
            _ => return None,
        })
    }

    /// A rig resting in the T-pose with the 15 required bones, bound as `bind`.
    fn t_rig(bind: &HumanoidPose) -> CharacterRig {
        let world: JointWorld = std::array::from_fn(|i| t_joint(HumanoidBone::ALL[i]));
        CharacterRig {
            skeleton: humanoid_skeleton(&world, &HumanoidPose::REST).unwrap(),
            springs: SpringSet::default(),
            drivers: Vec::new(),
            bind: *bind,
        }
    }

    /// A character sculpted with the left arm hanging: the upper arm swung 1 radian (about 57 degrees) down about Z,
    /// the forearm a little further.
    fn a_pose() -> HumanoidPose {
        let mut bind = HumanoidPose::REST;
        bind.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(-1.0));
        bind.set(HumanoidBone::LeftLowerArm, Quat::from_rotation_z(-0.2));
        bind
    }

    /// The local pose with each humanoid bone turned by `pose`. Exact only for identity rest rotations (which the
    /// builder's skeletons have), where a normalised rotation is the local rotation. Deliberately not the code under test.
    fn posed_in_t(rig: &CharacterRig, pose: &HumanoidPose) -> Pose {
        let mut local = rig.skeleton.rest_pose();
        for (bone, id) in rig.skeleton.humanoid_map().unwrap().iter() {
            local.local[id.index()].rotation = pose.get(bone);
        }
        local
    }

    fn bits(matrices: &[Mat4]) -> Vec<u32> {
        matrices.iter().flat_map(Mat4::to_cols_array).map(f32::to_bits).collect()
    }

    fn skin(rig: &CharacterRig, local: &Pose) -> Vec<Mat4> {
        let (mut world, mut out) = (Pose::IDENTITY, vec![Mat4::ZERO; rig.skeleton.len()]);
        rig.skeleton.skin_matrices(local, &rig.inverse_bind(), &mut world, &mut out);
        out
    }

    #[test]
    #[allow(clippy::cast_precision_loss)] // small bone indices
    fn an_identity_bind_gives_exactly_the_inverse_bind_matrices_of_the_rest() {
        let rig = t_rig(&HumanoidPose::REST);
        assert_eq!(bits(&rig.inverse_bind()), bits(&rig.skeleton.inverse_bind()));
        // Also for a skeleton whose bones have turned rest axes, where recomputing through the conversion would round.
        let skeleton = Skeleton::humanoid(&HumanoidBone::ALL, |bone| Transform {
            translation: Vec3::new(0.0, 0.1, 0.01 * bone.index() as f32),
            rotation: Quat::from_rotation_y(0.3 * (bone.index() % 5) as f32),
        })
        .unwrap();
        let rig = CharacterRig { skeleton, ..rig };
        assert_eq!(bits(&rig.inverse_bind()), bits(&rig.skeleton.inverse_bind()));
        // And a pose whose only difference from the T-pose is negative zero is still the T-pose.
        let mut negative_zero = HumanoidPose::REST;
        negative_zero.hips_offset = Vec3::new(-0.0, 0.0, 0.0);
        assert_eq!(
            bits(&CharacterRig { bind: negative_zero, ..rig.clone() }.inverse_bind()),
            bits(&rig.inverse_bind())
        );
    }

    #[test]
    fn a_mesh_built_in_the_bind_pose_is_as_built_when_posed_at_bind_and_level_in_the_t() {
        let rig = t_rig(&a_pose());
        let lower = rig.skeleton.humanoid_bone(HumanoidBone::LeftLowerArm).unwrap();
        // A vertex of the forearm, 10 cm along the bone, where the sculpted arm has it.
        let mut bind_world = Pose::IDENTITY;
        rig.skeleton.world_pose(&posed_in_t(&rig, &rig.bind), &mut bind_world);
        let built = bind_world.local[lower.index()].to_mat4().transform_point3(Vec3::new(0.1, 0.0, 0.0));
        assert!(built.y < 1.45 - 0.2, "the sculpted forearm hangs well below the shoulder: {built:?}");

        let at_bind = skin(&rig, &posed_in_t(&rig, &rig.bind))[lower.index()].transform_point3(built);
        assert!((at_bind - built).length() < 1e-6, "posed at bind the mesh lands where it was built: {at_bind:?}");

        let in_t = skin(&rig, &posed_in_t(&rig, &HumanoidPose::REST))[lower.index()].transform_point3(built);
        assert!((in_t - Vec3::new(0.55, 1.45, 0.0)).length() < 1e-6, "the T-pose levels the arm: {in_t:?}");
    }

    #[test]
    fn the_t_pose_raises_an_a_bound_arm_to_horizontal_and_leaves_the_rest_of_the_body_alone() {
        let rig = t_rig(&a_pose());
        let rest = skin(&rig, &rig.skeleton.rest_pose());
        let id = |b| rig.skeleton.humanoid_bone(b).unwrap().index();
        // The hand's bind-pose position is below the shoulder, and its T-pose position is on its level.
        let mut bind_world = Pose::IDENTITY;
        rig.skeleton.world_pose(&posed_in_t(&rig, &rig.bind), &mut bind_world);
        let hand = bind_world.local[id(HumanoidBone::LeftHand)].translation;
        assert!(hand.y < 1.45 - 0.3 && hand.x < 0.7, "hanging: {hand:?}");
        let moved = rest[id(HumanoidBone::LeftHand)].transform_point3(hand);
        assert!((moved - Vec3::new(0.7, 1.45, 0.0)).length() < 1e-6, "{moved:?}");
        // The right arm and the body were bound at rest, so the T-pose does not move them.
        for bone in [HumanoidBone::Hips, HumanoidBone::Head, HumanoidBone::RightUpperArm, HumanoidBone::RightHand] {
            assert!(rest[id(bone)].abs_diff_eq(Mat4::IDENTITY, 1e-6), "{bone}");
        }
    }

    #[test]
    fn bind_rotations_are_normalised_rotations_whatever_the_bone_axes() {
        // The upper arm's own axes are tilted, as in an export from Blender; the lower arm and hand cancel the tilt.
        let tilt = Quat::from_rotation_x(0.5) * Quat::from_rotation_y(0.7);
        let world: JointWorld = std::array::from_fn(|i| t_joint(HumanoidBone::ALL[i]));
        let mut bones = humanoid_skeleton(&world, &HumanoidPose::REST).unwrap().bones().to_vec();
        let id = |b: HumanoidBone| bones.iter().position(|d| d.humanoid == Some(b)).unwrap();
        let (upper, lower, hand) =
            (id(HumanoidBone::LeftUpperArm), id(HumanoidBone::LeftLowerArm), id(HumanoidBone::LeftHand));
        bones[upper].rest.rotation = tilt;
        bones[lower].rest =
            Transform { translation: tilt.inverse() * bones[lower].rest.translation, rotation: tilt.inverse() };
        let skeleton = Skeleton::new(bones).unwrap();
        // Turn the upper arm about the world's Z, then bend the forearm about the world's X (normalised: the tilt of the
        // bone's own axes does not enter into it).
        let (q_upper, q_lower) = (Quat::from_rotation_z(-1.0), Quat::from_rotation_x(0.6));
        let mut bind = HumanoidPose::REST;
        bind.set(HumanoidBone::LeftUpperArm, q_upper);
        bind.set(HumanoidBone::LeftLowerArm, q_lower);
        let rig = CharacterRig { skeleton, springs: SpringSet::default(), drivers: Vec::new(), bind };

        // A bone's bind-pose transform is the inverse of its inverse bind matrix.
        let bound = |bone: usize| rig.inverse_bind()[bone].inverse();
        let at = |bone: usize| bound(bone).transform_point3(Vec3::ZERO);
        let joint = |role| t_joint(role).unwrap();
        let (shoulder, elbow, wrist) =
            (joint(HumanoidBone::LeftUpperArm), joint(HumanoidBone::LeftLowerArm), joint(HumanoidBone::LeftHand));
        let elbow_bound = shoulder + q_upper * (elbow - shoulder);
        let wrist_bound = elbow_bound + q_upper * q_lower * (wrist - elbow);
        assert!((at(lower) - elbow_bound).length() < 1e-5, "the arm turns about the shoulder in world axes");
        assert!((at(hand) - wrist_bound).length() < 1e-5, "and the forearm about the elbow: {:?}", at(hand));
        let same_rotation = |a: Quat, b: Quat| a.abs_diff_eq(b, 1e-5) || a.abs_diff_eq(-b, 1e-5);
        assert!(
            same_rotation(Quat::from_mat4(&bound(upper)), q_upper * tilt),
            "a bone keeps its own axes under the turn"
        );
        assert!(same_rotation(Quat::from_mat4(&bound(lower)), q_upper * q_lower));
    }

    #[test]
    fn extra_bones_and_generic_skeletons_bind_at_rest() {
        let mut rig = t_rig(&a_pose());
        let mut bones = rig.skeleton.bones().to_vec();
        let (head, lower) = (
            rig.skeleton.humanoid_bone(HumanoidBone::Head).unwrap(),
            rig.skeleton.humanoid_bone(HumanoidBone::LeftLowerArm).unwrap(),
        );
        let extra = |name: &str, parent| BoneDef {
            name: name.to_owned(),
            parent: Some(parent),
            rest: Transform::from_translation(Vec3::new(0.0, 0.1, 0.0)),
            humanoid: None,
        };
        bones.push(extra("hair_a_0", head));
        bones.push(extra("sleeve_0", lower));
        rig.skeleton = Skeleton::new(bones).unwrap();
        let ibm = rig.inverse_bind();
        let (hair, sleeve) = (rig.skeleton.find("hair_a_0").unwrap(), rig.skeleton.find("sleeve_0").unwrap());
        assert_eq!(
            bits(&ibm[hair.index()..=hair.index()]),
            bits(&rig.skeleton.inverse_bind()[hair.index()..=hair.index()])
        );
        // The sleeve keeps its rest offset from the forearm, so it hangs with the forearm in the bind pose.
        let mut bind_world = Pose::IDENTITY;
        rig.skeleton.world_pose(&posed_in_t(&rig, &rig.bind), &mut bind_world);
        let expected = bind_world.local[lower.index()].to_mat4() * Mat4::from_translation(Vec3::new(0.0, 0.1, 0.0));
        assert!((ibm[sleeve.index()] * expected).abs_diff_eq(Mat4::IDENTITY, 1e-5));

        let creature = Skeleton::generic(vec![BoneDef {
            name: "body".to_owned(),
            parent: None,
            rest: Transform::from_translation(Vec3::Y),
            humanoid: None,
        }])
        .unwrap();
        let generic = CharacterRig { skeleton: creature, ..t_rig(&a_pose()) };
        assert_eq!(bits(&generic.inverse_bind()), bits(&generic.skeleton.inverse_bind()), "no humanoid bones to turn");
    }

    #[test]
    fn the_hips_offset_of_the_bind_moves_the_whole_body() {
        let mut bind = HumanoidPose::REST;
        bind.hips_offset = Vec3::new(0.0, -0.3, 0.1);
        let rig = t_rig(&bind);
        let spine = rig.skeleton.humanoid_bone(HumanoidBone::Spine).unwrap().index();
        let at = rig.inverse_bind()[spine].inverse().transform_point3(Vec3::ZERO);
        assert!((at - (Vec3::new(0.0, 1.2, 0.0) + bind.hips_offset)).length() < 1e-6, "{at:?}");
    }

    #[test]
    fn a_bind_that_is_not_a_finite_unit_rotation_is_refused_naming_the_field() {
        assert_eq!(t_rig(&a_pose()).validate_bind(), Ok(()));
        let bad = |edit: &dyn Fn(&mut HumanoidPose)| {
            let mut bind = HumanoidPose::REST;
            edit(&mut bind);
            t_rig(&bind).validate_bind().unwrap_err()
        };
        let field = |e: &BindError| match e {
            BindError::NotFinite { field } | BindError::NotNormalised { field } => field.clone(),
        };

        let nan = bad(&|p| p.set(HumanoidBone::LeftUpperArm, Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0)));
        assert_eq!(nan, BindError::NotFinite { field: "bind.rotations.leftUpperArm".to_owned() });
        assert!(nan.to_string().contains("bind.rotations.leftUpperArm"), "{nan}");
        let infinite = bad(&|p| p.set(HumanoidBone::Head, Quat::from_xyzw(0.0, f32::INFINITY, 0.0, 1.0)));
        assert_eq!(field(&infinite), "bind.rotations.head");
        let long = bad(&|p| p.set(HumanoidBone::RightHand, Quat::from_xyzw(0.0, 0.0, 0.0, 2.0)));
        assert_eq!(long, BindError::NotNormalised { field: "bind.rotations.rightHand".to_owned() });
        let zero = bad(&|p| p.set(HumanoidBone::Spine, Quat::from_xyzw(0.0, 0.0, 0.0, 0.0)));
        assert_eq!(field(&zero), "bind.rotations.spine", "the zero quaternion is not a rotation");
        let hips = bad(&|p| p.hips_offset = Vec3::new(0.0, f32::NAN, 0.0));
        assert_eq!(hips, BindError::NotFinite { field: "bind.hips_offset".to_owned() });
        let first = bad(&|p| {
            p.set(HumanoidBone::RightHand, Quat::from_xyzw(0.0, 0.0, 0.0, 2.0));
            p.set(HumanoidBone::Hips, Quat::from_xyzw(0.0, 0.0, 0.0, 2.0));
        });
        assert_eq!(field(&first), "bind.rotations.hips", "bones are checked in the humanoid table's order");
    }

    #[test]
    fn an_identity_bind_is_left_out_of_the_file_and_a_file_without_one_is_bound_at_rest() {
        let rest = t_rig(&HumanoidPose::REST);
        let json = serde_json::to_value(&rest).unwrap();
        assert!(
            json.get("bind").is_none(),
            "no `bind` key, and so no `null` or empty object, for a character built in the T-pose"
        );
        assert_eq!(serde_json::from_value::<CharacterRig>(json).unwrap(), rest);

        let bound = t_rig(&a_pose());
        let json = serde_json::to_value(&bound).unwrap();
        let listed = json["bind"]["rotations"].as_object().unwrap();
        assert_eq!(listed.keys().collect::<Vec<_>>(), ["leftLowerArm", "leftUpperArm"], "only the bones it turns");
        assert_eq!(serde_json::from_value::<CharacterRig>(json).unwrap(), bound, "exact round trip");
    }

    /// A body whose joints are the T-pose layout turned by `bind` (the sculpted pose) and which publishes that pose.
    #[derive(Debug)]
    struct SculptedBody {
        bind: HumanoidPose,
    }

    impl BodyFit for SculptedBody {
        fn section(&self, _: BodyRegion, _: f32) -> Option<Section> {
            None
        }

        fn joint(&self, joint: &JointRef) -> Option<Vec3> {
            let JointRef::Humanoid(bone) = joint else { return None };
            let rig = t_rig(&self.bind);
            let mut world = Pose::IDENTITY;
            rig.skeleton.world_pose(&posed_in_t(&rig, &rig.bind), &mut world);
            rig.skeleton.humanoid_bone(*bone).map(|id| world.local[id.index()].translation)
        }

        fn bind(&self) -> HumanoidPose {
            self.bind
        }
    }

    #[test]
    fn a_published_bind_makes_the_rig_rest_in_the_t_and_pose_back_onto_the_joints_extra_bones_included() {
        let published = published_of(vec![Arc::new(SculptedBody { bind: a_pose() })]);
        let hand_at_bind = published.joint(&JointRef::Humanoid(HumanoidBone::LeftHand)).unwrap();
        assert!(hand_at_bind.y < 1.2, "the published joints hang: {hand_at_bind:?}");
        // A sleeve bone 5 cm below the sculpted hand, authored in bind space with a quarter turn of its own.
        let at = hand_at_bind - Vec3::new(0.0, 0.05, 0.0);
        let rest = Transform { translation: at, rotation: Quat::from_rotation_x(0.4) };
        let name = BoneName::new("sleeve_0").unwrap();
        let contribution = RigContribution {
            bones: vec![ExtraBone { name, parent: JointRef::Humanoid(HumanoidBone::LeftHand), rest }],
            ..RigContribution::default()
        };
        let part = PartId::new("sleeve").unwrap();
        let rig = assemble(&published, &[(part, contribution)]).unwrap();
        assert_eq!(rig.bind, a_pose());
        assert!(rig.validate_bind().is_ok());

        // At rest the arm is level; posed at bind, every bone is where the part published it.
        let id = |b: HumanoidBone| rig.skeleton.humanoid_bone(b).unwrap().index();
        let mut rest_world = Pose::IDENTITY;
        rig.skeleton.world_pose(&rig.skeleton.rest_pose(), &mut rest_world);
        let y = |b| rest_world.local[id(b)].translation.y;
        assert!((y(HumanoidBone::LeftLowerArm) - y(HumanoidBone::LeftUpperArm)).abs() < 1e-6);
        let ibm = rig.inverse_bind();
        for bone in HumanoidBone::ALL.into_iter().filter(|b| published.joint(&JointRef::Humanoid(*b)).is_some()) {
            let bound = ibm[id(bone)].inverse().transform_point3(Vec3::ZERO);
            let published = published.joint(&JointRef::Humanoid(bone)).unwrap();
            assert!((bound - published).length() < 1e-5, "{bone}: {bound:?} vs {published:?}");
        }
        let sleeve = rig.skeleton.find("sleeve_0").unwrap().index();
        let bound = ibm[sleeve].inverse();
        assert!((bound.transform_point3(Vec3::ZERO) - at).length() < 1e-5, "the sleeve sits where it was authored");
        let turn = Quat::from_mat4(&bound);
        assert!(turn.abs_diff_eq(rest.rotation, 1e-5) || turn.abs_diff_eq(-rest.rotation, 1e-5));
    }

    fn published_of(fits: Vec<Arc<dyn BodyFit>>) -> Published {
        let mut out = Publishes::default();
        for fit in fits {
            out.put::<FitFact>(fit);
        }
        let mut published = Published::default();
        published.absorb(&PartId::new("body").unwrap(), out).unwrap();
        published
    }

    fn publish(published: &mut Published, part: &str, bind: &HumanoidPose) -> Result<(), BuildError> {
        let output = Publishes::default().with::<FitFact>(Arc::new(SculptedBody { bind: *bind }));
        published.absorb(&PartId::new(part).unwrap(), output)
    }

    #[test]
    fn two_parts_bound_in_different_poses_are_a_build_error_naming_both() {
        let mut published = Published::default();
        publish(&mut published, "body", &a_pose()).unwrap();
        let mut other = a_pose();
        other.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(-0.9));
        let Err(BuildError::Part { part, message }) = publish(&mut published, "garment", &other) else {
            panic!("a second, different bind must be refused");
        };
        assert_eq!(part.as_str(), "garment");
        assert!(message.contains("'body'"), "{message}");
        assert_eq!(published.get::<FitFact>().fits.len(), 1, "nothing of the refused part is absorbed");
        assert_eq!(published.bind(), a_pose(), "and the first bind stands");
    }

    #[test]
    fn parts_that_agree_on_the_bind_or_are_in_the_t_pose_build() {
        let mut published = Published::default();
        publish(&mut published, "body", &a_pose()).unwrap();
        // The same pose, computed by another part with rounding noise under the tolerance (and a negated quaternion).
        let mut noisy = a_pose();
        let q = noisy.get(HumanoidBone::LeftUpperArm);
        noisy.set(HumanoidBone::LeftUpperArm, -Quat::from_xyzw(q.x + 5e-7, q.y, q.z - 5e-7, q.w));
        publish(&mut published, "garment", &noisy).unwrap();
        // A fit in the T-pose says nothing about the bind.
        publish(&mut published, "hair", &HumanoidPose::REST).unwrap();
        assert_eq!(published.bind(), a_pose());
        // Past the tolerance it is a different pose.
        let mut off = a_pose();
        let q = off.get(HumanoidBone::LeftUpperArm);
        off.set(HumanoidBone::LeftUpperArm, Quat::from_xyzw(q.x + 5e-6, q.y, q.z, q.w));
        assert!(publish(&mut published, "late", &off).is_err());
    }

    /// A body that publishes named joints only: a creature's root and the places its limbs start.
    #[derive(Debug)]
    struct CreatureBody(Vec<(&'static str, Vec3)>);

    impl BodyFit for CreatureBody {
        fn section(&self, _: BodyRegion, _: f32) -> Option<Section> {
            None
        }

        fn joint(&self, joint: &JointRef) -> Option<Vec3> {
            let JointRef::Named(name) = joint else { return None };
            self.0.iter().find(|(n, _)| *n == name.as_str()).map(|(_, at)| *at)
        }
    }

    /// A one-triangle mesh skinned wholly to `joint`, owned by the part `body`.
    fn skinned_to(joints: Vec<JointRef>) -> BuiltMesh {
        let geometry = crate::MeshData {
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
            normals: vec![Vec3::Z; 3],
            indices: vec![0, 1, 2],
            joints: vec![[0; 4]; 3],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
            ..crate::MeshData::default()
        };
        let mesh = crate::PartMesh {
            name: "skin".to_owned(),
            geometry,
            joint_bones: joints,
            material: crate::MaterialName::new("skin").unwrap(),
            casts_shadow: true,
            render_order: 0,
            dynamic: false,
        };
        BuiltMesh { part: PartId::new("body").unwrap(), local: 0, mesh }
    }

    fn named(name: &str) -> JointRef {
        JointRef::Named(BoneName::new(name).unwrap())
    }

    fn creature(joints: Vec<(&'static str, Vec3)>, bones: &[(&str, &str, Vec3)]) -> Result<CharacterRig, BuildError> {
        let published = published_of(vec![Arc::new(CreatureBody(joints))]);
        let bones = bones
            .iter()
            .map(|&(name, parent, at)| ExtraBone {
                name: BoneName::new(name).unwrap(),
                parent: named(parent),
                rest: Transform::from_translation(at),
            })
            .collect();
        let rig = RigContribution { bones, ..RigContribution::default() };
        assemble(&published, &[(PartId::new("body").unwrap(), rig)])
    }

    #[test]
    fn a_quadruped_and_a_blob_build_a_generic_skeleton_a_mesh_can_skin_to_by_name() {
        let quadruped = creature(
            vec![("root", Vec3::new(0.0, 0.5, 0.0))],
            &[
                ("root", "root", Vec3::new(0.0, 0.5, 0.0)),
                ("spine", "root", Vec3::new(0.0, 0.5, 0.3)),
                ("front_leg", "spine", Vec3::new(0.1, 0.25, 0.3)),
                ("tail", "root", Vec3::new(0.0, 0.5, -0.4)),
            ],
        );
        // `root` hangs from the published joint of the same name, so it is the one root; the rest follow it.
        let rig = quadruped.unwrap();
        assert!(rig.skeleton.humanoid_map().is_none() && rig.skeleton.len() == 4);
        assert_eq!(rig.bone(&JointRef::HIPS), None, "no humanoid roles to resolve");
        let leg = rig.bone(&named("front_leg")).unwrap();
        let bones = rig.skeleton.bones();
        assert_eq!(bones[leg.index()].parent, rig.bone(&named("spine")));
        assert!((bones[leg.index()].rest.translation - Vec3::new(0.1, -0.25, 0.0)).length() < 1e-6);
        // Posed at the rest, each bone is where it was authored: the inverse bind matrices put the leg at its place.
        let at = rig.inverse_bind()[leg.index()].inverse().transform_point3(Vec3::ZERO);
        assert!((at - Vec3::new(0.1, 0.25, 0.3)).length() < 1e-6, "{at:?}");

        let blob = creature(vec![("core", Vec3::Y)], &[("core", "core", Vec3::Y)]).unwrap();
        assert_eq!(blob.skeleton.len(), 1);
        assert_eq!(blob.bone(&named("core")), Some(BoneId(0)));
        let skinned = skinned_to(vec![named("core")]);
        assert_eq!(check_mesh_bones(&blob, &[skinned]), Ok(()));
    }

    #[test]
    fn a_creature_whose_bone_hangs_from_nothing_is_refused_naming_the_part_and_the_parent() {
        let Err(BuildError::MissingParentBone { part, bone, parent }) =
            creature(vec![("root", Vec3::ZERO)], &[("root", "root", Vec3::ZERO), ("wing", "shoulder", Vec3::X)])
        else {
            panic!("a bone with no parent must be refused");
        };
        assert_eq!((part.as_str(), bone.as_str(), parent), ("body", "wing", named("shoulder")));
    }

    #[test]
    fn a_humanoid_binding_on_a_creature_is_an_error_naming_the_part() {
        let bone =
            ExtraBone { name: BoneName::new("core").unwrap(), parent: JointRef::HEAD, rest: Transform::IDENTITY };
        let published = published_of(vec![Arc::new(CreatureBody(vec![]))]);
        let part = PartId::new("ears").unwrap();
        let rig = RigContribution { bones: vec![bone], ..RigContribution::default() };
        let Err(BuildError::MissingParentBone { part: blamed, .. }) = assemble(&published, &[(part, rig)]) else {
            panic!("a creature has no head to hang from");
        };
        assert_eq!(blamed.as_str(), "ears");
        // And a mesh skinned to a humanoid role is `UnknownMeshBone`, naming the part that owns the mesh.
        let skinned = skinned_to(vec![JointRef::HEAD]);
        let blob = creature(vec![("core", Vec3::Y)], &[("core", "core", Vec3::Y)]).unwrap();
        let Err(BuildError::UnknownMeshBone { part, bone, .. }) = check_mesh_bones(&blob, &[skinned]) else {
            panic!("a humanoid role is not a bone of a creature");
        };
        assert_eq!((part.as_str(), bone), ("body", JointRef::HEAD));
    }

    #[test]
    fn nothing_published_and_no_bones_still_reports_the_missing_hips() {
        let err = assemble(&Published::default(), &[]).unwrap_err();
        assert!(matches!(err, BuildError::Skeleton(SkeletonError::MissingRequired(HumanoidBone::Hips))), "{err:?}");
    }
}
