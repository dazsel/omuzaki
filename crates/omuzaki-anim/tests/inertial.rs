//! Inertialized transitions (#2354): a switch from a gesture with the hands in front to the idle with the hands down
//! keeps the hands out of the body, keeps their velocity across the switch, plays the new source at full fidelity and
//! lands on it exactly; measured against a crossfade of the same switch.
//!
//! The body is a plain adult humanoid in metres, posed by forward kinematics over the normalised bones (T-pose rest,
//! arms along X): enough to place the hands against a torso capsule.

#![allow(clippy::unwrap_used, clippy::float_cmp, clippy::cast_precision_loss)]

use omuzaki_anim::inertial::{Inertializer, MAX_DT};
use omuzaki_anim_api::{HumanoidBone as B, HumanoidPose};
use omuzaki_core::glam::{Quat, Vec3};

const FPS: f32 = 60.0;
/// The torso: a capsule from the hips to the neck, radius its half depth at the chest.
const TORSO: (Vec3, Vec3, f32) = (Vec3::new(0.0, 0.95, 0.0), Vec3::new(0.0, 1.45, 0.0), 0.12);
/// A hand's radius about its centre.
const HAND: f32 = 0.04;

/// Rest offsets from each bone's parent joint, T-pose, metres: the left arm (the right is its mirror).
fn rest(bone: B) -> Vec3 {
    let v = Vec3::new;
    match bone {
        B::Hips => v(0.0, 0.95, 0.0),
        B::Spine | B::UpperChest => v(0.0, 0.12, 0.0),
        B::Chest => v(0.0, 0.15, 0.0),
        B::LeftShoulder => v(0.03, 0.08, 0.0),
        B::LeftUpperArm => v(0.16, 0.0, 0.0),
        B::LeftLowerArm => v(0.28, 0.0, 0.0),
        B::LeftHand => v(0.25, 0.0, 0.0),
        B::RightShoulder => v(-0.03, 0.08, 0.0),
        B::RightUpperArm => v(-0.16, 0.0, 0.0),
        B::RightLowerArm => v(-0.28, 0.0, 0.0),
        B::RightHand => v(-0.25, 0.0, 0.0),
        _ => Vec3::ZERO,
    }
}

/// The centre of each hand (8 cm past the wrist along the hand), `[left, right]`.
fn hands(pose: &HumanoidPose) -> [Vec3; 2] {
    let side = |chain: [B; 4], along: f32| {
        let mut at = rest(B::Hips) + pose.hips_offset;
        let mut rotation = pose.get(B::Hips);
        for bone in [B::Spine, B::Chest, B::UpperChest].into_iter().chain(chain) {
            at += rotation * rest(bone);
            rotation *= pose.get(bone);
        }
        at + rotation * Vec3::new(along, 0.0, 0.0)
    };
    [
        side([B::LeftShoulder, B::LeftUpperArm, B::LeftLowerArm, B::LeftHand], 0.08),
        side([B::RightShoulder, B::RightUpperArm, B::RightLowerArm, B::RightHand], -0.08),
    ]
}

/// The gap between a hand at `p` and the torso, metres (negative inside).
fn gap(p: Vec3) -> f32 {
    let (a, b, radius) = TORSO;
    let t = ((p - a).dot(b - a) / (b - a).length_squared()).clamp(0.0, 1.0);
    (p - (a + (b - a) * t)).length() - radius - HAND
}

/// Both arms set from world directions of the upper arm and forearm (left side; the right mirrors it).
fn arms(upper: Vec3, fore: Vec3) -> HumanoidPose {
    let mut pose = HumanoidPose::REST;
    for (bones, along, flip) in [
        ([B::LeftUpperArm, B::LeftLowerArm], Vec3::X, 1.0_f32),
        ([B::RightUpperArm, B::RightLowerArm], Vec3::NEG_X, -1.0),
    ] {
        let mirror = |v: Vec3| Vec3::new(v.x * flip, v.y, v.z);
        let u = Quat::from_rotation_arc(along, mirror(upper).normalize());
        let f = Quat::from_rotation_arc(along, mirror(fore).normalize());
        pose.set(bones[0], u);
        pose.set(bones[1], u.inverse() * f);
    }
    pose
}

/// The gesture: elbows forward, forearms across, the hands meeting in front of the belly and kneading (moving) there.
fn gesture(t: f32) -> HumanoidPose {
    let knead = 0.25 * (t * 7.0).sin();
    arms(Vec3::new(0.1, -0.75, 0.65), Vec3::new(-0.85, 0.05 + knead, 0.4))
}

/// The idle: arms hanging at the sides, swaying a little.
fn idle(t: f32) -> HumanoidPose {
    let sway = 0.03 * (t * 1.3).sin();
    arms(Vec3::new(0.26, -0.96, sway), Vec3::new(0.2, -0.98, 0.05 + sway))
}

/// When the gesture ends and the idle takes over, seconds and frames.
const SWITCH_AT: f32 = 1.0;
const SWITCH_FRAME: usize = 60;
/// How many frames each run has.
const FRAMES: usize = 180;

/// The hands, frame by frame, of a switch from the gesture to the idle at [`SWITCH_AT`].
fn inertialized(half_life: f32) -> (Vec<[Vec3; 2]>, Vec<HumanoidPose>) {
    let mut state = Inertializer::new(half_life);
    let (mut path, mut poses) = (Vec::new(), Vec::new());
    for i in 0..FRAMES {
        let t = i as f32 / FPS;
        let switch = (t - SWITCH_AT).abs() < 0.5 / FPS;
        if switch {
            state.switch();
        }
        let source = if t < SWITCH_AT - 0.5 / FPS { gesture(t) } else { idle(t) };
        let out = state.step(1.0 / FPS, &source);
        path.push(hands(&out));
        poses.push(out);
    }
    (path, poses)
}

/// The same switch as a linear crossfade of `fade` seconds, both sources playing.
fn crossfaded(fade: f32) -> Vec<[Vec3; 2]> {
    (0..FRAMES)
        .map(|i| {
            let t = i as f32 / FPS;
            let w = ((t - SWITCH_AT) / fade).clamp(0.0, 1.0);
            hands(&gesture(t).lerp(&idle(t), w))
        })
        .collect()
}

/// The closest any hand comes to the torso over `path`.
fn closest(path: &[[Vec3; 2]]) -> f32 {
    path.iter().flatten().map(|&p| gap(p)).fold(f32::INFINITY, f32::min)
}

/// The largest change in hand velocity from one frame to the next, metres per second.
fn largest_kick(path: &[[Vec3; 2]], from: usize, to: usize) -> f32 {
    (from.max(2)..to)
        .flat_map(|i| (0..2).map(move |h| (i, h)))
        .map(|(i, h)| {
            let v1 = (path[i][h] - path[i - 1][h]) * FPS;
            let v0 = (path[i - 1][h] - path[i - 2][h]) * FPS;
            (v1 - v0).length()
        })
        .fold(0.0, f32::max)
}

#[test]
fn a_switch_keeps_the_hands_out_of_the_body_and_their_motion_continuous() {
    let at = SWITCH_FRAME;
    let (path, poses) = inertialized(0.1);
    let fade = crossfaded(0.25);
    let cut: Vec<[Vec3; 2]> = (0..FRAMES)
        .map(|i| {
            let t = i as f32 / FPS;
            hands(&if t < SWITCH_AT - 0.5 / FPS { gesture(t) } else { idle(t) })
        })
        .collect();

    let (inert_gap, fade_gap) = (closest(&path), closest(&fade));
    // The kick in hand velocity around the switch, against the gesture's own frame-to-frame kicks before it.
    let own = largest_kick(&path, 10, at - 2);
    let (inert_kick, fade_kick, cut_kick) = (
        largest_kick(&path, at - 2, at + 30),
        largest_kick(&fade, at - 2, at + 30),
        largest_kick(&cut, at - 2, at + 30),
    );
    println!("closest hand to the torso: inertialized {:.1} mm, crossfade {:.1} mm", inert_gap * 1e3, fade_gap * 1e3);
    println!(
        "largest velocity kick: gesture alone {own:.2} m/s, inertialized {inert_kick:.2}, 0.25 s crossfade {fade_kick:.2}, \
         hard cut {cut_kick:.2}"
    );
    assert!(inert_gap > 0.0, "a hand went into the torso: {inert_gap}");
    // The velocity carries across the switch; the spring then turns the hands toward the idle, so the frame-to-frame
    // change in velocity is the spring's acceleration: smaller than a crossfade's, and a twentieth of a hard cut's.
    assert!(inert_kick < fade_kick && 20.0 * inert_kick < cut_kick, "{inert_kick} {fade_kick} {cut_kick}");

    // The hands reach the idle's to the centimetre within half a second and to the millimetre within 0.8 s, and the
    // idle is then shown exactly (bit for bit) once the offset has settled, within about a second; from there on it is
    // the idle's own motion.
    let within = |metres: f32| {
        let i = (at..path.len())
            .find(|&i| (0..2).all(|h| (path[i][h] - hands(&idle(i as f32 / FPS))[h]).length() < metres))
            .unwrap();
        (i - at) as f32 / FPS
    };
    let (cm, mm) = (within(0.01), within(0.001));
    let landing =
        poses.iter().enumerate().skip(at).find(|(i, p)| **p == idle(*i as f32 / FPS)).map(|(i, _)| i).unwrap();
    let landed = (landing - at) as f32 / FPS;
    println!(
        "after the switch: hands within 1 cm of the idle's at {cm:.2} s, 1 mm at {mm:.2} s, exact at {landed:.2} s"
    );
    assert!(cm < 0.5 && mm < 0.8 && landed < 1.2, "{cm} {mm} {landed}");
    assert!(poses[landing..].iter().enumerate().all(|(k, p)| *p == idle((landing + k) as f32 / FPS)));
}

#[test]
fn the_first_step_shows_its_source_and_steps_are_clamped() {
    let mut state = Inertializer::default();
    // Before any step there is nothing to carry.
    state.switch();
    assert_eq!(state.step(1.0 / FPS, &idle(0.0)), idle(0.0));
    assert!(!state.is_transitioning());
    // A switch to the same pose is no transition at all; a stalled frame (or a broken dt) cannot fling it.
    state.switch();
    assert_eq!(state.step(f32::NAN, &idle(0.0)), idle(0.0));
    state.switch();
    let out = state.step(10.0, &gesture(0.0));
    let one = {
        let mut s = Inertializer::default();
        s.step(0.0, &idle(0.0));
        s.switch();
        s.step(MAX_DT, &gesture(0.0))
    };
    assert_eq!(out, one, "a 10 s step is a MAX_DT step");
    // Reset forgets the last pose.
    state.reset();
    state.switch();
    assert_eq!(state.step(1.0 / FPS, &gesture(1.0)), gesture(1.0));
}

#[test]
fn a_switch_in_the_middle_of_a_transition_carries_on_from_where_it_was() {
    let mut state = Inertializer::new(0.1);
    let mut last = state.step(1.0 / FPS, &gesture(0.0));
    state.switch();
    for i in 1..8 {
        last = state.step(1.0 / FPS, &idle(i as f32 / FPS));
    }
    assert!(state.is_transitioning());
    state.switch();
    let next = state.step(1.0 / FPS, &gesture(8.0 / FPS));
    let [a, b] = [hands(&last), hands(&next)];
    // One frame on, the hands have moved no further than a frame's motion, though the source jumped back.
    let moved = (a[0] - b[0]).length().max((a[1] - b[1]).length());
    let jump = (hands(&gesture(8.0 / FPS))[0] - a[0]).length();
    println!("second switch: hands moved {:.1} mm in the frame, the source jumped {:.1} mm", moved * 1e3, jump * 1e3);
    assert!(moved < 0.2 * jump, "{moved} vs {jump}");
}
