//! The living body (`omuzaki_anim::body`): a face-only performance gets a body that never freezes, gestures whose
//! strokes land on the stressed syllables, a head that stays exactly where tracking put it, and bones that go back to
//! whoever else drives them. Every number here is on a synthetic talking take (`body::measure::Take`), so it is the
//! same on every machine.

use omuzaki_anim::BusAnimator;
use omuzaki_anim::body::LivingBody;
use omuzaki_anim::body::arm::{ArmPose, Side};
use omuzaki_anim::body::gesture::GestureLibrary;
use omuzaki_anim::body::measure::{Take, measure, measure_face_only};
use omuzaki_anim::body::ops::standing_base;
use omuzaki_anim_api::{Animator, BodyStyle, BoneMask, HumanoidBone as B, HumanoidPose, Personality, Tick};
use omuzaki_core::glam::{Quat, Vec3};

const PRESETS: [Personality; 5] =
    [Personality::Natural, Personality::Energetic, Personality::Calm, Personality::Shy, Personality::Sleepy];

/// The world rotation of `bone` through its humanoid parents.
fn world(pose: &HumanoidPose, bone: B) -> Quat {
    let mut chain = vec![bone];
    while let Some(p) = chain.last().and_then(|b| b.parent()) {
        chain.push(p);
    }
    chain.iter().rev().fold(Quat::IDENTITY, |q, b| q * pose.get(*b))
}

/// The angle between two rotations, degrees, precise near zero (where `acos` of a dot product is not).
fn small_angle(a: Quat, b: Quat) -> f32 {
    let d = a.inverse() * b;
    (2.0 * d.xyz().length().min(1.0).asin()).to_degrees()
}

#[test]
fn a_style_of_zeros_is_the_bus_animator_bit_for_bit() {
    let take = Take::synthesize(20.0, 0.7, 3);
    let base = standing_base();
    let mut bus = BusAnimator::default();
    let mut off = LivingBody::new(BodyStyle::OFF, 3);
    let mut a = Vec::new();
    take.play(&mut bus, &base, |_, p| a.push(*p));
    let mut b = Vec::new();
    take.play(&mut off, &base, |_, p| b.push(*p));
    assert_eq!(a, b);
}

#[test]
fn the_head_stays_exactly_where_tracking_put_it() {
    let take = Take::synthesize(30.0, 0.8, 11);
    let base = standing_base();
    let mut heads = Vec::new();
    take.play(&mut BusAnimator::default(), &base, |_, p| heads.push(world(p, B::Head)));
    let mut worst = 0.0_f32;
    let mut torso_moved = 0.0_f32;
    take.play(&mut LivingBody::new(BodyStyle::preset(Personality::Energetic), 11), &base, |i, p| {
        worst = worst.max(small_angle(world(p, B::Head), heads[i]));
        torso_moved = torso_moved.max(world(p, B::UpperChest).angle_between(world(&base, B::UpperChest)).to_degrees());
    });
    assert!(torso_moved > 2.0, "the torso moves under the head: {torso_moved}");
    assert!(worst < 0.01, "the head is off by {worst} degrees");
}

#[test]
fn a_talking_body_never_freezes_where_the_face_only_body_mostly_does() {
    let take = Take::synthesize(60.0, 0.7, 7);
    let base = standing_base();
    let face_only = measure_face_only(&take, &base);
    eprintln!("face only: {face_only:?}");
    assert!(face_only.frozen_share > 0.5, "{face_only:?}");
    for preset in PRESETS {
        let living = measure(&BodyStyle::preset(preset), &take, &base, 7);
        eprintln!("{preset:?}: {living:?}");
        assert_eq!(living.frozen_windows, 0, "{preset:?}: {living:?}");
        assert!(living.peak_deg_s < 900.0, "{preset:?} pops: {living:?}");
        assert!(living.arms_deg_s > 3.0 * face_only.arms_deg_s.max(0.5), "{preset:?}: {living:?}");
    }
}

#[test]
fn strokes_land_on_the_stressed_syllables() {
    for seed in [1, 2, 3] {
        let take = Take::synthesize(90.0, 0.8, seed);
        let living = measure(&BodyStyle::default(), &take, &standing_base(), seed);
        eprintln!("seed {seed}: {living:?}");
        assert!(living.strokes_per_min > 8.0, "{living:?}");
        assert!(living.strokes_within_100ms >= 0.9, "{living:?}");
        assert!(living.stroke_offset_p50_ms <= 60.0, "{living:?}");
        assert!(living.strokes_predicted >= 0.5, "most strokes are timed from the rhythm: {living:?}");
    }
}

#[test]
fn presets_differ_the_way_their_names_say() {
    let take = Take::synthesize(120.0, 0.75, 5);
    let base = standing_base();
    let m = |p| measure(&BodyStyle::preset(p), &take, &base, 5);
    let (energetic, natural, calm, shy) =
        (m(Personality::Energetic), m(Personality::Natural), m(Personality::Calm), m(Personality::Shy));
    assert!(energetic.phrases_per_min > natural.phrases_per_min, "{energetic:?} vs {natural:?}");
    assert!(natural.phrases_per_min > calm.phrases_per_min, "{natural:?} vs {calm:?}");
    assert!(calm.phrases_per_min > shy.phrases_per_min, "{calm:?} vs {shy:?}");
    assert!(energetic.arms_deg_s > calm.arms_deg_s);
}

#[test]
fn the_same_seed_replays_and_another_seed_does_not() {
    let take = Take::synthesize(20.0, 0.7, 9);
    let base = standing_base();
    let style = BodyStyle::default();
    assert_eq!(measure(&style, &take, &base, 4).checksum, measure(&style, &take, &base, 4).checksum);
    assert_ne!(measure(&style, &take, &base, 4).checksum, measure(&style, &take, &base, 5).checksum);
}

#[test]
fn tracked_bones_go_back_to_their_source() {
    let take = Take::synthesize(20.0, 1.0, 2);
    let base = standing_base();
    let mut body = LivingBody::new(BodyStyle::preset(Personality::Energetic), 2);
    let arm = [B::RightShoulder, B::RightUpperArm, B::RightLowerArm, B::RightHand, B::RightIndexProximal];
    let mut worst_free = 0.0_f32;
    let mut worst_held = 0.0_f32;
    for (i, frame) in take.frames.iter().enumerate() {
        if i == 600 {
            body.set_tracked(BoneMask::of(&[B::RightUpperArm, B::RightIndexProximal]));
        }
        body.hear(&frame.cues);
        let mut pose = base;
        body.animate(Tick::at_rate(i as u64, 60.0), &frame.bus, &[], &mut pose);
        let off = arm.iter().map(|b| pose.get(*b).angle_between(base.get(*b)).to_degrees()).fold(0.0, f32::max);
        if (300..600).contains(&i) {
            worst_free = worst_free.max(off);
        } else if i > 660 {
            worst_held = worst_held.max(off);
        }
    }
    assert!(worst_free > 10.0, "the free arm gestures: {worst_free}");
    assert!(worst_held < 1e-3, "a held arm is the source's: {worst_held}");
}

#[test]
fn every_gesture_keeps_its_hands_in_front_of_the_body() {
    // Unit-length bones from the shoulder; the body's middle is about 0.6 upper-arm lengths in from the shoulder.
    let wrist = |pose: &ArmPose| {
        let [s, u, l, _] = pose.rotations(Side::Left);
        let elbow = (s * u) * Vec3::X;
        elbow + (s * u * l) * Vec3::X
    };
    let library = GestureLibrary::builtin().expect("the gestures load");
    for g in library.iter() {
        for (which, key) in [("prep", &g.prep), ("stroke", &g.stroke)] {
            for arm in [Some(key.lead), key.other].into_iter().flatten() {
                let w = wrist(&arm);
                assert!(w.z > -0.3, "{} {which}: the hand goes behind the body: {w}", g.id);
                assert!(w.x > -1.0, "{} {which}: the hand crosses far past the middle: {w}", g.id);
                assert!(w.y < 1.0, "{} {which}: the hand goes over the head: {w}", g.id);
            }
        }
    }
}
