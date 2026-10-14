//! Foot planting against the issue's acceptance: over a procedural walk cycle a planted foot slides at most 1 mm over
//! its stance and its sole stays within 2 degrees of the ground, the knee never pops, an idle weight shift keeps both
//! feet planted, the hips drop when a leg cannot reach and rise back smoothly, a foot hovering at a threshold does not
//! chatter, and a solve is deterministic and allocates nothing.
//!
//! The "animation" is generated in the test (sinusoidal hip path, hermite foot paths) and posed with the two-bone
//! solver; no motion data. Expectations come from forward kinematics of the solved pose. It is its own test binary
//! because the counting allocator is global; it counts only the measuring thread.

#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::needless_range_loop // feet are indexed by side in several parallel arrays
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{BoneDef, HumanoidBone as B, IkGoals, IkSolver, Limb, Pose, Skeleton, Transform};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_rig::ik::TwoBoneIk;
use omuzaki_rig::ik::feet::{Config, FootPlanting, PLANT_HEIGHT, RELEASE_HEIGHT};

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

const FEET: [B; 2] = [B::LeftFoot, B::RightFoot];
const KNEES: [B; 2] = [B::LeftLowerLeg, B::RightLowerLeg];

/// The character, its planter and the facts tests measure against.
struct Rig {
    ch: common::Character,
    planter: FootPlanting,
    /// The ankle's height above the sole at rest.
    ankle: f32,
    /// The sole normal in each foot bone's frame.
    sole: [Vec3; 2],
}

impl Rig {
    fn new(tilted: bool, fps: f32) -> Self {
        let ch = common::full_character(tilted);
        let config = Config { frame_dt: 1.0 / fps, ..Config::default() };
        let planter = FootPlanting::new(&ch.skeleton, config).unwrap();
        let ankle = ch.rest_pos[ch.id(B::LeftFoot)].y - ch.rest_pos[ch.id(B::LeftToes)].y;
        let sole = FEET.map(|f| ch.rest_rot[ch.id(f)].inverse() * Vec3::Y);
        Self { ch, planter, ankle, sole }
    }

    /// The animated pose with the hips at `hips` and the feet at `feet` (root space), knees forward.
    fn animate(&self, hips: Vec3, feet: [Vec3; 2]) -> Pose {
        let mut pose = self.ch.skeleton.rest_pose();
        let id = self.ch.id(B::Hips);
        let parent = self.ch.skeleton.bones()[id].parent.map_or(Quat::IDENTITY, |p| self.ch.rest_rot[p.index()]);
        pose.local[id].translation += parent.inverse() * (hips - self.ch.rest_pos[id]);
        let limb = |foot: Vec3| Some(Limb { target: foot, pole: foot + Vec3::new(0.0, 0.4, 1.0), weight: 1.0 });
        let goals = IkGoals { left_foot: limb(feet[0]), right_foot: limb(feet[1]), ..IkGoals::NONE };
        TwoBoneIk::new().solve(&self.ch.skeleton, &goals, &mut pose);
        pose
    }

    fn world(&self, pose: &Pose) -> Pose {
        let mut out = Pose::IDENTITY;
        self.ch.skeleton.world_pose(pose, &mut out);
        out
    }

    fn position(&self, world: &Pose, bone: B) -> Vec3 {
        world.local[self.ch.id(bone)].translation
    }

    /// Angle (degrees) between the foot's sole normal and the ground's.
    fn sole_tilt(&self, world: &Pose, side: usize) -> f32 {
        let normal = world.local[self.ch.id(FEET[side])].rotation * self.sole[side];
        normal.dot(Vec3::Y).clamp(-1.0, 1.0).acos().to_degrees()
    }

    fn solve(&mut self, pose: &mut Pose) {
        let both = Some(Limb { target: Vec3::ZERO, pole: Vec3::ZERO, weight: 1.0 });
        let goals = IkGoals { left_foot: both, right_foot: both, ..IkGoals::NONE };
        self.planter.solve(&self.ch.skeleton, &goals, pose);
    }
}

/// A foot's root-space path in a walk: stance slides back at the body's speed, the swing is a hermite curve that meets
/// the stance with the same velocity (so the foot's world speed is zero at touchdown and lift-off).
fn walk_foot(phase: f32, ankle: f32, speed: f32, stance: f32) -> Vec3 {
    const SWING_HEIGHT: f32 = 0.08;
    let phase = phase.rem_euclid(1.0);
    let half = speed * stance * 0.5;
    if phase < stance {
        return Vec3::new(0.0, ankle, half - speed * phase);
    }
    let swing = 1.0 - stance;
    let u = (phase - stance) / swing;
    let (p0, p1, m) = (-half, half, -speed * swing);
    let (h00, h10, h01, h11) = (
        2.0 * u.powi(3) - 3.0 * u * u + 1.0,
        u.powi(3) - 2.0 * u * u + u,
        -2.0 * u.powi(3) + 3.0 * u * u,
        u.powi(3) - u * u,
    );
    let z = h00 * p0 + (h10 + h11) * m + h01 * p1;
    Vec3::new(0.0, ankle + SWING_HEIGHT * (std::f32::consts::PI * u).sin().powi(2), z)
}

/// What a walk records.
#[derive(Default)]
struct Walk {
    slide: f32,
    tilt: f32,
    stance_frames: [u32; 2],
    frames: u32,
    knee_steps: f32,
    max_drop: f32,
    toggles: [u32; 2],
}

const SPEED: f32 = 0.8;
const STANCE: f32 = 0.6;

/// Three walk cycles at `fps`.
fn walk(tilted: bool, fps: f32) -> Walk {
    let mut rig = Rig::new(tilted, fps);
    let mut log = Walk::default();
    let (mut was, mut knee_prev) = ([false; 2], None::<[Vec3; 2]>);
    let frames = (3.0 * fps) as u32;
    for i in 0..frames {
        let t = i as f32 / fps;
        rig.planter.set_root(Transform::from_translation(Vec3::new(0.0, 0.0, SPEED * t)));
        let sway = 0.015 * (std::f32::consts::TAU * t).sin();
        let hips = Vec3::new(sway, 0.88 + 0.01 * (2.0 * std::f32::consts::TAU * t).cos(), 0.0);
        let feet = [0.0, 0.5].map(|offset| walk_foot(t + offset, rig.ankle, SPEED, STANCE));
        let side = |s: f32, p: Vec3| Vec3::new(s * 0.08, 0.0, 0.0) + p;
        let mut pose = rig.animate(hips, [side(1.0, feet[0]), side(-1.0, feet[1])]);
        rig.solve(&mut pose);
        let world = rig.world(&pose);
        let knees = KNEES.map(|k| rig.position(&world, k));
        if let Some(prev) = knee_prev {
            log.knee_steps = log.knee_steps.max(knees[0].distance(prev[0])).max(knees[1].distance(prev[1]));
        }
        knee_prev = Some(knees);
        log.max_drop = log.max_drop.max(rig.planter.hip_drop());
        for s in 0..2 {
            let status = rig.planter.status(s);
            log.toggles[s] += u32::from(status.planted != was[s]);
            was[s] = status.planted;
            if status.planted && status.blend >= 1.0 {
                let ankle = Vec3::new(0.0, 0.0, SPEED * t) + rig.position(&world, FEET[s]);
                log.slide = log.slide.max(ankle.distance(status.lock));
                log.tilt = log.tilt.max(rig.sole_tilt(&world, s));
                log.stance_frames[s] += 1;
            }
        }
        log.frames += 1;
    }
    log
}

#[test]
fn a_walk_cycle_plants_each_foot_without_sliding_and_with_flat_soles() {
    for tilted in [false, true] {
        let log = walk(tilted, 60.0);
        assert!(log.slide <= 1e-3, "tilted={tilted}: a planted foot slid {} m", log.slide);
        assert!(log.tilt <= 2.0, "tilted={tilted}: a planted sole was {} degrees off the ground", log.tilt);
        for s in 0..2 {
            // Not vacuous: each foot is fully locked for most of its 60% stance, three times over.
            assert!(
                log.stance_frames[s] >= 60,
                "tilted={tilted}: foot {s} was locked on {} frames",
                log.stance_frames[s]
            );
            // Three steps each, counting the plant at the start: no chatter, which would add toggles by the dozen.
            assert!((5..=7).contains(&log.toggles[s]), "tilted={tilted}: foot {s} toggled {} times", log.toggles[s]);
        }
        assert!(log.max_drop < 1e-6, "a reachable walk must not drop the hips ({} m)", log.max_drop);
        eprintln!(
            "walk tilted={tilted}: slide {:.3} mm, tilt {:.3} deg, stance frames {:?} of {}",
            log.slide * 1e3,
            log.tilt,
            log.stance_frames,
            log.frames
        );
    }
}

#[test]
fn the_knee_never_pops_when_a_foot_plants_or_releases() {
    // Continuity by refinement: a jump keeps its size however finely the walk is sampled, a smooth path shrinks with
    // the step. Sampling at 4x the rate must shrink the largest knee step to about a quarter.
    for tilted in [false, true] {
        let (coarse, fine) = (walk(tilted, 60.0), walk(tilted, 240.0));
        assert!(coarse.knee_steps < 0.06, "tilted={tilted}: the knee moved {} m in one frame", coarse.knee_steps);
        assert!(
            fine.knee_steps < 0.5 * coarse.knee_steps,
            "tilted={tilted}: knee step {} at 240 Hz vs {} at 60 Hz: a jump, not a slope",
            fine.knee_steps,
            coarse.knee_steps
        );
    }
}

#[test]
fn an_idle_weight_shift_keeps_both_feet_planted() {
    let mut rig = Rig::new(true, 60.0);
    let feet = [Vec3::new(0.08, rig.ankle, 0.02), Vec3::new(-0.08, rig.ankle, -0.02)];
    let mut slide = 0.0_f32;
    for i in 0..240 {
        let t = i as f32 / 60.0;
        // The weight moves from foot to foot: hips sway 4 cm and bob 1 cm, the feet stay.
        let hips = Vec3::new(0.04 * (0.5 * std::f32::consts::TAU * t).sin(), 0.89 - 0.005 * (t * 3.0).cos(), 0.0);
        let mut pose = rig.animate(hips, feet);
        rig.solve(&mut pose);
        let world = rig.world(&pose);
        for s in 0..2 {
            let status = rig.planter.status(s);
            assert!(status.planted, "frame {i}: foot {s} not planted");
            slide = slide.max(rig.position(&world, FEET[s]).distance(status.lock));
        }
    }
    assert!(slide <= 1e-3, "idle feet slid {slide} m");
}

#[test]
fn the_hips_drop_when_a_leg_cannot_reach_and_rise_back_smoothly() {
    let mut rig = Rig::new(false, 60.0);
    rig.planter.set_contacts([Some(true), Some(true)]);
    let feet = [Vec3::new(0.08, rig.ankle, 0.0), Vec3::new(-0.08, rig.ankle, 0.0)];
    let (mut previous, mut peak, mut lock) = (0.0_f32, 0.0_f32, None);
    for i in 0..300 {
        // Hips rise from 0.88 m to 1.00 m (out of reach of a planted foot) and come back down.
        let lift = (std::f32::consts::PI * (i as f32 / 300.0)).sin();
        let hips = Vec3::new(0.0, 0.88 + 0.12 * lift, 0.0);
        let mut pose = rig.animate(hips, feet);
        rig.solve(&mut pose);
        let world = rig.world(&pose);
        let drop = rig.planter.hip_drop();
        assert!(previous - drop <= 0.4 / 60.0 + 1e-6, "frame {i}: the hips rose {} m in one frame", previous - drop);
        previous = drop;
        peak = peak.max(drop);
        for s in 0..2 {
            let at = rig.position(&world, FEET[s]);
            let locked = *lock.get_or_insert(rig.planter.status(s).lock);
            if i > 10 && s == 0 {
                assert!(at.distance(locked) <= 1e-3, "frame {i}: the planted foot moved {} m", at.distance(locked));
            }
        }
        // The hips end up exactly the animated height minus the drop.
        let hips_now = rig.position(&world, B::Hips).y;
        assert!((hips_now - (hips.y - drop)).abs() < 1e-4, "frame {i}: hips {hips_now} vs {}", hips.y - drop);
    }
    assert!(peak > 0.05, "the hips should have dropped, peak {peak}");
    assert!(previous < 1e-6, "the hips should be back up, drop {previous}");
}

#[test]
fn a_foot_hovering_at_a_threshold_does_not_chatter() {
    for around in [PLANT_HEIGHT, RELEASE_HEIGHT] {
        let mut rig = Rig::new(false, 60.0);
        let mut toggles = 0;
        let mut was = false;
        for i in 0..240 {
            // Start low so the foot plants, rise to the threshold over a second, then hover +-0.5 mm round it, a
            // different sign each frame.
            let wobble = if i % 2 == 0 { 0.0005 } else { -0.0005 };
            let height = if i < 5 {
                0.0
            } else {
                around * ((i - 5) as f32 / 60.0).min(1.0) + wobble * ((i - 5) as f32 / 60.0).min(1.0).ceil()
            };
            let feet = [Vec3::new(0.08, rig.ankle + height, 0.0), Vec3::new(-0.08, rig.ankle, 0.0)];
            let mut pose = rig.animate(Vec3::new(0.0, 0.88, 0.0), feet);
            rig.solve(&mut pose);
            let planted = rig.planter.status(0).planted;
            toggles += u32::from(planted != was);
            was = planted;
        }
        // Planted once at the start. Near the plant threshold it stays planted; the release threshold it crosses once,
        // and then it stays released, since planting again needs it back under the (lower) plant threshold.
        let expected = if around == RELEASE_HEIGHT { 2 } else { 1 };
        assert_eq!(toggles, expected, "a foot at {around} m toggled {toggles} times");
    }
}

#[test]
fn zero_weight_and_missing_goals_leave_the_pose_bit_identical() {
    let mut rig = Rig::new(true, 60.0);
    let pose =
        rig.animate(Vec3::new(0.0, 0.88, 0.0), [Vec3::new(0.08, rig.ankle, 0.1), Vec3::new(-0.08, rig.ankle, -0.1)]);
    let off = Limb { target: Vec3::ZERO, pole: Vec3::ZERO, weight: 0.0 };
    for goals in [IkGoals::NONE, IkGoals { left_foot: Some(off), right_foot: Some(off), ..IkGoals::NONE }] {
        let mut out = pose.clone();
        rig.planter.solve(&rig.ch.skeleton, &goals, &mut out);
        assert_eq!(out, pose);
    }
}

#[test]
fn a_non_humanoid_skeleton_has_no_planter() {
    let bone = BoneDef { name: "root".into(), parent: None, rest: Transform::IDENTITY, humanoid: None };
    let generic = Skeleton::generic(vec![bone]).unwrap();
    assert!(FootPlanting::new(&generic, Config::default()).is_none());
}

#[test]
fn a_solve_is_deterministic_and_allocates_nothing() {
    let run = || {
        let mut rig = Rig::new(true, 60.0);
        let mut last = Pose::IDENTITY;
        let mut allocations = 0;
        for i in 0..180 {
            let t = i as f32 / 60.0;
            rig.planter.set_root(Transform::from_translation(Vec3::new(0.0, 0.0, SPEED * t)));
            let feet = [0.0, 0.5].map(|o| walk_foot(t + o, rig.ankle, SPEED, STANCE) + Vec3::new(0.08, 0.0, 0.0));
            let mut pose = rig.animate(Vec3::new(0.0, 0.88, 0.0), [feet[0], feet[1] * Vec3::new(-1.0, 1.0, 1.0)]);
            let before = ALLOCATIONS.load(Ordering::Relaxed);
            MEASURING.with(|m| m.set(true));
            rig.solve(&mut pose);
            MEASURING.with(|m| m.set(false));
            allocations += ALLOCATIONS.load(Ordering::Relaxed) - before;
            last = pose;
        }
        (last, allocations)
    };
    let (a, allocations) = run();
    let (b, _) = run();
    assert_eq!(a, b, "two identical runs differ");
    assert_eq!(allocations, 0, "a solve allocated");
}

#[test]
fn a_leg_nobody_asked_for_stays_exactly_as_animated() {
    // Only the left foot has a goal; the right leg is straight enough that a full-strength solve would shorten it.
    let mut rig = Rig::new(true, 60.0);
    let feet = [Vec3::new(0.08, rig.ankle, 0.0), Vec3::new(-0.08, rig.ankle, 0.0)];
    let pose = rig.animate(Vec3::new(0.0, 0.9, 0.0), feet);
    let mut out = pose.clone();
    let goals =
        IkGoals { left_foot: Some(Limb { target: Vec3::ZERO, pole: Vec3::ZERO, weight: 1.0 }), ..IkGoals::NONE };
    rig.planter.solve(&rig.ch.skeleton, &goals, &mut out);
    assert!(rig.planter.status(0).planted && rig.planter.hip_drop() == 0.0);
    for bone in [B::RightUpperLeg, B::RightLowerLeg, B::RightFoot] {
        let id = rig.ch.id(bone);
        assert_eq!(out.local[id], pose.local[id], "{bone:?} moved with no goal on the right foot");
    }
}

#[test]
fn the_toe_off_fades_to_nothing_with_the_goal_weight() {
    let tilt_after_release = |weight: f32| {
        let mut rig = Rig::new(false, 60.0);
        let feet = [Vec3::new(0.08, rig.ankle, 0.0), Vec3::new(-0.08, rig.ankle, 0.0)];
        let goals = |weight| {
            let limb = Some(Limb { target: Vec3::ZERO, pole: Vec3::ZERO, weight });
            IkGoals { left_foot: limb, right_foot: limb, ..IkGoals::NONE }
        };
        rig.planter.set_contacts([Some(true), Some(true)]);
        for _ in 0..30 {
            let mut pose = rig.animate(Vec3::new(0.0, 0.88, 0.0), feet);
            rig.planter.solve(&rig.ch.skeleton, &goals(1.0), &mut pose);
        }
        rig.planter.set_contacts([Some(false), Some(false)]);
        let animated = rig.animate(Vec3::new(0.0, 0.88, 0.0), feet);
        let mut pose = animated.clone();
        rig.planter.solve(&rig.ch.skeleton, &goals(weight), &mut pose);
        (rig.sole_tilt(&rig.world(&pose), 0), rig.sole_tilt(&rig.world(&animated), 0))
    };
    let (lifted, animated) = tilt_after_release(1.0);
    assert!((lifted - animated).abs() > 3.0, "a released foot should lift its heel ({lifted} vs {animated})");
    let (tiny, animated) = tilt_after_release(1e-6);
    assert!((tiny - animated).abs() < 0.01, "toe-off at weight 1e-6 turned the sole {tiny} vs {animated} degrees");
}

#[test]
fn hip_compensation_fades_in_with_the_planting_weight() {
    // The straight rest pose is beyond the soft reach, so a planted foot there asks the hips to drop. A hair of weight
    // must move the pose a hair (at most a metre per unit weight, the limb's own size), not by the full correction.
    let moved = |weight: f32| {
        let mut rig = Rig::new(false, 60.0);
        rig.planter.set_contacts([Some(true), Some(true)]);
        let pose = rig.ch.skeleton.rest_pose();
        let mut out = pose.clone();
        let limb = Some(Limb { target: Vec3::ZERO, pole: Vec3::ZERO, weight });
        rig.planter.solve(&rig.ch.skeleton, &IkGoals { left_foot: limb, right_foot: limb, ..IkGoals::NONE }, &mut out);
        let (before, after) = (rig.world(&pose), rig.world(&out));
        [B::Hips, B::LeftLowerLeg, B::LeftFoot]
            .map(|b| rig.position(&before, b).distance(rig.position(&after, b)))
            .into_iter()
            .fold(0.0_f32, f32::max)
    };
    assert!(moved(1.0) > 0.01, "weight 1 should move the pose");
    for weight in [1e-6, 1e-3, 0.1] {
        assert!(moved(weight) <= weight + 1e-5, "weight {weight} moved the pose {} m", moved(weight));
    }
}

#[test]
fn a_faint_goal_on_one_foot_cannot_jump_its_leg_while_the_other_holds_the_hips_down() {
    // The left foot is planted at full strength with the hips too high for it, so they stay down; the right foot's
    // planting strength goes through zero. Its leg must move in proportion to the strength.
    let right_leg = |weight: f32| {
        let mut rig = Rig::new(false, 60.0);
        rig.planter.set_contacts([Some(true), Some(true)]);
        let feet = [Vec3::new(0.08, rig.ankle, 0.0), Vec3::new(-0.08, rig.ankle, 0.0)];
        let limb = |weight| Some(Limb { target: Vec3::ZERO, pole: Vec3::ZERO, weight });
        let goals = IkGoals { left_foot: limb(1.0), right_foot: limb(weight), ..IkGoals::NONE };
        let mut world = Pose::IDENTITY;
        for _ in 0..40 {
            let mut pose = rig.animate(Vec3::new(0.0, 0.98, 0.0), feet);
            rig.planter.solve(&rig.ch.skeleton, &goals, &mut pose);
            world = rig.world(&pose);
        }
        assert!(rig.planter.hip_drop() > 0.01, "the hips should be held down");
        [rig.position(&world, B::RightLowerLeg), rig.position(&world, B::RightFoot)]
    };
    let none = right_leg(0.0);
    for weight in [1e-6, 1e-3, 0.05] {
        let faint = right_leg(weight);
        let jump = faint[0].distance(none[0]).max(faint[1].distance(none[1]));
        assert!(jump <= 3.0 * weight + 1e-4, "right foot weight {weight} moved its leg {jump} m");
    }
}
