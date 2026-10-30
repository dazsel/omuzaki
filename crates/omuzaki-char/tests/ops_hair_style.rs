//! `hair.style.list` and `hair.style.apply` (#2486): the library's styles are listed, and applying one replaces the hair's
//! groups and volume in one undoable edit, keeps its shading, and refuses a style the library doesn't hold by name.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod common;

use common::{assert_error, at, refused, round_trip};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::json;

const PARAMS: &str = "characters/kit/parts/hair/params";

fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    doc
}

#[test]
fn list_shows_every_style_with_its_name_and_groups() {
    let doc = kit();
    let answer = common::plan(&doc, "hair.style.list", json!({})).unwrap().output.unwrap();
    let listed = answer["styles"].as_array().unwrap();
    assert!(listed.len() >= 10, "{answer}");
    let bob = listed.iter().find(|s| s["id"] == "bob").unwrap();
    assert_eq!(bob["name"], "Bob");
    assert!(bob["groups"].as_array().unwrap().iter().any(|g| g == "c-curtain"), "{bob}");
}

#[test]
fn apply_cuts_the_hair_in_a_style_undoably_and_keeps_its_shading() {
    let mut doc = kit();
    common::apply(
        &mut doc,
        "part.set_param",
        json!({"character": "kit", "part": "hair", "path": "shading/ring_strength", "value": 0.3}),
    );
    round_trip(&mut doc, "hair.style.apply", json!({"character": "kit", "part": "hair", "style": "twin-tails"}));
    assert!(at(&doc, &format!("{PARAMS}/groups/e-tails")).is_some());
    assert_eq!(at(&doc, &format!("{PARAMS}/groups/c-curtain")), None, "twin tails have no curtain");
    assert_eq!(at(&doc, &format!("{PARAMS}/shading/ring_strength")), Some(json!(0.3)), "the shading is kept");
    round_trip(&mut doc, "hair.style.apply", json!({"character": "kit", "part": "hair", "style": "bob"}));
    assert_eq!(at(&doc, &format!("{PARAMS}/groups/e-tails")), None);
    assert!(at(&doc, &format!("{PARAMS}/groups/c-curtain/generator/drape_deg")).is_some());
}

#[test]
fn apply_refuses_a_style_the_library_does_not_hold() {
    let doc = kit();
    let e = refused(&doc, "hair.style.apply", json!({"character": "kit", "part": "hair", "style": "bobb"}));
    assert_error(&e, ErrorCode::NotFound, "style", &["bobb"], &["bob", "hair.style.list"]);
    let e = refused(&doc, "hair.style.apply", json!({"character": "kit", "part": "body", "style": "bob"}));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("part")), "{e:?}");
}
