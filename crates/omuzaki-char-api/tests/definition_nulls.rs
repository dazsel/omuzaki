//! No `null` reaches a project file from the definition types: TOML has no null, so an absent option must be an
//! absent key. Every optional field of the types the part kinds' `defaults` and `check` serialise is covered, and files
//! that stored `null` before still read.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used)]

use omuzaki_char_api::{BodyShape, HeadParams, VariantDef};
use omuzaki_store_api::ContentHash;
use serde::Serialize;
use serde_json::{Value, json};

/// The JSON path of the first `null` in `value`, if any.
fn first_null(value: &Value, at: &str) -> Option<String> {
    match value {
        Value::Null => Some(at.to_owned()),
        Value::Array(items) => items.iter().enumerate().find_map(|(i, v)| first_null(v, &format!("{at}/{i}"))),
        Value::Object(map) => map.iter().find_map(|(k, v)| first_null(v, &format!("{at}/{k}"))),
        _ => None,
    }
}

fn stored<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("the definition types serialise")
}

#[test]
fn the_defaults_of_every_definition_type_hold_no_null() {
    for (name, value) in [
        ("HeadParams", stored(&HeadParams::default())),
        ("BodyShape", stored(&BodyShape::default())),
        ("VariantDef", stored(&VariantDef::default())),
    ] {
        assert_eq!(first_null(&value, name), None, "{value}");
    }
}

#[test]
fn optional_fields_that_hold_a_value_are_stored_and_read_back() {
    let hash = stored(&ContentHash::of(b"traced head"));
    let head: HeadParams = serde_json::from_value(json!({
        "outline": {"chin": {}},
        "traced": {"asset": hash, "theta_steps": 4, "phi_steps": 8, "mix": 0.5},
        "preset": "round-u",
    }))
    .unwrap();
    let body: BodyShape = serde_json::from_value(json!({"preset": "slim"})).unwrap();
    let variant: VariantDef =
        serde_json::from_value(json!({"extends": "base", "shot": {"min": 0.1, "max": 0.5}})).unwrap();
    let (head_json, body_json, variant_json) = (stored(&head), stored(&body), stored(&variant));
    assert_eq!(head_json["preset"], "round-u");
    assert_eq!(head_json["outline"]["chin"]["y"], json!(-0.21));
    assert_eq!(head_json["traced"]["theta_steps"], 4);
    assert_eq!(body_json["preset"], "slim");
    assert_eq!((variant_json["extends"].as_str(), variant_json["shot"]["max"].as_f64()), (Some("base"), Some(0.5)));
    assert_eq!(serde_json::from_value::<HeadParams>(head_json).unwrap(), head);
    assert_eq!(serde_json::from_value::<BodyShape>(body_json).unwrap(), body);
    assert_eq!(serde_json::from_value::<VariantDef>(variant_json).unwrap(), variant);
}

#[test]
fn files_that_stored_null_for_an_absent_option_still_read() {
    let head: HeadParams =
        serde_json::from_value(json!({"outline": {"chin": null}, "traced": null, "preset": null})).unwrap();
    assert_eq!(head, HeadParams::default());
    assert_eq!(serde_json::from_value::<BodyShape>(json!({"preset": null})).unwrap(), BodyShape::default());
    let variant: VariantDef = serde_json::from_value(json!({"extends": null, "shot": null})).unwrap();
    assert_eq!(variant, VariantDef::default());
}
