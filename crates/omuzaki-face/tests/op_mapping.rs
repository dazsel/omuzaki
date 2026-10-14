//! The face mapping ops (FE-06) against an in-memory document: `face.set_mapping`, `face.set_rest` and
//! `face.set_rest_channels` (dry-run, apply and undo for twenty paths), refusals that name every bad field and the
//! nearest key, the mirror test run with edited mappings, and outputs that stay in range for wild input (proptest).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{CHANNELS, Channel, ChannelBuf};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face as _;
use omuzaki_face_api::{FaceMapping, FaceProfile, Validate};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, Plan, find};
use proptest::prelude::*;
use serde_json::{Value, json};

/// Runs the registered op `name` with `args` against `doc` and returns its plan (a dry run: nothing is written).
fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

/// A document holding the character `mira` with a complete default face.
fn doc() -> MemDoc {
    let face = serde_json::to_value(FaceProfile::default()).expect("the default face serialises");
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }))
}

fn path(text: &str) -> DocPath {
    DocPath::parse(text).expect("a valid document path")
}

fn face_of(doc: &MemDoc) -> FaceProfile {
    serde_json::from_value(doc.get(&path("characters/mira/face")).cloned().expect("a face")).expect("a face that reads")
}

fn contains_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(contains_null),
        Value::Object(map) => map.values().any(contains_null),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

/// Fails if any change in `patch` writes or removes a `null`.
fn assert_no_null(patch: &Patch) {
    for change in &patch.changes {
        for side in [&change.before, &change.after].into_iter().flatten() {
            assert!(!contains_null(side), "{} would hold a null: {side}", change.path);
        }
    }
}

/// Twenty edits, fourteen coefficients and six resting fields, each with a value that differs from its default. The
/// tuple is the op, the path or field, the value, and the document path the change lands on.
fn twenty_edits() -> Vec<(&'static str, &'static str, Value, &'static str)> {
    vec![
        ("face.set_mapping", "eyes/wide", json!(0.9), "characters/mira/face/mapping/eyes/wide"),
        ("face.set_mapping", "eyes/squint", json!(0.3), "characters/mira/face/mapping/eyes/squint"),
        ("face.set_mapping", "eyes/shock_pupil", json!(0.4), "characters/mira/face/mapping/eyes/shock_pupil"),
        ("face.set_mapping", "eyes/tilt_from_brows", json!(0.2), "characters/mira/face/mapping/eyes/tilt_from_brows"),
        ("face.set_mapping", "brows/inner_up", json!(0.5), "characters/mira/face/mapping/brows/inner_up"),
        ("face.set_mapping", "brows/down", json!(0.2), "characters/mira/face/mapping/brows/down"),
        ("face.set_mapping", "mouth/smile", json!(1.6), "characters/mira/face/mapping/mouth/smile"),
        ("face.set_mapping", "mouth/frown", json!(0.3), "characters/mira/face/mapping/mouth/frown"),
        ("face.set_mapping", "mouth/open", json!(0.8), "characters/mira/face/mapping/mouth/open"),
        ("face.set_mapping", "mouth/teeth_gain", json!(3.0), "characters/mira/face/mapping/mouth/teeth_gain"),
        ("face.set_mapping", "cheeks/blush", json!(0.7), "characters/mira/face/mapping/cheeks/blush"),
        ("face.set_mapping", "gaze/x", json!(1.2), "characters/mira/face/mapping/gaze/x"),
        ("face.set_mapping", "gaze/y", json!(0.6), "characters/mira/face/mapping/gaze/y"),
        ("face.set_mapping", "mouth/press_close", json!(0.5), "characters/mira/face/mapping/mouth/press_close"),
        ("face.set_rest", "eye_open", json!(0.5), "characters/mira/face/mapping/rest/eye_open"),
        ("face.set_rest", "lower_lid", json!(0.2), "characters/mira/face/mapping/rest/lower_lid"),
        ("face.set_rest", "mouth_smile", json!(-0.45), "characters/mira/face/mapping/rest/mouth_smile"),
        ("face.set_rest", "blush", json!(0.75), "characters/mira/face/mapping/rest/blush"),
        ("face.set_rest", "pupil", json!(1.2), "characters/mira/face/mapping/rest/pupil"),
        ("face.set_rest", "brow_tilt", json!(0.5), "characters/mira/face/mapping/rest/brow_tilt"),
    ]
}

/// The arguments for one edit: `path` for the coefficients, `field` for the resting face.
fn edit_args(op: &str, key: &str, value: Value) -> Value {
    let name = if op == "face.set_rest" { "field" } else { "path" };
    let mut args = json!({ "character": "mira", name: key });
    args["value"] = value;
    args
}

#[test]
fn twenty_paths_dry_run_apply_and_undo_identically() {
    let edits = twenty_edits();
    assert_eq!(edits.len(), 20);
    for (op, key, value, doc_path) in edits {
        let mut document = doc();
        let before = document.root().clone();
        let dry = run(&document, op, edit_args(op, key, value.clone())).unwrap_or_else(|e| panic!("{key}: {e}"));
        let again = run(&document, op, edit_args(op, key, value.clone())).expect("a dry run repeats");
        assert_eq!(dry, again, "{key}: a dry run is repeatable");
        assert_no_null(&dry.patch);
        assert_eq!(dry.patch.changes.len(), 1, "{key}: one leaf changes");
        assert_eq!(dry.output.as_ref().and_then(|o| o.get("changed")), Some(&json!([doc_path])), "{key}");
        document.apply(&dry.patch).unwrap_or_else(|e| panic!("{key}: {e}"));
        assert_eq!(document.get(&path(doc_path)), Some(&value), "{key}: applied");
        assert!(face_of(&document).validate().is_ok(), "{key}: the face still validates");
        document.apply(&dry.patch.inverse()).unwrap_or_else(|e| panic!("{key}: {e}"));
        assert_eq!(document.root(), &before, "{key}: undo restores the document exactly");
    }
}

#[test]
fn the_plan_shows_the_resting_paint_before_and_after() {
    let document = doc();
    let plan = run(&document, "face.set_rest", edit_args("face.set_rest", "eye_open", json!(0.5))).expect("a plan");
    let neutral = plan.output.as_ref().and_then(|o| o.get("neutral_paint")).expect("the paint is in the plan");
    let (before, after) = (&neutral["before"], &neutral["after"]);
    assert_eq!(before["left_eye"]["open"], json!(1.0));
    assert_eq!(after["left_eye"]["open"], json!(0.5));
    assert_eq!(after["right_eye"]["open"], json!(0.5));
}

#[test]
fn a_bad_path_or_value_is_refused_with_its_field_and_the_nearest_key() {
    let document = doc();
    let typo = run(&document, "face.set_mapping", edit_args("face.set_mapping", "eyes/wdie", json!(0.5))).unwrap_err();
    assert_eq!(typo.code, ErrorCode::Invalid);
    assert_eq!(typo.field.as_deref(), Some("face/mapping/eyes/wdie"));
    assert!(typo.hint.as_deref().unwrap_or_default().contains("face/mapping/eyes/wide"), "{typo:?}");

    let wrong =
        run(&document, "face.set_mapping", edit_args("face.set_mapping", "eyes/wide", json!("big"))).unwrap_err();
    assert_eq!(wrong.field.as_deref(), Some("face/mapping/eyes/wide"));

    let range =
        run(&document, "face.set_mapping", edit_args("face.set_mapping", "eyes/shock_pupil", json!(2.0))).unwrap_err();
    assert_eq!(range.field.as_deref(), Some("face/mapping/eyes/shock_pupil"));

    let rest = run(&document, "face.set_rest", edit_args("face.set_rest", "eye_open", json!(3.0))).unwrap_err();
    assert_eq!(rest.field.as_deref(), Some("face/mapping/rest/eye_open"));
    assert!(rest.message.contains("0 to 1.9"), "{}", rest.message);

    let null = run(&document, "face.set_mapping", edit_args("face.set_mapping", "eyes/wide", Value::Null)).unwrap_err();
    assert_eq!(null.field.as_deref(), Some("face/mapping/eyes/wide"));
}

#[test]
fn the_resting_face_is_set_only_by_set_rest_and_the_mapping_is_never_replaced_whole() {
    let document = doc();
    let via_mapping =
        run(&document, "face.set_mapping", edit_args("face.set_mapping", "rest/eye_open", json!(0.5))).unwrap_err();
    assert_eq!(via_mapping.code, ErrorCode::Invalid);
    assert!(via_mapping.hint.as_deref().unwrap_or_default().contains("face.set_rest"), "{via_mapping:?}");

    let whole = run(&document, "face.set_mapping", edit_args("face.set_mapping", "mapping", json!({}))).unwrap_err();
    assert_eq!(whole.field.as_deref(), Some("face/mapping"));
}

#[test]
fn every_bad_rest_channel_is_named_and_a_typo_suggests_the_channel() {
    let document = doc();
    let two = json!({ "character": "mira", "channels": { "jawOpen": 9.0, "eyeBlinkLeft": -1.0 } });
    let err = run(&document, "face.set_rest_channels", two).unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert!(err.message.starts_with("2 fields are invalid"), "{}", err.message);
    assert!(err.message.contains("face/rest/jawOpen"), "{}", err.message);
    assert!(err.message.contains("face/rest/eyeBlinkLeft"), "{}", err.message);

    let typo = json!({ "character": "mira", "channels": { "jawOpne": 0.1 } });
    let err = run(&document, "face.set_rest_channels", typo).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("face/rest/jawOpne"));
    assert!(err.hint.as_deref().unwrap_or_default().contains("jawOpen"), "{err:?}");
}

#[test]
fn a_rest_layer_edit_writes_only_the_named_channels_and_reports_the_paint() {
    let mut document = doc();
    let args = json!({ "character": "mira", "channels": { "mouthSmileLeft": 0.2 } });
    let plan = run(&document, "face.set_rest_channels", args).expect("a plan");
    assert_eq!(plan.patch.changes.len(), 1, "{:?}", plan.patch);
    assert_eq!(plan.patch.changes[0].path.to_string(), "characters/mira/face/rest/mouthSmileLeft");
    document.apply(&plan.patch).expect("applies");
    let face = face_of(&document);
    assert_eq!(face.rest.0.get(&Channel::MOUTH_SMILE_LEFT), Some(&0.2));
    assert_eq!(face.rest.0.len(), 1);
    let paint = plan.output.as_ref().and_then(|o| o.get("paint_with_rest_layer")).expect("the paint is in the plan");
    assert_eq!(paint["before"]["mouth"]["smile_left"], json!(0.0));
    // The mouth's smile coefficient (1.35 by default) scales the rest channel: 1.35 x 0.2.
    assert_eq!(paint["after"]["mouth"]["smile_left"], json!(0.27));
}

#[test]
fn an_unknown_character_is_not_found() {
    let document = doc();
    let err =
        run(&document, "face.set_rest", json!({ "character": "nobody", "field": "blush", "value": 0.1 })).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[test]
fn the_mirror_test_holds_after_edits() {
    let mut document = doc();
    let edits = [
        ("face.set_mapping", "eyes/wide", json!(0.9)),
        ("face.set_mapping", "mouth/smile", json!(1.6)),
        ("face.set_rest", "mouth_smile", json!(-0.45)),
    ];
    for (op, key, value) in edits {
        let plan = run(&document, op, edit_args(op, key, value)).expect("a plan");
        document.apply(&plan.patch).expect("applies");
    }
    let mapping: FaceMapping = face_of(&document).mapping;
    let mut buf = ChannelBuf::NEUTRAL;
    buf.set(Channel::MOUTH_SMILE_LEFT, 0.8);
    buf.set(Channel::EYE_WIDE_LEFT, 0.5);
    buf.set(Channel::BROW_OUTER_UP_RIGHT, 0.6);
    let mut mirrored_input = buf;
    mirrored_input.mirror();
    assert_eq!(mapping.apply(&buf).mirrored(), mapping.apply(&mirrored_input));
}

/// Every mapping coefficient the ops can set, with a value range (inclusive) the validator accepts.
const COEFFICIENTS: [(&str, f64, f64); 6] = [
    ("eyes/wide", 0.0, 6.0),
    ("mouth/smile", 0.0, 6.0),
    ("brows/outer_up", 0.0, 6.0),
    ("eyes/shock_pupil", 0.0, 1.0),
    ("gaze/x", 0.0, 3.0),
    ("mouth/open", 0.0, 6.0),
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// After any valid edit, any bus input, even a wild one, gives paint inside the documented ranges.
    #[test]
    fn outputs_stay_in_range_for_wild_input(
        index in 0..COEFFICIENTS.len(),
        unit in 0.0f64..=1.0,
        // Each channel takes any value in its own range, the extremes included. The bus is sanitised to its ranges before
        // mapping (the pipeline's last step). A value outside a range makes `press` negative, so it is not fed here.
        wild in proptest::collection::vec(0.0f64..=1.0, CHANNELS.len()),
    ) {
        let (key, lo, hi) = COEFFICIENTS[index];
        let mut document = doc();
        let value = lo + (hi - lo) * unit;
        let plan = run(&document, "face.set_mapping", json!({ "character": "mira", "path": key, "value": value }));
        prop_assume!(plan.is_ok());
        document.apply(&plan.expect("checked above").patch).expect("applies");
        let mapping = face_of(&document).mapping;

        let mut buf = ChannelBuf::NEUTRAL;
        for (info, u) in CHANNELS.iter().zip(&wild) {
            let (lo, hi) = (f64::from(info.min), f64::from(info.max));
            // A channel range is a small f32 interval, so narrowing the f64 interpolation loses nothing that matters.
            #[allow(clippy::cast_possible_truncation)]
            buf.set(info.channel, (lo + (hi - lo) * u) as f32);
        }
        let p = mapping.apply(&buf);
        let inside = |v: f32, lo: f32, hi: f32| (lo..=hi).contains(&v);
        prop_assert!(inside(p.left_eye.open, 0.0, 1.9) && inside(p.right_eye.open, 0.0, 1.9));
        prop_assert!(inside(p.left_eye.lower_lid, 0.0, 1.0) && inside(p.right_eye.lower_lid, 0.0, 1.0));
        prop_assert!(inside(p.pupil, 0.4, 1.6) && inside(p.eye_tilt, -2.0, 2.0));
        prop_assert!(inside(p.mouth.wide, 0.3, 2.0) && inside(p.mouth.smile_left, -2.0, 2.0));
        prop_assert!(inside(p.mouth.open, 0.0, 1.5) && inside(p.mouth.press, 0.0, 1.0));
        prop_assert!(inside(p.blush, 0.0, 1.6) && inside(p.gaze.x, -1.0, 1.0) && inside(p.gaze.y, -1.0, 1.0));
    }
}

/// A document whose face holds damage elsewhere (`eyes.scale` is text), which its paint can't read.
fn damaged_doc() -> MemDoc {
    let mut face = serde_json::to_value(FaceProfile::default()).expect("the default face serialises");
    face["eyes"]["scale"] = json!("bad");
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }))
}

#[test]
fn an_edit_is_not_blamed_for_damage_elsewhere_and_repaints_the_whole_face() {
    let mut document = damaged_doc();
    let plan = run(&document, "face.set_mapping", edit_args("face.set_mapping", "eyes/wide", json!(0.9)))
        .expect("an edit that writes a good value succeeds on a face damaged elsewhere");
    let report = plan.output.as_ref().and_then(|o| o.get("neutral_paint")).expect("the report is in the plan");
    assert_eq!(report["repaint"], json!("full"), "the paint can't be read, so the whole face is dirty");
    assert!(report["before"]["unreadable"].is_string(), "{report}");
    assert_eq!(plan.patch.changes.len(), 1, "only the written leaf changes");
    document.apply(&plan.patch).expect("applies");
    assert_eq!(document.get(&path("characters/mira/face/mapping/eyes/wide")), Some(&json!(0.9)));

    let rest = run(&document, "face.set_rest", edit_args("face.set_rest", "blush", json!(0.5))).expect("a plan");
    assert_eq!(rest.output.as_ref().and_then(|o| o.get("neutral_paint")).map(|r| &r["repaint"]), Some(&json!("full")));
}

#[test]
fn an_edit_that_writes_a_bad_value_is_still_refused_on_a_damaged_face() {
    let document = damaged_doc();
    let err = run(&document, "face.set_mapping", edit_args("face.set_mapping", "eyes/wide", json!("big"))).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("face/mapping/eyes/wide"));
}

#[test]
fn every_unknown_rest_channel_name_is_reported_in_one_error() {
    let document = doc();
    let typos = json!({ "character": "mira", "channels": { "jawOpne": 0.1, "eyeBlinkLefft": 0.2 } });
    let err = run(&document, "face.set_rest_channels", typos).unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert!(err.message.starts_with("2 fields are invalid"), "{}", err.message);
    assert!(
        err.message.contains("face/rest/jawOpne") && err.message.contains("face/rest/eyeBlinkLefft"),
        "{}",
        err.message
    );
    let hint = err.hint.as_deref().unwrap_or_default();
    assert!(hint.contains("jawOpen") && hint.contains("eyeBlinkLeft"), "{hint}");
}
