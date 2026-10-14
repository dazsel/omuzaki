//! The `variant.*` ops against the in-memory document: `variant.create`, `set_override`, `clear_override`, `delete`,
//! `list` and `preview` (spec CB-13).
//!
//! Each op is decoded and planned through the shared harness in `common`, which spells out the editing contract every
//! edit keeps: a dry-run changes nothing, applying the patch changes the document, its inverse restores it exactly,
//! applying it again gives the same document, and no written value holds a `null`.

mod common;

use common::{apply, assert_error, at, plan, project, refused, round_trip};
use omuzaki_char::ops::variant::{
    clear_override::VariantClearOverride, create::VariantCreate, delete::VariantDelete, list::VariantList,
    preview::VariantPreview, set_override::VariantSetOverride,
};
use omuzaki_doc_api::{DocPath, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, Op, find};
use serde_json::{Value, json};

const OPS: [&str; 6] = [
    "variant.create",
    "variant.set_override",
    "variant.clear_override",
    "variant.delete",
    "variant.list",
    "variant.preview",
];

/// A project with the `game` and `close-up` variants of the mock character, the close-up with the four overrides the
/// contract's example names: a longer chin, a lower mouth, performing eyes and the fringe on.
fn with_game_and_close_up() -> MemDoc {
    let mut doc = project();
    apply(&mut doc, "variant.create", json!({"character": "mock", "id": "game", "shot": {"min": 0.0, "max": 0.35}}));
    apply(
        &mut doc,
        "variant.create",
        json!({"character": "mock", "id": "close-up", "extends": "game", "shot": {"min": 0.35, "max": 5.0}}),
    );
    for (path, value) in close_up_overrides() {
        apply(
            &mut doc,
            "variant.set_override",
            json!({"character": "mock", "variant": "close-up", "path": path, "value": value}),
        );
    }
    doc
}

/// The close-up's overrides, as the contract's example writes them.
fn close_up_overrides() -> [(&'static str, Value); 4] {
    [
        ("parts/head/params/outline/chin", json!({"y": -0.3, "stretch_top": -0.08, "min_ratio": 0.6})),
        ("face/layout/mouth_y", json!(-0.2)),
        ("face/mapping/rest/eye_open", json!(1.3)),
        ("parts/hair/params/groups/b-bangs", json!({"generator": {"type": "bangs"}})),
    ]
}

// ------------------------------------------------------------------------------------------------ the ops

#[test]
fn every_variant_op_is_registered_under_its_name() {
    // a test binary that names none of an op module's types drops that module's registrations, so the names
    // are used here to keep them. Remove this once the registrations survive linking.
    let _ = [
        VariantCreate::meta().name,
        VariantSetOverride::meta().name,
        VariantClearOverride::meta().name,
        VariantDelete::meta().name,
        VariantList::meta().name,
        VariantPreview::meta().name,
    ];
    for op in OPS {
        assert!(find(op).is_some(), "{op} isn't registered");
    }
}

#[test]
fn create_round_trips_and_refuses_what_it_cannot_make() {
    let mut doc = project();
    round_trip(
        &mut doc,
        "variant.create",
        json!({"character": "mock", "id": "game", "shot": {"min": 0.0, "max": 0.35}}),
    );
    round_trip(
        &mut doc,
        "variant.create",
        json!({"character": "mock", "id": "close-up", "extends": "game", "shot": {"min": 0.35, "max": 5.0}}),
    );
    assert_eq!(at(&doc, "characters/mock/variants/close-up/extends"), Some(json!("game")));
    assert_eq!(
        at(&doc, "characters/mock/variants/game/overrides"),
        Some(json!({})),
        "a new variant has no overrides yet"
    );

    let e = refused(&doc, "variant.create", json!({"character": "mock", "id": "game"}));
    assert_error(&e, ErrorCode::Invalid, "id", &["already exists"], &["variant.set_override"]);
    let e = refused(&doc, "variant.create", json!({"character": "mock", "id": "emote", "extends": "nope"}));
    assert_error(&e, ErrorCode::NotFound, "extends", &["no variant `nope`"], &["game", "close-up"]);
    let e =
        refused(&doc, "variant.create", json!({"character": "mock", "id": "emote", "shot": {"min": 0.5, "max": 0.5}}));
    assert_error(&e, ErrorCode::Invalid, "shot.max", &["not above its start"], &["min 0.35"]);
    let e =
        refused(&doc, "variant.create", json!({"character": "mock", "id": "emote", "shot": {"min": -0.1, "max": 1.0}}));
    assert_error(&e, ErrorCode::Invalid, "shot.min", &["not a head coverage"], &["0"]);
    let e = refused(&doc, "variant.create", json!({"character": "ghost", "id": "emote"}));
    assert_error(&e, ErrorCode::NotFound, "character", &["no character `ghost`"], &["mock"]);
}

#[test]
fn set_override_round_trips_the_close_up_and_names_what_it_refuses() {
    let mut doc = project();
    apply(&mut doc, "variant.create", json!({"character": "mock", "id": "game"}));
    apply(&mut doc, "variant.create", json!({"character": "mock", "id": "close-up", "extends": "game"}));
    for (path, value) in close_up_overrides() {
        round_trip(
            &mut doc,
            "variant.set_override",
            json!({"character": "mock", "variant": "close-up", "path": path, "value": value}),
        );
    }
    assert_eq!(
        at(&doc, "characters/mock/variants/close-up/overrides/face/layout/mouth_y"),
        None,
        "keys are whole paths, not nested"
    );
    assert_eq!(at(&doc, "characters/mock/variants/close-up/overrides").unwrap().as_object().unwrap().len(), 4);
    assert_eq!(at(&doc, "characters/mock/variants/game/overrides"), Some(json!({})), "the game variant is untouched");

    let set = |path: &str| json!({"character": "mock", "variant": "close-up", "path": path, "value": 0.1});
    let e = refused(&doc, "variant.set_override", set("face/layoutt/mouth_y"));
    assert_error(
        &e,
        ErrorCode::Invalid,
        "path",
        &["close-up", "face/layoutt/mouth_y", "layoutt"],
        &["variant.clear_override"],
    );
    let hint = refused(&doc, "variant.set_override", set("face/layoutt/mouth_y")).hint.unwrap();
    assert!(hint.starts_with("did you mean `face/layout/mouth_y`?"), "the misspelt field is corrected: {hint}");
    let e = refused(&doc, "variant.set_override", set("parts/nope/enabled"));
    assert_error(&e, ErrorCode::NotFound, "path", &["parts/nope/enabled", "no part `nope`"], &["head", "hair"]);
    assert!(!e.hint.unwrap().contains("did you mean"), "no part is close to `nope`");
    let e = refused(&doc, "variant.set_override", set("parts/hed/params/outline/chin"));
    assert_error(
        &e,
        ErrorCode::NotFound,
        "path",
        &["no part `hed`"],
        &["did you mean `head`?", "the parts of `mock` are"],
    );
    let e = refused(
        &doc,
        "variant.set_override",
        json!({"character": "mock", "variant": "close-up", "path": "parts/head/params/outlien/chin", "value": {"y": 0.0}}),
    );
    assert_error(
        &e,
        ErrorCode::Invalid,
        "path",
        &["parts/head/params/outlien/chin", "close-up", "outlien"],
        &["parameter"],
    );
    let e = refused(
        &doc,
        "variant.set_override",
        json!({"character": "mock", "variant": "close-up", "path": "rig/bones", "value": 1}),
    );
    assert_error(&e, ErrorCode::Invalid, "path", &["variant 'close-up'", "rig/bones"], &["variant.clear_override"]);
    let e = refused(
        &doc,
        "variant.set_override",
        json!({"character": "mock", "variant": "close-up", "path": "", "value": 1}),
    );
    assert_error(&e, ErrorCode::Invalid, "path", &["empty"], &["face/layout/mouth_y"]);
    let e = refused(
        &doc,
        "variant.set_override",
        json!({"character": "mock", "variant": "close-up", "path": "face/layout/mouth_y", "value": null}),
    );
    assert_error(&e, ErrorCode::Invalid, "value", &["null"], &["variant.clear_override"]);
    let e = refused(
        &doc,
        "variant.set_override",
        json!({"character": "mock", "variant": "emote", "path": "face/layout/mouth_y", "value": 1}),
    );
    assert_error(&e, ErrorCode::NotFound, "variant", &["no variant `emote`"], &["close-up"]);
}

#[test]
fn clear_override_round_trips_and_drops_an_empty_table() {
    let mut doc = with_game_and_close_up();
    round_trip(
        &mut doc,
        "variant.clear_override",
        json!({"character": "mock", "variant": "close-up", "path": "face/layout/mouth_y"}),
    );
    assert!(at(&doc, "characters/mock/variants/close-up/overrides/face/layout/mouth_y").is_none());

    apply(&mut doc, "variant.create", json!({"character": "mock", "id": "emote", "extends": "game"}));
    apply(
        &mut doc,
        "variant.set_override",
        json!({"character": "mock", "variant": "emote", "path": "face/layout/nose_y", "value": -0.1}),
    );
    round_trip(
        &mut doc,
        "variant.clear_override",
        json!({"character": "mock", "variant": "emote", "path": "face/layout/nose_y"}),
    );
    assert_eq!(
        at(&doc, "characters/mock/variants/emote/overrides"),
        None,
        "the last override leaves no empty table behind"
    );

    let e = refused(
        &doc,
        "variant.clear_override",
        json!({"character": "mock", "variant": "close-up", "path": "face/layout/nose_y"}),
    );
    assert_error(
        &e,
        ErrorCode::NotFound,
        "path",
        &["no override `face/layout/nose_y`"],
        &["face/mapping/rest/eye_open"],
    );
}

#[test]
fn delete_round_trips_and_is_refused_while_a_variant_extends_it() {
    let mut doc = with_game_and_close_up();
    let e = refused(&doc, "variant.delete", json!({"character": "mock", "variant": "game"}));
    assert_error(&e, ErrorCode::Invalid, "variant", &["can't delete `game`", "close-up"], &["close-up"]);

    round_trip(&mut doc, "variant.delete", json!({"character": "mock", "variant": "close-up"}));
    assert!(at(&doc, "characters/mock/variants/close-up").is_none());
    round_trip(&mut doc, "variant.delete", json!({"character": "mock", "variant": "game"}));
    assert!(
        at(&doc, "characters/mock/variants").is_some_and(|v| v.as_object().unwrap().is_empty()),
        "the table stays, empty"
    );
}

#[test]
fn list_names_each_variant_and_what_it_extends() {
    let doc = with_game_and_close_up();
    let planned = plan(&doc, "variant.list", json!({"character": "mock"})).unwrap();
    let output = planned.output.unwrap();
    assert!(planned.patch.is_empty(), "a query writes nothing");
    assert_eq!(output["count"], json!(2));
    let names: Vec<&str> =
        output["variants"].as_array().unwrap().iter().map(|v| v["variant"].as_str().unwrap()).collect();
    assert_eq!(names, ["close-up", "game"]);
    assert_eq!(output["variants"][0]["extends"], json!("game"));
    assert_eq!(output["variants"][0]["overrides"], json!(4));
    assert_eq!(output["variants"][1]["overrides"], json!(0));
    assert_eq!(output["variants"][0]["shot"], json!({"min": 0.35, "max": 5.0}));
}

#[test]
fn preview_shows_the_close_up_against_the_base_and_writes_nothing() {
    let doc = with_game_and_close_up();
    let before = doc.root().clone();
    let output =
        plan(&doc, "variant.preview", json!({"character": "mock", "variant": "close-up"})).unwrap().output.unwrap();
    assert_eq!(doc.root(), &before, "a preview writes nothing");

    let entries = output["changes"].as_array().unwrap();
    let changed: Vec<&str> = entries.iter().map(|c| c["path"].as_str().unwrap()).collect();
    for path in ["face/layout/mouth_y", "face/mapping/rest/eye_open", "parts/hair/params/groups/b-bangs"] {
        assert!(changed.contains(&path), "the close-up changes {path}: {changed:?}");
    }
    // The base has no `outline` table of its own, so the chin arrives as the whole table.
    let outline = entries.iter().find(|c| c["path"] == "parts/head/params/outline").unwrap();
    assert_eq!(outline["after"]["chin"]["y"], json!(-0.3), "the longer chin: {outline}");
    let mouth = entries.iter().find(|c| c["path"] == "face/layout/mouth_y").unwrap();
    assert_eq!(mouth["after"], json!(-0.2));
    assert!(mouth.get("before").is_some_and(Value::is_number), "the base mouth height is shown too: {mouth}");

    let game = plan(&doc, "variant.preview", json!({"character": "mock", "variant": "game"})).unwrap().output.unwrap();
    assert_eq!(game["count"], json!(0), "the game variant is the base character");
}

#[test]
fn a_null_is_never_written_and_an_old_null_reads_as_absent() {
    // A file written before the character format stopped writing nulls holds `extends` and `shot` as null. It reads
    // as absent, and an edit writes no null back.
    let mut doc = project();
    let old = json!({"old": {"description": "", "extends": null, "overrides": {}, "shot": null}});
    doc.apply(&Patch::new().with(DocPath::parse("characters/mock/variants").unwrap(), None, Some(old))).unwrap();

    let listed = plan(&doc, "variant.list", json!({"character": "mock"})).unwrap().output.unwrap();
    assert!(listed["variants"][0].get("shot").is_none(), "a null shot reads as no shot: {listed}");
    let preview =
        plan(&doc, "variant.preview", json!({"character": "mock", "variant": "old"})).unwrap().output.unwrap();
    assert_eq!(preview["count"], json!(0), "the variant is the base character: {preview}");
    round_trip(
        &mut doc,
        "variant.set_override",
        json!({"character": "mock", "variant": "old", "path": "face/layout/mouth_y", "value": -0.2}),
    );

    // A null given as an override value is refused: there is nothing to store that means "no value".
    let e = refused(
        &doc,
        "variant.set_override",
        json!({"character": "mock", "variant": "old", "path": "face/layout/nose_y", "value": null}),
    );
    assert_error(&e, ErrorCode::Invalid, "value", &["null"], &["variant.clear_override"]);
}
