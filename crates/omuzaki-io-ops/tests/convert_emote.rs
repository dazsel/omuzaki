//! An emote as an `IoAnimation` (`docs/design/vrma-export.md` V6, V9): a pose hold and an expression envelope.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_anim_api::{Ease, ExpressionId, HumanoidBone, HumanoidPose, Skeleton, Transform};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_face_api::ExpressionSet;
use omuzaki_io_api::{ExpressionName, IoAnimation, Keys, PresetExpression, Track};
use omuzaki_io_ops::convert::animation::{ClipExportOptions, EmoteMotionInput, emote_to_animation};
use omuzaki_io_ops::convert::channels::named_expression;
use omuzaki_store_api::{Commercial, Provenance, Source};

fn skeleton() -> Skeleton {
    Skeleton::humanoid(&HumanoidBone::ALL, |b| Transform {
        translation: if b == HumanoidBone::Hips { Vec3::new(0.0, 0.9, 0.0) } else { Vec3::new(0.0, 0.1, 0.0) },
        rotation: Quat::IDENTITY,
    })
    .unwrap()
}

fn record() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: None,
        licence: "CC0-1.0".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

fn emote(id: &str, pose: Option<&HumanoidPose>) -> (IoAnimation, omuzaki_io_api::Report) {
    let input = EmoteMotionInput::new("hi", ExpressionId::new(id).unwrap(), pose.copied());
    emote_to_animation(&input, &skeleton(), record(), &ClipExportOptions::default()).unwrap()
}

fn weights(a: &IoAnimation) -> Option<(ExpressionName, Keys<f32>)> {
    a.tracks.iter().find_map(|t| match t {
        Track::Expression { name, keys, .. } => Some((name.clone(), keys.clone())),
        _ => None,
    })
}

fn play(keys: &Keys<f32>, t: f32) -> f32 {
    let i = keys.times.partition_point(|k| *k <= t);
    if i == 0 || i >= keys.times.len() {
        return keys.values[i.min(keys.values.len() - 1)];
    }
    let u = (t - keys.times[i - 1]) / (keys.times[i] - keys.times[i - 1]);
    keys.values[i - 1] + (keys.values[i] - keys.values[i - 1]) * u
}

fn envelope(t: f32) -> f32 {
    if t <= 0.0 || t >= 2.0 {
        0.0
    } else if t < 0.2 {
        Ease::OutCubic.apply(t / 0.2)
    } else if t <= 1.8 {
        1.0
    } else {
        Ease::OutCubic.apply((2.0 - t) / 0.2)
    }
}

#[test]
fn happy_is_a_preset_weight_track_that_follows_the_envelope() {
    let (a, report) = emote("happy", None);
    assert!(a.check().findings.is_empty(), "{:?}", a.check());
    assert!(!report.has_rule("anim_export.expression_custom"));
    let (name, keys) = weights(&a).unwrap();
    assert_eq!(name, ExpressionName::Preset(PresetExpression::Happy));
    assert_eq!(a.duration(), 2.0, "0.2 + 1.6 + 0.2");
    for k in 0..=60 {
        #[allow(clippy::cast_precision_loss)]
        let t = k as f32 / 30.0;
        assert!((play(&keys, t) - envelope(t)).abs() < 1e-5, "{t}: {} vs {}", play(&keys, t), envelope(t));
    }
    assert!(a.tracks.iter().all(|t| matches!(t, Track::Expression { .. })), "no pose, no bones");
    assert!(!a.looped);
}

#[test]
fn other_ids_are_custom_and_look_presets_are_dropped() {
    let (a, report) = emote("smug", None);
    assert_eq!(weights(&a).unwrap().0.as_str(), "omuzaki_smug");
    assert!(report.has_rule("anim_export.expression_custom"));
    let (a, report) = emote("look-up", None);
    assert!(weights(&a).is_none());
    assert!(report.has_rule("anim_export.expression_reserved"));
}

#[test]
fn a_pose_is_eased_in_held_and_eased_out_on_the_bones_it_turns() {
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::RightUpperArm, Quat::from_rotation_z(-1.0));
    let (a, _) = emote("happy", Some(&pose));
    assert!(a.check().findings.is_empty(), "{:?}", a.check());
    let bones: Vec<&Track> = a.tracks.iter().filter(|t| matches!(t, Track::Rotation { .. })).collect();
    assert_eq!(bones.len(), 1, "only the turned bone");
    let Track::Rotation { keys, .. } = bones[0] else { unreachable!() };
    assert_eq!(keys.values[0], Quat::IDENTITY);
    assert_eq!(*keys.values.last().unwrap(), Quat::IDENTITY);
    let held = keys.times.iter().position(|t| (*t - 1.0).abs() < 1e-6).map(|i| keys.values[i]);
    let mid = held.unwrap_or_else(|| {
        let i = keys.times.partition_point(|k| *k <= 1.0);
        keys.values[i - 1].slerp(keys.values[i], (1.0 - keys.times[i - 1]) / (keys.times[i] - keys.times[i - 1]))
    });
    assert!(mid.angle_between(Quat::from_rotation_z(-1.0)) < 1e-4, "held at the pose");
    assert_eq!(emote("happy", Some(&pose)).0, a, "deterministic");
}

#[test]
fn a_vrma_names_every_starter_expression_as_the_vrm_export_does() {
    for id in ExpressionSet::starter().0.keys() {
        let (a, _) = emote(id.as_str(), None);
        let expected = named_expression(id);
        let reserved = matches!(
            expected,
            Some(ExpressionName::Preset(
                PresetExpression::LookUp
                    | PresetExpression::LookDown
                    | PresetExpression::LookLeft
                    | PresetExpression::LookRight
            ))
        );
        let got = weights(&a).map(|(name, _)| name);
        assert_eq!(got, if reserved { None } else { expected }, "{id}");
    }
}
