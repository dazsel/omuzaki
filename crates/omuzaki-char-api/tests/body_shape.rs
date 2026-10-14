//! Body diversity and hands (CB-04, RG-09): the new parameters keep old files valid, default to the template, reject
//! out-of-range values by field path, show up in the part's schema, and are written down in the design doc.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{
    BodyParams, BodyRegion, BodyShape, BodyShapeId, CharId, CharacterDef, ColourwayId, HandParams, HandStyle,
    SkinToneId, paths,
};
use omuzaki_face_api::{PaletteSlot, Validate};
use serde_json::json;

const DOC: &str = include_str!("../../../docs/design/char-face-api.md");

/// A body as saved before body shapes and hand parameters existed.
const OLD_BODY: &str = r#"{
    "proportions": { "leg_length": 1.1, "foot_size": 0.9 },
    "hands": "five_finger",
    "skin": { "base": "@skin" }
}"#;

/// The schema the `body` kind publishes: inlined, like `PartKindRegistration::schema`.
fn body_schema() -> serde_json::Value {
    let settings = schemars::generate::SchemaSettings::draft2020_12().with(|s| s.inline_subschemas = true);
    serde_json::to_value(settings.into_generator().into_root_schema_for::<BodyParams>()).unwrap()
}

#[test]
fn old_bodies_parse_unchanged_and_new_fields_default_to_the_template() {
    let old: BodyParams = serde_json::from_str(OLD_BODY).unwrap();
    old.validate().unwrap();
    assert_eq!((old.proportions.leg_length, old.hands), (1.1, HandStyle::FiveFinger));
    assert_eq!(old.shape, BodyShape::default());
    assert_eq!(old.hand, HandParams::default());
    let empty: BodyParams = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, BodyParams::default());
}

#[test]
fn the_default_shape_is_the_identity() {
    let s = BodyShape::default();
    for axis in [s.height, s.build, s.curves, s.musculature, s.shoulder_hip] {
        assert_eq!(axis, 0.0);
    }
    assert_eq!(s.preset, None);
    let h = HandParams::default();
    for multiplier in [h.palm_width, h.palm_length, h.finger_length, h.finger_thickness, h.thumb_length, h.taper] {
        assert_eq!(multiplier, 1.0);
    }
    assert_eq!(h.thumb_spread_deg, 0.0);
}

#[test]
fn shape_and_hand_round_trip_with_their_preset() {
    let mut b = BodyParams::default();
    b.shape = BodyShape { height: 0.5, build: -0.25, preset: Some(BodyShapeId::new("slim").unwrap()), ..b.shape };
    b.hand.finger_length = 1.3;
    b.hand.thumb_spread_deg = -12.0;
    let json = serde_json::to_value(&b).unwrap();
    assert_eq!(json["shape"]["preset"], "slim");
    assert_eq!(json["hand"]["thumb_spread_deg"], -12.0);
    assert_eq!(serde_json::from_value::<BodyParams>(json).unwrap(), b);
    assert!(
        serde_json::from_value::<BodyParams>(json!({ "shape": { "heigth": 0.1 } })).is_err(),
        "a typo in an axis is rejected, not ignored"
    );
}

#[test]
fn out_of_range_values_are_rejected_with_their_field_path() {
    let cases = [
        (json!({ "shape": { "build": 1.01 } }), "shape/build"),
        (json!({ "shape": { "height": -1.2 } }), "shape/height"),
        (json!({ "shape": { "curves": 5 } }), "shape/curves"),
        (json!({ "shape": { "musculature": -2 } }), "shape/musculature"),
        (json!({ "shape": { "shoulder_hip": 1.5 } }), "shape/shoulder_hip"),
        (json!({ "hand": { "palm_width": 0.4 } }), "hand/palm_width"),
        (json!({ "hand": { "palm_length": 2.1 } }), "hand/palm_length"),
        (json!({ "hand": { "finger_length": 0.0 } }), "hand/finger_length"),
        (json!({ "hand": { "finger_thickness": 3 } }), "hand/finger_thickness"),
        (json!({ "hand": { "thumb_length": 0.1 } }), "hand/thumb_length"),
        (json!({ "hand": { "thumb_spread_deg": 31 } }), "hand/thumb_spread_deg"),
        (json!({ "hand": { "taper": 2.5 } }), "hand/taper"),
    ];
    for (value, path) in cases {
        let b: BodyParams = serde_json::from_value(value.clone()).unwrap();
        let errors = b.validate().unwrap_err();
        assert_eq!(errors.0.len(), 1, "{value}: {errors:?}");
        assert_eq!(errors.0[0].path, path, "{value}");
    }
    let edge = json!({ "shape": { "height": -1, "build": 1 }, "hand": { "palm_width": 0.5, "taper": 2, "thumb_spread_deg": -30 } });
    serde_json::from_value::<BodyParams>(edge).unwrap().validate().expect("the range ends are inclusive");
}

#[test]
fn the_part_schema_lists_the_new_parameters_with_their_bounds_in_the_docs() {
    let schema = body_schema();
    let props = &schema["properties"];
    for field in ["height", "build", "curves", "musculature", "shoulder_hip", "preset"] {
        assert!(props["shape"]["properties"][field].is_object(), "shape.{field} missing from the schema: {schema}");
    }
    for field in
        ["palm_width", "palm_length", "finger_length", "finger_thickness", "thumb_length", "thumb_spread_deg", "taper"]
    {
        assert!(props["hand"]["properties"][field].is_object(), "hand.{field} missing from the schema: {schema}");
    }
    assert_eq!(props["shape"]["additionalProperties"], false, "typos are rejected, and the schema says so");
    assert!(
        props["shape"]["properties"]["height"]["description"].as_str().unwrap().contains("12%"),
        "the axis meaning travels with the schema (it is what the agent reads)"
    );
}

#[test]
fn a_character_records_a_skin_tone_and_old_characters_have_none() {
    let old: CharacterDef = serde_json::from_str(r#"{"name":"Old","template":"soft-sd"}"#).unwrap();
    assert_eq!(old.skin_tone, None);
    let new: CharacterDef =
        serde_json::from_str(r#"{"name":"New","template":"soft-sd","skin_tone":"deep-cool"}"#).unwrap();
    assert_eq!(new.skin_tone, Some(SkinToneId::new("deep-cool").unwrap()));
    assert_eq!(serde_json::from_value::<CharacterDef>(serde_json::to_value(&new).unwrap()).unwrap(), new);
    assert!(serde_json::from_str::<CharacterDef>(r#"{"name":"x","template":"t","skin_tone":"Deep Cool"}"#).is_err());
}

#[test]
fn feet_are_regions_and_new_regions_can_follow() {
    // `BodyRegion` is `#[non_exhaustive]`: outside the crate a match needs a wildcard, which is what lets CB-05 add
    // wings or tails without breaking garments.
    let name = |r: BodyRegion| match r {
        BodyRegion::FootL | BodyRegion::FootR => "foot",
        _ => "other",
    };
    assert_eq!((name(BodyRegion::FootL), name(BodyRegion::FootR), name(BodyRegion::LegL)), ("foot", "foot", "other"));
    assert!(BodyRegion::LegR < BodyRegion::FootL, "the new regions come after the old ones in the ordering");
}

#[test]
fn colourways_live_beside_the_palette() {
    let (c, w) = (CharId::new("mira").unwrap(), ColourwayId::new("summer-stream").unwrap());
    assert_eq!(paths::colourways(&c).to_string(), "characters/mira/colourways");
    assert_eq!(paths::colourway(&c, &w).to_string(), "characters/mira/colourways/summer-stream");
    assert_ne!(paths::colourway(&c, &w), paths::palette(&c));
}

#[test]
fn the_design_doc_describes_what_the_code_added() {
    assert!(DOC.contains("### Body diversity and hands"), "the design doc lost its 'Body diversity and hands' section");
    for name in [
        "BodyShape",
        "HandParams",
        "SkinToneId",
        "BodyShapeId",
        "ColourwayId",
        "paths::colourway",
        "skin_tone",
        "FootL",
        "FootR",
        "non_exhaustive",
    ] {
        assert!(DOC.contains(name), "the design doc doesn't mention `{name}`");
    }
    for axis in ["height", "build", "curves", "musculature", "shoulder_hip"] {
        assert!(DOC.contains(&format!("| `{axis}` |")), "the design doc has no row for the shape axis `{axis}`");
    }
    for field in
        ["palm_width", "palm_length", "finger_length", "finger_thickness", "thumb_length", "thumb_spread_deg", "taper"]
    {
        assert!(DOC.contains(field), "the design doc doesn't mention the hand parameter `{field}`");
    }
    // The standard palette slots are documented, and each is a valid slot name.
    for slot in [
        "skin",
        "skin_shade",
        "skin_line",
        "skin_rim",
        "blush",
        "lip",
        "eyes",
        "hair_root",
        "hair_mid",
        "hair_tip",
        "outfit_a",
        "outfit_b",
        "trim",
    ] {
        PaletteSlot::new(slot).unwrap();
        assert!(DOC.contains(&format!("`{slot}`")), "the design doc doesn't list the standard palette slot `{slot}`");
    }
}
