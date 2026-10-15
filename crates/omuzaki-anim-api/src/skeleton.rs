//! The skeleton and the humanoid bone set (spec RG-01, RG-06).
//!
//! Two levels, on purpose:
//! - [`HumanoidPose`] is **what animation produces**: one rotation per VRM humanoid bone, in the *normalised* space of
//!   VRM Animation: parent-local rotations where identity is the VRM T-pose. A pose in that space means the same thing
//!   on every character, which is what makes clips, mirrored poses, motion import (RG-08) and the AI's intents
//!   portable. It is `Copy` and about 900 bytes.
//! - [`Pose`] is **what the rig solves on one skeleton**: a local transform for every bone, including hair, cloth and
//!   accessory bones that animation never touches (spring bones and IK drive them). Fixed at [`MAX_BONES`], so it
//!   never allocates, but at 16 KiB it is passed by reference only.
//!
//! A [`Skeleton`] is the cold description both are measured against; [`Rig`] is the stage that turns a humanoid pose
//! into a solved pose for a skeleton.
//!
//! **Humanoid-ness is a property of a skeleton, never an assumption.** A skeleton is either a VRM humanoid
//! ([`Skeleton::new`], [`Skeleton::humanoid`]: the 15 required bones in the VRM hierarchy, with a [`HumanoidMap`] saying
//! which bone plays which role) or generic ([`Skeleton::generic`]: a quadruped, a winged or tailed creature, a blob;
//! no roles at all). [`Skeleton::humanoid_map`] answers which one it is, and everything that needs a humanoid
//! (VRM export, motion retargeting, the pose library) asks it and reports [`NotHumanoid`] otherwise. Everything that does
//! not (forward kinematics, skinning, spring bones) works on both alike.
//!
//! Sources: VRM 1.0 humanoid bone names and hierarchy
//! (<https://github.com/vrm-c/vrm-specification/blob/master/specification/VRMC_vrm-1.0/humanoid.md>); the T-pose rest
//! rule and normalised rotations come from VRM Animation
//! (<https://github.com/vrm-c/vrm-specification/tree/master/specification/VRMC_vrm_animation-1.0>). VRM 0.x called the
//! thumb bones Proximal, Intermediate, Distal; VRM 1.0 (and this crate) call them Metacarpal, Proximal, Distal, so the
//! 0.x importer shifts them by one.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::ops::Mul;

use omuzaki_core::glam::{Mat4, Quat, Vec3};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::blend::Bus;
use crate::channel::fnv_mix;
use crate::frame::Tick;
use crate::score::Cue;

/// How many humanoid bones VRM 1.0 defines.
pub const HUMANOID_COUNT: usize = 55;
/// The most bones a skeleton may have.
///
/// Why 512: the prototype's rig has about 50 bones (25 of them hair), and a hair-, outfit- and accessory-heavy
/// character with spring chains runs to a few hundred, so 512 leaves roughly ten times the prototype's size. It keeps
/// [`Pose`] at 16 KiB, which fits in L1 cache beside the other hot buffers, and it is well under the renderer's
/// 2048-joint limit, so the renderer never constrains a rig. Authoring should warn well before the limit; raising it
/// later is a one-constant change (the cost is `Pose` growing by 32 bytes per bone).
pub const MAX_BONES: usize = 512;

macro_rules! opt_bone {
    (none) => {
        None
    };
    ($b:ident) => {
        Some(HumanoidBone::$b)
    };
}

macro_rules! mirror_bone {
    ($me:ident, none) => {
        HumanoidBone::$me
    };
    ($me:ident, $other:ident) => {
        HumanoidBone::$other
    };
}

macro_rules! humanoid_bones {
    ($($var:ident = $idx:literal, $name:literal, $parent:ident, $required:literal, $mirror:ident;)*) => {
        /// A VRM 1.0 humanoid bone. The numbering is the order of [`HumanoidBone::ALL`] and is stable; every bone comes
        /// after its parent, so a loop over `ALL` is already a valid top-down traversal.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        #[non_exhaustive]
        pub enum HumanoidBone {
            $(
                #[doc = concat!("`", $name, "`.")]
                $var = $idx,
            )*
        }

        impl HumanoidBone {
            /// Every humanoid bone, parents before children.
            pub const ALL: [HumanoidBone; HUMANOID_COUNT] = [$(Self::$var),*];

            /// The VRM 1.0 name, e.g. `"leftUpperArm"`.
            pub const fn name(self) -> &'static str {
                match self { $(Self::$var => $name),* }
            }

            /// The bone's parent in the humanoid hierarchy (`None` for the hips). A skeleton may leave optional bones
            /// out; then the nearest present ancestor in this chain is the parent (VRM rule).
            pub const fn parent(self) -> Option<Self> {
                match self { $(Self::$var => opt_bone!($parent)),* }
            }

            /// True for the 15 bones every VRM must have.
            pub const fn is_required(self) -> bool {
                match self { $(Self::$var => $required),* }
            }

            /// The opposite-side bone (`leftHand` to `rightHand`); centre-line bones return themselves.
            #[must_use]
            pub const fn mirrored(self) -> Self {
                match self { $(Self::$var => mirror_bone!($var, $mirror)),* }
            }
        }
    };
}

humanoid_bones! {
    Hips = 0, "hips", none, true, none;
    Spine = 1, "spine", Hips, true, none;
    Chest = 2, "chest", Spine, false, none;
    UpperChest = 3, "upperChest", Chest, false, none;
    Neck = 4, "neck", UpperChest, false, none;
    Head = 5, "head", Neck, true, none;
    LeftEye = 6, "leftEye", Head, false, RightEye;
    RightEye = 7, "rightEye", Head, false, LeftEye;
    Jaw = 8, "jaw", Head, false, none;
    LeftUpperLeg = 9, "leftUpperLeg", Hips, true, RightUpperLeg;
    LeftLowerLeg = 10, "leftLowerLeg", LeftUpperLeg, true, RightLowerLeg;
    LeftFoot = 11, "leftFoot", LeftLowerLeg, true, RightFoot;
    LeftToes = 12, "leftToes", LeftFoot, false, RightToes;
    RightUpperLeg = 13, "rightUpperLeg", Hips, true, LeftUpperLeg;
    RightLowerLeg = 14, "rightLowerLeg", RightUpperLeg, true, LeftLowerLeg;
    RightFoot = 15, "rightFoot", RightLowerLeg, true, LeftFoot;
    RightToes = 16, "rightToes", RightFoot, false, LeftToes;
    LeftShoulder = 17, "leftShoulder", UpperChest, false, RightShoulder;
    LeftUpperArm = 18, "leftUpperArm", LeftShoulder, true, RightUpperArm;
    LeftLowerArm = 19, "leftLowerArm", LeftUpperArm, true, RightLowerArm;
    LeftHand = 20, "leftHand", LeftLowerArm, true, RightHand;
    RightShoulder = 21, "rightShoulder", UpperChest, false, LeftShoulder;
    RightUpperArm = 22, "rightUpperArm", RightShoulder, true, LeftUpperArm;
    RightLowerArm = 23, "rightLowerArm", RightUpperArm, true, LeftLowerArm;
    RightHand = 24, "rightHand", RightLowerArm, true, LeftHand;
    LeftThumbMetacarpal = 25, "leftThumbMetacarpal", LeftHand, false, RightThumbMetacarpal;
    LeftThumbProximal = 26, "leftThumbProximal", LeftThumbMetacarpal, false, RightThumbProximal;
    LeftThumbDistal = 27, "leftThumbDistal", LeftThumbProximal, false, RightThumbDistal;
    LeftIndexProximal = 28, "leftIndexProximal", LeftHand, false, RightIndexProximal;
    LeftIndexIntermediate = 29, "leftIndexIntermediate", LeftIndexProximal, false, RightIndexIntermediate;
    LeftIndexDistal = 30, "leftIndexDistal", LeftIndexIntermediate, false, RightIndexDistal;
    LeftMiddleProximal = 31, "leftMiddleProximal", LeftHand, false, RightMiddleProximal;
    LeftMiddleIntermediate = 32, "leftMiddleIntermediate", LeftMiddleProximal, false, RightMiddleIntermediate;
    LeftMiddleDistal = 33, "leftMiddleDistal", LeftMiddleIntermediate, false, RightMiddleDistal;
    LeftRingProximal = 34, "leftRingProximal", LeftHand, false, RightRingProximal;
    LeftRingIntermediate = 35, "leftRingIntermediate", LeftRingProximal, false, RightRingIntermediate;
    LeftRingDistal = 36, "leftRingDistal", LeftRingIntermediate, false, RightRingDistal;
    LeftLittleProximal = 37, "leftLittleProximal", LeftHand, false, RightLittleProximal;
    LeftLittleIntermediate = 38, "leftLittleIntermediate", LeftLittleProximal, false, RightLittleIntermediate;
    LeftLittleDistal = 39, "leftLittleDistal", LeftLittleIntermediate, false, RightLittleDistal;
    RightThumbMetacarpal = 40, "rightThumbMetacarpal", RightHand, false, LeftThumbMetacarpal;
    RightThumbProximal = 41, "rightThumbProximal", RightThumbMetacarpal, false, LeftThumbProximal;
    RightThumbDistal = 42, "rightThumbDistal", RightThumbProximal, false, LeftThumbDistal;
    RightIndexProximal = 43, "rightIndexProximal", RightHand, false, LeftIndexProximal;
    RightIndexIntermediate = 44, "rightIndexIntermediate", RightIndexProximal, false, LeftIndexIntermediate;
    RightIndexDistal = 45, "rightIndexDistal", RightIndexIntermediate, false, LeftIndexDistal;
    RightMiddleProximal = 46, "rightMiddleProximal", RightHand, false, LeftMiddleProximal;
    RightMiddleIntermediate = 47, "rightMiddleIntermediate", RightMiddleProximal, false, LeftMiddleIntermediate;
    RightMiddleDistal = 48, "rightMiddleDistal", RightMiddleIntermediate, false, LeftMiddleDistal;
    RightRingProximal = 49, "rightRingProximal", RightHand, false, LeftRingProximal;
    RightRingIntermediate = 50, "rightRingIntermediate", RightRingProximal, false, LeftRingIntermediate;
    RightRingDistal = 51, "rightRingDistal", RightRingIntermediate, false, LeftRingDistal;
    RightLittleProximal = 52, "rightLittleProximal", RightHand, false, LeftLittleProximal;
    RightLittleIntermediate = 53, "rightLittleIntermediate", RightLittleProximal, false, LeftLittleIntermediate;
    RightLittleDistal = 54, "rightLittleDistal", RightLittleIntermediate, false, LeftLittleDistal;
}

impl HumanoidBone {
    /// The index into a [`HumanoidPose`].
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The bone called `name` (VRM 1.0 spelling), if there is one.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name() == name)
    }
}

impl fmt::Display for HumanoidBone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl Serialize for HumanoidBone {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for HumanoidBone {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = <Cow<'de, str>>::deserialize(d)?;
        Self::from_name(&name).ok_or_else(|| serde::de::Error::custom(format!("unknown humanoid bone {name:?}")))
    }
}

impl JsonSchema for HumanoidBone {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "HumanoidBone".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let names: Vec<&str> = Self::ALL.iter().map(|b| b.name()).collect();
        json_schema!({ "type": "string", "enum": names, "description": "A VRM 1.0 humanoid bone name." })
    }
}

/// A rigid transform: translation then rotation. Bones carry no scale (VRM humanoids forbid non-uniform scale and
/// toon characters never need it), which keeps composition to a quaternion multiply.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Transform {
    /// Position, metres, in the parent's space.
    #[schemars(with = "[f32; 3]")]
    pub translation: Vec3,
    /// Rotation, in the parent's space.
    #[schemars(with = "[f32; 4]")]
    pub rotation: Quat,
}

impl Transform {
    /// No translation, no rotation.
    pub const IDENTITY: Self = Self { translation: Vec3::ZERO, rotation: Quat::IDENTITY };

    /// A pure translation.
    pub const fn from_translation(translation: Vec3) -> Self {
        Self { translation, rotation: Quat::IDENTITY }
    }

    /// The same transform as a matrix (rotate, then translate).
    pub fn to_mat4(self) -> Mat4 {
        Mat4::from_rotation_translation(self.rotation, self.translation)
    }
}

/// `parent * child`: the child's transform expressed in the parent's parent space.
impl Mul for Transform {
    type Output = Self;
    fn mul(self, child: Self) -> Self {
        Self {
            translation: self.translation + self.rotation * child.translation,
            rotation: self.rotation * child.rotation,
        }
    }
}

/// An animation pose in VRM normalised space: a rotation for every humanoid bone, plus where the hips are.
///
/// Identity everywhere (`REST`) is the T-pose. Fixed-size and `Copy`.
///
/// Serialised sparsely, as a pose file lists it: `{"hips_offset": [x, y, z], "rotations": {"leftUpperArm": [x, y, z, w]}}`
/// with only the bones that are not the identity, and nothing at all for the T-pose (`{}`). Reading checks the shape
/// (bone names, no unknown fields) but not the numbers; the owner of a pose that must be a valid rotation checks those.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "HumanoidPoseData", into = "HumanoidPoseData")]
pub struct HumanoidPose {
    /// Hips position relative to the rest hips, metres (walking, sitting, hopping).
    pub hips_offset: Vec3,
    /// Parent-local rotation of each bone, indexed by [`HumanoidBone::index`].
    pub rotations: [Quat; HUMANOID_COUNT],
}

impl HumanoidPose {
    /// The T-pose.
    pub const REST: Self = Self { hips_offset: Vec3::ZERO, rotations: [Quat::IDENTITY; HUMANOID_COUNT] };

    /// The rotation of `bone`.
    pub fn get(&self, bone: HumanoidBone) -> Quat {
        self.rotations[bone.index()]
    }

    /// Sets the rotation of `bone`.
    pub fn set(&mut self, bone: HumanoidBone, rotation: Quat) {
        self.rotations[bone.index()] = rotation;
    }

    /// Blends toward `other` by `weight` (`0` = self, `1` = other): spherical for rotations, linear for the hips.
    /// The primitive behind clip fades and pose blending (RG-06).
    #[must_use]
    pub fn lerp(&self, other: &Self, weight: f32) -> Self {
        // Exact at the ends: a fade at weight 0 or 1 must not disturb either pose (slerp renormalises).
        if weight <= 0.0 {
            return *self;
        }
        if weight >= 1.0 {
            return *other;
        }
        let mut out = *self;
        out.hips_offset = self.hips_offset.lerp(other.hips_offset, weight);
        for (o, (a, b)) in out.rotations.iter_mut().zip(self.rotations.iter().zip(&other.rotations)) {
            *o = a.slerp(*b, weight);
        }
        out
    }

    /// The pose reflected left to right: sides swap, and each rotation is reflected across the centre plane
    /// (`(x, y, z, w)` becomes `(x, -y, -z, w)`; valid because normalised bones share the T-pose's symmetric axes).
    #[must_use]
    pub fn mirrored(&self) -> Self {
        let mut out = Self::REST;
        out.hips_offset = Vec3::new(-self.hips_offset.x, self.hips_offset.y, self.hips_offset.z);
        for bone in HumanoidBone::ALL {
            let q = self.get(bone);
            out.set(bone.mirrored(), Quat::from_xyzw(q.x, -q.y, -q.z, q.w));
        }
        out
    }

    /// A stable digest, like [`ChannelBuf::checksum`](crate::ChannelBuf::checksum), for determinism checks.
    pub fn checksum(&self) -> u64 {
        let words = self.hips_offset.to_array().into_iter().chain(self.rotations.iter().flat_map(Quat::to_array));
        words.fold(0xcbf2_9ce4_8422_2325, |h, v| fnv_mix(h, v.to_bits()))
    }
}

impl Default for HumanoidPose {
    fn default() -> Self {
        Self::REST
    }
}

/// The serialised form of a [`HumanoidPose`]: 55 rotations are too many for serde's array support, and a pose that turns
/// a few bones should read as those few bones.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HumanoidPoseData {
    #[serde(default, skip_serializing_if = "is_zero")]
    hips_offset: Vec3,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    rotations: BTreeMap<HumanoidBone, Quat>,
}

fn is_zero(v: &Vec3) -> bool {
    *v == Vec3::ZERO
}

impl From<HumanoidPose> for HumanoidPoseData {
    fn from(pose: HumanoidPose) -> Self {
        let turned = HumanoidBone::ALL.into_iter().map(|b| (b, pose.get(b))).filter(|(_, q)| *q != Quat::IDENTITY);
        Self { hips_offset: pose.hips_offset, rotations: turned.collect() }
    }
}

impl From<HumanoidPoseData> for HumanoidPose {
    fn from(data: HumanoidPoseData) -> Self {
        let mut pose = Self { hips_offset: data.hips_offset, ..Self::REST };
        for (bone, rotation) in data.rotations {
            pose.set(bone, rotation);
        }
        pose
    }
}

/// A bone's index in its [`Skeleton`]. Parents always have lower ids than their children.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct BoneId(pub u16);

impl BoneId {
    /// The index into a [`Pose`].
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// One bone of a skeleton.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BoneDef {
    /// The bone's name (unique in the skeleton). Humanoid bones keep the importer's name; hair and accessory bones
    /// are named by the part that made them (`hair_clump_03_2`).
    pub name: String,
    /// The parent, or `None` for a root. Must be an earlier bone.
    pub parent: Option<BoneId>,
    /// The rest (bind) transform, relative to the parent.
    pub rest: Transform,
    /// Which humanoid bone this is, if any.
    #[serde(default)]
    pub humanoid: Option<HumanoidBone>,
}

/// Why a list of bones isn't a valid [`Skeleton`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SkeletonError {
    /// A generic skeleton needs at least one bone to skin to ([`Skeleton::new`] reports the missing hips instead).
    #[error("a skeleton needs at least one bone")]
    Empty,
    /// More than [`MAX_BONES`] bones.
    #[error("{count} bones is more than the limit of {MAX_BONES}")]
    TooManyBones {
        /// How many were given.
        count: usize,
    },
    /// A bone's parent isn't an earlier bone (parents must come first).
    #[error("bone {bone} ({name:?}) has parent {parent}, which isn't an earlier bone")]
    ParentNotEarlier {
        /// The bone's index.
        bone: usize,
        /// Its name.
        name: String,
        /// The parent it claims.
        parent: u16,
    },
    /// Two bones share a name.
    #[error("two bones are named {0:?}")]
    DuplicateName(String),
    /// Two bones claim the same humanoid role.
    #[error("{0} is assigned to two bones")]
    DuplicateHumanoid(HumanoidBone),
    /// A bone VRM requires is missing.
    #[error("the skeleton has no {0} bone, which VRM requires")]
    MissingRequired(HumanoidBone),
    /// A bone of a generic skeleton claims a humanoid role. Roles are all or nothing: a skeleton with any is built with
    /// [`Skeleton::new`] and must be a complete humanoid; one without is built with [`Skeleton::generic`].
    #[error("bone {bone} ({name:?}) is tagged {role}, but a generic skeleton has no humanoid roles")]
    UnexpectedHumanoidRole {
        /// The bone's index.
        bone: usize,
        /// Its name.
        name: String,
        /// The role it claims.
        role: HumanoidBone,
    },
    /// A humanoid bone's parent isn't the one the VRM hierarchy prescribes.
    #[error("{bone} should hang under {expected:?} but hangs under {found:?}")]
    WrongHumanoidParent {
        /// The bone.
        bone: HumanoidBone,
        /// The nearest present ancestor VRM prescribes.
        expected: Option<HumanoidBone>,
        /// The nearest humanoid ancestor the skeleton has.
        found: Option<HumanoidBone>,
    },
    /// A rest transform has NaN, infinite or non-unit-rotation values.
    #[error("bone {0:?} has a rest transform that isn't finite with a unit rotation")]
    BadTransform(String),
}

/// Which bone plays each VRM humanoid role in a skeleton that is a humanoid.
///
/// You only get one from [`Skeleton::humanoid_map`], so holding a `HumanoidMap` proves the skeleton passed the VRM rules:
/// all 15 required bones are present (so [`HumanoidMap::hips`] needs no `Option`) and the roles sit in the VRM hierarchy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HumanoidMap {
    hips: BoneId,
    bones: [Option<BoneId>; HUMANOID_COUNT],
}

impl HumanoidMap {
    /// Wraps a role table, checking the required bones. Roles are visited in [`HumanoidBone::ALL`] order, so the hips
    /// (the first) are reported before any other missing bone, as [`Skeleton::new`] always has.
    fn from_roles(bones: [Option<BoneId>; HUMANOID_COUNT]) -> Result<Self, SkeletonError> {
        let hips = bones[HumanoidBone::Hips.index()].ok_or(SkeletonError::MissingRequired(HumanoidBone::Hips))?;
        match HumanoidBone::ALL.into_iter().find(|b| b.is_required() && bones[b.index()].is_none()) {
            Some(missing) => Err(SkeletonError::MissingRequired(missing)),
            None => Ok(Self { hips, bones }),
        }
    }

    /// Where `bone` is, or `None` for an optional bone this skeleton leaves out (chest, toes, fingers).
    pub fn get(&self, bone: HumanoidBone) -> Option<BoneId> {
        self.bones[bone.index()]
    }

    /// The hips, which every humanoid has (VRM requires them).
    pub fn hips(&self) -> BoneId {
        self.hips
    }

    /// Every role the skeleton fills, with its bone, in [`HumanoidBone::ALL`] order (parents first).
    pub fn iter(&self) -> impl Iterator<Item = (HumanoidBone, BoneId)> + '_ {
        HumanoidBone::ALL.into_iter().filter_map(|b| self.get(b).map(|id| (b, id)))
    }
}

/// A consumer that needs a VRM humanoid (VRM export, motion retargeting, the pose library) was given a generic skeleton.
///
/// Returned by [`Skeleton::require_humanoid`]; wrap it with `#[from]` in the consumer's own error to say what it was
/// trying to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the skeleton is not a VRM humanoid (it was built with Skeleton::generic and has no humanoid bone map)")]
pub struct NotHumanoid;

/// The cold description of a rig's bones: names, hierarchy, rest transforms and, for a humanoid, which bone plays which
/// VRM role. Immutable once built, so it can be shared by reference across threads.
///
/// Built with [`Skeleton::new`] / [`Skeleton::humanoid`] (a VRM humanoid) or [`Skeleton::generic`] (anything else).
/// Deserializing a list of bones picks the constructor from the bones: any humanoid role means [`Skeleton::new`] (so a
/// humanoid that lost a required bone still fails to load), none means [`Skeleton::generic`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<BoneDef>", into = "Vec<BoneDef>")]
pub struct Skeleton {
    bones: Vec<BoneDef>,
    humanoid: Option<HumanoidMap>,
}

impl JsonSchema for Skeleton {
    fn schema_name() -> Cow<'static, str> {
        "Skeleton".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <Vec<BoneDef>>::json_schema(generator)
    }
}

impl TryFrom<Vec<BoneDef>> for Skeleton {
    type Error = SkeletonError;
    fn try_from(bones: Vec<BoneDef>) -> Result<Self, SkeletonError> {
        // Roles are all or nothing, so the bones say which rules apply; neither constructor is weakened by this.
        if bones.iter().any(|b| b.humanoid.is_some()) { Self::new(bones) } else { Self::generic(bones) }
    }
}

impl From<Skeleton> for Vec<BoneDef> {
    fn from(s: Skeleton) -> Self {
        s.bones
    }
}

/// The checks every skeleton passes, humanoid or not, in the order they have always run: for each bone in turn, the size
/// limit (first, once), parent order, unique name, finite unit-rotation rest transform, then `on_role` for a bone that
/// claims a humanoid role. Reporting the first bone's first failure keeps the error stable for a list with several faults.
fn scan_bones(
    bones: &[BoneDef],
    mut on_role: impl FnMut(usize, &BoneDef, HumanoidBone) -> Result<(), SkeletonError>,
) -> Result<(), SkeletonError> {
    if bones.len() > MAX_BONES {
        return Err(SkeletonError::TooManyBones { count: bones.len() });
    }
    let mut names = HashSet::new();
    for (i, bone) in bones.iter().enumerate() {
        if let Some(p) = bone.parent
            && p.index() >= i
        {
            return Err(SkeletonError::ParentNotEarlier { bone: i, name: bone.name.clone(), parent: p.0 });
        }
        if !names.insert(bone.name.as_str()) {
            return Err(SkeletonError::DuplicateName(bone.name.clone()));
        }
        let t = bone.rest;
        if !(t.translation.is_finite() && t.rotation.is_finite() && t.rotation.is_normalized()) {
            return Err(SkeletonError::BadTransform(bone.name.clone()));
        }
        if let Some(role) = bone.humanoid {
            on_role(i, bone, role)?;
        }
    }
    Ok(())
}

/// Every humanoid bone's nearest humanoid ancestor must be the one VRM prescribes (skipping absent optional bones).
fn check_humanoid_hierarchy(bones: &[BoneDef], map: &HumanoidMap) -> Result<(), SkeletonError> {
    for (bone, id) in map.iter() {
        let expected = std::iter::successors(bone.parent(), |p| p.parent()).find(|p| map.get(*p).is_some());
        let found = std::iter::successors(bones[id.index()].parent, |p| bones[p.index()].parent)
            .find_map(|p| bones[p.index()].humanoid);
        if expected != found {
            return Err(SkeletonError::WrongHumanoidParent { bone, expected, found });
        }
    }
    Ok(())
}

impl Skeleton {
    /// Validates `bones` as a VRM humanoid and builds the skeleton.
    ///
    /// # Errors
    /// A [`SkeletonError`] naming the first rule broken: size, parent order, unique names, finite rest transforms,
    /// unique humanoid roles, the 15 required bones, and the VRM hierarchy. For a creature that is not a humanoid use
    /// [`Skeleton::generic`].
    #[allow(clippy::cast_possible_truncation)] // bone indices are below MAX_BONES (checked first)
    pub fn new(bones: Vec<BoneDef>) -> Result<Self, SkeletonError> {
        let mut roles = [None; HUMANOID_COUNT];
        scan_bones(&bones, |i, _, role| match roles[role.index()].replace(BoneId(i as u16)) {
            Some(_) => Err(SkeletonError::DuplicateHumanoid(role)),
            None => Ok(()),
        })?;
        let map = HumanoidMap::from_roles(roles)?;
        check_humanoid_hierarchy(&bones, &map)?;
        Ok(Self { bones, humanoid: Some(map) })
    }

    /// Validates `bones` as a skeleton with no humanoid roles (a quadruped, a winged or tailed creature, a blob, a
    /// prop) and builds it. It keeps every rule that is not about VRM: at least one and at most [`MAX_BONES`] bones,
    /// parents before children, unique names, finite unit-rotation rest transforms. [`Skeleton::humanoid_map`] is
    /// `None` on the result, so humanoid-only consumers refuse it with [`NotHumanoid`]; forward kinematics, skinning
    /// and spring bones work as on any skeleton.
    ///
    /// # Errors
    /// A [`SkeletonError`] naming the first rule broken, including [`SkeletonError::UnexpectedHumanoidRole`] if a bone is
    /// tagged with a humanoid role (a skeleton with roles must be a complete humanoid: use [`Skeleton::new`]).
    pub fn generic(bones: Vec<BoneDef>) -> Result<Self, SkeletonError> {
        if bones.is_empty() {
            return Err(SkeletonError::Empty);
        }
        scan_bones(&bones, |bone, def, role| {
            Err(SkeletonError::UnexpectedHumanoidRole { bone, name: def.name.clone(), role })
        })?;
        Ok(Self { bones, humanoid: None })
    }

    /// Builds the standard VRM hierarchy for a chosen set of humanoid bones, with `rest` giving each one's rest
    /// transform. The template rig and tests start here; non-humanoid bones are added after with [`Skeleton::new`].
    ///
    /// # Errors
    /// As [`Skeleton::new`] (for example if a required bone is not in `bones`).
    #[allow(clippy::cast_possible_truncation)] // at most HUMANOID_COUNT bones
    pub fn humanoid(bones: &[HumanoidBone], rest: impl Fn(HumanoidBone) -> Transform) -> Result<Self, SkeletonError> {
        let mut present = [None; HUMANOID_COUNT];
        let mut defs = Vec::new();
        for bone in HumanoidBone::ALL.into_iter().filter(|b| bones.contains(b)) {
            let parent = std::iter::successors(bone.parent(), |p| p.parent()).find_map(|p| present[p.index()]);
            present[bone.index()] = Some(BoneId(defs.len() as u16));
            defs.push(BoneDef { name: bone.name().to_owned(), parent, rest: rest(bone), humanoid: Some(bone) });
        }
        Self::new(defs)
    }

    /// The bones, parents first.
    pub fn bones(&self) -> &[BoneDef] {
        &self.bones
    }

    /// How many bones.
    pub fn len(&self) -> usize {
        self.bones.len()
    }

    /// True if there are no bones (never true for a valid skeleton: both constructors need at least one).
    pub fn is_empty(&self) -> bool {
        self.bones.is_empty()
    }

    /// The bone with this id.
    pub fn get(&self, id: BoneId) -> Option<&BoneDef> {
        self.bones.get(id.index())
    }

    /// The bone called `name`.
    pub fn find(&self, name: &str) -> Option<BoneId> {
        #[allow(clippy::cast_possible_truncation)] // bone indices are below MAX_BONES
        self.bones.iter().position(|b| b.name == name).map(|i| BoneId(i as u16))
    }

    /// The humanoid roles of this skeleton, or `None` if it is generic ([`Skeleton::generic`]). The one place to ask
    /// "is this a humanoid?"; code that cannot work without one should call [`Skeleton::require_humanoid`] instead.
    pub fn humanoid_map(&self) -> Option<&HumanoidMap> {
        self.humanoid.as_ref()
    }

    /// Whether this skeleton is a VRM humanoid (it has a [`HumanoidMap`]).
    pub fn is_humanoid(&self) -> bool {
        self.humanoid.is_some()
    }

    /// The humanoid roles, or [`NotHumanoid`] for a generic skeleton: the gate for VRM export, motion retargeting and
    /// the pose library, which have no meaning without a humanoid.
    ///
    /// # Errors
    /// [`NotHumanoid`] if the skeleton was built with [`Skeleton::generic`].
    pub fn require_humanoid(&self) -> Result<&HumanoidMap, NotHumanoid> {
        self.humanoid.as_ref().ok_or(NotHumanoid)
    }

    /// Where a humanoid bone is in this skeleton, if it has it. Always `None` on a generic skeleton, so lookups by role
    /// (`JointRef::Humanoid`) fail soft there; use [`Skeleton::require_humanoid`] to turn that into an error.
    pub fn humanoid_bone(&self, bone: HumanoidBone) -> Option<BoneId> {
        self.humanoid.as_ref().and_then(|map| map.get(bone))
    }

    /// Whether `ancestor` is `bone` or one of its ancestors (used to validate spring-bone centres).
    pub fn is_ancestor_or_self(&self, ancestor: BoneId, bone: BoneId) -> bool {
        std::iter::successors(Some(bone), |b| self.bones.get(b.index()).and_then(|d| d.parent)).any(|b| b == ancestor)
    }

    /// The rest pose as a [`Pose`] (cold: loops the bones).
    pub fn rest_pose(&self) -> Pose {
        let mut pose = Pose::IDENTITY;
        for (slot, bone) in pose.local.iter_mut().zip(&self.bones) {
            *slot = bone.rest;
        }
        pose
    }

    /// The inverse bind matrices of a mesh bound with the bones at `bind`: for each bone, the inverse of its `bind`
    /// transform in skeleton space. Computed once per character (cold); [`Skeleton::skin_matrices`] needs them every
    /// frame.
    ///
    /// The mesh and the skeleton's rest need not agree: a character sculpted in an A-pose rests in the VRM T-pose
    /// (identity is the T-pose, [`HumanoidPose`]) and passes its sculpted pose here, so posing the bones at `bind`
    /// reproduces the mesh exactly and posing them at rest turns the arms to the T (decision 0014).
    pub fn inverse_bind_for(&self, bind: &Pose) -> Vec<Mat4> {
        let mut world = Pose::IDENTITY;
        self.world_pose(bind, &mut world);
        world.local.iter().take(self.len()).map(|t| t.to_mat4().inverse()).collect()
    }

    /// The inverse bind matrices when the mesh is bound at rest: [`Skeleton::inverse_bind_for`] the
    /// [rest pose](Skeleton::rest_pose). The default for a mesh built in the rest pose.
    pub fn inverse_bind(&self) -> Vec<Mat4> {
        self.inverse_bind_for(&self.rest_pose())
    }

    /// The skinning matrices the renderer takes (`omuzaki-render-api`'s `Scene::joints`): for each bone,
    /// `world(bone) * inverse_bind(bone)`, bind space to model space. Hot path: `world` is scratch space, `out` is
    /// filled for as many bones as it has room for.
    pub fn skin_matrices(&self, local: &Pose, inverse_bind: &[Mat4], world: &mut Pose, out: &mut [Mat4]) {
        self.world_pose(local, world);
        for ((slot, w), ib) in out.iter_mut().zip(&world.local[..self.len()]).zip(inverse_bind) {
            *slot = w.to_mat4() * *ib;
        }
    }

    /// Forward kinematics: fills `out` with every bone's transform relative to the skeleton root, from the local
    /// transforms in `local`. One pass, no allocation, because parents come first. Used by spring bones, IK and
    /// skinning, so the traversal exists once.
    pub fn world_pose(&self, local: &Pose, out: &mut Pose) {
        for (i, bone) in self.bones.iter().enumerate() {
            out.local[i] = match bone.parent {
                Some(p) => out.local[p.index()] * local.local[i],
                None => local.local[i],
            };
        }
    }
}

/// Local transform of every bone of one skeleton, indexed by [`BoneId`]. Entries past the skeleton's length are unused.
#[derive(Clone, PartialEq)]
pub struct Pose {
    /// Parent-relative transforms (or, after [`Skeleton::world_pose`], root-relative ones).
    pub local: [Transform; MAX_BONES],
}

impl Pose {
    /// Every bone at the identity transform.
    pub const IDENTITY: Self = Self { local: [Transform::IDENTITY; MAX_BONES] };
}

impl Default for Pose {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl fmt::Debug for Pose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pose({MAX_BONES} slots)")
    }
}

/// The stage that turns animation into a solved skeleton pose: normalised humanoid rotations to this skeleton's
/// local rotations, then IK, then spring bones (spec §8 order). One instance per character.
pub trait Rig: Send {
    /// The skeleton this rig drives.
    fn skeleton(&self) -> &Skeleton;

    /// Solves one frame. `out` receives the local transform of every bone in [`Rig::skeleton`]: humanoid bones from
    /// `pose` (applied on top of the rest pose), hair, cloth and accessory bones from the spring solver, everything
    /// else at rest. An implementation may also run a node-constraint stage between IK and the springs, set by its own
    /// builder (`FullRig::with_constraints` in `omuzaki-rig`): a constrained bone then takes its constraint's rotation.
    /// This is the VRM 1.0 order, in which `UniVRM` applies `VRMC_node_constraint` after the humanoid pose and before the
    /// spring bones. On a generic skeleton there are no humanoid bones, so `pose` is ignored and only the spring bones
    /// and any constrained bones move. Hot path: no allocation, no locking.
    fn solve(&mut self, tick: Tick, pose: &HumanoidPose, out: &mut Pose);

    /// Clears physics state (springs at rest, as after a teleport or a scrub).
    fn reset(&mut self);
}

/// The "anim blend" stage: turns the blended bus (and the cues that fired this frame) into a humanoid pose.
///
/// Reference behaviour from the prototype: the bus head angles are shared between neck and head (35% and 65%), `lean`
/// bends the spine forward, `breath` lifts the chest, `bodyTurn` rotates the hips, clips from cues play on top of
/// procedural idle. Clip data (the pose library, RG-06) arrives with that feature; this is the interface it plugs into.
pub trait Animator: Send {
    /// Layers this frame's pose onto `out`. Hot path.
    ///
    /// `out` arrives holding the **base pose**, the character's bind (decision 0014 §4): the caller seeds it, and the
    /// animator layers onto it, so bones it does not drive stay as given and the stage with no animation shows the
    /// character as sculpted. The meaning of "layers onto" is pinned by [`PoseLayers::resolve`](crate::PoseLayers::resolve);
    /// an animator must not overwrite `out` wholesale.
    fn animate(&mut self, tick: Tick, bus: &Bus, cues: &[Cue], out: &mut HumanoidPose);

    /// Forgets playing clips and filter state.
    fn reset(&mut self);
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // tests compare values that are copied or exactly computed, and cast small loop counters
mod tests {
    use super::*;

    fn rest_of(bone: HumanoidBone) -> Transform {
        Transform::from_translation(Vec3::new(0.0, 0.1 * (bone.index() % 5) as f32, 0.0))
    }

    #[test]
    fn bone_table_matches_vrm_1_0() {
        assert_eq!(HumanoidBone::ALL.len(), 55);
        assert_eq!(HumanoidBone::ALL.iter().filter(|b| b.is_required()).count(), 15);
        assert_eq!(HumanoidBone::Hips.parent(), None);
        assert_eq!(HumanoidBone::Head.parent(), Some(HumanoidBone::Neck));
        assert_eq!(HumanoidBone::LeftUpperArm.parent(), Some(HumanoidBone::LeftShoulder));
        assert_eq!(HumanoidBone::LeftThumbProximal.parent(), Some(HumanoidBone::LeftThumbMetacarpal));
        assert_eq!(HumanoidBone::RightLittleDistal.name(), "rightLittleDistal");
        assert!(HumanoidBone::ALL.iter().enumerate().all(|(i, b)| b.index() == i));
    }

    #[test]
    fn parents_come_first_and_mirroring_is_an_involution() {
        for bone in HumanoidBone::ALL {
            if let Some(p) = bone.parent() {
                assert!(p.index() < bone.index(), "{bone}");
            }
            assert_eq!(bone.mirrored().mirrored(), bone);
            assert_eq!(bone.name().starts_with("left"), bone.mirrored().name().starts_with("right"));
            assert_eq!(HumanoidBone::from_name(bone.name()), Some(bone));
        }
        assert_eq!(HumanoidBone::Spine.mirrored(), HumanoidBone::Spine);
    }

    #[test]
    fn required_bones_alone_make_a_valid_skeleton() {
        let required: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect();
        let s = Skeleton::humanoid(&required, rest_of).unwrap();
        assert_eq!(s.len(), 15);
        // With chest/neck/shoulders absent, the head hangs directly under the spine and arms under the spine.
        let head = s.get(s.humanoid_bone(HumanoidBone::Head).unwrap()).unwrap();
        assert_eq!(head.parent, s.humanoid_bone(HumanoidBone::Spine));
        let arm = s.get(s.humanoid_bone(HumanoidBone::LeftUpperArm).unwrap()).unwrap();
        assert_eq!(arm.parent, s.humanoid_bone(HumanoidBone::Spine));
    }

    #[test]
    fn validation_names_the_rule_that_failed() {
        let all = Skeleton::humanoid(&HumanoidBone::ALL, rest_of).unwrap();
        assert_eq!(all.len(), 55);
        let mut bones = all.bones().to_vec();

        let without_hand: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| *b != HumanoidBone::LeftHand).collect();
        assert!(matches!(
            Skeleton::humanoid(&without_hand, rest_of),
            Err(SkeletonError::MissingRequired(HumanoidBone::LeftHand))
        ));

        let mut dup = bones.clone();
        dup[3].name = dup[2].name.clone();
        assert!(matches!(Skeleton::new(dup), Err(SkeletonError::DuplicateName(_))));

        let mut twice = bones.clone();
        twice[3].humanoid = twice[2].humanoid;
        assert!(matches!(Skeleton::new(twice), Err(SkeletonError::DuplicateHumanoid(_))));

        let mut bad_parent = bones.clone();
        bad_parent[1].parent = Some(BoneId(5));
        assert!(matches!(Skeleton::new(bad_parent), Err(SkeletonError::ParentNotEarlier { bone: 1, .. })));

        let mut wrong = bones.clone();
        let (head, spine) =
            (all.humanoid_bone(HumanoidBone::Head).unwrap(), all.humanoid_bone(HumanoidBone::Spine).unwrap());
        wrong[head.index()].parent = Some(spine);
        assert!(matches!(
            Skeleton::new(wrong),
            Err(SkeletonError::WrongHumanoidParent { bone: HumanoidBone::Head, .. })
        ));

        bones[0].rest.rotation = Quat::from_xyzw(0.0, 0.0, 0.0, 2.0);
        assert!(matches!(Skeleton::new(bones), Err(SkeletonError::BadTransform(_))));
    }

    #[test]
    fn extra_bones_are_allowed_and_found_by_name() {
        let base = Skeleton::humanoid(&HumanoidBone::ALL, rest_of).unwrap();
        let head = base.humanoid_bone(HumanoidBone::Head).unwrap();
        let mut bones = base.bones().to_vec();
        bones.push(BoneDef { name: "hair_0".into(), parent: Some(head), rest: Transform::IDENTITY, humanoid: None });
        let s = Skeleton::new(bones).unwrap();
        let hair = s.find("hair_0").unwrap();
        assert_eq!(hair.index(), 55);
        assert!(s.is_ancestor_or_self(head, hair) && !s.is_ancestor_or_self(hair, head));
        assert_eq!(s.find("nope"), None);
    }

    #[test]
    fn forward_kinematics_composes_parent_first() {
        let s = Skeleton::humanoid(&HumanoidBone::ALL, |_| Transform::from_translation(Vec3::Y)).unwrap();
        let rest = s.rest_pose();
        let mut world = Pose::IDENTITY;
        s.world_pose(&rest, &mut world);
        let id = |b| s.humanoid_bone(b).unwrap().index();
        assert_eq!(world.local[id(HumanoidBone::Hips)].translation, Vec3::Y);
        assert_eq!(world.local[id(HumanoidBone::Spine)].translation, 2.0 * Vec3::Y);
        // Rotating the hips 90 degrees about Z swings everything above it sideways.
        let mut posed = rest.clone();
        posed.local[id(HumanoidBone::Hips)].rotation = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        s.world_pose(&posed, &mut world);
        let spine = world.local[id(HumanoidBone::Spine)].translation;
        assert!((spine - Vec3::new(-1.0, 1.0, 0.0)).length() < 1e-5, "{spine:?}");
    }

    #[test]
    fn skinning_matrices_are_identity_at_rest_and_follow_the_pose() {
        let s = Skeleton::humanoid(&HumanoidBone::ALL, |_| Transform::from_translation(Vec3::Y)).unwrap();
        let inverse_bind = s.inverse_bind();
        assert_eq!(inverse_bind.len(), s.len());
        let (mut world, mut skin) = (Pose::IDENTITY, vec![Mat4::IDENTITY; s.len()]);
        s.skin_matrices(&s.rest_pose(), &inverse_bind, &mut world, &mut skin);
        assert!(skin.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-5)), "no pose, no skinning");

        // Turn the hips 90 degrees about Z: a vertex bound at the spine's rest position (0, 2, 0) swings to (-1, 1, 0).
        let mut posed = s.rest_pose();
        let hips = s.humanoid_bone(HumanoidBone::Hips).unwrap();
        posed.local[hips.index()].rotation = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        s.skin_matrices(&posed, &inverse_bind, &mut world, &mut skin);
        let spine = s.humanoid_bone(HumanoidBone::Spine).unwrap();
        let moved = skin[spine.index()].transform_point3(Vec3::new(0.0, 2.0, 0.0));
        assert!((moved - Vec3::new(-1.0, 1.0, 0.0)).length() < 1e-5, "{moved:?}");
        // A short output buffer is filled as far as it goes and nothing panics.
        let mut short = [Mat4::ZERO; 3];
        s.skin_matrices(&posed, &inverse_bind, &mut world, &mut short);
        assert!(short.iter().all(|m| *m != Mat4::ZERO));
    }

    #[test]
    fn pose_blend_endpoints_and_mirror() {
        let mut a = HumanoidPose::REST;
        a.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(0.8));
        a.hips_offset = Vec3::new(0.2, 0.0, 0.0);
        assert_eq!(a.lerp(&HumanoidPose::REST, 0.0), a);
        assert!(a.lerp(&HumanoidPose::REST, 1.0).get(HumanoidBone::LeftUpperArm).angle_between(Quat::IDENTITY) < 1e-5);
        let half = a.lerp(&HumanoidPose::REST, 0.5);
        assert!((half.get(HumanoidBone::LeftUpperArm).angle_between(Quat::IDENTITY) - 0.4).abs() < 1e-4);

        let m = a.mirrored();
        assert_eq!(m.hips_offset.x, -0.2);
        let q = m.get(HumanoidBone::RightUpperArm);
        assert!(
            (q.angle_between(Quat::from_rotation_z(-0.8))) < 1e-5,
            "raising the left arm mirrors to raising the right"
        );
        assert_eq!(m.get(HumanoidBone::LeftUpperArm), Quat::IDENTITY);
        assert!(m.mirrored().rotations.iter().zip(&a.rotations).all(|(x, y)| x.angle_between(*y) < 1e-5));
    }

    #[test]
    fn checksum_distinguishes_poses() {
        let mut a = HumanoidPose::REST;
        let rest = a.checksum();
        a.set(HumanoidBone::Head, Quat::from_rotation_y(0.1));
        assert_ne!(a.checksum(), rest);
    }

    #[test]
    fn skeleton_serde_is_a_list_of_bones_and_revalidates() {
        let s = Skeleton::humanoid(&HumanoidBone::ALL, rest_of).unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.starts_with("[{\"name\":\"hips\""), "{}", &json[..40]);
        assert_eq!(serde_json::from_str::<Skeleton>(&json).unwrap(), s);
        assert!(serde_json::from_str::<Skeleton>("[]").is_err(), "an empty list is not a skeleton");
    }

    #[test]
    fn generic_skeleton_serde_round_trips_as_generic() {
        let s = Skeleton::generic(quadruped()).unwrap();
        let json = serde_json::to_string(&s).unwrap();
        let back = serde_json::from_str::<Skeleton>(&json).unwrap();
        assert_eq!(back, s);
        assert!(!back.is_humanoid(), "no bone claims a role, so the list loads as generic");
        // A list that claims roles is held to the VRM rules: losing a required bone is still an error on load.
        let mut humanoid = Skeleton::humanoid(&HumanoidBone::ALL, rest_of).unwrap().bones().to_vec();
        humanoid.retain(|b| b.humanoid != Some(HumanoidBone::LeftHand));
        assert!(Skeleton::try_from(humanoid).is_err());
    }

    /// A small four-legged creature: body, neck, head, a two-bone tail and four legs. No humanoid roles.
    fn quadruped() -> Vec<BoneDef> {
        let bone = |name: &str, parent: Option<u16>, at: Vec3| BoneDef {
            name: name.into(),
            parent: parent.map(BoneId),
            rest: Transform::from_translation(at),
            humanoid: None,
        };
        vec![
            bone("body", None, Vec3::new(0.0, 0.5, 0.0)),
            bone("neck", Some(0), Vec3::new(0.0, 0.1, 0.3)),
            bone("head", Some(1), Vec3::new(0.0, 0.1, 0.1)),
            bone("tail_0", Some(0), Vec3::new(0.0, 0.05, -0.3)),
            bone("tail_1", Some(3), Vec3::new(0.0, 0.0, -0.2)),
            bone("leg_fl", Some(0), Vec3::new(0.1, -0.1, 0.25)),
            bone("leg_fr", Some(0), Vec3::new(-0.1, -0.1, 0.25)),
            bone("leg_bl", Some(0), Vec3::new(0.1, -0.1, -0.25)),
            bone("leg_br", Some(0), Vec3::new(-0.1, -0.1, -0.25)),
        ]
    }

    #[test]
    fn a_generic_skeleton_needs_no_humanoid_bones() {
        let s = Skeleton::generic(quadruped()).unwrap();
        assert_eq!(s.len(), 9);
        assert!(!s.is_empty());
        assert!(!s.is_humanoid());
        assert_eq!(s.humanoid_map(), None);
        assert_eq!(s.require_humanoid(), Err(NotHumanoid));
        assert!(HumanoidBone::ALL.into_iter().all(|b| s.humanoid_bone(b).is_none()), "no role is filled");
        assert_eq!(s.find("tail_1"), Some(BoneId(4)));
        assert!(s.is_ancestor_or_self(BoneId(0), BoneId(4)));
    }

    #[test]
    fn generic_keeps_every_rule_that_is_not_about_vrm() {
        assert_eq!(Skeleton::generic(Vec::new()), Err(SkeletonError::Empty));

        let root = |i: usize| BoneDef {
            name: format!("b{i}"),
            parent: i.checked_sub(1).map(|p| BoneId(u16::try_from(p).unwrap())),
            rest: Transform::IDENTITY,
            humanoid: None,
        };
        let chain = |n: usize| (0..n).map(root).collect::<Vec<_>>();
        assert_eq!(Skeleton::generic(chain(MAX_BONES)).unwrap().len(), MAX_BONES);
        assert_eq!(Skeleton::generic(chain(MAX_BONES + 1)), Err(SkeletonError::TooManyBones { count: MAX_BONES + 1 }));

        let mut dup = quadruped();
        dup[2].name = dup[1].name.clone();
        assert_eq!(Skeleton::generic(dup), Err(SkeletonError::DuplicateName("neck".into())));

        let mut later_parent = quadruped();
        later_parent[1].parent = Some(BoneId(5));
        assert!(matches!(Skeleton::generic(later_parent), Err(SkeletonError::ParentNotEarlier { bone: 1, .. })));

        let mut self_parent = quadruped();
        self_parent[2].parent = Some(BoneId(2));
        assert!(matches!(Skeleton::generic(self_parent), Err(SkeletonError::ParentNotEarlier { bone: 2, .. })));

        let mut not_unit = quadruped();
        not_unit[0].rest.rotation = Quat::from_xyzw(0.0, 0.0, 0.0, 2.0);
        assert_eq!(Skeleton::generic(not_unit), Err(SkeletonError::BadTransform("body".into())));

        let mut not_finite = quadruped();
        not_finite[3].rest.translation.y = f32::NAN;
        assert_eq!(Skeleton::generic(not_finite), Err(SkeletonError::BadTransform("tail_0".into())));
    }

    #[test]
    fn roles_are_all_or_nothing() {
        // A role on a generic skeleton is refused, with the bone and role named.
        let mut tagged = quadruped();
        tagged[2].humanoid = Some(HumanoidBone::Head);
        assert_eq!(
            Skeleton::generic(tagged.clone()),
            Err(SkeletonError::UnexpectedHumanoidRole { bone: 2, name: "head".into(), role: HumanoidBone::Head })
        );
        // The humanoid constructor is not weakened: without the 15 required bones it still refuses, tagged or not.
        assert_eq!(Skeleton::new(quadruped()), Err(SkeletonError::MissingRequired(HumanoidBone::Hips)));
        assert_eq!(Skeleton::new(tagged.clone()), Err(SkeletonError::MissingRequired(HumanoidBone::Hips)));
        // `TryFrom` (and so serde) lets the bones pick the constructor.
        assert!(!Skeleton::try_from(quadruped()).unwrap().is_humanoid());
        assert_eq!(Skeleton::try_from(tagged), Err(SkeletonError::MissingRequired(HumanoidBone::Hips)));
        let humanoid = Skeleton::humanoid(&HumanoidBone::ALL, rest_of).unwrap();
        assert_eq!(Skeleton::try_from(humanoid.bones().to_vec()).unwrap(), humanoid);
    }

    #[test]
    fn a_humanoid_knows_its_roles() {
        let all = Skeleton::humanoid(&HumanoidBone::ALL, rest_of).unwrap();
        assert!(all.is_humanoid());
        let map = all.require_humanoid().unwrap();
        assert_eq!(all.humanoid_map(), Some(map));
        assert_eq!(map.hips(), BoneId(0));
        assert_eq!(map.get(HumanoidBone::Hips), Some(map.hips()));
        for bone in HumanoidBone::ALL {
            assert_eq!(map.get(bone), all.humanoid_bone(bone));
        }
        assert!(map.iter().map(|(b, _)| b).eq(HumanoidBone::ALL), "every role, in the table's order");

        let required: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect();
        let small = Skeleton::humanoid(&required, rest_of).unwrap();
        let map = small.require_humanoid().unwrap();
        assert_eq!(map.iter().count(), 15);
        assert_eq!(map.get(HumanoidBone::Chest), None, "optional bones may be absent; required ones may not");
        assert!(map.iter().all(|(b, id)| small.get(id).and_then(|d| d.humanoid) == Some(b)));
    }

    #[test]
    fn a_generic_skeleton_poses_and_skins_like_any_other() {
        let s = Skeleton::generic(quadruped()).unwrap();
        let inverse_bind = s.inverse_bind();
        assert_eq!(inverse_bind.len(), 9);
        let (mut world, mut skin) = (Pose::IDENTITY, vec![Mat4::ZERO; s.len()]);
        s.skin_matrices(&s.rest_pose(), &inverse_bind, &mut world, &mut skin);
        assert!(skin.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-5)), "no pose, no skinning");

        // Swing the body 90 degrees about Z: the tail tip, bound 0.5 behind the body origin, swings with it.
        let mut posed = s.rest_pose();
        posed.local[0].rotation = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        s.skin_matrices(&posed, &inverse_bind, &mut world, &mut skin);
        let tail = s.find("tail_1").unwrap();
        let bound = Vec3::new(0.0, 0.55, -0.5); // tail_1's rest position: body (0, 0.5, 0) + tail_0 + tail_1
        let moved = skin[tail.index()].transform_point3(bound);
        assert!((moved - Vec3::new(-0.05, 0.5, -0.5)).length() < 1e-5, "{moved:?}");
        let leg = s.find("leg_fl").unwrap();
        assert!(skin[leg.index()].abs_diff_eq(skin[0], 1e-5), "a leg is carried by the body like the tail");
    }

    #[test]
    fn binding_at_rest_is_the_inverse_bind_matrices_there_have_always_been_bit_for_bit() {
        // A rest with turned bones, so the inverse is not just a negated translation.
        let s = Skeleton::humanoid(&HumanoidBone::ALL, |bone| Transform {
            translation: Vec3::new(0.01 * bone.index() as f32, 0.1, -0.02),
            rotation: Quat::from_euler(omuzaki_core::glam::EulerRot::XYZ, 0.3, -0.2 * bone.index() as f32, 0.1),
        })
        .unwrap();
        // The definition `inverse_bind` has had since it was written: forward kinematics of the rest pose, inverted.
        let mut world = Pose::IDENTITY;
        s.world_pose(&s.rest_pose(), &mut world);
        let today: Vec<Mat4> = world.local[..s.len()].iter().map(|t| t.to_mat4().inverse()).collect();
        let bits = |m: &[Mat4]| m.iter().flat_map(Mat4::to_cols_array).map(f32::to_bits).collect::<Vec<_>>();
        assert_eq!(bits(&s.inverse_bind()), bits(&today));
        assert_eq!(bits(&s.inverse_bind_for(&s.rest_pose())), bits(&today));
    }

    /// A shoulder, an arm and a forearm laid out along +X in the rest (a T), and the same arm turned 60 degrees down.
    fn arm_in_t_and_a() -> (Skeleton, Pose) {
        let bone = |name: &str, parent: Option<u16>, x: f32| BoneDef {
            name: name.into(),
            parent: parent.map(BoneId),
            rest: Transform::from_translation(Vec3::new(x, 0.0, 0.0)),
            humanoid: None,
        };
        let s = Skeleton::generic(vec![
            bone("shoulder", None, 0.0),
            bone("arm", Some(0), 0.2),
            bone("forearm", Some(1), 0.3),
        ])
        .unwrap();
        let mut bind = s.rest_pose();
        bind.local[1].rotation = Quat::from_rotation_z(-std::f32::consts::FRAC_PI_3);
        (s, bind)
    }

    #[test]
    fn a_mesh_bound_in_another_pose_skins_to_itself_there_and_to_the_rest_pose_at_rest() {
        let (s, bind) = arm_in_t_and_a();
        let inverse_bind = s.inverse_bind_for(&bind);
        assert_ne!(inverse_bind, s.inverse_bind(), "binding in another pose changes the matrices");
        // A vertex of the forearm, built where the sculpted (bound) arm has it.
        let mut world = Pose::IDENTITY;
        s.world_pose(&bind, &mut world);
        let built = world.local[2].to_mat4().transform_point3(Vec3::new(0.1, 0.0, 0.0));
        assert!(built.y < -0.2, "the sculpted forearm hangs down: {built:?}");

        let mut skin = vec![Mat4::ZERO; s.len()];
        s.skin_matrices(&bind, &inverse_bind, &mut world, &mut skin);
        let at_bind = skin[2].transform_point3(built);
        assert!((at_bind - built).length() < 1e-6, "posed at its bind pose the mesh is as built: {at_bind:?}");

        // Posed at rest, the same vertex lands on the forearm's rest position: along +X, level with the shoulder.
        s.skin_matrices(&s.rest_pose(), &inverse_bind, &mut world, &mut skin);
        let at_rest = skin[2].transform_point3(built);
        assert!((at_rest - Vec3::new(0.6, 0.0, 0.0)).length() < 1e-6, "{at_rest:?}");
    }

    #[test]
    fn a_humanoid_pose_serialises_only_the_bones_it_turns() {
        assert_eq!(serde_json::to_string(&HumanoidPose::REST).unwrap(), "{}", "the T-pose is empty");
        assert_eq!(serde_json::from_str::<HumanoidPose>("{}").unwrap(), HumanoidPose::REST);

        let mut pose = HumanoidPose::REST;
        pose.hips_offset = Vec3::new(0.0, -0.25, 0.5);
        pose.set(HumanoidBone::RightUpperArm, Quat::from_rotation_z(-1.0));
        pose.set(HumanoidBone::Head, Quat::from_rotation_y(0.3));
        let json = serde_json::to_string(&pose).unwrap();
        assert!(json.starts_with(r#"{"hips_offset":[0.0,-0.25,0.5],"rotations":{"head":["#), "{json}");
        assert_eq!(json.matches("Upper").count(), 1, "only the two turned bones are listed: {json}");
        assert_eq!(serde_json::from_str::<HumanoidPose>(&json).unwrap(), pose, "exact round trip");
        assert_eq!(serde_json::from_str::<HumanoidPose>(&json).unwrap().checksum(), pose.checksum());
    }

    #[test]
    fn reading_a_humanoid_pose_refuses_names_it_does_not_know() {
        let unknown_bone = serde_json::from_str::<HumanoidPose>(r#"{"rotations":{"leftWing":[0,0,0,1]}}"#);
        assert!(unknown_bone.unwrap_err().to_string().contains("leftWing"));
        assert!(serde_json::from_str::<HumanoidPose>(r#"{"rotation":{}}"#).is_err(), "a typo is not silently ignored");
    }

    #[test]
    fn hot_types_are_fixed_size() {
        assert!(std::mem::size_of::<HumanoidPose>() <= 1024);
        assert_eq!(std::mem::size_of::<Pose>(), MAX_BONES * std::mem::size_of::<Transform>());
    }
}
