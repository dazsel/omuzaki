//! `motion.import`: a motion-capture take stored as a character's clip, played back exactly at its keys, through the
//! registry as an agent calls it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use omuzaki_anim::motion::asf::Skeleton;
use omuzaki_anim::poses::find_clip;
use omuzaki_anim_api::HumanoidBone as B;
use omuzaki_char_api::CharId;
use omuzaki_core::glam::Quat;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

fn doc() -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira" } } }))
}

fn plan(args: Value, doc: &MemDoc) -> Result<Plan, OpError> {
    let reg = find("motion.import").expect("the op registers itself");
    let clock = FakeClock::new(Seconds(0.0));
    let actor = Actor::Agent { id: "test".into(), model: None };
    (reg.decode)(args).unwrap().plan(&OpCtx::new(doc, &actor, &clock))
}

/// A skeleton with only a spine and a left arm: every other humanoid bone is missing and stays straight.
const ARM: &str = "\
:units
  length 0.45
:bonedata
  begin
    id 1
    name lowerback
    direction 0 1 0
    length 2
    axis 0 0 0 XYZ
    dof rx ry rz
  end
  begin
    id 2
    name lhumerus
    direction 1 0 0
    length 5
    axis 0 0 -90 XYZ
    dof rx ry rz
  end
  begin
    id 3
    name lradius
    direction 1 0 0
    length 4
    axis 0 0 -90 XYZ
    dof rx
  end
:hierarchy
  begin
    root lowerback
    lowerback lhumerus
    lhumerus lradius
  end
";

/// Five frames at 4 fps: the elbow folds to 80 degrees while the spine turns to 20.
fn amc() -> String {
    let mut s = ":FULLY-SPECIFIED\n:DEGREES\n".to_owned();
    for k in 0..5 {
        let f = k as f32 / 4.0;
        s += &format!("{}\nroot 0 30 0 0 0 0\nlowerback 0 {} 0\nlhumerus 0 0 0\nlradius {}\n", k + 1, 20.0 * f, 80.0 * f);
    }
    s
}

fn import(extra: Value) -> Value {
    let mut args = json!({
        "character": "mira", "id": "fold", "skeleton": {"text": ARM}, "motion": {"text": amc()}, "fps": 4.0,
        "key_fps": 8.0
    });
    args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    args
}

#[test]
fn every_example_imports_into_a_character() {
    let reg = find("motion.import").unwrap();
    let examples = reg.examples();
    assert!(!examples.is_empty());
    let mut doc = MemDoc::from_value(json!({ "characters": { "kit": { "name": "Kit" } } }));
    let planned = plan(examples[0].clone(), &doc).unwrap();
    doc.apply(&planned.patch).unwrap();
    let clip = find_clip(&doc, Some(&CharId::new("kit").unwrap()), "turn").unwrap();
    let turned = clip.sample_posture(clip.length()).body.get(B::Hips);
    assert!((turned.angle_between(Quat::from_rotation_y(30f32.to_radians()))).to_degrees() < 1e-3, "{turned}");
}

#[test]
fn the_clip_is_the_take_exactly_at_every_key_and_lands_in_the_set() {
    let mut doc = doc();
    let planned = plan(import(json!({})), &doc).unwrap();
    let out = planned.output.clone().unwrap();
    assert_eq!((out["created"].clone(), out["keys"].clone()), (json!(true), json!(9)), "{out}");
    assert_eq!(out["plays_with"]["op"], json!("act.clip"));
    doc.apply(&planned.patch).unwrap();
    let stored = &doc.root()["characters"]["mira"]["clips"]["fold"];
    assert_eq!(stored["easing"], json!("linear"));

    let take = Skeleton::parse(ARM).unwrap().motion(&amc(), 4.0, 0.9).unwrap().in_place();
    let clip = find_clip(&doc, Some(&CharId::new("mira").unwrap()), "fold").unwrap();
    assert!((clip.length() - 1.0).abs() < 1e-6);
    for k in 0..=8 {
        let t = k as f32 / 8.0;
        let ours = clip.sample_posture(t).body;
        let theirs = take.sample(t);
        // Compared by component: `angle_between`'s acos is noisy in f32 by hundredths of a degree near zero.
        for bone in B::ALL {
            let (a, b) = (ours.get(bone), theirs.get(bone));
            assert!(a.abs_diff_eq(b, 1e-6) || a.abs_diff_eq(-b, 1e-6), "{bone:?} at {t}: {a} against {b}");
        }
        assert!((ours.hips_offset - theirs.hips_offset).length() < 1e-6);
    }
    let end = clip.sample_posture(1.0).body;
    assert!((end.get(B::LeftLowerArm).angle_between(Quat::IDENTITY).to_degrees() - 80.0).abs() < 1e-2);
    // Bones the skeleton lacks stay straight on their parents.
    for bone in [B::Head, B::RightUpperArm, B::LeftUpperLeg, B::LeftHand] {
        assert!(end.get(bone).angle_between(Quat::IDENTITY) < 1e-5, "{bone:?}");
    }

    // A second import of the same name replaces it, and says so.
    let again = plan(import(json!({"from_s": 0.5, "hands": "rest"})), &doc).unwrap();
    assert_eq!(again.output.unwrap()["created"], json!(false));
}

#[test]
fn bad_arguments_are_refused_on_their_fields() {
    let doc = doc();
    let field = |extra: Value| plan(import(extra), &doc).expect_err("refused").field;
    assert_eq!(field(json!({"from_s": 5.0})).as_deref(), Some("from_s"));
    assert_eq!(field(json!({"key_fps": 0.0})).as_deref(), Some("key_fps"));
    assert_eq!(field(json!({"length_s": 500.0})).as_deref(), Some("length_s"));
    assert_eq!(field(json!({"motion": {"text": "1\nnobone 1\n"}})).as_deref(), Some("motion"));
    assert_eq!(field(json!({"hands": "no-such-shape"})).map(|f| f.starts_with("keys/")), Some(true));
    // A path goes through the actor's file access: with no grants and no project, it is refused before any read.
    let denied = plan(import(json!({"skeleton": {"path": "/etc/passwd"}})), &doc).expect_err("refused");
    assert_eq!(denied.field.as_deref(), Some("skeleton/path"));
    assert_ne!(denied.code, ErrorCode::Internal);
    let missing = plan(import(json!({"character": "nobody"})), &doc).expect_err("refused");
    assert_eq!(missing.code, ErrorCode::NotFound);
}
