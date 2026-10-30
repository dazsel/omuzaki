//! Physical reactions on the living body (#2581): a blow or a fall knocks the body off its course and hands it back
//! exactly at the recovery time, the head included; the face's channels never change; the same impact replays; and a
//! reaction step stays inside its 0.3 ms budget.

use std::time::Instant;

use omuzaki_anim::body::LivingBody;
use omuzaki_anim::body::measure::{ReactionMotion, TAKE_FPS, Take, measure_reaction};
use omuzaki_anim::body::ops::standing_base;
use omuzaki_anim::body::react::Reaction;
use omuzaki_anim_api::{Animator, BodyPart, BodyStyle, Impact, Personality, Tick};

const FRONT: [f32; 3] = [0.0, 0.0, -1.0];

fn react(style: &BodyStyle, impact: &Impact) -> ReactionMotion {
    let take = Take::synthesize(6.0, 0.7, 5);
    let motion = measure_reaction(style, impact, &take, &standing_base(), 5, 1.0);
    eprintln!("{impact:?}: {motion:?}");
    motion
}

#[test]
fn a_bonk_rocks_the_head_and_hands_it_back_exactly_on_time() {
    for style in [BodyStyle::default(), BodyStyle::OFF, BodyStyle::preset(Personality::Shy)] {
        let bonk = Impact::hit(BodyPart::Face, FRONT, 1.0);
        let m = react(&style, &bonk);
        assert!((8.0..=30.0).contains(&m.head_deg), "{m:?}");
        assert!(m.chest_deg < m.head_deg * 0.5, "{m:?}");
        assert!(m.head_peak_s < 0.3, "the head goes at once: {m:?}");
        assert!(m.settled_s <= bonk.recover_s + 1e-3, "{m:?}");
        assert!(m.exact_after_recovery, "{m:?}");
        assert!(m.peak_deg_s < 1200.0, "{m:?}");
    }
}

#[test]
fn a_shove_moves_the_body_more_than_the_head_and_a_fall_takes_it_over() {
    let shove = react(&BodyStyle::default(), &Impact::hit(BodyPart::Chest, FRONT, 1.5));
    assert!(shove.chest_deg > 5.0 && shove.hips_deg > 0.5, "{shove:?}");
    let fall = Impact::fall(BodyPart::Chest, FRONT, 1.0);
    let m = react(&BodyStyle::default(), &fall);
    assert!(m.chest_deg > 25.0, "{m:?}");
    assert!(m.settled_s <= fall.recover_s + 1e-3 && m.exact_after_recovery, "{m:?}");
}

#[test]
fn the_same_impact_replays_and_the_face_is_left_alone() {
    let take = Take::synthesize(4.0, 0.7, 9);
    let base = standing_base();
    let run = || {
        let mut body = LivingBody::new(BodyStyle::default(), 9);
        let mut poses = Vec::new();
        take.play(&mut body, &base, |i, pose| {
            poses.push(*pose);
            let _ = i;
        });
        poses
    };
    let knocked = |seed| {
        let mut body = LivingBody::new(BodyStyle::default(), seed);
        let mut poses = Vec::new();
        for (i, frame) in take.frames.iter().enumerate() {
            if i == 30 {
                body.react(&Impact::fall(BodyPart::RightShoulder, [1.0, 0.0, 0.0], 1.2));
            }
            body.hear(&frame.cues);
            let mut pose = base;
            let bus = frame.bus;
            body.animate(Tick::at_rate(i as u64, TAKE_FPS), &bus, &[], &mut pose);
            assert_eq!(bus, frame.bus, "the animator only reads the face");
            poses.push(pose);
        }
        poses
    };
    assert_eq!(knocked(9), knocked(9));
    assert_ne!(knocked(9), run());
}

#[test]
fn a_reaction_step_stays_inside_its_budget() {
    let mut reaction = Reaction::default();
    let mut pose = standing_base();
    let steps = 600;
    let start = Instant::now();
    for i in 0..steps {
        if i % 180 == 0 {
            reaction.hit(&Impact::fall(BodyPart::Stomach, [0.3, -0.2, -1.0], 2.0));
        }
        reaction.step(1.0 / 60.0);
        reaction.apply(&mut pose);
    }
    let per_step_ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(steps);
    eprintln!("reaction step: {per_step_ms:.4} ms");
    // The spec's ceiling for a character; it holds unoptimised on CI's runners.
    assert!(per_step_ms < 0.3, "{per_step_ms} ms");
}
