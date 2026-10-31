//! The hairstyle library (#2486): at least the ten styles the issue names, each a valid hair part that builds on every
//! template with no hair more than a millimetre deeper inside the head than the kit's own default style goes.
//! Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)] // fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_possible_truncation)]

use std::sync::Arc;

use omuzaki_char::hair::styles;
use omuzaki_char_api::{
    Assembler, BodyTemplateId, BuildOptions, BuildServices, CharId, CharacterDef, HairParams, HeadFact, HeadSurface,
    PartDef, PartId, PartKindId, PartOutput, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use serde_json::json;

const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

const NAMED: [&str; 14] = [
    "long-straight",
    "bob",
    "twin-tails",
    "ponytail",
    "side-braid",
    "hime-cut",
    "messy-short",
    "wavy-long",
    "bun",
    "updo-slick",
    "half-up",
    "anime-bob",
    "anime-long",
    "crown-braid",
];

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The hair part `hair` builds on `template`, with the default body and head, and the head it is built on.
fn hair_on(template: &str, hair: &HairParams) -> (PartOutput, Arc<dyn HeadSurface>) {
    omuzaki_char::link();
    let mut def: CharacterDef = mock_character().unwrap();
    def.template = BodyTemplateId::new(template).unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
    def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(hair).unwrap()));
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, &def, &options);
    let mut result = None;
    for p in plan(&def).unwrap() {
        let out = build_part(&def, &p, &options, &services, assembler.published())
            .unwrap_or_else(|e| panic!("{template}: {e}"));
        if p.as_str() == "hair" {
            let head = assembler.published().get::<HeadFact>().clone().unwrap();
            result = Some((build_part(&def, &p, &options, &services, assembler.published()).unwrap(), head));
        }
        assembler.add(&p, out).unwrap();
    }
    result.unwrap()
}

/// How far the deepest hair vertex of `out` sits inside `head`, metres (0 if none does), and the vertex count.
fn deepest(out: &PartOutput, head: &dyn HeadSurface) -> (f32, usize) {
    let centre = head.centre();
    let mut worst = 0.0_f32;
    let mut count = 0;
    for mesh in &out.meshes {
        for p in &mesh.geometry.positions {
            assert!(p.is_finite(), "{}: a vertex is not finite", mesh.name);
            let offset = *p - centre;
            let Some(dir) = offset.try_normalize() else { continue };
            worst = worst.max(head.radius(dir) - offset.length());
            count += 1;
        }
    }
    (worst, count)
}

#[test]
fn the_library_holds_the_ten_named_styles_each_valid() {
    let library = styles::list().unwrap();
    for id in NAMED {
        let style = styles::find(id).unwrap_or_else(|e| panic!("{e}"));
        assert!(!style.name.is_empty() && !style.summary.is_empty() && !style.origin.is_empty(), "{id}");
    }
    assert!(library.len() >= NAMED.len());
    assert!(matches!(styles::find("mohawk"), Err(styles::HairStyleError::Unknown(_))));
}

#[test]
fn applying_a_style_keeps_the_colours_shading_and_physics() {
    let mut hair = HairParams::default();
    hair.shading.ring_strength = 0.3;
    hair.physics.damping = 0.5;
    let bob = styles::find("bob").unwrap();
    let cut = bob.applied_to(&hair);
    assert_eq!((cut.shading.clone(), cut.physics.clone()), (hair.shading.clone(), hair.physics.clone()));
    assert_eq!((&cut.groups, &cut.volume), (&bob.groups, &bob.volume));
}

#[test]
fn every_style_builds_on_every_template_no_deeper_into_the_head_than_the_default() {
    let mut failures = Vec::new();
    for template in TEMPLATES {
        let (default, head) = hair_on(template, &HairParams::default());
        let (baseline, _) = deepest(&default, head.as_ref());
        for style in styles::list().unwrap() {
            let (out, head) = hair_on(template, &style.applied_to(&HairParams::default()));
            let (depth, vertices) = deepest(&out, head.as_ref());
            eprintln!("{template} {}: {vertices} vertices, deepest {depth:.4} m (default {baseline:.4})", style.id);
            if vertices < 1000 || depth > baseline + 0.001 {
                failures.push(format!("{template} {}: {vertices} vertices, {depth} m inside the head", style.id));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
