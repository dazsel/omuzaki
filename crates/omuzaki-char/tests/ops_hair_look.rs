//! `hair.colour.natural` (#2524), `hair.strays.set` (#2516) and the new shading fields of `hair.set_shading` (#2521,
//! #2525, #2526): each one undoable edit that keeps what it isn't given and refuses a value out of range by name.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod common;

use common::{assert_error, at, refused, round_trip};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::json;

const SHADING: &str = "characters/kit/parts/hair/params/shading";
const STRAYS: &str = "characters/kit/parts/hair/params/strays";

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

fn luma(hex: &serde_json::Value) -> f64 {
    let s = hex.as_str().unwrap().trim_start_matches('#');
    let channel = |i: usize| f64::from(u8::from_str_radix(&s[i..i + 2], 16).unwrap());
    0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
}

#[test]
fn natural_colour_writes_every_hair_colour_and_keeps_its_pigment() {
    let mut doc = kit();
    round_trip(&mut doc, "hair.colour.natural", hair(json!({"melanin": 0.8, "redness": 0.2})));
    let dark = at(&doc, &format!("{SHADING}/mid")).unwrap();
    assert_eq!(at(&doc, &format!("{SHADING}/pigment/melanin")), Some(json!(0.8)));
    assert!(luma(&dark) < 70.0, "melanin 0.8 is dark brown: {dark}");
    for field in ["root", "tip", "shade", "line", "outline", "ring", "rim"] {
        assert!(at(&doc, &format!("{SHADING}/{field}")).is_some(), "{field} is written");
    }
    assert!(luma(&at(&doc, &format!("{SHADING}/root")).unwrap()) < luma(&dark), "darker roots");

    round_trip(&mut doc, "hair.colour.natural", hair(json!({"melanin": 0.1})));
    let light = at(&doc, &format!("{SHADING}/mid")).unwrap();
    assert!(luma(&light) > 150.0, "melanin 0.1 is blond: {light}");
    assert_eq!(at(&doc, &format!("{SHADING}/pigment/redness")), Some(json!(0.2)), "redness is kept");

    round_trip(&mut doc, "hair.colour.natural", hair(json!({"dye": "#3060e0", "dye_strength": 1.0})));
    let dyed = at(&doc, &format!("{SHADING}/mid")).unwrap();
    let s = dyed.as_str().unwrap();
    let (r, b) = (u8::from_str_radix(&s[1..3], 16).unwrap(), u8::from_str_radix(&s[5..7], 16).unwrap());
    assert!(b > r, "blond dyed blue reads blue: {dyed}");
    round_trip(&mut doc, "hair.colour.natural", hair(json!({"dye": "none"})));
    assert_eq!(at(&doc, &format!("{SHADING}/pigment/dye")), None);
    assert_eq!(at(&doc, &format!("{SHADING}/mid")), Some(light), "no dye is the natural colour again");
}

#[test]
fn strays_set_adds_baby_hairs_and_reset_puts_back_the_defaults() {
    let mut doc = kit();
    assert_eq!(at(&doc, STRAYS), None, "the default strays are left out");
    round_trip(&mut doc, "hair.strays.set", hair(json!({"baby": 40, "frizz": 0.5})));
    assert_eq!(at(&doc, &format!("{STRAYS}/baby")), Some(json!(40)));
    assert_eq!(at(&doc, &format!("{STRAYS}/frizz")), Some(json!(0.5)));
    round_trip(&mut doc, "hair.strays.set", hair(json!({"share": 0.0})));
    assert_eq!(at(&doc, &format!("{STRAYS}/baby")), Some(json!(40)), "the other fields are kept");
    round_trip(&mut doc, "hair.strays.set", hair(json!({"reset": true})));
    assert_eq!(at(&doc, STRAYS), None);
}

#[test]
fn set_shading_takes_the_highlights_the_deep_shade_the_ring_shape_and_the_occlusion() {
    let mut doc = kit();
    for (field, value) in [
        ("highlight_strength", json!(0.8)),
        ("highlight_cel", json!(1.0)),
        ("highlight", json!("#ffe0f0")),
        ("shade_deep", json!("#c0a0c0")),
        ("ring_shape", json!("zigzag")),
        ("occlusion", json!(0.9)),
    ] {
        round_trip(&mut doc, "hair.set_shading", hair(json!({"field": field, "value": value})));
        assert_eq!(at(&doc, &format!("{SHADING}/{field}")), Some(value), "{field}");
    }
    let e = refused(&doc, "hair.set_shading", hair(json!({"field": "ring_shape", "value": "star"})));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("value")), "{e:?}");
    let e = refused(&doc, "hair.set_shading", hair(json!({"field": "occlusion_reach", "value": 0.5})));
    assert_eq!(e.code, ErrorCode::Invalid, "{e:?}");
    let e = refused(&doc, "hair.set_shading", hair(json!({"field": "pigment", "value": {"melanin": 0.5}})));
    assert_error(&e, ErrorCode::Invalid, "field", &["pigment"], &["highlight"]);
}

#[test]
fn the_look_ops_refuse_what_the_hair_cannot_be() {
    let doc = kit();
    let e = refused(&doc, "hair.colour.natural", hair(json!({"melanin": 1.5})));
    assert_error(&e, ErrorCode::Invalid, "melanin", &["1.5"], &[]);
    let e = refused(&doc, "hair.colour.natural", hair(json!({"dye": "blue"})));
    assert_error(&e, ErrorCode::Invalid, "dye", &["blue"], &["none"]);
    let e = refused(&doc, "hair.strays.set", hair(json!({})));
    assert_error(&e, ErrorCode::Invalid, "share", &["baby"], &["frizz"]);
    let e = refused(&doc, "hair.strays.set", hair(json!({"reset": true, "baby": 3})));
    assert_error(&e, ErrorCode::Invalid, "share", &["reset"], &[]);
    let e = refused(&doc, "hair.strays.set", hair(json!({"baby": 500})));
    assert_eq!(e.code, ErrorCode::Invalid, "{e:?}");
}
