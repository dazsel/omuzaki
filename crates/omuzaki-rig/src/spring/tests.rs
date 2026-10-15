//! Tests for the spring solver: agreement with the spec's algorithm (`reference`), the rules the spec leaves to the
//! implementation, and the contract of `SpringSolver`.

#![allow(
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names
)] // tests compare copied values and cast small loop counters

use omuzaki_anim_api::{
    BoneDef, BoneId, ColliderGroup, ColliderShape, HumanoidBone, PHYSICS_HZ, PHYSICS_STEP, Pose, Skeleton, SpringChain,
    SpringError, SpringParams, SpringSet, SpringSolver, Tick, Transform,
};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::rng::Rng;

use super::VrmSpringSolver;
use super::reference::Reference;

/// The agreement the spec comparison must reach: `f32` solver against `f64` oracle, rotations (quaternion components)
/// and joint positions (metres).
const TOLERANCE: f64 = 1e-4;

/// A humanoid with hair bones under the head, and an animated pose for it.
struct Fixture {
    skeleton: Skeleton,
    hips: BoneId,
    head: BoneId,
}

fn bone(name: String, parent: BoneId, translation: Vec3, rotation: Quat) -> BoneDef {
    BoneDef { name, parent: Some(parent), rest: Transform { translation, rotation }, humanoid: None }
}

impl Fixture {
    /// The 15 required humanoid bones plus whatever `extra` adds (it gets the head's id and returns bones to append).
    fn with(extra: impl FnOnce(BoneId, u16) -> Vec<BoneDef>) -> Self {
        let required: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect();
        let base = Skeleton::humanoid(&required, |b| {
            Transform::from_translation(if b == HumanoidBone::Hips { Vec3::Y } else { Vec3::Y * 0.1 })
        })
        .unwrap();
        let (hips, head) =
            (base.humanoid_bone(HumanoidBone::Hips).unwrap(), base.humanoid_bone(HumanoidBone::Head).unwrap());
        let mut bones = base.bones().to_vec();
        let added = extra(head, bones.len() as u16);
        bones.extend(added);
        Self { skeleton: Skeleton::new(bones).unwrap(), hips, head }
    }

    /// `chains` chains of `joints` hair bones each, hanging from the head. Rest rotations are not identity and the
    /// bones do not hang straight down, so the maths is exercised away from the easy axes.
    fn hair(chains: usize, joints: usize) -> (Self, Vec<Vec<BoneId>>) {
        let mut ids = vec![vec![]; chains];
        let fixture = Self::with(|head, first| {
            let mut bones = Vec::new();
            for (c, chain) in ids.iter_mut().enumerate() {
                let mut parent = head;
                for j in 0..joints {
                    let id = BoneId(first + bones.len() as u16);
                    let (c_f, j_f) = (c as f32, j as f32);
                    bones.push(bone(
                        format!("hair_{c}_{j}"),
                        parent,
                        Vec3::new(0.02 * (c_f - 1.0), -0.06 - 0.005 * j_f, 0.015 * (j_f - 1.0)),
                        Quat::from_euler(omuzaki_core::glam::EulerRot::XYZ, 0.1 * j_f, 0.05 * c_f, 0.12 * (j_f + 1.0)),
                    ));
                    chain.push(id);
                    parent = id;
                }
            }
            bones
        });
        (fixture, ids)
    }

    /// The animated pose at `frame`: hips travel and turn, the head nods and shakes. Hair bones stay at rest.
    fn pose_at(&self, frame: u64) -> Pose {
        let t = frame as f32;
        let mut pose = self.skeleton.rest_pose();
        let hips = &mut pose.local[self.hips.index()];
        hips.translation += Vec3::new(0.4 * (0.07 * t).sin(), 0.05 * (0.11 * t).sin(), 0.2 * (0.05 * t).cos());
        hips.rotation = Quat::from_rotation_y(0.5 * (0.05 * t).sin());
        pose.local[self.head.index()].rotation = Quat::from_euler(
            omuzaki_core::glam::EulerRot::XYZ,
            0.3 * (0.13 * t).sin(),
            0.5 * (0.09 * t).sin(),
            0.2 * (0.17 * t).sin(),
        );
        pose
    }
}

/// Asserts two poses are bit-identical, naming the first bone that differs (`Pose`'s own `Debug` prints nothing useful).
fn assert_same_pose(a: &Pose, b: &Pose, what: &str) {
    if let Some(i) = (0..omuzaki_anim_api::MAX_BONES).find(|&i| a.local[i] != b.local[i]) {
        panic!("{what}: bone {i} differs: {:?} vs {:?}", a.local[i], b.local[i]);
    }
}

/// How far apart two rotations are, as the largest difference of any component (sign-aligned, so `q` and `-q` agree).
/// Angles from `acos` are useless below about 5e-4 rad in `f32`; components keep their precision.
fn distance(a: Quat, b: Quat) -> f32 {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    (a - b).to_array().iter().fold(0.0, |m, d| m.max(d.abs()))
}

fn windy() -> SpringParams {
    SpringParams { hit_radius: 0.01, stiffness: 2.0, gravity_power: 1.0, drag_force: 0.3, ..SpringParams::default() }
}

fn set_of(chains: Vec<SpringChain>, groups: Vec<ColliderGroup>) -> SpringSet {
    SpringSet { chains, groups }
}

/// How well the solver agrees with the oracle over a run.
#[derive(Debug, Default)]
struct Agreement {
    rotation: f64,
    position: f64,
    /// The largest angle any swinging bone turned from rest, radians: proves the run actually moved the hair.
    swing: f64,
}

/// Steps the solver and the oracle through `frames` frames of the fixture's animation and measures the worst
/// disagreement in rotations and joint positions.
fn compare_with_spec(fixture: &Fixture, set: &SpringSet, fps: f32, frames: u64) -> Agreement {
    let skeleton = &fixture.skeleton;
    let mut solver = VrmSpringSolver::new(set, skeleton).unwrap();
    let mut oracle = Reference::new(skeleton, set, &fixture.pose_at(0));
    let swinging: Vec<usize> =
        set.chains.iter().flat_map(|c| &c.joints[..c.joints.len() - 1]).map(|j| j.bone.index()).collect();
    let joints: Vec<usize> = set.chains.iter().flat_map(|c| &c.joints).map(|j| j.bone.index()).collect();
    let mut world = Pose::default();
    let mut worst = Agreement::default();
    for frame in 0..frames {
        let input = fixture.pose_at(frame);
        let mut out = input.clone();
        solver.step(Tick::at_rate(frame, fps), skeleton, &mut out);
        let substeps = solver.stats().substeps;
        if substeps == 0 {
            oracle.hold(&input);
        }
        for _ in 0..substeps {
            oracle.substep(&input);
        }
        for &b in &swinging {
            let (got, want) = (out.local[b].rotation.as_dquat(), oracle.rotation(b));
            let sign = if got.dot(want) < 0.0 { -1.0 } else { 1.0 };
            let diff = (got * sign - want).to_array().map(f64::abs).into_iter().fold(0.0, f64::max);
            worst.rotation = worst.rotation.max(diff);
            let rest = skeleton.bones()[b].rest.rotation.as_dquat();
            worst.swing = worst.swing.max(2.0 * got.dot(rest).abs().min(1.0).acos());
        }
        skeleton.world_pose(&out, &mut world);
        for &b in &joints {
            let want = oracle.world(b).w_axis.truncate();
            worst.position = worst.position.max((world.local[b].translation.as_dvec3() - want).length());
        }
    }
    worst
}

fn assert_agrees(agreement: &Agreement, least_swing: f64) {
    assert!(agreement.rotation < TOLERANCE, "rotations differ from the spec by {}", agreement.rotation);
    assert!(agreement.position < TOLERANCE, "joint positions differ from the spec by {} m", agreement.position);
    assert!(agreement.swing > least_swing, "the hair only swung {} rad: the test proves little", agreement.swing);
}

fn chains_of(ids: &[Vec<BoneId>], params: SpringParams) -> Vec<SpringChain> {
    ids.iter().enumerate().map(|(i, bones)| SpringChain::new(format!("hair_{i}"), bones, params)).collect()
}

fn ball(bone: BoneId, offset: Vec3, radius: f32) -> omuzaki_anim_api::Collider {
    omuzaki_anim_api::Collider { bone, shape: ColliderShape::Sphere { offset, radius } }
}

#[test]
fn matches_the_spec_on_free_chains() {
    let (fixture, ids) = Fixture::hair(3, 4);
    let set = set_of(chains_of(&ids, windy()), vec![]);
    assert_agrees(&compare_with_spec(&fixture, &set, 60.0, 300), 0.3);
}

#[test]
fn matches_the_spec_with_tapered_parameters() {
    let (fixture, ids) = Fixture::hair(2, 5);
    let (root, tip) = (windy(), SpringParams { stiffness: 0.3, drag_force: 0.1, gravity_power: 2.0, ..windy() });
    let chains = ids.iter().enumerate().map(|(i, b)| SpringChain::tapered(format!("c{i}"), b, root, tip)).collect();
    assert_agrees(&compare_with_spec(&fixture, &set_of(chains, vec![]), 60.0, 300), 0.3);
}

/// Every frame rate the solver must agree with the oracle at (#1849): the common display and video rates, 50 fps
/// (not a divisor of 120, so whole and partial sub-steps alternate) and 144 fps (shorter than a sub-step, so some
/// frames run none and only hold).
const FRAME_RATES: [f32; 6] = [30.0, 50.0, 60.0, 90.0, 120.0, 144.0];

#[test]
fn matches_the_spec_at_every_frame_rate() {
    let (fixture, ids) = Fixture::hair(3, 4);
    let free = set_of(chains_of(&ids, windy()), vec![]);
    // The hold maps the tails out of centre space and re-aims under the new pose, so it gets a centre and a collider.
    let mut chains = chains_of(&ids, windy());
    for chain in &mut chains {
        chain.center = Some(fixture.hips);
        chain.collider_groups = vec!["head".into()];
    }
    let head =
        ColliderGroup { name: "head".into(), colliders: vec![ball(fixture.head, Vec3::new(0.0, -0.1, 0.0), 0.12)] };
    let centred = set_of(chains, vec![head]);
    for (name, set) in [("free", &free), ("centre and collider", &centred)] {
        for fps in FRAME_RATES {
            let agreement = compare_with_spec(&fixture, set, fps, 300);
            println!("{name}, {fps} fps: rotation {:.1e}, position {:.1e} m", agreement.rotation, agreement.position);
            assert_agrees(&agreement, 0.1);
        }
    }
}

/// #1849's cause: at 144 fps a frame is shorter than a sub-step, so one frame in six owes none. The solver holds the
/// tails and re-aims the bones from that frame's pose; a driver that gives the oracle nothing on such a frame compares
/// the solver with the previous frame's pose (0.03 m apart) although the two simulations never diverged.
#[test]
fn a_frame_with_no_sub_step_is_one_in_six_at_144_fps_and_the_oracle_holds_it_too() {
    let (fixture, ids) = Fixture::hair(1, 4);
    let set = set_of(chains_of(&ids, windy()), vec![]);
    let mut solver = VrmSpringSolver::new(&set, &fixture.skeleton).unwrap();
    let counts: Vec<u32> = (0..12)
        .map(|frame| {
            let mut pose = fixture.pose_at(frame);
            solver.step(Tick::at_rate(frame, 144.0), &fixture.skeleton, &mut pose);
            solver.stats().substeps
        })
        .collect();
    assert_eq!(counts, [0, 1, 1, 1, 1, 1, 0, 1, 1, 1, 1, 1]);

    // Without the hold, the oracle lags a frame on exactly those frames: the comparison would fail.
    let mut oracle = Reference::new(&fixture.skeleton, &set, &fixture.pose_at(0));
    let mut solver = VrmSpringSolver::new(&set, &fixture.skeleton).unwrap();
    let tip = ids[0][3].index();
    let mut world = Pose::default();
    let mut lag = 0.0_f64;
    for frame in 0..7 {
        let mut pose = fixture.pose_at(frame);
        solver.step(Tick::at_rate(frame, 144.0), &fixture.skeleton, &mut pose);
        for _ in 0..solver.stats().substeps {
            oracle.substep(&fixture.pose_at(frame));
        }
        fixture.skeleton.world_pose(&pose, &mut world);
        lag = lag.max((world.local[tip].translation.as_dvec3() - oracle.world(tip).w_axis.truncate()).length());
    }
    assert!(lag > 100.0 * TOLERANCE, "an unheld oracle should lag on frame 6, but it is only {lag} m off");
}

#[test]
fn matches_the_spec_in_centre_space() {
    let (fixture, ids) = Fixture::hair(2, 4);
    let in_centre = |centre: Option<BoneId>| {
        let mut chains = chains_of(&ids, windy());
        for chain in &mut chains {
            chain.center = centre;
        }
        set_of(chains, vec![])
    };
    for centre in [fixture.hips, fixture.head] {
        assert_agrees(&compare_with_spec(&fixture, &in_centre(Some(centre)), 60.0, 300), 0.1);
    }

    // The centre must matter, or the comparison above says nothing about it: walking the hips about throws
    // world-space hair around much more than hair that travels with the hips.
    let swing = |centre| compare_with_spec(&fixture, &in_centre(centre), 60.0, 300).swing;
    assert!(swing(None) > swing(Some(fixture.hips)) + 0.05, "{} vs {}", swing(None), swing(Some(fixture.hips)));
}

#[test]
fn matches_the_spec_with_sphere_and_capsule_colliders() {
    let (fixture, ids) = Fixture::hair(3, 4);
    let spine = fixture.skeleton.humanoid_bone(HumanoidBone::Spine).unwrap();
    let groups = vec![
        ColliderGroup { name: "head".into(), colliders: vec![ball(fixture.head, Vec3::new(0.0, -0.1, 0.0), 0.12)] },
        ColliderGroup {
            name: "spine".into(),
            colliders: vec![omuzaki_anim_api::Collider {
                bone: spine,
                shape: ColliderShape::Capsule {
                    offset: Vec3::new(0.0, 0.0, 0.0),
                    radius: 0.07,
                    tail: Vec3::new(0.05, -0.15, 0.1),
                },
            }],
        },
    ];
    let with_colliders = |names: &[&str]| {
        let mut chains = chains_of(&ids, windy());
        for chain in &mut chains {
            chain.collider_groups = names.iter().map(|n| (*n).to_owned()).collect();
        }
        set_of(chains, groups.clone())
    };
    for names in [&["head"][..], &["spine"][..], &["head", "spine"][..]] {
        assert_agrees(&compare_with_spec(&fixture, &with_colliders(names), 60.0, 300), 0.3);
    }

    // The colliders must be hit, or the comparison says nothing about them: the same run without them ends elsewhere.
    let run = |set: &SpringSet| {
        let mut solver = VrmSpringSolver::new(set, &fixture.skeleton).unwrap();
        let mut out = fixture.pose_at(0);
        for frame in 0..120 {
            out = fixture.pose_at(frame);
            solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut out);
        }
        out
    };
    let (free, blocked) = (run(&with_colliders(&[])), run(&with_colliders(&["head", "spine"])));
    let moved = ids[0]
        .iter()
        .map(|b| distance(free.local[b.index()].rotation, blocked.local[b.index()].rotation))
        .fold(0.0, f32::max);
    assert!(moved > 0.02, "colliders changed the hair by only {moved}");
}

/// The three extended colliders, placed where the hair of `Fixture::hair(3, 4)` swings into them.
fn extended_colliders() -> [(&'static str, ColliderShape); 5] {
    [
        // A capsule whose ends coincide is a ball: it must still collide (the spec's 0/0 would not).
        (
            "capsule_of_zero_length",
            ColliderShape::Capsule {
                offset: Vec3::new(0.0, -0.12, 0.0),
                radius: 0.1,
                tail: Vec3::new(0.0, -0.12, 0.0),
            },
        ),
        (
            "capsule_inside_of_zero_length",
            ColliderShape::CapsuleInside {
                offset: Vec3::new(0.0, -0.12, 0.0),
                radius: 0.14,
                tail: Vec3::new(0.0, -0.12, 0.0),
            },
        ),
        ("sphere_inside", ColliderShape::SphereInside { offset: Vec3::new(0.0, -0.12, 0.0), radius: 0.14 }),
        (
            "capsule_inside",
            ColliderShape::CapsuleInside {
                offset: Vec3::new(0.0, -0.05, 0.0),
                radius: 0.07,
                tail: Vec3::new(0.0, -0.2, 0.02),
            },
        ),
        (
            "plane",
            ColliderShape::Plane { offset: Vec3::new(0.0, -0.2, 0.0), normal: Vec3::new(0.1, 1.0, 0.05).normalize() },
        ),
    ]
}

#[test]
fn matches_the_spec_with_inside_and_plane_colliders() {
    let (fixture, ids) = Fixture::hair(2, 4);
    for (name, shape) in extended_colliders() {
        let group = ColliderGroup {
            name: name.into(),
            colliders: vec![omuzaki_anim_api::Collider { bone: fixture.head, shape }],
        };
        let with = |attach: bool| {
            let mut chains = chains_of(&ids, SpringParams { stiffness: 4.0, drag_force: 0.4, ..windy() });
            if attach {
                for chain in &mut chains {
                    chain.collider_groups = vec![name.to_owned()];
                }
            }
            set_of(chains, vec![group.clone()])
        };
        let set = with(true);
        set.validate(&fixture.skeleton).unwrap();
        assert_agrees(&compare_with_spec(&fixture, &set, 60.0, 300), 0.1);
        assert_agrees(&compare_with_spec(&fixture, &set, 30.0, 300), 0.1);

        // The collider must be hit, or the comparison says nothing about it.
        let run = |set: &SpringSet| {
            let mut solver = VrmSpringSolver::new(set, &fixture.skeleton).unwrap();
            let mut out = fixture.pose_at(0);
            for frame in 0..120 {
                out = fixture.pose_at(frame);
                solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut out);
            }
            out
        };
        let (free, blocked) = (run(&with(false)), run(&set));
        let moved = ids[0]
            .iter()
            .map(|b| distance(free.local[b.index()].rotation, blocked.local[b.index()].rotation))
            .fold(0.0, f32::max);
        assert!(moved > 0.02, "{name} changed the hair by only {moved}");
    }
}

#[test]
fn matches_the_spec_on_seeded_random_scenes_to_1e_5() {
    let (fixture, ids) = Fixture::hair(2, 4);
    let mut rng = Rng::new(984);
    let mut point = |spread: f32| {
        Vec3::new(rng.range_f32(-spread, spread), rng.range_f32(-0.2, -0.05), rng.range_f32(-spread, spread))
    };
    let mut engaged = 0;
    for scene in 0..24 {
        let shape = match scene % 3 {
            0 => ColliderShape::SphereInside { offset: point(0.03), radius: 0.12 + 0.02 * (scene % 5) as f32 },
            1 => ColliderShape::CapsuleInside {
                offset: point(0.03),
                radius: 0.07 + 0.01 * (scene % 4) as f32,
                tail: point(0.03),
            },
            _ => ColliderShape::Plane {
                offset: point(0.05),
                normal: (point(0.3) * Vec3::new(1.0, -5.0, 1.0)).normalize(),
            },
        };
        let group = ColliderGroup {
            name: "g".into(),
            colliders: vec![omuzaki_anim_api::Collider { bone: fixture.head, shape }],
        };
        let mut chains = chains_of(&ids, SpringParams { stiffness: 4.0, drag_force: 0.4, ..windy() });
        for chain in &mut chains {
            chain.collider_groups = vec!["g".into()];
        }
        let set = set_of(chains, vec![group]);
        set.validate(&fixture.skeleton).unwrap();
        let agreement = compare_with_spec(&fixture, &set, 60.0, 200);
        // Joint positions agree to 1e-5 m. A violent scene (over 1 rad of swing) carries the `f32` rounding of a whole
        // chain's turn: 2e-5 m there (the worst is 1.2e-5), and quaternion components to 5e-5 (the worst is 4.2e-5).
        let position = if agreement.swing > 1.0 { 2e-5 } else { 1e-5 };
        assert!(agreement.rotation < 5e-5 && agreement.position < position, "scene {scene} {shape:?}: {agreement:?}");
        engaged += i32::from(agreement.swing > 0.43); // the free run's swing is 0.42 rad
    }
    assert!(engaged >= 12, "only {engaged} of 24 scenes changed the swing: the colliders were mostly missed");
}

/// A root bone at the head's origin with one bone of `length` hanging from it, simulated under `shape` on the head.
/// Returns, per frame, the head's world transform and the world positions of the root and the tail.
fn swing_into(shape: Option<ColliderShape>, length: f32, seed: u64) -> Vec<(Transform, Vec3, Vec3)> {
    let mut ids = (BoneId(0), BoneId(0));
    let fixture = Fixture::with(|head, first| {
        ids = (BoneId(first), BoneId(first + 1));
        vec![
            bone("root".into(), head, Vec3::ZERO, Quat::IDENTITY),
            bone("tip".into(), ids.0, Vec3::NEG_Y * length, Quat::IDENTITY),
        ]
    });
    let params = SpringParams { stiffness: 0.5, gravity_power: 1.0, drag_force: 0.1, ..SpringParams::default() };
    let mut chain = SpringChain::new("c", &[ids.0, ids.1], params);
    chain.collider_groups = vec!["g".into()];
    let colliders = shape.map(|shape| omuzaki_anim_api::Collider { bone: fixture.head, shape }).into_iter().collect();
    let set = set_of(vec![chain], vec![ColliderGroup { name: "g".into(), colliders }]);
    set.validate(&fixture.skeleton).unwrap();
    let mut solver = VrmSpringSolver::new(&set, &fixture.skeleton).unwrap();
    let mut rng = Rng::new(seed);
    let mut world = Pose::default();
    (0..90)
        .map(|frame| {
            let mut pose = fixture.skeleton.rest_pose();
            pose.local[fixture.head.index()].rotation = Quat::from_euler(
                omuzaki_core::glam::EulerRot::XYZ,
                rng.range_f32(-1.5, 1.5),
                rng.range_f32(-1.5, 1.5),
                rng.range_f32(-1.5, 1.5),
            );
            solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pose);
            fixture.skeleton.world_pose(&pose, &mut world);
            (
                world.local[fixture.head.index()],
                world.local[ids.0.index()].translation,
                world.local[ids.1.index()].translation,
            )
        })
        .collect()
}

mod properties {
    use proptest::prelude::*;

    use super::*;

    fn at(frame: &Transform, p: Vec3) -> Vec3 {
        frame.translation + frame.rotation * p
    }

    /// Distance from `p` to the segment `a`..`b`, written independently of the solver.
    fn to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
        let ab = b - a;
        let t = if ab.length_squared() > 0.0 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
        (p - (a + ab * t)).length()
    }

    fn vec3() -> impl Strategy<Value = Vec3> {
        prop::array::uniform3(-0.1_f32..0.1).prop_map(Vec3::from_array)
    }

    fn bits(frames: &[(Transform, Vec3, Vec3)]) -> Vec<u32> {
        frames
            .iter()
            .flat_map(|(_, root, tail)| [root.to_array(), tail.to_array()])
            .flatten()
            .map(f32::to_bits)
            .collect()
    }

    /// Whether `frames` (a run without the collider) ever puts the tail where the collider would push it: more than
    /// 1 mm behind `room`'s zero, with `room` measured in the head's frame of that frame.
    fn reaches(free: &[(Transform, Vec3, Vec3)], room: impl Fn(&Transform, Vec3) -> f32) -> bool {
        free.iter().any(|(head, _, tail)| room(head, *tail) < -1e-3)
    }

    /// The checks every collider property shares: the bone keeps its length through the whole step, the run is
    /// bit-identical when repeated, and the collider changed the swing (so the contacts were real, not skipped).
    fn check(
        shape: ColliderShape,
        length: f32,
        seed: u64,
        free: &[(Transform, Vec3, Vec3)],
    ) -> Result<(), TestCaseError> {
        let frames = swing_into(Some(shape), length, seed);
        for (_, root, tail) in &frames {
            prop_assert!(((*tail - *root).length() - length).abs() <= 1e-5, "bone length changed");
        }
        prop_assert_eq!(bits(&frames), bits(&swing_into(Some(shape), length, seed)));
        prop_assert!(bits(&frames) != bits(free), "the collider never touched the tail");
        Ok(())
    }

    proptest! {
        /// Inside shapes whose wall the bone can reach (the radius is less than the root's room plus the bone's
        /// length). The push itself is held to 1e-5 by `collide`'s `push_properties`; here the whole step keeps the
        /// bone length and is deterministic, and the tail does hit the wall.
        #[test]
        fn a_tail_that_reaches_the_wall_of_an_inside_shape_collides(
            centre in vec3(), end in vec3(), length in 0.05_f32..0.15, reach in 0.2_f32..0.8, capsule: bool, seed: u64,
        ) {
            // The root sits at the head's origin, so its distance to the shape is its distance to the centre or axis.
            let axis_end = if capsule { end } else { centre };
            let radius = to_segment(Vec3::ZERO, centre, axis_end) + length * reach;
            let shape = if capsule {
                ColliderShape::CapsuleInside { offset: centre, radius, tail: end }
            } else {
                ColliderShape::SphereInside { offset: centre, radius }
            };
            let free = swing_into(None, length, seed);
            prop_assume!(reaches(&free, |head, tail| radius - to_segment(tail, at(head, centre), at(head, axis_end))));
            check(shape, length, seed, &free)?;
        }

        /// Planes the bone can reach (the root is less than a bone length in front of the plane).
        #[test]
        fn a_tail_that_reaches_a_plane_collides(
            tilt in vec3(), length in 0.05_f32..0.15, reach in 0.2_f32..0.8, sideways in vec3(), seed: u64,
        ) {
            let normal = (Vec3::Y + tilt * 5.0).normalize();
            // The root at the origin is `length * reach` in front of the plane.
            let offset = sideways - normal * (normal.dot(sideways) + length * reach);
            let free = swing_into(None, length, seed);
            prop_assume!(reaches(&free, |head, tail| (tail - at(head, offset)).dot(head.rotation * normal)));
            check(ColliderShape::Plane { offset, normal }, length, seed, &free)?;
        }
    }
}

/// A plane whose normal is 0.9995 long (accepted by `validate`) must collide like the unit normal the oracle uses.
#[test]
fn matches_the_spec_for_a_plane_normal_that_is_not_quite_unit() {
    let (fixture, ids) = Fixture::hair(2, 4);
    let normal = Vec3::new(0.1, 1.0, 0.05).normalize() * 0.9995;
    let group = ColliderGroup {
        name: "g".into(),
        colliders: vec![omuzaki_anim_api::Collider {
            bone: fixture.head,
            shape: ColliderShape::Plane { offset: Vec3::new(0.0, -0.2, 0.0), normal },
        }],
    };
    let mut chains = chains_of(&ids, SpringParams { stiffness: 4.0, drag_force: 0.4, ..windy() });
    for chain in &mut chains {
        chain.collider_groups = vec!["g".into()];
    }
    let set = set_of(chains, vec![group]);
    set.validate(&fixture.skeleton).unwrap();
    let agreement = compare_with_spec(&fixture, &set, 60.0, 300);
    assert!(agreement.position < 1e-5 && agreement.rotation < 5e-5, "{agreement:?}");
    assert!(agreement.swing > 0.1, "{agreement:?}");
}

/// The spec collides and then restores the bone length, so a plane closer than a bone length is not a wall: the
/// root 0.1 m in front of the plane and a 0.2 m bone hanging straight down ends the step 0.1 m behind it, as in the
/// oracle (and three-vrm). `mod.rs` records this; the guarantee is on the push.
#[test]
fn a_bone_longer_than_its_root_clearance_ends_the_step_behind_the_plane_as_the_spec_does() {
    let (length, clearance) = (0.2, 0.1);
    let mut ids = (BoneId(0), BoneId(0));
    let fixture = Fixture::with(|head, first| {
        ids = (BoneId(first), BoneId(first + 1));
        vec![
            bone("root".into(), head, Vec3::ZERO, Quat::IDENTITY),
            bone("tip".into(), ids.0, Vec3::NEG_Y * length, Quat::IDENTITY),
        ]
    });
    // No gravity, the bone held at rest by stiffness: the tail wants to stay straight down every sub-step.
    let params = SpringParams { stiffness: 1.0, gravity_power: 0.0, drag_force: 0.0, ..SpringParams::default() };
    let mut chain = SpringChain::new("c", &[ids.0, ids.1], params);
    chain.collider_groups = vec!["g".into()];
    let plane = ColliderShape::Plane { offset: Vec3::NEG_Y * clearance, normal: Vec3::Y };
    let group = ColliderGroup {
        name: "g".into(),
        colliders: vec![omuzaki_anim_api::Collider { bone: fixture.head, shape: plane }],
    };
    let set = set_of(vec![chain], vec![group]);
    set.validate(&fixture.skeleton).unwrap();
    let pose = fixture.skeleton.rest_pose();
    let mut solver = VrmSpringSolver::new(&set, &fixture.skeleton).unwrap();
    let mut oracle = Reference::new(&fixture.skeleton, &set, &pose);
    let (mut out, mut world) = (pose.clone(), Pose::default());
    for frame in 0..30 {
        out = pose.clone();
        solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut out);
        for _ in 0..solver.stats().substeps {
            oracle.substep(&pose);
        }
    }
    fixture.skeleton.world_pose(&out, &mut world);
    let (root, tail) = (world.local[ids.0.index()].translation, world.local[ids.1.index()].translation);
    let behind = (root.y - clearance) - tail.y;
    assert!((behind - (length - clearance)).abs() < 1e-4, "the tail ends {behind} m behind the plane");
    assert!(((tail - root).length() - length).abs() < 1e-5, "the bone keeps its length");
    let want = oracle.world(ids.1.index()).w_axis.truncate();
    assert!((world.local[ids.1.index()].translation.as_dvec3() - want).length() < 1e-5, "the oracle agrees");
}

#[test]
fn a_chain_only_collides_with_the_groups_it_names() {
    let (fixture, ids) = Fixture::hair(2, 3);
    let groups = vec![ColliderGroup {
        name: "head".into(),
        colliders: vec![ball(fixture.head, Vec3::new(0.0, -0.1, 0.0), 0.2)],
    }];
    let mut chains = chains_of(&ids, windy());
    chains[1].collider_groups = vec!["head".into()];
    let mut solver = VrmSpringSolver::new(&set_of(chains, groups.clone()), &fixture.skeleton).unwrap();
    let mut lone = VrmSpringSolver::new(&set_of(chains_of(&ids[..1], windy()), vec![]), &fixture.skeleton).unwrap();
    let (mut both, mut alone) = (fixture.pose_at(0), fixture.pose_at(0));
    for frame in 0..60 {
        both = fixture.pose_at(frame);
        alone = fixture.pose_at(frame);
        solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut both);
        lone.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut alone);
    }
    let first = ids[0][0].index();
    assert_eq!(
        both.local[first].rotation, alone.local[first].rotation,
        "the chain without groups ignores the collider"
    );
    assert_ne!(both.local[ids[1][0].index()].rotation, fixture.skeleton.bones()[ids[1][0].index()].rest.rotation);
}

#[test]
fn matches_the_spec_for_nested_chains_and_skipped_bones() {
    // head > a0 > a1 > a2, with chain A = [a0, a2] (a1 is skipped), chain B = [b0, b2, b3] hanging off the skipped a1
    // (b1 skipped), chain C hanging off A's tail bone a2, and chain D hanging off s1, two plain bones below a0 on a
    // side branch that is not on A's path (so only a rebuild of s0 and s1 keeps D following A).
    let (mut names, mut next) = (Vec::new(), 0u16);
    let fixture = Fixture::with(|head, first| {
        let mut add = |name: &str, parent: BoneId, t: Vec3, r: Quat| {
            let id = BoneId(first + next);
            next += 1;
            names.push(bone(name.to_owned(), parent, t, r));
            id
        };
        let a0 = add("a0", head, Vec3::new(0.0, -0.05, 0.0), Quat::from_rotation_z(0.2));
        let a1 = add("a1", a0, Vec3::new(0.0, -0.05, 0.01), Quat::from_rotation_x(0.1));
        let a2 = add("a2", a1, Vec3::new(0.01, -0.05, 0.0), Quat::IDENTITY);
        let b0 = add("b0", a1, Vec3::new(0.03, -0.02, 0.0), Quat::from_rotation_z(-0.3));
        let b1 = add("b1", b0, Vec3::new(0.0, -0.04, 0.0), Quat::from_rotation_y(0.2));
        let b2 = add("b2", b1, Vec3::new(0.0, -0.04, 0.01), Quat::IDENTITY);
        add("b3", b2, Vec3::new(0.0, -0.05, 0.0), Quat::IDENTITY);
        let c0 = add("c0", a2, Vec3::new(0.0, -0.04, 0.0), Quat::from_rotation_x(-0.2));
        add("c1", c0, Vec3::new(0.0, -0.05, 0.0), Quat::IDENTITY);
        let s0 = add("s0", a0, Vec3::new(-0.03, -0.01, 0.0), Quat::from_rotation_z(0.4));
        let s1 = add("s1", s0, Vec3::new(0.0, -0.02, 0.01), Quat::IDENTITY);
        let d0 = add("d0", s1, Vec3::new(0.0, -0.03, 0.0), Quat::from_rotation_x(0.2));
        add("d1", d0, Vec3::new(0.0, -0.05, 0.0), Quat::IDENTITY);
        std::mem::take(&mut names)
    });
    let id = |name: &str| fixture.skeleton.find(name).unwrap();
    let chain = |name: &str, bones: &[&str], params| {
        SpringChain::new(name, &bones.iter().map(|b| id(b)).collect::<Vec<_>>(), params)
    };
    // Listed out of order on purpose: the solver orders them itself.
    let set = set_of(
        vec![
            chain("c", &["c0", "c1"], windy()),
            chain("d", &["d0", "d1"], SpringParams { stiffness: 1.0, ..windy() }),
            chain("b", &["b0", "b2", "b3"], SpringParams { stiffness: 1.0, ..windy() }),
            chain("a", &["a0", "a2"], SpringParams { stiffness: 0.5, gravity_power: 2.0, ..windy() }),
        ],
        vec![],
    );
    assert_eq!(set.validate(&fixture.skeleton), Ok(()));
    assert_agrees(&compare_with_spec(&fixture, &set, 60.0, 300), 0.3);
}

#[test]
fn chains_can_be_listed_in_any_order() {
    let (fixture, ids) = Fixture::hair(3, 3);
    let chains = chains_of(&ids, windy());
    let mut reversed = chains.clone();
    reversed.reverse();
    let (mut a, mut b) = (
        VrmSpringSolver::new(&set_of(chains, vec![]), &fixture.skeleton).unwrap(),
        VrmSpringSolver::new(&set_of(reversed, vec![]), &fixture.skeleton).unwrap(),
    );
    for frame in 0..90 {
        let (mut pa, mut pb) = (fixture.pose_at(frame), fixture.pose_at(frame));
        a.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pa);
        b.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pb);
        assert_same_pose(&pa, &pb, &format!("frame {frame}"));
    }
}

/// A one-segment chain at rest: the simplest system with a closed-form answer.
fn single_segment(params: SpringParams) -> (Fixture, VrmSpringSolver, BoneId) {
    let (fixture, ids) = Fixture::hair(1, 2);
    let solver = VrmSpringSolver::new(&set_of(chains_of(&ids, params), vec![]), &fixture.skeleton).unwrap();
    let bone = ids[0][0];
    (fixture, solver, bone)
}

#[test]
fn a_stable_settle_has_a_closed_form() {
    // At rest, `nextTail = tail + dt * (stiffness * rest_dir + gravity_power * gravity_dir)` re-projected onto the
    // bone's sphere, so the tail stops where the bone points along that sum. This answer comes from neither the solver
    // nor the oracle, so it checks both.
    let params = SpringParams { stiffness: 1.5, gravity_power: 0.5, drag_force: 0.5, ..SpringParams::default() };
    let (fixture, ids) = Fixture::hair(1, 2);
    let set = set_of(chains_of(&ids, params), vec![]);
    let bone = ids[0][0];
    let rest = fixture.skeleton.rest_pose();
    let mut rest_world = Pose::default();
    fixture.skeleton.world_pose(&rest, &mut rest_world);
    let parent = fixture.skeleton.bones()[bone.index()].parent.unwrap().index();
    let rest_dir = rest_world.local[parent].rotation * rest.local[bone.index()].rotation * tail_axis(&fixture, bone);
    let expected = (rest_dir * params.stiffness + Vec3::NEG_Y * params.gravity_power).normalize();

    let mut pose = rest.clone();
    VrmSpringSolver::new(&set, &fixture.skeleton).unwrap().warm_up(&fixture.skeleton, &mut pose, 20.0);
    let mut world = Pose::default();
    fixture.skeleton.world_pose(&pose, &mut world);
    let got = world.local[bone.index()].rotation * tail_axis(&fixture, bone);
    assert!((got - expected).length() < 1e-4, "solver settled at {got}, expected {expected}");

    let mut oracle = Reference::new(&fixture.skeleton, &set, &rest);
    for _ in 0..2400 {
        oracle.substep(&rest);
    }
    let settled = oracle.world(bone.index()).transform_vector3(tail_axis(&fixture, bone).as_dvec3());
    assert!((settled - expected.as_dvec3()).length() < 1e-4, "oracle settled at {settled}, expected {expected}");
}

/// Direction from `bone` to its (only) child in the bone's own rest frame.
fn tail_axis(fixture: &Fixture, bone: BoneId) -> Vec3 {
    let child = BoneId(bone.0 + 1);
    fixture.skeleton.bones()[child.index()].rest.translation.normalize()
}

#[test]
fn settling_comes_to_rest_and_stays() {
    let (fixture, ids) = Fixture::hair(3, 5);
    let mut solver = VrmSpringSolver::new(&set_of(chains_of(&ids, windy()), vec![]), &fixture.skeleton).unwrap();
    let mut pose = fixture.pose_at(0);
    solver.warm_up(&fixture.skeleton, &mut pose, 15.0);
    let settled = pose.clone();
    for frame in 0..60 {
        let mut next = fixture.pose_at(0);
        solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut next);
        for chain in &ids {
            for b in chain {
                let moved = distance(next.local[b.index()].rotation, settled.local[b.index()].rotation);
                assert!(moved < 1e-5, "{} still moving by {moved} rad", fixture.skeleton.bones()[b.index()].name);
            }
        }
    }
}

#[test]
fn warm_up_changes_the_hair_and_zero_seconds_leaves_it_at_rest() {
    let (fixture, ids) = Fixture::hair(1, 4);
    let set = set_of(chains_of(&ids, windy()), vec![]);
    let rest = fixture.pose_at(0);

    let mut solver = VrmSpringSolver::new(&set, &fixture.skeleton).unwrap();
    let mut none = rest.clone();
    solver.warm_up(&fixture.skeleton, &mut none, 0.0);
    assert_eq!(none.local[ids[0][0].index()].rotation, fixture.skeleton.bones()[ids[0][0].index()].rest.rotation);

    let mut settled = rest.clone();
    VrmSpringSolver::new(&set, &fixture.skeleton).unwrap().warm_up(&fixture.skeleton, &mut settled, 5.0);
    let sag = distance(settled.local[ids[0][0].index()].rotation, none.local[ids[0][0].index()].rotation);
    assert!(sag > 0.02, "gravity should have pulled the hair down, but it moved {sag}");

    // Not-a-number and negative durations are no time at all; huge ones are cut short instead of hanging.
    for seconds in [f32::NAN, -3.0, f32::NEG_INFINITY] {
        let mut pose = rest.clone();
        VrmSpringSolver::new(&set, &fixture.skeleton).unwrap().warm_up(&fixture.skeleton, &mut pose, seconds);
        assert_same_pose(&pose, &none, &format!("{seconds} s"));
    }
    VrmSpringSolver::new(&set, &fixture.skeleton).unwrap().warm_up(&fixture.skeleton, &mut rest.clone(), f32::INFINITY);
}

#[test]
fn the_result_does_not_depend_on_the_frame_rate() {
    let (fixture, ids) = Fixture::hair(2, 4);
    let set = set_of(chains_of(&ids, windy()), vec![]);
    let settle_for_a_second_and_a_half = |fps: f32| {
        let mut solver = VrmSpringSolver::new(&set, &fixture.skeleton).unwrap();
        let (frames, mut substeps) = ((fps * 1.5).round() as u64, 0);
        let mut out = fixture.pose_at(0);
        for frame in 0..frames {
            out = fixture.pose_at(0);
            solver.step(Tick::at_rate(frame, fps), &fixture.skeleton, &mut out);
            substeps += solver.stats().substeps;
        }
        assert_eq!(substeps, 180, "{fps} fps");
        out
    };
    let reference = settle_for_a_second_and_a_half(120.0);
    for fps in [24.0, 30.0, 50.0, 60.0, 90.0, 144.0] {
        assert_same_pose(&settle_for_a_second_and_a_half(fps), &reference, &format!("{fps} fps vs 120 fps"));
    }
}

#[test]
fn sub_steps_are_counted_with_the_remainder_carried_over() {
    let (fixture, mut solver, _) = single_segment(SpringParams::default());
    let mut pose = fixture.skeleton.rest_pose();
    let mut substeps = |dt: f32| {
        solver.step(Tick { index: 0, time: omuzaki_core::time::Seconds(0.0), dt }, &fixture.skeleton, &mut pose);
        solver.stats().substeps
    };
    let sixth = PHYSICS_STEP * 0.6; // (three fifths of a step, really)
    assert_eq!(
        [substeps(sixth), substeps(sixth), substeps(sixth), substeps(sixth)],
        [0, 1, 0, 1],
        "0.6, 1.2, 0.8 and 1.4 steps owed"
    );
    assert_eq!(substeps(0.0), 0);
    for dt in [f32::NAN, -1.0, f32::INFINITY] {
        assert_eq!(substeps(dt), 0, "{dt}");
    }
}

#[test]
fn the_sub_step_cap_drops_the_excess_and_reports_it() {
    let (fixture, mut solver, _) = single_segment(SpringParams::default());
    let mut pose = fixture.skeleton.rest_pose();
    let tick = Tick { index: 0, time: omuzaki_core::time::Seconds(0.0), dt: 0.1 };

    solver.step(tick, &fixture.skeleton, &mut pose);
    assert_eq!(solver.stats().substeps, 8, "the default cap");
    assert!((solver.stats().dropped_s - 4.0 / 120.0).abs() < 1e-6, "{:?}", solver.stats());

    solver.set_max_substeps(2);
    solver.step(tick, &fixture.skeleton, &mut pose);
    assert_eq!(solver.stats().substeps, 2);
    assert!((solver.stats().dropped_s - 10.0 / 120.0).abs() < 1e-6, "{:?}", solver.stats());

    solver.set_max_substeps(0);
    solver.step(tick, &fixture.skeleton, &mut pose);
    assert_eq!(solver.stats().substeps, 1, "the cap is at least one");

    // Dropped time is gone for good: the next short frame does not pay it back.
    solver.step(Tick { dt: 0.0, ..tick }, &fixture.skeleton, &mut pose);
    assert_eq!(solver.stats(), omuzaki_anim_api::SpringStats::default());
}

#[test]
fn a_frame_too_short_for_a_sub_step_keeps_the_hair_where_it_was() {
    let (fixture, ids) = Fixture::hair(1, 4);
    let mut solver = VrmSpringSolver::new(&set_of(chains_of(&ids, windy()), vec![]), &fixture.skeleton).unwrap();
    let mut pose = fixture.pose_at(40);
    for frame in 0..40 {
        pose = fixture.pose_at(40);
        solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pose);
    }
    let before = pose.clone();
    let mut paused = fixture.pose_at(40);
    solver.step(Tick { dt: 0.0, ..Tick::at_rate(40, 60.0) }, &fixture.skeleton, &mut paused);
    for b in &ids[0][..3] {
        let moved = distance(before.local[b.index()].rotation, paused.local[b.index()].rotation);
        assert!(moved < 1e-6, "{moved}");
    }
    assert!(
        ids[0][..3]
            .iter()
            .any(|b| paused.local[b.index()].rotation != fixture.skeleton.bones()[b.index()].rest.rotation)
    );
}

#[test]
fn only_the_swinging_bones_rotations_are_written() {
    let (fixture, ids) = Fixture::hair(2, 4);
    let mut solver = VrmSpringSolver::new(&set_of(chains_of(&ids, windy()), vec![]), &fixture.skeleton).unwrap();
    let input = fixture.pose_at(30);
    let mut out = input.clone();
    for frame in 0..30 {
        out = input.clone();
        solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut out);
    }
    let swinging: Vec<usize> = ids.iter().flat_map(|c| &c[..3]).map(|b| b.index()).collect();
    for i in 0..fixture.skeleton.len() {
        if swinging.contains(&i) {
            assert_eq!(
                out.local[i].translation, input.local[i].translation,
                "bone {i}: translation is the animation's"
            );
            assert_ne!(out.local[i].rotation, input.local[i].rotation, "bone {i} should swing");
        } else {
            assert_eq!(out.local[i], input.local[i], "bone {i} must be untouched");
        }
    }
}

#[test]
fn reset_puts_the_hair_back_at_rest_without_a_whip() {
    let (fixture, ids) = Fixture::hair(1, 4);
    let params = SpringParams { gravity_power: 0.0, ..windy() };
    let mut solver = VrmSpringSolver::new(&set_of(chains_of(&ids, params), vec![]), &fixture.skeleton).unwrap();
    for frame in 0..120 {
        let mut pose = fixture.pose_at(frame);
        solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pose);
    }
    solver.reset();
    assert_eq!(solver.stats(), omuzaki_anim_api::SpringStats::default());

    // Teleported far away, standing still: with no gravity and no motion the hair stays exactly at rest.
    let mut pose = fixture.pose_at(0);
    pose.local[fixture.hips.index()].translation = Vec3::new(5.0, 0.0, -3.0);
    for frame in 0..30 {
        let mut out = pose.clone();
        solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut out);
        for b in &ids[0][..3] {
            let rest = fixture.skeleton.bones()[b.index()].rest.rotation;
            assert!(distance(out.local[b.index()].rotation, rest) < 1e-5, "frame {frame}");
        }
    }
}

#[test]
fn the_first_step_does_not_whip_hair_for_a_character_that_starts_away_from_the_origin() {
    let (fixture, ids) = Fixture::hair(1, 4);
    let params = SpringParams { gravity_power: 0.0, ..windy() };
    let mut solver = VrmSpringSolver::new(&set_of(chains_of(&ids, params), vec![]), &fixture.skeleton).unwrap();
    let mut pose = fixture.pose_at(0);
    pose.local[fixture.hips.index()].translation += Vec3::new(3.0, 0.0, 4.0);
    solver.step(Tick::at_rate(0, 60.0), &fixture.skeleton, &mut pose);
    for b in &ids[0][..3] {
        let rest = fixture.skeleton.bones()[b.index()].rest.rotation;
        assert!(distance(pose.local[b.index()].rotation, rest) < 1e-5);
    }
}

#[test]
fn a_bad_frame_does_not_poison_the_hair() {
    let (fixture, ids) = Fixture::hair(1, 4);
    let mut solver = VrmSpringSolver::new(&set_of(chains_of(&ids, windy()), vec![]), &fixture.skeleton).unwrap();
    for frame in 0..10 {
        let mut pose = fixture.pose_at(frame);
        solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pose);
    }
    let mut bad = fixture.pose_at(10);
    bad.local[fixture.hips.index()].translation = Vec3::NAN;
    solver.step(Tick::at_rate(10, 60.0), &fixture.skeleton, &mut bad);
    for frame in 11..14 {
        let mut pose = fixture.pose_at(frame);
        solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pose);
    }
    let mut pose = fixture.pose_at(14);
    solver.step(Tick::at_rate(14, 60.0), &fixture.skeleton, &mut pose);
    for b in &ids[0] {
        let q = pose.local[b.index()].rotation;
        assert!(q.is_finite() && q.is_normalized(), "bone {} is {q}", b.0);
    }
}

#[test]
fn random_parameters_stay_finite_and_unit_length() {
    let (fixture, ids) = Fixture::hair(4, 6);
    let mut rng = Rng::new(249);
    let chains: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(i, bones)| {
            let mut params = || SpringParams {
                hit_radius: rng.range_f32(0.0, 0.05),
                stiffness: rng.range_f32(0.0, 6.0),
                gravity_power: rng.range_f32(0.0, 3.0),
                gravity_dir: Vec3::new(rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 0.0), rng.range_f32(-1.0, 1.0)),
                drag_force: rng.range_f32(0.0, 1.0),
            };
            SpringChain::tapered(format!("c{i}"), bones, params(), params())
        })
        .collect();
    let groups =
        vec![ColliderGroup { name: "g".into(), colliders: vec![ball(fixture.head, Vec3::new(0.0, -0.1, 0.0), 0.1)] }];
    let mut chains = chains;
    for chain in &mut chains {
        chain.collider_groups = vec!["g".into()];
    }
    let mut solver = VrmSpringSolver::new(&set_of(chains, groups), &fixture.skeleton).unwrap();
    for frame in 0..600 {
        let mut pose = fixture.pose_at(frame);
        solver.step(Tick::at_rate(frame, 30.0 + (frame % 5) as f32 * 20.0), &fixture.skeleton, &mut pose);
        for b in ids.iter().flat_map(|c| &c[..5]) {
            let q = pose.local[b.index()].rotation;
            assert!(q.is_finite() && q.is_normalized(), "frame {frame}, bone {}: {q}", b.0);
        }
    }
}

#[test]
fn a_pair_with_no_length_keeps_its_rest_rotation_and_does_not_break_the_chain() {
    // hair_1 sits exactly on hair_0, so the first pair has nothing to point along.
    let fixture = Fixture::with(|head, first| {
        let h0 = BoneId(first);
        vec![
            bone("hair_0".into(), head, Vec3::new(0.0, -0.05, 0.0), Quat::from_rotation_z(0.3)),
            bone("hair_1".into(), h0, Vec3::ZERO, Quat::IDENTITY),
            bone("hair_2".into(), BoneId(first + 1), Vec3::new(0.0, -0.05, 0.0), Quat::IDENTITY),
        ]
    });
    let ids: Vec<_> = ["hair_0", "hair_1", "hair_2"].iter().map(|n| fixture.skeleton.find(n).unwrap()).collect();
    let set = set_of(vec![SpringChain::new("c", &ids, windy())], vec![]);
    let mut solver = VrmSpringSolver::new(&set, &fixture.skeleton).unwrap();
    for frame in 0..60 {
        let mut pose = fixture.pose_at(frame);
        solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pose);
        assert_eq!(pose.local[ids[0].index()].rotation, Quat::from_rotation_z(0.3));
        assert!(pose.local[ids[1].index()].rotation.is_finite());
    }
    // The pair that has a length still swings.
    assert_agrees(
        &compare_with_spec(&fixture, &set_of(vec![SpringChain::new("c", &ids[1..], windy())], vec![]), 60.0, 120),
        0.1,
    );
}

#[test]
fn identical_runs_are_bit_identical_and_a_clone_continues_the_same_way() {
    let (fixture, ids) = Fixture::hair(3, 4);
    let set = set_of(chains_of(&ids, windy()), vec![]);
    let (mut a, mut b) = (
        VrmSpringSolver::new(&set, &fixture.skeleton).unwrap(),
        VrmSpringSolver::new(&set, &fixture.skeleton).unwrap(),
    );
    let mut snapshot = None;
    for frame in 0..120 {
        let (mut pa, mut pb) = (fixture.pose_at(frame), fixture.pose_at(frame));
        a.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pa);
        b.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pb);
        assert_same_pose(&pa, &pb, &format!("frame {frame}"));
        if frame == 60 {
            snapshot = Some(a.clone());
        }
    }
    let mut resumed = snapshot.unwrap();
    for frame in 61..120 {
        let mut p = fixture.pose_at(frame);
        resumed.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut p);
    }
    let (mut last_a, mut last_resumed) = (fixture.pose_at(120), fixture.pose_at(120));
    a.step(Tick::at_rate(120, 60.0), &fixture.skeleton, &mut last_a);
    resumed.step(Tick::at_rate(120, 60.0), &fixture.skeleton, &mut last_resumed);
    assert_same_pose(&last_a, &last_resumed, "a clone");
}

#[test]
fn an_invalid_set_is_refused_with_the_reason() {
    let (fixture, ids) = Fixture::hair(1, 3);
    let short = SpringChain::new("short", &ids[0][..1], windy());
    assert!(matches!(
        VrmSpringSolver::new(&set_of(vec![short], vec![]), &fixture.skeleton),
        Err(SpringError::TooShort { count: 1, .. })
    ));
    let mut missing = SpringChain::new("missing", &ids[0], windy());
    missing.collider_groups.push("nope".into());
    assert!(matches!(
        VrmSpringSolver::new(&set_of(vec![missing], vec![]), &fixture.skeleton),
        Err(SpringError::NoSuchGroup { .. })
    ));
}

#[test]
fn an_empty_set_steps_without_touching_the_pose() {
    let (fixture, _) = Fixture::hair(1, 2);
    let mut solver = VrmSpringSolver::new(&SpringSet::default(), &fixture.skeleton).unwrap();
    let input = fixture.pose_at(3);
    let mut out = input.clone();
    solver.step(Tick::at_rate(0, 60.0), &fixture.skeleton, &mut out);
    solver.warm_up(&fixture.skeleton, &mut out, 1.0);
    assert_same_pose(&out, &input, "an empty set");
}

#[test]
fn the_last_joint_is_only_a_tail_so_its_parameters_are_unused() {
    let (fixture, ids) = Fixture::hair(1, 3);
    let mut loud = SpringChain::new("c", &ids[0], windy());
    loud.joints[2].params = SpringParams { stiffness: 99.0, gravity_power: 99.0, drag_force: 0.0, ..windy() };
    let plain = SpringChain::new("c", &ids[0], windy());
    let run = |chain: SpringChain| {
        let mut solver = VrmSpringSolver::new(&set_of(vec![chain], vec![]), &fixture.skeleton).unwrap();
        let mut out = fixture.pose_at(0);
        for frame in 0..60 {
            out = fixture.pose_at(frame);
            solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut out);
        }
        out
    };
    assert_same_pose(&run(loud), &run(plain), "the last joint's parameters");
}

/// One frame at [`PHYSICS_HZ`], which is exactly one sub-step, so each frame is one step of the spring.
fn run_frame(solver: &mut VrmSpringSolver, fixture: &Fixture, frame: u64) -> Pose {
    let mut out = fixture.pose_at(frame);
    solver.step(Tick::at_rate(frame, PHYSICS_HZ), &fixture.skeleton, &mut out);
    out
}

/// The largest change in any of `bones`' solved rotations between two frames.
fn largest_turn(bones: &[BoneId], before: &Pose, after: &Pose) -> f32 {
    bones
        .iter()
        .map(|b| distance(before.local[b.index()].rotation, after.local[b.index()].rotation))
        .fold(0.0, f32::max)
}

#[test]
fn changing_parameters_mid_swing_does_not_pop_the_hair() {
    const SWITCH: u64 = 300;
    // A nudge a user makes with a slider: stiffness up a quarter, drag up a little.
    let nudged = SpringParams { stiffness: 2.5, drag_force: 0.35, ..windy() };
    let (fixture, ids) = Fixture::hair(1, 3);
    let swinging = [ids[0][0], ids[0][1]];
    let mut solver = VrmSpringSolver::new(&set_of(chains_of(&ids, windy()), vec![]), &fixture.skeleton).unwrap();

    // Swing for a while; the largest per-frame turn after the hair has left rest is the normal step to beat.
    let mut before = run_frame(&mut solver, &fixture, 0);
    let mut normal = 0.0_f32;
    for frame in 1..SWITCH {
        let after = run_frame(&mut solver, &fixture, frame);
        if frame > 60 {
            normal = normal.max(largest_turn(&swinging, &before, &after));
        }
        before = after;
    }
    assert!(normal > 0.0, "the hair never moved, so the test proves nothing");

    for joint in 0..2 {
        solver.set_params("hair_0", joint, nudged).unwrap();
    }
    let after = run_frame(&mut solver, &fixture, SWITCH);
    let switch = largest_turn(&swinging, &before, &after);
    assert!(switch <= normal, "the first step after the switch turned {switch}, a normal step turns at most {normal}");

    // What a rebuild would do instead: every tail snaps back to rest, several times a normal step.
    let mut rebuilt = VrmSpringSolver::new(&set_of(chains_of(&ids, nudged), vec![]), &fixture.skeleton).unwrap();
    let popped = run_frame(&mut rebuilt, &fixture, SWITCH);
    let pop = largest_turn(&swinging, &before, &popped);
    assert!(
        pop > 2.0 * normal,
        "a rebuild turned {pop} in one step, not more than a normal step's {normal}: the test is too weak"
    );
}

#[test]
fn after_a_switch_the_solver_matches_one_built_with_the_new_parameters_from_the_same_state() {
    let (fixture, ids) = Fixture::hair(2, 3);
    let nudged = SpringParams { stiffness: 2.5, gravity_power: 1.5, drag_force: 0.35, hit_radius: 0.02, ..windy() };
    let chain = |i: usize, params| SpringChain::new(format!("hair_{i}"), &ids[i], params);
    let before = set_of(vec![chain(0, windy()), chain(1, windy())], vec![]);
    let after = set_of(vec![chain(0, windy()), chain(1, nudged)], vec![]);

    let mut switched = VrmSpringSolver::new(&before, &fixture.skeleton).unwrap();
    for frame in 0..120 {
        run_frame(&mut switched, &fixture, frame);
    }
    let mut built = VrmSpringSolver::new(&after, &fixture.skeleton).unwrap();
    built.copy_state_from(&switched);

    // Only the second chain changes, and it is found by name.
    for joint in 0..2 {
        switched.set_params("hair_1", joint, nudged).unwrap();
    }
    for frame in 120..240 {
        let a = run_frame(&mut switched, &fixture, frame);
        let b = run_frame(&mut built, &fixture, frame);
        assert_same_pose(&a, &b, &format!("frame {frame}"));
    }
}

#[test]
fn set_params_refuses_an_unknown_chain_and_the_tail_joint() {
    let (fixture, ids) = Fixture::hair(1, 3);
    let mut solver = VrmSpringSolver::new(&set_of(chains_of(&ids, windy()), vec![]), &fixture.skeleton).unwrap();
    let unknown = solver.set_params("nope", 0, windy()).unwrap_err();
    assert!(matches!(unknown, SpringError::BadParams { .. }) && unknown.to_string().contains("nope"), "{unknown}");
    let tail = solver.set_params("hair_0", 2, windy()).unwrap_err();
    assert!(tail.to_string().contains("tail"), "{tail}");
    assert!(solver.set_params("hair_0", 1, windy()).is_ok(), "the second joint swings, so it takes parameters");
}

/// The scene `testdata/three_vrm_golden.mjs` feeds to three-vrm, rebuilt here number for number.
mod three_vrm {
    use super::*;

    const GOLDEN: &str = include_str!("testdata/three_vrm.txt");

    /// The recorded trajectory of one scenario: per frame, the rotations of the three swinging bones.
    fn recorded(scenario: &str) -> Vec<[Quat; 3]> {
        let mut lines = GOLDEN.lines().skip_while(|l| !l.starts_with(&format!("scenario {scenario} ")));
        assert!(lines.next().is_some(), "no scenario {scenario} in the golden file");
        lines
            .take_while(|l| !l.starts_with("scenario"))
            .map(|line| {
                let n: Vec<f32> = line.split_whitespace().map(|x| x.parse().unwrap()).collect();
                assert_eq!(n.len(), 12, "{line}");
                [0, 4, 8].map(|i| Quat::from_xyzw(n[i], n[i + 1], n[i + 2], n[i + 3]))
            })
            .collect()
    }

    fn scene() -> (Fixture, [BoneId; 4]) {
        let mut ids = [BoneId(0); 4];
        let fixture = Fixture::with(|head, first| {
            let spec = [
                (Vec3::new(0.02, -0.06, 0.01), Quat::from_rotation_z(0.15)),
                (Vec3::new(-0.01, -0.065, 0.0), Quat::from_rotation_x(0.1)),
                (Vec3::new(0.0, -0.07, 0.015), Quat::from_rotation_z(-0.1)),
                (Vec3::new(0.0, -0.05, 0.0), Quat::IDENTITY),
            ];
            let mut parent = head;
            let mut bones = Vec::new();
            for (i, (translation, rotation)) in spec.into_iter().enumerate() {
                ids[i] = BoneId(first + i as u16);
                bones.push(bone(format!("h{i}"), parent, translation, rotation));
                parent = ids[i];
            }
            bones
        });
        (fixture, ids)
    }

    fn pose_at(fixture: &Fixture, frame: u64) -> Pose {
        let t = frame as f32;
        let mut pose = fixture.skeleton.rest_pose();
        let hips = &mut pose.local[fixture.hips.index()];
        hips.translation = Vec3::new(0.35 * (0.09 * t).sin(), 1.0 + 0.04 * (0.13 * t).sin(), 0.15 * (0.07 * t).cos());
        hips.rotation = Quat::from_rotation_y(0.4 * (0.06 * t).sin());
        pose.local[fixture.head.index()].rotation =
            Quat::from_rotation_x(0.3 * (0.11 * t).sin()) * Quat::from_rotation_z(0.35 * (0.15 * t).sin());
        pose
    }

    /// A collider set of `three_vrm_golden.mjs` (its `COLLIDER_SETS`): the one group, named `"body"`, a chain uses.
    type ColliderSet = fn(&Fixture) -> Vec<ColliderGroup>;

    fn chain(ids: [BoneId; 4], centre: Option<BoneId>, colliders: bool) -> SpringChain {
        let params = SpringParams {
            hit_radius: 0.02,
            stiffness: 1.5,
            gravity_power: 1.0,
            gravity_dir: Vec3::NEG_Y,
            drag_force: 0.4,
        };
        let mut chain = SpringChain::new("h", &ids, params);
        chain.center = centre;
        if colliders {
            chain.collider_groups = vec!["body".into()];
        }
        chain
    }

    /// One collider in the group the chain names.
    fn body(collider: omuzaki_anim_api::Collider) -> Vec<ColliderGroup> {
        vec![ColliderGroup { name: "body".into(), colliders: vec![collider] }]
    }

    /// `'sphere_inside'`: an inside sphere on the head, 0.1 below the root `h0`.
    fn sphere_inside(fixture: &Fixture) -> Vec<ColliderGroup> {
        let shape = ColliderShape::SphereInside { offset: Vec3::new(0.02, -0.16, 0.01), radius: 0.12 };
        body(omuzaki_anim_api::Collider { bone: fixture.head, shape })
    }

    /// `'capsule_inside'`: an inside capsule on the head, from the root `h0` straight down.
    fn capsule_inside(fixture: &Fixture) -> Vec<ColliderGroup> {
        let shape = ColliderShape::CapsuleInside {
            offset: Vec3::new(0.02, -0.06, 0.01),
            radius: 0.05,
            tail: Vec3::new(0.02, -0.26, 0.01),
        };
        body(omuzaki_anim_api::Collider { bone: fixture.head, shape })
    }

    /// `'plane'`: a plane on the spine, just behind it, facing forward.
    fn plane(fixture: &Fixture) -> Vec<ColliderGroup> {
        let spine = fixture.skeleton.humanoid_bone(HumanoidBone::Spine).unwrap();
        let shape = ColliderShape::Plane { offset: Vec3::new(0.0, 0.0, -0.02), normal: Vec3::Z };
        body(omuzaki_anim_api::Collider { bone: spine, shape })
    }

    /// `'body'`: a sphere on the head and a capsule on the spine.
    fn colliders(fixture: &Fixture) -> Vec<ColliderGroup> {
        let spine = fixture.skeleton.humanoid_bone(HumanoidBone::Spine).unwrap();
        vec![ColliderGroup {
            name: "body".into(),
            colliders: vec![
                ball(fixture.head, Vec3::new(0.0, -0.12, 0.0), 0.1),
                omuzaki_anim_api::Collider {
                    bone: spine,
                    shape: ColliderShape::Capsule {
                        offset: Vec3::ZERO,
                        radius: 0.06,
                        tail: Vec3::new(0.0, 0.15, 0.03),
                    },
                },
            ],
        }]
    }

    /// The largest rotation-component difference between the solver and three-vrm over the scenario.
    fn worst_difference(scenario: &str, centre: bool, collider_set: Option<ColliderSet>) -> f32 {
        let (fixture, ids) = scene();
        let set = set_of(
            vec![chain(ids, centre.then_some(fixture.hips), collider_set.is_some())],
            collider_set.map_or_else(Vec::new, |groups| groups(&fixture)),
        );
        let mut solver = VrmSpringSolver::new(&set, &fixture.skeleton).unwrap();
        let expected = recorded(scenario);
        assert_eq!(expected.len(), 100);
        let mut worst = 0.0_f32;
        for (frame, want) in expected.iter().enumerate() {
            let mut pose = pose_at(&fixture, frame as u64);
            solver.step(Tick::at_rate(frame as u64, 60.0), &fixture.skeleton, &mut pose);
            for (bone, want) in ids.iter().zip(want) {
                worst = worst.max(distance(pose.local[bone.index()].rotation, *want));
            }
        }
        println!("{scenario}: worst difference from three-vrm {worst:.1e}");
        worst
    }

    #[test]
    fn matches_three_vrm_on_a_free_chain() {
        let worst = worst_difference("free", false, None);
        assert!(worst < TOLERANCE as f32, "differs from three-vrm by {worst}");
    }

    #[test]
    fn matches_three_vrm_in_centre_space() {
        let worst = worst_difference("centre", true, None);
        assert!(worst < TOLERANCE as f32, "differs from three-vrm by {worst}");
    }

    #[test]
    fn matches_three_vrm_with_a_sphere_and_a_capsule() {
        let worst = worst_difference("colliders", false, Some(colliders));
        assert!(worst < TOLERANCE as f32, "differs from three-vrm by {worst}");
    }

    #[test]
    fn matches_three_vrm_with_an_inside_sphere() {
        let worst = worst_difference("sphere_inside", false, Some(sphere_inside));
        assert!(worst < TOLERANCE as f32, "differs from three-vrm by {worst}");
    }

    #[test]
    fn matches_three_vrm_with_an_inside_capsule() {
        let worst = worst_difference("capsule_inside", false, Some(capsule_inside));
        assert!(worst < TOLERANCE as f32, "differs from three-vrm by {worst}");
    }

    #[test]
    fn matches_three_vrm_with_a_plane() {
        let worst = worst_difference("plane", false, Some(plane));
        assert!(worst < TOLERANCE as f32, "differs from three-vrm by {worst}");
    }

    #[test]
    fn the_recorded_scenarios_differ_so_each_comparison_means_something() {
        let free = recorded("free");
        let apart = |a: &[[Quat; 3]], b: &[[Quat; 3]]| {
            a.iter().zip(b).flat_map(|(x, y)| x.iter().zip(y)).map(|(p, q)| distance(*p, *q)).fold(0.0, f32::max)
        };
        for scenario in ["centre", "colliders", "sphere_inside", "capsule_inside", "plane"] {
            let difference = apart(&free, &recorded(scenario));
            println!("{scenario}: differs from free by {difference}");
            assert!(difference > 0.02, "{scenario} changes nothing: {difference}");
        }
    }
}

/// The pose after stepping `set` through `frames` frames of the fixture's animation, every frame run in order.
fn pose_after(fixture: &Fixture, set: &SpringSet, frames: u64) -> Pose {
    let mut solver = VrmSpringSolver::new(set, &fixture.skeleton).unwrap();
    let mut last = fixture.pose_at(0);
    for frame in 0..frames {
        last = run_frame(&mut solver, fixture, frame);
    }
    last
}

/// `VRMC_springBone_limit`: the pure limit maths, the step order, and agreement with the spec's pseudocode.
mod limits {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_3, FRAC_PI_6, PI};

    use omuzaki_anim_api::spring::JointLimit;
    use proptest::prelude::*;

    use super::super::limit::apply;
    use super::*;

    const ID: Quat = Quat::IDENTITY;

    fn place_point(frame: &Transform, p: Vec3) -> Vec3 {
        frame.translation + frame.rotation * p
    }

    /// The angle between two vectors as `atan2(|cross|, dot)`: `acos` of a dot product is blind below about 5e-4 rad
    /// in `f32`, which is exactly where a zero-angle limit leaks.
    fn tilt(a: Vec3, b: Vec3) -> f32 {
        a.cross(b).length().atan2(a.dot(b))
    }

    /// The hinge's in-plane angle and the spherical limit's pitch and yaw of a direction in the limit's space.
    fn plane_angle(d: Vec3) -> f32 {
        d.z.abs().atan2(d.y)
    }
    fn pitch_yaw(d: Vec3) -> (f32, f32) {
        (d.z.atan2(d.y).abs(), d.x.abs().atan2(d.y.hypot(d.z)))
    }

    #[test]
    fn a_cone_clamps_a_tilt_of_sixty_degrees_to_thirty() {
        let dir = Vec3::new(FRAC_PI_3.sin(), FRAC_PI_3.cos(), 0.0);
        let out = apply(&JointLimit::cone(FRAC_PI_6), ID, dir) * 0.1;
        assert!((tilt(out, Vec3::Y) - FRAC_PI_6).abs() < 1e-5, "tilted {} rad", tilt(out, Vec3::Y));
        assert!((out.length() - 0.1).abs() < 1e-6, "length {}", out.length());
        let inside = Vec3::new(0.2, 0.9, -0.1).normalize();
        assert_eq!(apply(&JointLimit::cone(FRAC_PI_3), ID, inside), inside, "inside is returned bit for bit");
    }

    #[test]
    fn a_hinge_stays_in_its_plane_and_a_spherical_limit_bounds_both_angles() {
        let hinge = apply(&JointLimit::hinge(0.5), ID, Vec3::new(0.1, 0.3, 0.9).normalize());
        assert!(hinge.x.abs() < 1e-6 && tilt(hinge, Vec3::Y) <= 0.5 + 1e-5, "{hinge}");
        let sphere = apply(
            &JointLimit::spherical(20_f32.to_radians(), 10_f32.to_radians()),
            ID,
            Vec3::new(1.0, 1.0, 1.0).normalize(),
        );
        assert!(sphere.z.atan2(sphere.y).abs() <= 20_f32.to_radians() + 1e-5, "pitch of {sphere}");
        assert!(sphere.x.asin().abs() <= 10_f32.to_radians() + 1e-5, "yaw of {sphere}");
    }

    /// The five rows of the spec's table of singular directions, one assertion each, exact side.
    #[test]
    fn the_spec_table_of_singular_directions() {
        let down = Vec3::NEG_Y;
        let cone = apply(&JointLimit::cone(FRAC_PI_6), ID, down);
        assert_eq!((cone.x, cone.z > 0.0), (0.0, true), "cone, tail at -Y: boundary on the +Z side, {cone}");
        assert!((tilt(cone, Vec3::Y) - FRAC_PI_6).abs() < 1e-5);
        for x in [Vec3::X, Vec3::NEG_X] {
            assert_eq!(apply(&JointLimit::hinge(0.5), ID, x), Vec3::Y, "hinge, tail at {x}: +Y");
        }
        let hinge = apply(&JointLimit::hinge(0.5), ID, down);
        assert!(hinge.x == 0.0 && hinge.z > 0.0 && (tilt(hinge, Vec3::Y) - 0.5).abs() < 1e-5, "hinge at -Y: {hinge}");
        let limit = JointLimit::spherical(0.5, 0.2);
        for (x, side) in [(Vec3::X, 1.0), (Vec3::NEG_X, -1.0)] {
            let out = apply(&limit, ID, x);
            assert!(out.y > 0.0 && out.z == 0.0 && out.x * side > 0.0, "spherical, tail at {x}: +Y boundary, {out}");
            assert!((out.x.asin().abs() - 0.2).abs() < 1e-5);
        }
        let out = apply(&limit, ID, down);
        assert!(out.z > 0.0 && out.x.abs() < 1e-6 && (out.z.atan2(out.y) - 0.5).abs() < 1e-5, "spherical at -Y: {out}");
    }

    #[test]
    fn a_rotation_turns_the_limit_in_its_own_frame() {
        let frame = Quat::from_rotation_z(FRAC_PI_2); // +Y of the limit is -X in the world
        let out = apply(&JointLimit::cone(0.1), frame, Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(out, Vec3::new(-1.0, 0.0, 0.0), "the world -X is the cone's axis");
        let out = apply(&JointLimit::cone(0.1), frame, Vec3::Y);
        assert!((tilt(out, Vec3::NEG_X) - 0.1).abs() < 1e-5, "{out}");
    }

    proptest! {
        #[test]
        fn the_result_is_inside_unit_length_and_idempotent(
            d in prop::array::uniform3(-1.0_f32..1.0),
            a in 0.0_f32..4.0,
            b in 0.0_f32..2.0,
            axis in prop::array::uniform3(-1.0_f32..1.0),
            turn in -3.0_f32..3.0,
            kind in 0..3_u8,
        ) {
            let dir = Vec3::from_array(d);
            prop_assume!(dir.length() > 0.1 && Vec3::from_array(axis).length() > 0.1);
            let dir = dir.normalize();
            let frame = Quat::from_axis_angle(Vec3::from_array(axis).normalize(), turn);
            let limit = match kind {
                0 => JointLimit::cone(a),
                1 => JointLimit::hinge(a),
                _ => JointLimit::spherical(a, b),
            };
            let once = apply(&limit, frame, dir);
            prop_assert!((once.length() - 1.0).abs() < 1e-5, "length {}", once.length());
            let local = frame.conjugate() * once;
            let (a, b) = (a.min(PI), b.min(FRAC_PI_2));
            match limit {
                JointLimit::Cone { .. } => prop_assert!(tilt(local, Vec3::Y) <= a + 1e-5),
                JointLimit::Hinge { .. } => prop_assert!(local.x.abs() < 1e-5 && plane_angle(local) <= a + 1e-5),
                _ => {
                    let (pitch, yaw) = pitch_yaw(local);
                    prop_assert!(pitch <= a + 1e-5 && yaw <= b + 1e-5, "pitch {pitch} yaw {yaw}");
                }
            }
            let twice = apply(&limit, frame, once);
            prop_assert!((twice - once).length() < 1e-6, "{once} then {twice}");
        }

        /// The `f32` limit against the appendix listings in `f64`, on random directions, frames and limits.
        #[test]
        fn apply_matches_the_spec_listings_to_1e_5(
            d in prop::array::uniform3(-1.0_f32..1.0),
            a in 0.0_f32..4.0,
            b in 0.0_f32..2.0,
            axis in prop::array::uniform3(-1.0_f32..1.0),
            turn in -3.0_f32..3.0,
            kind in 0..3_u8,
        ) {
            let dir = Vec3::from_array(d);
            prop_assume!(dir.length() > 0.1 && Vec3::from_array(axis).length() > 0.1);
            let dir = dir.normalize();
            let frame = Quat::from_axis_angle(Vec3::from_array(axis).normalize(), turn);
            let limit = match kind {
                0 => JointLimit::cone(a),
                1 => JointLimit::hinge(a),
                _ => JointLimit::spherical(a, b),
            };
            let got = apply(&limit, frame, dir);
            let local = super::super::reference::limit_direction(&limit, (frame.conjugate().as_dquat() * dir.as_dvec3()).normalize());
            let want = frame.as_dquat() * local;
            prop_assert!((got.as_dvec3() - want).length() < 1e-5, "{got} against {want}");
        }
    }

    /// A tilt too small for `cos` to see (1e-4 rad is `1 - 5e-9`, which is 1 in `f32`) is still outside a cone or hinge
    /// of angle 0, and still held to a small one.
    #[test]
    fn a_tiny_tilt_is_limited_at_zero_and_small_angles() {
        for tilt_by in [1e-4_f32, 2e-4, 1e-3] {
            let dir = Vec3::new(0.0, 1.0, tilt_by).normalize();
            for angle in [0.0, tilt_by * 0.5, tilt_by * 0.9] {
                for limit in [JointLimit::cone(angle), JointLimit::hinge(angle), JointLimit::spherical(angle, angle)] {
                    let out = apply(&limit, ID, dir);
                    assert!(
                        tilt(out, Vec3::Y) <= angle + 1e-6,
                        "{limit:?} left {tilt_by} rad as {}",
                        tilt(out, Vec3::Y)
                    );
                    assert!((out.length() - 1.0).abs() < 1e-6);
                }
            }
            // A limit wider than the tilt leaves it alone, bit for bit.
            for limit in [JointLimit::cone(tilt_by * 2.0), JointLimit::spherical(tilt_by * 2.0, 1.0)] {
                assert_eq!(apply(&limit, ID, dir), dir, "{limit:?}");
            }
        }
        // And a tail rotated into a frame, since the solver always works in one.
        let frame = Quat::from_rotation_x(0.7);
        let out = apply(&JointLimit::cone(0.0), frame, frame * Vec3::new(1e-4, 1.0, 0.0).normalize());
        assert!(tilt(out, frame * Vec3::Y) < 1e-6, "{out}");
    }

    /// One chain of one swinging bone, the plane collider (in the head's space) that the test describes in the world,
    /// and the solver's tail position after a single sub-step from rest.
    struct Run {
        head: Vec3,
        rest_dir: Vec3,
        length: f32,
        tail: Vec3,
    }

    /// Steps one bone one sub-step from rest, under gravity `g` of `power` and a world-space plane given by a point
    /// relative to the bone's head and a normal, both from `plane`.
    fn one_step(
        limit: Option<JointLimit>,
        g: impl Fn(Vec3) -> Vec3,
        plane: impl Fn(Vec3, Vec3, Vec3, f32) -> (Vec3, Vec3),
    ) -> Run {
        let (fixture, ids) = Fixture::hair(1, 2);
        let bone = ids[0][0];
        let rest = fixture.skeleton.rest_pose();
        let mut world = Pose::default();
        fixture.skeleton.world_pose(&rest, &mut world);
        let parent = fixture.skeleton.bones()[bone.index()].parent.unwrap().index();
        let rest_dir = world.local[parent].rotation * rest.local[bone.index()].rotation * tail_axis(&fixture, bone);
        let head = world.local[bone.index()].translation;
        let length = fixture.skeleton.bones()[bone.index() + 1].rest.translation.length();
        let gravity = g(rest_dir);
        let (point, normal) = plane(rest_dir, gravity.normalize(), head, length);
        let head_world = world.local[fixture.head.index()];
        let shape = ColliderShape::Plane {
            offset: head_world.rotation.conjugate() * (point - head_world.translation),
            normal: head_world.rotation.conjugate() * normal,
        };
        let params = SpringParams {
            stiffness: 0.0,
            drag_force: 0.0,
            hit_radius: 0.0,
            gravity_power: gravity.length(),
            gravity_dir: gravity.normalize(),
        };
        let mut chain = SpringChain::new("c", &ids[0], params);
        chain.joints[0].limit = limit;
        chain.collider_groups = vec!["g".into()];
        let group = ColliderGroup {
            name: "g".into(),
            colliders: vec![omuzaki_anim_api::Collider { bone: fixture.head, shape }],
        };
        let mut solver = VrmSpringSolver::new(&set_of(vec![chain], vec![group]), &fixture.skeleton).unwrap();
        let mut pose = rest.clone();
        solver.step(Tick::at_rate(0, PHYSICS_HZ), &fixture.skeleton, &mut pose);
        fixture.skeleton.world_pose(&pose, &mut world);
        let tail = place_point(&world.local[bone.index()], tail_axis(&fixture, bone) * length);
        Run { head, rest_dir, length, tail }
    }

    /// The limit comes after the inertia and gravity update and before the collider: the raw tail is behind the plane,
    /// the clamped one in front of it, so nothing is pushed and the tail is exactly the clamped one. A solver that
    /// collided first would push the raw tail sideways (the normal leans toward `w`) and clamp from there.
    #[test]
    fn the_limit_is_applied_right_after_the_inertia_and_before_the_collider() {
        let theta = 0.3;
        let run = one_step(
            Some(JointLimit::cone(theta)),
            |r| r.any_orthogonal_vector().normalize() * 1000.0,
            |r, g, head, length| (head + g * (0.6 * length), (-g + r.cross(g) * 0.5).normalize()),
        );
        let g = run.rest_dir.any_orthogonal_vector().normalize();
        let expected = run.head + (run.rest_dir * theta.cos() + g * theta.sin()) * run.length;
        assert!((run.tail - expected).length() < 1e-5, "tail {} expected {expected}", run.tail);
    }

    /// And again after a push: a plane that shoves the tail out of the cone must not leave it there.
    #[test]
    fn the_limit_is_applied_again_after_each_collision() {
        let theta = 0.3;
        // The tail rests half a bone length behind a plane whose normal is sideways: the push alone tilts it past 20 degrees.
        let plane = |r: Vec3, _g: Vec3, head: Vec3, length: f32| {
            let n = (r * 0.2 + r.cross(Vec3::Y).normalize() * 1.0).normalize();
            (head + r * length + n * (0.5 * length), n)
        };
        let run = one_step(
            Some(JointLimit::cone(theta)),
            |r| r * 0.0 + Vec3::NEG_Y * 0.0 + r.cross(Vec3::Y).normalize() * 0.01,
            plane,
        );
        assert!(tilt(run.tail - run.head, run.rest_dir) <= theta + 1e-5, "tail {} left the cone", run.tail);
        let free = one_step(None, |r| r * 0.0 + Vec3::NEG_Y * 0.0 + r.cross(Vec3::Y).normalize() * 0.01, plane);
        assert!(tilt(free.tail - free.head, free.rest_dir) > theta + 1e-3, "the plane never pushed the tail out");
    }
}

/// The solver against the oracle's transcription of the limit pseudocode, on seeded random limits.
mod limits_vs_spec {
    use omuzaki_anim_api::spring::JointLimit;

    use super::*;

    fn random_limit(rng: &mut Rng, kind: u32) -> JointLimit {
        let turn = Quat::from_euler(
            omuzaki_core::glam::EulerRot::XYZ,
            rng.range_f32(-1.0, 1.0),
            rng.range_f32(-1.0, 1.0),
            rng.range_f32(-1.0, 1.0),
        );
        let angle = rng.range_f32(0.05, 0.6);
        match kind % 3 {
            0 => JointLimit::cone(angle),
            1 => JointLimit::hinge(angle),
            _ => JointLimit::spherical(angle, rng.range_f32(0.05, 0.6)),
        }
        .rotated(turn)
    }

    #[test]
    fn matches_the_spec_on_seeded_random_limits_to_1e_5() {
        let (fixture, ids) = Fixture::hair(2, 4);
        let mut rng = Rng::new(985);
        let mut engaged = 0;
        for scene in 0..24_u32 {
            let params = SpringParams { stiffness: 4.0, drag_force: 0.4, ..windy() };
            let mut chains = chains_of(&ids, params);
            for chain in &mut chains {
                for joint in &mut chain.joints {
                    joint.limit = Some(random_limit(&mut rng, scene));
                }
            }
            let set = set_of(chains, vec![]);
            set.validate(&fixture.skeleton).unwrap();
            let agreement = compare_with_spec(&fixture, &set, 60.0, 200);
            assert!(agreement.rotation < 1e-5 && agreement.position < 1e-5, "scene {scene}: {agreement:?}");
            // The limit has to matter: the limited hair ends somewhere the free hair does not.
            let (limited, free) =
                (pose_after(&fixture, &set, 200), pose_after(&fixture, &set_of(chains_of(&ids, params), vec![]), 200));
            let moved = ids
                .iter()
                .flatten()
                .map(|b| distance(limited.local[b.index()].rotation, free.local[b.index()].rotation));
            engaged += i32::from(moved.fold(0.0, f32::max) > 0.02);
        }
        assert!(engaged >= 12, "only {engaged} of 24 scenes were held back by their limit");
    }

    #[test]
    fn matches_the_spec_with_colliders_pushing_into_the_limits() {
        let (fixture, ids) = Fixture::hair(2, 4);
        let mut rng = Rng::new(9850);
        let group = ColliderGroup {
            name: "g".into(),
            colliders: vec![omuzaki_anim_api::Collider {
                bone: fixture.head,
                shape: ColliderShape::Sphere { offset: Vec3::new(0.0, -0.1, 0.0), radius: 0.07 },
            }],
        };
        let mut chains = chains_of(&ids, SpringParams { stiffness: 4.0, drag_force: 0.4, ..windy() });
        for (i, chain) in chains.iter_mut().enumerate() {
            chain.collider_groups = vec!["g".into()];
            for joint in &mut chain.joints {
                joint.limit = Some(random_limit(&mut rng, i as u32));
            }
        }
        let agreement = compare_with_spec(&fixture, &set_of(chains, vec![group]), 60.0, 200);
        assert!(agreement.rotation < 1e-5 && agreement.position < 1e-5, "{agreement:?}");
    }

    #[test]
    fn a_bone_pointing_straight_down_uses_the_half_turn_default_orientation() {
        let (fixture, ids) = {
            let mut ids = vec![];
            let fixture = Fixture::with(|head, first| {
                ids = vec![BoneId(first), BoneId(first + 1)];
                vec![
                    bone("a".into(), head, Vec3::new(0.0, -0.05, 0.0), Quat::IDENTITY),
                    bone("b".into(), BoneId(first), Vec3::new(0.0, -0.1, 0.0), Quat::IDENTITY),
                ]
            });
            (fixture, vec![ids])
        };
        let mut chains = chains_of(&ids, SpringParams { stiffness: 0.5, drag_force: 0.3, ..windy() });
        chains[0].joints[0].limit = Some(JointLimit::cone(0.4));
        let agreement = compare_with_spec(&fixture, &set_of(chains, vec![]), 60.0, 200);
        assert!(agreement.rotation < 1e-5 && agreement.position < 1e-5, "{agreement:?}");
    }
}

/// `SpringSet::validate` and the wire form of a limit.
mod limit_data {
    use omuzaki_anim_api::spring::JointLimit;

    use super::*;

    fn set_with(limit: JointLimit, joint: usize) -> (Fixture, SpringSet) {
        let (fixture, ids) = Fixture::hair(1, 3);
        let mut chains = chains_of(&ids, windy());
        chains[0].joints[joint].limit = Some(limit);
        (fixture, set_of(chains, vec![]))
    }

    #[test]
    fn a_malformed_limit_is_bad_params() {
        let bad = [
            JointLimit::cone(-0.1),
            JointLimit::hinge(f32::NAN),
            JointLimit::spherical(0.5, f32::INFINITY),
            JointLimit::spherical(-0.5, 0.5),
            JointLimit::cone(0.5).rotated(Quat::from_xyzw(0.0, 0.0, 0.0, 1.01)),
            JointLimit::hinge(0.5).rotated(Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0)),
        ];
        for limit in bad {
            let (fixture, set) = set_with(limit, 0);
            assert!(matches!(set.validate(&fixture.skeleton), Err(SpringError::BadParams { .. })), "{limit:?}");
        }
        // Angles past the clamp, and a unit quaternion within the tolerance, are fine.
        let (fixture, set) =
            set_with(JointLimit::spherical(9.0, 9.0).rotated(Quat::from_xyzw(0.0, 0.0, 0.0, 1.0005)), 1);
        assert_eq!(set.validate(&fixture.skeleton), Ok(()));
    }

    #[test]
    fn a_limit_on_the_last_joint_is_ignored() {
        let (fixture, with) = set_with(JointLimit::cone(0.01), 2);
        let mut without = with.clone();
        without.chains[0].joints[2].limit = None;
        let run = |set: &SpringSet| {
            let mut solver = VrmSpringSolver::new(set, &fixture.skeleton).unwrap();
            let mut pose = fixture.pose_at(0);
            for frame in 0..60 {
                pose = fixture.pose_at(frame);
                solver.step(Tick::at_rate(frame, 60.0), &fixture.skeleton, &mut pose);
            }
            pose
        };
        assert_same_pose(&run(&with), &run(&without), "a limit on the tail joint");
    }

    #[test]
    fn a_limit_does_not_change_an_unlimited_run_bit_for_bit() {
        let (fixture, ids) = Fixture::hair(2, 4);
        let plain = set_of(chains_of(&ids, windy()), vec![]);
        // A cone of pi is a sphere: no direction is outside it.
        let mut wide = plain.clone();
        for joint in wide.chains.iter_mut().flat_map(|c| &mut c.joints) {
            joint.limit = Some(JointLimit::cone(PI_OR_MORE));
        }
        assert_same_pose(&pose_after(&fixture, &plain, 120), &pose_after(&fixture, &wide, 120), "a cone of pi");
    }

    const PI_OR_MORE: f32 = 3.5;
}
