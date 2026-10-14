//! The VRM humanoid: which node plays which bone.
//!
//! Bone names are VRM 1.0 names (`VRMC_vrm-1.0/humanoid.md`), the canonical vocabulary of the scene. VRM 0.x differs in
//! one place, the thumb: 0.x has `ThumbProximal`, `ThumbIntermediate`, `ThumbDistal`, while 1.0 has `ThumbMetacarpal`,
//! `ThumbProximal`, `ThumbDistal`, so 0.x `ThumbProximal` is 1.0 `ThumbMetacarpal` and 0.x `ThumbIntermediate` is 1.0
//! `ThumbProximal` (`UniVRM`'s migrator does the same). [`BoneSpec::vrm0_name`] and [`HumanBoneName::from_vrm0`] hold
//! that rule so the four importers and exporters share it.
//!
//! The bone set is a *file format* fact, so it lives here and not in the animation contract. `omuzaki-anim-api`'s
//! runtime skeleton converts through the bone's string name (see `docs/design/io-api.md`).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::NodeIx;
use crate::meta::VrmVersion;

/// One row of the spec's bone table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BoneSpec {
    /// The VRM 1.0 name, such as `leftUpperArm`.
    pub name: &'static str,
    /// The parent bone in the humanoid tree; `None` for `hips`.
    pub parent: Option<&'static str>,
    /// Required by VRM 1.0. VRM 0.x also requires `chest` and `neck` ([`BoneSpec::required_in`]).
    pub required: bool,
    /// The bone can exist only if its parent does (`upperChest` needs `chest`; finger joints need the finger's base).
    /// Otherwise a missing parent is skipped and the grandparent is used.
    pub needs_parent: bool,
}

const fn bone(name: &'static str, parent: Option<&'static str>, required: bool, needs_parent: bool) -> BoneSpec {
    BoneSpec { name, parent, required, needs_parent }
}

/// Every humanoid bone of VRM 1.0 (55), in the spec's order: torso, head, legs, arms, then fingers.
pub const HUMAN_BONES: [BoneSpec; 55] = [
    bone("hips", None, true, false),
    bone("spine", Some("hips"), true, false),
    bone("chest", Some("spine"), false, false),
    bone("upperChest", Some("chest"), false, true),
    bone("neck", Some("upperChest"), false, false),
    bone("head", Some("neck"), true, false),
    bone("leftEye", Some("head"), false, false),
    bone("rightEye", Some("head"), false, false),
    bone("jaw", Some("head"), false, false),
    bone("leftUpperLeg", Some("hips"), true, false),
    bone("leftLowerLeg", Some("leftUpperLeg"), true, false),
    bone("leftFoot", Some("leftLowerLeg"), true, false),
    bone("leftToes", Some("leftFoot"), false, false),
    bone("rightUpperLeg", Some("hips"), true, false),
    bone("rightLowerLeg", Some("rightUpperLeg"), true, false),
    bone("rightFoot", Some("rightLowerLeg"), true, false),
    bone("rightToes", Some("rightFoot"), false, false),
    bone("leftShoulder", Some("upperChest"), false, false),
    bone("leftUpperArm", Some("leftShoulder"), true, false),
    bone("leftLowerArm", Some("leftUpperArm"), true, false),
    bone("leftHand", Some("leftLowerArm"), true, false),
    bone("rightShoulder", Some("upperChest"), false, false),
    bone("rightUpperArm", Some("rightShoulder"), true, false),
    bone("rightLowerArm", Some("rightUpperArm"), true, false),
    bone("rightHand", Some("rightLowerArm"), true, false),
    bone("leftThumbMetacarpal", Some("leftHand"), false, false),
    bone("leftThumbProximal", Some("leftThumbMetacarpal"), false, true),
    bone("leftThumbDistal", Some("leftThumbProximal"), false, true),
    bone("leftIndexProximal", Some("leftHand"), false, false),
    bone("leftIndexIntermediate", Some("leftIndexProximal"), false, true),
    bone("leftIndexDistal", Some("leftIndexIntermediate"), false, true),
    bone("leftMiddleProximal", Some("leftHand"), false, false),
    bone("leftMiddleIntermediate", Some("leftMiddleProximal"), false, true),
    bone("leftMiddleDistal", Some("leftMiddleIntermediate"), false, true),
    bone("leftRingProximal", Some("leftHand"), false, false),
    bone("leftRingIntermediate", Some("leftRingProximal"), false, true),
    bone("leftRingDistal", Some("leftRingIntermediate"), false, true),
    bone("leftLittleProximal", Some("leftHand"), false, false),
    bone("leftLittleIntermediate", Some("leftLittleProximal"), false, true),
    bone("leftLittleDistal", Some("leftLittleIntermediate"), false, true),
    bone("rightThumbMetacarpal", Some("rightHand"), false, false),
    bone("rightThumbProximal", Some("rightThumbMetacarpal"), false, true),
    bone("rightThumbDistal", Some("rightThumbProximal"), false, true),
    bone("rightIndexProximal", Some("rightHand"), false, false),
    bone("rightIndexIntermediate", Some("rightIndexProximal"), false, true),
    bone("rightIndexDistal", Some("rightIndexIntermediate"), false, true),
    bone("rightMiddleProximal", Some("rightHand"), false, false),
    bone("rightMiddleIntermediate", Some("rightMiddleProximal"), false, true),
    bone("rightMiddleDistal", Some("rightMiddleIntermediate"), false, true),
    bone("rightRingProximal", Some("rightHand"), false, false),
    bone("rightRingIntermediate", Some("rightRingProximal"), false, true),
    bone("rightRingDistal", Some("rightRingIntermediate"), false, true),
    bone("rightLittleProximal", Some("rightHand"), false, false),
    bone("rightLittleIntermediate", Some("rightLittleProximal"), false, true),
    bone("rightLittleDistal", Some("rightLittleIntermediate"), false, true),
];

impl BoneSpec {
    /// Whether the bone must be assigned for a model to be valid in `version`.
    pub fn required_in(&self, version: VrmVersion) -> bool {
        self.required || (version == VrmVersion::V0 && matches!(self.name, "chest" | "neck"))
    }

    /// The name this bone has in VRM 0.x files. Only the thumb joints differ from [`BoneSpec::name`].
    pub fn vrm0_name(&self) -> Cow<'static, str> {
        if self.name.contains("Thumb") {
            // Order matters: rename Proximal first, so the Metacarpal -> Proximal result isn't renamed again.
            Cow::Owned(self.name.replace("Proximal", "Intermediate").replace("Metacarpal", "Proximal"))
        } else {
            Cow::Borrowed(self.name)
        }
    }
}

/// A name that is not one of the 55 VRM 1.0 humanoid bones.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a VRM humanoid bone")]
pub struct UnknownBone(pub String);

/// A humanoid bone, guaranteed to be one of [`HUMAN_BONES`]. Serialised as its VRM 1.0 name. Ordered like the table,
/// so iteration over a [`Humanoid`] is deterministic and parents come before children.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HumanBoneName(u8);

impl HumanBoneName {
    /// Looks up a VRM 1.0 name such as `leftHand`.
    ///
    /// # Errors
    /// [`UnknownBone`] if the name isn't in the spec's table (names are case-sensitive).
    pub fn new(name: &str) -> Result<Self, UnknownBone> {
        Self::find(|b| b.name == name).ok_or_else(|| UnknownBone(name.to_owned()))
    }

    /// Looks up a VRM 0.x name (`leftThumbIntermediate` is 1.0's `leftThumbProximal`).
    pub fn from_vrm0(name: &str) -> Option<Self> {
        Self::find(|b| b.vrm0_name() == name)
    }

    fn find(pred: impl Fn(&BoneSpec) -> bool) -> Option<Self> {
        let ix = HUMAN_BONES.iter().position(pred)?;
        u8::try_from(ix).ok().map(Self)
    }

    /// The bone's table row.
    pub fn spec(self) -> &'static BoneSpec {
        // The index is only ever made by `find`, which takes it from the table, so it is in range.
        &HUMAN_BONES[usize::from(self.0)]
    }

    /// The VRM 1.0 name.
    pub fn as_str(self) -> &'static str {
        self.spec().name
    }
}

impl fmt::Display for HumanBoneName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for HumanBoneName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HumanBoneName({})", self.as_str())
    }
}

impl TryFrom<String> for HumanBoneName {
    type Error = UnknownBone;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(&s)
    }
}

impl From<HumanBoneName> for String {
    fn from(b: HumanBoneName) -> Self {
        b.as_str().to_owned()
    }
}

/// Which node plays which humanoid bone. A node plays at most one bone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Humanoid {
    /// Assigned bones, in table order.
    pub bones: BTreeMap<HumanBoneName, NodeIx>,
}

impl Humanoid {
    /// Assigns `node` to `bone`.
    ///
    /// # Errors
    /// [`UnknownBone`] if `bone` isn't a VRM 1.0 humanoid bone name.
    pub fn assign(&mut self, bone: &str, node: NodeIx) -> Result<(), UnknownBone> {
        self.bones.insert(HumanBoneName::new(bone)?, node);
        Ok(())
    }

    /// The node assigned to `bone`, if any.
    pub fn node(&self, bone: &str) -> Option<NodeIx> {
        self.bones.get(&HumanBoneName::new(bone).ok()?).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_the_spec_counts() {
        assert_eq!(HUMAN_BONES.iter().filter(|b| b.required).count(), 15);
        assert_eq!(HUMAN_BONES.iter().filter(|b| b.required_in(VrmVersion::V0)).count(), 17);
        // Every parent is itself a bone, and appears earlier in the table (so checks can walk it in one pass).
        for (i, b) in HUMAN_BONES.iter().enumerate() {
            if let Some(p) = b.parent {
                let pi = HUMAN_BONES.iter().position(|x| x.name == p);
                assert!(pi.is_some_and(|pi| pi < i), "{} has a bad parent {p}", b.name);
            }
        }
    }

    #[test]
    fn thumbs_are_renamed_between_vrm0_and_vrm1() {
        let get = |n: &str| HumanBoneName::new(n).unwrap().spec().vrm0_name().into_owned();
        assert_eq!(get("leftThumbMetacarpal"), "leftThumbProximal");
        assert_eq!(get("leftThumbProximal"), "leftThumbIntermediate");
        assert_eq!(get("rightThumbDistal"), "rightThumbDistal");
        assert_eq!(get("leftIndexProximal"), "leftIndexProximal");
        assert_eq!(HumanBoneName::from_vrm0("leftThumbProximal").unwrap().as_str(), "leftThumbMetacarpal");
        assert_eq!(HumanBoneName::from_vrm0("rightThumbIntermediate").unwrap().as_str(), "rightThumbProximal");
        assert_eq!(HumanBoneName::from_vrm0("leftThumbMetacarpal"), None, "0.x has no metacarpal");
    }

    #[test]
    fn names_round_trip_through_serde_and_reject_unknowns() {
        let b = HumanBoneName::new("leftUpperArm").unwrap();
        assert_eq!(serde_json::to_string(&b).unwrap(), "\"leftUpperArm\"");
        assert_eq!(serde_json::from_str::<HumanBoneName>("\"leftUpperArm\"").unwrap(), b);
        assert!(serde_json::from_str::<HumanBoneName>("\"LeftUpperArm\"").is_err());
        assert!(HumanBoneName::new("tail").is_err());
    }

    #[test]
    fn humanoid_assigns_and_looks_up_by_name() {
        let mut h = Humanoid::default();
        h.assign("hips", NodeIx(3)).unwrap();
        assert_eq!(h.node("hips"), Some(NodeIx(3)));
        assert_eq!(h.node("head"), None);
        assert!(h.assign("wing", NodeIx(0)).is_err());
    }
}
