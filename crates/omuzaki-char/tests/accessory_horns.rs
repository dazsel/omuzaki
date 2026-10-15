//! The `horns` accessory (CB-43, #132): four curl presets on the four templates, rooted on the skull, both sides the
//! same, rigid on the head, and the hair of today's styles around them.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

mod accessory_common;

use accessory_common::{
    ACC, TEMPLATES, above_head, assert_closed, assert_mirror_symmetric, assert_recolours_in_place,
    assert_rides_the_bone, assert_weights_valid, build_budget_ms, build_ms, built_scene, character, character_with,
    inside_mesh, meshes, meshes_of, scene, skinned, vertices,
};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::find_kind;
use omuzaki_core::glam::{Quat, Vec3};
use serde_json::{Value, json};

/// Triangles one accessory kind may use at live quality.
const TRIANGLE_BUDGET: usize = 2000;

const SHAPES: [&str; 4] = ["swept_back", "ram", "oni", "antler"];

fn cases() -> Vec<Value> {
    let mut all: Vec<Value> = SHAPES.iter().map(|s| json!({ "shape": s })).collect();
    all.push(json!({ "shape": "swept_back", "mirror": false, "direction": [0.0, 0.95, 0.3] }));
    all.push(json!({ "shape": "ram", "ridges": 6, "glow": 0.8 }));
    all.push(json!({ "shape": "oni", "tip_round": 1.0, "taper": 0.0, "curl": 2.0 }));
    all.push(json!({ "shape": "antler", "glow": 1.0, "spread_deg": 40.0, "tilt_deg": -30.0 }));
    all
}

#[test]
fn every_shape_builds_closed_shells_on_the_four_templates() {
    for template in TEMPLATES {
        for params in cases() {
            let s = built_scene(template, "horns", params.clone(), None);
            assert_closed(&s.built, &format!("horns {params} on {template}"));
            assert_weights_valid(&s.built, &format!("horns {params} on {template}"));
        }
    }
}

#[test]
fn the_kind_is_an_accessory_that_needs_the_head() {
    let registration = find_kind("horns").expect("horns is registered");
    let meta = (registration.meta)();
    assert_eq!(meta.category, omuzaki_char_api::PartCategory::Accessory);
    assert_eq!(meta.requires, &["head"]);
    let defaults = (registration.defaults)();
    assert_eq!((registration.check)(&defaults).unwrap(), defaults);
    assert!((registration.check)(&json!({ "no_such_field": 1 })).is_err());
    for bad in [
        json!({ "direction": [0.0, 0.0, 0.0] }),
        json!({ "direction": [0.5, -0.5, 0.1] }),
        json!({ "length": 0.0 }),
        json!({ "taper": 1.5 }),
        json!({ "ridges": 99 }),
    ] {
        assert!((registration.check)(&bad).is_err(), "{bad} should be refused");
    }
}

#[test]
fn the_pair_is_an_exact_mirror_image_across_the_head() {
    for params in [json!({}), json!({ "shape": "ram", "glow": 1.0 }), json!({ "shape": "antler", "glow": 1.0 })] {
        let s = built_scene("standard", "horns", params.clone(), None);
        assert_mirror_symmetric(&vertices(&s.built), s.head.centre().x, 2e-4, &format!("horns {params}"));
    }
}

#[test]
fn a_single_horn_stands_on_the_centre_line() {
    let s = built_scene("standard", "horns", json!({ "mirror": false, "direction": [0.9, 0.9, 0.3] }), None);
    let xs: Vec<f32> = vertices(&s.built).iter().map(|p| p.x - s.head.centre().x).collect();
    let (min, max) = xs.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    // x is ignored with mirror off, and a swept-back horn leaves the centre line only by its own spread.
    assert!(min < 0.0 && max > 0.0 && (min + max).abs() < 1e-3, "x from {min} to {max}");
}

#[test]
fn the_base_is_buried_in_the_skull_so_there_is_no_gap() {
    for template in TEMPLATES {
        for shape in SHAPES {
            let s = built_scene(template, "horns", json!({ "shape": shape }), None);
            let heights: Vec<f32> = vertices(&s.built).iter().map(|p| above_head(s.head.as_ref(), *p)).collect();
            let lowest = heights.iter().copied().fold(f32::MAX, f32::min);
            assert!(lowest < -0.002, "{shape} on {template}: the base reaches only {lowest} m below the skull");
            // And it rises out of it: the farthest vertex is far outside.
            let highest = heights.iter().copied().fold(f32::MIN, f32::max);
            assert!(highest > 0.02, "{shape} on {template}: the tip is only {highest} m above the skull");
        }
    }
}

#[test]
fn the_horns_ride_the_head_over_a_45_degree_turn() {
    for template in TEMPLATES {
        let s = built_scene(template, "horns", json!({ "shape": "ram" }), None);
        for turn in [
            Quat::from_rotation_y(45f32.to_radians()),
            Quat::from_rotation_y(-45f32.to_radians()),
            Quat::from_rotation_x(30f32.to_radians()),
        ] {
            assert_rides_the_bone(&s.built, HumanoidBone::Head, turn, &format!("horns on {template}"));
        }
    }
}

#[test]
fn glow_builds_cracks_in_an_emissive_material_and_off_builds_none() {
    let off = built_scene("standard", "horns", json!({}), None).built;
    assert!(off.meshes.iter().all(|m| !m.mesh.name.ends_with("horn_glow")));
    let on = built_scene("standard", "horns", json!({ "glow": 0.9 }), None).built;
    let crack = on.meshes.iter().find(|m| m.mesh.name.ends_with("horn_glow")).expect("cracks are built");
    let omuzaki_render_api::Shading::Toon(toon) = &on.materials[&crack.mesh.material].shading else {
        panic!("the crack material is toon");
    };
    assert_ne!(toon.emissive, omuzaki_core::colour::Colour::BLACK, "the cracks give off light");
}

#[test]
fn a_palette_change_recolours_the_horns_without_new_geometry() {
    let params = json!({ "horn": "@h", "horn_tip": "@t", "horn_glow": "@g", "glow": 0.7 });
    assert_recolours_in_place(character("standard", "horns", params, None), &["h", "t", "g"]);
}

#[test]
fn the_bytes_are_deterministic() {
    for params in cases() {
        let (a, b) =
            (built_scene("tall", "horns", params.clone(), None), built_scene("tall", "horns", params.clone(), None));
        for (x, y) in meshes(&a.built).iter().zip(meshes(&b.built)) {
            assert_eq!(x.geometry, y.geometry, "{params}");
        }
    }
}

#[test]
fn triangle_counts_stay_within_the_budget() {
    // The budget for one accessory kind at live quality: it shares the 32 000 triangles of a whole avatar with the body,
    // head and hair.
    for params in cases() {
        let s = built_scene("standard", "horns", params.clone(), None);
        let triangles: usize = meshes(&s.built).iter().map(|m| m.geometry.indices.len() / 3).sum();
        println!("horns {params}: {triangles} triangles");
        assert!(triangles <= TRIANGLE_BUDGET, "horns {params}: {triangles} triangles");
    }
}

#[test]
fn the_horns_build_in_5_ms_or_less() {
    for params in [json!({ "shape": "antler", "glow": 1.0 }), json!({ "shape": "ram", "ridges": 8 })] {
        let ms = build_ms(&character("standard", "horns", params.clone(), None), 20);
        println!("horns {params}: {ms:.3} ms");
        assert!(ms <= build_budget_ms(), "horns {params} took {ms:.2} ms");
    }
}

// ------------------------------------------------------------------------------------------------ the hair of today

/// The hair vertices inside the horns' solid, with the head turned by `yaw` degrees (hair and horns skinned alike).
fn hair_inside_horns(hair: &Value, shape: &str, yaw: f32) -> usize {
    let def = character_with("standard", Some(hair), &[(ACC, "horns", json!({ "shape": shape }), None)]);
    let s = scene(&def).unwrap();
    let skeleton = &s.built.rig.skeleton;
    let head = skeleton.humanoid_bone(HumanoidBone::Head).unwrap().index();
    let rest = skeleton.rest_pose();
    let mut posed = rest.clone();
    posed.local[head].rotation = Quat::from_rotation_y(yaw.to_radians()) * rest.local[head].rotation;
    let horns: Vec<Vec3> = meshes(&s.built).iter().flat_map(|m| skinned(&s.built, m, &posed)).collect();
    let (lo, hi) =
        horns.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), p| (l.min(*p), h.max(*p)));
    let hair: Vec<Vec3> = meshes_of(&s.built, "hair").iter().flat_map(|m| skinned(&s.built, m, &posed)).collect();
    // The horns' meshes, posed, as solids to test the hair against.
    let solids: Vec<omuzaki_char_api::PartMesh> = meshes(&s.built)
        .iter()
        .map(|m| {
            let mut posed_mesh = (*m).clone();
            posed_mesh.geometry.positions = skinned(&s.built, m, &posed);
            posed_mesh
        })
        .collect();
    hair.iter()
        .filter(|p| p.cmpge(lo).all() && p.cmple(hi).all())
        .filter(|p| solids.iter().any(|solid| inside_mesh(**p, solid)))
        .count()
}

#[test]
fn only_the_scalp_shell_meets_the_horns_which_is_the_hole_the_hair_cap_will_get() {
    // The hair cap has no opening for a horn yet (#141 adds it), so the horn passes through it. Every other part of today's hair
    // keeps clear of the horns, at rest and over a 45 degree head turn either way, and the cap's hole is the only thing
    // the default style adds.
    let cap = json!({ "generator": { "type": "cap" } });
    let bangs = json!({ "generator": { "type": "bangs" } });
    let curtain = json!({ "generator": { "type": "curtain" }, "dynamics": { "simulate": true, "follow": "body" } });
    let only = |groups: Value| json!({ "groups": groups });
    for shape in ["swept_back", "ram", "antler"] {
        for yaw in [0.0, 45.0, -45.0] {
            let clear =
                [("bangs", only(json!({ "b-bangs": bangs }))), ("curtain", only(json!({ "c-curtain": curtain })))];
            for (name, hair) in clear {
                let inside = hair_inside_horns(&hair, shape, yaw);
                assert_eq!(inside, 0, "{shape} at yaw {yaw}: {inside} {name} vertices are inside the horns");
            }
            let with_cap =
                hair_inside_horns(&only(json!({ "a-cap": cap, "b-bangs": bangs, "c-curtain": curtain })), shape, yaw);
            let cap_alone = hair_inside_horns(&only(json!({ "a-cap": cap })), shape, yaw);
            assert_eq!(
                with_cap, cap_alone,
                "{shape} at yaw {yaw}: the bangs and the curtain add vertices inside the horns"
            );
        }
    }
}
