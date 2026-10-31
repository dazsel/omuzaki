//! Character styles (`character.set_style`, `character.create` with a `style`, `character.list_styles`): the `mature`
//! style changes the face design, face shape, neck and proportions together in one undoable step, is refused on a
//! template it isn't tuned for, and a character created with it builds on `standard` and `tall` with its braided locks
//! and ahoge.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::json;

fn created(template: &str, style: Option<&str>) -> MemDoc {
    let mut doc = MemDoc::default();
    let mut args = json!({"id": "kit", "name": "Kit", "template": template});
    if let Some(style) = style {
        args["style"] = json!(style);
    }
    common::apply(&mut doc, "character.create", args);
    doc
}

#[test]
fn set_style_changes_face_and_proportions_in_one_undoable_step() {
    for template in ["standard", "tall"] {
        let mut doc = created(template, None);
        let before = common::definition(&doc, "kit");
        let plan = common::round_trip(&mut doc, "character.set_style", json!({"character": "kit", "style": "mature"}));
        let after = common::definition(&doc, "kit");
        assert!(after.face.eyes.aperture.roundness < before.face.eyes.aperture.roundness, "{template}: narrower eyes");
        assert!(after.face.layout.eye_width < before.face.layout.eye_width, "{template}: smaller eyes");
        let neck = |d: &omuzaki_char_api::CharacterDef| {
            d.parts[&omuzaki_char_api::PartId::new("body").unwrap()].params["proportions"]["neck_length"].as_f64()
        };
        assert!(neck(&after) > neck(&before), "{template}: a longer neck");
        let changed = plan.output.as_ref().unwrap()["changed"].as_array().unwrap();
        assert!(changed.contains(&json!("face")) && changed.contains(&json!("parts/body")), "{changed:?}");
        // Applying it again changes nothing more.
        let again = common::plan(&doc, "character.set_style", json!({"character": "kit", "style": "mature"})).unwrap();
        assert!(again.patch.is_empty(), "a second apply is a no-op");
    }
}

#[test]
fn set_style_refuses_an_untuned_template_and_an_unknown_style() {
    let doc = created("soft-sd", None);
    let e = common::refused(&doc, "character.set_style", json!({"character": "kit", "style": "mature"}));
    common::assert_error(&e, ErrorCode::Invalid, "style", &["standard and tall", "soft-sd"], &["body.set_template"]);
    let e = common::refused(&doc, "character.set_style", json!({"character": "kit", "style": "gothic"}));
    assert!(e.message.contains("gothic") && e.hint.unwrap().contains("mature"), "{}", e.message);
    let e = common::refused(
        &doc,
        "character.create",
        json!({"id": "x", "name": "X", "template": "soft-sd", "style": "mature"}),
    );
    assert!(e.message.contains("soft-sd"), "{}", e.message);
}

#[test]
fn a_mature_character_builds_with_its_braids_and_ahoge() {
    for template in ["standard", "tall"] {
        let doc = created(template, Some("mature"));
        let def = common::definition(&doc, "kit");
        let groups = def.parts[&omuzaki_char_api::PartId::new("hair").unwrap()].params["groups"].as_object().unwrap();
        assert!(groups.contains_key("e-braids") && groups.contains_key("f-ahoge"), "{template}: {:?}", groups.keys());
        let styled = {
            let mut plain = created(template, None);
            common::apply(&mut plain, "character.set_style", json!({"character": "kit", "style": "mature"}));
            common::definition(&plain, "kit")
        };
        assert_eq!(def.face, styled.face, "{template}: create with a style matches set_style on the face");
        let built = common::build(&doc, "kit");
        let warnings: Vec<_> = built.report.parts.iter().flat_map(|p| &p.warnings).collect();
        assert!(warnings.is_empty(), "{template}: {warnings:?}");
    }
}

#[test]
fn list_styles_names_mature_and_its_templates() {
    let doc = MemDoc::default();
    let plan = common::plan(&doc, "character.list_styles", json!({})).unwrap();
    let styles = plan.output.unwrap()["styles"].clone();
    let mature = styles.as_array().unwrap().iter().find(|s| s["id"] == "mature").unwrap();
    assert_eq!(mature["templates"], json!(["standard", "tall"]));
}

#[test]
fn western_toon_draws_a_structured_face_and_builds_on_both_templates() {
    for template in ["standard", "tall"] {
        let mut doc = created(template, None);
        common::round_trip(&mut doc, "character.set_style", json!({"character": "kit", "style": "western-toon"}));
        let def = common::definition(&doc, "kit");
        let face = &def.face;
        assert_eq!(face.nose.kind, omuzaki_face_api::NoseKind::Wedge, "{template}: a cel-shadow nose");
        assert!(face.mouth.lips.full > 0.5, "{template}: full lips");
        let lines = &face.cheeks.lines;
        assert!(lines.cheekbone > 0.0 && lines.smile > 0.0 && lines.jaw_shade > 0.0, "{template}: face lines");
        let built = common::build(&doc, "kit");
        let warnings: Vec<_> = built.report.parts.iter().flat_map(|p| &p.warnings).collect();
        assert!(warnings.is_empty(), "{template}: {warnings:?}");
    }
    let plan = common::plan(&MemDoc::default(), "character.list_styles", json!({})).unwrap();
    let styles = plan.output.unwrap()["styles"].clone();
    assert!(styles.as_array().unwrap().iter().any(|s| s["id"] == "western-toon"), "{styles}");
}
