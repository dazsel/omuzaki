//! The sculpted 3D nose, lips and ear through the face ops, as an agent drives them over MCP: `face.set_style` adds one
//! to a face by setting one field and edits any field by path, `face.describe_style` lists its schema, values and
//! ranges, every range it lists is the one the edit enforces, and every design inside those ranges sculpts a closed,
//! finite surface.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::glam::Vec3;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::feature::{Paint, ear, lips, nose, skin_mesh};
use omuzaki_face_api::sculpt::{EarDesign, LipsDesign, NoseDesign};
use omuzaki_face_api::{FaceProfile, Validate};
use omuzaki_ops_api::{Actor, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

/// Runs the registered op `name` with `args` against `doc` and returns its plan (a dry run: nothing is written).
fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    omuzaki_face::link(); // the ops under test register in omuzaki-face; this links them, as Apps do (ADR 0015)
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

/// A document holding the character `mira` with a default face, which has no sculpted features.
fn doc() -> MemDoc {
    let face = serde_json::to_value(FaceProfile::default()).expect("the default face serialises");
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }))
}

fn set(field: &str, value: Value) -> Value {
    let mut args = json!({ "character": "mira", "path": field });
    args["value"] = value;
    args
}

fn face_of(doc: &MemDoc) -> FaceProfile {
    let path = DocPath::parse("characters/mira/face").expect("a valid path");
    serde_json::from_value(doc.get(&path).cloned().expect("a face")).expect("still a face")
}

/// The `section` entry of `face.describe_style`.
fn describe(doc: &MemDoc, section: &str) -> Value {
    let plan = run(doc, "face.describe_style", json!({ "character": "mira", "section": section })).expect("describes");
    plan.output.expect("a query answers")["sections"][section].clone()
}

const SECTIONS: [&str; 3] = ["nose3d", "lips3d", "ear3d"];

#[test]
fn an_agent_adds_each_feature_by_setting_one_field_and_undo_takes_it_away() {
    let mut doc = doc();
    let before = doc.root().clone();
    for section in SECTIONS {
        let entry = describe(&doc, section);
        assert_eq!(entry["in_use"], json!(false), "a default face has no {section}");
        assert_eq!(entry["current"], entry["defaults"], "without one, current is what it starts from");
    }

    let nose = run(&doc, "face.set_style", set("face/nose3d/upturn", json!(15))).expect("plans");
    doc.apply(&nose.patch).expect("applies");
    let lips = run(&doc, "face.set_style", set("lips3d", json!({ "style": 1.0, "width": 0.02 }))).expect("plans");
    doc.apply(&lips.patch).expect("applies");
    let ear = run(&doc, "face.set_style", set("face/ear3d/tone", json!("#f6dccf"))).expect("plans");
    doc.apply(&ear.patch).expect("applies");

    let face = face_of(&doc);
    let n = face.nose3d.expect("the face now has a sculpted nose");
    assert!((n.upturn - 15.0).abs() < 1e-6);
    assert!((n.length - NoseDesign::default().length).abs() < 1e-9, "every other field keeps its default");
    let l = face.lips3d.expect("sculpted lips");
    assert!((l.style - 1.0).abs() < 1e-6 && (l.width - 0.02).abs() < 1e-6);
    assert!((l.upper - LipsDesign::default().upper).abs() < 1e-9);
    let e = face.ear3d.expect("a sculpted ear");
    assert_eq!(e.tone.to_string(), "#f6dccf");
    assert!((e.height - EarDesign::default().height).abs() < 1e-9);
    assert!(face_of(&doc).validate().is_ok());
    assert_eq!(describe(&doc, "ear3d")["in_use"], json!(true));

    for plan in [ear, lips, nose] {
        doc.apply(&plan.patch.inverse()).expect("undoes");
    }
    assert_eq!(doc.root(), &before, "undo leaves the face as it was");
}

#[test]
fn every_range_describe_lists_is_the_one_set_style_enforces() {
    let doc = doc();
    for (section, fields) in [("nose3d", 11), ("lips3d", 8), ("ear3d", 7)] {
        let entry = describe(&doc, section);
        let ranges = entry["ranges"].as_array().expect("a list of ranges");
        assert_eq!(ranges.len(), fields, "every numeric field of {section} has a range");
        for range in ranges {
            let (path, lo, hi) =
                (range["path"].as_str().expect("path"), range["lo"].as_f64().unwrap(), range["hi"].as_f64().unwrap());
            assert!(path.starts_with(&format!("face/{section}/")), "{path}");
            for end in [lo, hi] {
                let sets = run(&doc, "face.set_style", set(path, json!(end)));
                assert!(sets.is_ok(), "{path} refuses its own end {end}: {sets:?}");
            }
            let step = (hi - lo) * 1e-3;
            for outside in [lo - step, hi + step] {
                let err = run(&doc, "face.set_style", set(path, json!(outside))).unwrap_err();
                assert!(err.to_string().contains(path), "{path} = {outside}: {err}");
            }
        }
    }
    assert_eq!(doc.root(), self::doc().root(), "a dry run writes nothing");
}

/// Fails unless `mesh` has triangles and every position and normal is finite.
fn assert_sculpted(mesh: &omuzaki_core::mesh::MeshData, at: &str) {
    assert!(mesh.indices.len() > 300, "{at} sculpts almost nothing: {} indices", mesh.indices.len());
    assert!(mesh.positions.iter().chain(&mesh.normals).all(|p| p.is_finite()), "{at} is not finite");
}

#[test]
fn every_design_inside_the_ranges_sculpts_a_finite_surface() {
    // Each numeric field at each end of its range (the rest at their defaults), set through the op as an agent would,
    // then sculpted and meshed coarsely: the ranges promise a feature, not only a valid file.
    let doc = doc();
    let white = |_: Vec3| Vec3::ONE;
    let none = |_: Vec3| 0.0;
    let paint = Paint { colour: &white, wet: &none, flush: &none };
    let cell = 0.0015;
    let mut built = 0;
    for section in SECTIONS {
        for range in describe(&doc, section)["ranges"].as_array().expect("a list of ranges") {
            let path = range["path"].as_str().expect("path");
            for end in [range["lo"].as_f64().unwrap(), range["hi"].as_f64().unwrap()] {
                let plan = run(&doc, "face.set_style", set(path, json!(end))).expect("an end is accepted");
                let mut edited = doc.clone();
                edited.apply(&plan.patch).expect("applies");
                let face = face_of(&edited);
                let at = format!("{path} = {end}");
                let mesh = match section {
                    "nose3d" => {
                        let d = face.nose3d.expect("a nose");
                        let (min, max) = nose::bounds(&d);
                        skin_mesh(&nose::field(&d), min, max, cell, &paint)
                    }
                    "lips3d" => {
                        let d = face.lips3d.expect("lips");
                        let (min, max) = lips::bounds(&d);
                        skin_mesh(&lips::field(&d), min, max, cell, &paint)
                    }
                    _ => {
                        let d = face.ear3d.expect("an ear");
                        let (min, max) = ear::bounds(&d);
                        skin_mesh(&ear::field(&d), min, max, cell, &paint)
                    }
                };
                assert_sculpted(&mesh, &at);
                built += 1;
            }
        }
    }
    assert_eq!(built, 2 * (11 + 8 + 7));
}
