//! The character's pose set (RG-06, #249): `pose.set`, `pose.delete` and `pose.list_character`, held to the acceptance.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

use omuzaki_anim::poses::PoseLibrary;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

/// A document holding one character, `mira`, with the given pose set (or none).
fn doc_with(poses: Option<Value>) -> MemDoc {
    let mut character = json!({ "name": "Mira" });
    if let Some(poses) = poses {
        character["poses"] = poses;
    }
    MemDoc::from_value(json!({ "characters": { "mira": character } }))
}

/// Plans the op `name` with `args` against `doc`, as an agent would call it.
fn plan(name: &str, args: Value, doc: &MemDoc) -> Result<Plan, OpError> {
    let reg = find(name).expect("the op registers itself");
    let clock = FakeClock::new(Seconds(0.0));
    let actor = Actor::Agent { id: "test".into(), model: None };
    (reg.decode)(args).unwrap().plan(&OpCtx::new(doc, &actor, &clock))
}

/// Plans `name` and applies its patch, so the next call sees the change.
fn run(name: &str, args: Value, doc: &mut MemDoc) -> Plan {
    let planned = plan(name, args, doc).unwrap();
    doc.apply(&planned.patch).unwrap();
    planned
}

fn stored(doc: &MemDoc, name: &str) -> Value {
    doc.root()["characters"]["mira"]["poses"][name].clone()
}

fn refusal(result: Result<Plan, OpError>) -> OpError {
    match result {
        Ok(_) => panic!("the op should refuse"),
        Err(e) => e,
    }
}

/// A unit quaternion turning about Z by a right angle, in `[x, y, z, w]`.
const QUARTER_TURN_Z: [f32; 4] = [0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2];

#[test]
fn setting_from_the_library_materialises_the_rotations() {
    let mut doc = doc_with(None);
    run("pose.set", json!({ "character": "mira", "id": "wave", "from": "wave-right" }), &mut doc);
    let library_pose = PoseLibrary::builtin().unwrap().get("wave-right").unwrap().clone();
    let value = stored(&doc, "wave");
    assert_eq!(value["summary"], json!(library_pose.summary));
    assert_eq!(value["tags"], json!(library_pose.tags));
    let bones = value["bones"].as_object().unwrap();
    assert_eq!(bones.len(), library_pose.bones.len(), "every bone the library sets is stored");
    for (bone, q) in &library_pose.bones {
        assert_eq!(bones[bone.name()], json!(q.to_array()), "{bone} is copied, not referenced");
    }
}

#[test]
fn setting_then_undoing_is_exact_with_and_without_a_set() {
    let cases = [
        doc_with(None),
        doc_with(Some(json!({ "wave": { "summary": "old", "bones": { "rightUpperArm": QUARTER_TURN_Z } } }))),
    ];
    for start in cases {
        let before = start.root().clone();
        let mut doc = start;
        let planned = run("pose.set", json!({ "character": "mira", "id": "wave", "from": "wave-left" }), &mut doc);
        assert_ne!(doc.root(), &before, "the set changes");
        doc.apply(&planned.patch.inverse()).unwrap();
        assert_eq!(doc.root(), &before, "undo restores the document exactly");
    }
}

#[test]
fn mirror_stores_the_mirrored_pose() {
    let mut doc = doc_with(None);
    let output = run(
        "pose.set",
        json!({ "character": "mira", "id": "wave-mirror", "from": "wave-right", "mirror": true }),
        &mut doc,
    )
    .output
    .unwrap();
    assert_eq!(output["mirrored"], json!(true));
    let expected = PoseLibrary::builtin().unwrap().get("wave-right").unwrap().mirrored().unwrap();
    let value = stored(&doc, "wave-mirror");
    let bones = value["bones"].as_object().unwrap();
    assert_eq!(bones.len(), expected.bones.len());
    for (bone, q) in &expected.bones {
        assert_eq!(bones[bone.name()], json!(q.to_array()), "{bone} holds the mirrored rotation");
    }
    assert_ne!(value["bones"], stored_unmirrored(), "the mirror differs from the original");
}

/// The stored bones of `wave-right` as the set holds them without mirroring.
fn stored_unmirrored() -> Value {
    let mut doc = doc_with(None);
    run("pose.set", json!({ "character": "mira", "id": "plain", "from": "wave-right" }), &mut doc);
    stored(&doc, "plain")["bones"].clone()
}

#[test]
fn an_inline_pose_is_stored_as_given_and_listed() {
    let mut doc = doc_with(None);
    let args = json!({
        "character": "mira",
        "id": "nod",
        "pose": {
            "summary": "A small nod.",
            "tags": ["gesture"],
            "bones": { "neck": QUARTER_TURN_Z }
        }
    });
    run("pose.set", args, &mut doc);
    let value = stored(&doc, "nod");
    assert_eq!(value["bones"], json!({ "neck": QUARTER_TURN_Z }));
    assert_eq!(value["mirrorable"], json!(true), "mirrorable defaults to true and is stored");
    assert_eq!(value["hands"], json!({}), "no hands: neither side is written, so there is no null");

    let listed = plan("pose.list_character", json!({ "character": "mira" }), &doc).unwrap().output.unwrap();
    assert_eq!(
        listed["poses"],
        json!([{ "id": "nod", "summary": "A small nod.", "tags": ["gesture"], "mirrorable": true }])
    );
}

#[test]
fn an_inline_pose_with_a_bad_bone_is_refused_naming_it() {
    let doc = doc_with(None);
    let args = json!({
        "character": "mira",
        "id": "nod",
        "pose": { "summary": "x", "bones": { "leftUpperArmm": QUARTER_TURN_Z } }
    });
    let e = refusal(plan("pose.set", args, &doc));
    assert_eq!(e.field.as_deref(), Some("pose/bones/leftUpperArmm"), "{e:?}");
    assert!(e.hint.as_deref().unwrap_or_default().contains("did you mean `leftUpperArm`?"), "{e:?}");
}

#[test]
fn a_rotation_that_is_not_a_unit_quaternion_is_refused_naming_the_bone() {
    let doc = doc_with(None);
    let args = json!({
        "character": "mira",
        "id": "nod",
        "pose": { "summary": "x", "bones": { "leftUpperArm": [0.0, 0.0, 0.0, 2.0] } }
    });
    let e = refusal(plan("pose.set", args, &doc));
    assert_eq!(e.field.as_deref(), Some("pose/bones/leftUpperArm"), "{e:?}");
    assert!(e.message.contains("unit quaternion"), "{e:?}");
}

#[test]
fn a_rotation_that_is_not_finite_is_refused_naming_the_bone() {
    let doc = doc_with(None);
    let args = json!({
        "character": "mira",
        "id": "nod",
        "pose": { "summary": "x", "bones": { "leftUpperArm": [0.0, 0.0, 1e39, 1.0] } }
    });
    let e = refusal(plan("pose.set", args, &doc));
    assert_eq!(e.field.as_deref(), Some("pose/bones/leftUpperArm"), "{e:?}");
}

#[test]
fn from_and_pose_together_or_neither_is_refused_naming_the_fields() {
    let doc = doc_with(None);
    let both = json!({
        "character": "mira", "id": "wave", "from": "wave-right", "pose": { "summary": "x" }
    });
    let neither = json!({ "character": "mira", "id": "wave" });
    for args in [both, neither] {
        let e = refusal(plan("pose.set", args, &doc));
        assert_eq!(e.field.as_deref(), Some("from/pose"), "{e:?}");
        assert!(e.message.contains("`from`") && e.message.contains("`pose`"), "{e:?}");
    }
}

#[test]
fn an_unknown_library_pose_is_refused_with_a_did_you_mean() {
    let doc = doc_with(None);
    let e = refusal(plan("pose.set", json!({ "character": "mira", "id": "wave", "from": "wave-rigt" }), &doc));
    assert!(e.message.contains("wave-rigt"), "{e:?}");
    assert!(e.hint.as_deref().unwrap_or_default().contains("did you mean `wave-right`?"), "{e:?}");
}

#[test]
fn mirroring_a_pose_that_is_not_mirrorable_is_refused() {
    let doc = doc_with(None);
    let args = json!({
        "character": "mira", "id": "salute", "mirror": true,
        "pose": { "summary": "A salute with a handedness.", "mirrorable": false, "bones": {} }
    });
    let e = refusal(plan("pose.set", args, &doc));
    assert!(e.message.contains("not mirrorable"), "{e:?}");
}

#[test]
fn a_pose_name_that_is_not_a_name_is_refused() {
    let doc = doc_with(None);
    let args = json!({ "character": "mira", "id": "wave/left", "from": "wave-right" });
    let e = refusal(plan("pose.set", args, &doc));
    assert_eq!(e.field.as_deref(), Some("pose"), "{e:?}");
}

#[test]
fn a_missing_character_is_refused() {
    let doc = doc_with(None);
    let e = refusal(plan("pose.set", json!({ "character": "nobody", "id": "wave", "from": "wave-right" }), &doc));
    assert!(e.message.contains("nobody"), "{e:?}");
}

#[test]
fn deleting_removes_the_pose_and_undo_restores_it() {
    let mut doc = doc_with(None);
    run("pose.set", json!({ "character": "mira", "id": "wave", "from": "wave-right" }), &mut doc);
    run("pose.set", json!({ "character": "mira", "id": "keep", "from": "wave-left" }), &mut doc);
    let before = doc.root().clone();
    let planned = run("pose.delete", json!({ "character": "mira", "id": "wave" }), &mut doc);
    assert!(stored(&doc, "wave").is_null(), "the pose is gone");
    assert!(!stored(&doc, "keep").is_null(), "the other pose stays");
    doc.apply(&planned.patch.inverse()).unwrap();
    assert_eq!(doc.root(), &before, "undo restores the set exactly");
}

#[test]
fn deleting_an_unknown_id_gives_a_did_you_mean() {
    let mut doc = doc_with(None);
    run("pose.set", json!({ "character": "mira", "id": "wave", "from": "wave-right" }), &mut doc);
    run("pose.set", json!({ "character": "mira", "id": "nod", "pose": { "summary": "x" } }), &mut doc);
    let e = refusal(plan("pose.delete", json!({ "character": "mira", "id": "wvae" }), &doc));
    assert!(e.message.contains("wvae"), "{e:?}");
    assert!(e.hint.as_deref().unwrap_or_default().contains("did you mean `wave`?"), "{e:?}");
}

#[test]
fn deleting_from_a_character_with_no_set_says_so() {
    let doc = doc_with(None);
    let e = refusal(plan("pose.delete", json!({ "character": "mira", "id": "wave" }), &doc));
    assert!(e.hint.as_deref().unwrap_or_default().contains("pose.set"), "{e:?}");
}

#[test]
fn list_character_is_in_name_order_and_stable() {
    let mut doc = doc_with(None);
    for id in ["zeta", "alpha", "mid"] {
        run("pose.set", json!({ "character": "mira", "id": id, "from": "wave-right" }), &mut doc);
    }
    let first = plan("pose.list_character", json!({ "character": "mira" }), &doc).unwrap();
    let second = plan("pose.list_character", json!({ "character": "mira" }), &doc).unwrap();
    assert!(first.patch.is_empty(), "a query changes nothing");
    assert_eq!(first.output, second.output, "the same set lists the same way every time");
    let ids: Vec<_> = first.output.unwrap()["poses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["alpha", "mid", "zeta"]);
}

#[test]
fn a_character_with_no_set_lists_none() {
    let doc = doc_with(None);
    let out = plan("pose.list_character", json!({ "character": "mira" }), &doc).unwrap().output.unwrap();
    assert_eq!(out, json!({ "poses": [] }));
}
