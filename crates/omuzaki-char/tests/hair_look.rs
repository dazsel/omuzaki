//! The hair's look baked at build: occlusion (#2526) darkens lock roots and the layers under the outer locks while the
//! outside stays bright, and the loose strands (#2516) add flyaways at live quality and baby hairs along the hairline,
//! all clear of the head. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)] // fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::sync::Arc;

use omuzaki_char::hair::occlusion;
use omuzaki_char_api::{
    Assembler, BodyTemplateId, BuildOptions, BuildServices, CharId, CharacterDef, HAIR_INFO, HairParams, HeadFact,
    HeadSurface, PartDef, PartId, PartKindId, PartOutput, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::mesh::MeshData;
use serde_json::json;

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The hair part `hair` builds on `template` with the default body and head, and the head it is built on.
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
    for p in plan(&def).unwrap() {
        let out = build_part(&def, &p, &options, &services, assembler.published())
            .unwrap_or_else(|e| panic!("{template}: {e}"));
        if p.as_str() == "hair" {
            let head = assembler.published().get::<HeadFact>().clone().unwrap();
            return (out, head);
        }
        assembler.add(&p, out).unwrap();
    }
    panic!("no hair part was planned")
}

/// The hair meshes (not the tie bands) of `out`.
fn hair_meshes(out: &PartOutput) -> Vec<&MeshData> {
    out.meshes.iter().filter(|m| m.geometry.extra_attribute(HAIR_INFO).is_some()).map(|m| &m.geometry).collect()
}

/// Every vertex's shade (`hair-info.y`) across the hair meshes.
fn shades(out: &PartOutput) -> Vec<f32> {
    hair_meshes(out)
        .iter()
        .flat_map(|m| {
            m.extra_attribute(HAIR_INFO).unwrap().data.as_chunks::<3>().0.iter().map(|c| c[1]).collect::<Vec<_>>()
        })
        .collect()
}

fn mean(xs: impl IntoIterator<Item = f32>) -> f32 {
    let (sum, n) = xs.into_iter().fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    sum / n.max(1) as f32
}

#[test]
fn occlusion_darkens_what_is_buried_and_keeps_the_outside_bright() {
    // A layered style: a fringe, framing locks and a draped curtain over the scalp shell.
    let styled = omuzaki_char::hair::styles::find("long-straight").unwrap().applied_to(&HairParams::default());
    let mut plain = styled.clone();
    plain.shading.occlusion = 0.0;
    let (open, head) = hair_on("standard", &plain);
    let meshes = hair_meshes(&open);
    let measured = occlusion::measure(&meshes, head.as_ref(), plain.shading.occlusion_reach as f32);
    let centre = head.centre();
    // Outer vertices: the farthest tenth from the head, their normals facing out of it.
    let mut points: Vec<(f32, f32, f32)> = Vec::new();
    for (mesh, open) in meshes.iter().zip(&measured) {
        for ((p, n), o) in mesh.positions.iter().zip(&mesh.normals).zip(open) {
            let out = (*p - centre).normalize_or_zero();
            let depth = (*p - centre).length() - head.radius(out);
            points.push((depth, n.dot(out), *o));
        }
    }
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    let tenth = points.len() / 10;
    let inner = mean(points[..tenth].iter().map(|p| p.2));
    let outer = mean(points[points.len() - tenth..].iter().filter(|p| p.1 > 0.5).map(|p| p.2));
    println!("occlusion: inner tenth {inner:.3}, outer tenth facing out {outer:.3} over {} vertices", points.len());
    assert!(outer > 0.85, "the outside of the hair stays open: {outer}");
    assert!(inner < outer - 0.15, "the hair against the head is buried: {inner} against {outer}");

    // Baked: the default darkens the shade where it is buried, never lightens it, and leaves the outside near as it was.
    let (baked, _) = hair_on("standard", &styled);
    let (before, after) = (shades(&open), shades(&baked));
    assert_eq!(before.len(), after.len());
    assert!(before.iter().zip(&after).all(|(b, a)| *a <= *b + 1e-6 && *a > 0.0), "only darker, never unset");
    let darkened = before.iter().zip(&after).filter(|(b, a)| **a < **b * 0.8).count();
    assert!(darkened * 5 > before.len(), "a good share darkens: {darkened} of {}", before.len());
    let (again, _) = hair_on("standard", &styled);
    assert_eq!(shades(&again), after, "the same hair bakes the same shade");
}

#[test]
fn flyaways_shed_at_live_quality_and_baby_hairs_lie_along_the_hairline_clear_of_the_head() {
    let mut none = HairParams::default();
    none.strays.share = 0.0;
    let mut shed = HairParams::default();
    shed.strays.share = 0.6;
    let vertices = |out: &PartOutput| hair_meshes(out).iter().map(|m| m.positions.len()).sum::<usize>();
    let (bare, head) = hair_on("tall", &none);
    let (default, _) = hair_on("tall", &shed);
    let mut babies = shed.clone();
    babies.strays.baby = 40;
    let (with_babies, _) = hair_on("tall", &babies);
    let (n0, n1, n2) = (vertices(&bare), vertices(&default), vertices(&with_babies));
    println!("vertices: no strays {n0}, flyaways off 60% of the outer locks {n1}, and 40 baby hairs {n2}");
    assert!(n1 > n0, "the hair sheds flyaways at live quality");
    // A live strand is a tube of 8 a ring with two caps of 9: each wisp adds a whole number of them.
    assert!(n2 > n1 && (n2 - n1) % 40 == 0, "40 baby hairs of one size each: {}", n2 - n1);

    let centre = head.centre();
    let deepest = |out: &PartOutput, from: usize| {
        let mut worst = f32::NEG_INFINITY;
        let mut offset = 0;
        for m in hair_meshes(out) {
            for (i, p) in m.positions.iter().enumerate() {
                if offset + i < from {
                    continue;
                }
                let d = *p - centre;
                worst = worst.max(head.radius(d.normalize_or_zero()) - d.length());
            }
            offset += m.positions.len();
        }
        worst
    };
    // The strays come last in the static mesh; the static mesh comes first.
    let stat = hair_meshes(&with_babies)[0].positions.len();
    let first_stray = stat - (n2 - n0);
    let inside = deepest(&with_babies, first_stray);
    assert!(inside < 0.0005, "no stray goes more than half a millimetre into the head: {inside}");
    let babies_only = stat - (n2 - n1);
    let lift: Vec<f32> = hair_meshes(&with_babies)[0].positions[babies_only..stat]
        .iter()
        .map(|p| {
            let d = *p - centre;
            d.length() - head.radius(d.normalize_or_zero())
        })
        .collect();
    let highest = lift.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    assert!(highest < 0.006, "baby hairs lie on the skin, within 6 mm of it: {highest}");
    let front = hair_meshes(&with_babies)[0].positions[babies_only..stat].iter().filter(|p| p.z > centre.z).count();
    assert_eq!(front, stat - babies_only, "baby hairs are all on the forehead side");
}
