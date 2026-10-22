//! Characters drawn in their look (#714, LK-06): a look edit reaches a staged character as material updates only, with
//! no mesh created, and taking the look back gives the frame it started from byte for byte. A character loaded from the
//! document comes in its stored look.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, LookDef, LookParams, MaterialClass,
    RegistryBuilder,
};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_doc_mock::MemDoc;
use omuzaki_render_api::{AlphaEncoding, Camera, Projection, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::body::draw_in_look;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use serde_json::{Value, json};

fn mock() -> BuiltCharacter {
    omuzaki_stage::link();
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

fn staged(renderer: &mut dyn Renderer) -> Stage {
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::splat(48), alpha: AlphaEncoding::Straight }).unwrap();
    let scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.8, 0.95),
        Vec3::new(0.0, 0.78, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    ));
    Stage::new(renderer, mock(), UploadOptions::default(), scene, target, Quality::offline(), StageClock::fixed(60.0))
        .expect("the stage starts")
}

fn look(node: &Value) -> LookParams {
    LookDef::from_node(Some(node)).and_then(|def| def.resolve()).expect("the look resolves")
}

/// The handle of every uploaded mesh, in build order.
fn meshes(stage: &Stage) -> Vec<omuzaki_render_api::MeshId> {
    stage.character().meshes().iter().map(|m| m.mesh).collect()
}

#[test]
fn a_look_edit_updates_materials_only_and_taking_it_back_restores_the_frame() {
    let mut renderer = MockRenderer::new();
    let mut stage = staged(&mut renderer);
    let target = stage.target();
    let input = FrameInput::default();
    stage.tick(&mut renderer, &input).unwrap();
    let before = renderer.read_target(target).unwrap();
    let (handles, stats) = (meshes(&stage), renderer.stats());

    let hard = look(&json!({ "overrides": { "all": { "softness_scale": 0.0, "outline_width_scale": 2.0 } } }));
    let updated = stage.apply_look(&mut renderer, &hard).unwrap();
    assert!(updated > 0, "the look changes materials");
    assert_eq!(meshes(&stage), handles, "no mesh was made again");
    assert_eq!(renderer.stats().meshes, stats.meshes, "no mesh was created");
    assert_eq!(renderer.stats().textures, stats.textures, "no texture was created");
    assert_eq!(stage.built().look().class(MaterialClass::Skin).softness_scale, Some(0.0));

    // Back to the default look: the frame is the one before any look edit.
    let default = look(&json!({ "preset": "soft-sd" }));
    stage.apply_look(&mut renderer, &default).unwrap();
    stage.redraw(&mut renderer, &input).unwrap();
    let after = renderer.read_target(target).unwrap();
    assert_eq!(before.rgba8, after.rgba8, "the default look gives back the frame byte for byte");
    stage.release(&mut renderer).unwrap();
}

#[test]
fn a_look_the_materials_cant_take_changes_nothing() {
    let mut renderer = MockRenderer::new();
    let mut stage = staged(&mut renderer);
    let mut bad = LookParams::default();
    bad.all.outline_width_scale = Some(f32::NAN);
    assert!(stage.apply_look(&mut renderer, &bad).is_err());
    assert!(stage.built().look().is_empty(), "the character keeps its look");
    stage.release(&mut renderer).unwrap();
}

#[test]
fn a_character_from_the_document_comes_in_its_stored_look() {
    let id = CharId::new("mock").unwrap();
    let mut built = mock();
    let none = MemDoc::from_value(json!({ "characters": { "mock": {} } }));
    assert!(draw_in_look(&none, &id, &mut built).unwrap().is_empty(), "no look node: the parts' own materials");
    let doc = MemDoc::from_value(
        json!({ "characters": { "mock": { "look": { "overrides": { "skin": { "rim_scale": 0.0 } } } } } }),
    );
    let changed = draw_in_look(&doc, &id, &mut built).unwrap();
    assert!(!changed.is_empty(), "the skin materials change");
    assert_eq!(built.look().class(MaterialClass::Skin).rim_scale, Some(0.0));
    let broken = MemDoc::from_value(json!({ "characters": { "mock": { "look": { "overrides": { "skin": 3 } } } } }));
    let error = draw_in_look(&broken, &id, &mut built).unwrap_err();
    assert_eq!(error.field.as_deref(), Some("look"));
}
