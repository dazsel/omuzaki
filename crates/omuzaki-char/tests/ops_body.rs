//! The body ops (`body.set_proportion`, `body.set_shape`, `body.apply_shape_preset`, `body.set_hand_style`,
//! `body.set_template`) against the in-memory document.
//!
//! The body part is the mock torso under the `body` kind the ops look for (registers the real one), and the garments
//! require it, as they do in a real character. Every edit goes through `round_trip`, which spells out the editing contract.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{assert_error, at, mock_node, plan, refused, round_trip};
use omuzaki_char::template::shape::preset;
use omuzaki_char_api::{
    BodyParams, BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput, register_part_kind,
};
use omuzaki_char_mock::{GarmentParams, MockBody, MockGarment};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, Op, OpKind, OpMeta, Scope, Undo, find};
use serde_json::json;

/// Takes the address of every body op's `meta`, so this test binary links the modules that register them: `inventory`
/// drops a module nothing references, and nothing else here calls into them.
#[used]
static LINKED_OPS: [fn() -> OpMeta; 5] = [
    <omuzaki_char::ops::body::set_proportion::SetProportion as Op>::meta,
    <omuzaki_char::ops::body::set_shape::SetShape as Op>::meta,
    <omuzaki_char::ops::body::apply_shape_preset::ApplyShapePreset as Op>::meta,
    <omuzaki_char::ops::body::set_hand_style::SetHandStyle as Op>::meta,
    <omuzaki_char::ops::body::set_template::SetTemplate as Op>::meta,
];

/// The stand-in `body` kind: the mock torso, under the id the body ops look for.
struct Body;

impl PartKind for Body {
    type Params = BodyParams;

    fn meta() -> PartMeta {
        PartMeta { id: "body", summary: "Stand-in: the mock torso.", category: PartCategory::Body, requires: &[] }
    }

    fn build(params: &BodyParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        <MockBody as PartKind>::build(params, ctx)
    }
}

/// The stand-in `garment` kind: the mock garment, requiring the `body` kind as the real garments do.
struct Garment;

impl PartKind for Garment {
    type Params = GarmentParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "garment",
            summary: "Stand-in: the mock garment.",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn build(params: &GarmentParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        <MockGarment as PartKind>::build(params, ctx)
    }
}

register_part_kind!(Body);
register_part_kind!(Garment);

/// The mock character with its body as the `body` kind and its garments as `garment`.
fn project() -> MemDoc {
    let mut node = mock_node();
    node["parts"]["body"]["kind"] = json!("body");
    node["parts"]["shirt"]["kind"] = json!("garment");
    node["parts"]["vest"]["kind"] = json!("garment");
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

/// The stored body parameters of the mock character.
const PARAMS: &str = "characters/mock/parts/body/params";

#[test]
fn every_body_op_is_registered_as_an_edit_on_the_character_with_auto_undo() {
    for name in
        ["body.set_proportion", "body.set_shape", "body.apply_shape_preset", "body.set_hand_style", "body.set_template"]
    {
        let registration = find(name).unwrap_or_else(|| panic!("{name} isn't registered"));
        let meta = (registration.meta)();
        assert_eq!((meta.kind, meta.scope, meta.undo), (OpKind::Edit, Scope::EditCharacter, Undo::Auto), "{name}");
    }
}

#[test]
fn set_proportion_round_trips_and_writes_only_the_slider_it_sets() {
    let mut doc = project();
    round_trip(&mut doc, "body.set_proportion", json!({ "character": "mock", "name": "leg_length", "value": 1.2 }));
    assert_eq!(at(&doc, &format!("{PARAMS}/proportions/leg_length")), Some(json!(1.2)));
    assert_eq!(
        at(&doc, &format!("{PARAMS}/proportions/neck_length")),
        None,
        "a default the edit leaves alone is not written"
    );
}

#[test]
fn set_proportion_refuses_a_value_outside_its_range_naming_the_field_and_range() {
    let doc = project();
    let e = refused(&doc, "body.set_proportion", json!({ "character": "mock", "name": "leg_length", "value": 3.0 }));
    assert_error(&e, ErrorCode::Invalid, "proportions/leg_length", &["from 0.5 to 2"], &["range"]);
}

#[test]
fn set_proportion_suggests_the_closest_slider_for_a_misspelt_name() {
    let doc = project();
    let e = refused(&doc, "body.set_proportion", json!({ "character": "mock", "name": "leg_lenght", "value": 1.0 }));
    assert_error(&e, ErrorCode::Invalid, "name", &["leg_lenght"], &["did you mean `leg_length`"]);
}

#[test]
fn set_shape_round_trips_and_leaves_the_other_axes_alone() {
    let mut doc = project();
    round_trip(&mut doc, "body.set_shape", json!({ "character": "mock", "axis": "build", "value": -0.5 }));
    assert_eq!(at(&doc, &format!("{PARAMS}/shape/build")), Some(json!(-0.5)));
    assert_eq!(at(&doc, &format!("{PARAMS}/shape/height")), None);
}

#[test]
fn set_shape_refuses_an_axis_outside_minus_one_to_one_naming_the_path() {
    let doc = project();
    let e = refused(&doc, "body.set_shape", json!({ "character": "mock", "axis": "build", "value": 1.5 }));
    assert_error(&e, ErrorCode::Invalid, "shape/build", &["from -1 to 1"], &["range"]);
}

#[test]
fn set_shape_suggests_the_closest_axis_for_a_misspelt_name() {
    let doc = project();
    let e = refused(&doc, "body.set_shape", json!({ "character": "mock", "axis": "bulid", "value": 0.2 }));
    assert_error(&e, ErrorCode::Invalid, "axis", &["bulid"], &["did you mean `build`", "height"]);
}

#[test]
fn apply_shape_preset_at_full_blend_takes_the_preset_and_names_it() {
    let mut doc = project();
    round_trip(&mut doc, "body.apply_shape_preset", json!({ "character": "mock", "preset": "curvy", "blend": 1.0 }));
    let curvy = preset("curvy").unwrap().expect("curvy is in the library").shape.clone();
    assert_eq!(at(&doc, &format!("{PARAMS}/shape/curves")), Some(json!(curvy.curves)));
    assert_eq!(at(&doc, &format!("{PARAMS}/shape/preset")), Some(json!("curvy")));
}

#[test]
fn apply_shape_preset_at_half_blend_moves_halfway_and_names_no_preset() {
    let mut doc = project();
    round_trip(&mut doc, "body.apply_shape_preset", json!({ "character": "mock", "preset": "curvy", "blend": 0.5 }));
    let curvy = preset("curvy").unwrap().expect("curvy is in the library").shape.clone();
    let halfway = at(&doc, &format!("{PARAMS}/shape/curves")).and_then(|v| v.as_f64()).unwrap();
    assert!((halfway - curvy.curves / 2.0).abs() < 1e-12, "the current shape is the template: {halfway}");
    assert_eq!(at(&doc, &format!("{PARAMS}/shape/preset")), None, "halfway between a preset and nothing is no preset");
}

#[test]
fn apply_shape_preset_suggests_the_closest_preset_and_refuses_a_blend_outside_zero_to_one() {
    let doc = project();
    let e = refused(&doc, "body.apply_shape_preset", json!({ "character": "mock", "preset": "curvey", "blend": 0.5 }));
    assert_error(&e, ErrorCode::Invalid, "preset", &["curvey"], &["did you mean `curvy`"]);
    let e = refused(&doc, "body.apply_shape_preset", json!({ "character": "mock", "preset": "curvy", "blend": 1.5 }));
    assert_error(&e, ErrorCode::Invalid, "blend", &["from 0 to 1"], &["0 keeps"]);
}

#[test]
fn set_hand_style_round_trips_and_suggests_the_closest_style_for_a_misspelt_one() {
    let mut doc = project();
    round_trip(&mut doc, "body.set_hand_style", json!({ "character": "mock", "style": "five_finger" }));
    assert_eq!(at(&doc, &format!("{PARAMS}/hands")), Some(json!("five_finger")));
    let e = refused(&doc, "body.set_hand_style", json!({ "character": "mock", "style": "mittens" }));
    assert_error(&e, ErrorCode::Invalid, "style", &["mittens"], &["did you mean `mitten`"]);
}

#[test]
fn set_template_round_trips_and_reports_the_fits_the_build_actually_moved() {
    let mut doc = project();
    let planned = round_trip(&mut doc, "body.set_template", json!({ "character": "mock", "template": "tall" }));
    assert_eq!(at(&doc, "characters/mock/template"), Some(json!("tall")));
    // The real body and head read the template from `BuildCtx`, so a bigger template moves the fits that sit on them.
    let output = planned.output.expect("the op reports its result");
    let moved: Vec<&str> =
        output["fit_changed"].as_array().expect("a list").iter().filter_map(|v| v.as_str()).collect();
    assert!(moved.contains(&"head") && moved.contains(&"shirt"), "{output}");
}

#[test]
fn set_template_to_the_current_template_changes_nothing() {
    let doc = project();
    let planned = plan(&doc, "body.set_template", json!({ "character": "mock", "template": "soft-sd" })).unwrap();
    assert!(planned.patch.is_empty());
    assert_eq!(planned.output.unwrap()["fit_changed"], json!([]));
}

#[test]
fn set_template_to_an_unknown_id_lists_the_registered_templates() {
    let doc = project();
    let e = refused(&doc, "body.set_template", json!({ "character": "mock", "template": "soft-sdd" }));
    assert_error(&e, ErrorCode::Invalid, "template", &["soft-sdd"], &["did you mean `soft-sd`", "valid choices"]);
}

#[test]
fn a_character_without_a_body_part_is_refused_and_told_to_add_one() {
    let mut node = mock_node();
    node["parts"]["body"]["kind"] = json!("mock-body");
    let doc = MemDoc::from_value(json!({ "characters": { "mock": node } }));
    let e = refused(&doc, "body.set_proportion", json!({ "character": "mock", "name": "leg_length", "value": 1.0 }));
    assert_error(&e, ErrorCode::NotFound, "character", &["no body part"], &["part.add"]);
}
