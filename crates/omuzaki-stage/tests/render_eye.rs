//! `render.eye`: a close-up of a character's photoreal 3D eye, stored as a PNG, so an agent sees the eye it designs
//! with `face.set_style`. It renders a face with a 3D eye of its own and one without (the design one starts from),
//! changes nothing in the document, gives back every renderer resource it made, and refuses what it can't render.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::eye3d::EyeDesign;
use omuzaki_ops_api::{ErrorCode, Op, OpError, Undo};
use omuzaki_stage::ops::render::eye::{EyeCloseup, EyeCloseupResult};
use omuzaki_store_api::Store;
use serde_json::{Value, json};

use support::{Harness, TestDoc, mock_doc};

fn closeup(args: Value) -> EyeCloseup {
    serde_json::from_value(args).expect("the arguments read")
}

fn run(harness: &Harness, args: Value) -> Result<EyeCloseupResult, OpError> {
    let (plan, value) = harness.run(&closeup(args))?;
    assert!(plan.patch.is_empty(), "a close-up changes nothing in the document");
    assert!(plan.cost.is_some(), "a close-up is costed before it runs");
    Ok(serde_json::from_value(value).expect("the result reads"))
}

/// The mock character with a face whose 3D eye has blue irises and fewer lashes, as an agent would set them.
fn with_eye() -> Harness {
    let mut eye = EyeDesign::default();
    eye.iris.outer = "#3a6a9a".parse().unwrap();
    eye.lashes.upper = 180;
    with_face(json!({ "eye3d": eye }))
}

/// The mock character with `face` as its face.
fn with_face(face: Value) -> Harness {
    let TestDoc(doc) = mock_doc();
    let mut root = doc.root().clone();
    root["characters"]["mock"]["face"] = face;
    Harness::new(TestDoc(MemDoc::from_value(root)))
}

#[test]
fn a_face_with_a_3d_eye_and_one_without_both_render_as_stored_pngs() {
    for (harness, in_use) in [(Harness::new(mock_doc()), false), (with_eye(), true)] {
        for view in ["macro", "front"] {
            let before = harness.renderer.lock().unwrap().stats();
            let args = json!({ "character": "mock", "view": view, "size": [96, 64], "quality": "live" });
            let result = run(&harness, args).unwrap_or_else(|e| panic!("{view}: {e}"));
            assert_eq!(result.in_use, in_use, "{view}");
            assert_eq!(result.size, [96, 64]);
            assert_eq!(result.uri, format!("omuzaki://render/{}.png", result.hash));
            let png = harness.store.get(&result.hash.parse().expect("a hash")).expect("the PNG is in the store");
            let image = image::load_from_memory(&png).expect("a PNG");
            assert_eq!((image.width(), image.height()), (96, 64), "{view}");
            let after = harness.renderer.lock().unwrap().stats();
            let counts = |s: omuzaki_render_api::ResourceStats| (s.meshes, s.textures, s.materials, s.environments);
            assert_eq!(counts(after), counts(before), "{view}: every resource the close-up made is given back");
        }
    }
}

#[test]
fn a_close_up_refuses_what_it_cannot_find_or_size() {
    let harness = Harness::new(mock_doc());
    let code = |args: Value| run(&harness, args).expect_err("refused").code;
    assert_eq!(code(json!({ "character": "nobody" })), ErrorCode::NotFound);
    assert_eq!(code(json!({ "character": "mock", "size": [8, 8] })), ErrorCode::Invalid);
    let unknown = serde_json::from_value::<EyeCloseup>(json!({ "character": "mock", "view": "side" }));
    assert!(unknown.is_err(), "a view it doesn't have is refused as it is read");

    let broken = with_face(json!({ "eye3d": { "ball": 3 } }));
    let error = run(&broken, json!({ "character": "mock" })).expect_err("a face that doesn't read");
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.to_string().contains("doesn't read"), "{error}");
}

#[test]
fn the_op_is_a_costed_render_job_that_undoes_nothing() {
    let meta = EyeCloseup::meta();
    assert_eq!(meta.name, "render.eye");
    assert_eq!(meta.undo, Undo::Auto);
    assert!(meta.summary.contains("face.set_style"), "the summary says how to design the eye");
}
