//! `hair.flow.set` and `hair.volume.set` (#2478): set the hair's shared parting, whorl and fullness in one undoable edit
//! each, keep the fields they aren't given, and refuse an unknown field or a value out of range by name.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod common;

use common::{assert_error, at, refused, round_trip};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::json;

const VOLUME: &str = "characters/kit/parts/hair/params/volume";

fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    doc
}

fn hair(args: serde_json::Value) -> serde_json::Value {
    let mut base = json!({"character": "kit", "part": "hair"});
    let serde_json::Value::Object(fields) = args else { panic!("args are an object") };
    base.as_object_mut().unwrap().extend(fields);
    base
}

#[test]
fn flow_set_parts_the_hair_and_moves_the_whorl_undoably() {
    let mut doc = kit();
    assert_eq!(at(&doc, VOLUME), None, "no shared volume until one is set");
    round_trip(&mut doc, "hair.flow.set", hair(json!({"parting": {"side_deg": 20}})));
    assert_eq!(at(&doc, &format!("{VOLUME}/part/side_deg")), Some(json!(20.0)));
    assert_eq!(at(&doc, &format!("{VOLUME}/part/front_deg")), Some(json!(60.0)), "the parting starts at its defaults");
    round_trip(&mut doc, "hair.flow.set", hair(json!({"whorl": {"polar_deg": 35}})));
    assert_eq!(at(&doc, &format!("{VOLUME}/whorl/polar_deg")), Some(json!(35.0)));
    assert_eq!(at(&doc, &format!("{VOLUME}/part/side_deg")), Some(json!(20.0)), "the parting is kept");
    round_trip(&mut doc, "hair.flow.set", hair(json!({"parting": {"back_deg": 45}})));
    assert_eq!(at(&doc, &format!("{VOLUME}/part/side_deg")), Some(json!(20.0)), "the parting's other fields are kept");
    round_trip(&mut doc, "hair.flow.set", hair(json!({"parting": "none"})));
    assert_eq!(at(&doc, &format!("{VOLUME}/part")), None);
    assert_eq!(at(&doc, &format!("{VOLUME}/whorl/polar_deg")), Some(json!(35.0)), "the whorl is kept");
}

#[test]
fn volume_set_changes_the_fullness_and_clear_drops_the_shared_volume() {
    let mut doc = kit();
    round_trip(&mut doc, "hair.volume.set", hair(json!({"fullness": 1.3, "crown": 1.4})));
    assert_eq!(at(&doc, &format!("{VOLUME}/fullness")), Some(json!(1.3)));
    assert_eq!(at(&doc, &format!("{VOLUME}/crown")), Some(json!(1.4)));
    round_trip(&mut doc, "hair.volume.set", hair(json!({"back": 0.8})));
    assert_eq!(at(&doc, &format!("{VOLUME}/fullness")), Some(json!(1.3)), "the other fields are kept");
    round_trip(&mut doc, "hair.volume.set", hair(json!({"clear": true})));
    assert_eq!(at(&doc, VOLUME), None);
}

#[test]
fn the_volume_ops_refuse_what_the_volume_cannot_be() {
    let doc = kit();
    let e = refused(&doc, "hair.flow.set", hair(json!({})));
    assert_error(&e, ErrorCode::Invalid, "parting", &["parting", "whorl"], &["side_deg"]);
    let e = refused(&doc, "hair.flow.set", hair(json!({"parting": {"sid_deg": 10}})));
    assert_error(&e, ErrorCode::Invalid, "parting/sid_deg", &["sid_deg"], &["side_deg"]);
    let e = refused(&doc, "hair.flow.set", hair(json!({"parting": {"side_deg": 90}})));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("parting")), "{e:?}");
    let e = refused(&doc, "hair.flow.set", hair(json!({"parting": "left"})));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("parting")), "{e:?}");
    let e = refused(&doc, "hair.flow.set", hair(json!({"whorl": {"polar_deg": 80}})));
    assert_eq!(e.code, ErrorCode::Invalid, "{e:?}");
    let e = refused(&doc, "hair.volume.set", hair(json!({})));
    assert_error(&e, ErrorCode::Invalid, "fullness", &["fullness"], &["fullness"]);
    let e = refused(&doc, "hair.volume.set", hair(json!({"clear": true, "fullness": 1.2})));
    assert_error(&e, ErrorCode::Invalid, "fullness", &["clear"], &[]);
    let e = refused(&doc, "hair.volume.set", hair(json!({"fullness": 3.0})));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("fullness")), "{e:?}");
}
