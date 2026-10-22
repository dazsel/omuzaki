//! The scene lifecycle (design D1, D8, D11; #275): `stage.create_scene`, `stage.list_scenes`,
//! `stage.set_default_scene`, the default scene and the scene a project opens in. Every edit is one patch whose
//! inverse undoes it exactly.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::CharId;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError, Plan};
use omuzaki_stage::ops::scene::create::CreateScene;
use omuzaki_stage::ops::scene::list::ListScenes;
use omuzaki_stage::ops::scene::set_default::SetDefaultScene;
use omuzaki_stage::recipe::LightRig;
use omuzaki_stage::scene::default::{default_scene, scene_for};
use omuzaki_stage_api::{Framing, LightShape, SceneId, paths};
use serde_json::{Value, json};

fn plan(doc: &MemDoc, op: &impl Op) -> Result<Plan, OpError> {
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

fn apply(doc: &mut MemDoc, op: &impl Op) -> Patch {
    let patch = plan(doc, op).expect("plans").patch;
    doc.apply(&patch).expect("applies");
    patch
}

fn create(id: &str, args: Value) -> CreateScene {
    let mut args = args;
    args["id"] = json!(id);
    serde_json::from_value(args).expect("the arguments read")
}

fn one_character() -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "kiri": { "name": "Kiri", "parts": {} } } }))
}

fn sid(s: &str) -> SceneId {
    SceneId::new(s).unwrap()
}

#[test]
fn the_first_scene_is_the_default_scene_and_one_undo_takes_both_back() {
    let mut doc = one_character();
    let before = doc.root().clone();
    let patch = apply(&mut doc, &create("main", json!({})));
    assert_eq!(doc.get(&paths::default_scene()), Some(&json!("main")), "the first scene becomes the default");
    let (id, scene) = scene_for(&doc, None).expect("reads");
    assert_eq!(id, Some(sid("main")));
    assert_eq!(scene.scene.name, "Main");
    assert_eq!(scene.cast.values().next().map(|m| m.character.clone()), CharId::new("kiri").ok(), "the only character");
    assert!(scene.check().expect("passes the rules").is_empty(), "no findings");
    let after = doc.root().clone();
    doc.apply(&patch.inverse()).expect("undoes");
    assert_eq!(doc.root(), &before, "undo is exact");
    doc.apply(&patch).expect("redoes");
    assert_eq!(doc.root(), &after, "redo is exact");
}

#[test]
fn a_second_scene_leaves_the_default_alone_and_set_default_moves_it() {
    let mut doc = one_character();
    apply(&mut doc, &create("main", json!({})));
    apply(&mut doc, &create("night_stream", json!({ "cast": [] })));
    assert_eq!(doc.get(&paths::default_scene()), Some(&json!("main")));
    let (_, night) = scene_for(&doc, Some(&sid("night_stream"))).unwrap();
    assert_eq!(night.scene.name, "Night stream");
    assert!(night.cast.is_empty(), "an empty cast is nobody");
    let patch = apply(&mut doc, &SetDefaultScene { scene: sid("night_stream") });
    assert_eq!(scene_for(&doc, None).unwrap().0, Some(sid("night_stream")));
    doc.apply(&patch.inverse()).unwrap();
    assert_eq!(scene_for(&doc, None).unwrap().0, Some(sid("main")));
    let listed = plan(&doc, &ListScenes {}).unwrap().output.unwrap();
    let scenes = listed["scenes"].as_array().unwrap();
    assert_eq!(scenes.len(), 2);
    assert_eq!(scenes[0]["id"], "main");
    assert_eq!(scenes[0]["default"], true);
    assert_eq!(scenes[0]["cast"][0]["character"], "kiri");
    assert_eq!(scenes[1]["default"], false);
}

#[test]
fn scene_for_takes_the_named_scene_then_the_default_then_the_first_then_the_built_in() {
    let mut doc = one_character();
    // No scene: the built-in default, unsaved.
    let (id, scene) = scene_for(&doc, None).unwrap();
    assert_eq!(id, None);
    assert_eq!(scene, default_scene(&doc).unwrap());
    assert!(doc.get(&paths::scene(&sid("main"))).is_none(), "scene_for never writes");
    apply(&mut doc, &create("b", json!({})));
    apply(&mut doc, &create("a", json!({})));
    // The default (b, the first one created) wins over the first by id (a).
    assert_eq!(scene_for(&doc, None).unwrap().0, Some(sid("b")));
    // A default that names no scene falls through to the first by id.
    let stale = Patch::new().with(paths::default_scene(), Some(json!("b")), Some(json!("gone")));
    doc.apply(&stale).unwrap();
    assert_eq!(scene_for(&doc, None).unwrap().0, Some(sid("a")));
    // A named scene wins; a missing one is not found.
    assert_eq!(scene_for(&doc, Some(&sid("b"))).unwrap().0, Some(sid("b")));
    assert_eq!(scene_for(&doc, Some(&sid("zzz"))).unwrap_err().code, ErrorCode::NotFound);
}

#[test]
fn the_default_scene_is_the_studio_look_with_three_framings() {
    let doc = MemDoc::from_value(json!({ "characters": { "a": {}, "b": {} } }));
    let scene = default_scene(&doc).unwrap();
    assert!(scene.cast.is_empty(), "two characters and no default character: nobody is guessed");
    assert_eq!(scene.lighting.rig.as_deref(), Some("viewer-studio"));
    let framings: Vec<Framing> = scene.cameras.values().map(|c| c.framing).collect();
    assert_eq!(framings, [Framing::Bust, Framing::Face, Framing::Full], "by id: bust, face, full");
    let full = &scene.cameras[&omuzaki_stage_api::CameraId::new("full").unwrap()];
    assert_eq!((full.yaw, full.pitch, full.fov), (30.0, 6.0, 30.0));
    assert!(scene.scene.post.bloom);
    let named =
        MemDoc::from_value(json!({ "characters": { "a": {}, "b": {} }, "project": { "default_character": "b" } }));
    let cast = default_scene(&named).unwrap().cast;
    assert_eq!(cast.values().next().unwrap().character.as_str(), "b", "the project's default character");
}

#[test]
fn the_studio_rig_as_scene_lighting_resolves_to_the_rigs_own_lights() {
    // Every directional light of every rig lands on its rig's direction when the scene resolves it at the stage.
    for name in LightRig::names() {
        let rig = LightRig::load(name).unwrap();
        let expected = rig.to_lights().unwrap();
        let lighting = rig.to_lighting().unwrap();
        for def in lighting.lights.values() {
            let LightShape::Directional { azimuth, elevation } = def.shape else { continue };
            let (az, el) = (azimuth.to_radians(), elevation.to_radians());
            let local = omuzaki_core::glam::Vec3::new(el.cos() * az.sin(), el.sin(), el.cos() * az.cos());
            let found = expected.iter().any(|l| match l {
                omuzaki_render_api::Light::Directional { to_light, intensity, .. } => {
                    to_light.abs_diff_eq(local, 1e-5) && (*intensity - def.intensity).abs() < 1e-6
                }
                _ => false,
            });
            assert!(found, "{name}: {def:?} is one of the rig's lights");
        }
    }
}

#[test]
fn create_refuses_a_taken_id_and_an_unknown_character_with_the_nearest_one() {
    let mut doc = one_character();
    apply(&mut doc, &create("main", json!({})));
    assert_eq!(plan(&doc, &create("main", json!({}))).unwrap_err().code, ErrorCode::Conflict);
    let error = plan(&doc, &create("other", json!({ "cast": ["kirl"] }))).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.hint.as_deref().is_some_and(|h| h.contains("kiri")), "{error:?}");
    assert_eq!(plan(&doc, &SetDefaultScene { scene: sid("nope") }).unwrap_err().code, ErrorCode::NotFound);
}
