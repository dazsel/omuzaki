//! The feature-layout ops (CB-12) against the in-memory document: `face.layout.measure` reads the layout as ratios,
//! `face.set_layout` sets one field, and `face.set_layout_ratios` sets the whole layout from ratios.
//!
//! The editing contract (dry-run, apply, undo, redo, no `null` written) is checked by the shared harness in
//! `common`, through its `round_trip`. The head's chin comes from a head part the test gives its own chin. The layout
//! numbers are `FaceLayout`'s neutral defaults and synthetic values; no real character's numbers belong in this file.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss)] // the random draws map a 53-bit integer to [0, 1)

mod common;

use common::{assert_error, at, mock_node, plan, refused, round_trip};
use omuzaki_char::ops::layout::{MeasureLayout, SetLayout, SetLayoutRatios};
use omuzaki_core::rng::Rng;
use omuzaki_doc_api::{DocPath, DocRead, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::{FaceLayout, Validate};
use omuzaki_ops_api::{ErrorCode, Op, OpKind, OpMeta, Scope, Undo, find};
use serde_json::{Value, json};

/// The head's chin in head-local height. The default layout's ratios read against it: nose 0.156.
const CHIN: f64 = -0.25;

/// The three layout ops.
const OPS: [&str; 3] = ["face.layout.measure", "face.set_layout", "face.set_layout_ratios"];

// each op registers from its own module's object file, and a test binary keeps that file only if it references a
// symbol in it. Naming the three op types here pulls the modules in, so `find` sees every layout op. Remove once the layout registrations survive linking.
#[used]
static LINKED_OPS: [fn() -> OpMeta; 3] = [MeasureLayout::meta, SetLayout::meta, SetLayoutRatios::meta];

// ------------------------------------------------------------------------------------------------ the layout documents

/// Plans a query or an edit that must change nothing, and returns its output.
fn run(doc: &MemDoc, op: &str, args: Value) -> Value {
    let planned = plan(doc, op, args).unwrap_or_else(|e| panic!("{op} was refused: {e:?}"));
    assert!(planned.patch.is_empty(), "{op}: planning must not produce a change here");
    planned.output.expect("the op answers")
}

/// The mock character as `mock` (from the shared fixture), with its head part of kind `head` whose chin is `chin`, and
/// with `face` holding `layout` (absent: the character has no face yet).
fn node(chin: f64, layout: Option<FaceLayout>) -> Value {
    let mut node = mock_node();
    node["parts"]["head"]["kind"] = json!("head");
    node["parts"]["head"]["params"] = json!({ "outline": { "chin": { "y": chin } } });
    if let Some(layout) = layout {
        node["face"] = json!({ "layout": layout });
    }
    node
}

fn project_of(node: &Value) -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

/// A project with the mock character, its head's chin at [`CHIN`] and `layout` as its face layout.
fn project(layout: Option<FaceLayout>) -> MemDoc {
    project_of(&node(CHIN, layout))
}

/// The layout the document holds for `mock`, with the defaults for the fields it leaves out.
fn stored_layout(doc: &MemDoc) -> FaceLayout {
    let stored = doc.get(&DocPath::parse("characters/mock/face/layout").unwrap()).cloned();
    serde_json::from_value(stored.unwrap_or_else(|| json!({}))).unwrap()
}

/// `layout` as its fields and values, so two layouts compare field by field.
fn fields(layout: &FaceLayout) -> Vec<(String, f64)> {
    let node = serde_json::to_value(layout).unwrap();
    node.as_object().unwrap().iter().map(|(name, value)| (name.clone(), value.as_f64().unwrap())).collect()
}

#[track_caller]
fn assert_layouts_close(got: &FaceLayout, want: &FaceLayout, tolerance: f64) {
    for ((name, g), (_, w)) in fields(got).into_iter().zip(fields(want)) {
        assert!((g - w).abs() <= tolerance, "{name}: {g} is not {w} within {tolerance}");
    }
}

fn changed_paths(patch: &Patch) -> Vec<String> {
    patch.changes.iter().map(|c| c.path.to_string()).collect()
}

// ------------------------------------------------------------------------------------------------ the ops as an agent sees them

#[test]
fn every_layout_op_is_registered_with_its_kind_and_a_closed_documented_schema() {
    for name in OPS {
        let registration = find(name).unwrap_or_else(|| panic!("{name} isn't registered"));
        let meta = (registration.meta)();
        let expected = if name == "face.layout.measure" {
            (OpKind::Query, Scope::Read, Undo::None)
        } else {
            (OpKind::Edit, Scope::EditCharacter, Undo::Auto)
        };
        assert_eq!((meta.kind, meta.scope, meta.undo), expected, "{name}");

        let schema = serde_json::to_value((registration.schema)()).unwrap();
        assert_eq!(schema["additionalProperties"], json!(false), "{name}: a typo'd argument must be refused");
        assert!(!schema.to_string().contains("$ref"), "{name}: MCP schemas are inlined");
        assert!(schema["description"].as_str().unwrap().len() > 100, "{name}: the struct docs are the agent's manual");
        for (field, property) in schema["properties"].as_object().unwrap() {
            assert!(property["description"].is_string(), "{name}.{field} has no description");
        }
    }
}

#[test]
fn a_misspelt_argument_is_refused_before_anything_is_planned() {
    let doc = project(Some(FaceLayout::default()));
    let typo = refused(&doc, "face.set_layout", json!({"character": "mock", "field": "eye_y", "val": 0.1}));
    assert_eq!(typo.code, ErrorCode::Invalid);
    assert!(typo.message.contains("unknown field `val`"), "{}", typo.message);
}

// ------------------------------------------------------------------------------------------------ face.layout.measure

#[test]
fn measure_reads_the_default_layout_as_the_overlay_ratios() {
    let doc = project(Some(FaceLayout::default()));
    let out = run(&doc, "face.layout.measure", json!({"character": "mock"}));

    let base = FaceLayout::default().ratios(CHIN).unwrap();
    let ratios = &out["ratios"];
    assert_eq!(ratios["eye_width_to_iod"].as_f64(), Some(base.eye_width_to_iod));
    assert_eq!(ratios["drop_to_iod"].as_f64(), Some(base.drop_to_iod));
    assert_eq!(ratios["nose"].as_f64(), Some(base.nose));
    assert_eq!(ratios["mouth"].as_f64(), Some(base.mouth));
    assert!((base.nose - 0.156).abs() < 1e-3, "the prototype's nose ratio: {}", base.nose);
    assert_eq!(out["chin_y"].as_f64(), Some(CHIN));
    assert!(out.get("reference").is_none() && out.get("difference").is_none(), "no reference, no comparison");
    assert!(!out.to_string().contains("null"), "the answer never carries a null: {out}");
}

#[test]
fn measure_beside_a_reference_gives_the_difference_and_plain_notes() {
    let doc = project(Some(FaceLayout::default()));
    let mine = ratios_of(&FaceLayout::default());
    let x = |field: &str| mine[field].as_f64().unwrap();
    // The reference's mouth sits 0.06 higher on the face (a smaller fraction), so ours is 0.06 lower.
    // Its eyes are 0.031 larger than ours, and its nose is ours.
    let reference = json!({
        "eye_width_to_iod": x("eye_width_to_iod") + 0.031,
        "drop_to_iod": x("drop_to_iod"),
        "nose": x("nose"),
        "mouth": x("mouth") - 0.06,
        "blush": x("blush"),
        "blush_spacing_to_iod": x("blush_spacing_to_iod"),
    });
    let out = run(&doc, "face.layout.measure", json!({"character": "mock", "reference": reference}));

    assert!((out["difference"]["mouth"].as_f64().unwrap() - 0.06).abs() < 1e-12, "{out}");
    let notes: Vec<&str> = out["notes"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
    assert_eq!(notes.len(), 6, "one note per feature: {notes:?}");
    assert_eq!(notes[0], "the eye size is 0.03 smaller than the reference (in eye distances)");
    assert_eq!(notes[2], "the nose matches the reference");
    assert_eq!(notes[3], "the mouth is 0.06 lower than the reference");
    assert!(!out.to_string().contains("null"), "{out}");
}

#[test]
fn a_partial_reference_compares_only_the_ratios_it_gives() {
    let doc = project(Some(FaceLayout::default()));
    let mine = ratios_of(&FaceLayout::default());
    let x = |field: &str| mine[field].as_f64().unwrap();
    let reference = json!({ "drop_to_iod": x("drop_to_iod") - 0.2, "mouth": x("mouth") + 0.05 });
    let out = run(&doc, "face.layout.measure", json!({"character": "mock", "reference": reference}));
    let difference = out["difference"].as_object().unwrap();
    assert_eq!(difference.len(), 2, "two fields given, two compared: {out}");
    assert!((difference["drop_to_iod"].as_f64().unwrap() - 0.2).abs() < 1e-12, "{out}");
    assert!((difference["mouth"].as_f64().unwrap() + 0.05).abs() < 1e-12, "{out}");
    let notes: Vec<&str> = out["notes"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
    assert_eq!(
        notes,
        [
            "the eye-to-chin distance is 0.20 longer than the reference (in eye distances)",
            "the mouth is 0.05 higher than the reference"
        ]
    );
    assert_eq!(out["reference"], reference, "the reference is echoed as given: {out}");
    assert!(!out.to_string().contains("null"), "{out}");
}

#[test]
fn an_empty_reference_is_refused_and_every_reference_field_is_optional() {
    let doc = project(Some(FaceLayout::default()));
    let e = refused(&doc, "face.layout.measure", json!({"character": "mock", "reference": {}}));
    assert_error(&e, ErrorCode::Invalid, "reference", &["no ratio"], &[]);
    let e = refused(&doc, "face.layout.measure", json!({"character": "mock", "reference": {"mouth": 1.5}}));
    assert_error(&e, ErrorCode::Invalid, "reference/mouth", &["0 to 1"], &[]);

    let schema = serde_json::to_value((find("face.layout.measure").unwrap().schema)()).unwrap();
    let reference = &schema["properties"]["reference"];
    let text = reference.to_string();
    for field in ["eye_width_to_iod", "drop_to_iod", "nose", "mouth", "blush", "blush_spacing_to_iod"] {
        assert!(text.contains(field), "the reference schema names {field}: {reference}");
    }
    let required = |v: &Value| v.get("required").and_then(Value::as_array).is_some_and(|r| !r.is_empty());
    let object = reference
        .get("anyOf")
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find(|v| v.get("properties").is_some()))
        .unwrap_or(reference);
    assert!(!required(object), "no reference field is required: {reference}");
}

/// The ratios `face.layout.measure` reports for `layout` on the test head.
fn ratios_of(layout: &FaceLayout) -> Value {
    run(&project(Some(*layout)), "face.layout.measure", json!({"character": "mock"}))["ratios"].clone()
}

#[test]
fn a_reference_outside_its_range_is_refused_by_name() {
    let doc = project(Some(FaceLayout::default()));
    let reference = json!({
        "eye_width_to_iod": 0.3, "drop_to_iod": 1.0, "nose": 1.5, "mouth": 0.4, "blush": 0.2, "blush_spacing_to_iod": 1.0,
    });
    let e = refused(&doc, "face.layout.measure", json!({"character": "mock", "reference": reference}));
    assert_error(&e, ErrorCode::Invalid, "reference/nose", &["0 to 1"], &[]);
}

#[test]
fn measure_needs_exactly_one_enabled_head_and_names_the_problem() {
    let mut none = node(CHIN, Some(FaceLayout::default()));
    none["parts"].as_object_mut().unwrap().remove("head");
    let e = refused(&project_of(&none), "face.layout.measure", json!({"character": "mock"}));
    assert_error(&e, ErrorCode::Invalid, "character", &["no enabled head part"], &[]);

    let mut two = node(CHIN, Some(FaceLayout::default()));
    two["parts"]["hair"]["kind"] = json!("head");
    let e = refused(&project_of(&two), "face.layout.measure", json!({"character": "mock"}));
    assert_error(&e, ErrorCode::Invalid, "character", &["2 enabled head parts"], &[]);
}

#[test]
fn a_chin_at_or_above_the_eye_line_has_no_ratios() {
    let high = FaceLayout { eye_y: -0.3, ..FaceLayout::default() };
    let e = refused(&project_of(&node(CHIN, Some(high))), "face.layout.measure", json!({"character": "mock"}));
    assert_error(&e, ErrorCode::Invalid, "face/layout/eye_y", &["the chin must be below the eye line"], &[]);
}

#[test]
fn measure_refuses_a_missing_character_by_name() {
    let doc = project(Some(FaceLayout::default()));
    let e = refused(&doc, "face.layout.measure", json!({"character": "nobody"}));
    assert_eq!(e.code, ErrorCode::NotFound);
    assert_eq!(e.field.as_deref(), Some("character"));
}

// ------------------------------------------------------------------------------------------------ face.set_layout

#[test]
fn set_layout_round_trips_and_changes_only_the_named_field() {
    let mut doc = project(Some(FaceLayout::default()));
    let planned =
        round_trip(&mut doc, "face.set_layout", json!({"character": "mock", "field": "eye_spacing", "value": 0.06}));
    assert_eq!(changed_paths(&planned.patch), ["characters/mock/face/layout/eye_spacing"]);
    assert_eq!(stored_layout(&doc).eye_spacing, 0.06);
}

#[test]
fn setting_a_field_to_its_current_value_changes_nothing() {
    let doc = project(Some(FaceLayout::default()));
    let out = run(&doc, "face.set_layout", json!({"character": "mock", "field": "eye_y", "value": -0.09}));
    assert_eq!(out["changed"], json!([]));
}

#[test]
fn a_bad_value_names_the_field_and_the_range_it_accepts() {
    let doc = project(Some(FaceLayout::default()));
    let e = refused(&doc, "face.set_layout", json!({"character": "mock", "field": "eye_spacing", "value": 0.5}));
    assert_error(&e, ErrorCode::Invalid, "face/layout/eye_spacing", &["0.02 to 0.2"], &[]);
}

#[test]
fn a_mouth_above_the_eye_line_is_refused_and_the_mouth_is_named() {
    let doc = project(Some(FaceLayout::default()));
    let e = refused(&doc, "face.set_layout", json!({"character": "mock", "field": "mouth_y", "value": -0.05}));
    assert_error(&e, ErrorCode::Invalid, "face/layout/mouth_y", &["the mouth must be below the eye line"], &[]);
}

#[test]
fn an_unknown_field_is_refused_and_the_fields_are_listed() {
    let doc = project(Some(FaceLayout::default()));
    let e = refused(&doc, "face.set_layout", json!({"character": "mock", "field": "eye_height", "value": 0.1}));
    assert_error(&e, ErrorCode::Invalid, "face/layout/eye_height", &["no field 'eye_height'"], &[]);
    assert!(e.hint.unwrap().contains("eye_y"), "the hint lists the fields");
}

#[test]
fn a_null_value_is_refused_and_never_written() {
    let doc = project(Some(FaceLayout::default()));
    assert!(plan(&doc, "face.set_layout", json!({"character": "mock", "field": "eye_y", "value": null})).is_err());
}

#[test]
fn a_layout_damaged_elsewhere_still_takes_an_unrelated_edit() {
    let damaged = json!({ "layout": { "eye_spacing": 0.5 } });
    let mut node = node(CHIN, None);
    node["face"] = damaged;
    let mut doc = project_of(&node);

    round_trip(&mut doc, "face.set_layout", json!({"character": "mock", "field": "eye_width", "value": 0.03}));
    assert_eq!(stored_layout(&doc).eye_width, 0.03);

    let e = refused(&doc, "face.set_layout", json!({"character": "mock", "field": "eye_spacing", "value": 0.7}));
    assert_error(&e, ErrorCode::Invalid, "face/layout/eye_spacing", &["0.02 to 0.2"], &[]);
}

#[test]
fn a_character_with_no_face_gets_the_face_and_its_layout_for_the_field() {
    let mut doc = project(None);
    round_trip(&mut doc, "face.set_layout", json!({"character": "mock", "field": "eye_width", "value": 0.03}));
    assert_eq!(at(&doc, "characters/mock/face"), Some(json!({ "layout": { "eye_width": 0.03 } })));
    assert_eq!(stored_layout(&doc).eye_width, 0.03);
    assert_eq!(stored_layout(&doc).eye_y, FaceLayout::default().eye_y, "the other fields read as defaults");
}

#[test]
fn a_face_without_a_layout_gets_the_layout_for_the_field() {
    let mut node = node(CHIN, None);
    node["face"] = json!({});
    let mut doc = project_of(&node);
    let planned =
        round_trip(&mut doc, "face.set_layout", json!({"character": "mock", "field": "eye_width", "value": 0.03}));
    assert_eq!(changed_paths(&planned.patch), ["characters/mock/face/layout"]);
}

// ------------------------------------------------------------------------------------------------ face.set_layout_ratios

#[test]
fn set_layout_ratios_inverts_measure_for_100_random_layouts() {
    let mut rng = Rng::new(464);
    for case in 0..100 {
        let x = random_layout(&mut rng);
        assert!(x.validate().is_ok(), "case {case}: the generator makes valid layouts: {x:?}");

        let ratios = ratios_of(&x);

        // Start from another layout with the same eye line and brows (the ratios do not carry the brows, so they stay as
        // the layout has them). The eye distance and every feature must come from the ratios alone.
        let start = FaceLayout {
            eye_y: x.eye_y,
            brow_gap: x.brow_gap,
            eye_spacing: 0.05,
            mouth_y: x.eye_y - 0.05,
            blush_y: x.eye_y - 0.02,
            ..FaceLayout::default()
        };
        let mut doc = project(Some(start));
        let planned = round_trip(&mut doc, "face.set_layout_ratios", json!({"character": "mock", "ratios": ratios}));
        assert!(changed_paths(&planned.patch).iter().all(|p| p.starts_with("characters/mock/face/layout/")));
        assert_layouts_close(&stored_layout(&doc), &x, 1e-9);
    }
}

/// A valid layout on the test head: the eye line above [`CHIN`], and every feature inside its range and below the eye line.
fn random_layout(rng: &mut Rng) -> FaceLayout {
    let eye_y = between(rng, -0.2, 0.2);
    FaceLayout {
        eye_y,
        eye_spacing: between(rng, 0.03, 0.15),
        eye_width: between(rng, 0.02, 0.1),
        brow_gap: between(rng, 0.0, 0.15),
        nose_y: between(rng, CHIN, eye_y),
        mouth_y: between(rng, CHIN, eye_y - 0.001),
        blush_y: between(rng, CHIN, eye_y),
        blush_spacing: between(rng, 0.0, 0.25),
    }
}

fn between(rng: &mut Rng, lo: f64, hi: f64) -> f64 {
    lo + (hi - lo) * unit(rng)
}

/// A number in [0, 1) from the top 53 bits of the generator.
fn unit(rng: &mut Rng) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1_u64 << 53) as f64
}

#[test]
fn set_layout_ratios_refuses_a_chin_at_or_above_the_eye_line() {
    let ratios = ratios_of(&FaceLayout::default());
    for eye_y in [-0.3, CHIN] {
        let doc = project(Some(FaceLayout { eye_y, ..FaceLayout::default() }));
        let e = refused(&doc, "face.set_layout_ratios", json!({"character": "mock", "ratios": ratios}));
        assert_error(&e, ErrorCode::Invalid, "face/layout/eye_y", &["the chin must be below the eye line"], &[]);
    }
}

#[test]
fn set_layout_ratios_names_each_bad_ratio_and_its_range() {
    let doc = project(Some(FaceLayout::default()));
    let bad = json!({
        "eye_width_to_iod": 0.3, "drop_to_iod": 0.0, "nose": 1.5, "mouth": 0.4, "blush": 0.2, "blush_spacing_to_iod": 0.5,
    });
    let e = refused(&doc, "face.set_layout_ratios", json!({"character": "mock", "ratios": bad}));
    assert_error(&e, ErrorCode::Invalid, "ratios/drop_to_iod", &["greater than 0"], &[]);
    assert!(e.message.contains("ratios/nose"), "every bad ratio is listed: {}", e.message);
}

#[test]
fn set_layout_ratios_with_the_same_ratios_changes_nothing() {
    let layout = FaceLayout::default();
    let ratios = ratios_of(&layout);
    let doc = project(Some(layout));
    let out = run(&doc, "face.set_layout_ratios", json!({"character": "mock", "ratios": ratios}));
    assert_eq!(out["changed"], json!([]));
}
