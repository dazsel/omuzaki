//! Hair follows the head's own size (`HeadParams::size`): it is built against the head at the template's size and sized
//! with it about its joint, so a smaller head wears a smaller cut of the same proportions instead of the template's
//! hair standing off it. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)] // fixtures; a bad one should stop the test with its reason

use std::sync::Arc;

use omuzaki_char::hair::styles;
use omuzaki_char_api::{
    Assembler, BuildOptions, BuildServices, CharId, CharacterDef, HairParams, HeadFact, HeadSurface, PartDef, PartId,
    PartKindId, PartOutput, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use serde_json::{Value, json};

fn part(kind: &str, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The hair part `hair` builds on the standard template with a head of `head` params, and the head it is built on.
fn hair_on(head: Value, hair: &HairParams) -> (PartOutput, Arc<dyn HeadSurface>) {
    omuzaki_char::link();
    let mut def: CharacterDef = mock_character().unwrap();
    def.template = "standard".parse().unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", head));
    def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(hair).unwrap()));
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, &def, &options);
    let mut result = None;
    for p in plan(&def).unwrap() {
        let out = build_part(&def, &p, &options, &services, assembler.published()).unwrap();
        if p.as_str() == "hair" {
            result = Some((
                build_part(&def, &p, &options, &services, assembler.published()).unwrap(),
                assembler.published().get::<HeadFact>().clone().unwrap(),
            ));
        }
        assembler.add(&p, out).unwrap();
    }
    result.unwrap()
}

/// The lowest and highest point of the hair that follows the head (the meshes the runtime does not move).
fn head_hair_bounds(out: &PartOutput) -> (Vec3, Vec3) {
    let ps: Vec<Vec3> =
        out.meshes.iter().filter(|m| !m.dynamic).flat_map(|m| m.geometry.positions.iter().copied()).collect();
    assert!(ps.len() > 1000, "the cut has hair on the head");
    (
        ps.iter().copied().fold(Vec3::splat(f32::INFINITY), Vec3::min),
        ps.iter().copied().fold(Vec3::splat(f32::NEG_INFINITY), Vec3::max),
    )
}

/// How far the deepest hair vertex sits inside `head`, metres (0 if none does).
fn deepest(out: &PartOutput, head: &dyn HeadSurface) -> f32 {
    let centre = head.centre();
    let mut worst = 0.0_f32;
    for m in &out.meshes {
        for p in &m.geometry.positions {
            let offset = *p - centre;
            let Some(dir) = offset.try_normalize() else { continue };
            let d = head.radius(dir) - offset.length();
            worst = worst.max(d);
        }
    }
    worst
}

#[test]
fn hair_is_sized_with_the_head_about_its_joint() {
    for id in ["bob", "anime-bob", "crown-braid"] {
        let hair = styles::find(id).unwrap().applied_to(&HairParams::default());
        let (one, head_one) = hair_on(json!({}), &hair);
        let (unset, _) = hair_on(json!({ "size": 1.0 }), &hair);
        assert_eq!(one.meshes, unset.meshes, "{id}: at size 1 the hair is the template's, bit for bit");
        let (size_one, about) = head_one.own_size();
        assert!((size_one - 1.0).abs() < f32::EPSILON, "{id}: the template's head reports size 1");
        let depth_one = deepest(&one, head_one.as_ref());
        let (lo1, hi1) = head_hair_bounds(&one);
        for size in [0.7_f32, 1.25] {
            let (out, head) = hair_on(json!({ "size": size }), &hair);
            let (reported, joint) = head.own_size();
            assert!((reported - size).abs() < 1e-5, "{id}: the head reports its size {size}: {reported}");
            assert!(joint.is_finite() && about.is_finite());
            let (lo, hi) = head_hair_bounds(&out);
            let ratio = (hi - lo) / (hi1 - lo1);
            eprintln!("{id} size {size}: head-hair extent ratio {ratio:?}");
            assert!(
                (ratio - Vec3::splat(size)).abs().max_element() < 0.02,
                "{id}: the head's hair scales by {size}: {ratio:?}"
            );
            let depth = deepest(&out, head.as_ref());
            assert!(
                depth <= depth_one * size + 0.001,
                "{id} size {size}: {depth} m inside the head (size 1: {depth_one})"
            );
        }
    }
}
