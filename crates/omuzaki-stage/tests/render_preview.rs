//! `render.preview` (spec v3 §11, #336): one framed, lit picture of any character of the project, stored as a PNG. A
//! kit character and the same character imported from its own VRM file both render through the one op, so an agent
//! sees an imported character exactly as it sees one it built. No third-party model.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "avatar/kit.rs"]
mod kit;
#[path = "render/support.rs"]
mod support;

use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, Op};
use omuzaki_stage::ops::render::preview::{Preview, PreviewResult};
use omuzaki_store_api::{ContentHash, Store};
use serde_json::{Value, json};

use support::{Harness, TestDoc, mock_doc};

/// The mock character at `characters/mock`, and its own VRM export imported at `characters/mock_vrm` with the
/// original in the store.
fn harness() -> Harness {
    let bytes = kit::vrm_bytes(&kit::kit());
    let def = AvatarDef::new(AvatarSource::new(ContentHash::of(&bytes), SourceFormat::Vrm1, "mock.vrm"));
    let TestDoc(doc) = mock_doc();
    let mut root = doc.root().clone();
    root["characters"]["mock_vrm"] = json!({ "avatar": serde_json::to_value(&def).unwrap() });
    let harness = Harness::new(TestDoc(MemDoc::from_value(root)));
    let stored = harness.store.put(&bytes, kit::provenance()).expect("stored");
    assert_eq!(stored, def.source.hash, "the store is content-addressed");
    harness
}

fn preview(args: Value) -> Preview {
    serde_json::from_value(args).expect("the arguments read")
}

fn run(harness: &Harness, args: Value) -> Result<PreviewResult, omuzaki_ops_api::OpError> {
    let (plan, value) = harness.run(&preview(args))?;
    assert!(plan.patch.is_empty(), "a preview changes nothing in the document");
    assert!(plan.cost.is_some(), "a preview is costed before it runs");
    Ok(serde_json::from_value(value).expect("the result reads"))
}

#[test]
fn a_built_and_an_imported_character_both_preview_as_stored_pngs() {
    let harness = harness();
    for (id, body) in [("mock", "kit"), ("mock_vrm", "imported")] {
        // The mock has no eye bones, so no face shot.
        for framing in ["full", "bust"] {
            let args = json!({ "character": id, "framing": framing, "size": [64, 96], "quality": "live" });
            let result = run(&harness, args).unwrap_or_else(|e| panic!("{id} {framing}: {e}"));
            assert_eq!(result.body, body);
            assert_eq!(result.size, [64, 96]);
            assert!(result.uri.starts_with("omuzaki://render/") && result.uri.ends_with(".png"), "{}", result.uri);
            let png = harness.store.get(&result.hash.parse().expect("a hash")).expect("the PNG is in the store");
            let image = image::load_from_memory(&png).expect("a PNG");
            assert_eq!((image.width(), image.height()), (64, 96), "{id} {framing}");
        }
    }
}

#[test]
fn a_preview_refuses_what_it_cannot_frame_or_find() {
    let harness = harness();
    let code = |args: Value| run(&harness, args).expect_err("refused").code;
    assert_eq!(code(json!({ "character": "nobody" })), ErrorCode::NotFound);
    assert_eq!(code(json!({ "character": "mock", "framing": "free" })), ErrorCode::Invalid);
    assert_eq!(code(json!({ "character": "mock", "size": [8, 8] })), ErrorCode::Invalid);
    assert_eq!(code(json!({ "character": "mock", "light_rig": "no-such-rig" })), ErrorCode::Invalid);
    assert_eq!(code(json!({ "character": "mock_vrm", "variant": "game" })), ErrorCode::Invalid);
}

#[test]
fn the_op_is_a_costed_render_job_that_undoes_nothing() {
    let meta = Preview::meta();
    assert_eq!(meta.name, "render.preview");
    assert_eq!(meta.kind, omuzaki_ops_api::OpKind::Job);
    assert_eq!(meta.scope, omuzaki_ops_api::Scope::Render);
    assert!(omuzaki_ops_api::find("render.preview").is_some(), "registered, so MCP and the CLI list it");
}
