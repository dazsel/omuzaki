//! Spring bones and colliders (spec RG-03): secondary motion for hair, ears, tails, skirts and accessories.
//!
//! The data model is VRM 1.0's `VRMC_springBone` (names, units and defaults), so a character's springs export to and
//! import from VRM without translation (<https://github.com/vrm-c/vrm-specification/tree/master/specification/VRMC_springBone-1.0>).
//! Two deliberate differences, both for people and agents editing the data rather than for the maths:
//! - colliders live **inside** their group and groups are referred to by **name**, not by index, so reordering a list
//!   never silently rewires a chain;
//! - the checks VRM leaves to the importer are in [`SpringSet::validate`], returned as errors that say what to fix.
//!
//! Spring chains are authored by the character builder (hair clumps become chains, CB-33) and simulated by the rig;
//! the [`SpringSolver`] trait is the boundary between them.

use omuzaki_core::glam::Vec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::frame::Tick;
use crate::skeleton::{BoneId, Pose, Skeleton};

/// Spring physics runs in fixed sub-steps of this rate, whatever the frame rate (spec §8: 120 Hz).
pub const PHYSICS_HZ: f32 = 120.0;
/// One physics sub-step, seconds.
pub const PHYSICS_STEP: f32 = 1.0 / PHYSICS_HZ;
/// The most sub-steps run in one frame by default. A frame that needs more (a long stall) drops the excess time
/// rather than spiralling; see [`SpringStats::dropped_s`].
pub const DEFAULT_MAX_SUBSTEPS: u32 = 8;

/// How one joint of a chain behaves. Field names, units and defaults are VRM 1.0's.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SpringParams {
    /// Radius of the joint's collision sphere, metres.
    pub hit_radius: f32,
    /// Force pulling the joint back toward its rest pose. Higher is stiffer.
    pub stiffness: f32,
    /// Strength of gravity on the joint.
    pub gravity_power: f32,
    /// Direction of gravity (world space; usually straight down).
    #[schemars(with = "[f32; 3]")]
    pub gravity_dir: Vec3,
    /// Air resistance, `0..=1`: how much of the joint's velocity is shed each step. Higher damps faster.
    pub drag_force: f32,
}

impl Default for SpringParams {
    fn default() -> Self {
        Self { hit_radius: 0.0, stiffness: 1.0, gravity_power: 0.0, gravity_dir: Vec3::NEG_Y, drag_force: 0.5 }
    }
}

impl SpringParams {
    /// The blend of two parameter sets: `t = 0` is `self`, `t = 1` is `other`. Chains taper this from root to tip.
    #[must_use]
    pub fn lerp(&self, other: &Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Self {
            hit_radius: mix(self.hit_radius, other.hit_radius),
            stiffness: mix(self.stiffness, other.stiffness),
            gravity_power: mix(self.gravity_power, other.gravity_power),
            gravity_dir: self.gravity_dir.lerp(other.gravity_dir, t),
            drag_force: mix(self.drag_force, other.drag_force),
        }
    }

    fn check(&self) -> Option<&'static str> {
        if !(self.hit_radius >= 0.0 && self.hit_radius.is_finite()) {
            Some("hit_radius must be 0 or more")
        } else if !(self.stiffness >= 0.0 && self.stiffness.is_finite()) {
            Some("stiffness must be 0 or more")
        } else if !(self.gravity_power >= 0.0 && self.gravity_power.is_finite()) {
            Some("gravity_power must be 0 or more")
        } else if !(0.0..=1.0).contains(&self.drag_force) {
            Some("drag_force must be between 0 and 1")
        } else if !self.gravity_dir.is_finite() {
            Some("gravity_dir must be finite")
        } else {
            None
        }
    }
}

/// One joint of a chain: a bone and how it moves. The last joint of a chain is only a tail (the point the previous
/// joint's bone points at); its parameters are unused, as in VRM.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SpringJoint {
    /// The bone.
    pub bone: BoneId,
    /// Its parameters.
    #[serde(flatten)]
    pub params: SpringParams,
}

/// A chain of joints swinging together: one lock of hair, one ear, one skirt panel edge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SpringChain {
    /// A unique name, e.g. `hair_fringe_2`.
    pub name: String,
    /// Joints from root to tip; each bone is a descendant of the one before.
    pub joints: Vec<SpringJoint>,
    /// Collider groups this chain collides with, by [`ColliderGroup::name`].
    #[serde(default)]
    pub collider_groups: Vec<String>,
    /// The bone whose space the chain is simulated in (so walking doesn't fling the hair). Must be the first joint's
    /// bone or one of its ancestors. `None` simulates in world space.
    #[serde(default)]
    pub center: Option<BoneId>,
}

impl SpringChain {
    /// A chain over `bones` where every joint has the same parameters.
    pub fn new(name: impl Into<String>, bones: &[BoneId], params: SpringParams) -> Self {
        Self::tapered(name, bones, params, params)
    }

    /// A chain whose parameters blend from `root` at the first joint to `tip` at the last (hair is stiff at the root
    /// and loose at the ends).
    #[allow(clippy::cast_precision_loss)] // chains are a handful of joints
    pub fn tapered(name: impl Into<String>, bones: &[BoneId], root: SpringParams, tip: SpringParams) -> Self {
        let last = bones.len().saturating_sub(1).max(1) as f32;
        let joints = bones
            .iter()
            .enumerate()
            .map(|(i, &bone)| SpringJoint { bone, params: root.lerp(&tip, i as f32 / last) })
            .collect();
        Self { name: name.into(), joints, collider_groups: Vec::new(), center: None }
    }
}

/// A collision shape attached to a bone, in the bone's local space (VRM `sphere` and `capsule`).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColliderShape {
    /// A ball.
    Sphere {
        /// Centre.
        #[schemars(with = "[f32; 3]")]
        offset: Vec3,
        /// Radius, metres.
        radius: f32,
    },
    /// A capsule: the volume within `radius` of the segment `offset` to `tail`.
    Capsule {
        /// Centre of the start cap.
        #[schemars(with = "[f32; 3]")]
        offset: Vec3,
        /// Radius of the caps and the cylinder, metres.
        radius: f32,
        /// Centre of the end cap.
        #[schemars(with = "[f32; 3]")]
        tail: Vec3,
    },
}

impl ColliderShape {
    fn radius(&self) -> f32 {
        match *self {
            Self::Sphere { radius, .. } | Self::Capsule { radius, .. } => radius,
        }
    }
}

/// A collision shape on a bone.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Collider {
    /// The bone it follows.
    pub bone: BoneId,
    /// Its shape in that bone's space.
    pub shape: ColliderShape,
}

/// A named set of colliders that chains opt into together ("head and shoulders").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ColliderGroup {
    /// A unique name.
    pub name: String,
    /// The colliders.
    pub colliders: Vec<Collider>,
}

/// Why a [`SpringSet`] doesn't fit its skeleton. Each message says what to fix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpringError {
    /// Two chains or two groups share a name, or a name is empty.
    #[error("{kind} name {name:?} is empty or used twice")]
    BadName {
        /// `"chain"` or `"collider group"`.
        kind: &'static str,
        /// The name.
        name: String,
    },
    /// A chain has fewer than two joints (a root and a tail).
    #[error("chain {chain:?} has {count} joint(s); it needs at least 2 (the last is only a tail)")]
    TooShort {
        /// The chain.
        chain: String,
        /// How many joints it has.
        count: usize,
    },
    /// A bone id isn't in the skeleton.
    #[error("{what} refers to bone {bone}, but the skeleton has {len} bones")]
    NoSuchBone {
        /// Where the id appears.
        what: String,
        /// The id.
        bone: u16,
        /// The skeleton's size.
        len: usize,
    },
    /// A joint isn't below the one before it.
    #[error("chain {chain:?}: joint {joint} is not a descendant of joint {previous}")]
    NotAChain {
        /// The chain.
        chain: String,
        /// The joint's position.
        joint: usize,
        /// The position of the joint before it.
        previous: usize,
    },
    /// The chain's centre isn't the first joint or one of its ancestors.
    #[error("chain {chain:?}: the center bone must be the first joint's bone or one of its ancestors")]
    BadCenter {
        /// The chain.
        chain: String,
    },
    /// A bone is a joint of two chains. The later chain would silently overwrite the rotation the earlier one wrote,
    /// so VRM forbids it.
    #[error("bone {bone} is a joint of both chain {other:?} and chain {chain:?}; remove it from one of them")]
    SharedJoint {
        /// The chain that repeats the bone (the later one in the set).
        chain: String,
        /// The chain that already has the bone.
        other: String,
        /// The bone's id.
        bone: u16,
    },
    /// A chain's centre is a joint of another chain, or below one of its joints. VRM forbids it: the centre must not
    /// move with another chain's joints.
    #[error(
        "chain {chain:?}: the center bone {bone} is a joint of chain {other:?} or below one of its joints; pick a centre outside that chain"
    )]
    CenterInOtherChain {
        /// The chain whose centre is wrong.
        chain: String,
        /// The other chain.
        other: String,
        /// The centre bone's id.
        bone: u16,
    },
    /// A chain names a collider group that doesn't exist.
    #[error("chain {chain:?} uses collider group {group:?}, which doesn't exist")]
    NoSuchGroup {
        /// The chain.
        chain: String,
        /// The missing group.
        group: String,
    },
    /// A parameter is out of range.
    #[error("{what}: {reason}")]
    BadParams {
        /// Where.
        what: String,
        /// What is wrong.
        reason: &'static str,
    },
}

/// Every spring chain and collider group of one character.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SpringSet {
    /// The chains.
    pub chains: Vec<SpringChain>,
    /// The collider groups.
    #[serde(default)]
    pub groups: Vec<ColliderGroup>,
}

impl SpringSet {
    /// Checks the set against the skeleton it will drive.
    ///
    /// # Errors
    /// The first [`SpringError`] found: duplicate or empty names, chains that are too short or aren't descendant
    /// chains, a bad centre, missing bones or groups, out-of-range parameters, a bone that is a joint of two chains,
    /// or a centre that is a joint of another chain or below one.
    pub fn validate(&self, skeleton: &Skeleton) -> Result<(), SpringError> {
        let bone_ok = |what: &str, bone: BoneId| {
            skeleton.get(bone).map(|_| ()).ok_or_else(|| SpringError::NoSuchBone {
                what: what.to_owned(),
                bone: bone.0,
                len: skeleton.len(),
            })
        };
        unique_names("collider group", self.groups.iter().map(|g| g.name.as_str()))?;
        unique_names("chain", self.chains.iter().map(|c| c.name.as_str()))?;
        for group in &self.groups {
            for (i, c) in group.colliders.iter().enumerate() {
                let what = format!("collider {i} of group {:?}", group.name);
                bone_ok(&what, c.bone)?;
                if !(c.shape.radius() >= 0.0 && c.shape.radius().is_finite()) {
                    return Err(SpringError::BadParams { what, reason: "radius must be 0 or more" });
                }
            }
        }
        for chain in &self.chains {
            self.validate_chain(chain, skeleton, &bone_ok)?;
        }
        self.validate_between_chains(skeleton)
    }

    /// The rules that relate chains to each other (VRM 1.0): a bone is a joint of at most one chain, and a centre is
    /// neither a joint of another chain nor below one. Runs after each chain's own checks, so every bone exists.
    fn validate_between_chains(&self, skeleton: &Skeleton) -> Result<(), SpringError> {
        let mut owner: std::collections::HashMap<BoneId, &str> = std::collections::HashMap::new();
        for chain in &self.chains {
            for joint in &chain.joints {
                if let Some(&other) = owner.get(&joint.bone) {
                    return Err(SpringError::SharedJoint {
                        chain: chain.name.clone(),
                        other: other.to_owned(),
                        bone: joint.bone.0,
                    });
                }
                owner.insert(joint.bone, chain.name.as_str());
            }
        }
        for (i, chain) in self.chains.iter().enumerate() {
            let Some(center) = chain.center else { continue };
            // A joint `j` of another chain covers the centre when `j` is the centre or one of its ancestors.
            let clash = self.chains.iter().enumerate().find(|&(j, other)| {
                j != i && other.joints.iter().any(|joint| skeleton.is_ancestor_or_self(joint.bone, center))
            });
            if let Some((_, other)) = clash {
                return Err(SpringError::CenterInOtherChain {
                    chain: chain.name.clone(),
                    other: other.name.clone(),
                    bone: center.0,
                });
            }
        }
        Ok(())
    }

    fn validate_chain(
        &self,
        chain: &SpringChain,
        skeleton: &Skeleton,
        bone_ok: &dyn Fn(&str, BoneId) -> Result<(), SpringError>,
    ) -> Result<(), SpringError> {
        let name = &chain.name;
        if chain.joints.len() < 2 {
            return Err(SpringError::TooShort { chain: name.clone(), count: chain.joints.len() });
        }
        for (i, joint) in chain.joints.iter().enumerate() {
            let what = format!("joint {i} of chain {name:?}");
            bone_ok(&what, joint.bone)?;
            if let Some(reason) = joint.params.check() {
                return Err(SpringError::BadParams { what, reason });
            }
            if i > 0 {
                let previous = chain.joints[i - 1].bone;
                if joint.bone == previous || !skeleton.is_ancestor_or_self(previous, joint.bone) {
                    return Err(SpringError::NotAChain { chain: name.clone(), joint: i, previous: i - 1 });
                }
            }
        }
        if let Some(center) = chain.center {
            bone_ok(&format!("center of chain {name:?}"), center)?;
            if !skeleton.is_ancestor_or_self(center, chain.joints[0].bone) {
                return Err(SpringError::BadCenter { chain: name.clone() });
            }
        }
        for group in &chain.collider_groups {
            if !self.groups.iter().any(|g| &g.name == group) {
                return Err(SpringError::NoSuchGroup { chain: name.clone(), group: group.clone() });
            }
        }
        Ok(())
    }
}

fn unique_names<'a>(kind: &'static str, names: impl Iterator<Item = &'a str>) -> Result<(), SpringError> {
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if name.is_empty() || !seen.insert(name) {
            return Err(SpringError::BadName { kind, name: name.to_owned() });
        }
    }
    Ok(())
}

/// What the solver did last frame: the numbers the frame-budget scheduler and the live health meter read.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct SpringStats {
    /// Sub-steps run last frame.
    pub substeps: u32,
    /// Simulated time dropped last frame because the frame needed more than the maximum sub-steps, seconds.
    pub dropped_s: f32,
}

/// Simulates a [`SpringSet`] on a skeleton. Build one with the set and skeleton (an implementation's `new`); it then
/// owns all its state in fixed buffers, so stepping never allocates.
pub trait SpringSolver: Send {
    /// Advances the springs by `tick.dt`.
    ///
    /// Time is consumed in whole [`PHYSICS_STEP`] sub-steps, at most the current maximum per call; the fractional
    /// remainder carries to the next call, so the simulation is independent of the frame rate. `pose` holds the
    /// animated local transforms on entry (colliders and chain roots follow it) and the spring bones' solved local
    /// rotations on exit; other bones are untouched. The result is a pure function of the inputs and prior calls.
    fn step(&mut self, tick: Tick, skeleton: &Skeleton, pose: &mut Pose);

    /// Settles the springs against `pose` for `seconds` of simulated time without moving the character, so hair is at
    /// rest before the first frame (the prototype ran 45 frames of this).
    fn warm_up(&mut self, skeleton: &Skeleton, pose: &mut Pose, seconds: f32);

    /// Puts every spring at rest, as after a teleport.
    fn reset(&mut self);

    /// Lowers or raises the sub-step cap: the frame-budget scheduler's way to degrade gracefully (spec §8).
    fn set_max_substeps(&mut self, max: u32);

    /// What the last [`SpringSolver::step`] did.
    fn stats(&self) -> SpringStats;
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // tests compare values that are copied or exactly computed, and cast small loop counters
mod tests {
    use super::*;
    use crate::skeleton::{BoneDef, HumanoidBone, Transform};

    /// A humanoid plus a line of `len` hair bones under the head, each the child of the one before.
    fn hair_line(len: usize) -> (Skeleton, Vec<BoneId>, BoneId) {
        let base = Skeleton::humanoid(&HumanoidBone::ALL, |_| Transform::from_translation(Vec3::Y * 0.1)).unwrap();
        let head = base.humanoid_bone(HumanoidBone::Head).unwrap();
        let mut bones = base.bones().to_vec();
        let mut ids = Vec::with_capacity(len);
        let mut parent = head;
        for i in 0..len {
            let id = BoneId(bones.len() as u16);
            bones.push(BoneDef {
                name: format!("hair_{i}"),
                parent: Some(parent),
                rest: Transform::from_translation(Vec3::NEG_Y * 0.05),
                humanoid: None,
            });
            ids.push(id);
            parent = id;
        }
        (Skeleton::new(bones).unwrap(), ids, head)
    }

    /// A humanoid plus a 3-bone hair chain under the head.
    fn skeleton() -> (Skeleton, [BoneId; 3], BoneId) {
        let (sk, ids, head) = hair_line(3);
        (sk, ids.try_into().unwrap(), head)
    }

    fn valid_set(ids: [BoneId; 3], head: BoneId) -> SpringSet {
        let mut chain = SpringChain::new("hair", &ids, SpringParams::default());
        chain.collider_groups.push("head".into());
        chain.center = Some(head);
        let group = ColliderGroup {
            name: "head".into(),
            colliders: vec![Collider {
                bone: head,
                shape: ColliderShape::Sphere { offset: Vec3::Y * 0.1, radius: 0.1 },
            }],
        };
        SpringSet { chains: vec![chain], groups: vec![group] }
    }

    #[test]
    fn defaults_are_vrm_1_0s() {
        let p = SpringParams::default();
        assert_eq!((p.hit_radius, p.stiffness, p.gravity_power, p.drag_force), (0.0, 1.0, 0.0, 0.5));
        assert_eq!(p.gravity_dir, Vec3::new(0.0, -1.0, 0.0));
        assert!((PHYSICS_STEP * PHYSICS_HZ - 1.0).abs() < 1e-6);
    }

    #[test]
    fn tapered_chains_blend_root_to_tip() {
        let (_, ids, _) = skeleton();
        let root = SpringParams { stiffness: 2.0, drag_force: 0.2, ..SpringParams::default() };
        let tip = SpringParams { stiffness: 0.5, drag_force: 0.6, ..SpringParams::default() };
        let c = SpringChain::tapered("t", &ids, root, tip);
        assert_eq!(c.joints[0].params, root);
        assert_eq!(c.joints[2].params, tip);
        assert!((c.joints[1].params.stiffness - 1.25).abs() < 1e-6);
        assert_eq!(SpringChain::tapered("one", &ids[..1], root, tip).joints[0].params, root, "no divide by zero");
    }

    #[test]
    fn a_good_set_validates_and_round_trips() {
        let (sk, ids, head) = skeleton();
        let set = valid_set(ids, head);
        assert_eq!(set.validate(&sk), Ok(()));
        let json = serde_json::to_string(&set).unwrap();
        assert!(json.contains("\"drag_force\":0.5") && json.contains("\"stiffness\":1.0"), "{json}");
        assert_eq!(serde_json::from_str::<SpringSet>(&json).unwrap(), set);
        let partial: SpringJoint = serde_json::from_str(r#"{"bone": 3, "stiffness": 0.3}"#).unwrap();
        assert_eq!((partial.params.stiffness, partial.params.drag_force), (0.3, 0.5), "VRM defaults fill the rest");
    }

    #[test]
    fn each_rule_has_its_own_error() {
        let (sk, ids, head) = skeleton();
        let good = valid_set(ids, head);
        let check = |edit: &dyn Fn(&mut SpringSet)| {
            let mut s = good.clone();
            edit(&mut s);
            s.validate(&sk).unwrap_err()
        };
        assert!(matches!(check(&|s| s.chains[0].name.clear()), SpringError::BadName { kind: "chain", .. }));
        assert!(matches!(
            check(&|s| s.groups.push(s.groups[0].clone())),
            SpringError::BadName { kind: "collider group", .. }
        ));
        assert!(matches!(check(&|s| s.chains[0].joints.truncate(1)), SpringError::TooShort { count: 1, .. }));
        assert!(matches!(
            check(&|s| s.chains[0].joints[1].bone = BoneId(999)),
            SpringError::NoSuchBone { bone: 999, .. }
        ));
        assert!(matches!(check(&|s| s.chains[0].joints.swap(1, 2)), SpringError::NotAChain { joint: 2, .. }));
        assert!(matches!(check(&|s| s.chains[0].center = Some(ids[2])), SpringError::BadCenter { .. }));
        assert!(matches!(check(&|s| s.chains[0].collider_groups[0] = "nope".into()), SpringError::NoSuchGroup { .. }));
        assert!(matches!(check(&|s| s.chains[0].joints[0].params.drag_force = 1.5), SpringError::BadParams { .. }));
        assert!(matches!(check(&|s| s.chains[0].joints[0].params.stiffness = f32::NAN), SpringError::BadParams { .. }));
        let neg = |s: &mut SpringSet| {
            s.groups[0].colliders[0].shape = ColliderShape::Sphere { offset: Vec3::ZERO, radius: -1.0 }
        };
        assert!(matches!(check(&neg), SpringError::BadParams { .. }));
    }

    /// A set of two chains, `front` and `back`, over [`hair_line`] bones, with `back_centre` on `back`.
    fn two_chains(front_bones: &[BoneId], back_bones: &[BoneId], back_centre: Option<BoneId>) -> SpringSet {
        let mut back = SpringChain::new("back", back_bones, SpringParams::default());
        back.center = back_centre;
        let front = SpringChain::new("front", front_bones, SpringParams::default());
        SpringSet { chains: vec![front, back], groups: Vec::new() }
    }

    #[test]
    fn a_bone_is_a_joint_of_one_chain_only() {
        let (sk, ids, _) = hair_line(5);
        let err = two_chains(&ids[..3], &ids[2..], None).validate(&sk).unwrap_err();
        assert_eq!(err, SpringError::SharedJoint { chain: "back".into(), other: "front".into(), bone: ids[2].0 });
        let text = err.to_string();
        assert!(text.contains("\"front\"") && text.contains("\"back\""), "{text}");
        assert_eq!(two_chains(&ids[..2], &ids[2..], None).validate(&sk), Ok(()), "disjoint chains are fine");
    }

    #[test]
    fn a_centre_is_not_a_joint_of_another_chain() {
        let (sk, ids, _) = hair_line(5);
        let err = two_chains(&ids[..3], &ids[3..], Some(ids[1])).validate(&sk).unwrap_err();
        let want = SpringError::CenterInOtherChain { chain: "back".into(), other: "front".into(), bone: ids[1].0 };
        assert_eq!(err, want);
    }

    #[test]
    fn a_centre_is_not_below_a_joint_of_another_chain() {
        let (sk, ids, _) = hair_line(5);
        // `front` stops at `ids[1]`, so `ids[2]` is not one of its joints, but it hangs below one.
        let err = two_chains(&ids[..2], &ids[3..], Some(ids[2])).validate(&sk).unwrap_err();
        let want = SpringError::CenterInOtherChain { chain: "back".into(), other: "front".into(), bone: ids[2].0 };
        assert_eq!(err, want);
    }

    #[test]
    fn a_centre_above_another_chain_is_fine() {
        let (sk, ids, head) = hair_line(5);
        assert_eq!(two_chains(&ids[1..3], &ids[3..], Some(ids[0])).validate(&sk), Ok(()), "above the other chain");
        assert_eq!(two_chains(&ids[1..3], &ids[3..], Some(head)).validate(&sk), Ok(()), "above the head");
    }
}
