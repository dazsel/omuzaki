//! The face style ops (CB-20) against an in-memory document: dry-run, apply and undo for thirty paths across every
//! section, refusals that name every bad field and the nearest key, an agent's three-op batch undone as one, the marks
//! list, and `face.describe_style`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::{FaceProfile, Validate};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

/// Runs the registered op `name` with `args` against `doc` and returns its plan (a dry run: nothing is written).
fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    omuzaki_face::link(); // the ops under test register in omuzaki-face; this links them, as Apps do (ADR 0015)
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

/// A document holding the character `mira` with `face` as her face.
fn doc_with_face(face: Value) -> MemDoc {
    let mut root = json!({ "characters": { "mira": { "name": "Mira" } } });
    root["characters"]["mira"]["face"] = face;
    MemDoc::from_value(root)
}

/// A complete face as the file holds it, with two marks.
fn full_face() -> Value {
    let mut face = serde_json::to_value(FaceProfile::default()).expect("the default face serialises");
    face["marks"] = json!([
        { "kind": "freckles", "at": [0.0, -0.1], "size": 0.003, "colour": "#7a4a40", "opacity": 0.8, "count": 12, "seed": 1 },
        { "kind": "beauty_mark", "at": [0.12, 0.05], "size": 0.002 }
    ]);
    face
}

fn path(text: &str) -> DocPath {
    DocPath::parse(text).expect("a valid document path")
}

fn value_at(doc: &MemDoc, text: &str) -> Value {
    doc.get(&path(text)).cloned().unwrap_or(Value::Null)
}

/// Fails if any change in `patch` writes or removes a `null`.
fn assert_no_null(patch: &Patch) {
    for change in &patch.changes {
        for side in [&change.before, &change.after].into_iter().flatten() {
            assert!(!contains_null(side), "{} would hold a null: {side}", change.path);
        }
    }
}

fn contains_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(contains_null),
        Value::Object(map) => map.values().any(contains_null),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn set_args(field: &str, value: Value) -> Value {
    let mut args = json!({ "character": "mira", "path": field });
    args["value"] = value;
    args
}

/// Thirty representative fields, at least four in every section, with values the validator accepts.
fn thirty_paths() -> Vec<(&'static str, Value)> {
    vec![
        ("face/eyes/scale", json!(1.2)),
        ("face/eyes/drop", json!(0.01)),
        ("face/eyes/aperture/roundness", json!(0.6)),
        ("face/eyes/aperture/tilt", json!(-0.2)),
        ("face/eyes/lash/thickness", json!(0.012)),
        ("face/eyes/lash/colour", json!("@eyes")),
        ("face/eyes/crease/strength", json!(0.0)),
        ("face/eyes/iris/width", json!(0.8)),
        ("face/eyes/iris/height", json!(0.9)),
        ("face/eyes/iris/top", json!("#223344")),
        ("face/eyes/iris/pupil/size", json!(0.8)),
        ("face/eyes/sclera/top", json!("#fafafa")),
        ("face/eyes/highlights/size", json!(0.5)),
        ("face/eyes/lower_lid/line", json!(0.5)),
        ("face/eyes/lid_shadow", json!(0.3)),
        ("face/brows/inner_x", json!(0.05)),
        ("face/brows/outer_x", json!(0.2)),
        ("face/brows/thickness", json!(0.004)),
        ("face/brows/arch", json!(0.01)),
        ("face/brows/through_hair", json!(false)),
        ("face/mouth/half_width", json!(0.02)),
        ("face/mouth/line_width", json!(0.002)),
        ("face/mouth/lips/strength", json!(0.6)),
        ("face/mouth/fang/height", json!(0.01)),
        ("face/mouth/interior/tongue", json!("#cc6677")),
        ("face/nose/kind", json!("dot")),
        ("face/nose/size", json!(1.5)),
        ("face/cheeks/radius/0", json!(0.07)),
        ("face/cheeks/strength", json!(0.5)),
        ("face/marks/1/opacity", json!(0.5)),
    ]
}

#[test]
fn thirty_paths_dry_run_apply_and_undo_identically() {
    let cases = thirty_paths();
    assert_eq!(cases.len(), 30);
    for (field, value) in cases {
        let mut doc = doc_with_face(full_face());
        let before = doc.root().clone();
        let dry =
            run(&doc, "face.set_style", set_args(field, value.clone())).unwrap_or_else(|e| panic!("{field}: {e}"));
        let again = run(&doc, "face.set_style", set_args(field, value.clone())).expect("a dry run repeats");
        assert_eq!(dry, again, "{field}: a dry run is repeatable");
        assert_no_null(&dry.patch);
        assert_eq!(dry.patch.changes.len(), 1, "{field}: one leaf changes");

        let doc_path = format!("characters/mira/{field}");
        assert_eq!(dry.output, Some(json!({ "changed": [doc_path] })), "{field}: the plan names the changed path");

        doc.apply(&dry.patch).unwrap_or_else(|e| panic!("{field}: {e}"));
        assert_eq!(value_at(&doc, &doc_path), value, "{field}: applied");
        let typed: FaceProfile = serde_json::from_value(value_at(&doc, "characters/mira/face")).expect("still a face");
        assert!(typed.validate().is_ok(), "{field}: the face still validates");

        doc.apply(&dry.patch.inverse()).unwrap_or_else(|e| panic!("{field}: {e}"));
        assert_eq!(doc.root(), &before, "{field}: undo restores the document exactly");
    }
}

#[test]
fn an_agent_gives_the_right_eye_its_own_iris_by_path() {
    // The right iris is left out of a saved face until it is set: one colour by path, then a whole section, both
    // through the op, and the face reads back with the left iris untouched.
    let mut doc = doc_with_face(full_face());
    let one = run(&doc, "face.set_style", set_args("face/eyes/right_iris/mid", json!("#8a96a8"))).expect("one colour");
    doc.apply(&one.patch).expect("applies");
    let typed: FaceProfile = serde_json::from_value(value_at(&doc, "characters/mira/face")).expect("still a face");
    let right = typed.eyes.right_iris.as_ref().expect("the right eye has its own iris");
    assert_eq!(right.mid.as_ref().map(ToString::to_string).as_deref(), Some("#8a96a8"));
    assert!(right.top.is_none(), "only the colour that was set is the right eye's own");
    assert_eq!(typed.eyes.iris, FaceProfile::default().eyes.iris, "the left iris is untouched");

    let whole = json!({ "top": "#4a5260", "low": "#c8d2e0", "pupil": { "shape": "slit" } });
    let section = run(&doc, "face.set_style", set_args("face/eyes/right_iris", whole)).expect("a whole section");
    doc.apply(&section.patch).expect("applies");
    let typed: FaceProfile = serde_json::from_value(value_at(&doc, "characters/mira/face")).expect("still a face");
    assert!(typed.validate().is_ok());
    let right = typed.eyes.right_iris.expect("still its own");
    assert!(right.top.is_some() && right.low.is_some() && right.mid.is_some() && right.pupil.is_some());

    let wrong = run(&doc, "face.set_style", set_args("face/eyes/right_iris/middle", json!("#000000")))
        .expect_err("an unknown key under the right iris");
    assert!(wrong.to_string().contains("mid"), "the nearest key is suggested: {wrong}");
}

#[test]
fn every_bad_field_is_reported_with_its_path() {
    let doc = doc_with_face(full_face());
    let err =
        run(&doc, "face.set_style", set_args("face/eyes/iris", json!({ "width": 3.0, "height": 9.0 }))).unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert_eq!(err.field.as_deref(), Some("face/eyes/iris/width"), "the first bad field is the error's field");
    assert!(err.message.contains("face/eyes/iris/width"), "{}", err.message);
    assert!(err.message.contains("face/eyes/iris/height"), "{}", err.message);
    assert!(err.message.starts_with("2 fields are invalid"), "{}", err.message);
}

#[test]
fn one_out_of_range_value_names_its_range() {
    let doc = doc_with_face(full_face());
    let err = run(&doc, "face.set_style", set_args("face/eyes/iris/width", json!(3.0))).unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert_eq!(err.field.as_deref(), Some("face/eyes/iris/width"));
    assert!(err.message.contains("0.2 to 1"), "{}", err.message);
}

#[test]
fn a_wrong_type_names_the_field() {
    let doc = doc_with_face(full_face());
    let err = run(&doc, "face.set_style", set_args("face/eyes/iris/width", json!("big"))).unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert_eq!(err.field.as_deref(), Some("face/eyes/iris/width"));
}

#[test]
fn an_unknown_key_suggests_the_nearest_one() {
    let doc = doc_with_face(full_face());
    let typo = run(&doc, "face.set_style", set_args("face/eyes/iris/widht", json!(0.5))).unwrap_err();
    assert_eq!(typo.code, ErrorCode::Invalid);
    assert_eq!(typo.field.as_deref(), Some("face/eyes/iris/widht"));
    assert!(typo.hint.as_deref().unwrap_or_default().contains("face/eyes/iris/width"), "{typo:?}");

    let section = run(&doc, "face.set_style", set_args("face/eys/scale", json!(1.0))).unwrap_err();
    assert_eq!(section.field.as_deref(), Some("face/eys"));
    assert!(section.hint.as_deref().unwrap_or_default().contains("face/eyes"), "{section:?}");

    let inside_value = run(&doc, "face.set_style", set_args("face/eyes/iris", json!({ "widht": 0.5 }))).unwrap_err();
    assert_eq!(inside_value.field.as_deref(), Some("face/eyes/iris/widht"));
    assert!(inside_value.hint.as_deref().unwrap_or_default().contains("face/eyes/iris/width"), "{inside_value:?}");
}

#[test]
fn a_value_is_never_null() {
    let doc = doc_with_face(full_face());
    let whole = run(&doc, "face.set_style", set_args("face/eyes/iris/width", Value::Null)).unwrap_err();
    assert_eq!(whole.code, ErrorCode::Invalid);
    assert_eq!(whole.field.as_deref(), Some("face/eyes/iris/width"));
    let inside = run(&doc, "face.set_style", set_args("face/eyes/iris", json!({ "width": null }))).unwrap_err();
    assert_eq!(inside.field.as_deref(), Some("face/eyes/iris/width"));
}

#[test]
fn an_agent_sets_bigger_eyes_in_one_transaction_and_undoes_them_as_one() {
    let doc = doc_with_face(full_face());
    let batch = [
        set_args("face/eyes/iris", json!({ "width": 0.85 })),
        set_args("face/eyes/lash/thickness", json!(0.012)),
        set_args("face/eyes/crease/strength", json!(0.0)),
    ];
    // Each op plans against the state the earlier ones leave, as a batch does; the patches join into one transaction.
    let mut working = doc.clone();
    let mut transaction = Patch::new();
    for args in batch {
        let plan = run(&working, "face.set_style", args).expect("each op in the batch plans");
        working.apply(&plan.patch).expect("each op in the batch applies");
        transaction.extend(plan.patch);
    }

    let mut committed = doc.clone();
    committed.apply(&transaction).expect("the transaction applies at once");
    assert_eq!(committed.root(), working.root());
    assert_eq!(value_at(&committed, "characters/mira/face/eyes/iris/width"), json!(0.85));
    assert_eq!(value_at(&committed, "characters/mira/face/eyes/lash/thickness"), json!(0.012));
    assert_eq!(value_at(&committed, "characters/mira/face/eyes/crease/strength"), json!(0.0));

    committed.apply(&transaction.inverse()).expect("the whole batch undoes at once");
    assert_eq!(committed.root(), doc.root());
}

#[test]
fn a_missing_character_and_a_missing_face_are_handled() {
    let doc = doc_with_face(full_face());
    let missing =
        run(&doc, "face.set_style", json!({ "character": "nobody", "path": "face/eyes/scale", "value": 1.0 }))
            .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);

    let mut bare = MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira" } } }));
    let plan = run(&bare, "face.set_style", set_args("face/eyes/iris/width", json!(0.7))).expect("a face is created");
    assert_eq!(plan.patch.changes.len(), 1);
    assert_eq!(plan.patch.changes[0].path, path("characters/mira/face"));
    assert_eq!(plan.patch.changes[0].before, None);
    assert_eq!(plan.patch.changes[0].after, Some(json!({ "eyes": { "iris": { "width": 0.7 } } })));
    assert_no_null(&plan.patch);
    bare.apply(&plan.patch).expect("applies");
    assert_eq!(value_at(&bare, "characters/mira/face/eyes/iris/width"), json!(0.7));
    bare.apply(&plan.patch.inverse()).expect("undoes");
    assert_eq!(value_at(&bare, "characters/mira/face"), Value::Null);
}

#[test]
fn the_marks_list_is_changed_only_at_its_end() {
    let doc = doc_with_face(full_face());
    let whole = run(&doc, "face.set_style", set_args("face/marks", json!([]))).unwrap_err();
    assert_eq!(whole.field.as_deref(), Some("face/marks"));
    let missing = run(&doc, "face.set_style", set_args("face/marks/5/size", json!(0.01))).unwrap_err();
    assert_eq!(missing.field.as_deref(), Some("face/marks/5"));
    assert!(missing.message.contains("holds 2"), "{}", missing.message);

    let add = json!({ "character": "mira", "mark": { "kind": "scar", "at": [0.1, 0.0], "size": 0.02 } });
    let plan = run(&doc, "face.add_mark", add).expect("a mark is added");
    assert_eq!(plan.patch.changes.len(), 1);
    assert_eq!(plan.patch.changes[0].path, path("characters/mira/face/marks/2"));
    assert_eq!(plan.patch.changes[0].before, None);
    assert_eq!(plan.output, Some(json!({ "index": 2, "changed": ["characters/mira/face/marks/2"] })));
    assert_no_null(&plan.patch);
    let mut doc = doc;
    doc.apply(&plan.patch).expect("applies");
    assert_eq!(value_at(&doc, "characters/mira/face/marks/2/kind"), json!("scar"));
    assert_eq!(value_at(&doc, "characters/mira/face/marks/2/count"), json!(12), "defaults fill the rest");
    doc.apply(&plan.patch.inverse()).expect("undoes");
}

#[test]
fn a_bad_mark_names_its_field_and_adds_nothing() {
    let doc = doc_with_face(full_face());
    let bad = json!({ "character": "mira", "mark": { "opacity": 2.0 } });
    let err = run(&doc, "face.add_mark", bad).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("face/marks/2/opacity"));
    let unknown = json!({ "character": "mira", "mark": { "sise": 0.01 } });
    let typo = run(&doc, "face.add_mark", unknown).unwrap_err();
    assert_eq!(typo.code, ErrorCode::Invalid, "{typo:?}");
}

#[test]
fn marks_are_added_to_a_face_without_a_list_and_to_a_character_without_a_face() {
    let no_list = doc_with_face(json!({ "eyes": {} }));
    let plan =
        run(&no_list, "face.add_mark", json!({ "character": "mira", "mark": { "kind": "paint" } })).expect("adds");
    assert_eq!(plan.patch.changes[0].path, path("characters/mira/face/marks"));
    assert_eq!(
        plan.patch.changes[0].after,
        Some(
            json!([{ "kind": "paint", "at": [0.0, 0.0], "size": 0.003, "colour": "#7a4a40", "opacity": 0.8, "count": 12, "seed": 1 }])
        )
    );

    let no_face = MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira" } } }));
    let plan = run(&no_face, "face.add_mark", json!({ "character": "mira" })).expect("adds");
    assert_eq!(plan.patch.changes[0].path, path("characters/mira/face"));
    assert_eq!(plan.patch.changes[0].before, None);
}

#[test]
fn the_last_mark_is_removed_and_undo_puts_it_back() {
    let mut doc = doc_with_face(full_face());
    let before = doc.root().clone();
    let plan = run(&doc, "face.remove_mark", json!({ "character": "mira", "index": 1 })).expect("removes the last");
    assert_eq!(plan.patch.changes[0].path, path("characters/mira/face/marks/1"));
    assert_eq!(plan.patch.changes[0].after, None);
    doc.apply(&plan.patch).expect("applies");
    assert_eq!(value_at(&doc, "characters/mira/face/marks").as_array().map(Vec::len), Some(1));
    doc.apply(&plan.patch.inverse()).expect("undoes");
    assert_eq!(doc.root(), &before);
}

#[test]
fn marks_come_off_the_end_only() {
    let doc = doc_with_face(full_face());
    let middle = run(&doc, "face.remove_mark", json!({ "character": "mira", "index": 0 })).unwrap_err();
    assert_eq!(middle.field.as_deref(), Some("face/marks/0"));
    assert!(middle.hint.as_deref().unwrap_or_default().contains("remove mark 1 first"), "{middle:?}");

    let beyond = run(&doc, "face.remove_mark", json!({ "character": "mira", "index": 5 })).unwrap_err();
    assert!(beyond.message.contains("there is no mark 5"), "{beyond:?}");

    let empty = doc_with_face(json!({ "marks": [] }));
    let none = run(&empty, "face.remove_mark", json!({ "character": "mira", "index": 0 })).unwrap_err();
    assert_eq!(none.field.as_deref(), Some("face/marks"));
}

#[test]
fn describe_gives_the_schema_defaults_and_current_values_of_a_section() {
    let mut doc = doc_with_face(full_face());
    let plan = run(&doc, "face.describe_style", json!({ "character": "mira", "section": "eyes" })).expect("describes");
    let eyes = &plan.output.as_ref().expect("a query answers")["sections"]["eyes"];
    assert_eq!(eyes["path"], json!("face/eyes"));
    assert!(eyes["schema"]["properties"]["iris"].is_object(), "{eyes}");
    assert_eq!(eyes["defaults"]["scale"], json!(1.0));
    assert_eq!(eyes["current"]["iris"]["width"], json!(FaceProfile::default().eyes.iris.width));

    let set = run(&doc, "face.set_style", set_args("face/eyes/scale", json!(1.4))).expect("sets");
    doc.apply(&set.patch).expect("applies");
    let after = run(&doc, "face.describe_style", json!({ "character": "mira", "section": "eyes" })).expect("describes");
    assert_eq!(after.output.as_ref().expect("answers")["sections"]["eyes"]["current"]["scale"], json!(1.4));
    assert_eq!(plan.patch, Patch::new(), "a query changes nothing");
}

#[test]
fn describe_covers_every_section_and_the_marks_list() {
    let doc = doc_with_face(full_face());
    let all = run(&doc, "face.describe_style", json!({ "character": "mira" })).expect("describes");
    let sections = all.output.as_ref().expect("answers")["sections"].as_object().expect("an object").clone();
    assert_eq!(
        sections.keys().map(String::as_str).collect::<Vec<_>>(),
        ["brows", "cheeks", "ear3d", "eye3d", "eyes", "lips3d", "marks", "mouth", "nose", "nose3d"]
    );

    let marks = &sections["marks"];
    assert_eq!(marks["defaults"], json!([]));
    assert_eq!(marks["current"].as_array().map(Vec::len), Some(2));
    assert_eq!(marks["item_defaults"]["kind"], json!("beauty_mark"));
}

#[test]
fn describe_reports_each_numeric_fields_range_and_set_style_agrees() {
    let doc = doc_with_face(full_face());
    let all = run(&doc, "face.describe_style", json!({ "character": "mira" })).expect("describes");
    let sections = &all.output.as_ref().expect("answers")["sections"];
    let mut checked = 0;
    for name in ["eyes", "brows", "mouth", "nose", "cheeks"] {
        for range in sections[name]["ranges"].as_array().expect("a list of ranges") {
            let (path, lo, hi) =
                (range["path"].as_str().expect("path"), range["lo"].as_f64().unwrap(), range["hi"].as_f64().unwrap());
            let step = (hi - lo) * 1e-3;
            // A field with a rule between fields may fail that rule at an end; only its outside is checked.
            let ends: &[f64] = if range.get("note").is_some() { &[] } else { &[lo, hi] };
            for end in ends {
                let sets = run(&doc, "face.set_style", set_args(path, json!(end)));
                assert!(sets.is_ok(), "{path} refuses its own end {end}: {sets:?}");
            }
            for outside in [lo - step, hi + step] {
                let err = run(&doc, "face.set_style", set_args(path, json!(outside))).unwrap_err();
                assert!(err.message.contains(path), "{path} = {outside}: {err:?}");
            }
            checked += 1;
        }
    }
    assert!(checked > 40, "only {checked} ranges");
    let marks = &sections["marks"];
    assert!(
        marks["item_ranges"].as_array().expect("a list").iter().any(|r| r["path"] == json!("face/marks/<i>/opacity"))
    );
    let brow = sections["brows"]["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == json!("face/brows/outer_x"))
        .unwrap();
    assert!(brow["note"].as_str().unwrap().contains("farther from the midline"), "{brow}");
}

#[test]
fn describe_refuses_an_unknown_section_and_a_missing_character() {
    let doc = doc_with_face(full_face());
    let err = run(&doc, "face.describe_style", json!({ "character": "mira", "section": "mouht" })).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("face/mouht"));
    assert!(err.hint.as_deref().unwrap_or_default().contains("face/mouth"), "{err:?}");
    let missing = run(&doc, "face.describe_style", json!({ "character": "nobody" })).unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
}

/// A face that is already damaged in four places: an unknown key, a field out of range, a field of the wrong kind, and
/// a mark out of range.
fn damaged_face() -> Value {
    let mut face = full_face();
    face["eyes"]["lash"]["bogus"] = json!(1);
    face["eyes"]["iris"]["width"] = json!(5.0);
    face["brows"]["thickness"] = json!("thin");
    face["marks"][0]["opacity"] = json!(9.0);
    face
}

#[test]
fn a_list_or_a_wrong_kind_written_over_a_field_is_refused_by_name() {
    let mut doc = doc_with_face(full_face());
    let whole_section = run(&doc, "face.set_style", set_args("face/eyes/iris", json!([0.5, 0.5]))).unwrap_err();
    assert_eq!(whole_section.code, ErrorCode::Invalid);
    assert_eq!(whole_section.field.as_deref(), Some("face/eyes/iris"));
    assert!(whole_section.message.contains("expected a section, found a list"), "{}", whole_section.message);

    let section_as_number = run(&doc, "face.set_style", set_args("face/eyes", json!(1.0))).unwrap_err();
    assert_eq!(section_as_number.field.as_deref(), Some("face/eyes"));

    let short = run(&doc, "face.set_style", set_args("face/cheeks/radius", json!([0.1, 0.2, 0.3]))).unwrap_err();
    assert_eq!(short.field.as_deref(), Some("face/cheeks/radius"));
    assert!(short.message.contains("expected 2 items, found 3"), "{}", short.message);

    let leaf = run(&doc, "face.set_style", set_args("face/eyes/iris/width", json!([0.5]))).unwrap_err();
    assert_eq!(leaf.field.as_deref(), Some("face/eyes/iris/width"));

    // The refused writes left the face alone: a merge into the same section still applies cleanly.
    let good = run(&doc, "face.set_style", set_args("face/eyes/iris", json!({ "width": 0.5 }))).expect("a merge plans");
    doc.apply(&good.patch).expect("applies");
    assert_eq!(value_at(&doc, "characters/mira/face/eyes/iris/width"), json!(0.5));
    assert_eq!(value_at(&doc, "characters/mira/face/eyes/iris/height"), full_face()["eyes"]["iris"]["height"]);
    assert_eq!(value_at(&doc, "characters/mira/face/eyes/iris/top"), full_face()["eyes"]["iris"]["top"]);
}

#[test]
fn three_bad_fields_are_all_reported_together() {
    let doc = doc_with_face(full_face());
    let value = json!({ "scale": 99, "iris": { "width": "big", "bogus": 1 } });
    let err = run(&doc, "face.set_style", set_args("face/eyes", value)).unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert!(err.message.starts_with("3 fields are invalid"), "{}", err.message);
    for field in ["face/eyes/scale", "face/eyes/iris/width", "face/eyes/iris/bogus"] {
        assert!(err.message.contains(field), "{field} missing from: {}", err.message);
    }
    assert!(err.message.contains("expected a number, found text"), "{}", err.message);
    assert!(err.message.contains("there is no key 'bogus'"), "{}", err.message);
}

#[test]
fn damage_already_in_the_file_does_not_block_an_unrelated_edit() {
    let mut doc = doc_with_face(damaged_face());
    let plan = run(&doc, "face.set_style", set_args("face/brows/arch", json!(0.01))).expect("not blocked");
    assert_eq!(plan.patch.changes.len(), 1);
    assert_eq!(plan.patch.changes[0].path, path("characters/mira/face/brows/arch"));
    doc.apply(&plan.patch).expect("applies");
    assert_eq!(value_at(&doc, "characters/mira/face/eyes/lash/bogus"), json!(1), "the damage is left as it was");

    run(&doc, "face.set_style", set_args("face/eyes/iris/height", json!(0.9))).expect("not blocked");
    run(&doc, "face.set_style", set_args("face/marks/1/opacity", json!(0.5))).expect("not blocked");
    run(&doc, "face.add_mark", json!({ "character": "mira", "mark": { "kind": "scar" } })).expect("not blocked");

    // A new bad value in the damaged field is the edit's own fault, so it is refused.
    let again = run(&doc, "face.set_style", set_args("face/eyes/iris/width", json!(6.0))).unwrap_err();
    assert_eq!(again.field.as_deref(), Some("face/eyes/iris/width"));
    assert!(again.message.contains("6 is out of range"), "{}", again.message);

    // A good value repairs it.
    let fixed = run(&doc, "face.set_style", set_args("face/eyes/iris/width", json!(0.5))).expect("accepted");
    doc.apply(&fixed.patch).expect("applies");
}

#[test]
fn an_edit_inside_a_value_that_is_not_a_section_is_refused_at_that_value() {
    let mut face = full_face();
    face["eyes"] = json!(5);
    let doc = doc_with_face(face);
    let err = run(&doc, "face.set_style", set_args("face/eyes/iris/width", json!(0.5))).unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert_eq!(err.field.as_deref(), Some("face/eyes"));
}

#[test]
fn the_add_mark_hint_is_shown_only_for_the_marks_list() {
    let doc = doc_with_face(full_face());
    let radius = run(&doc, "face.set_style", set_args("face/cheeks/radius/9", json!(0.1))).unwrap_err();
    assert_eq!(radius.field.as_deref(), Some("face/cheeks/radius/9"));
    let hint = radius.hint.unwrap_or_default();
    assert!(!hint.contains("add_mark"), "{hint}");
    assert!(hint.contains("0 to 1"), "{hint}");

    let mark = run(&doc, "face.set_style", set_args("face/marks/9/size", json!(0.01))).unwrap_err();
    assert!(mark.hint.unwrap_or_default().contains("face.add_mark"));
}

/// The three repro paths of #125: a bad enum name, colour and integer are named at their leaf, not at the section.
#[test]
fn enum_colour_and_integer_errors_name_their_leaf() {
    let doc = doc_with_face(full_face());
    for (path, value, leaf, text) in [
        ("face/nose/kind", json!("dott"), "face/nose/kind", "unknown variant `dott`"),
        ("face/eyes/lash/colour", json!("#zzzzzz"), "face/eyes/lash/colour", ""),
        ("face/marks/0/count", json!(300), "face/marks/0/count", "300"),
    ] {
        let err = run(&doc, "face.set_style", set_args(path, value)).unwrap_err();
        assert_eq!(err.field.as_deref(), Some(leaf), "{err:?}");
        assert!(err.message.contains(text) && err.message.contains(leaf), "{err:?}");
    }
}

#[test]
fn every_unreadable_leaf_is_listed_with_the_others() {
    let doc = doc_with_face(full_face());
    let value = json!({ "lash": { "colour": "#zzzzzz" }, "iris": { "pupil": { "shape": "roung", "size": 5.0 } } });
    let err = run(&doc, "face.set_style", set_args("face/eyes", value)).unwrap_err();
    for expected in ["face/eyes/lash/colour", "face/eyes/iris/pupil/shape", "face/eyes/iris/pupil/size"] {
        assert!(err.message.contains(expected), "{expected} missing: {}", err.message);
    }
    assert!(err.message.starts_with("3 fields are invalid"), "{}", err.message);
}

#[test]
fn a_suggestion_is_made_only_when_a_name_is_close() {
    let doc = doc_with_face(full_face());
    let far = run(&doc, "face.set_style", set_args("face/eyes/iris/bogus", json!(1))).unwrap_err();
    assert!(!far.hint.as_deref().unwrap_or_default().contains("did you mean"), "{far:?}");
    let near = run(&doc, "face.set_style", set_args("face/eyes/iris/widht", json!(0.5))).unwrap_err();
    assert!(near.hint.as_deref().unwrap_or_default().contains("did you mean `face/eyes/iris/width`"), "{near:?}");
    let layout = run(&doc, "face.set_style", set_args("face/layout/eye_y", json!(0.0))).unwrap_err();
    assert!(layout.message.contains("not a style section"), "{layout:?}");
    assert!(!layout.hint.as_deref().unwrap_or_default().contains("did you mean"), "{layout:?}");
    // Every other part of the face says the same, known from the profile itself rather than from a list of names.
    for part in ["mapping", "rest", "expressions", "grammar"] {
        let err = run(&doc, "face.set_style", set_args(&format!("face/{part}/x"), json!(0.0))).unwrap_err();
        assert!(err.message.contains("not a style section"), "{part}: {err:?}");
    }
}

/// #637: "no fangs" is a value an agent can write and read back.
#[test]
fn a_character_can_be_given_no_fangs_through_the_style_ops() {
    let mut doc = doc_with_face(full_face());
    let plan = run(&doc, "face.set_style", set_args("face/mouth/fang/kind", json!("none"))).expect("sets");
    doc.apply(&plan.patch).expect("applies");
    assert_eq!(value_at(&doc, "characters/mira/face/mouth/fang/kind"), json!("none"));
    let mouth =
        run(&doc, "face.describe_style", json!({ "character": "mira", "section": "mouth" })).expect("describes");
    let mouth = &mouth.output.as_ref().expect("answers")["sections"]["mouth"];
    assert_eq!(mouth["current"]["fang"]["kind"], json!("none"));
    assert_eq!(mouth["defaults"]["fang"]["kind"], json!("both"));
    let err = run(&doc, "face.set_style", set_args("face/mouth/fang/kind", json!("sharp"))).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("face/mouth/fang/kind"), "{err:?}");
}
