//! `hair.group.set_lock` (#2480): shapes a group's locks in one undoable edit, keeps the fields it isn't given, and
//! refuses an unknown field or a value out of range by name.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod common;

use common::{assert_error, at, refused, round_trip};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::json;

const LOCK: &str = "characters/kit/parts/hair/params/groups/b-bangs/lock";

fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    doc
}

#[test]
fn set_lock_shapes_a_group_undoably_and_keeps_the_other_fields() {
    let mut doc = kit();
    assert_eq!(at(&doc, LOCK), None, "the default shape is left out of the file");
    round_trip(
        &mut doc,
        "hair.group.set_lock",
        json!({"character": "kit", "part": "hair", "group": "b-bangs", "lock": {"tip": "fork", "split": 2}}),
    );
    assert_eq!(at(&doc, &format!("{LOCK}/tip")), Some(json!("fork")));
    assert_eq!(at(&doc, &format!("{LOCK}/split")), Some(json!(2)));
    round_trip(
        &mut doc,
        "hair.group.set_lock",
        json!({"character": "kit", "part": "hair", "group": "b-bangs", "lock": {"profile": "card"}}),
    );
    assert_eq!(at(&doc, &format!("{LOCK}/profile")), Some(json!("card")));
    assert_eq!(at(&doc, &format!("{LOCK}/tip")), Some(json!("fork")), "the other fields are kept");
}

#[test]
fn set_lock_refuses_what_a_lock_cannot_be() {
    let doc = kit();
    let set = |lock| json!({"character": "kit", "part": "hair", "group": "b-bangs", "lock": lock});
    let e = refused(&doc, "hair.group.set_lock", set(json!({"tipp": "fork"})));
    assert_error(&e, ErrorCode::Invalid, "lock/tipp", &["tipp"], &["tip"]);
    let e = refused(&doc, "hair.group.set_lock", set(json!({"split": 7})));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("lock")), "{e:?}");
    let e = refused(&doc, "hair.group.set_lock", set(json!({"tip": "spiral"})));
    assert_eq!(e.code, ErrorCode::Invalid, "{e:?}");
    let e = refused(&doc, "hair.group.set_lock", set(json!({})));
    assert_error(&e, ErrorCode::Invalid, "lock", &["object"], &["tip"]);
    let e = refused(
        &doc,
        "hair.group.set_lock",
        json!({"character": "kit", "part": "hair", "group": "z-none", "lock": {"tip": "blunt"}}),
    );
    assert_error(&e, ErrorCode::NotFound, "group", &["z-none"], &["b-bangs"]);
}
