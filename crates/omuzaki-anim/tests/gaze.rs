//! Gaze targets: parity with the prototype's camera follow, per-eye convergence, easing, frame
//! invariance, determinism, and zero allocation in the frame.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::cast_precision_loss, clippy::float_cmp)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::gaze::{
    DRIVEN_RATE, FOLLOW_RATE, GazeFrame, GazeKind, GazeSolver, GazeTarget, HALF_EYE_SPACING, Resolved,
    X_RADIANS_PER_UNIT,
};
use omuzaki_anim_api::{Bus, Channel, ChannelMask, Source, Tick};
use omuzaki_core::glam::{DMat4, DVec3, Mat4, Quat, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_core::time::Seconds;

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

/// The eyes sit 0.22 above the head joint, as in `face.js`.
const EYE_HEIGHT: f32 = 0.22;

#[allow(clippy::unnecessary_wraps)] // the solver takes an `Option`; call sites read better with the wrap here
fn driven(position: Vec3) -> Option<Resolved> {
    Some(Resolved { position, kind: GazeKind::Driven })
}

#[allow(clippy::unnecessary_wraps)] // as `driven`
fn follow(position: Vec3) -> Option<Resolved> {
    Some(Resolved { position, kind: GazeKind::Follow })
}

/// One snapped solve (a zero step), the way a first frame lands.
fn snap(eye_frame: Mat4, target: Vec3) -> (GazeFrame, Bus) {
    let mut bus = Bus::default();
    let frame = GazeSolver::default().step(
        Tick { index: 0, time: Seconds::default(), dt: 0.0 },
        eye_frame,
        follow(target),
        &mut bus,
    );
    (frame, bus)
}

fn eye_frame_at_head(head: Mat4) -> Mat4 {
    head * Mat4::from_translation(Vec3::new(0.0, EYE_HEIGHT, 0.0))
}

/// `face.js` `update`, camera follow, in f64: the reference the solver is held to.
fn prototype_look(eye_frame: Mat4, camera: Vec3) -> (f64, f64) {
    let m = DMat4::from_cols_array(&eye_frame.to_cols_array().map(f64::from));
    let v = m.inverse().transform_point3(DVec3::new(f64::from(camera.x), f64::from(camera.y), f64::from(camera.z)));
    let l = v.length();
    if v.z / l > 0.25 {
        ((v.x.atan2(v.z) / 0.7).clamp(-1.0, 1.0), ((v.y / l).asin() / 0.6).clamp(-1.0, 1.0))
    } else {
        (0.0, 0.0)
    }
}

#[test]
#[allow(clippy::unreadable_literal, clippy::excessive_precision)] // digits pasted from the Node reference, kept verbatim
fn pinned_prototype_vectors_match_within_1e_6() {
    // Computed by running the `face.js` formula in Node (head at the origin, so the camera is relative to the joint).
    let cases: &[(f32, f32, f32, f64, f64)] = &[
        (0.930924296, -0.560742617, 1.39490092, 0.84070566679774961, -0.72619097276730793),
        (2.97528648, 0.937845945, 1.63874018, 1.0000000000000000, 0.34711614898269222),
        (-1.96381187, 1.75268829, 1.84364307, -1.0000000000000000, 0.86219828636709184),
        (1.96520734, 1.17767739, -0.373549610, 0.0000000000000000, 0.0000000000000000),
        (1.76337647, -1.01115966, 1.07922935, 1.0000000000000000, -0.89517947500322759),
        (-1.59742785, -0.944206357, 2.32544112, -0.85986265909938275, -0.65228176930543957),
        (2.22783852, 1.59594989, -1.73196828, 0.0000000000000000, 0.0000000000000000),
        (-0.177675068, 1.14533257, -0.203919560, 0.0000000000000000, 0.0000000000000000),
        (-1.32409644, -0.306978017, 3.11234641, -0.57462538926979978, -0.25760350986226910),
        (2.22458768, -0.102924138, 0.171933025, 0.0000000000000000, 0.0000000000000000),
        (1.91824937, -1.08909392, -1.45013237, 0.0000000000000000, 0.0000000000000000),
        (-1.09277737, -0.272084266, 0.626984119, -1.0000000000000000, -0.62060404257757162),
    ];
    for &(x, y, z, want_x, want_y) in cases {
        let mut s = GazeSolver::default();
        s.step(
            Tick { index: 0, time: Seconds::default(), dt: 0.0 },
            eye_frame_at_head(Mat4::IDENTITY),
            follow(Vec3::new(x, y, z)),
            &mut Bus::default(),
        );
        let (lx, ly) = s.look();
        assert!((f64::from(lx) - want_x).abs() < 1e-6, "x for ({x}, {y}, {z}): {lx} vs {want_x}");
        assert!((f64::from(ly) - want_y).abs() < 1e-6, "y for ({x}, {y}, {z}): {ly} vs {want_y}");
    }
}

#[test]
fn a_thousand_random_cameras_match_the_prototype_formula() {
    let mut rng = Rng::new(495);
    let mut spread = |scale: f32| (rng.next_f32() - 0.5) * scale;
    for i in 0..1000 {
        let rotation = Quat::from_euler(omuzaki_core::glam::EulerRot::YXZ, spread(6.0), spread(1.5), spread(1.0));
        let head = Mat4::from_rotation_translation(rotation, Vec3::new(spread(2.0), spread(1.0), spread(2.0)));
        let eye_frame = eye_frame_at_head(head);
        let camera = Vec3::new(spread(8.0), spread(4.0), spread(8.0));
        let (want_x, want_y) = prototype_look(eye_frame, camera);
        let mut s = GazeSolver::default();
        s.step(Tick { index: 0, time: Seconds::default(), dt: 0.0 }, eye_frame, follow(camera), &mut Bus::default());
        let (x, y) = s.look();
        assert!((f64::from(x) - want_x).abs() < 1e-6, "{i}: x {x} vs {want_x}");
        assert!((f64::from(y) - want_y).abs() < 1e-6, "{i}: y {y} vs {want_y}");
    }
}

#[test]
fn a_target_at_half_a_metre_converges_by_the_eye_spacing() {
    let (frame, bus) = snap(Mat4::IDENTITY, Vec3::new(0.0, 0.0, 0.5));
    let per_eye = (HALF_EYE_SPACING / 0.5).atan();
    assert!((per_eye.to_degrees() - 10.2).abs() < 0.1, "the expected angle is {} degrees", per_eye.to_degrees());
    // Each eye turns inward by that angle; the channel carries it divided by the look scale.
    let unit = per_eye / X_RADIANS_PER_UNIT;
    assert!((frame.vergence() - unit).abs() < 1e-6);
    assert!((frame.vergence() * X_RADIANS_PER_UNIT).to_degrees() - 10.2 < 0.1);
    assert!((bus.values[Channel::EYE_LOOK_IN_LEFT] - unit).abs() < 1e-6);
    assert!((bus.values[Channel::EYE_LOOK_IN_RIGHT] - unit).abs() < 1e-6);
    assert_eq!(bus.values[Channel::EYE_LOOK_OUT_LEFT] + bus.values[Channel::EYE_LOOK_OUT_RIGHT], 0.0);
    // What the painter derives from the channels agrees with what the solver reports.
    let b = &bus.values;
    let painter = f32::midpoint(b[Channel::EYE_LOOK_IN_LEFT], b[Channel::EYE_LOOK_IN_RIGHT])
        - f32::midpoint(b[Channel::EYE_LOOK_OUT_LEFT], b[Channel::EYE_LOOK_OUT_RIGHT]);
    assert!((painter - frame.vergence()).abs() < 1e-6);
    // The mean gaze stays on the centre line.
    assert!(b.gaze().0.abs() < 1e-6);
}

#[test]
fn vergence_falls_away_with_distance_and_far_targets_go_parallel() {
    let at = |z: f32| snap(Mat4::IDENTITY, Vec3::new(0.0, 0.0, z)).0.vergence();
    // At 3 m each eye is only 1.7 degrees (0.043 look units) off parallel; it is the true angle, not a cut-off.
    assert!((at(3.0) - (HALF_EYE_SPACING / 3.0).atan() / X_RADIANS_PER_UNIT).abs() < 1e-6);
    assert!(at(3.0) < 0.05);
    assert!(at(0.5) > at(1.0) && at(1.0) > at(3.0));
    assert!(at(1000.0) < 2e-4, "{}", at(1000.0));
    // Parallel means both eyes carry the same look.
    let (frame, _) = snap(Mat4::IDENTITY, Vec3::new(0.3, 0.1, 1000.0));
    assert!((frame.left.0 - frame.right.0).abs() < 3e-4 && (frame.left.1 - frame.right.1).abs() < 1e-6);
}

#[test]
fn looking_toward_her_left_turns_the_left_eye_out_and_the_right_eye_in() {
    let (_, bus) = snap(Mat4::IDENTITY, Vec3::new(2.0, 1.0, 4.0));
    let b = &bus.values;
    assert!(b[Channel::EYE_LOOK_OUT_LEFT] > 0.0 && b[Channel::EYE_LOOK_IN_RIGHT] > 0.0);
    assert_eq!(b[Channel::EYE_LOOK_IN_LEFT] + b[Channel::EYE_LOOK_OUT_RIGHT], 0.0);
    assert!(b[Channel::EYE_LOOK_UP_LEFT] > 0.0 && b[Channel::EYE_LOOK_UP_RIGHT] > 0.0);
    assert_eq!(b[Channel::EYE_LOOK_DOWN_LEFT] + b[Channel::EYE_LOOK_DOWN_RIGHT], 0.0);
}

#[test]
fn a_camera_behind_the_head_eases_back_to_centre_without_a_jump() {
    let eye_frame = eye_frame_at_head(Mat4::IDENTITY);
    let mut solver = GazeSolver::default();
    let mut bus = Bus::default();
    let dt = 1.0 / 60.0;
    let tick = |i: u64| Tick { index: i, time: Seconds::default(), dt };
    // Settle on a camera in front and to the side.
    for i in 0..120 {
        solver.step(tick(i), eye_frame, follow(Vec3::new(1.0, 0.0, 2.0)), &mut bus);
    }
    let settled = solver.look();
    assert!(settled.0 > 0.5, "settled at {settled:?}");
    // The camera swings behind (z / |v| <= 0.25): each frame covers the follow ease and no more.
    let k = 1.0 - (-dt * FOLLOW_RATE).exp();
    let mut previous = settled;
    for i in 120..180 {
        solver.step(tick(i), eye_frame, follow(Vec3::new(1.0, 0.0, -2.0)), &mut bus);
        let now = solver.look();
        assert!((now.0 - previous.0 * (1.0 - k)).abs() < 1e-6, "frame {i}: {now:?} after {previous:?}");
        assert!(now.0 < previous.0, "monotone return");
        previous = now;
    }
    assert!(previous.0 < 0.01, "back near centre after a second: {previous:?}");
    // The cone edge itself: just inside follows, just outside does not.
    assert!(snap(Mat4::IDENTITY, Vec3::new(1.0, 0.0, 0.3)).0.look().0 != 0.0);
    assert_eq!(snap(Mat4::IDENTITY, Vec3::new(1.0, 0.0, 0.2)).0, GazeFrame::default());
}

#[test]
fn easing_uses_the_follow_and_driven_rates_and_snaps_on_a_zero_step() {
    let eye_frame = Mat4::IDENTITY;
    let target = Vec3::new(0.0, 0.0, 2.0) + Vec3::X; // 26.6 degrees to the left: about 0.66 look units
    let goal = (1.0_f32).atan2(2.0) / X_RADIANS_PER_UNIT;
    for (kind, rate) in [(GazeKind::Follow, FOLLOW_RATE), (GazeKind::Driven, DRIVEN_RATE)] {
        let mut solver = GazeSolver::default();
        let dt = 0.02;
        solver.step(
            Tick { index: 1, time: Seconds::default(), dt },
            eye_frame,
            Some(Resolved { position: target, kind }),
            &mut Bus::default(),
        );
        let want = goal * (1.0 - (-dt * rate).exp());
        assert!((solver.look().0 - want).abs() < 1e-6, "{kind:?}: {} vs {want}", solver.look().0);
    }
    let mut snapped = GazeSolver::default();
    snapped.step(Tick { index: 0, time: Seconds::default(), dt: 0.0 }, eye_frame, follow(target), &mut Bus::default());
    assert!((snapped.look().0 - goal).abs() < 1e-6);
}

#[test]
fn a_scaled_and_rotated_character_looks_the_same_way() {
    let target = Vec3::new(0.4, 0.3, 0.7);
    let (reference, _) = snap(Mat4::IDENTITY, target);
    let world = Mat4::from_scale_rotation_translation(
        Vec3::splat(2.5),
        Quat::from_euler(omuzaki_core::glam::EulerRot::YXZ, 1.1, -0.4, 0.3),
        Vec3::new(3.0, -1.0, 8.0),
    );
    let (moved, _) = snap(world, world.transform_point3(target));
    for (a, b) in [(reference.left, moved.left), (reference.right, moved.right)] {
        assert!((a.0 - b.0).abs() < 1e-5 && (a.1 - b.1).abs() < 1e-5, "{a:?} vs {b:?}");
    }
}

#[test]
fn no_target_or_a_human_on_the_eyes_leaves_the_bus_alone_and_resets() {
    let eye_frame = Mat4::IDENTITY;
    let tick = Tick { index: 0, time: Seconds::default(), dt: 0.0 };
    let mut solver = GazeSolver::default();
    let mut bus = Bus::default();
    bus.values[Channel::EYE_LOOK_UP_LEFT] = 0.3;
    let before = bus;
    assert_eq!(solver.step(tick, eye_frame, GazeTarget::Off.resolve(&NoScene), &mut bus), GazeFrame::default());
    assert_eq!(bus, before);

    solver.step(tick, eye_frame, follow(Vec3::new(1.0, 0.0, 2.0)), &mut bus);
    assert_eq!(bus.owners.of(Channel::EYE_LOOK_IN_RIGHT), Source::Score);
    assert_eq!(bus.owners.of(Channel::JAW_OPEN), Source::Rest, "only the gaze region is claimed");

    let mut person = Bus::default();
    person.owners.claim(Source::Tracker, ChannelMask::GAZE);
    person.values[Channel::EYE_LOOK_OUT_LEFT] = 0.6;
    let before = person;
    solver.step(tick, eye_frame, follow(Vec3::new(1.0, 0.0, 2.0)), &mut person);
    assert_eq!(person, before, "never fight a human");
    assert_eq!(solver.look(), (0.0, 0.0), "the easing restarts from rest");
}

struct NoScene;
impl omuzaki_anim::gaze::GazeScene for NoScene {
    fn camera_position(&self) -> Option<Vec3> {
        None
    }
    fn entity_position(&self, _: omuzaki_anim::gaze::EntityId) -> Option<Vec3> {
        None
    }
}

#[test]
fn non_finite_inputs_ease_to_centre_and_never_write_nan() {
    let mut solver = GazeSolver::default();
    let mut bus = Bus::default();
    let tick = Tick::at_rate(1, 60.0);
    solver.step(tick, Mat4::IDENTITY, follow(Vec3::new(f32::NAN, 0.0, 1.0)), &mut bus);
    solver.step(tick, Mat4::IDENTITY, follow(Vec3::new(0.0, 0.0, f32::INFINITY)), &mut bus);
    solver.step(Tick { dt: f32::NAN, ..tick }, Mat4::IDENTITY, follow(Vec3::new(1.0, 0.0, 2.0)), &mut bus);
    assert!(bus.values.as_slice().iter().all(|v| v.is_finite()));
}

#[test]
fn the_same_inputs_give_the_same_channels() {
    let run = || {
        let mut solver = GazeSolver::default();
        let mut bus = Bus::default();
        for i in 0..240 {
            let t = i as f32 * 0.05;
            solver.step(
                Tick::at_rate(i, 60.0),
                Mat4::IDENTITY,
                driven(Vec3::new(t.sin(), 0.2 * t.cos(), 1.5)),
                &mut bus,
            );
        }
        bus.values.checksum()
    };
    assert_eq!(run(), run());
}

#[test]
fn a_frame_of_the_solver_allocates_nothing() {
    let mut solver = GazeSolver::default();
    let mut bus = Bus::default();
    let head = Mat4::from_rotation_y(0.3);
    let mut run = |from: u64, to: u64| {
        for i in from..to {
            let t = i as f32 * 0.02;
            let target = Vec3::new(2.0 * t.sin(), 0.3 * t.cos(), 0.6 + t.cos());
            let kind = if i % 2 == 0 { GazeKind::Follow } else { GazeKind::Driven };
            solver.step(
                Tick::at_rate(i, 60.0),
                eye_frame_at_head(head),
                Some(Resolved { position: target, kind }),
                &mut bus,
            );
        }
    };
    run(0, 600);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    run(600, 3600);
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "3000 solver frames made {allocations} allocations");
}

#[test]
fn a_saturated_target_still_gives_each_eye_its_own_clamped_look() {
    // Review finding: the midpoint is clamped at 1.0 while the left eye (0.09 to its right of the target)
    // looks straight ahead; the per-eye channels must say so.
    let (frame, bus) = snap(Mat4::IDENTITY, Vec3::new(0.09, 0.0, 0.04));
    let goal = |eye_x: f32| ((0.09 - eye_x).atan2(0.04) / X_RADIANS_PER_UNIT).clamp(-1.0, 1.0);
    assert!((frame.left.0 - goal(HALF_EYE_SPACING)).abs() < 1e-6, "left eye {:?}", frame.left);
    assert!((frame.right.0 - goal(-HALF_EYE_SPACING)).abs() < 1e-6, "right eye {:?}", frame.right);
    assert!(frame.left.0.abs() < 1e-6, "the left eye looks straight ahead: {:?}", frame.left);
    assert_eq!(bus.values[Channel::EYE_LOOK_OUT_LEFT] + bus.values[Channel::EYE_LOOK_IN_LEFT], frame.left.0.abs());
    assert_eq!(bus.values[Channel::EYE_LOOK_IN_RIGHT], 1.0, "the right eye is at full lock toward her left");
}

#[test]
fn every_channel_stays_in_range_for_wild_targets() {
    let mut rng = Rng::new(7);
    for _ in 0..1000 {
        let target = Vec3::new((rng.next_f32() - 0.5) * 0.4, (rng.next_f32() - 0.5) * 0.4, rng.next_f32() * 0.3);
        let (frame, bus) = snap(Mat4::IDENTITY, target);
        for (x, y) in [frame.left, frame.right] {
            assert!(x.abs() <= 1.0 && y.abs() <= 1.0, "{target:?}: {frame:?}");
        }
        assert!(bus.values.as_slice().iter().all(|v| (0.0..=1.0).contains(v)));
    }
}
