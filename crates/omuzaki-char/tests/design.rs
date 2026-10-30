//! The design layer on the kit (#2609 to #2615): the dial and anatomy libraries load and every dial, structure and
//! factor moves the shape it names on every template; random anatomies are coherent; age deflates and lowers the fat
//! and recedes the bone; sculpt strokes survive a template change and land where they were made; random sampling stays
//! plausible; material layers and the age re-derive on every build (a recolour, a template change); the cached builder
//! gives the reference builder's bytes; and every design op keeps the editing contract.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss)] // small counts become floats

mod common;

use std::collections::BTreeSet;

use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::design::{
    Brush, DialSetting, Factor, PartPoint, SculptStroke, StructureDef, Tissue, design_library, locate,
};
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, PartId, RegistryBuilder,
};
use omuzaki_core::glam::Vec3;
use omuzaki_doc_mock::MemDoc;
use omuzaki_render_api::{ParamValue, Shading};
use serde_json::json;

const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(omuzaki_face_mock::MockRasterizer))
}

fn kit_doc(template: &str) -> MemDoc {
    let mut doc = MemDoc::from_value(json!({}));
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": template}));
    doc
}

fn kit(template: &str) -> CharacterDef {
    common::definition(&kit_doc(template), "kit")
}

/// `def` with only its body and head: what the dials shape, without the parts that follow them.
fn bare(mut def: CharacterDef) -> CharacterDef {
    def.parts.retain(|id, _| matches!(id.as_str(), "body" | "head"));
    def
}

fn build(def: &CharacterDef) -> BuiltCharacter {
    RegistryBuilder
        .build(&CharId::new("kit").unwrap(), def, &BuildOptions::default(), &services())
        .unwrap_or_else(|e| panic!("doesn't build: {e}"))
}

/// The positions of `part`'s meshes that don't simulate, in order.
fn positions(built: &BuiltCharacter, part: &str) -> Vec<Vec3> {
    built
        .meshes
        .iter()
        .filter(|m| m.part.as_str() == part && !m.mesh.dynamic)
        .flat_map(|m| m.mesh.geometry.positions.iter().copied())
        .collect()
}

/// The largest move of any vertex of `part` between two builds of the same topology, and where it was (before).
fn largest_move(a: &BuiltCharacter, b: &BuiltCharacter, part: &str) -> (f32, Vec3) {
    let (pa, pb) = (positions(a, part), positions(b, part));
    assert_eq!(pa.len(), pb.len(), "{part}: the design must keep the topology");
    pa.iter().zip(&pb).map(|(p, q)| (p.distance(*q), *p)).fold((0.0, Vec3::ZERO), |m, x| if x.0 > m.0 { x } else { m })
}

fn height(built: &BuiltCharacter) -> f32 {
    let ys: Vec<f32> = positions(built, "body").iter().map(|p| p.y).collect();
    ys.iter().copied().fold(f32::MIN, f32::max) - ys.iter().copied().fold(f32::MAX, f32::min)
}

fn finite(built: &BuiltCharacter) -> bool {
    built.meshes.iter().all(|m| {
        let g = &m.mesh.geometry;
        g.positions.iter().all(|p| p.is_finite())
            && g.normals.iter().all(|n| n.is_finite() && (n.length() - 1.0).abs() < 0.05)
    })
}

#[test]
fn the_library_loads_every_dial_and_the_age_model() {
    omuzaki_char::link();
    assert!(omuzaki_char::design::files().len() >= 6, "face, head, body, age and the two anatomy files");
    omuzaki_char::design::load().unwrap();
    let lib = design_library().unwrap();
    assert!(lib.dials.len() >= 40, "{} dials", lib.dials.len());
    let groups: BTreeSet<&str> = lib.dials.iter().map(|d| d.group.as_str()).collect();
    for group in ["face", "head", "neck", "torso", "arms", "legs"] {
        assert!(groups.contains(group), "no `{group}` dials: {groups:?}");
    }
    assert!(lib.dials.iter().filter(|d| d.sided).count() >= 10, "per-side dials");
    assert!(!lib.age.is_empty(), "the age model ships");
    // The anatomy: every tissue, every factor loading several structures, and age carried by the structures.
    assert!(lib.structures.len() >= 30, "{} structures", lib.structures.len());
    for tissue in Tissue::ALL {
        assert!(lib.structures.iter().any(|s| s.tissue == tissue), "no {} structure", tissue.as_str());
    }
    for factor in Factor::ALL {
        let loaded = lib.structures.iter().filter(|s| s.factors.contains_key(&factor)).count();
        assert!(loaded >= 3, "{} loads only {loaded} structures", factor.as_str());
    }
    assert!(lib.structures.iter().filter(|s| !s.age.is_empty()).count() >= 25, "age moves the anatomy");
    assert!(lib.dials.iter().all(|d| d.age.is_empty()), "the face and body age through the anatomy, not the dials");
}

/// The structures and factors at `z` standard deviations, built on `base_def`: each one's largest move of the body or
/// the head, checked for broken vertices and a bounded, visible move.
fn check_anatomy(template: &str, base_def: &CharacterDef, base: &BuiltCharacter, z: f64) {
    let tall = height(base);
    let lib = design_library().unwrap();
    let check = |what: &str, def: &CharacterDef, floor_m: f32| {
        let built = build(def);
        assert!(finite(&built), "{template} {what} {z}: a vertex or a normal is broken");
        let (body, head) = (largest_move(base, &built, "body"), largest_move(base, &built, "head"));
        let moved = body.0.max(head.0);
        assert!(moved > floor_m, "{template} {what} {z}: moves only {moved} m");
        assert!(
            moved < 0.04 * tall,
            "{template} {what} {z}: the body moves {body:?}, the head {head:?}, of a {tall} m body"
        );
    };
    for s in &lib.structures {
        let mut def = base_def.clone();
        def.design.anatomy.structures.insert(s.id.clone(), DialSetting::new(z));
        check(s.id.as_str(), &def, expected_floor(s, z));
    }
    for f in Factor::ALL {
        let mut def = base_def.clone();
        def.design.anatomy.factors.insert(f, z);
        check(f.as_str(), &def, 5e-4);
    }
}

/// The smallest move a structure at `z` must make: a tenth of its millimetres at the centre (the mesh's vertices
/// needn't fall on the bump's peak, and a small template's girths are a fraction of an adult's).
#[allow(clippy::cast_possible_truncation)]
fn expected_floor(s: &StructureDef, z: f64) -> f32 {
    0.1 * s.sd_mm * 1e-3 * z.abs() as f32
}

#[test]
fn every_structure_and_factor_shapes_every_template() {
    omuzaki_char::link();
    for template in TEMPLATES {
        let base_def = bare(kit(template));
        let base = build(&base_def);
        check_anatomy(template, &base_def, &base, 2.0);
        if template == "standard" {
            check_anatomy(template, &base_def, &base, -2.0);
        }
    }
}

/// The mean pairwise correlation of `values` (one row per sample, one column per structure).
fn mean_correlation(values: &[Vec<f64>]) -> f64 {
    let cols = values[0].len();
    let n = values.len() as f64;
    let column = |j: usize| values.iter().map(|row| row[j]).collect::<Vec<_>>();
    let stats = |c: &[f64]| {
        let mean = c.iter().sum::<f64>() / n;
        let var = c.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
        (mean, var.sqrt().max(1e-12))
    };
    let mut sum = 0.0;
    let mut pairs = 0.0;
    for a in 0..cols {
        for b in a + 1..cols {
            let (ca, cb) = (column(a), column(b));
            let ((ma, sa), (mb, sb)) = (stats(&ca), stats(&cb));
            let cov = ca.iter().zip(&cb).map(|(x, y)| (x - ma) * (y - mb)).sum::<f64>() / n;
            sum += cov / (sa * sb);
            pairs += 1.0;
        }
    }
    sum / pairs
}

#[test]
fn random_anatomies_are_coherent_bounded_and_repeatable() {
    omuzaki_char::link();
    let lib = design_library().unwrap();
    // The fat pads the adiposity factor loads strongly, read across many samples.
    let fat: Vec<&StructureDef> = lib
        .structures
        .iter()
        .filter(|s| s.tissue == Tissue::Fat && s.factors.get(&Factor::Adiposity).is_some_and(|w| *w >= 0.6))
        .collect();
    assert!(fat.len() >= 6, "{} fat structures", fat.len());
    let mut doc = kit_doc("standard");
    let base = build(&bare(common::definition(&doc, "kit")));
    let tall = height(&base);
    let mut rows = Vec::new();
    for seed in 0..48u64 {
        common::apply(&mut doc, "anatomy.randomise", json!({"character": "kit", "seed": seed, "asymmetry": 0.4}));
        let def = common::definition(&doc, "kit");
        let anatomy = &def.design.anatomy;
        for s in &lib.structures {
            for side in [None, Some(omuzaki_char_api::design::DialSide::Left)] {
                let v = s.value(anatomy, None, side);
                assert!(v.abs() <= 3.0, "seed {seed} {}: {v}", s.id);
            }
        }
        rows.push(fat.iter().map(|s| s.value(anatomy, None, None)).collect::<Vec<f64>>());
        if seed < 6 {
            let mut again = kit_doc("standard");
            common::apply(&mut again, "anatomy.randomise", json!({"character": "kit", "seed": seed, "asymmetry": 0.4}));
            assert_eq!(def.design, common::definition(&again, "kit").design, "seed {seed} repeats");
            let built = build(&bare(def));
            assert!(finite(&built), "seed {seed}");
            let moved = largest_move(&base, &built, "body").0.max(largest_move(&base, &built, "head").0);
            assert!(moved < 0.04 * tall, "seed {seed}: moves {moved} m");
        }
    }
    // Coherent: the fat pads rise and fall together, as one body's fat does, not as independent sliders would.
    let r = mean_correlation(&rows);
    println!("the fat structures' mean pairwise correlation over {} samples: {r:.2}", rows.len());
    assert!(r > 0.5, "the fat pads vary independently ({r:.2})");
}

#[test]
fn a_factor_moves_the_face_and_the_body_together() {
    omuzaki_char::link();
    let mut doc = kit_doc("standard");
    let base = build(&bare(common::definition(&doc, "kit")));
    common::round_trip(&mut doc, "anatomy.set", json!({"character": "kit", "name": "adiposity", "value": 2.0}));
    let heavy = build(&bare(common::definition(&doc, "kit")));
    let (head, body) = (largest_move(&base, &heavy, "head").0, largest_move(&base, &heavy, "body").0);
    println!("adiposity +2: the head moves up to {:.1} mm, the body {:.1} mm", head * 1e3, body * 1e3);
    assert!(head > 3e-3 && body > 1e-2, "head {head}, body {body}");
    // Outward: the volume grows.
    let (pa, pb) = (positions(&base, "head"), positions(&heavy, "head"));
    let centre = pa.iter().copied().sum::<Vec3>() / pa.len() as f32;
    let out: f32 = pa.iter().zip(&pb).map(|(p, q)| (*q - *p).dot((*p - centre).normalize_or_zero())).sum();
    assert!(out > 0.0, "adiposity fills the face out");
}

/// The mean height of the head's vertices that `age` moved outward and inward by more than `at` metres.
fn moved_heights(young: &BuiltCharacter, old: &BuiltCharacter, at: f32) -> (f32, f32, usize, usize) {
    let (pa, pb) = (positions(young, "head"), positions(old, "head"));
    let centre = pa.iter().copied().sum::<Vec3>() / pa.len() as f32;
    let (mut up, mut down, mut n_out, mut n_in) = (0.0, 0.0, 0, 0);
    for (p, q) in pa.iter().zip(&pb) {
        // Only the face's front, where the fat pads and the folds are.
        if (*p - centre).z <= 0.0 {
            continue;
        }
        let radial = (*q - *p).dot((*p - centre).normalize_or_zero());
        if radial > at {
            up += p.y;
            n_out += 1;
        } else if radial < -at {
            down += p.y;
            n_in += 1;
        }
    }
    (up / n_out.max(1) as f32, down / n_in.max(1) as f32, n_out, n_in)
}

#[test]
fn age_deflates_and_lowers_the_fat_and_cuts_the_folds() {
    omuzaki_char::link();
    let mut doc = kit_doc("standard");
    common::apply(&mut doc, "character.set_age", json!({"character": "kit", "age": 30.0}));
    let thirty = build(&bare(common::definition(&doc, "kit")));
    common::apply(&mut doc, "character.set_age", json!({"character": "kit", "age": 80.0}));
    let eighty = build(&bare(common::definition(&doc, "kit")));
    assert!(finite(&eighty));
    let (gain_y, loss_y, gains, losses) = moved_heights(&thirty, &eighty, 4e-4);
    println!("age 30 to 80: {gains} vertices fill out at mean height {gain_y:.3} m, {losses} sink at {loss_y:.3} m");
    assert!(gains > 20 && losses > 20, "{gains} fill out, {losses} sink");
    // The fat leaves the upper face and gathers low (the jowls, under the chin): what fills out sits lower than what
    // sinks.
    assert!(gain_y < loss_y, "the filled vertices ({gain_y}) sit below the sunken ones ({loss_y})");
    // The folds are grooves: the nasolabial fold sinks the skin beside the mouth at 80 more than at 30.
    let listed = common::plan(&doc, "anatomy.list", json!({"character": "kit", "tissue": "skin"})).unwrap();
    let out = listed.output.unwrap();
    let fold = out["structures"].as_array().unwrap().iter().find(|s| s["id"] == "nasolabial_fold").unwrap();
    assert!(fold["value"]["left"].as_f64().unwrap() > 1.5, "{fold}");
}

#[test]
fn every_dial_moves_the_shape_on_every_template() {
    omuzaki_char::link();
    let lib = design_library().unwrap();
    for template in TEMPLATES {
        let base_def = bare(kit(template));
        let base = build(&base_def);
        let tall = height(&base);
        for dial in &lib.dials {
            let values: &[f64] = if template == "standard" { &[1.0, -1.0] } else { &[1.0] };
            for &value in values {
                let mut def = base_def.clone();
                def.design.dials.insert(dial.id.clone(), DialSetting::new(value * dial.range.1.min(-dial.range.0)));
                let built = build(&def);
                assert!(finite(&built), "{template} {} {value}: a vertex or a normal is broken", dial.id);
                let moved = largest_move(&base, &built, "body").0.max(largest_move(&base, &built, "head").0);
                assert!(moved > 4e-4 * tall, "{template} {} {value}: moves only {moved} m", dial.id);
                assert!(moved < 0.06 * tall, "{template} {} {value}: moves {moved} m of a {tall} m body", dial.id);
            }
        }
    }
}

#[test]
fn a_sided_dial_moves_one_side_only() {
    omuzaki_char::link();
    let base_def = bare(kit("standard"));
    let base = build(&base_def);
    let mut def = base_def.clone();
    let id = design_library().unwrap().dials.iter().find(|d| d.id.as_str() == "calf_size").unwrap().id.clone();
    def.design.dials.insert(id, DialSetting { value: 0.0, left: 1.0, right: 0.0 });
    let built = build(&def);
    let (pa, pb) = (positions(&base, "body"), positions(&built, "body"));
    let (mut left, mut right) = (0.0f32, 0.0f32);
    for (p, q) in pa.iter().zip(&pb) {
        let d = p.distance(*q);
        if p.x > 0.0 { left = left.max(d) } else { right = right.max(d) }
    }
    assert!(left > 2e-3 && right < 1e-5, "left {left}, right {right}");
}

/// The head point of a stroke made at bind-space `p` on `def`'s head.
fn head_point(def: &CharacterDef, p: Vec3) -> PartPoint {
    locate(&CharId::new("kit").unwrap(), def, &PartId::new("head").unwrap(), p, &BuildOptions::default(), &services())
        .unwrap()
}

#[test]
fn a_sculpt_stroke_lands_where_it_was_made_and_survives_a_template_change() {
    omuzaki_char::link();
    let stroke = SculptStroke {
        brush: Brush::Inflate,
        at: PartPoint::Head { dir: [0.5, -0.3, 0.8], lift: 1.0 },
        radius: 0.25,
        strength: 0.05,
        direction: None,
        symmetric: false,
    };
    let leg = SculptStroke {
        at: PartPoint::Body { region: omuzaki_char_api::design::RegionGroup::LegLeft, t: 0.7, angle: 3.1, lift: 1.0 },
        radius: 0.8,
        strength: 0.15,
        ..stroke.clone()
    };
    for template in TEMPLATES {
        let base_def = bare(kit(template));
        let base = build(&base_def);
        let mut def = base_def.clone();
        def.design.sculpt.insert(PartId::new("head").unwrap(), vec![stroke.clone()]);
        def.design.sculpt.insert(PartId::new("body").unwrap(), vec![leg.clone()]);
        let built = build(&def);
        assert!(finite(&built), "{template}");
        // The head: the vertex that moved most is where the stroke was made, in the head's proportions.
        let (moved, at) = largest_move(&base, &built, "head");
        assert!(moved > 1e-3, "{template}: the head stroke moved {moved} m");
        let PartPoint::Head { dir, lift } = head_point(&base_def, at) else { panic!("a head point") };
        let (got, want) = (Vec3::from(dir).normalize(), Vec3::from([0.5, -0.3, 0.8]).normalize());
        assert!(got.angle_between(want).to_degrees() < 8.0, "{template}: the stroke peaks at {got}, made at {want}");
        // On the surface: the peak is a vertex of the bare head, so it lies on the head's surface.
        assert!((lift - 1.0).abs() < 0.05, "{template}: the stroke peaks off the surface (lift {lift})");
        // The leg: the stroke stays on the left calf's back.
        let (moved, at) = largest_move(&base, &built, "body");
        assert!(moved > 2e-3, "{template}: the leg stroke moved {moved} m");
        assert!(at.x > 0.0 && at.z < 0.05, "{template}: the leg stroke peaks at {at}");
    }
}

#[test]
fn every_stroke_of_every_brush_builds() {
    omuzaki_char::link();
    let base_def = bare(kit("standard"));
    let base = build(&base_def);
    for brush in Brush::ALL {
        let mut def = base_def.clone();
        let strength = if brush.is_fractional() { 0.8 } else { 0.05 };
        let direction = (brush == Brush::Grab).then_some([0.0, 0.0, 1.0]);
        let first = SculptStroke {
            brush: Brush::Inflate,
            at: PartPoint::Head { dir: [0.0, 0.2, 1.0], lift: 1.0 },
            radius: 0.3,
            strength: 0.06,
            direction: None,
            symmetric: true,
        };
        let stroke = SculptStroke { brush, strength, direction, ..first.clone() };
        def.design.sculpt.insert(PartId::new("head").unwrap(), vec![first, stroke]);
        let built = build(&def);
        assert!(finite(&built), "{brush:?}");
        assert!(largest_move(&base, &built, "head").0 > 1e-4, "{brush:?}");
    }
}

#[test]
fn random_characters_are_plausible_and_repeatable() {
    omuzaki_char::link();
    let mut doc = kit_doc("standard");
    let base = build(&bare(common::definition(&doc, "kit")));
    let tall = height(&base);
    let mut seen = BTreeSet::new();
    for seed in 0..10u64 {
        common::apply(&mut doc, "shape.randomise", json!({"character": "kit", "seed": seed, "asymmetry": 0.5}));
        let def = common::definition(&doc, "kit");
        let again = {
            let mut d = kit_doc("standard");
            common::apply(&mut d, "shape.randomise", json!({"character": "kit", "seed": seed, "asymmetry": 0.5}));
            common::definition(&d, "kit")
        };
        assert_eq!(def.design, again.design, "seed {seed} repeats");
        seen.insert(format!("{:?}", def.design.dials.values().next()));
        let built = build(&bare(def));
        assert!(finite(&built), "seed {seed}");
        let moved = largest_move(&base, &built, "body").0.max(largest_move(&base, &built, "head").0);
        assert!(moved < 0.05 * tall, "seed {seed}: moves {moved} m");
    }
    assert!(seen.len() >= 9, "the seeds differ");
}

fn mean_colour(built: &BuiltCharacter, part: &str) -> [f32; 3] {
    let mut sum = [0.0f32; 3];
    let mut n = 0.0f32;
    for m in built.meshes.iter().filter(|m| m.part.as_str() == part) {
        for c in &m.mesh.geometry.colours {
            for k in 0..3 {
                sum[k] += c[k];
            }
            n += 1.0;
        }
    }
    sum.map(|s| s / n.max(1.0))
}

/// The mean distance of `part`'s surface colours (each material's base times the vertex colour) from `target` (linear).
fn distance_to(built: &BuiltCharacter, part: &str, target: Vec3) -> f32 {
    let (mut sum, mut n) = (0.0f32, 0.0f32);
    for m in built.meshes.iter().filter(|m| m.part.as_str() == part) {
        let base = built.materials[&m.mesh.material].base.to_linear();
        let g = &m.mesh.geometry;
        for i in 0..g.positions.len() {
            let c = g.colours.get(i).map_or(Vec3::ONE, |c| Vec3::new(c[0], c[1], c[2]));
            sum += (base * c).distance(target);
            n += 1.0;
        }
    }
    sum / n.max(1.0)
}

#[test]
fn material_layers_rederive_on_a_recolour_and_a_template_change() {
    omuzaki_char::link();
    for template in ["standard", "tall"] {
        let mut doc = kit_doc(template);
        let plain = common::build(&doc, "kit");
        common::round_trip(
            &mut doc,
            "part.layer.add",
            json!({"character": "kit", "part": "head", "layer": {"id": "blush", "kind": {"type": "makeup", "zone": "blush", "colour": "#ff2050", "amount": 0.9}}}),
        );
        common::round_trip(
            &mut doc,
            "part.layer.add",
            json!({"character": "kit", "part": "boots", "layer": {"id": "mud", "kind": {"type": "tint", "colour": "#403020"}, "masks": [{"mask": {"type": "height", "from": 0.0, "to": 0.3}}]}}),
        );
        let layered = common::build(&doc, "kit");
        let (a, b) = (mean_colour(&plain, "head"), mean_colour(&layered, "head"));
        assert!(b[0] - b[1] > a[0] - a[1] + 1e-3, "{template}: the blush reddens the head ({a:?} to {b:?})");
        let mud = omuzaki_core::colour::Colour::hex(0x0040_3020).to_linear();
        let (a, b) = (distance_to(&plain, "boots", mud), distance_to(&layered, "boots", mud));
        assert!(b < 0.8 * a, "{template}: the mud takes the boots' colour toward it ({a} to {b})");
        // The base colour is untouched, so the boots still recolour through the palette.
        for (name, spec) in &plain.materials {
            assert_eq!(spec.base, layered.materials[name].base, "{template}: {name}'s base colour");
        }
        // A template change re-derives the layers on the new shape.
        common::apply(&mut doc, "body.set_template", json!({"character": "kit", "template": "mini"}));
        let mini = common::build(&doc, "kit");
        assert!(finite(&mini));
        let c = mean_colour(&mini, "head");
        assert!(c[0] > c[1], "{template} to mini: the blush is still there ({c:?})");
    }
}

fn hair_mid(built: &BuiltCharacter) -> Option<omuzaki_core::colour::Colour> {
    built.materials.values().find_map(|spec| match &spec.shading {
        Shading::Program { params, .. } => params.iter().find_map(|(k, v)| match (k.as_str(), v) {
            ("mid", ParamValue::Colour(c)) => Some(*c),
            _ => None,
        }),
        _ => None,
    })
}

fn saturation(c: omuzaki_core::colour::Colour) -> f32 {
    let l = c.to_linear();
    let (hi, lo) = (l.max_element(), l.min_element());
    if hi <= 0.0 { 0.0 } else { (hi - lo) / hi }
}

#[test]
fn age_reshapes_the_face_and_ages_the_skin_and_hair() {
    omuzaki_char::link();
    let mut doc = kit_doc("standard");
    let young_adult = common::build(&doc, "kit");
    common::round_trip(&mut doc, "character.set_age", json!({"character": "kit", "age": 72.0}));
    let old = common::build(&doc, "kit");
    assert!(finite(&old));
    assert!(largest_move(&young_adult, &old, "head").0 > 1e-3, "age 72 reshapes the face");
    let (a, b) = (hair_mid(&young_adult).unwrap(), hair_mid(&old).unwrap());
    assert!(saturation(b) < saturation(a) - 0.1, "the hair greys ({a:?} to {b:?})");
    common::round_trip(&mut doc, "character.set_age", json!({"character": "kit", "age": 6.0}));
    let child = common::build(&doc, "kit");
    assert!(largest_move(&young_adult, &child, "head").0 > 1e-3, "age 6 reshapes the face");
    assert_eq!(hair_mid(&child), hair_mid(&young_adult), "a child's hair keeps its colour");
    // Every look works: the toon and the photoreal presets both build the aged character.
    common::round_trip(&mut doc, "character.set_age", json!({"character": "kit"}));
    assert!(common::at(&doc, "characters/kit/design").is_none(), "no age, no design node");
}

#[test]
fn the_cached_builder_gives_the_reference_builders_character() {
    omuzaki_char::link();
    let mut doc = kit_doc("standard");
    let builder = CachingBuilder::new();
    let id = CharId::new("kit").unwrap();
    let steps = [
        ("shape.set_dial", json!({"character": "kit", "dial": "jaw_width", "value": 0.7})),
        (
            "part.sculpt.stroke",
            json!({"character": "kit", "part": "head", "brush": "inflate", "at": {"on": "head", "dir": [0.4, -0.2, 0.9]}, "radius": 0.2, "strength": 0.05, "symmetric": true}),
        ),
        (
            "part.layer.add",
            json!({"character": "kit", "part": "shirt", "layer": {"id": "wear", "kind": {"type": "wear", "colour": "#ffffff", "amount": 0.8}}}),
        ),
        ("character.set_age", json!({"character": "kit", "age": 55.0})),
        ("anatomy.set", json!({"character": "kit", "name": "robusticity", "value": 1.2})),
        ("shape.set_dial", json!({"character": "kit", "dial": "calf_size", "value": -0.5, "side": "right"})),
        ("anatomy.set", json!({"character": "kit", "name": "malar_fat", "value": -0.8, "side": "left"})),
        ("body.set_template", json!({"character": "kit", "template": "tall"})),
        ("part.sculpt.clear", json!({"character": "kit", "part": "head"})),
        ("shape.reset", json!({"character": "kit"})),
        ("anatomy.randomise", json!({"character": "kit", "seed": 9})),
    ];
    for (op, args) in steps {
        common::apply(&mut doc, op, args);
        let def = common::definition(&doc, "kit");
        let cached = builder.build(&id, &def, &BuildOptions::default(), &services()).unwrap();
        let reference = build(&def);
        assert_eq!(cached.meshes, reference.meshes, "after {op}: the meshes");
        assert_eq!(cached.materials, reference.materials, "after {op}: the materials");
    }
}

#[test]
fn the_design_ops_keep_the_editing_contract() {
    omuzaki_char::link();
    let mut doc = kit_doc("standard");
    let steps = [
        ("shape.set_dial", json!({"character": "kit", "dial": "cheek_fullness", "value": 0.5})),
        ("shape.set_dial", json!({"character": "kit", "dial": "cheek_fullness", "value": 0.2, "side": "left"})),
        ("shape.randomise", json!({"character": "kit", "seed": 4, "group": "legs"})),
        ("character.set_age", json!({"character": "kit", "age": 40.0})),
        (
            "part.sculpt.stroke",
            json!({"character": "kit", "part": "head", "brush": "crease", "at": [0.03, 1.3, 0.1], "radius": 0.1, "strength": 0.02}),
        ),
        (
            "part.sculpt.stroke",
            json!({"character": "kit", "part": "body", "brush": "grab", "at": [0.12, 0.45, 0.05], "radius": 0.6, "strength": 0.1, "direction": [1.0, 0.0, 0.0]}),
        ),
        ("part.sculpt.clear", json!({"character": "kit", "part": "body", "index": 0})),
        (
            "part.layer.add",
            json!({"character": "kit", "part": "head", "layer": {"id": "shadow", "kind": {"type": "makeup", "zone": "eyeshadow", "colour": "#704080", "amount": 0.5}}}),
        ),
        (
            "part.layer.add",
            json!({"character": "kit", "part": "head", "layer": {"id": "wet", "kind": {"type": "wet", "amount": 0.4}}, "index": 0}),
        ),
        ("part.layer.move", json!({"character": "kit", "part": "head", "layer": "shadow", "index": 0})),
        ("part.layer.set", json!({"character": "kit", "part": "head", "layer": "wet", "opacity": 0.3})),
        (
            "part.layer.mask",
            json!({"character": "kit", "part": "head", "layer": "wet", "masks": [{"mask": {"type": "facing", "dir": [0.0, 1.0, 0.0], "from": 0.2, "to": 0.8}}]}),
        ),
        (
            "part.decal.add",
            json!({"character": "kit", "part": "head", "id": "star", "shape": "star", "at": {"on": "head", "dir": [-0.45, -0.1, 0.9]}, "size": 0.015, "colour": "#3050c0"}),
        ),
        ("part.layer.remove", json!({"character": "kit", "part": "head", "layer": "wet"})),
        ("shape.reset", json!({"character": "kit", "group": "legs"})),
        ("anatomy.set", json!({"character": "kit", "name": "adiposity", "value": 1.0})),
        ("anatomy.set", json!({"character": "kit", "name": "jowl_fat", "value": 0.6, "side": "right"})),
        ("anatomy.randomise", json!({"character": "kit", "seed": 2, "amount": 0.7, "asymmetry": 0.3})),
        ("anatomy.reset", json!({"character": "kit", "name": "adiposity"})),
        ("shape.randomise", json!({"character": "kit", "seed": 5})),
        ("anatomy.reset", json!({"character": "kit"})),
    ];
    for (op, args) in steps {
        common::round_trip(&mut doc, op, args);
    }
    let def = common::definition(&doc, "kit");
    assert_eq!(def.design.layers[&PartId::new("head").unwrap()].len(), 2);
    let built = common::build(&doc, "kit");
    assert!(finite(&built));
    let listed = common::plan(&doc, "shape.list_dials", json!({"character": "kit", "group": "face"})).unwrap();
    let out = listed.output.unwrap();
    assert!(out["count"].as_u64().unwrap() >= 15, "{out}");
}

#[test]
fn the_ops_refuse_what_they_cannot_do() {
    omuzaki_char::link();
    let mut doc = kit_doc("standard");
    let e = common::refused(&doc, "shape.set_dial", json!({"character": "kit", "dial": "jaw_widht", "value": 0.5}));
    assert!(e.hint.unwrap().contains("jaw_width"));
    let e = common::refused(&doc, "shape.set_dial", json!({"character": "kit", "dial": "jaw_width", "value": 3.0}));
    assert_eq!(e.field.as_deref(), Some("value"));
    let e = common::refused(
        &doc,
        "shape.set_dial",
        json!({"character": "kit", "dial": "nose_length", "value": 0.2, "side": "left"}),
    );
    assert_eq!(e.field.as_deref(), Some("side"));
    let e = common::refused(&doc, "character.set_age", json!({"character": "kit", "age": 400.0}));
    assert_eq!(e.field.as_deref(), Some("age"));
    let e = common::refused(
        &doc,
        "part.sculpt.stroke",
        json!({"character": "kit", "part": "head", "brush": "grab", "at": {"on": "head", "dir": [0.0, 0.0, 1.0]}, "radius": 0.2, "strength": 0.1}),
    );
    assert_eq!(e.field.as_deref(), Some("direction"));
    common::apply(
        &mut doc,
        "part.layer.add",
        json!({"character": "kit", "part": "head", "layer": {"id": "a", "kind": {"type": "wet", "amount": 0.4}}}),
    );
    let e = common::refused(
        &doc,
        "part.layer.add",
        json!({"character": "kit", "part": "head", "layer": {"id": "a", "kind": {"type": "wet", "amount": 0.4}}}),
    );
    assert_eq!(e.field.as_deref(), Some("layer/id"));
    let e = common::refused(&doc, "part.layer.remove", json!({"character": "kit", "part": "head", "layer": "b"}));
    assert_eq!(e.field.as_deref(), Some("layer"));
    let e = common::refused(&doc, "anatomy.set", json!({"character": "kit", "name": "malar_fta", "value": 1.0}));
    assert!(e.hint.unwrap().contains("malar_fat"));
    let e = common::refused(&doc, "anatomy.set", json!({"character": "kit", "name": "adiposity", "value": 4.0}));
    assert_eq!(e.field.as_deref(), Some("value"));
    let e = common::refused(
        &doc,
        "anatomy.set",
        json!({"character": "kit", "name": "adiposity", "value": 1.0, "side": "left"}),
    );
    assert_eq!(e.field.as_deref(), Some("side"));
    let e = common::refused(&doc, "anatomy.list", json!({"tissue": "cartilage"}));
    assert_eq!(e.field.as_deref(), Some("tissue"));
}
