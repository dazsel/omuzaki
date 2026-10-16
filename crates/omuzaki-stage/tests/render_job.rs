//! The render-job plumbing on the mock document and the mock renderer: the size limits, the cost hint, building the
//! character, the light rigs, the renderer lock, and storing the PNG under its name.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use omuzaki_char_api::{CharId, VariantId};
use omuzaki_char_mock::CHARACTER_JSON;
use omuzaki_core::glam::UVec2;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_render_api::{AlphaEncoding, Image, Renderer};
use omuzaki_stage::StageError;
use omuzaki_stage::ops::render::job::{
    DEFAULT_LIGHT_RIG, RenderQuality, build_character, check_size, cost, render_provenance, still_light, store_png,
    with_renderer,
};
use omuzaki_stage::ops::render::still::encode_png;
use omuzaki_stage::recipe::LightRig;
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use support::{Harness, Missing, TestDoc, mock_doc};

fn mock() -> CharId {
    CharId::new("mock").expect("a valid id")
}

/// A 16 by 16 transparent PNG: real bytes for the store, small enough to be quick.
fn png() -> Vec<u8> {
    let image = Image::new(UVec2::splat(16), AlphaEncoding::Straight, vec![0; 16 * 16 * 4]).expect("the image");
    encode_png(&image).expect("the PNG encodes")
}

fn code(error: Option<OpError>) -> Option<ErrorCode> {
    error.map(|e| e.code)
}

#[test]
fn sizes_outside_the_limits_are_refused() {
    for size in [[0, 64], [64, 0], [15, 64], [64, 15], [4097, 64], [64, 4097]] {
        assert_eq!(code(check_size(size).err()), Some(ErrorCode::Invalid), "{size:?} is refused");
    }
    assert_eq!(check_size([16, 16]).ok(), Some(UVec2::new(16, 16)), "the smallest side passes");
    assert_eq!(check_size([4096, 4096]).ok(), Some(UVec2::new(4096, 4096)), "the largest side passes");
}

#[test]
fn the_cost_hint_counts_every_image_the_render_allocates() {
    // The bloom chain of a target T has the bright image at half T and two images for each of 5 mips, at 8 bytes. Of a
    // 4096 target that is 117 MiB; the renderer keeps 3 chains, so the cache can hold 352 MiB of them for any job.
    //
    // High 4096 by 4096 at 4 samples: internal 6144 by 6144 at 56 bytes: 2016 MiB of attachments; a 2048 shadow map at 4
    // bytes: 16 MiB; the output: 64 MiB; the read-back: 64 MiB; and the bloom cache: 352 MiB. Total 2512 MiB, and 2576 with
    // the meshes and textures.
    let hint = cost(UVec2::splat(4096), 1, RenderQuality::High);
    assert_eq!(hint.gpu_mib, Some(2576));
    assert_eq!(hint.seconds, Some(0.5), "a high still takes half a second");
    assert_eq!(hint.usd, None, "no money is spent");

    // Offline is 16 samples (200 bytes a pixel) at 1024 by 1024: 200 MiB of attachments, 16 MiB of shadow map at high
    // shadows, 4 MiB of output, 4 MiB of read-back and 352 MiB of bloom cache: 576 MiB, and 640 with the meshes.
    let offline = cost(UVec2::splat(1024), 1, RenderQuality::Offline);
    assert_eq!(offline.gpu_mib, Some(640));
    assert_eq!(offline.seconds, Some(2.0), "an offline still takes two seconds");

    // Live: 4 samples at 64 by 64, with the low shadow map (4 MiB): 4.25 MiB of images, and 356 with the bloom cache, which
    // an earlier job's chains can fill whatever this preset's post effects are: 420 with the meshes and textures.
    assert_eq!(cost(UVec2::splat(64), 1, RenderQuality::Live).gpu_mib, Some(420));
}

#[test]
fn a_missing_parent_variant_is_named_as_inherited() {
    // `broken` extends a variant the document lacks: the error names that parent, not `broken`.
    let mut character: Value = serde_json::from_str(CHARACTER_JSON).expect("the fixture parses");
    let broken = json!({ "description": "extends a missing variant", "extends": "missing", "overrides": {} });
    character.as_object_mut().expect("a table").insert("variants".to_owned(), json!({ "broken": broken }));
    let harness = Harness::new(TestDoc(MemDoc::from_value(json!({ "characters": { "mock": character } }))));

    let broken = VariantId::new("broken").expect("a valid id");
    let error = harness
        .with_job(|job| build_character(job, &mock(), Some(&broken), RenderQuality::Offline))
        .expect_err("the parent is missing");
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.message.contains("'missing'"), "the error names the parent: {}", error.message);
    assert!(error.message.contains("extends"), "and says it is inherited: {}", error.message);
    assert!(!error.message.contains("there is no variant"), "not the variant asked for: {}", error.message);
}

#[test]
fn the_mock_character_builds_from_the_document() {
    let harness = Harness::new(mock_doc());
    let built = harness
        .with_job(|job| build_character(job, &mock(), None, RenderQuality::Offline))
        .expect("the mock character builds");
    assert_eq!(built.id, mock());
}

#[test]
fn a_variant_is_applied() {
    let harness = Harness::new(mock_doc());
    let close_up = VariantId::new("close-up").expect("a valid id");
    let plain = harness.with_job(|job| build_character(job, &mock(), None, RenderQuality::Offline)).expect("builds");
    let varied = harness
        .with_job(|job| build_character(job, &mock(), Some(&close_up), RenderQuality::Offline))
        .expect("the variant builds");
    assert_eq!(varied.variant, Some(close_up), "the built character records its variant");
    assert_ne!(
        serde_json::to_value(&varied.def).expect("serialises"),
        serde_json::to_value(&plain.def).expect("serialises"),
        "the variant changes the definition"
    );
}

#[test]
fn an_unknown_character_or_variant_is_not_found() {
    let harness = Harness::new(mock_doc());
    let nobody = CharId::new("nobody").expect("a valid id");
    let missing = harness.with_job(|job| build_character(job, &nobody, None, RenderQuality::Offline)).err();
    assert_eq!(code(missing), Some(ErrorCode::NotFound));

    let no_such = VariantId::new("no-such-variant").expect("a valid id");
    let variant = harness.with_job(|job| build_character(job, &mock(), Some(&no_such), RenderQuality::Offline)).err();
    assert_eq!(code(variant), Some(ErrorCode::NotFound));
}

#[test]
fn a_missing_service_is_unavailable() {
    let no_builder = Harness::without(mock_doc(), Some(Missing::Builder));
    let build = no_builder.with_job(|job| build_character(job, &mock(), None, RenderQuality::Offline)).err();
    assert_eq!(code(build), Some(ErrorCode::Unavailable), "no character builder");

    let no_build_services = Harness::without(mock_doc(), Some(Missing::BuildServices));
    let build = no_build_services.with_job(|job| build_character(job, &mock(), None, RenderQuality::Offline)).err();
    assert_eq!(code(build), Some(ErrorCode::Unavailable), "no build services");

    let no_renderer = Harness::without(mock_doc(), Some(Missing::Renderer));
    let lend = no_renderer.with_job(|job| with_renderer(job, |_| Ok(()))).err();
    assert_eq!(code(lend), Some(ErrorCode::Unavailable), "no renderer");

    let no_store = Harness::without(mock_doc(), Some(Missing::Store));
    let stored = no_store.with_job(|job| store_png(job, &png())).err();
    assert_eq!(code(stored), Some(ErrorCode::Unavailable), "no store");
}

#[test]
fn a_stored_png_is_named_by_its_hash() {
    let harness = Harness::new(mock_doc());
    let bytes = png();
    let stored = harness.with_job(|job| store_png(job, &bytes)).expect("the PNG is stored");
    assert_eq!(stored.uri, format!("omuzaki://render/{}.png", stored.hash));
    assert_eq!(harness.store.get(&stored.hash).expect("the store holds it"), bytes);
    assert_eq!(harness.store.provenance(&stored.hash).expect("the store has its provenance"), render_provenance());
}

#[test]
fn every_light_rig_loads_and_an_unknown_one_is_refused() {
    for name in LightRig::names() {
        still_light(name).unwrap_or_else(|e| panic!("the light rig '{name}' should load: {e}"));
    }
    assert!(still_light(DEFAULT_LIGHT_RIG).is_ok(), "the default rig is one of them");
    assert_eq!(code(still_light("no-such-rig").err()), Some(ErrorCode::Invalid));
}

#[test]
fn with_renderer_lends_the_injected_renderer() {
    let harness = Harness::new(mock_doc());
    let targets = harness.with_job(|job| {
        with_renderer(job, |renderer: &mut dyn Renderer| {
            renderer
                .create_target(&omuzaki_render_api::TargetDesc {
                    size: UVec2::splat(16),
                    alpha: AlphaEncoding::Straight,
                })
                .map_err(|source| StageError::Render { what: "a test target".to_owned(), source })?;
            Ok(renderer.stats().targets)
        })
    });
    assert_eq!(targets.expect("the renderer is lent"), 1, "the closure works on the injected renderer");
}

#[test]
fn a_poisoned_renderer_lock_is_an_internal_error() {
    let harness = Harness::new(mock_doc());
    let renderer = harness.renderer.clone();
    // A render that panics while it holds the renderer poisons the lock; the next job must see that, not a panic.
    let poisoner = std::thread::spawn(move || {
        let _held = renderer.lock().expect("not yet poisoned");
        panic!("an earlier render failed");
    });
    assert!(poisoner.join().is_err(), "the thread panicked, so the lock is poisoned");

    let lend = harness.with_job(|job| with_renderer(job, |_| Ok(()))).err();
    assert_eq!(code(lend), Some(ErrorCode::Internal));
}

/// A job op that stores one PNG and answers with its name, to run the harness end to end.
#[derive(Serialize, Deserialize, JsonSchema)]
struct StorePng;

impl Op for StorePng {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.store_png",
            summary: "Store a test PNG and answer with its name",
            kind: OpKind::Job,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let stored = store_png(job, &png())?;
        Ok(json!({ "uri": stored.uri }))
    }
}

#[test]
fn the_harness_runs_a_job_op() {
    let harness = Harness::new(mock_doc());
    let (_plan, value) = harness.run(&StorePng).expect("the job runs");
    let uri = value["uri"].as_str().expect("the answer names the PNG");
    assert!(uri.starts_with("omuzaki://render/"), "{uri}");
}
