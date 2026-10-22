//! Editing a scene through ops (design D8; #700 to #704, #362): every edit is one patch with an exact inverse, a
//! one-field edit is a one-path patch, the knock-on changes (the shadow, the opening camera, the subject, the default
//! scene) land in the same patch, and refusals name the field with the nearest id.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError, Plan, Scope};
use omuzaki_render_api::Colour;
use omuzaki_stage::ops::scene::camera::{AddCamera, RemoveCamera, SetActiveCamera, SetCamera};
use omuzaki_stage::ops::scene::cast::{PlaceCharacter, RemoveMember, SetMember, SetSubject};
use omuzaki_stage::ops::scene::create::CreateScene;
use omuzaki_stage::ops::scene::light::{AddLight, LightRigs, RemoveLight, SetLight, SetLightRig, SetLighting};
use omuzaki_stage::ops::scene::look::{SetBackdrop, SetGround, SetPost};
use omuzaki_stage::ops::scene::manage::{DeleteScene, DescribeScene, DuplicateScene, RenameScene};
use omuzaki_stage::scene::default::scene_for;
use omuzaki_stage_api::{Anchor, Backdrop, MAX_LIGHTS, SceneDef, SceneId, paths};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn plan<T: Op + DeserializeOwned>(doc: &MemDoc, args: Value) -> Result<Plan, OpError> {
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let op: T = serde_json::from_value(args).expect("the arguments read");
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

/// Applies the op, checks that its inverse restores the document exactly and that redo lands the same state again,
/// and returns the patch and the output.
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

fn scene(doc: &MemDoc, id: &str) -> SceneDef {
    scene_for(doc, Some(&SceneId::new(id).unwrap())).unwrap().1
}

/// Two characters (`kiri`, with a variant `close_up`, and `aiko`) and the default scene `main` casting `kiri` as the
/// member `main`, lit by the studio rig (`key` with the shadow, `rim`, `fill`), with cameras `full`, `bust` and `face`.
fn project() -> MemDoc {
    let mut doc = MemDoc::from_value(json!({
        "characters": {
            "aiko": { "name": "Aiko", "parts": {} },
            "kiri": { "name": "Kiri", "parts": {}, "variants": { "close_up": { "overrides": {} } } },
        },
        "project": { "default_character": "kiri" },
    }));
    edit::<CreateScene>(&mut doc, json!({ "id": "main" }));
    doc
}

// ---- #700: describe, rename, duplicate, delete --------------------------------------------------------------------

#[test]
fn describe_reports_the_scene_and_a_missing_character_and_a_dangling_anchor_as_findings() {
    let mut doc = project();
    let described = plan::<DescribeScene>(&doc, json!({})).unwrap().output.unwrap();
    assert_eq!(described["scene"], "main");
    assert_eq!(described["default"], true);
    assert_eq!(described["lighting"]["lights"].as_array().unwrap().len(), 3);
    assert_eq!(described["lighting"]["limit"], format!("3 of {} lights besides the ambient", MAX_LIGHTS - 1));
    assert_eq!(described["cast"][0]["character"], "kiri");
    assert_eq!(described["findings"], json!([]));

    // A member whose character is gone is a finding, not a refusal.
    let mut root = doc.root().clone();
    root["scenes"]["main"]["cast"]["ghost"] = json!({ "character": "nobody" });
    doc = MemDoc::from_value(root);
    let described = plan::<DescribeScene>(&doc, json!({ "scene": "main" })).unwrap().output.unwrap();
    let findings = described["findings"].as_array().unwrap();
    assert!(findings.iter().any(|f| f["path"] == "cast.ghost.character"), "{findings:?}");

    // A light placed against a member who isn't in the cast still describes, with the rule it breaks.
    let mut root = doc.root().clone();
    root["scenes"]["main"]["lighting"]["lights"]["key"]["anchor"] = json!("member:left");
    let doc = MemDoc::from_value(root);
    let described = plan::<DescribeScene>(&doc, json!({})).unwrap().output.unwrap();
    let findings = described["findings"].as_array().unwrap();
    assert_eq!(findings[0]["path"], "lighting.lights.key.anchor", "{findings:?}");
}

#[test]
fn rename_changes_only_the_name_and_duplicate_leaves_the_applied_records_out() {
    let mut doc = project();
    let (patch, _) = edit::<RenameScene>(&mut doc, json!({ "scene": "main", "name": "Morning show" }));
    assert_eq!(paths_of(&patch), ["scenes/main/scene/name"]);
    assert_eq!(refused::<RenameScene>(&doc, json!({ "scene": "main", "name": "  " })).field.as_deref(), Some("name"));

    let mut root = doc.root().clone();
    root["scenes"]["main"]["applied"] = json!({ "studio": { "version": 1 } });
    let mut doc = MemDoc::from_value(root);
    let (_, out) = edit::<DuplicateScene>(&mut doc, json!({ "scene": "main", "id": "rehearsal" }));
    assert_eq!(out["name"], "Morning show (copy)");
    assert!(doc.get(&paths::scene(&SceneId::new("rehearsal").unwrap())).is_some());
    assert!(doc.root()["scenes"]["rehearsal"].get("applied").is_none(), "the records stay with the original");
    assert_eq!(scene(&doc, "rehearsal").lighting, scene(&doc, "main").lighting);
    assert_eq!(scene(&doc, "rehearsal").cast, scene(&doc, "main").cast);
    let taken = refused::<DuplicateScene>(&doc, json!({ "scene": "main", "id": "rehearsal" }));
    assert_eq!(taken.code, ErrorCode::Conflict);
}

#[test]
fn deleting_the_default_scene_moves_the_default_in_the_same_undo_step() {
    let mut doc = project();
    edit::<CreateScene>(&mut doc, json!({ "id": "night_stream", "cast": [] }));
    let (patch, out) = edit::<DeleteScene>(&mut doc, json!({ "scene": "main" }));
    assert_eq!(out["default"], "night_stream");
    assert_eq!(doc.get(&paths::default_scene()), Some(&json!("night_stream")));
    assert_eq!(patch.changes.len(), 2, "the scene and the default, one patch");
    let (_, out) = edit::<DeleteScene>(&mut doc, json!({ "scene": "night_stream" }));
    assert_eq!(out["default"], Value::Null);
    assert!(doc.get(&paths::default_scene()).is_none(), "no scene left, no default");
    let missing = refused::<DeleteScene>(&doc, json!({ "scene": "main" }));
    assert_eq!(missing.code, ErrorCode::NotFound);
}

#[test]
fn an_unknown_scene_is_refused_with_the_nearest_one_and_never_created() {
    let doc = project();
    let error = refused::<SetLighting>(&doc, json!({ "scene": "mian", "exposure": 1.2 }));
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.hint.as_deref().unwrap_or_default().contains("'main'"), "{error:?}");
}

// ---- #701 and #362: lights, lighting, rigs ------------------------------------------------------------------------

#[test]
fn a_one_field_light_edit_is_a_one_path_patch() {
    let mut doc = project();
    let (patch, _) = edit::<SetLight>(&mut doc, json!({ "light": "key", "intensity": 2.5 }));
    assert_eq!(paths_of(&patch), ["scenes/main/lighting/lights/key/intensity"]);
    let (patch, _) = edit::<SetLighting>(&mut doc, json!({ "exposure": 1.3 }));
    assert_eq!(paths_of(&patch), ["scenes/main/lighting/exposure"]);
    let (patch, _) = edit::<SetLight>(&mut doc, json!({ "light": "key", "intensity": 2.5 }));
    assert!(patch.changes.is_empty(), "setting what is already there changes nothing");
}

#[test]
fn giving_a_light_the_shadow_takes_it_from_the_old_caster_in_one_step() {
    let mut doc = project();
    let before = scene(&doc, "main");
    assert!(before.lighting.lights.iter().any(|(id, l)| id.as_str() == "key" && l.shadow));
    let (patch, out) = edit::<SetLight>(&mut doc, json!({ "light": "rim", "shadow": true }));
    assert_eq!(out["shadow_taken_from"], json!(["key"]));
    let mut changed = paths_of(&patch);
    changed.sort();
    assert_eq!(changed, ["scenes/main/lighting/lights/key/shadow", "scenes/main/lighting/lights/rim/shadow"]);
    let after = scene(&doc, "main");
    let casters: Vec<&str> =
        after.lighting.lights.iter().filter(|(_, l)| l.shadow).map(|(id, _)| id.as_str()).collect();
    assert_eq!(casters, ["rim"]);
}

#[test]
fn add_light_names_its_id_from_the_role_and_refuses_past_the_limit_with_the_count() {
    let mut doc = project();
    let (_, out) = edit::<AddLight>(&mut doc, json!({ "kind": "directional", "role": "rim", "azimuth": 150.0 }));
    assert_eq!(out["light"], "rim-2");
    let (_, out) = edit::<AddLight>(&mut doc, json!({ "kind": "point", "position": [0.5, 1.2, 0.5] }));
    assert_eq!(out["light"], "light");
    let wrong = refused::<AddLight>(&doc, json!({ "scene": "main", "kind": "point", "azimuth": 10.0 }));
    assert_eq!(wrong.field.as_deref(), Some("azimuth"));
    while scene(&doc, "main").lighting.lights.len() < MAX_LIGHTS - 1 {
        edit::<AddLight>(&mut doc, json!({ "kind": "directional" }));
    }
    let full = refused::<AddLight>(&doc, json!({ "kind": "directional" }));
    assert!(full.message.contains(&format!("has {} lights", MAX_LIGHTS - 1)), "{}", full.message);
}

#[test]
fn an_unknown_light_is_refused_with_the_nearest_id_and_remove_undoes_exactly() {
    let mut doc = project();
    let error = refused::<SetLight>(&doc, json!({ "light": "kye", "intensity": 1.0 }));
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::NotFound, Some("light")));
    assert!(error.hint.as_deref().unwrap_or_default().contains("'key'"), "{error:?}");
    let (patch, _) = edit::<RemoveLight>(&mut doc, json!({ "light": "fill" }));
    assert_eq!(paths_of(&patch), ["scenes/main/lighting/lights/fill"]);
}

#[test]
fn set_lighting_turns_the_skin_floor_and_bloom_off_and_on() {
    let mut doc = project();
    edit::<SetLighting>(
        &mut doc,
        json!({ "skin_floor": 0.3, "bloom": { "strength": 0.5, "radius": 0.4, "threshold": 0.8 } }),
    );
    assert_eq!(scene(&doc, "main").lighting.skin_floor, Some(0.3));
    assert!(scene(&doc, "main").lighting.bloom.is_some());
    let (_, out) = edit::<SetLighting>(&mut doc, json!({ "skin_floor": "off", "bloom": "off" }));
    assert_eq!(scene(&doc, "main").lighting.skin_floor, None);
    assert!(scene(&doc, "main").lighting.bloom.is_none());
    assert_eq!(out["changed"].as_array().unwrap().len(), 2, "{out}");
    let error = refused::<SetLighting>(&doc, json!({ "exposure": -1.0 }));
    assert_eq!(error.field.as_deref(), Some("lighting.exposure"));
}

#[test]
fn a_light_rig_replaces_only_the_lighting_and_the_same_rig_again_changes_nothing() {
    let mut doc = project();
    let (patch, out) = edit::<SetLightRig>(&mut doc, json!({ "rig": "viewer-moonlit" }));
    assert!(paths_of(&patch).iter().all(|p| p.starts_with("scenes/main/lighting")), "{:?}", paths_of(&patch));
    assert_eq!(out["title"], "Moonlit");
    assert_eq!(out["replaced"], json!({ "rig": "viewer-studio", "edited": false }));
    assert_eq!(scene(&doc, "main").lighting.rig.as_deref(), Some("viewer-moonlit"));
    let (again, _) = edit::<SetLightRig>(&mut doc, json!({ "rig": "viewer-moonlit" }));
    assert!(again.changes.is_empty(), "the same rig twice is no change");
    edit::<SetLight>(&mut doc, json!({ "light": "key", "intensity": 0.1 }));
    let (_, out) = edit::<SetLightRig>(&mut doc, json!({ "rig": "viewer-studio" }));
    assert_eq!(out["replaced"], json!({ "rig": "viewer-moonlit", "edited": true }), "the edit is reported");

    let unknown = refused::<SetLightRig>(&doc, json!({ "rig": "viewer-moonlight" }));
    let hint = unknown.hint.unwrap_or_default();
    assert!(hint.contains("'viewer-moonlit'") && hint.contains("perform-facecam"), "{hint}");
    let no_scene = refused::<SetLightRig>(&doc, json!({ "scene": "nope", "rig": "viewer-studio" }));
    assert_eq!(no_scene.code, ErrorCode::NotFound);
    assert!(doc.get(&paths::scene(&SceneId::new("nope").unwrap())).is_none());
    // The executor refuses an actor without `edit:scene` before planning; every scene edit asks for it.
    assert_eq!(SetLightRig::meta().scope, Scope::EditScene);
    assert_eq!(AddCamera::meta().scope, Scope::EditScene);
    assert_eq!(PlaceCharacter::meta().scope, Scope::EditScene);

    let rigs = plan::<LightRigs>(&doc, json!({})).unwrap().output.unwrap();
    let rigs = rigs["rigs"].as_array().unwrap();
    assert_eq!(rigs.len(), 9);
    for rig in rigs {
        assert!(!rig["title"].as_str().unwrap().is_empty() && !rig["summary"].as_str().unwrap().is_empty(), "{rig}");
    }
}

// ---- #702: cameras ------------------------------------------------------------------------------------------------

#[test]
fn cameras_are_added_edited_one_path_at_a_time_and_free_ones_need_a_pose() {
    let mut doc = project();
    let (_, out) = edit::<AddCamera>(&mut doc, json!({}));
    assert_eq!(out["camera"], "bust-2", "bust from the front by default, named after its framing");
    let (patch, _) = edit::<SetCamera>(&mut doc, json!({ "camera": "full", "yaw": 45.0 }));
    assert_eq!(paths_of(&patch), ["scenes/main/cameras/full/yaw"]);
    let error = refused::<SetCamera>(&doc, json!({ "camera": "bust", "framing": "free" }));
    assert_eq!(error.field.as_deref(), Some("position"));
    assert!(error.message.contains("free camera needs"), "{}", error.message);
    let error = refused::<AddCamera>(&doc, json!({ "framing": "free", "position": [0.0, 1.0, 2.0] }));
    assert_eq!(error.field.as_deref(), Some("target"));
    edit::<SetCamera>(
        &mut doc,
        json!({ "camera": "bust", "framing": "free", "position": [0.4, 1.5, 1.8], "target": [0.0, 1.3, 0.0] }),
    );
    // Moving the pose alone keeps it free.
    let (patch, _) = edit::<SetCamera>(&mut doc, json!({ "camera": "bust", "position": [0.5, 1.5, 1.8] }));
    assert_eq!(paths_of(&patch), ["scenes/main/cameras/bust/position/0"]);
    // Back to a preset drops the pose.
    edit::<SetCamera>(&mut doc, json!({ "camera": "bust", "framing": "bust" }));
    let bust = &scene(&doc, "main").cameras[&"bust".parse().unwrap()];
    assert!(bust.position.is_none() && bust.target.is_none());
    let error = refused::<SetCamera>(&doc, json!({ "camera": "bsut", "yaw": 1.0 }));
    assert!(error.hint.unwrap_or_default().contains("'bust'"));
}

#[test]
fn removing_the_opening_camera_moves_it_in_the_same_step() {
    let mut doc = project();
    assert_eq!(scene(&doc, "main").scene.camera.as_ref().map(ToString::to_string).as_deref(), Some("full"));
    let (patch, out) = edit::<RemoveCamera>(&mut doc, json!({ "camera": "full" }));
    assert_eq!(out["opening_camera"], "bust", "the first remaining by id");
    let mut changed = paths_of(&patch);
    changed.sort();
    assert_eq!(changed, ["scenes/main/cameras/full", "scenes/main/scene/camera"]);
    let (patch, _) = edit::<SetActiveCamera>(&mut doc, json!({ "camera": "face" }));
    assert_eq!(paths_of(&patch), ["scenes/main/scene/camera"]);
}

#[test]
fn an_orbit_of_sixty_camera_edits_in_one_transaction_undoes_in_one_step() {
    let mut doc = project();
    let before = doc.root().clone();
    assert!(<SetCamera as Op>::coalesces(), "consecutive orbits merge into one history entry");
    let mut transaction = Patch::new();
    for step in 0..60_u8 {
        let yaw = -180.0 + f32::from(step) * 6.0;
        let plan = plan::<SetCamera>(&doc, json!({ "camera": "full", "yaw": yaw })).unwrap();
        doc.apply(&plan.patch).unwrap();
        transaction.extend(plan.patch);
    }
    doc.apply(&transaction.inverse()).unwrap();
    assert_eq!(doc.root(), &before, "one undo of the transaction restores the camera");
}

// ---- #703: the cast -----------------------------------------------------------------------------------------------

#[test]
fn a_character_placed_without_a_position_stands_right_of_the_rightmost_member() {
    let mut doc = project();
    edit::<CreateScene>(&mut doc, json!({ "id": "empty", "cast": [] }));
    let (_, out) = edit::<PlaceCharacter>(&mut doc, json!({ "scene": "empty", "character": "aiko" }));
    assert_eq!((out["member"].clone(), out["position"].clone()), (json!("aiko"), json!([0.0, 0.0])), "zero members");
    let (_, out) = edit::<PlaceCharacter>(&mut doc, json!({ "scene": "empty", "character": "aiko" }));
    assert_eq!(out["member"], "aiko-2");
    let x = out["position"][0].as_f64().unwrap();
    assert!((x - 0.7).abs() < 1e-6, "one member: {x}");
    edit::<SetMember>(&mut doc, json!({ "scene": "empty", "member": "aiko", "position": [1.5, 0.0] }));
    let (_, out) = edit::<PlaceCharacter>(&mut doc, json!({ "scene": "empty", "character": "kiri" }));
    let x = out["position"][0].as_f64().unwrap();
    assert!((x - 2.2).abs() < 1e-6, "two members, the rightmost at 1.5: {x}");
}

#[test]
fn unknown_characters_and_variants_are_refused_with_the_nearest() {
    let mut doc = project();
    let error = refused::<PlaceCharacter>(&doc, json!({ "character": "aiku" }));
    assert_eq!(error.field.as_deref(), Some("character"));
    assert!(error.hint.unwrap_or_default().contains("'aiko'"));
    let error = refused::<PlaceCharacter>(&doc, json!({ "character": "kiri", "variant": "closeup" }));
    assert_eq!(error.field.as_deref(), Some("variant"));
    assert!(error.hint.unwrap_or_default().contains("'close_up'"));
    let (patch, _) = edit::<SetMember>(&mut doc, json!({ "member": "main", "variant": "close_up" }));
    assert_eq!(paths_of(&patch), ["scenes/main/cast/main/variant"]);
    edit::<SetMember>(&mut doc, json!({ "member": "main", "variant": "off" }));
    assert!(scene(&doc, "main").cast[&"main".parse().unwrap()].variant.is_none());
}

#[test]
fn removing_the_subject_moves_it_and_repoints_its_anchors_in_one_undo_step() {
    let mut doc = project();
    edit::<PlaceCharacter>(&mut doc, json!({ "character": "aiko", "id": "guest" }));
    edit::<SetLight>(&mut doc, json!({ "light": "rim", "anchor": "member:main" }));
    edit::<SetCamera>(&mut doc, json!({ "camera": "face", "subject": "member:main" }));
    let before = doc.root().clone();
    let (patch, out) = edit::<RemoveMember>(&mut doc, json!({ "member": "main" }));
    assert_eq!(out["subject"], "guest");
    assert_eq!(out["repointed_to_subject"], json!(["lighting.lights.rim.anchor", "cameras.face.subject"]));
    let after = scene(&doc, "main");
    assert_eq!(after.lighting.lights[&"rim".parse().unwrap()].anchor, Anchor::Subject);
    assert_eq!(after.cameras[&"face".parse().unwrap()].subject, Anchor::Subject);
    doc.apply(&patch.inverse()).unwrap();
    assert_eq!(doc.root(), &before, "one undo brings the member, the subject and the anchors back");
    let (patch, _) = edit::<SetSubject>(&mut doc, json!({ "member": "guest" }));
    assert_eq!(paths_of(&patch), ["scenes/main/scene/subject"]);
    assert_eq!(refused::<SetSubject>(&doc, json!({ "member": "gust" })).code, ErrorCode::NotFound);
}

// ---- #704: backdrop, ground, post ---------------------------------------------------------------------------------

#[test]
fn backdrop_ground_and_post_edit_one_path_and_refuse_what_doesnt_fit() {
    let mut doc = project();
    edit::<SetBackdrop>(&mut doc, json!({ "kind": "colour", "colour": "#00b140" }));
    assert!(matches!(scene(&doc, "main").scene.backdrop, Backdrop::Colour { .. }));
    edit::<SetBackdrop>(&mut doc, json!({ "kind": "transparent" }));
    assert_eq!(scene(&doc, "main").scene.backdrop, Backdrop::Transparent {});
    let error = refused::<SetBackdrop>(&doc, json!({ "kind": "colour" }));
    assert_eq!(error.field.as_deref(), Some("colour"));

    let (patch, _) = edit::<SetGround>(&mut doc, json!({ "opacity": 0.4 }));
    assert_eq!(paths_of(&patch), ["scenes/main/scene/ground/opacity"]);
    edit::<SetGround>(&mut doc, json!({ "radius": 1.5 }));
    assert_eq!(scene(&doc, "main").scene.ground.radius, Some(1.5));
    edit::<SetGround>(&mut doc, json!({ "radius": "fit" }));
    assert_eq!(scene(&doc, "main").scene.ground.radius, None);
    let error = refused::<SetGround>(&doc, json!({ "opacity": 1.5 }));
    assert_eq!(error.field.as_deref(), Some("scene.ground.opacity"));

    let (patch, _) = edit::<SetPost>(&mut doc, json!({ "bloom": false }));
    assert_eq!(paths_of(&patch), ["scenes/main/scene/post/bloom"]);
    edit::<SetPost>(&mut doc, json!({ "grain": 0.2, "grade": { "saturation": 1.1, "contrast": 1.05 } }));
    assert_eq!(scene(&doc, "main").scene.post.grain, Some(0.2));
    let (_, out) = edit::<SetPost>(&mut doc, json!({ "grain": "off", "grade": "off" }));
    let post = scene(&doc, "main").scene.post;
    assert!(post.grain.is_none() && post.grade.is_none(), "{out}");
    let error = refused::<SetPost>(&doc, json!({ "grain": 2.0 }));
    assert_eq!(error.field.as_deref(), Some("scene.post.grain"));
}

// ---- #931: spot lights, fog and the environment ----------------------------------------------------------------

#[test]
fn a_spot_is_added_with_its_beam_and_edited_without_the_fields_of_other_kinds() {
    let mut doc = project();
    let (_, out) = edit::<AddLight>(
        &mut doc,
        json!({ "kind": "spot", "role": "accent", "position": [0.0, 2.6, 1.2], "target": [0.0, 1.0, 0.0],
                "cone": 25.0, "intensity": 6.0, "shadow": true }),
    );
    assert_eq!(out["light"], "accent");
    assert_eq!(out["shadow_taken_from"], json!([]), "a spot's shadow leaves the directional key's alone");
    let lights = scene(&doc, "main").lighting.lights;
    let accent = &lights[&omuzaki_stage_api::LightId::new("accent").unwrap()];
    assert_eq!(
        accent.shape,
        omuzaki_stage_api::LightShape::Spot {
            position: [0.0, 2.6, 1.2],
            target: [0.0, 1.0, 0.0],
            cone: 25.0,
            penumbra: 0.2,
            range: 6.0
        }
    );
    assert!(accent.shadow && lights[&omuzaki_stage_api::LightId::new("key").unwrap()].shadow);

    let (patch, _) = edit::<SetLight>(&mut doc, json!({ "light": "accent", "cone": 35.0 }));
    assert_eq!(paths_of(&patch), ["scenes/main/lighting/lights/accent/cone"]);
    let wrong = refused::<SetLight>(&doc, json!({ "light": "accent", "azimuth": 10.0 }));
    assert_eq!(wrong.field.as_deref(), Some("azimuth"));
    let wrong = refused::<SetLight>(&doc, json!({ "light": "key", "target": [0.0, 1.0, 0.0] }));
    assert_eq!(wrong.field.as_deref(), Some("target"));
    let wrong = refused::<AddLight>(&doc, json!({ "kind": "point", "cone": 20.0 }));
    assert_eq!(wrong.field.as_deref(), Some("cone"));
    let wide = refused::<SetLight>(&doc, json!({ "light": "accent", "cone": 120.0 }));
    assert_eq!(wide.field.as_deref(), Some("lighting.lights.accent.cone"));
}

#[test]
fn fog_and_an_environment_are_set_and_turned_off_and_the_backdrop_shows_the_sky() {
    let mut doc = project();
    let (_, out) = edit::<SetLighting>(
        &mut doc,
        json!({ "fog": { "colour": "#2a2238", "near": 2.0, "far": 9.0 },
                "environment": { "kind": "sky", "intensity": 0.6 } }),
    );
    assert_eq!(out["changed"].as_array().map(Vec::len), Some(2), "{out}");
    let lighting = scene(&doc, "main").lighting;
    assert_eq!(lighting.fog.map(|f| (f.near, f.far)), Some((2.0, 9.0)));
    assert_eq!(lighting.environment.map(|e| e.intensity), Some(0.6));
    edit::<SetBackdrop>(&mut doc, json!({ "kind": "environment" }));
    assert_eq!(scene(&doc, "main").scene.backdrop, Backdrop::Environment {});
    let described = plan::<DescribeScene>(&doc, json!({})).unwrap().output.unwrap();
    assert_eq!(described["lighting"]["environment"]["kind"], "sky");
    assert_eq!(described["findings"], json!([]));

    let far = refused::<SetLighting>(&doc, json!({ "fog": { "colour": "#000000", "near": 5.0, "far": 4.0 } }));
    assert_eq!(far.field.as_deref(), Some("lighting.fog.far"));
    edit::<SetLighting>(&mut doc, json!({ "fog": "off", "environment": "off" }));
    let lighting = scene(&doc, "main").lighting;
    assert_eq!((lighting.fog, lighting.environment), (None, None));
    let described = plan::<DescribeScene>(&doc, json!({})).unwrap().output.unwrap();
    assert_eq!(described["findings"][0]["kind"], "no_environment", "the backdrop has no sky to show now");
}

// ---- #930: gradient and image backdrops --------------------------------------------------------------------------

#[test]
fn a_gradient_and_an_image_backdrop_are_set_with_their_own_fields_only() {
    let mut doc = project();
    edit::<SetBackdrop>(&mut doc, json!({ "kind": "gradient", "top": "#1d2b4f", "bottom": "#e8a87c" }));
    assert_eq!(
        scene(&doc, "main").scene.backdrop,
        Backdrop::Gradient { top: Colour::hex(0x001d_2b4f), bottom: Colour::hex(0x00e8_a87c) }
    );
    let half = refused::<SetBackdrop>(&doc, json!({ "kind": "gradient", "top": "#1d2b4f" }));
    assert_eq!(half.field.as_deref(), Some("bottom"));

    let asset = omuzaki_store_api::ContentHash::of(b"a picture");
    edit::<SetBackdrop>(&mut doc, json!({ "kind": "image", "asset": asset, "fit": "contain", "top": "#000000" }));
    assert_eq!(
        scene(&doc, "main").scene.backdrop,
        Backdrop::Image {
            asset,
            fit: omuzaki_stage_api::BackdropFit::Contain,
            bars: Some(omuzaki_stage_api::Bars { top: Colour::BLACK, bottom: Colour::BLACK }),
        },
        "one bar colour is flat bars"
    );
    let described = plan::<DescribeScene>(&doc, json!({})).unwrap().output.unwrap();
    assert_eq!(described["backdrop"]["kind"], "image", "{described}");

    let no_asset = refused::<SetBackdrop>(&doc, json!({ "kind": "image" }));
    assert_eq!(no_asset.field.as_deref(), Some("asset"));
    let foreign = refused::<SetBackdrop>(
        &doc,
        json!({ "kind": "gradient", "top": "#000000", "bottom": "#000000", "fit": "cover" }),
    );
    assert_eq!(foreign.field.as_deref(), Some("fit"));
    let foreign = refused::<SetBackdrop>(&doc, json!({ "kind": "colour", "colour": "#00b140", "asset": asset }));
    assert_eq!(foreign.field.as_deref(), Some("asset"));
    let foreign = refused::<SetBackdrop>(&doc, json!({ "kind": "transparent", "colour": "#00b140" }));
    assert_eq!(foreign.field.as_deref(), Some("colour"));
}
