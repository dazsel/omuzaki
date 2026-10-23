//! A clip as an `IoAnimation` (`docs/design/vrma-export.md` V4, V7, V8): the rest tree, sampling, continuity,
//! reduction to the tolerance, the hips, the gaze and the limits.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_anim_api::{BoneDef, HumanoidBone, HumanoidPose, Skeleton, Transform};
use omuzaki_core::glam::{DQuat, EulerRot, Quat, Vec3};
use omuzaki_io_api::{
    ExportCtx, ExportOptions, HumanBoneName, IoAnimation, Keys, Track, TrackTarget, run_export_animation,
};
use omuzaki_io_ops::convert::animation::{
    BoneSelection, ClipExportOptions, ClipSampler, ConvertError, clip_to_animation, rest_humanoid,
};
use omuzaki_store_api::{Commercial, Provenance, Source};

/// A humanoid of every bone but `upperChest`, each bone 10 cm above or beside its parent, the hips 0.9 m up.
fn skeleton() -> Skeleton {
    let bones: Vec<HumanoidBone> = HumanoidBone::ALL.into_iter().filter(|b| *b != HumanoidBone::UpperChest).collect();
    Skeleton::humanoid(&bones, |b| {
        let translation = match b {
            HumanoidBone::Hips => Vec3::new(0.0, 0.9, 0.0),
            b if b.name().starts_with("left") => Vec3::new(0.1, 0.0, 0.0),
            b if b.name().starts_with("right") => Vec3::new(-0.1, 0.0, 0.0),
            _ => Vec3::new(0.0, 0.1, 0.0),
        };
        Transform { translation, rotation: Quat::IDENTITY }
    })
    .unwrap()
}

fn record() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: Some("Omuzaki contributors".into()),
        licence: "CC0-1.0".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

/// A clip from a function of time.
struct Clip<F: Fn(f32) -> HumanoidPose> {
    duration: f32,
    looped: bool,
    pose: F,
}

impl<F: Fn(f32) -> HumanoidPose> ClipSampler for Clip<F> {
    fn duration(&self) -> f32 {
        self.duration
    }
    fn looped(&self) -> bool {
        self.looped
    }
    fn sample(&self, t: f32) -> HumanoidPose {
        (self.pose)(t)
    }
}

/// A wave: the right upper arm swings on a sine for 2 s, the forearm holds still at 0.4 rad, the hips bob.
fn wave(t: f32) -> HumanoidPose {
    let mut p = HumanoidPose::REST;
    p.set(HumanoidBone::RightUpperArm, Quat::from_rotation_z(-1.2 + 0.4 * (t * std::f32::consts::TAU).sin()));
    p.set(HumanoidBone::RightLowerArm, Quat::from_rotation_y(0.4));
    p.hips_offset = Vec3::new(0.0, -0.01 * (t * std::f32::consts::PI).sin(), 0.0);
    p
}

fn export(clip: &dyn ClipSampler, opts: &ClipExportOptions) -> (IoAnimation, omuzaki_io_api::Report) {
    clip_to_animation(clip, &skeleton(), "wave", record(), opts).unwrap()
}

fn rotation(a: &IoAnimation, bone: &str) -> Option<Keys<Quat>> {
    let b = HumanBoneName::new(bone).unwrap();
    a.tracks.iter().find_map(|t| match t {
        Track::Rotation { bone, keys, .. } if *bone == b => Some(keys.clone()),
        _ => None,
    })
}

fn dquat(q: Quat) -> DQuat {
    DQuat::from_array(q.to_array().map(f64::from))
}

/// Linear playback of `keys` at `t`, slerped like a player, in `f64` so the check measures the keys and not the
/// check's own rounding (glam's `f32` slerp falls back to a lerp for close keys).
fn play(keys: &Keys<Quat>, t: f32) -> DQuat {
    let i = keys.times.partition_point(|k| *k <= t);
    if i == 0 {
        return dquat(keys.values[0]);
    }
    if i >= keys.times.len() {
        return dquat(*keys.values.last().unwrap());
    }
    let u = f64::from(t - keys.times[i - 1]) / f64::from(keys.times[i] - keys.times[i - 1]);
    let (from, mut to) = (dquat(keys.values[i - 1]), dquat(keys.values[i]));
    if from.dot(to) < 0.0 {
        to = -to;
    }
    let theta = from.dot(to).clamp(-1.0, 1.0).acos();
    if theta < 1e-12 {
        return from;
    }
    (from * (((1.0 - u) * theta).sin() / theta.sin()) + to * ((u * theta).sin() / theta.sin())).normalize()
}

/// The angle between two rotations, precise for small angles (`atan2` of the vector part, not `acos`).
fn angle(a: DQuat, b: DQuat) -> f64 {
    let d = a.conjugate() * b;
    2.0 * (d.x * d.x + d.y * d.y + d.z * d.z).sqrt().atan2(d.w.abs())
}

#[test]
fn the_rest_tree_is_the_skeletons_t_pose_relative_to_present_ancestors() {
    let rest = rest_humanoid(&skeleton()).unwrap();
    let get = |b: &str| rest.get(HumanBoneName::new(b).unwrap()).map(|r| r.translation);
    let near = |b: &str, v: Vec3| get(b).is_some_and(|t| (t - v).length() < 1e-6);
    assert!(near("hips", Vec3::new(0.0, 0.9, 0.0)), "{:?}", get("hips"));
    assert!(near("chest", Vec3::new(0.0, 0.1, 0.0)), "{:?}", get("chest"));
    assert!(near("neck", Vec3::new(0.0, 0.1, 0.0)), "under chest, upperChest is missing: {:?}", get("neck"));
    assert!(get("upperChest").is_none() && get("leftEye").is_none() && get("rightEye").is_none());
    assert_eq!(rest.bones.len(), 52, "55 bones but the eyes and upperChest");
    let order: Vec<HumanBoneName> = rest.bones.iter().map(|b| b.bone).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "table order");
    let generic = Skeleton::generic(vec![BoneDef {
        name: "root".into(),
        parent: None,
        rest: Transform::IDENTITY,
        humanoid: None,
    }])
    .unwrap();
    assert!(rest_humanoid(&generic).is_err());
    let clip = Clip { duration: 1.0, looped: false, pose: wave };
    let err = clip_to_animation(&clip, &generic, "x", record(), &ClipExportOptions::default()).unwrap_err();
    assert_eq!((err.clone(), err.rule()), (ConvertError::NotHumanoid, "anim_export.not_humanoid"));
}

#[test]
fn a_clip_plays_back_within_a_hundred_thousandth_of_a_radian_at_every_sample() {
    let clip = Clip { duration: 2.0, looped: true, pose: wave };
    let (a, report) = export(&clip, &ClipExportOptions::default());
    assert!(a.check().findings.is_empty(), "{:?}", a.check());
    assert!(a.looped);
    let targets: Vec<TrackTarget> = a.canonical_tracks().iter().map(|t| t.target()).collect();
    let bone = |b: &str| TrackTarget::Bone(HumanBoneName::new(b).unwrap());
    assert_eq!(targets, [TrackTarget::Hips, bone("rightUpperArm"), bone("rightLowerArm")], "only what moves");
    let arm = rotation(&a, "rightUpperArm").unwrap();
    for k in 0..=60 {
        #[allow(clippy::cast_precision_loss)]
        let t = k as f32 / 30.0;
        let exact = wave(t).get(HumanoidBone::RightUpperArm);
        let error = angle(play(&arm, t), dquat(exact));
        assert!(error < 1e-5, "{t}: {error}");
    }
    assert_eq!((arm.times[0], *arm.times.last().unwrap()), (0.0, 2.0));
    assert!(arm.values.windows(2).all(|w| w[0].dot(w[1]) >= 0.0), "continuous");
    let forearm = rotation(&a, "rightLowerArm").unwrap();
    assert_eq!(forearm.times, [0.0, 2.0], "a held bone is two keys");
    assert!(arm.times.len() < 61, "a sine is not reducible to a line, but the keys never grow past the samples");
    let Some(Track::Hips(hips)) = a.tracks.iter().find(|t| matches!(t, Track::Hips(_))) else { panic!("hips") };
    assert!((hips.values[0] - Vec3::new(0.0, 0.9, 0.0)).length() < 1e-6, "rest hips plus the offset");
    assert!(report.has_rule("anim_export.sampling_error"));
    // And it is a well-formed animation for the exporter.
    let out = run_export_animation(&omuzaki_io_mock::MockVrmaExporter, &a, &ExportCtx::new(&ExportOptions::default()));
    assert!(out.is_ok(), "{out:?}");
}

#[test]
fn reduction_is_lossless_to_the_tolerance_and_deterministic() {
    // A ramp at constant speed is two keys; a hold then a ramp is three.
    let ramp = Clip {
        duration: 1.0,
        looped: false,
        pose: |t: f32| {
            let mut p = HumanoidPose::REST;
            p.set(HumanoidBone::Spine, Quat::from_rotation_x(0.5 * t));
            p.set(HumanoidBone::Neck, Quat::from_rotation_x(if t < 0.5 { 0.0 } else { t - 0.5 }));
            p
        },
    };
    let (a, _) = export(&ramp, &ClipExportOptions::default());
    assert_eq!(rotation(&a, "spine").unwrap().times, [0.0, 1.0]);
    assert_eq!(rotation(&a, "neck").unwrap().times, [0.0, 0.5, 1.0]);
    assert_eq!(export(&ramp, &ClipExportOptions::default()).0, a, "deterministic");
}

#[test]
fn all_bones_writes_every_rest_bone() {
    let clip = Clip { duration: 1.0, looped: false, pose: wave };
    let (a, _) = export(&clip, &ClipExportOptions::default().with_bones(BoneSelection::All));
    let bones = a.tracks.iter().filter(|t| matches!(t, Track::Rotation { .. })).count();
    assert_eq!(bones, a.rest.bones.len());
}

#[test]
fn the_eyes_become_one_gaze_and_lost_bones_are_reported() {
    let clip = Clip {
        duration: 1.0,
        looped: false,
        pose: |t: f32| {
            let mut p = HumanoidPose::REST;
            let q = Quat::from_euler(EulerRot::YXZ, 0.3 * t, 0.2 * t, 0.0);
            p.set(HumanoidBone::LeftEye, q);
            p.set(HumanoidBone::RightEye, q * Quat::from_rotation_y(0.1 * t));
            p.set(HumanoidBone::UpperChest, Quat::from_rotation_x(0.2));
            p
        },
    };
    let (a, report) = export(&clip, &ClipExportOptions::default());
    for rule in ["anim_export.eyes_to_look_at", "anim_export.eye_asymmetry", "anim_export.bone_not_in_rest"] {
        assert!(report.has_rule(rule), "{rule}");
    }
    assert!(rotation(&a, "upperChest").is_none() && rotation(&a, "leftEye").is_none());
    let Some(Track::LookAt(gaze)) = a.tracks.iter().find(|t| matches!(t, Track::LookAt(_))) else { panic!("gaze") };
    let (yaw, pitch, _) = gaze.values.last().unwrap().to_euler(EulerRot::YXZ);
    assert!((yaw - 0.35).abs() < 0.01 && (pitch - 0.2).abs() < 0.01, "the mean direction: {yaw} {pitch}");
    assert!(a.check().findings.is_empty(), "{:?}", a.check());
}

#[test]
fn limits_are_checked_before_anything_is_sampled() {
    let never = Clip { duration: 2.0, looped: false, pose: |_t: f32| -> HumanoidPose { panic!("sampled") } };
    for fps in [0.0, 0.5, 121.0, f32::NAN] {
        let err = clip_to_animation(&never, &skeleton(), "x", record(), &ClipExportOptions::default().at_fps(fps));
        assert!(matches!(err, Err(ConvertError::Fps(_))), "{fps}");
    }
    for duration in [0.0, -1.0, 600.5, f32::INFINITY] {
        let long = Clip { duration, looped: false, pose: |_t: f32| -> HumanoidPose { panic!("sampled") } };
        let err = clip_to_animation(&long, &skeleton(), "x", record(), &ClipExportOptions::default());
        assert!(matches!(err, Err(ConvertError::Duration(_))), "{duration}");
    }
}

#[test]
fn short_clips_at_any_rate_have_strictly_increasing_times() {
    for (duration, fps) in [(0.1, 30.0), (1.0 / 30.0, 30.0), (1.0 / 120.0, 30.0), (0.1, 120.0), (1.0 / 120.0, 120.0)] {
        let clip = Clip { duration, looped: false, pose: wave };
        let (a, _) = export(&clip, &ClipExportOptions::default().at_fps(fps));
        assert!(!a.check().has_rule("anim.time_order"), "{duration} at {fps}");
        assert_eq!(a.duration(), duration);
    }
}
