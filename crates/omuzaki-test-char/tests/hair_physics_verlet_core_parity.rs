//! Parity of the hair simulation core (`omuzaki_char::hair::physics::verlet`) with the prototype's (`hair.js` 379-410
//! and 495-540): the particles of every simulated clump after settling in the dump's pose, and after the scripted head
//! turn.
//!
//! The oracle is the sample character's hair dump, private test material kept in the local assets folder: without it
//! (always, in CI and in the cloud) every test here skips with a message. The style's physics numbers and colliders are
//! the character's too, so a local file next to the dump supplies them, `state/game/hair_physics.local.json`, naming
//! the variant it is for:
//!
//! ```json
//! { "variant": "game", "head_drop": 0.016, "physics": { ... HairPhysics, colliders included ... } }
//! ```
//!
//! `head_drop` is how far the build lowered the head joint (the dump's `headDropApplied`, metres). The dump's normalised
//! bones do not carry it, and the bind inverses it was skinned against predate it, so the posed head bone is lowered by
//! it here and the rest head bone is not.
//!
//! The pose is rebuilt from the dump's normalised bones (rest rotations identity, so a bone's bind matrix is the
//! translation of its rest offsets down the humanoid hierarchy) under the character group's world matrix. Particle
//! stiffness is taken from the dump, so the test checks the simulation rather than the stiffness profile.
//!
//! **Tolerance.** The prototype runs in `f64`, this port in `f32`. Settled: 5e-4 m RMS and 2e-3 m max; after the head
//! turn: 2e-3 m RMS. The measured values are printed so the first run with the dump can tighten them.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason

use std::collections::BTreeMap;
use std::fs;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::hair::physics::verlet::ChainSet;
use omuzaki_char_api::{Bulge, Falloff, Follow, GroupId, HairClump, HairPhysics, JointRef, PoseSource, Taper};
use omuzaki_core::glam::{Mat4, Quat};
use omuzaki_test::jsref::{BoneState, HairDump, JsRef};
use serde::Deserialize;

/// Settled tolerances, metres.
const SETTLED_RMS: f32 = 5e-4;
const SETTLED_MAX: f32 = 2e-3;
/// Head-turn tolerance, metres RMS.
const YAWED_RMS: f32 = 2e-3;

#[derive(Deserialize)]
struct Local {
    variant: String,
    #[serde(default)]
    head_drop: f32,
    physics: HairPhysics,
}

/// The dump's normalised bones as a pose: skinning matrix = root x posed world x rest world inverse.
struct DumpPose {
    root: Mat4,
    /// How far the posed head bone is lower than the dump's normalised one, metres.
    head_drop: f32,
    bones: BTreeMap<String, BoneState>,
}

impl DumpPose {
    /// The bone's world matrix down the humanoid hierarchy (skipping bones the dump lacks); `posed` false gives the
    /// rest (translations only).
    fn world(&self, bone: HumanoidBone, posed: bool) -> Option<Mat4> {
        let state = self.bones.get(bone.name())?;
        let rotation = if posed { state.quaternion } else { Quat::IDENTITY };
        let mut position = state.position;
        if posed && bone == HumanoidBone::Head {
            position.y -= self.head_drop;
        }
        let local = Mat4::from_rotation_translation(rotation, position);
        let mut parent = bone.parent();
        while let Some(p) = parent {
            if self.bones.contains_key(p.name()) {
                return Some(self.world(p, posed)? * local);
            }
            parent = p.parent();
        }
        Some(local)
    }
}

impl PoseSource for DumpPose {
    fn skin_matrix(&self, joint: &JointRef) -> Option<Mat4> {
        let JointRef::Humanoid(bone) = joint else { return None };
        Some(self.root * self.world(*bone, true)? * self.world(*bone, false)?.inverse())
    }

    fn root(&self) -> Mat4 {
        self.root
    }
}

/// The dump, its pose and the character's physics, or `None` (after saying why) so the caller skips.
fn private() -> Option<(HairDump, DumpPose, HairPhysics)> {
    let js = JsRef::open()?;
    let path = js.root().join("state/game/hair_physics.local.json");
    let Ok(text) = fs::read_to_string(&path) else {
        eprintln!("skipped: no local hair physics numbers at {}", path.display());
        return None;
    };
    let local: Local = serde_json::from_str(&text).expect("the local hair physics file parses");
    let dump = js.hair(&local.variant).unwrap();
    let root = js.geometry(&local.variant).unwrap().group_matrix_world;
    let fixed_dt = 1.0 / local.physics.fixed_hz;
    assert!(
        (dump.sim.dt - fixed_dt).abs() < 1e-9,
        "the dump stepped at {} s, the physics at {fixed_dt} s",
        dump.sim.dt
    );
    let pose = DumpPose { root, head_drop: local.head_drop, bones: dump.bones_settled.clone() };
    Some((dump, pose, local.physics))
}

/// The dump's simulated clumps as kit clumps (stiffness comes from the dump afterwards).
fn clumps(dump: &HairDump) -> Vec<HairClump> {
    let group = GroupId::new("dump").unwrap();
    dump.clumps
        .iter()
        .filter(|c| c.particles.is_some())
        .map(|c| HairClump {
            group: group.clone(),
            rest: c.rest.clone(),
            width: Taper::default(),
            thickness: Bulge::default(),
            stiffness: Falloff::default(),
            shade: c.shade,
            simulate: true,
            follow: if c.follow.as_deref() == Some("body") { Follow::Body } else { Follow::Head },
            tint: None,
        })
        .collect()
}

/// Builds the set, checks its particles against the dump's, and loads the dump's stiffness.
fn chain_set(dump: &HairDump, physics: &HairPhysics) -> ChainSet {
    let mut set = ChainSet::from_clumps(&clumps(dump), physics, &[]).unwrap();
    let particles: Vec<_> = dump.clumps.iter().filter_map(|c| c.particles.as_ref()).collect();
    assert_eq!(set.chains().len(), particles.len());
    let mut stiffness = Vec::new();
    for (n, (range, p)) in set.chains().zip(&particles).enumerate() {
        assert_eq!(&set.rest_indices()[range.clone()], p.rest_index.as_slice(), "chain {n}: particle placement");
        let ours = &set.segment_lengths()[range.start + 1..range.end];
        for (a, b) in ours.iter().zip(&p.segment_len) {
            assert!((a - b).abs() < 1e-5, "chain {n}: segment {a} vs {b}");
        }
        stiffness.extend_from_slice(&p.stiffness);
    }
    set.set_stiffness(&stiffness).unwrap();
    set
}

/// RMS and max distance between the set's particles and the dump's.
fn error(set: &ChainSet, dump: &HairDump, yawed: bool) -> (f32, f32) {
    let expected =
        dump.clumps.iter().filter_map(|c| c.particles.as_ref()).flat_map(|p| if yawed { &p.yawed } else { &p.settled });
    let (mut sum, mut max, mut n) = (0.0_f64, 0.0_f32, 0_u32);
    for (a, b) in set.positions().iter().zip(expected) {
        let d = a.distance(*b);
        sum += f64::from(d * d);
        max = max.max(d);
        n += 1;
    }
    assert_eq!(n as usize, set.positions().len(), "particle counts");
    #[allow(clippy::cast_possible_truncation)]
    ((sum / f64::from(n)).sqrt() as f32, max)
}

fn settled(dump: &HairDump, pose: &DumpPose, physics: &HairPhysics) -> ChainSet {
    let mut set = chain_set(dump, physics);
    set.reset(pose).unwrap();
    set.settle(dump.sim.settle_steps);
    set
}

#[test]
fn settled_particles_match_the_prototypes() {
    let Some((dump, pose, physics)) = private() else { return };
    let set = settled(&dump, &pose, &physics);
    let (rms, max) = error(&set, &dump, false);
    eprintln!("settled ({} steps): {rms:.2e} m RMS, {max:.2e} m max", dump.sim.settle_steps);
    assert!(rms <= SETTLED_RMS && max <= SETTLED_MAX, "{rms:.2e} m RMS, {max:.2e} m max");
}

#[test]
fn the_head_turn_matches_the_prototypes() {
    let Some((dump, mut pose, physics)) = private() else { return };
    let mut set = settled(&dump, &pose, &physics);
    for step in &dump.motion {
        for (name, rotation) in [("neck", step.neck), ("head", step.head)] {
            pose.bones.get_mut(name).expect("the dump has the bone").quaternion = rotation;
        }
        set.targets(&pose).unwrap();
        set.settle(1);
    }
    let (rms, max) = error(&set, &dump, true);
    eprintln!("after the {}-step {} degree turn: {rms:.2e} m RMS, {max:.2e} m max", dump.motion.len(), dump.yaw_deg);
    assert!(rms <= YAWED_RMS, "{rms:.2e} m RMS");
}
