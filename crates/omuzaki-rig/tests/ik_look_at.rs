//! The look-at solver against the issue's acceptance: on a sphere around the head the forward axis lands within 1 degree
//! of the target inside the limits and on the limit outside them; a target crossing behind (or over the top, or through
//! the head) never makes the output jump; the 10/30/60% shares and the eye share go through the VRM range maps for both
//! the bone and the expression type; weight 0 is the identity; and a solve allocates nothing.
//!
//! Expectations are computed from the posed skeleton's world rotations and the fixture's rest orientations, not from
//! the solver's own maths. Every sweep runs on the aligned humanoid and on the one whose bone axes (armature root
//! included) are tilted at random, since the solve must not depend on how a skeleton's local axes are laid out.
//!
//! It is its own test binary because the counting allocator is global; it counts only the measuring thread.

#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::f32::consts::PI;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{HumanoidBone as B, IkGoals, IkSolver, LookAt, Pose};
use omuzaki_anim_mock::fixtures::quadruped_rig;
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_rig::ik::look_at::{
    EyeKind, EyeRanges, EyeWeights, LookAtIk, LookAtParams, NEAR_FULL, PITCH_DOWN, PITCH_UP, RangeMap, YAW_LIMIT,
};

mod common;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// A character and the pose the look-at starts from.
struct Fixture {
    ch: common::Character,
    base: Pose,
}

impl Fixture {
    /// `animated` bends the spine and nods the neck and head, so the body frame and the head's own tilt are not rest.
    fn new(tilted: bool, animated: bool) -> Self {
        let ch = common::full_character(tilted);
        let mut base = ch.skeleton.rest_pose();
        if animated {
            for (bone, axis, angle) in [
                (B::Spine, Vec3::new(1.0, 2.0, 3.0), 0.4),
                (B::Neck, Vec3::new(-1.0, 0.5, 0.2), 0.15),
                (B::Head, Vec3::new(0.3, -1.0, 0.7), 0.2),
            ] {
                let local = &mut base.local[ch.id(bone)].rotation;
                *local = (Quat::from_axis_angle(axis.normalize(), angle) * *local).normalize();
            }
        }
        Self { ch, base }
    }

    fn world(&self, pose: &Pose) -> Pose {
        let mut out = Pose::IDENTITY;
        self.ch.skeleton.world_pose(pose, &mut out);
        out
    }

    /// The frame of `bone` aligned to the root space at rest: +Z is its forward whatever its own axes are.
    fn aligned(&self, world: &Pose, bone: B) -> Quat {
        let i = self.ch.id(bone);
        world.local[i].rotation * self.ch.rest_rot[i].inverse()
    }

    fn head_pos(&self, world: &Pose) -> Vec3 {
        world.local[self.ch.id(B::Head)].translation
    }

    fn head_forward(&self, pose: &Pose) -> Vec3 {
        self.aligned(&self.world(pose), B::Head) * Vec3::Z
    }

    /// The pose after solving `look` with default parameters.
    fn solve(&self, look: LookAt) -> Pose {
        self.solve_with(LookAtParams::default(), look).0
    }

    fn solve_with(&self, params: LookAtParams, look: LookAt) -> (Pose, EyeWeights) {
        let mut pose = self.base.clone();
        let mut ik = LookAtIk::new(params);
        ik.solve(&self.ch.skeleton, &IkGoals { look_at: Some(look), ..IkGoals::NONE }, &mut pose);
        (pose, ik.eye_weights())
    }

    /// Where `dir` (+Z ahead, +Y up, +X left, as a yaw/pitch pair gives it) points at `radius` from the rest head.
    fn target(&self, yaw: f32, pitch: f32, radius: f32) -> Vec3 {
        self.ch.rest_pos[self.ch.id(B::Head)] + radius * direction(yaw, pitch)
    }
}

fn direction(yaw: f32, pitch: f32) -> Vec3 {
    Vec3::new(pitch.cos() * yaw.sin(), pitch.sin(), pitch.cos() * yaw.cos())
}

fn look(target: Vec3) -> LookAt {
    LookAt { target, weight: 1.0, eye_share: 0.0 }
}

/// The angle between two vectors, accurate for tiny angles (`acos` of a dot product is not).
fn angle(a: Vec3, b: Vec3) -> f32 {
    a.cross(b).length().atan2(a.dot(b))
}

const DEG: f32 = PI / 180.0;

/// What the head's forward axis must be for a target direction `d` in the body frame, by the issue's rule (stay on the
/// limit). `None` inside the wedge behind the body and the cone about the vertical, where the module doc puts the
/// easing, so the rule does not apply.
fn expected_forward(d: Vec3) -> Option<Vec3> {
    let (yaw, pitch) = (d.x.atan2(d.z), d.y.asin());
    (yaw.abs() < 155.0 * DEG && d.x.hypot(d.z) > 0.2)
        .then(|| direction(yaw.clamp(-YAW_LIMIT, YAW_LIMIT), pitch.clamp(-PITCH_DOWN, PITCH_UP)))
}

#[test]
fn on_a_sphere_the_head_lands_on_the_target_inside_the_limits_and_on_the_limit_outside() {
    let (mut inside, mut outside) = (0_u32, 0_u32);
    for (tilted, animated) in [(false, false), (true, false), (false, true), (true, true)] {
        let fx = Fixture::new(tilted, animated);
        for yaw_step in -36..=36_i32 {
            for pitch_step in -17..=17_i32 {
                let (yaw, pitch) = (yaw_step as f32 * 5.0 * DEG, pitch_step as f32 * 5.0 * DEG);
                let pose = fx.solve(look(fx.target(yaw, pitch, 3.0)));
                let world = fx.world(&pose);
                let to_target = (fx.target(yaw, pitch, 3.0) - fx.head_pos(&world)).normalize();
                let body = fx.aligned(&fx.world(&fx.base), B::Spine);
                let Some(want) = expected_forward(body.inverse() * to_target) else { continue };
                let got = fx.aligned(&world, B::Head) * Vec3::Z;
                let off = angle(got, body * want).to_degrees();
                assert!(
                    off < 1.0,
                    "tilted {tilted} animated {animated} yaw {yaw_step}*5 pitch {pitch_step}*5: {off} deg off"
                );
                if angle(want, body.inverse() * to_target) < 0.01 { inside += 1 } else { outside += 1 }
            }
        }
    }
    assert!(inside > 1000 && outside > 1000, "both sides of the limits are covered ({inside}, {outside})");
}

/// The angle (radians) between the head's forward axes for two target directions.
fn forward_after(fx: &Fixture, target: Vec3) -> Vec3 {
    fx.head_forward(&fx.solve(look(target)))
}

/// Walks `path(0..=1)` in `steps` and checks the head's forward axis moves at most `max_gain` times as far as the
/// target direction does; any step that exceeds it is bisected, and the jump must shrink to nothing.
fn assert_continuous(fx: &Fixture, name: &str, steps: u32, max_gain: f32, bound: f32, path: impl Fn(f32) -> Vec3) {
    let out = |t: f32| forward_after(fx, path(t));
    let head = fx.ch.rest_pos[fx.ch.id(B::Head)];
    let spread = |a: f32, b: f32| angle(path(a) - head, path(b) - head);
    let mut worst: f32 = 0.0;
    for i in 0..steps {
        let (a, b) = (i as f32 / steps as f32, (i + 1) as f32 / steps as f32);
        let (jump, step) = (angle(out(a), out(b)), spread(a, b));
        worst = worst.max(jump / step.max(1e-9));
        if jump > max_gain * step + 1e-4 {
            let (mut lo, mut hi) = (a, b);
            for _ in 0..30 {
                let mid = f32::midpoint(lo, hi);
                if angle(out(lo), out(mid)) > angle(out(mid), out(hi)) { hi = mid } else { lo = mid }
            }
            let left = angle(out(lo), out(hi));
            assert!(
                left < 1e-3,
                "{name}: a step of {step} rad jumps {jump} rad and does not shrink on bisection ({left})"
            );
        }
    }
    assert!(worst < bound, "{name}: the gain {worst} is past {bound}");
}

#[test]
fn the_output_never_jumps_as_the_target_crosses_behind_over_the_top_or_through_the_head() {
    for (tilted, animated) in [(false, false), (true, true)] {
        let fx = Fixture::new(tilted, animated);
        // Rings at several pitches: front, side, behind, side, front.
        for pitch in [-80.0_f32, -60.0, -40.0, -10.0, 0.0, 20.0, 30.0, 45.0, 85.0] {
            // The sweep from one limit to the other takes 140 degrees of yaw over 40 degrees of ring, which is `cos(pitch)`
            // as long a path near the vertical: the gain is at most about `8 / cos(pitch)` (measured 64 at 85 degrees).
            let bound = (8.0 / (pitch * DEG).cos()).max(8.0);
            assert_continuous(&fx, &format!("ring at pitch {pitch}"), 1440, 8.0, bound, |t| {
                fx.target(t * 2.0 * PI - PI, pitch * DEG, 2.0)
            });
        }
        // Great circles through the poles, so the target passes directly above, directly behind and directly below.
        for azimuth in 0..16 {
            let a = azimuth as f32 * 22.5 * DEG;
            assert_continuous(&fx, &format!("meridian {azimuth}"), 1440, 60.0, 70.0, |t| {
                let tilt = t * 2.0 * PI;
                let (up, around) = (tilt.sin(), tilt.cos());
                fx.ch.rest_pos[fx.ch.id(B::Head)] + 2.0 * (Vec3::new(a.sin(), 0.0, a.cos()) * around + Vec3::Y * up)
            });
        }
        // Straight through the head and out the other side. The target's direction swings by 180 degrees within the
        // fade's few centimetres, so only the bisection (no jump) applies, not a gain against the swing.
        assert_continuous(&fx, "through the head", 800, 60.0, f32::INFINITY, |t| {
            fx.target(0.7, 0.3, 0.0) + (t * 2.0 - 1.0) * Vec3::new(0.0, 0.2, 0.2)
        });
        // Out from the head's origin along a ray past a limit: the direction is fixed, so there is no swing to take a gain against.
        assert_continuous(&fx, "radius", 400, 60.0, f32::INFINITY, |t| fx.target(1.4, 0.2, t * 0.3));
    }
}

#[test]
fn behind_the_head_holds_the_limit_then_sweeps_through_the_front_inside_the_wedge() {
    let fx = Fixture::new(false, false);
    // 100 degrees: past the limit, outside the wedge: on the limit, on the target's side.
    for sign in [1.0_f32, -1.0] {
        let f = forward_after(&fx, fx.target(sign * 100.0 * DEG, 0.0, 3.0));
        assert!((f.x.atan2(f.z) - sign * YAW_LIMIT).abs() < 0.2 * DEG);
    }
    // Exactly behind: the documented fallback, the head faces front.
    let f = forward_after(&fx, fx.target(PI, 0.0, 3.0));
    assert!(angle(f, Vec3::Z) < 0.5 * DEG, "facing {f:?}");
    // Never turns the other way round: along the whole arc the yaw keeps the target's sign.
    for step in 0..=170 {
        let yaw = (10.0 + step as f32) * DEG;
        let f = forward_after(&fx, fx.target(yaw, 0.0, 3.0));
        assert!(f.x >= -1e-4 && f.x.atan2(f.z) <= YAW_LIMIT + 0.2 * DEG, "yaw {step}: {f:?}");
    }
}

/// Angle (degrees) of the aligned frame of `bone` about +Y, for a pure yaw.
fn yaw_of(fx: &Fixture, pose: &Pose, bone: B) -> f32 {
    let f = fx.aligned(&fx.world(pose), bone) * Vec3::Z;
    f.x.atan2(f.z).to_degrees()
}

#[test]
fn the_head_chest_and_neck_share_the_turn_ten_thirty_and_sixty_percent() {
    let fx = Fixture::new(true, false);
    let pose = fx.solve(look(fx.target(50.0 * DEG, 0.0, 200.0)));
    for (bone, want) in [(B::Chest, 5.0), (B::Neck, 20.0), (B::Head, 50.0)] {
        let got = yaw_of(&fx, &pose, bone);
        assert!((got - want).abs() < 0.2, "{bone:?}: {got} not {want}");
    }
    // Custom shares and limits through the params.
    let params = LookAtParams { shares: [0.0, 0.0, 1.0], yaw_limit: 30.0 * DEG, ..LookAtParams::default() };
    let (pose, _) = fx.solve_with(params, look(fx.target(80.0 * DEG, 0.0, 200.0)));
    assert!(yaw_of(&fx, &pose, B::Chest).abs() < 1e-3 && yaw_of(&fx, &pose, B::Neck).abs() < 1e-3);
    assert!((yaw_of(&fx, &pose, B::Head) - 30.0).abs() < 0.2);
    // Unusable shares give the head the turn rather than nothing.
    let params = LookAtParams { shares: [0.0, f32::NAN, -1.0], ..LookAtParams::default() };
    let (pose, _) = fx.solve_with(params, look(fx.target(40.0 * DEG, 0.0, 200.0)));
    assert!((yaw_of(&fx, &pose, B::Head) - 40.0).abs() < 0.2);
}

/// Per-eye ranges that differ on every axis, so a mixed-up map shows.
fn uneven_ranges(output: f32) -> EyeRanges {
    let map = |input, scale: f32| RangeMap { input_max_value: input, output_scale: scale * output };
    EyeRanges {
        horizontal_inner: map(45.0, 10.0),
        horizontal_outer: map(90.0, 10.0),
        vertical_down: map(60.0, 12.0),
        vertical_up: map(30.0, 6.0),
    }
}

/// The eye's forward axis in the head's aligned frame.
fn eye_forward(fx: &Fixture, pose: &Pose, eye: B) -> Vec3 {
    let world = fx.world(pose);
    fx.aligned(&world, B::Head).inverse() * (fx.aligned(&world, eye) * Vec3::Z)
}

fn eyes_look(fx: &Fixture, yaw: f32, pitch: f32, eye_share: f32) -> Pose {
    let params = LookAtParams { eyes: uneven_ranges(1.0), ..LookAtParams::default() };
    fx.solve_with(params, LookAt { target: fx.target(yaw * DEG, pitch * DEG, 200.0), weight: 1.0, eye_share }).0
}

#[test]
fn eye_share_one_leaves_the_chain_and_turns_the_eyes_through_the_bone_range_maps() {
    for tilted in [false, true] {
        let fx = Fixture::new(tilted, false);
        // (target yaw, pitch) -> (left eye yaw, pitch, right eye yaw, pitch) in degrees.
        let cases = [
            (40.0, 0.0, 40.0 / 90.0 * 10.0, 0.0, 40.0 / 45.0 * 10.0, 0.0), // left outer, right inner
            (-40.0, 0.0, -40.0 / 45.0 * 10.0, 0.0, -40.0 / 90.0 * 10.0, 0.0),
            (0.0, 20.0, 0.0, 20.0 / 30.0 * 6.0, 0.0, 20.0 / 30.0 * 6.0),
            (0.0, -30.0, 0.0, -30.0 / 60.0 * 12.0, 0.0, -30.0 / 60.0 * 12.0),
            (120.0, 0.0, 10.0, 0.0, 10.0, 0.0), // saturates at the end of the map
        ];
        for (yaw, pitch, ly, lp, ry, rp) in cases {
            let pose = eyes_look(&fx, yaw, pitch, 1.0);
            for bone in [B::Chest, B::Neck, B::Head] {
                assert_eq!(pose.local[fx.ch.id(bone)], fx.base.local[fx.ch.id(bone)], "{bone:?} stays");
            }
            for (eye, y, p) in [(B::LeftEye, ly, lp), (B::RightEye, ry, rp)] {
                let want = direction(y * DEG, p * DEG);
                let got = eye_forward(&fx, &pose, eye);
                assert!(
                    angle(got, want).to_degrees() < 0.05,
                    "tilted {tilted} {eye:?} at ({yaw}, {pitch}): {got:?} not {want:?}"
                );
            }
        }
        // Directly behind: the eyes ease through the front too.
        let pose = eyes_look(&fx, 180.0, 0.0, 1.0);
        assert!(angle(eye_forward(&fx, &pose, B::LeftEye), Vec3::Z) < 0.01);
    }
}

#[test]
fn a_shared_turn_leaves_the_eyes_what_the_head_leaves() {
    let fx = Fixture::new(true, false);
    // Half the turn to the chain, the other half to the eyes: head at 20, the left eye's outer map at 20 degrees.
    let pose = eyes_look(&fx, 40.0, 0.0, 0.5);
    assert!((yaw_of(&fx, &pose, B::Head) - 20.0).abs() < 0.1);
    let want = direction(20.0 / 90.0 * 10.0 * DEG, 0.0);
    assert!(angle(eye_forward(&fx, &pose, B::LeftEye), want).to_degrees() < 0.05);
    // At zero the eyes stay put even for a target past the head's limit, and the gain ramps in without a step.
    let pose = eyes_look(&fx, 100.0, 0.0, 0.0);
    assert_eq!(pose.local[fx.ch.id(B::LeftEye)], fx.base.local[fx.ch.id(B::LeftEye)]);
    let (a, b) = (eyes_look(&fx, 100.0, 0.0, 1e-4), eyes_look(&fx, 100.0, 0.0, 0.0));
    assert!(angle(eye_forward(&fx, &a, B::LeftEye), eye_forward(&fx, &b, B::LeftEye)) < 1e-3);
}

#[test]
fn the_expression_type_leaves_the_eye_bones_and_reports_weights_through_the_range_maps() {
    let fx = Fixture::new(true, false);
    let params = LookAtParams { eye_kind: EyeKind::Expression, eyes: uneven_ranges(0.1), ..LookAtParams::default() };
    let solve = |yaw: f32, pitch: f32| {
        fx.solve_with(params, LookAt { target: fx.target(yaw * DEG, pitch * DEG, 200.0), weight: 1.0, eye_share: 1.0 })
    };
    // Only the outer horizontal map: the one expression pair has no inner and outer.
    let (pose, w) = solve(40.0, 0.0);
    assert!(
        (w.look_left - 40.0 / 90.0).abs() < 1e-4 && w.look_right == 0.0 && w.look_up == 0.0 && w.look_down < 1e-6,
        "{w:?}"
    );
    for eye in [B::LeftEye, B::RightEye] {
        assert_eq!(pose.local[fx.ch.id(eye)], fx.base.local[fx.ch.id(eye)]);
    }
    let (_, w) = solve(-40.0, 0.0);
    assert!((w.look_right - 40.0 / 90.0).abs() < 1e-4 && w.look_left == 0.0, "{w:?}");
    let (_, w) = solve(0.0, 20.0);
    assert!((w.look_up - 20.0 / 30.0 * 0.6).abs() < 1e-4, "{w:?}");
    let (_, w) = solve(0.0, -30.0);
    assert!((w.look_down - 30.0 / 60.0 * 1.2).abs() < 1e-4, "{w:?}");
    let (_, w) = solve(150.0, 0.0);
    assert!((w.look_left - 1.0).abs() < 1e-4, "saturates: {w:?}");
    // Reset and an absent goal both clear them.
    let mut ik = LookAtIk::new(params);
    let mut pose = fx.base.clone();
    let goals = IkGoals {
        look_at: Some(LookAt { target: fx.target(0.7, 0.0, 5.0), weight: 1.0, eye_share: 1.0 }),
        ..IkGoals::NONE
    };
    ik.solve(&fx.ch.skeleton, &goals, &mut pose);
    assert!(ik.eye_weights() != EyeWeights::ZERO);
    ik.solve(&fx.ch.skeleton, &IkGoals::NONE, &mut pose);
    assert_eq!(ik.eye_weights(), EyeWeights::ZERO);
    ik.solve(&fx.ch.skeleton, &goals, &mut pose);
    ik.reset();
    assert_eq!(ik.eye_weights(), EyeWeights::ZERO);
}

#[test]
fn weight_zero_is_the_identity_and_the_fade_is_continuous_and_monotone() {
    for (tilted, animated) in [(false, false), (true, true)] {
        let fx = Fixture::new(tilted, animated);
        let target = fx.target(1.0, 0.2, 2.0);
        let at =
            |weight: f32, eye_share: f32| fx.solve_with(LookAtParams::default(), LookAt { target, weight, eye_share });
        assert_eq!(at(0.0, 0.3).0, fx.base, "weight 0 changes nothing");
        assert_eq!(at(0.0, 0.3).1, EyeWeights::ZERO);
        let base_forward = fx.head_forward(&fx.base);
        let mut last = 0.0_f32;
        for i in 1..=500 {
            let turned = angle(base_forward, fx.head_forward(&at(i as f32 / 500.0, 0.0).0));
            assert!(turned >= last - 1e-4 && turned - last < 0.02, "weight {i}/500: {last} -> {turned}");
            last = turned;
        }
        let half = angle(base_forward, fx.head_forward(&at(0.5, 0.0).0));
        assert!((half / last - 0.5).abs() < 0.1, "half weight turns about half: {half} of {last}");
    }
}

#[test]
fn only_the_chain_and_eye_rotations_change_and_the_result_is_deterministic() {
    let fx = Fixture::new(true, true);
    let moves = [B::Chest, B::Neck, B::Head, B::LeftEye, B::RightEye].map(|b| fx.ch.id(b));
    let l = LookAt { target: fx.target(1.1, 0.2, 1.5), weight: 0.8, eye_share: 0.4 };
    let a = fx.solve_with(LookAtParams::default(), l).0;
    assert_eq!(a, fx.solve_with(LookAtParams::default(), l).0, "same inputs, same bits");
    for (i, (new, old)) in a.local.iter().zip(&fx.base.local).enumerate().take(fx.ch.skeleton.len()) {
        assert_eq!(new.translation, old.translation, "bone {i}: translation");
        if moves.contains(&i) {
            assert!((new.rotation.length() - 1.0).abs() < 1e-5, "bone {i} stays unit");
        } else {
            assert_eq!(new, old, "bone {i} is outside the chain");
        }
    }
    // A reused solver has no memory of the last frame.
    let mut ik = LookAtIk::default();
    let goals = IkGoals { look_at: Some(l), ..IkGoals::NONE };
    let (mut p1, mut p2) = (fx.base.clone(), fx.base.clone());
    ik.solve(&fx.ch.skeleton, &goals, &mut p1);
    ik.solve(&fx.ch.skeleton, &goals, &mut p2);
    assert_eq!(p1, p2);
}

#[test]
fn bad_input_and_non_humanoids_leave_the_pose_alone() {
    let fx = Fixture::new(false, true);
    for bad in [Vec3::NAN, Vec3::new(f32::INFINITY, 0.0, 0.0)] {
        assert_eq!(fx.solve(look(bad)), fx.base);
    }
    let nan_weight = LookAt { target: Vec3::Z, weight: f32::NAN, eye_share: 0.0 };
    assert_eq!(fx.solve(nan_weight), fx.base);
    // A target on the head's origin has no direction: the pose stays as animated.
    let head = fx.head_pos(&fx.world(&fx.base));
    assert_eq!(fx.solve(look(head)), fx.base);
    assert_eq!(fx.solve(look(head + Vec3::new(0.0, 0.0, 0.005))), fx.base);
    // Full weight from NEAR_FULL out.
    let near = fx.solve(look(head + Vec3::new(0.0, 0.0, NEAR_FULL * 1.5)));
    assert_ne!(near, fx.base);

    let creature = quadruped_rig().unwrap();
    let skeleton = &creature.skeleton;
    let mut pose = skeleton.rest_pose();
    let before = pose.clone();
    let goals = IkGoals { look_at: Some(look(Vec3::new(1.0, 1.0, 1.0))), ..IkGoals::NONE };
    LookAtIk::default().solve(skeleton, &goals, &mut pose);
    assert_eq!(pose, before, "a generic skeleton has no head to turn");
}

#[test]
fn a_head_carried_onto_the_target_by_the_chest_fades_out_without_a_jump() {
    // The review's case: with the head pitched 80 degrees the chest's own turn carries the head onto a target that
    // started 5 cm away, so the fade has to follow the head's place at each bone, not its place at the start.
    let mut fx = Fixture::new(false, false);
    let head = fx.ch.id(B::Head);
    fx.base.local[head].rotation = Quat::from_rotation_x(80.0 * DEG) * fx.base.local[head].rotation;
    let origin = fx.head_pos(&fx.world(&fx.base));
    let target = origin + Vec3::new(0.0, -0.007_765_719, -0.053_727_17);
    let nudge = Vec3::new(0.0, 1e-5, 0.0);
    let jump = angle(forward_after(&fx, target + nudge), forward_after(&fx, target - nudge)).to_degrees();
    assert!(jump < 0.5, "a 20 micrometre move jumps the head {jump} degrees");

    // And at random: heads pitched every which way, targets within 10 cm of the head.
    let mut rng = Rng::new(0x4e45_4152);
    for case in 0..1500 {
        let mut fx = Fixture::new(case % 2 == 0, false);
        let axis = Vec3::new(rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0))
            .normalize_or(Vec3::X);
        for bone in [B::Spine, B::Chest, B::Neck, B::Head] {
            let local = &mut fx.base.local[fx.ch.id(bone)].rotation;
            *local = (Quat::from_axis_angle(axis, rng.range_f32(-1.5, 1.5)) * *local).normalize();
        }
        let origin = fx.head_pos(&fx.world(&fx.base));
        let target = origin + Vec3::new(rng.range_f32(-0.1, 0.1), rng.range_f32(-0.1, 0.1), rng.range_f32(-0.1, 0.1));
        let jump = angle(forward_after(&fx, target + nudge), forward_after(&fx, target - nudge)).to_degrees();
        assert!(jump < 0.5, "case {case}: a 20 micrometre move jumps the head {jump} degrees");
    }
}

#[test]
fn expression_weights_never_pass_one_even_when_the_map_scale_does() {
    let fx = Fixture::new(false, false);
    // The test ranges scale `vertical_down` by 1.2: a -60 degree gaze saturates at 1.2 before the clamp.
    let params = LookAtParams { eye_kind: EyeKind::Expression, eyes: uneven_ranges(1.0), ..LookAtParams::default() };
    let l = LookAt { target: fx.target(0.0, -60.0 * DEG, 200.0), weight: 1.0, eye_share: 1.0 };
    let (_, w) = fx.solve_with(params, l);
    assert_eq!(w.look_down, 1.0, "{w:?}");
    for (yaw, pitch) in [(170.0, 80.0), (-170.0, -80.0), (90.0, 0.0), (0.0, -89.0)] {
        let l = LookAt { target: fx.target(yaw * DEG, pitch * DEG, 50.0), weight: 1.0, eye_share: 1.0 };
        let (_, w) = fx.solve_with(params, l);
        for v in [w.look_left, w.look_right, w.look_up, w.look_down] {
            assert!((0.0..=1.0).contains(&v), "{w:?}");
        }
    }
}

#[test]
fn a_solve_allocates_nothing() {
    let fx = Fixture::new(true, true);
    let mut rng = Rng::new(11);
    let goals: Vec<IkGoals> = (0..32)
        .map(|_| {
            let target = fx.target(rng.range_f32(-PI, PI), rng.range_f32(-1.4, 1.4), rng.range_f32(0.1, 5.0));
            IkGoals {
                look_at: Some(LookAt { target, weight: rng.next_f32(), eye_share: rng.next_f32() }),
                ..IkGoals::NONE
            }
        })
        .collect();
    let mut ik = LookAtIk::default();
    let mut pose = fx.base.clone();
    ik.solve(&fx.ch.skeleton, &goals[0], &mut pose); // warm up anything lazy
    ALLOCATIONS.store(0, Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for goal in goals.iter().cycle().take(512) {
        pose.clone_from(&fx.base);
        ik.solve(&fx.ch.skeleton, goal, &mut pose);
    }
    MEASURING.with(|m| m.set(false));
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0, "look-at allocated");
}
