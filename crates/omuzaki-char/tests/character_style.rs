//! Character styles (`character.set_style`, `character.mix_styles`, `character.create` with a `style`,
//! `character.list_styles`): every style builds on each template it is tuned for; a style changes the face design,
//! face shape, proportions, hair and look together in one undoable step; styles never compound (one after another is
//! the second alone); an amount and a mix blend them; a template the style isn't tuned for moves the character to the
//! style's, or is refused with `keep_template`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use omuzaki_char::style;
use omuzaki_char_api::{CharacterDef, PartId};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::{Value, json};

fn created(template: &str, style: Option<&str>) -> MemDoc {
    let mut doc = MemDoc::default();
    let mut args = json!({"id": "kit", "name": "Kit", "template": template});
    if let Some(style) = style {
        args["style"] = json!(style);
    }
    common::apply(&mut doc, "character.create", args);
    doc
}

fn params(def: &CharacterDef, part: &str) -> Value {
    def.parts[&PartId::new(part).unwrap()].params.clone()
}

fn look(doc: &MemDoc) -> Option<Value> {
    common::at(doc, "characters/kit/look")
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
        let neck = |d: &CharacterDef| params(d, "body")["proportions"]["neck_length"].as_f64();
        assert!(neck(&after) > neck(&before), "{template}: a longer neck");
        let changed = plan.output.as_ref().unwrap()["changed"].as_array().unwrap();
        assert!(changed.contains(&json!("face")) && changed.contains(&json!("parts/body")), "{changed:?}");
        // Applying it again changes nothing more.
        let again = common::plan(&doc, "character.set_style", json!({"character": "kit", "style": "mature"})).unwrap();
        assert!(again.patch.is_empty(), "a second apply is a no-op");
    }
}

#[test]
fn every_style_builds_without_warnings_on_each_template_it_is_tuned_for() {
    let all = style::styles().unwrap();
    assert!(all.len() >= 10, "a wide library: {}", all.len());
    for s in all {
        if let Some(look) = &s.look {
            omuzaki_char_api::find_look_preset(look).unwrap_or_else(|e| panic!("{}: look {look}: {e}", s.id));
        }
        for template in &s.templates {
            let mut doc = created(template, None);
            common::apply(&mut doc, "character.set_style", json!({"character": "kit", "style": s.id}));
            let built = common::build(&doc, "kit");
            let warnings: Vec<_> = built.report.parts.iter().flat_map(|p| &p.warnings).collect();
            assert!(warnings.is_empty(), "{} on {template}: {warnings:?}", s.id);
        }
    }
}

#[test]
fn styles_never_compound() {
    // Every ordered pair: B after A is exactly B on a fresh character of the template B ends on (the template is the
    // one discrete choice that may stay put when B is tuned for A's): face, head, body, hair and look alike.
    let all = style::styles().unwrap();
    let mut alone = std::collections::BTreeMap::new();
    for a in all {
        let mut doc = created("standard", None);
        common::apply(&mut doc, "character.set_style", json!({"character": "kit", "style": a.id}));
        let after_a = doc.clone();
        for b in all {
            let mut doc = after_a.clone();
            common::apply(&mut doc, "character.set_style", json!({"character": "kit", "style": b.id}));
            let def = common::definition(&doc, "kit");
            let template = def.template.to_string();
            let (only, only_look): &(CharacterDef, Option<Value>) =
                alone.entry((b.id.clone(), template.clone())).or_insert_with(|| {
                    let mut fresh = created(&template, None);
                    common::apply(&mut fresh, "character.set_style", json!({"character": "kit", "style": b.id}));
                    (common::definition(&fresh, "kit"), look(&fresh))
                });
            let pair = format!("{} after {} on {template}", b.id, a.id);
            assert!(b.tuned_for(&template), "{pair}: a template the style is tuned for");
            assert_eq!(def.face, only.face, "{pair}: face");
            for part in ["head", "body", "hair"] {
                assert_eq!(params(&def, part), params(only, part), "{pair}: {part}");
            }
            assert_eq!(look(&doc), *only_look, "{pair}: look");
        }
    }
}

#[test]
fn a_style_brings_its_hair_and_look_unless_asked_not_to() {
    let mut doc = created("standard", None);
    let hair_before = params(&common::definition(&doc, "kit"), "hair");
    let plan = common::round_trip(&mut doc, "character.set_style", json!({"character": "kit", "style": "semi-real"}));
    let def = common::definition(&doc, "kit");
    assert_eq!(def.template.as_str(), "tall", "semi-real moves the character to its template");
    assert_ne!(params(&def, "hair")["groups"], hair_before["groups"], "the hair is cut to the style's");
    assert_eq!(params(&def, "hair")["shading"], hair_before["shading"], "the hair keeps its colours");
    let stored = look(&doc).unwrap();
    assert_eq!(stored["preset"], "semi-real");
    assert!((stored["overrides"]["all"]["style"].as_f64().unwrap() - 0.4).abs() < 1e-6, "{stored}");
    let out = plan.output.unwrap();
    assert_eq!((out["template"].as_str(), out["look"].as_str()), (Some("tall"), Some("semi-real")));

    let mut unstyled = created("standard", None);
    common::apply(
        &mut unstyled,
        "character.set_style",
        json!({"character": "kit", "style": "semi-real", "hair": false, "look": false}),
    );
    assert_eq!(params(&common::definition(&unstyled, "kit"), "hair"), hair_before);
    assert_eq!(look(&unstyled), None);
}

#[test]
fn amount_blends_from_the_character_toward_the_style() {
    let mut doc = created("standard", None);
    let before = common::definition(&doc, "kit");
    let mut full = doc.clone();
    common::apply(&mut full, "character.set_style", json!({"character": "kit", "style": "shonen"}));
    let full = common::definition(&full, "kit");
    common::round_trip(&mut doc, "character.set_style", json!({"character": "kit", "style": "shonen", "amount": 0.5}));
    let half = common::definition(&doc, "kit");
    let mid = f64::midpoint;
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    let r = |d: &CharacterDef| d.face.eyes.aperture.roundness;
    assert!(close(r(&half), mid(r(&before), r(&full))), "{} between {} and {}", r(&half), r(&before), r(&full));
    let brows = |d: &CharacterDef| d.face.brows.thickness;
    assert!(close(brows(&half), mid(brows(&before), brows(&full))));
    // At 0.5 the discrete choices follow the style (the entries tie and the later one leads); below, the character.
    let mut low = created("standard", None);
    common::apply(&mut low, "character.set_style", json!({"character": "kit", "style": "shonen", "amount": 0.25}));
    assert_eq!(params(&common::definition(&low, "kit"), "hair"), params(&before, "hair"));
    assert_eq!(look(&low), Some(json!({"overrides": {"all": {"style": 1.0}}})), "the axis still blends: 1 to 1");
    // Amount 0 changes nothing; out of range is refused.
    let zero = common::plan(&doc, "character.set_style", json!({"character": "kit", "style": "comic", "amount": 0.0}));
    assert!(zero.unwrap().patch.is_empty());
    let e = common::refused(&doc, "character.set_style", json!({"character": "kit", "style": "comic", "amount": 1.5}));
    common::assert_error(&e, ErrorCode::Invalid, "amount", &["1.5"], &["from 0 to 1"]);
}

#[test]
fn mix_styles_weights_every_number_and_follows_the_heaviest() {
    let one = |id: &str| {
        let mut doc = created("standard", None);
        common::apply(&mut doc, "character.set_style", json!({"character": "kit", "style": id}));
        common::definition(&doc, "kit")
    };
    let (shoujo, semi) = (one("shoujo"), one("semi-real"));
    let mut doc = created("standard", None);
    let plan = common::round_trip(
        &mut doc,
        "character.mix_styles",
        json!({"character": "kit", "styles": [{"style": "shoujo", "weight": 2.0}, {"style": "semi-real", "weight": 1.0}]}),
    );
    let mixed = common::definition(&doc, "kit");
    let expect = |a: f64, b: f64| (2.0 * a + b) / 3.0;
    let iris = |d: &CharacterDef| d.face.eyes.iris.width;
    assert!((iris(&mixed) - expect(iris(&shoujo), iris(&semi))).abs() < 1e-9);
    let width = |d: &CharacterDef| d.face.layout.eye_width;
    assert!((width(&mixed) - expect(width(&shoujo), width(&semi))).abs() < 1e-9);
    assert_eq!(mixed.template.as_str(), "standard", "shoujo leads, and it is tuned for standard");
    assert_eq!(params(&mixed, "hair")["groups"], params(&shoujo, "hair")["groups"], "the heaviest's hair");
    assert_eq!(mixed.face.eyes.highlights.shape, shoujo.face.eyes.highlights.shape, "discrete: the heaviest's");
    let axis = look(&doc).unwrap()["overrides"]["all"]["style"].as_f64().unwrap();
    assert!((axis - expect(1.0, 0.4)).abs() < 1e-6, "the axis is weighted too: {axis}");
    assert_eq!(plan.output.unwrap()["styles"][0]["style"], "shoujo");

    let e = common::refused(
        &doc,
        "character.mix_styles",
        json!({"character": "kit", "styles": [{"style": "shoujo", "weight": 0.0}]}),
    );
    common::assert_error(&e, ErrorCode::Invalid, "styles", &["add up to 0"], &["weight"]);
    let e = common::refused(&doc, "character.mix_styles", json!({"character": "kit", "styles": []}));
    assert_eq!(e.field.as_deref(), Some("styles"));
}

#[test]
fn a_template_the_style_isnt_tuned_for_moves_the_character_or_is_refused() {
    let mut doc = created("standard", None);
    common::round_trip(&mut doc, "character.set_style", json!({"character": "kit", "style": "chibi"}));
    assert_eq!(common::definition(&doc, "kit").template.as_str(), "soft-sd");
    let built = common::build(&doc, "kit");
    let warnings: Vec<_> = built.report.parts.iter().flat_map(|p| &p.warnings).collect();
    assert!(warnings.is_empty(), "{warnings:?}");

    let mut doc = created("soft-sd", None);
    common::apply(&mut doc, "character.set_style", json!({"character": "kit", "style": "high-fidelity"}));
    assert_eq!(common::definition(&doc, "kit").template.as_str(), "tall", "a chibi grows to the photoreal template");

    let doc = created("soft-sd", None);
    let e = common::refused(
        &doc,
        "character.set_style",
        json!({"character": "kit", "style": "mature", "keep_template": true}),
    );
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
        let groups = def.parts[&PartId::new("hair").unwrap()].params["groups"].as_object().unwrap();
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
fn create_with_a_style_cuts_the_hair_to_its_hairstyle() {
    let doc = created("standard", Some("shoujo"));
    let def = common::definition(&doc, "kit");
    let mut plain = created("standard", None);
    common::apply(&mut plain, "character.set_style", json!({"character": "kit", "style": "shoujo"}));
    let styled = common::definition(&plain, "kit");
    assert_eq!(def.face, styled.face);
    assert_eq!(params(&def, "hair")["groups"], params(&styled, "hair")["groups"]);
}

#[test]
fn list_styles_names_every_family_and_filters_by_one() {
    let doc = MemDoc::default();
    let plan = common::plan(&doc, "character.list_styles", json!({})).unwrap();
    let out = plan.output.unwrap();
    let mature = out["styles"].as_array().unwrap().iter().find(|s| s["id"] == "mature").unwrap();
    assert_eq!(mature["templates"], json!(["standard", "tall"]));
    let families = out["families"].as_array().unwrap();
    for family in ["anime", "western", "real"] {
        assert!(families.contains(&json!(family)), "{families:?}");
    }
    let real = common::plan(&doc, "character.list_styles", json!({"family": "real"})).unwrap().output.unwrap();
    let ids: Vec<_> = real["styles"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"semi-real") && ids.contains(&"high-fidelity") && !ids.contains(&"chibi"), "{ids:?}");
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
