//! Props through ops (#705, design D7, D8) and stage presets (#1273, D9): every edit is one patch with an exact inverse,
//! a one-param edit is a one-path patch, params are checked by the kind's own schema with every default stored, kinds
//! are found in the registry without a list, and a preset replaces the last preset's set and nothing else.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError, Plan};
use omuzaki_stage::ops::scene::create::CreateScene;
use omuzaki_stage::ops::scene::light::SetLightRig;
use omuzaki_stage::ops::scene::prop::{AddProp, PropKinds, RemoveProp, SetProp};
use omuzaki_stage::ops::scene::stage_preset::{SetStagePreset, StagePresets};
use omuzaki_stage::scene::default::scene_for;
use omuzaki_stage_api::{Anchor, Backdrop, GroundKind, PropId, SceneDef, SceneId};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn plan<T: Op + DeserializeOwned>(doc: &MemDoc, args: Value) -> Result<Plan, OpError> {
    omuzaki_stage::link();
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let op: T = serde_json::from_value(args).expect("the arguments read");
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

/// Applies the op, checks that undo restores the document exactly and redo lands the same state, and returns the patch
/// and the output.
#[allow(clippy::needless_pass_by_value, reason = "every call site passes a json! literal")]
fn edit<T: Op + DeserializeOwned>(doc: &mut MemDoc, args: Value) -> (Patch, Value) {
    let before = doc.root().clone();
    let plan = plan::<T>(doc, args.clone()).unwrap_or_else(|e| panic!("{} {args}: {e:?}", T::meta().name));
    doc.apply(&plan.patch).expect("applies");
    let after = doc.root().clone();
    doc.apply(&plan.patch.inverse()).expect("undoes");
    assert_eq!(doc.root(), &before, "{}: undo is exact", T::meta().name);
    doc.apply(&plan.patch).expect("redoes");
    assert_eq!(doc.root(), &after, "{}: redo is exact", T::meta().name);
    (plan.patch, plan.output.unwrap_or(Value::Null))
}

#[allow(clippy::needless_pass_by_value, reason = "every call site passes a json! literal")]
fn refused<T: Op + DeserializeOwned>(doc: &MemDoc, args: Value) -> OpError {
    match plan::<T>(doc, args.clone()) {
        Ok(_) => panic!("{} {args} should be refused", T::meta().name),
        Err(e) => e,
    }
}

fn paths_of(patch: &Patch) -> Vec<String> {
    patch.changes.iter().map(|c| c.path.to_string()).collect()
}

fn scene(doc: &MemDoc) -> SceneDef {
    scene_for(doc, Some(&SceneId::new("main").unwrap())).unwrap().1
}

fn id(name: &str) -> PropId {
    PropId::new(name).unwrap()
}

/// A character `kiri` and the default scene `main` casting it.
fn project() -> MemDoc {
    let mut doc = MemDoc::from_value(json!({
        "characters": { "kiri": { "name": "Kiri", "parts": {} } },
        "project": { "default_character": "kiri" },
    }));
    edit::<CreateScene>(&mut doc, json!({ "id": "main" }));
    doc
}

#[test]
fn a_prop_is_added_with_every_default_and_a_free_id() {
    let mut doc = project();
    let (patch, out) = edit::<AddProp>(&mut doc, json!({ "kind": "desk", "position": [0.0, 0.0, 0.45] }));
    assert_eq!(out["prop"], "desk");
    assert_eq!(paths_of(&patch), ["scenes/main/props"], "the first prop makes the table in one change");
    let (_, out) = edit::<AddProp>(&mut doc, json!({ "kind": "desk", "params": { "width": 1.8 } }));
    assert_eq!(out["prop"], "desk-2");
    let def = scene(&doc);
    let desk = &def.props[&id("desk-2")];
    assert_eq!(desk.params["width"], 1.8);
    assert!(desk.params.get("height").is_some(), "the kind's defaults are stored, so the document says what is drawn");
    assert_eq!(def.props[&id("desk")].position, [0.0, 0.0, 0.45]);
}

#[test]
fn a_one_param_edit_is_a_one_path_patch_and_moves_are_one_field() {
    let mut doc = project();
    edit::<AddProp>(&mut doc, json!({ "kind": "shelf" }));
    let (patch, _) = edit::<SetProp>(&mut doc, json!({ "prop": "shelf", "params": { "books": 0.3 } }));
    assert_eq!(paths_of(&patch), ["scenes/main/props/shelf/params/books"]);
    let (patch, _) = edit::<SetProp>(&mut doc, json!({ "prop": "shelf", "visible": false }));
    assert_eq!(paths_of(&patch), ["scenes/main/props/shelf/visible"]);
    let (patch, _) = edit::<SetProp>(&mut doc, json!({ "prop": "shelf", "position": [1.0, 0.0, -2.0] }));
    assert!(paths_of(&patch).iter().all(|p| p.starts_with("scenes/main/props/shelf/position")), "{patch:?}");
    let def = scene(&doc);
    assert_eq!(def.props[&id("shelf")].params["books"], 0.3);
    assert_eq!(def.props[&id("shelf")].params["shelves"], 5, "the other params keep their values");
}

#[test]
fn a_cup_goes_into_a_hand_and_out_of_it() {
    let mut doc = project();
    edit::<AddProp>(&mut doc, json!({ "kind": "cup", "anchor": "subject", "attach": "rightHand" }));
    assert_eq!(scene(&doc).props[&id("cup")].attach, Some(omuzaki_anim_api::HumanoidBone::RightHand));
    edit::<SetProp>(&mut doc, json!({ "prop": "cup", "attach": "leftHand" }));
    assert_eq!(scene(&doc).props[&id("cup")].attach, Some(omuzaki_anim_api::HumanoidBone::LeftHand));
    edit::<SetProp>(&mut doc, json!({ "prop": "cup", "attach": "off", "anchor": "stage" }));
    let def = scene(&doc);
    assert_eq!((def.props[&id("cup")].attach, &def.props[&id("cup")].anchor), (None, &Anchor::Stage));
    let (patch, out) = edit::<RemoveProp>(&mut doc, json!({ "prop": "cup" }));
    assert_eq!(out["removed"], "cup");
    assert_eq!(paths_of(&patch), ["scenes/main/props"], "the last prop takes the table with it");
}

#[test]
fn refusals_name_the_field_and_the_nearest_kind() {
    let mut doc = project();
    let e = refused::<AddProp>(&doc, json!({ "kind": "desc" }));
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(e.hint.as_deref().unwrap_or_default().contains("'desk'"), "{e:?}");
    let e = refused::<AddProp>(&doc, json!({ "kind": "desk", "params": { "width": 9.0 } }));
    assert_eq!(e.field.as_deref(), Some("params.width"), "{e:?}");
    let e = refused::<AddProp>(&doc, json!({ "kind": "lamp", "params": { "glow": 2.0 } }));
    assert_eq!(e.code, ErrorCode::Invalid, "an unknown param is refused: {e:?}");
    let e = refused::<AddProp>(&doc, json!({ "kind": "cup", "attach": "rightHand" }));
    assert_eq!(e.field.as_deref(), Some("props.cup.attach"), "a bone needs a member anchor: {e:?}");
    edit::<AddProp>(&mut doc, json!({ "kind": "desk" }));
    let e = refused::<AddProp>(&doc, json!({ "kind": "chair", "id": "desk" }));
    assert_eq!(e.code, ErrorCode::Conflict);
    let e = refused::<SetProp>(&doc, json!({ "prop": "dex", "visible": false }));
    assert!(e.hint.as_deref().unwrap_or_default().contains("'desk'"), "{e:?}");
}

#[test]
fn the_kinds_are_listed_from_the_registry_with_their_params() {
    let doc = project();
    let all = plan::<PropKinds>(&doc, json!({})).unwrap().output.unwrap();
    let kinds: Vec<&str> = all["kinds"].as_array().unwrap().iter().map(|k| k["kind"].as_str().unwrap()).collect();
    for kind in ["bed", "cafe-table", "chair", "cup", "desk", "lamp", "plant", "room", "rug", "shelf", "window"] {
        assert!(kinds.contains(&kind), "{kind} in {kinds:?}");
    }
    let lamp = plan::<PropKinds>(&doc, json!({ "kind": "lamp" })).unwrap().output.unwrap();
    assert_eq!(lamp["defaults"]["on"], true);
    assert!(lamp["params"]["properties"]["intensity"].is_object(), "{}", lamp["params"]);
}

#[test]
fn a_stage_preset_sets_the_room_and_replaces_only_the_last_presets_set() {
    let mut doc = project();
    edit::<SetLightRig>(&mut doc, json!({ "rig": "viewer-studio" }));
    edit::<AddProp>(&mut doc, json!({ "kind": "cup", "id": "my-cup", "position": [0.3, 0.75, 0.4] }));
    let lighting = scene(&doc).lighting;
    let (_, out) = edit::<SetStagePreset>(&mut doc, json!({ "preset": "study" }));
    assert_eq!(out["light_rig"], "dev-warm");
    let study = scene(&doc);
    let set = study.props.keys().filter(|k| k.as_str().starts_with("set-")).count();
    assert!(set >= 9, "{set}");
    assert!(study.props.contains_key(&id("my-cup")), "a prop someone added stays");
    assert_eq!(study.scene.ground.kind, GroundKind::None, "the room has its own floor");
    assert_eq!(study.lighting, lighting, "a preset never touches the lighting (D9)");

    let (patch, out) = edit::<SetStagePreset>(&mut doc, json!({ "preset": "green-screen" }));
    assert_eq!(out["replaced"].as_array().unwrap().len(), set, "every prop the study placed is replaced");
    let green = scene(&doc);
    assert_eq!(green.props.keys().map(PropId::as_str).collect::<Vec<_>>(), ["my-cup"]);
    assert!(
        matches!(green.scene.backdrop, Backdrop::Colour { colour } if colour == omuzaki_render_api::Colour::hex(0x0000_b140))
    );
    assert!(!patch.is_empty());
    let e = refused::<SetStagePreset>(&doc, json!({ "preset": "studdy" }));
    assert!(e.hint.as_deref().unwrap_or_default().contains("'study'"), "{e:?}");

    let listed = plan::<StagePresets>(&doc, json!({})).unwrap().output.unwrap();
    let names: Vec<&str> = listed["presets"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["study", "bedroom", "cafe", "night-library", "desk", "green-screen"]);
}
