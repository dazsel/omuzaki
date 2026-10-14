//! The expression ops (FE-03) against an in-memory document: set, delete, rename, duplicate and preview. Covers the
//! round trip and undo of each edit, the refusals that name their field, the reserved names of the VRM contract, the
//! curve applied at the weight, additive and clamped composition, and the preview's paint summary.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face as _;
use omuzaki_face_api::{ExpressionId, ExpressionSet, VISEMES};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, OpKind, Plan, find};
use serde_json::{Value, json};

/// Runs the registered op `name` with `args` against `doc` and returns its plan (a dry run: nothing is written).
fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

/// The refusal an op gave; a call that succeeds fails the test here.
fn refused(result: Result<Plan, OpError>) -> OpError {
    result.expect_err("the op should refuse these arguments")
}

/// A document holding the character `mira`, with `face` as her face when she has one.
fn doc_with(face: Option<Value>) -> MemDoc {
    let mut mira = json!({ "name": "Mira" });
    if let Some(face) = face {
        mira["face"] = face;
    }
    MemDoc::from_value(json!({ "characters": { "mira": mira } }))
}

/// A face holding the five visemes, `blink` and the starter moods, as the starter library writes them.
fn starter_face() -> Value {
    json!({ "expressions": serde_json::to_value(ExpressionSet::starter()).expect("the starter serialises") })
}

fn id(name: &str) -> ExpressionId {
    ExpressionId::new(name).expect("a valid expression name")
}

/// The starter expression `name` as the document would hold it.
fn starter_entry(name: &str) -> Value {
    let set = ExpressionSet::starter();
    serde_json::to_value(&set.0[&id(name)]).expect("an expression serialises")
}

fn path(text: &str) -> DocPath {
    DocPath::parse(text).expect("a valid document path")
}

fn value_at(doc: &MemDoc, text: &str) -> Value {
    doc.get(&path(text)).cloned().unwrap_or(Value::Null)
}

/// The stored expression `name` on `mira`, or `null` when there is none.
fn stored(doc: &MemDoc, name: &str) -> Value {
    value_at(doc, &format!("characters/mira/face/expressions/{name}"))
}

fn set_args(name: &str, kind: &str, deltas: Value) -> Value {
    let mut args = json!({ "character": "mira", "id": name, "kind": kind });
    args["deltas"] = deltas;
    args
}

fn with_curve(mut args: Value, curve: Value) -> Value {
    args["curve"] = curve;
    args
}

fn values_args(name: &str, weight: f64) -> Value {
    json!({ "character": "mira", "id": name, "weight": weight })
}

/// Sets the starter expression `name` on `mira` through the op, and applies the patch.
fn set_starter(doc: &mut MemDoc, name: &str) {
    let entry = starter_entry(name);
    let args = json!({
        "character": "mira",
        "id": name,
        "kind": entry["kind"],
        "deltas": entry["deltas"],
        "curve": entry["curve"],
    });
    let plan = run(doc, "face.expression.set", args).unwrap_or_else(|e| panic!("set {name}: {e}"));
    doc.apply(&plan.patch).unwrap_or_else(|e| panic!("apply {name}: {e}"));
}

#[test]
fn all_five_ops_are_registered_with_their_kinds() {
    for (name, kind) in [
        ("face.expression.set", OpKind::Edit),
        ("face.expression.delete", OpKind::Edit),
        ("face.expression.rename", OpKind::Edit),
        ("face.expression.duplicate", OpKind::Edit),
        ("face.expression.values", OpKind::Query),
    ] {
        let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
        assert_eq!((registration.meta)().kind, kind, "{name}");
    }
}

#[test]
fn set_creates_the_face_and_its_library_when_there_are_none() {
    let mut doc = doc_with(None);
    let before = doc.root().clone();
    let args = set_args("happy", "emotion", json!({ "jawOpen": 0.5 }));
    let plan = run(&doc, "face.expression.set", args.clone()).expect("a new expression plans");
    assert_eq!(run(&doc, "face.expression.set", args).expect("a dry run repeats"), plan);
    assert_eq!(plan.output.as_ref().expect("an output")["created"], json!(true));
    assert_eq!(plan.patch.changes.len(), 1, "one change creates the face");
    assert_eq!(plan.patch.changes[0].path.to_string(), "characters/mira/face");

    doc.apply(&plan.patch).expect("the patch applies");
    assert_eq!(stored(&doc, "happy"), json!({ "kind": "emotion", "deltas": { "jawOpen": 0.5 }, "curve": "linear" }));
    doc.apply(&plan.patch.inverse()).expect("the undo applies");
    assert_eq!(doc.root(), &before);
}

#[test]
fn set_adds_the_library_to_a_face_that_has_none() {
    let mut doc = doc_with(Some(json!({ "eyes": { "scale": 1.2 } })));
    let before = doc.root().clone();
    let plan = run(&doc, "face.expression.set", set_args("sad", "emotion", json!({ "tears": 1.0 })))
        .expect("a new expression plans");
    assert_eq!(plan.patch.changes.len(), 1);
    assert_eq!(plan.patch.changes[0].path.to_string(), "characters/mira/face/expressions");
    doc.apply(&plan.patch).expect("the patch applies");
    assert_eq!(stored(&doc, "sad")["deltas"], json!({ "tears": 1.0 }));
    doc.apply(&plan.patch.inverse()).expect("the undo applies");
    assert_eq!(doc.root(), &before);
}

#[test]
fn set_replaces_an_expression_and_undo_restores_the_old_one() {
    let mut doc = doc_with(Some(starter_face()));
    let before = doc.root().clone();
    let plan = run(&doc, "face.expression.set", set_args("happy", "emotion", json!({ "jawOpen": 0.1 })))
        .expect("replacing plans");
    assert_eq!(plan.output.as_ref().expect("an output")["created"], json!(false));
    assert_eq!(plan.patch.changes.len(), 1);
    assert_eq!(plan.patch.changes[0].path.to_string(), "characters/mira/face/expressions/happy");
    doc.apply(&plan.patch).expect("the patch applies");
    assert_eq!(stored(&doc, "happy")["deltas"], json!({ "jawOpen": 0.1 }));
    doc.apply(&plan.patch.inverse()).expect("the undo applies");
    assert_eq!(doc.root(), &before);
}

#[test]
fn a_viseme_can_be_tuned_but_keeps_its_kind() {
    let doc = doc_with(Some(starter_face()));
    run(&doc, "face.expression.set", set_args("aa", "viseme", json!({ "jawOpen": 0.6 })))
        .expect("tuning aa as a viseme is allowed");
    run(&doc, "face.expression.set", set_args("blink", "blink", json!({ "eyeBlinkLeft": 1.0 })))
        .expect("tuning blink as blink is allowed");
    for (name, kind) in [("aa", "emotion"), ("blink", "accent"), ("happy", "viseme"), ("wink", "blink")] {
        let err = refused(run(&doc, "face.expression.set", set_args(name, kind, json!({ "jawOpen": 0.6 }))));
        assert_eq!(err.code, ErrorCode::Invalid, "{name} as {kind}");
        assert_eq!(err.field.as_deref(), Some("kind"), "{name} as {kind}: {}", err.message);
    }
}

#[test]
fn the_visemes_and_blink_cannot_be_deleted() {
    let doc = doc_with(Some(starter_face()));
    for name in VISEMES.iter().copied().chain(["blink"]) {
        let err = refused(run(&doc, "face.expression.delete", json!({ "character": "mira", "id": name })));
        assert_eq!(err.code, ErrorCode::Invalid, "{name}");
        assert_eq!(err.field.as_deref(), Some("id"), "{name}");
        assert!(err.message.contains("VRM contract") && err.message.contains(name), "{name}: {}", err.message);
        assert!(err.hint.is_some(), "{name} names a way forward");
    }
}

#[test]
fn delete_removes_a_mood_and_undo_restores_it() {
    let mut doc = doc_with(Some(starter_face()));
    let before = doc.root().clone();
    let plan =
        run(&doc, "face.expression.delete", json!({ "character": "mira", "id": "happy" })).expect("a mood deletes");
    doc.apply(&plan.patch).expect("the patch applies");
    assert_eq!(stored(&doc, "happy"), Value::Null);
    assert_eq!(stored(&doc, "aa"), starter_entry("aa"), "the other entries stay");
    doc.apply(&plan.patch.inverse()).expect("the undo applies");
    assert_eq!(doc.root(), &before);
}

#[test]
fn rename_moves_the_entry_and_refuses_taken_or_reserved_names() {
    let mut doc = doc_with(Some(starter_face()));
    let before = doc.root().clone();
    let args = json!({ "character": "mira", "id": "happy", "to": "glad" });
    let plan = run(&doc, "face.expression.rename", args).expect("a free name takes the entry");
    doc.apply(&plan.patch).expect("the patch applies");
    assert_eq!(stored(&doc, "happy"), Value::Null);
    assert_eq!(stored(&doc, "glad"), starter_entry("happy"));
    doc.apply(&plan.patch.inverse()).expect("the undo applies");
    assert_eq!(doc.root(), &before);

    let taken =
        refused(run(&doc, "face.expression.rename", json!({ "character": "mira", "id": "happy", "to": "sad" })));
    assert_eq!(taken.field.as_deref(), Some("to"));
    assert!(taken.message.contains("already"), "{}", taken.message);
    let same =
        refused(run(&doc, "face.expression.rename", json!({ "character": "mira", "id": "happy", "to": "happy" })));
    assert_eq!(same.field.as_deref(), Some("to"), "renaming to its own name is a taken name");

    for name in VISEMES.iter().copied().chain(["blink"]) {
        let mood_takes_it =
            refused(run(&doc, "face.expression.rename", json!({ "character": "mira", "id": "happy", "to": name })));
        assert_eq!(mood_takes_it.field.as_deref(), Some("to"), "a mood can't take the reserved name {name}");

        let reserved_moves =
            refused(run(&doc, "face.expression.rename", json!({ "character": "mira", "id": name, "to": "mouth_x" })));
        assert_eq!(reserved_moves.field.as_deref(), Some("id"), "{name} can't be renamed");
        assert!(reserved_moves.message.contains("VRM contract"), "{name}: {}", reserved_moves.message);
    }
}

#[test]
fn duplicate_copies_the_entry_and_refuses_reserved_sources() {
    let mut doc = doc_with(Some(starter_face()));
    let before = doc.root().clone();
    let args = json!({ "character": "mira", "id": "happy", "to": "happy_soft" });
    let plan = run(&doc, "face.expression.duplicate", args).expect("a free name takes the copy");
    doc.apply(&plan.patch).expect("the patch applies");
    assert_eq!(stored(&doc, "happy_soft"), stored(&doc, "happy"));
    assert_eq!(stored(&doc, "happy"), starter_entry("happy"), "the original stays");
    doc.apply(&plan.patch.inverse()).expect("the undo applies");
    assert_eq!(doc.root(), &before);

    for name in VISEMES.iter().copied().chain(["blink"]) {
        let err =
            refused(run(&doc, "face.expression.duplicate", json!({ "character": "mira", "id": name, "to": "copy" })));
        assert_eq!(err.field.as_deref(), Some("id"), "{name}");
        assert!(err.message.contains("VRM contract"), "{name}: {}", err.message);
    }
    let taken =
        refused(run(&doc, "face.expression.duplicate", json!({ "character": "mira", "id": "happy", "to": "sad" })));
    assert_eq!(taken.field.as_deref(), Some("to"));
}

#[test]
fn the_curve_applies_at_the_weight_not_inside_the_deltas() {
    let mut doc = doc_with(None);
    let eased = with_curve(set_args("soft", "emotion", json!({ "jawOpen": 0.8 })), json!("ease_in"));
    let plan = run(&doc, "face.expression.set", eased).expect("an ease-in expression plans");
    doc.apply(&plan.patch).expect("the patch applies");
    let eased_half =
        run(&doc, "face.expression.values", values_args("soft", 0.5)).expect("a preview").output.expect("output");
    assert_eq!(eased_half["applied"], json!(0.25), "ease-in at 0.5 applies a quarter");
    assert_eq!(eased_half["channels"]["jawOpen"], json!(0.2), "0.8 times the quarter, not 0.4");

    let linear = set_args("plain", "emotion", json!({ "jawOpen": 0.8 }));
    let plan = run(&doc, "face.expression.set", linear).expect("a linear expression plans");
    doc.apply(&plan.patch).expect("the patch applies");
    let plain_half =
        run(&doc, "face.expression.values", values_args("plain", 0.5)).expect("a preview").output.expect("output");
    assert_eq!(plain_half["channels"]["jawOpen"], json!(0.4), "linear at 0.5 is half the delta");
}

#[test]
fn a_power_curve_needs_a_positive_exponent() {
    let doc = doc_with(None);
    for exponent in [0.0, -1.0] {
        let args = with_curve(
            set_args("x", "emotion", json!({ "jawOpen": 0.5 })),
            json!({ "power": { "exponent": exponent } }),
        );
        let err = refused(run(&doc, "face.expression.set", args));
        assert_eq!(err.field.as_deref(), Some("curve/exponent"), "{exponent}: {}", err.message);
    }
    let args = with_curve(set_args("x", "emotion", json!({ "jawOpen": 0.5 })), json!({ "power": { "exponent": 2.0 } }));
    run(&doc, "face.expression.set", args).expect("an exponent above 0 is allowed");
}

#[test]
fn deltas_name_the_channel_and_its_range() {
    let doc = doc_with(None);
    let big = refused(run(&doc, "face.expression.set", set_args("big", "emotion", json!({ "jawOpen": 1.5 }))));
    assert_eq!(big.field.as_deref(), Some("deltas/jawOpen"), "{}", big.message);
    assert!(big.message.contains("jawOpen"), "{}", big.message);
    assert!(big.hint.is_some(), "the range is in the hint");

    run(&doc, "face.expression.set", set_args("shut", "emotion", json!({ "jawOpen": -0.5 })))
        .expect("a negative delta inside the channel's range is allowed");
    let empty = refused(run(&doc, "face.expression.set", set_args("none", "emotion", json!({}))));
    assert_eq!(empty.field.as_deref(), Some("deltas"));
}

#[test]
fn an_unknown_channel_name_is_refused() {
    let doc = doc_with(None);
    let err = refused(run(&doc, "face.expression.set", set_args("x", "emotion", json!({ "jawOpenn": 0.5 }))));
    assert_eq!(err.code, ErrorCode::Invalid);
    assert!(err.message.contains("jawOpenn"), "{}", err.message);
}

#[test]
fn happy_and_aa_compose_additively_and_clamp() {
    let mut doc = doc_with(None);
    set_starter(&mut doc, "happy");
    set_starter(&mut doc, "aa");
    let set: ExpressionSet =
        serde_json::from_value(value_at(&doc, "characters/mira/face/expressions")).expect("the library reads");
    let buf =
        set.blend(&ChannelBuf::NEUTRAL, &[(&id("happy"), 1.0), (&id("aa"), 1.0)]).expect("both expressions exist");
    assert_eq!(buf.get(Channel::JAW_OPEN), 1.0, "0.36 plus 0.74 clamps to the channel maximum");
    assert!((buf.get(Channel::MOUTH_SMILE_LEFT) - 0.9).abs() < 1e-6, "0.8 plus 0.1 adds");
}

#[test]
fn the_preview_reports_the_channels_and_the_paint_an_expression_makes() {
    let doc = doc_with(Some(starter_face()));
    let out = run(&doc, "face.expression.values", values_args("aa", 1.0)).expect("a preview").output.expect("output");
    assert_eq!(out["channels"]["jawOpen"], json!(0.74));
    let open = out["paint"]["mouth"]["open"].as_f64().expect("the mouth opens");
    assert!((open - 0.85).abs() < 0.03, "aa opens the mouth about as far as the prototype did: {open}");
    assert_eq!(out["paint"]["gaze"].as_array().map(Vec::len), Some(2));

    let off = run(&doc, "face.expression.values", values_args("aa", 0.0)).expect("a preview").output.expect("output");
    assert_eq!(off["channels"], json!({}), "weight 0 leaves the neutral bus untouched");
}

#[test]
fn a_weight_outside_zero_to_one_is_refused() {
    let doc = doc_with(Some(starter_face()));
    for weight in [1.5, -0.1] {
        let err = refused(run(&doc, "face.expression.values", values_args("aa", weight)));
        assert_eq!(err.field.as_deref(), Some("weight"), "{weight}");
    }
}

#[test]
fn an_unknown_name_or_character_is_not_found() {
    let doc = doc_with(Some(starter_face()));
    for (op, args) in [
        ("face.expression.delete", json!({ "character": "mira", "id": "smug" })),
        ("face.expression.rename", json!({ "character": "mira", "id": "smug", "to": "sly" })),
        ("face.expression.duplicate", json!({ "character": "mira", "id": "smug", "to": "sly" })),
        ("face.expression.values", values_args("smug", 1.0)),
    ] {
        let err = refused(run(&doc, op, args));
        assert_eq!(err.code, ErrorCode::NotFound, "{op}");
        assert!(err.message.contains("smug"), "{op}: {}", err.message);
    }
    let nobody = refused(run(
        &doc,
        "face.expression.set",
        json!({ "character": "nobody", "id": "happy", "kind": "emotion", "deltas": { "jawOpen": 0.5 } }),
    ));
    assert_eq!(nobody.code, ErrorCode::NotFound);
}

#[test]
fn a_library_that_is_not_an_object_is_a_conflict() {
    let doc = doc_with(Some(json!({ "expressions": [1, 2] })));
    let err = refused(run(&doc, "face.expression.set", set_args("happy", "emotion", json!({ "jawOpen": 0.5 }))));
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(err.message, "face/expressions is not an object, so it can't hold expressions");
}
