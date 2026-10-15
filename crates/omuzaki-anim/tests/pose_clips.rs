//! Pose clips (RG-06): sampling, the loop seam, mirroring, contacts and the checks on load.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp, clippy::cast_precision_loss)]

use omuzaki_anim::hands::{HandLibrary, HandRest};
use omuzaki_anim::poses::{ClipError, ClipLibrary, Easing, PoseClip, PoseLibrary, Posture};
use omuzaki_anim_api::HumanoidPose;

fn clip(id: &str) -> PoseClip {
    ClipLibrary::builtin().unwrap().get(id).unwrap().clone()
}

/// Parses `text` against the built-in pose library (`wave-right`, `wave-left`) only.
fn parse(text: &str) -> Result<PoseClip, ClipError> {
    PoseClip::parse("test", text, &[PoseLibrary::builtin().unwrap()], HandLibrary::builtin().unwrap())
}

/// A one-shot clip file with `keys` spliced in.
fn file(head: &str, keys: &str) -> String {
    format!("id = \"c\"\nsummary = \"s\"\n{head}\n{keys}")
}

const OK_HEAD: &str = "length = 1.0\neasing = \"smooth\"";
const KEY_A: &str = "[[keys]]\nt = 0.0\npose = \"wave-right\"\n";
const KEY_B: &str = "[[keys]]\nt = 0.5\npose = \"wave-left\"\n";

fn assert_same(a: &HumanoidPose, b: &HumanoidPose, eps: f32) {
    assert!((a.hips_offset - b.hips_offset).length() <= eps);
    for (x, y) in a.rotations.iter().zip(&b.rotations) {
        assert!(x.dot(*y).abs() >= 1.0 - eps, "{x:?} vs {y:?}");
    }
}

#[test]
fn builtin_clips_load() {
    let lib = ClipLibrary::builtin().unwrap();
    let ids: Vec<_> = lib.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["wave", "wave-both", "yawn", "walk"]);
    assert_eq!(clip("yawn").length, 4.2);
    assert!(!clip("yawn").looped && clip("walk").looped && clip("wave").looped);
}

#[test]
fn key_times_return_the_keys_exactly() {
    let rest = HandRest::nominal();
    for c in ClipLibrary::builtin().unwrap().iter() {
        for k in &c.keys {
            assert_eq!(c.sample_posture(k.t), k.posture, "{} at {}", c.id, k.t);
            assert_eq!(c.sample(k.t, &rest), k.posture.to_humanoid(&rest));
        }
    }
}

#[test]
fn one_shot_holds_its_ends_and_a_loop_wraps() {
    let yawn = clip("yawn");
    assert_eq!(yawn.sample_posture(-3.0), yawn.keys[0].posture);
    assert_eq!(yawn.sample_posture(99.0), yawn.keys.last().unwrap().posture);
    assert_eq!(yawn.sample_posture(f32::NAN), yawn.keys[0].posture);
    let wave = clip("wave");
    assert_same(&wave.sample(0.1, &HandRest::nominal()), &wave.sample(0.7, &HandRest::nominal()), 1e-5);
    assert_same(&wave.sample(-0.5, &HandRest::nominal()), &wave.sample(0.1, &HandRest::nominal()), 1e-5);
}

#[test]
fn yawn_peak_is_held_between_its_two_keys() {
    let yawn = clip("yawn");
    let peak = yawn.keys[2].posture;
    assert_same(&yawn.sample_posture(2.4).body, &peak.body, 1e-6);
    assert_ne!(yawn.sample_posture(1.5), peak);
}

/// Speed of the hips and every bone (as the quaternion's change), by a centred difference.
fn speed(c: &PoseClip, t: f32) -> f32 {
    let step = 1e-3;
    let (before, after) = (c.sample_posture(t - step).body, c.sample_posture(t + step).body);
    let bones: f32 =
        before.rotations.iter().zip(&after.rotations).map(|(p, q)| (p.conjugate() * *q).xyz().length() * 2.0).sum();
    ((before.hips_offset - after.hips_offset).length() + bones) / (2.0 * step)
}

#[test]
fn the_loop_seam_is_c1() {
    for id in ["wave", "wave-both", "walk"] {
        let c = clip(id);
        // Both segments at the seam come to rest at it: a hundredth of a segment away they move at a small part of
        // their mid-segment speed.
        let (last, first) = (c.keys.last().unwrap().t, c.keys[1].t - c.keys[0].t);
        let before = c.length - last;
        let (mid_in, mid_out) = (speed(&c, last + before / 2.0), speed(&c, c.keys[0].t + first / 2.0));
        assert!(mid_in > 0.1 && mid_out > 0.1, "{id} moves between its keys");
        assert!(speed(&c, c.length - before * 0.01) < mid_in * 0.1, "{id} into the seam");
        assert!(speed(&c, first * 0.01) < mid_out * 0.1, "{id} out of the seam");
    }
}

#[test]
fn a_looped_clip_must_stop_at_its_keys() {
    let looped = format!("{OK_HEAD}\nlooped = true\n").replace("smooth", "linear");
    assert!(matches!(parse(&file(&looped, KEY_A)), Err(ClipError::SeamNotSmooth { .. })));
    assert!(parse(&file(&format!("{OK_HEAD}\nlooped = true"), KEY_A)).is_ok());
    assert!(parse(&file(&OK_HEAD.replace("smooth", "linear"), KEY_A)).is_ok());
}

#[test]
fn the_walk_mirrored_and_shifted_by_half_a_cycle_is_the_walk() {
    let walk = clip("walk");
    let mirrored = walk.mirrored();
    for i in 0..=40 {
        let t = i as f32 * walk.length / 40.0 + 0.0137;
        let a = walk.sample_posture(t).body;
        let b = mirrored.sample_posture(t + walk.length / 2.0).body;
        assert_same(&a, &b, 1e-5);
    }
}

#[test]
fn mirroring_twice_is_exact() {
    for c in ClipLibrary::builtin().unwrap().iter() {
        assert_eq!(c.mirrored().mirrored(), *c);
    }
    assert_ne!(clip("wave").mirrored(), clip("wave"));
}

#[test]
fn sampling_is_deterministic() {
    let walk = clip("walk");
    let run = || (0..100).map(|i| walk.sample_posture(i as f32 * 0.037).body.checksum()).collect::<Vec<_>>();
    assert_eq!(run(), run());
    assert_eq!(walk.mirrored().sample_posture(0.3), walk.mirrored().sample_posture(0.3));
}

#[test]
fn contacts_are_read_back_from_the_toml_and_swap_when_mirrored() {
    let walk = clip("walk");
    assert_eq!(walk.keys[0].contacts, [Some(true), Some(true)]);
    assert_eq!(walk.contacts(0.3), [Some(true), Some(false)]);
    assert_eq!(walk.contacts(0.8), [Some(false), Some(true)]);
    assert_eq!(walk.contacts(1.3), walk.contacts(0.3));
    assert_eq!(walk.mirrored().contacts(0.3), [Some(false), Some(true)]);
    // A side left out is the planter's own detection.
    let c = parse(&file(OK_HEAD, &format!("{KEY_A}contacts = {{ right = false }}\n"))).unwrap();
    assert_eq!(c.keys[0].contacts, [None, Some(false)]);
    assert_eq!(clip("wave").contacts(0.2), [None, None]);
}

#[test]
fn a_mirrored_key_is_the_mirror_of_its_pose() {
    let c = parse(&file(OK_HEAD, &format!("{KEY_A}mirror = true\n"))).unwrap();
    let lib = PoseLibrary::builtin().unwrap();
    let plain = lib.get("wave-right").unwrap().posture(HandLibrary::builtin().unwrap()).unwrap();
    assert_eq!(c.keys[0].posture, Posture::mirrored(&plain));
}

#[test]
fn the_wave_poses_are_hands_up() {
    let rest = HandRest::nominal();
    assert_ne!(clip("wave").sample(0.0, &rest), HumanoidPose::REST);
    assert_ne!(clip("wave").sample(0.0, &rest), clip("wave-both").sample(0.0, &rest));
}

// --- Rejections: one test each. ---

#[test]
fn rejects_bad_toml() {
    assert!(matches!(parse("id = "), Err(ClipError::Parse { .. })));
    assert!(matches!(
        parse(&file(OK_HEAD, "[[keys]]\nt = 0.0\npose = \"wave-right\"\nextra = 1")),
        Err(ClipError::Parse { .. })
    ));
}

#[test]
fn rejects_a_bad_id() {
    let text = file(OK_HEAD, KEY_A).replace("id = \"c\"", "id = \"Not Valid\"");
    assert!(matches!(parse(&text), Err(ClipError::BadName { .. })));
}

#[test]
fn rejects_an_empty_summary() {
    let text = file(OK_HEAD, KEY_A).replace("summary = \"s\"", "summary = \" \"");
    assert!(matches!(parse(&text), Err(ClipError::EmptySummary(_))));
}

#[test]
fn rejects_a_bad_length() {
    for len in ["0.0", "-1.0", "nan", "inf"] {
        let head = OK_HEAD.replace("1.0", len);
        assert!(matches!(parse(&file(&head, KEY_A)), Err(ClipError::BadLength { .. })), "{len}");
    }
}

#[test]
fn rejects_no_keys() {
    assert!(matches!(parse(&file(OK_HEAD, "keys = []")), Err(ClipError::NoKeys(_))));
}

#[test]
fn rejects_key_times_that_are_not_finite_or_outside_the_clip() {
    for t in ["nan", "inf", "-0.1", "1.5"] {
        let key = KEY_A.replace("0.0", t);
        assert!(matches!(parse(&file(OK_HEAD, &key)), Err(ClipError::KeyOutside { .. })), "{t}");
    }
    // A key at the length is the end of a one-shot clip, but the seam of a looped one.
    let at_end = KEY_A.replace("0.0", "1.0");
    assert!(parse(&file(OK_HEAD, &at_end)).is_ok());
    assert!(matches!(parse(&file(&format!("{OK_HEAD}\nlooped = true"), &at_end)), Err(ClipError::KeyOutside { .. })));
}

#[test]
fn rejects_key_times_that_do_not_increase() {
    let same = format!("{KEY_A}{}", KEY_B.replace("0.5", "0.0"));
    assert!(matches!(parse(&file(OK_HEAD, &same)), Err(ClipError::NotIncreasing { index: 1, .. })));
    let back = format!("{KEY_B}{KEY_A}");
    assert!(matches!(parse(&file(OK_HEAD, &back)), Err(ClipError::NotIncreasing { index: 1, .. })));
}

#[test]
fn rejects_an_unknown_pose() {
    let key = KEY_A.replace("wave-right", "no-such-pose");
    assert!(matches!(parse(&file(OK_HEAD, &key)), Err(ClipError::UnknownPose { index: 0, .. })));
}

#[test]
fn rejects_mirroring_a_pose_that_is_not_mirrorable() {
    let signed = r#"id = "sign"
summary = "s"
mirrorable = false
"#;
    let poses = PoseLibrary::parse(&[("sign", signed)]).unwrap();
    let text = file(OK_HEAD, "[[keys]]\nt = 0.0\npose = \"sign\"\nmirror = true\n");
    let err = PoseClip::parse("t", &text, &[&poses], HandLibrary::builtin().unwrap());
    assert!(matches!(err, Err(ClipError::NotMirrorable { .. })));
    assert!(
        PoseClip::parse("t", &text.replace("mirror = true", ""), &[&poses], HandLibrary::builtin().unwrap()).is_ok()
    );
}

#[test]
fn rejects_an_unknown_easing() {
    let head = OK_HEAD.replace("smooth", "bouncy");
    assert!(matches!(parse(&file(&head, KEY_A)), Err(ClipError::UnknownEasing { .. })));
}

#[test]
fn rejects_a_pose_whose_hand_shape_is_missing() {
    let odd = "id = \"odd\"\nsummary = \"s\"\n[hands.left]\nshape = \"no-such-shape\"\n";
    let poses = PoseLibrary::parse(&[("odd", odd)]).unwrap();
    let text = file(OK_HEAD, "[[keys]]\nt = 0.0\npose = \"odd\"\n");
    let err = PoseClip::parse("t", &text, &[&poses], HandLibrary::builtin().unwrap());
    assert!(matches!(err, Err(ClipError::Pose(_))));
}

#[test]
fn rejects_two_clips_with_one_id() {
    let text = file(OK_HEAD, KEY_A);
    let err = ClipLibrary::parse(
        &[("a", &text), ("b", &text)],
        &[PoseLibrary::builtin().unwrap()],
        HandLibrary::builtin().unwrap(),
    );
    assert!(matches!(err, Err(ClipError::DuplicateId(_))));
}

#[test]
fn easing_is_exact_at_the_ends() {
    for e in [Easing::Linear, Easing::Smooth, Easing::Smoother] {
        assert_eq!((e.apply(0.0), e.apply(1.0)), (0.0, 1.0));
        assert_eq!(Easing::from_name(e.name()), Some(e));
    }
}
