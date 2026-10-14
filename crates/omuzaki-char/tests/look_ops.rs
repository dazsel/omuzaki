//! The `look.*` ops: a preset switch and a parameter edit are each one undoable patch of the look node, an
//! unknown preset or parameter is refused with the nearest valid name, and the look they store is what the build draws:
//! a test preset changes every material's ramp and outline and the scene's bloom, and switching back to Soft SD gives
//! the parts' bytes again.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{apply, assert_error, at, build, definition, plan, project, refused, round_trip};
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, LookDef, LookParams, LookPreset,
    MaterialClass, RegistryBuilder, find_look_preset,
};
use omuzaki_core::colour::Colour;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::PaletteSlot;
use omuzaki_ops_api::ErrorCode;
use omuzaki_render_api::{ParamValue, Shading};
use serde_json::{Value, json};

/// A preset that changes everything a look can: hard terminators, no rims, thick uniform lines, the hair program's ring
/// off, no bloom.
const CONTRAST: &str = r#"
id = "test-contrast"
name = "Test contrast"
summary = "Changes every material and the bloom, for the plumbing tests."
origin = "a test"

[params.all]
softness_scale = 0.0
rim_scale = 0.0
outline_width = 0.003

[params.hair.program_params.hair]
ring-strength = { float = 0.0 }

[params.scene]
bloom = false
"#;

fn presets() -> Result<Vec<LookPreset>, String> {
    Ok(vec![omuzaki_char::look::parse("test-contrast", CONTRAST)?])
}
omuzaki_char_api::register_look_presets!(presets);

const LOOK: &str = "characters/mock/look";

/// The mock character drawn in the look its document holds.
fn drawn(doc: &MemDoc) -> (BuiltCharacter, LookParams) {
    let look = LookDef::from_node(at(doc, LOOK).as_ref()).unwrap().resolve().unwrap();
    let mut built = build(doc, "mock");
    built.apply_look(&look).unwrap();
    (built, look)
}

fn set_preset(preset: &str) -> Value {
    json!({ "character": "mock", "preset": preset })
}

fn set_param(param: &str, value: &Value) -> Value {
    json!({ "character": "mock", "param": param, "value": value })
}

#[test]
fn a_preset_switch_is_one_undoable_edit_that_restyles_every_material() {
    let mut doc = project();
    let (soft, soft_look) = drawn(&doc);
    assert_eq!(format!("{:?}", soft.materials), format!("{:?}", soft.part_materials), "no look node: Soft SD");

    let planned = round_trip(&mut doc, "look.set_preset", set_preset("test-contrast"));
    assert_eq!(at(&doc, LOOK), Some(json!({ "preset": "test-contrast" })));
    let summary = planned.output.unwrap();
    assert_eq!((summary["preset"].as_str(), summary["previous"].as_str()), (Some("test-contrast"), Some("soft-sd")));
    assert!(planned.patch.changes.iter().all(|c| c.path.to_string().starts_with(LOOK)), "only the look node changes");

    let (contrast, look) = drawn(&doc);
    assert_eq!((soft_look.scene.bloom, look.scene.bloom), (Some(true), Some(false)), "the scene's bloom follows");
    for (name, part) in &contrast.part_materials {
        let now = &contrast.materials[name];
        assert_ne!(now.shading, part.shading, "{name}: the ramp changed");
        assert_eq!(now.outline.map(|o| o.width), Some(0.003), "{name}: one uniform line");
        match (&now.shading, contrast.material_classes[name]) {
            (Shading::Toon(t), _) => assert_eq!((t.softness, t.rim.strength), (0.0, 0.0), "{name}"),
            (Shading::Program { params, .. }, MaterialClass::Hair) => {
                assert_eq!(
                    params[&omuzaki_core::id::Slug::new("ring-strength").unwrap()],
                    ParamValue::Float(0.0),
                    "{name}"
                );
            }
            (other, class) => panic!("{name} ({class:?}) is drawn with {other:?}"),
        }
    }
    assert_eq!(contrast.meshes, soft.meshes, "a look never touches geometry");

    // Repeating the edit changes nothing; and back, Soft SD gives the parts' own bytes.
    assert_eq!(plan(&doc, "look.set_preset", set_preset("test-contrast")).unwrap().patch.changes.len(), 0);
    round_trip(&mut doc, "look.set_preset", set_preset("soft-sd"));
    let (again, _) = drawn(&doc);
    assert_eq!(format!("{:?}", again.materials), format!("{:?}", soft.materials));
}

#[test]
fn a_preset_switch_clears_the_overrides_unless_asked_to_keep_them() {
    let mut doc = project();
    apply(&mut doc, "look.set_param", set_param("cloth.outline_width_scale", &json!(2.0)));
    let keep = json!({ "character": "mock", "preset": "test-contrast", "keep_overrides": true });
    round_trip(&mut doc, "look.set_preset", keep);
    assert_eq!(
        at(&doc, LOOK),
        Some(json!({ "preset": "test-contrast", "overrides": { "cloth": { "outline_width_scale": 2.0 } } }))
    );
    let (built, _) = drawn(&doc);
    let cloth: Vec<_> = built.material_classes.iter().filter(|(_, c)| **c == MaterialClass::Cloth).collect();
    assert_ne!(cloth.len(), 0, "the mock wears cloth");
    for (name, _) in cloth {
        assert_eq!(built.materials[name].outline.unwrap().width, 0.006, "{name}: 0.003 × 2");
    }

    let planned = round_trip(&mut doc, "look.set_preset", set_preset("soft-sd"));
    assert_eq!(at(&doc, LOOK), Some(json!({ "preset": "soft-sd" })));
    assert_eq!(planned.output.unwrap()["cleared_overrides"], true);
}

#[test]
fn set_param_sets_and_clears_one_override_over_the_preset() {
    let mut doc = project();
    let planned = round_trip(&mut doc, "look.set_param", set_param("cloth.outline_width_scale", &json!(0.5)));
    assert_eq!(at(&doc, LOOK), Some(json!({ "overrides": { "cloth": { "outline_width_scale": 0.5 } } })));
    assert_eq!(planned.output.unwrap()["preset"], "soft-sd");

    let (built, _) = drawn(&doc);
    let (part, now) = (&built.part_materials, &built.materials);
    for (name, class) in &built.material_classes {
        let (before, after) = (part[name].outline.map(|o| o.width), now[name].outline.map(|o| o.width));
        if *class == MaterialClass::Cloth {
            assert_eq!(after, before.map(|w| w * 0.5), "{name}: cloth lines halve");
        } else {
            assert_eq!(after, before, "{name}: other classes keep their lines");
        }
    }

    round_trip(&mut doc, "look.set_param", set_param("scene.bloom", &json!(false)));
    let cleared = json!({ "character": "mock", "param": "cloth.outline_width_scale" });
    round_trip(&mut doc, "look.set_param", cleared);
    assert_eq!(at(&doc, LOOK), Some(json!({ "overrides": { "scene": { "bloom": false } } })));
    let planned = round_trip(&mut doc, "look.set_param", set_param("scene.bloom", &Value::Null));
    assert_eq!(at(&doc, LOOK), None, "the default look leaves no node");
    assert_eq!(planned.output.unwrap()["cleared"], true);
}

#[test]
fn unknown_presets_and_parameters_are_refused_with_the_nearest_name() {
    let doc = project();
    let e = refused(&doc, "look.set_preset", set_preset("soft-dd"));
    assert_error(&e, ErrorCode::NotFound, "preset", &["soft-dd", "did you mean `soft-sd`?"], &["look.list_presets"]);

    let e = refused(&doc, "look.set_param", set_param("cloth.outline_widht", &json!(1.0)));
    assert_error(&e, ErrorCode::Invalid, "param", &["did you mean `outline_width`?"], &["dotted path"]);
    let e = refused(&doc, "look.set_param", set_param("skn.rim_scale", &json!(1.0)));
    assert_error(&e, ErrorCode::Invalid, "param", &["did you mean `skin`?"], &["dotted path"]);
    let e = refused(&doc, "look.set_param", set_param("skin.rim_scale", &json!(-1.0)));
    assert_error(&e, ErrorCode::Invalid, "value", &["skin.rim_scale", ">= 0"], &["omit the value"]);
    let e = refused(&doc, "look.set_param", set_param("all.outline_colour", &json!("black")));
    assert_error(&e, ErrorCode::Invalid, "value", &["all.outline_colour"], &[]);

    let e = refused(&doc, "look.set_preset", json!({ "character": "nobody", "preset": "soft-sd" }));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::NotFound, Some("character")));

    let mut broken = project();
    apply(&mut broken, "look.set_preset", set_preset("soft-sd"));
    let mut node = at(&broken, LOOK).unwrap();
    node["colour"] = json!("red");
    let broken = MemDoc::from_value(json!({ "characters": { "mock": { "look": node } } }));
    assert!(plan(&broken, "look.set_param", set_param("scene.bloom", &json!(true))).is_err());
}

#[test]
fn list_presets_names_every_preset_and_the_default() {
    let answer = plan(&project(), "look.list_presets", json!({})).unwrap().output.unwrap();
    assert_eq!(answer["default"], "soft-sd");
    let ids: Vec<_> = answer["presets"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"soft-sd") && ids.contains(&"test-contrast"), "{ids:?}");
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "sorted: {ids:?}");
    assert_eq!(answer["presets"][ids.iter().position(|i| *i == "soft-sd").unwrap()]["name"], "Soft SD");
}

#[test]
fn a_palette_change_through_the_caching_builder_keeps_the_look() {
    let (id, def) = (CharId::new("mock").unwrap(), definition(&project(), "mock"));
    let (options, services) = (BuildOptions::default(), BuildServices::default());
    let builder = CachingBuilder::new();
    let look = find_look_preset("test-contrast").unwrap().params;
    let mut built = builder.build(&id, &def, &options, &services).unwrap();
    built.apply_look(&look).unwrap();

    let mut palette = built.palette.clone();
    palette.0.insert(PaletteSlot::new("outfit").unwrap(), Colour::rgb(200, 30, 60));
    builder.rebuild_materials(&mut built, &palette, &options, &services).unwrap();
    assert_eq!(built.look(), &look, "the look survives the rebuild");

    let recoloured = omuzaki_char_api::CharacterDef { palette, ..def };
    let mut fresh = RegistryBuilder.build(&id, &recoloured, &options, &services).unwrap();
    fresh.apply_look(&look).unwrap();
    assert_eq!(
        format!("{:?}", built.materials),
        format!("{:?}", fresh.materials),
        "the new colours, drawn in the look"
    );
    assert_ne!(built.materials, built.part_materials);
}
