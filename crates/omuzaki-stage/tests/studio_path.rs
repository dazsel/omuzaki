//! The studio's path from a project to a rendered frame (design D1, D10, D11; `docs/design/byom.md` D1): the scene the
//! project opens in, its cast loaded as a body whichever way it was made, the scene resolved against the body's
//! landmarks, the body staged with the scene's camera, lights and ground, and a frame rendered into the target the
//! viewport and the outputs read. The cast is the kit's mock character imported from its own VRM file; no third-party
//! model. On the GPU (`--features wgpu`) the frame is checked and, with `OMUZAKI_EVIDENCE_DIR`, written as a PNG.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "avatar/kit.rs"]
mod kit;

use std::collections::BTreeMap;

use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_char_api::{BuildServices, Quality as BuildQuality, RegistryBuilder};
use omuzaki_core::glam::{Mat4, UVec2};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Op, OpCtx};
use omuzaki_render_api::{AlphaEncoding, Camera, MtoonSupport, Quality, Renderer, Scene, TargetDesc, TargetId};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::body::{BodyServices, load_body};
use omuzaki_stage::frame::{FrameInput, StageClock};
use omuzaki_stage::ops::scene::create::CreateScene;
use omuzaki_stage::scene::default::scene_for;
use omuzaki_stage::scene::resolve::{CheckedScene, Figure, ResolvedScene};
use omuzaki_stage::stages::{AnyStage, FrameLoop};
use omuzaki_store_api::Store;
use omuzaki_store_mock::MemStore;
use serde_json::json;

/// A project whose only character is the mock imported from its VRM, with the default scene created as
/// `stage.create_scene {id: "main"}` writes it.
fn project() -> (MemDoc, MemStore) {
    let store = MemStore::new();
    let hash = store.put(&kit::vrm_bytes(&kit::kit()), kit::provenance()).expect("stored");
    let def = AvatarDef::new(AvatarSource::new(hash, SourceFormat::Vrm1, "mock.vrm"));
    let mut doc =
        MemDoc::from_value(json!({ "characters": { "mock_vrm": { "avatar": serde_json::to_value(&def).unwrap() } } }));
    let create: CreateScene = serde_json::from_value(json!({ "id": "main" })).unwrap();
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let patch = create.plan(&OpCtx::new(&doc, &actor, &clock)).expect("plans").patch;
    doc.apply(&patch).expect("applies");
    (doc, store)
}

/// Stages the project's opening scene on `renderer` at `size`, as the studio's viewport does, and renders one frame.
fn open(
    renderer: &mut dyn Renderer,
    doc: &MemDoc,
    store: &MemStore,
    size: UVec2,
) -> (AnyStage, TargetId, ResolvedScene) {
    let (_, scene) = scene_for(doc, None).expect("the opening scene");
    let (member, cast) = scene.cast.iter().next().expect("the default character is cast");
    let build = BuildServices::default();
    let clock = FakeClock::default();
    let services = BodyServices::new(&RegistryBuilder, &build, store, MtoonSupport::NONE, &clock);
    let body = load_body(doc, &services, &cast.character, None, BuildQuality::Live).expect("the cast loads");
    let figure = Figure::new(cast, body.landmarks().expect("measures"));
    let figures = BTreeMap::from([(member.clone(), figure)]);
    let mut resolved = ResolvedScene::new();
    #[allow(clippy::cast_precision_loss)]
    let aspect = size.x as f32 / size.y as f32;
    resolved.resolve_into(CheckedScene::new(&scene).expect("checks"), &figures, aspect).expect("resolves");
    let pose = body.base_pose();
    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).unwrap();
    let mut stage = body
        .stage(
            renderer,
            UploadOptions::default(),
            Scene::new(Camera::default()),
            target,
            Quality::offline(),
            StageClock::fixed(60.0),
        )
        .expect("stages");
    let owed = stage.set_scene(&resolved, Mat4::from(figure.root));
    assert!(owed.ground.is_some(), "a new scene owes its ground disc");
    stage.pay_scene(renderer).expect("the ground is made");
    assert!(stage.owed().ground.is_none(), "and is no longer owed");
    stage.tick(renderer, &FrameInput { pose, ..FrameInput::default() }).expect("a frame");
    (stage, target, resolved)
}

#[test]
fn an_imported_character_opens_in_the_default_scene_with_its_camera_lights_and_ground() {
    let (doc, store) = project();
    assert!(doc.get(&omuzaki_stage_api::paths::default_scene()).is_some());
    let mut renderer = omuzaki_render_mock::MockRenderer::new();
    let (stage, target, resolved) = open(&mut renderer, &doc, &store, UVec2::new(96, 128));
    let scene = stage.scene();
    let (_, camera) = resolved.cameras[resolved.active.expect("an opening camera")];
    assert_eq!(scene.camera, camera, "the scene's opening camera, framed on the imported body");
    assert_eq!(scene.lights.as_slice(), resolved.lights.as_slice(), "the studio rig's lights");
    let meshes = stage.scene().instances.len();
    assert!(scene.instances.iter().any(|i| !i.casts_shadow), "the ground disc is drawn and casts nothing");
    assert!(meshes > 1, "the avatar and the ground");
    stage.release(&mut renderer).unwrap();
    renderer.release(target.into()).unwrap();
}

/// On the GPU: the frame has the character in it, inside the picture, over the rig's backdrop.
#[cfg(feature = "wgpu")]
#[test]
#[allow(clippy::disallowed_methods, reason = "the evidence image is written beside the test, not by an op")]
fn on_the_gpu_the_default_scene_frames_the_imported_character() {
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};

    let (doc, store) = project();
    let mut renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let size = UVec2::new(240, 320);
    let (stage, target, _) = open(&mut renderer, &doc, &store, size);
    let image = renderer.read_target(target).expect("reads");
    let pixels = image.rgba8.as_chunks::<4>().0;
    let backdrop = pixels[0];
    let differs = |p: &[u8; 4]| p.iter().zip(&backdrop).any(|(a, b)| a.abs_diff(*b) > 8);
    let (w, h) = (size.x as usize, size.y as usize);
    let covered = pixels.iter().filter(|p| differs(p)).count();
    assert!(covered * 20 > w * h, "the character fills more than 5% of the frame: {covered} pixels");
    let edge = (0..w).flat_map(|x| [x, (h - 1) * w + x]).chain((0..h).flat_map(|y| [y * w, y * w + w - 1]));
    let on_edge = edge.filter(|&i| differs(&pixels[i])).count();
    assert!(on_edge * 50 < 2 * (w + h), "a full shot keeps the character inside the frame: {on_edge} edge pixels");
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        let png = omuzaki_stage::ops::render::still::encode_png(&image).expect("encodes");
        std::fs::write(std::path::Path::new(&dir).join("studio-default-scene-imported.png"), png).expect("writes");
    }
    stage.release(&mut renderer).unwrap();
    renderer.release(target.into()).unwrap();
}
