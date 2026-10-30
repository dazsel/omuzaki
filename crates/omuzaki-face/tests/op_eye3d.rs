//! The photoreal 3D eye through the face ops, as an agent drives it over MCP: `face.set_style` adds it to a face by
//! setting one field and edits any field by path, `face.describe_style` lists its schema, values and ranges, every range
//! it lists is the one the edit enforces, and every design inside those ranges builds an eye (meshes and textures)
//! with nothing but finite numbers in it.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::glam::Vec3;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::eye3d::{self, EyeDesign};
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

/// A document holding the character `mira` with a default face, which has painted eyes only.
fn doc() -> MemDoc {
    let face = serde_json::to_value(FaceProfile::default()).expect("the default face serialises");
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }))
}

fn set(field: &str, value: Value) -> Value {
    let mut args = json!({ "character": "mira", "path": field });
    args["value"] = value;
    args
}

/// `value` as an agent writes it: a whole number as one (a count reads only a whole number), anything else as it is.
fn number(value: f64) -> Value {
    #[allow(clippy::cast_possible_truncation, reason = "a whole number from a range's end")]
    if value.fract() == 0.0 && value.abs() < 1e9 { json!(value as i64) } else { json!(value) }
}

fn face_of(doc: &MemDoc) -> FaceProfile {
    let path = DocPath::parse("characters/mira/face").expect("a valid path");
    serde_json::from_value(doc.get(&path).cloned().expect("a face")).expect("still a face")
}

/// The `eye3d` entry of `face.describe_style`.
fn describe(doc: &MemDoc) -> Value {
    let plan = run(doc, "face.describe_style", json!({ "character": "mira", "section": "eye3d" })).expect("describes");
    plan.output.expect("a query answers")["sections"]["eye3d"].clone()
}

#[test]
fn an_agent_adds_a_3d_eye_by_setting_one_field_and_undo_takes_it_away() {
    let mut doc = doc();
    let before = doc.root().clone();
    let entry = describe(&doc);
    assert_eq!(entry["in_use"], json!(false), "a default face has painted eyes only");
    assert_eq!(entry["current"], entry["defaults"], "without one, current is what it starts from");

    let plan = run(&doc, "face.set_style", set("face/eye3d/iris/outer", json!("#3a6a9a"))).expect("plans");
    doc.apply(&plan.patch).expect("applies");
    let eye = face_of(&doc).eye3d.expect("the face now has a 3D eye");
    assert_eq!(eye.iris.outer.to_string(), "#3a6a9a");
    assert_eq!(eye.iris.inner, EyeDesign::default().iris.inner, "every other field keeps its default");
    assert_eq!(eye.ball, EyeDesign::default().ball);

    let entry = describe(&doc);
    assert_eq!(entry["in_use"], json!(true));
    assert_eq!(entry["current"]["iris"]["outer"], json!("#3a6a9a"));

    doc.apply(&plan.patch.inverse()).expect("undoes");
    assert_eq!(doc.root(), &before, "undo leaves the face as it was, painted eyes only");
}

#[test]
fn an_agent_sets_several_fields_at_once_and_the_rest_stay() {
    let mut doc = doc();
    let first = run(&doc, "face.set_style", set("face/eye3d/lashes/upper", json!(220))).expect("plans");
    doc.apply(&first.patch).expect("applies");
    let several = json!({ "pupil": 0.3, "iris_radius": 0.0062 });
    let plan = run(&doc, "face.set_style", set("eye3d/ball", several)).expect("the leading face/ may be left out");
    doc.apply(&plan.patch).expect("applies");
    let eye = face_of(&doc).eye3d.expect("a 3D eye");
    assert!((eye.ball.pupil - 0.3).abs() < 1e-6 && (eye.ball.iris_radius - 0.0062).abs() < 1e-6);
    assert!((eye.ball.radius - EyeDesign::default().ball.radius).abs() < 1e-9, "a field it doesn't name stays");
    assert_eq!(eye.lashes.upper, 220, "an earlier edit stays");
    assert!(face_of(&doc).validate().is_ok());
}

#[test]
fn a_wrong_value_is_refused_with_its_path_its_range_or_the_nearest_key() {
    let doc = doc();
    let out = run(&doc, "face.set_style", set("face/eye3d/ball/pupil", json!(2.0))).unwrap_err();
    assert!(out.to_string().contains("face/eye3d/ball/pupil") && out.to_string().contains("0.05 to 0.8"), "{out}");

    let typo = run(&doc, "face.set_style", set("face/eye3d/iris/fibers", json!(0.5))).unwrap_err();
    let hint = typo.hint.as_deref().unwrap_or_default();
    assert!(hint.contains("did you mean `face/eye3d/iris/fibres`"), "the nearest key is suggested: {hint}");

    let kind = run(&doc, "face.set_style", set("face/eye3d/lashes/upper", json!("many"))).unwrap_err();
    assert!(kind.to_string().contains("face/eye3d/lashes/upper"), "{kind}");

    let rule = run(&doc, "face.set_style", set("face/eye3d/ball", json!({ "radius": 0.008, "iris_radius": 0.0075 })))
        .unwrap_err();
    assert!(rule.to_string().contains("0.7 of the eyeball"), "a rule between fields is named: {rule}");
    assert_eq!(doc.root(), self::doc().root(), "a refused edit writes nothing");
}

#[test]
fn every_range_describe_lists_is_the_one_set_style_enforces() {
    let doc = doc();
    let entry = describe(&doc);
    let ranges = entry["ranges"].as_array().expect("a list of ranges");
    assert_eq!(ranges.len(), 37, "every numeric field of the eye has a range");
    for range in ranges {
        let (path, lo, hi) =
            (range["path"].as_str().expect("path"), range["lo"].as_f64().unwrap(), range["hi"].as_f64().unwrap());
        assert!(path.starts_with("face/eye3d/"), "{path}");
        let step = (hi - lo) * 1e-3;
        // A field with a rule between fields may fail that rule at an end; only its outside is checked.
        let ends: &[f64] = if range.get("note").is_some() { &[] } else { &[lo, hi] };
        for end in ends {
            let sets = run(&doc, "face.set_style", set(path, number(*end)));
            assert!(sets.is_ok(), "{path} refuses its own end {end}: {sets:?}");
        }
        for outside in [lo - step, hi + step] {
            let err = run(&doc, "face.set_style", set(path, json!(outside))).unwrap_err();
            assert!(err.to_string().contains(path), "{path} = {outside}: {err}");
        }
    }
}

/// Fails unless every position, normal and texture coordinate of `mesh` is finite and it has triangles.
fn assert_finite(name: &str, mesh: &omuzaki_core::mesh::MeshData, at: &str) {
    assert!(!mesh.indices.is_empty() || mesh.positions.is_empty(), "{name} has no triangles at {at}");
    assert!(mesh.positions.iter().chain(&mesh.normals).all(|p| p.is_finite()), "{name} is not finite at {at}");
    assert!(mesh.uvs.iter().all(|uv| uv.is_finite()), "{name}'s texture coordinates are not finite at {at}");
}

#[test]
fn every_design_inside_the_ranges_builds_a_finite_eye() {
    // Each numeric field at each end of its range (with the rest at their defaults), set through the op as an agent
    // would, then built (meshes, the eye's atlas and the skin): the ranges promise an eye, not only a valid file.
    let doc = doc();
    let entry = describe(&doc);
    let (view, gaze) = (Vec3::new(0.024, -0.008, 0.056), Vec3::new(0.16, 0.12, 1.0));
    let mut built = 0;
    for range in entry["ranges"].as_array().expect("a list of ranges") {
        let path = range["path"].as_str().expect("path");
        for end in [range["lo"].as_f64().unwrap(), range["hi"].as_f64().unwrap()] {
            let Ok(plan) = run(&doc, "face.set_style", set(path, number(end))) else {
                // An end that breaks a rule between fields is refused, as it should be; the test above covers it.
                continue;
            };
            let mut edited = doc.clone();
            edited.apply(&plan.patch).expect("applies");
            let design = face_of(&edited).eye3d.expect("a 3D eye");
            let at = format!("{path} = {end}");
            let meshes = eye3d::meshes(&design, view, gaze);
            assert_finite("the eyeball", &meshes.eyeball, &at);
            assert_finite("the lids", &meshes.region, &at);
            assert_finite("the shell", &meshes.shell, &at);
            assert_finite("the lashes", &meshes.lashes, &at);
            assert!(!meshes.eyeball.positions.is_empty() && !meshes.region.positions.is_empty(), "{at}");
            let atlas = eye3d::textures::atlas(&design, 32);
            assert_eq!(atlas.rgba.len(), (atlas.width * atlas.height * 4) as usize, "{at}");
            eye3d::skin::texture(&design, 64);
            built += 1;
        }
    }
    assert!(built > 55, "only {built} designs built");
}
