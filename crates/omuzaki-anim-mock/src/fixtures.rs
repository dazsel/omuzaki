//! Ready-made, valid data for tests of anything that consumes the contract: a humanoid skeleton with hair bones, its
//! spring set, two generic (non-humanoid) creatures with theirs, and a small score. Nothing here is a real character or
//! comes from a third-party file.

use omuzaki_anim_api::{
    BoneDef, BoneId, Channel, ChannelTrack, ClipId, Collider, ColliderGroup, ColliderShape, Cue, CueKind, Ease,
    HumanoidBone, Key, Score, Skeleton, SkeletonError, SpringChain, SpringError, SpringParams, SpringSet, Transform,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::id::SlugError;
use omuzaki_core::time::Seconds;

/// A fixture failed to build: a bug in the fixture, reported as a value instead of a panic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FixtureError {
    /// The skeleton was rejected.
    #[error("fixture skeleton: {0}")]
    Skeleton(#[from] SkeletonError),
    /// An id was not a valid slug.
    #[error("fixture id: {0}")]
    Id(#[from] SlugError),
    /// A humanoid bone the fixture relies on is missing.
    #[error("fixture skeleton has no head bone")]
    NoHead,
    /// A spring set was rejected.
    #[error("fixture springs: {0}")]
    Springs(#[from] SpringError),
}

/// A skeleton with hair chains, the spring set that moves them, and the bone ids of each chain.
#[derive(Clone, Debug)]
pub struct HairRig {
    /// Humanoid plus hair bones.
    pub skeleton: Skeleton,
    /// One spring chain per hair chain, colliding with a head sphere.
    pub springs: SpringSet,
    /// The bones of each chain, root first.
    pub chains: Vec<Vec<BoneId>>,
}

/// Hair bones hanging from the head, per chain.
pub const HAIR_BONES_PER_CHAIN: usize = 4;
/// Hair chains in [`skeleton_with_hair`].
pub const HAIR_CHAINS: usize = 3;

/// Rest offsets (metres, T-pose, character faces +Z) roughly shaped like a small adult. Left is +X.
fn rest_offset(bone: HumanoidBone) -> Vec3 {
    use HumanoidBone as B;
    let side = if bone.name().starts_with("left") { 1.0 } else { -1.0 };
    match bone {
        B::Hips => Vec3::new(0.0, 0.95, 0.0),
        B::Spine => Vec3::new(0.0, 0.10, 0.0),
        B::Chest => Vec3::new(0.0, 0.15, 0.0),
        B::UpperChest | B::Neck => Vec3::new(0.0, 0.12, 0.0),
        B::Head => Vec3::new(0.0, 0.08, 0.0),
        B::LeftEye | B::RightEye => Vec3::new(0.03 * side, 0.07, 0.08),
        B::Jaw => Vec3::new(0.0, 0.02, 0.05),
        B::LeftUpperLeg | B::RightUpperLeg => Vec3::new(0.09 * side, -0.05, 0.0),
        B::LeftLowerLeg | B::RightLowerLeg | B::LeftFoot | B::RightFoot => Vec3::new(0.0, -0.42, 0.0),
        B::LeftToes | B::RightToes => Vec3::new(0.0, -0.06, 0.12),
        B::LeftShoulder | B::RightShoulder => Vec3::new(0.03 * side, 0.08, 0.0),
        B::LeftUpperArm | B::RightUpperArm => Vec3::new(0.12 * side, 0.0, 0.0),
        B::LeftLowerArm | B::RightLowerArm => Vec3::new(0.28 * side, 0.0, 0.0),
        B::LeftHand | B::RightHand => Vec3::new(0.25 * side, 0.0, 0.0),
        _ => Vec3::new(0.03 * side, 0.0, 0.0), // fingers
    }
}

/// The full 55-bone humanoid with plausible proportions.
///
/// # Errors
/// Only if the fixture itself is broken (a test pins that it is not).
pub fn humanoid_skeleton() -> Result<Skeleton, FixtureError> {
    Ok(Skeleton::humanoid(&HumanoidBone::ALL, |b| Transform::from_translation(rest_offset(b)))?)
}

/// [`humanoid_skeleton`] plus [`HAIR_CHAINS`] chains of [`HAIR_BONES_PER_CHAIN`] hair bones under the head, and the
/// spring set that moves them (colliding with a head sphere).
///
/// # Errors
/// Only if the fixture itself is broken (a test pins that it is not).
pub fn skeleton_with_hair() -> Result<HairRig, FixtureError> {
    let base = humanoid_skeleton()?;
    let head = base.humanoid_bone(HumanoidBone::Head).ok_or(FixtureError::NoHead)?;
    let mut bones: Vec<BoneDef> = base.bones().to_vec();
    let mut chains = Vec::new();
    for chain in 0..HAIR_CHAINS {
        let mut parent = head;
        let mut ids = Vec::new();
        for joint in 0..HAIR_BONES_PER_CHAIN {
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // a few dozen bones
            let (id, x) = (BoneId(bones.len() as u16), 0.04 * (chain as f32 - 1.0));
            let offset = if joint == 0 { Vec3::new(x, 0.1, -0.08) } else { Vec3::new(0.0, -0.06, 0.0) };
            bones.push(BoneDef {
                name: format!("hair_{chain}_{joint}"),
                parent: Some(parent),
                rest: Transform::from_translation(offset),
                humanoid: None,
            });
            ids.push(id);
            parent = id;
        }
        chains.push(ids);
    }
    let skeleton = Skeleton::new(bones)?;
    let root = SpringParams { stiffness: 1.5, drag_force: 0.4, gravity_power: 0.1, ..SpringParams::default() };
    let tip = SpringParams {
        stiffness: 0.4,
        drag_force: 0.3,
        gravity_power: 0.3,
        hit_radius: 0.01,
        ..SpringParams::default()
    };
    let spring_chains = chains
        .iter()
        .enumerate()
        .map(|(i, ids)| {
            let mut chain = SpringChain::tapered(format!("hair_{i}"), ids, root, tip);
            chain.collider_groups.push("head".into());
            chain.center = Some(head);
            chain
        })
        .collect();
    let groups = vec![ColliderGroup {
        name: "head".into(),
        colliders: vec![Collider {
            bone: head,
            shape: ColliderShape::Sphere { offset: Vec3::new(0.0, 0.1, 0.0), radius: 0.1 },
        }],
    }];
    Ok(HairRig { skeleton, springs: SpringSet { chains: spring_chains, groups }, chains })
}

/// A generic (non-humanoid) skeleton with the spring set that moves its soft parts.
#[derive(Clone, Debug)]
pub struct CreatureRig {
    /// Built with [`Skeleton::generic`]: no bone has a humanoid role.
    pub skeleton: Skeleton,
    /// Chains for the soft parts (tail and ears; wobble arms and tuft), already validated against the skeleton.
    pub springs: SpringSet,
}

/// Bones in [`quadruped_rig`]: pelvis, spine, chest, neck, head, jaw, two bones per ear (4), three per leg (12), and four
/// of tail.
pub const QUADRUPED_BONES: usize = 26;
/// Bones in [`blob_rig`]: core, body, two bones per wobble arm (4) and three of tuft.
pub const BLOB_BONES: usize = 9;

/// Builds a bone list one parent-first `add` at a time, so a fixture never counts indices by hand.
#[derive(Default)]
struct Bones(Vec<BoneDef>);

impl Bones {
    /// Appends a bone `at` (parent space, metres) and returns its id.
    fn add(&mut self, name: &str, parent: Option<BoneId>, at: Vec3) -> BoneId {
        // The fixtures are a few dozen bones; an id this large would fail `Skeleton::generic` as a bad parent.
        let id = BoneId(u16::try_from(self.0.len()).unwrap_or(u16::MAX));
        self.0.push(BoneDef { name: name.into(), parent, rest: Transform::from_translation(at), humanoid: None });
        id
    }

    /// Appends `offsets.len()` bones named `{prefix}_{i}`, each the child of the one before (the first of `parent`).
    fn chain(&mut self, prefix: &str, parent: BoneId, offsets: &[Vec3]) -> Vec<BoneId> {
        let mut ids = Vec::new();
        let mut above = parent;
        for (i, at) in offsets.iter().enumerate() {
            above = self.add(&format!("{prefix}_{i}"), Some(above), *at);
            ids.push(above);
        }
        ids
    }
}

/// A spring chain over `bones`, softer toward the tip. It has no centre bone, so it swings when the whole creature moves.
fn soft_chain(name: &str, bones: &[BoneId], stiffness: f32) -> SpringChain {
    let root = SpringParams { stiffness, drag_force: 0.4, gravity_power: 0.1, ..SpringParams::default() };
    let tip =
        SpringParams { stiffness: stiffness * 0.4, drag_force: 0.3, gravity_power: 0.2, ..SpringParams::default() };
    SpringChain::tapered(name, bones, root, tip)
}

/// A dog-sized quadruped facing +Z: a 6-bone spine and head, floppy ears, four 3-bone legs and a 4-bone tail. The pelvis
/// is the root. The tail and ears are spring chains; the tail also collides with a sphere on the pelvis.
///
/// # Errors
/// Only if the fixture itself is broken (a test pins that it is not).
pub fn quadruped_rig() -> Result<CreatureRig, FixtureError> {
    let mut b = Bones::default();
    let pelvis = b.add("pelvis", None, Vec3::new(0.0, 0.5, 0.0));
    let spine = b.add("spine", Some(pelvis), Vec3::new(0.0, 0.0, 0.25));
    let chest = b.add("chest", Some(spine), Vec3::new(0.0, 0.02, 0.25));
    let neck = b.add("neck", Some(chest), Vec3::new(0.0, 0.1, 0.12));
    let head = b.add("head", Some(neck), Vec3::new(0.0, 0.08, 0.1));
    b.add("jaw", Some(head), Vec3::new(0.0, -0.04, 0.08));
    let ear = |side: f32| [Vec3::new(0.05 * side, 0.08, -0.02), Vec3::new(0.02 * side, -0.07, -0.02)];
    let ear_l = b.chain("ear_l", head, &ear(1.0));
    let ear_r = b.chain("ear_r", head, &ear(-1.0));
    for (leg, trunk) in [("fl", chest), ("fr", chest), ("bl", pelvis), ("br", pelvis)] {
        let x = if leg.ends_with('l') { 0.08 } else { -0.08 };
        let upper = b.add(&format!("leg_{leg}_upper"), Some(trunk), Vec3::new(x, -0.05, 0.0));
        let lower = b.add(&format!("leg_{leg}_lower"), Some(upper), Vec3::new(0.0, -0.2, 0.0));
        b.add(&format!("leg_{leg}_paw"), Some(lower), Vec3::new(0.0, -0.2, 0.02));
    }
    let tail = b.chain(
        "tail",
        pelvis,
        &[
            Vec3::new(0.0, 0.03, -0.15),
            Vec3::new(0.0, 0.03, -0.1),
            Vec3::new(0.0, 0.0, -0.1),
            Vec3::new(0.0, -0.02, -0.1),
        ],
    );
    let skeleton = Skeleton::generic(b.0)?;

    let mut tail_chain = soft_chain("tail", &tail, 1.2);
    tail_chain.collider_groups.push("hindquarters".into());
    let springs = SpringSet {
        chains: vec![tail_chain, soft_chain("ear_l", &ear_l, 0.8), soft_chain("ear_r", &ear_r, 0.8)],
        groups: vec![ColliderGroup {
            name: "hindquarters".into(),
            colliders: vec![Collider {
                bone: pelvis,
                shape: ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.1 },
            }],
        }],
    };
    springs.validate(&skeleton)?;
    Ok(CreatureRig { skeleton, springs })
}

/// A jelly blob: a `core` root with a `body` above it, a two-bone wobbling arm on each side and a three-bone tuft on
/// top. No head, no limbs, nothing humanoid. Each arm and the tuft is a spring chain.
///
/// # Errors
/// Only if the fixture itself is broken (a test pins that it is not).
pub fn blob_rig() -> Result<CreatureRig, FixtureError> {
    let mut b = Bones::default();
    let core = b.add("core", None, Vec3::new(0.0, 0.3, 0.0));
    let body = b.add("body", Some(core), Vec3::new(0.0, 0.15, 0.0));
    let arm_l = b.chain("wobble_l", body, &[Vec3::new(0.2, 0.0, 0.0), Vec3::new(0.1, -0.02, 0.0)]);
    let arm_r = b.chain("wobble_r", body, &[Vec3::new(-0.2, 0.0, 0.0), Vec3::new(-0.1, -0.02, 0.0)]);
    let tuft = b.chain("tuft", body, &[Vec3::new(0.0, 0.2, 0.0), Vec3::new(0.0, 0.08, 0.0), Vec3::new(0.0, 0.08, 0.0)]);
    let skeleton = Skeleton::generic(b.0)?;
    let springs = SpringSet {
        chains: vec![
            soft_chain("wobble_l", &arm_l, 0.6),
            soft_chain("wobble_r", &arm_r, 0.6),
            soft_chain("tuft", &tuft, 1.0),
        ],
        groups: Vec::new(),
    };
    springs.validate(&skeleton)?;
    Ok(CreatureRig { skeleton, springs })
}

/// A six-second score: a smile that builds, a wide-eyed beat, a head turn, a wave and a marker.
///
/// # Errors
/// Only if the fixture itself is broken (a test pins that it is not).
pub fn demo_score() -> Result<Score, FixtureError> {
    let key = |time: f64, value: f32, over: f32| Key { time: Seconds(time), value, over, ease: Ease::OutCubic };
    let track = |channel, keys| ChannelTrack { channel, initial: 0.0, keys };
    Ok(Score {
        duration: Seconds(6.0),
        tracks: vec![
            track(Channel::MOUTH_SMILE_LEFT, vec![key(0.5, 0.8, 0.4), key(4.0, 0.1, 0.6)]),
            track(Channel::MOUTH_SMILE_RIGHT, vec![key(0.5, 0.8, 0.4), key(4.0, 0.1, 0.6)]),
            track(Channel::EYE_WIDE_LEFT, vec![key(2.0, 1.0, 0.1), key(2.4, 0.0, 0.3)]),
            track(Channel::EYE_WIDE_RIGHT, vec![key(2.0, 1.0, 0.1), key(2.4, 0.0, 0.3)]),
            track(Channel::HEAD_YAW, vec![key(3.0, 0.4, 0.5), key(5.0, 0.0, 0.5)]),
        ],
        cues: vec![
            Cue { time: Seconds(1.0), kind: CueKind::Clip { clip: ClipId::new("wave")?, weight: 1.0, fade_s: 0.2 } },
            Cue { time: Seconds(5.5), kind: CueKind::Marker { name: "end".into() } },
        ],
    })
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // exact values copied through; small loop counters
mod tests {
    use super::*;

    #[test]
    fn the_fixtures_are_valid() {
        assert_eq!(humanoid_skeleton().unwrap().len(), 55);
        let rig = skeleton_with_hair().unwrap();
        assert_eq!(rig.skeleton.len(), 55 + HAIR_CHAINS * HAIR_BONES_PER_CHAIN);
        assert_eq!(rig.springs.validate(&rig.skeleton), Ok(()));
        assert_eq!(rig.chains.len(), HAIR_CHAINS);
        assert!(demo_score().unwrap().compile().is_ok());
    }

    #[test]
    fn the_creatures_are_valid_generic_skeletons_with_valid_springs() {
        for (rig, bones) in [(quadruped_rig().unwrap(), QUADRUPED_BONES), (blob_rig().unwrap(), BLOB_BONES)] {
            assert_eq!(rig.skeleton.len(), bones);
            assert!(!rig.skeleton.is_humanoid() && rig.skeleton.humanoid_map().is_none());
            assert_eq!(rig.springs.validate(&rig.skeleton), Ok(()));
            // Neither is a humanoid in disguise: the strict constructor refuses the same bones.
            assert_eq!(
                Skeleton::new(rig.skeleton.bones().to_vec()),
                Err(SkeletonError::MissingRequired(HumanoidBone::Hips))
            );
        }
        assert_eq!(quadruped_rig().unwrap().springs.chains.len(), 3);
        assert_eq!(blob_rig().unwrap().springs.chains.len(), 3);
    }
}
