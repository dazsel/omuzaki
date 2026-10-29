//! The character's clip set (#2414): `clip.set`, `clip.delete` and `clip.list`, and [`find_clip`] that `act.clip` and
//! shorts play from.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

use omuzaki_anim::hands::HandRest;
use omuzaki_anim::poses::{ClipLibrary, PoseLibrary, find_clip};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::CharId;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

fn doc() -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira" } } }))
}

fn plan(name: &str, args: Value, doc: &MemDoc) -> Result<Plan, OpError> {
    let reg = find(name).expect("the op registers itself");
    let clock = FakeClock::new(Seconds(0.0));
    let actor = Actor::Agent { id: "test".into(), model: None };
    (reg.decode)(args).unwrap().plan(&OpCtx::new(doc, &actor, &clock))
}

fn run(name: &str, args: Value, doc: &mut MemDoc) -> Plan {
    let planned = plan(name, args, doc).unwrap();
    doc.apply(&planned.patch).unwrap();
    planned
}

fn refusal(result: Result<Plan, OpError>) -> OpError {
    result.expect_err("the op should refuse")
}

fn mira() -> CharId {
    CharId::new("mira").unwrap()
}

/// A clip from rest to a hand on the chest and back, with an inline pose in the middle.
fn hello() -> Value {
    json!({
        "character": "mira", "id": "hello", "summary": "A hand on the chest, then a nod.", "length": 2.0,
        "keys": [
            { "t": 0.0, "pose": "wave-right" },
            { "t": 0.8, "pose": "hand-on-chest" },
            { "t": 2.0, "pose": { "summary": "a nod", "bones": { "head": [0.130_526_2, 0.0, 0.0, 0.991_444_9] } } }
        ]
    })
}

#[test]
fn a_clip_is_stored_materialised_and_plays_by_name_before_the_library() {
    let mut doc = doc();
    let planned = run("clip.set", hello(), &mut doc);
    assert_eq!(planned.output.unwrap()["created"], json!(true));
    let stored = &doc.root()["characters"]["mira"]["clips"]["hello"];
    assert_eq!(stored["keys"][1]["from"], json!("hand-on-chest"));
    assert_eq!(stored["keys"][2]["from"], json!("inline"));
    let chest = PoseLibrary::builtin().unwrap().get("hand-on-chest").unwrap();
    let bones = stored["keys"][1]["pose"]["bones"].as_object().unwrap();
    assert_eq!(bones.len(), chest.bones.len(), "the pose is copied in, not referenced");
    assert_eq!(stored["keys"][1]["pose"]["reach"]["right"]["bone"], json!("upperChest"), "with its reach");

    let clip = find_clip(&doc, Some(&mira()), "hello").unwrap();
    assert_eq!((clip.length(), clip.keys().len()), (2.0, 3));
    let at_chest = clip.sample_posture(0.8);
    assert_eq!(at_chest.reach.right.unwrap().bone, HumanoidBone::UpperChest);
    assert_eq!(
        clip.sample(0.8, &HandRest::nominal()),
        chest.posture(omuzaki_anim::hands::HandLibrary::builtin().unwrap()).unwrap().to_humanoid(&HandRest::nominal())
    );

    // A character's clip of a library name plays before the library's; without the character the library's plays.
    let mut own_wave = hello();
    own_wave["id"] = json!("wave");
    run("clip.set", own_wave, &mut doc);
    assert_eq!(find_clip(&doc, Some(&mira()), "wave").unwrap().length(), 2.0);
    let library_wave = ClipLibrary::builtin().unwrap().get("wave").unwrap();
    assert_eq!(*find_clip(&doc, None, "wave").unwrap(), *library_wave);
}

#[test]
fn setting_replacing_and_deleting_undo_exactly() {
    let mut doc = doc();
    let empty = doc.root().clone();
    let first = run("clip.set", hello(), &mut doc);
    let one = doc.root().clone();
    let mut longer = hello();
    longer["length"] = json!(3.0);
    let replaced = run("clip.set", longer, &mut doc);
    assert_eq!(replaced.output.unwrap()["created"], json!(false));
    doc.apply(&replaced.patch.inverse()).unwrap();
    assert_eq!(doc.root(), &one);
    let deleted = run("clip.delete", json!({ "character": "mira", "id": "hello" }), &mut doc);
    assert!(doc.root()["characters"]["mira"]["clips"].as_object().unwrap().is_empty());
    doc.apply(&deleted.patch.inverse()).unwrap();
    assert_eq!(doc.root(), &one);
    doc.apply(&first.patch.inverse()).unwrap();
    assert_eq!(doc.root(), &empty);
}

#[test]
fn a_bad_clip_is_refused_naming_the_key_and_what_to_do() {
    let doc = doc();
    let mut late = hello();
    late["keys"][2]["t"] = json!(5.0);
    let e = refusal(plan("clip.set", late, &doc));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("keys")));
    assert!(e.message.contains("outside the clip") && e.hint.unwrap().contains("length"), "{}", e.message);

    let mut typo = hello();
    typo["keys"][1]["pose"] = json!("hand-on-chets");
    let e = refusal(plan("clip.set", typo, &doc));
    assert_eq!(e.field.as_deref(), Some("keys/1/pose"));
    assert!(e.hint.unwrap().contains("hand-on-chest"));

    let mut looped = hello();
    looped["looped"] = json!(true);
    looped["easing"] = json!("linear");
    looped["keys"][2]["t"] = json!(1.5);
    let e = refusal(plan("clip.set", looped, &doc));
    assert_eq!(e.field.as_deref(), Some("easing"));

    let e = refusal(plan("clip.delete", json!({ "character": "mira", "id": "nope" }), &doc));
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = find_clip(&doc, Some(&mira()), "wavee").unwrap_err();
    assert!(e.hint.unwrap().contains("did you mean 'wave'"));
}

#[test]
fn listing_shows_the_library_and_the_characters_own() {
    let mut doc = doc();
    run("clip.set", hello(), &mut doc);
    let library = plan("clip.list", json!({}), &doc).unwrap().output.unwrap();
    let ids: Vec<&str> = library["library"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"hand-on-chest") && ids.contains(&"cheer"));
    assert!(library.get("character").is_none());
    let own = plan("clip.list", json!({ "character": "mira" }), &doc).unwrap().output.unwrap();
    assert_eq!(own["character"][0]["id"], json!("hello"));
    assert_eq!(own["character"][0]["keys"], json!(3));
}
