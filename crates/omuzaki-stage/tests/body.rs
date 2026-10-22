//! A character from the document on a stage, whichever way it was made (`docs/design/byom.md` D1): a kit character is
//! built from its definition, an imported one rebuilt from the original in the store, and both stage, frame and
//! perform through the same calls. The imported character is the kit's own mock exported to VRM 1.0; no third-party
//! model.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "avatar/kit.rs"]
mod kit;

use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_char_api::{BuildServices, CharId, Quality as BuildQuality, RegistryBuilder, VariantId};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use omuzaki_render_api::{AlphaEncoding, Camera, MtoonSupport, Projection, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::body::{Body, BodyServices, load_body};
use omuzaki_stage::frame::{FrameInput, StageClock};
use omuzaki_stage::stages::FrameLoop;
use omuzaki_store_api::Store;
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

/// A project holding the kit's mock character twice: built from its parts as `kit`, and imported from its own VRM
/// file as `kit_vrm`, the original in the store as `io.import {as: avatar}` leaves it.
struct Project {
    doc: MemDoc,
    store: MemStore,
}

fn project() -> Project {
    let built = kit::kit();
    let store = MemStore::new();
    let hash = store.put(&kit::vrm_bytes(&built), kit::provenance()).expect("stored");
    let def = AvatarDef::new(AvatarSource::new(hash, SourceFormat::Vrm1, "kit.vrm"));
    let mut node = Value::Null;
    built.def.to_node(&mut node).expect("the definition writes");
    let doc = MemDoc::from_value(json!({
        "characters": {
            "kit": node,
            "kit_vrm": { "avatar": serde_json::to_value(&def).expect("the node writes") },
        }
    }));
    Project { doc, store }
}

fn load(project: &Project, id: &str, variant: Option<&VariantId>) -> Result<Body, omuzaki_ops_api::OpError> {
    let build = BuildServices::default();
    let clock = FakeClock::default();
    let services = BodyServices::new(&RegistryBuilder, &build, &project.store, MtoonSupport::NONE, &clock);
    load_body(&project.doc, &services, &CharId::new(id).expect("an id"), variant, BuildQuality::Live)
}

fn near(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

#[test]
fn both_kinds_of_character_load_from_the_document_as_what_they_are() {
    let project = project();
    assert!(matches!(load(&project, "kit", None).expect("the kit character"), Body::Kit(_)));
    assert!(matches!(load(&project, "kit_vrm", None).expect("the imported character"), Body::Avatar(_)));
}

#[test]
fn an_imported_character_frames_like_the_kit_character_it_was_exported_from() {
    let project = project();
    let kit = load(&project, "kit", None).unwrap().landmarks().expect("the kit measures");
    let imported = load(&project, "kit_vrm", None).unwrap().landmarks().expect("the avatar measures");
    let mm = 1e-3;
    assert!(
        kit.body.min.abs_diff_eq(imported.body.min, mm) && kit.body.max.abs_diff_eq(imported.body.max, mm),
        "{kit:?}\n{imported:?}"
    );
    assert!(near(kit.height_m, imported.height_m, mm), "{} vs {}", kit.height_m, imported.height_m);
    for (what, a, b) in [("eyes", kit.eyes, imported.eyes), ("chest", kit.chest, imported.chest)] {
        match (a, b) {
            (Some(a), Some(b)) => assert!(near(a, b, mm), "{what}: {a} vs {b}"),
            (None, None) => {}
            other => panic!("{what}: {other:?}"),
        }
    }
    let (crown, chin) = (imported.crown().expect("a head"), imported.chin().expect("a head"));
    assert!(chin < crown && crown <= imported.top() + mm, "the head is a span inside the body: {chin}..{crown}");
}

#[test]
fn a_loaded_body_stages_and_runs_the_frame_loop_either_way() {
    let project = project();
    for id in ["kit", "kit_vrm"] {
        let body = load(&project, id, None).expect("loads");
        let pose = body.base_pose();
        let mut renderer = MockRenderer::new();
        let target =
            renderer.create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight }).unwrap();
        let camera = Camera::look_at(
            Vec3::new(0.0, 0.6, 2.2),
            Vec3::new(0.0, 0.6, 0.0),
            Vec3::Y,
            Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
        );
        let mut stage = body
            .stage(
                &mut renderer,
                UploadOptions::default(),
                Scene::new(camera),
                target,
                Quality::offline(),
                StageClock::fixed(60.0),
            )
            .expect("stages");
        let input = FrameInput { pose, ..FrameInput::default() };
        let frame = stage.tick(&mut renderer, &input).expect("a frame");
        assert_eq!(frame.tick.index, 0, "{id}");
        assert!(!stage.scene().instances.is_empty(), "{id} draws");
        stage.release(&mut renderer).expect("released");
        renderer.release(target.into()).unwrap();
    }
}

#[test]
fn an_imported_character_has_no_variants_and_a_missing_one_is_not_found() {
    let project = project();
    let variant = VariantId::new("summer").expect("an id");
    let error = load(&project, "kit_vrm", Some(&variant)).expect_err("no variants");
    assert_eq!(error.code, ErrorCode::Invalid, "{error}");
    let error = load(&project, "nobody", None).expect_err("no such character");
    assert_eq!(error.code, ErrorCode::NotFound, "{error}");
    let empty = Project { doc: MemDoc::from_value(project.doc.root().clone()), store: MemStore::new() };
    let error = load(&empty, "kit_vrm", None).expect_err("the original is gone");
    assert_eq!(error.code, ErrorCode::NotFound, "{error}");
}
