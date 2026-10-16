//! The `headphones` accessory (CB-43, #344): the band over the hair of every hairstyle on `main`, the cups over the ears,
//! on the four templates.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

mod accessory_common;

use accessory_common::{
    ACC, TEMPLATES, assert_closed, assert_mirror_symmetric, assert_recolours_in_place, assert_rides_the_bone,
    assert_weights_valid, box_centre, build_budget_ms, build_ms, built_scene, character, character_with,
    distance_to_mesh, hairstyles, mesh_named, meshes, pid, scene, styles, vertices, vertices_of,
};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::find_kind;
use omuzaki_core::glam::{Quat, Vec3};
use serde_json::json;

/// The headphones to try: plain, each flag the kind has, and all of them.
fn variants() -> Vec<(String, serde_json::Value)> {
    let mut all = vec![("plain".to_owned(), json!({}))];
    all.extend(styles("headphones"));
    all.push(("both".to_owned(), json!({ "cat_ears": true, "mic": true, "clearance": 0.075 })));
    all
}

/// Triangles one accessory kind may use at live quality.
const TRIANGLE_BUDGET: usize = 2000;

#[test]
fn every_style_builds_closed_shells_on_the_four_templates() {
    for template in TEMPLATES {
        for (name, params) in variants() {
            let s = built_scene(template, "headphones", params.clone(), None);
            assert_closed(&s.built, &format!("headphones {name} on {template}"));
            assert_weights_valid(&s.built, &format!("headphones {name} on {template}"));
        }
    }
}

#[test]
fn the_kind_is_an_accessory_that_needs_the_head() {
    let registration = find_kind("headphones").expect("headphones is registered");
    let meta = (registration.meta)();
    assert_eq!(meta.category, omuzaki_char_api::PartCategory::Accessory);
    assert_eq!(meta.requires, &["head"]);
    let defaults = (registration.defaults)();
    assert_eq!((registration.check)(&defaults).unwrap(), defaults);
    assert!((registration.check)(&json!({ "no_such_field": 1 })).is_err());
    for bad in [json!({ "cup_radius": 0.0 }), json!({ "clearance": -0.01 }), json!({ "band_width": 1.0 })] {
        assert!((registration.check)(&bad).is_err(), "{bad} should be refused");
    }
}

#[test]
fn the_band_clears_the_hair_of_every_hairstyle_by_2_mm() {
    for template in TEMPLATES {
        for (hair_name, hair) in hairstyles() {
            for (name, params) in variants().into_iter().filter(|(n, _)| ["plain", "both"].contains(&n.as_str())) {
                let def = character_with(template, Some(&hair), &[(ACC, "headphones", params.clone(), None)]);
                let s = scene(&def).unwrap();
                let band = mesh_named(&s.built, "-band");
                // Only the hair near the band's height can be near it.
                let hair_points: Vec<Vec3> = vertices_of(&s.built, "hair");
                let top = s.head.centre().y;
                let worst = hair_points
                    .iter()
                    .filter(|p| p.y > top - 0.02)
                    .map(|p| distance_to_mesh(*p, band))
                    .fold(f32::MAX, f32::min);
                assert!(worst >= 0.002, "{name} on {template} over {hair_name}: the band is {worst} m from the hair");
            }
        }
    }
}

#[test]
fn the_cups_sit_over_the_ears_within_3_mm() {
    for template in TEMPLATES {
        let s = built_scene(template, "headphones", json!({}), None);
        let cups = mesh_named(&s.built, "-cup");
        for (side, name) in [(1.0_f32, "ear_l"), (-1.0, "ear_r")] {
            let ear =
                s.built.anchors.iter().find(|((_, anchor), _)| anchor.as_str() == name).expect("the head has ears").1;
            let on_side: Vec<Vec3> = cups.geometry.positions.iter().copied().filter(|p| p.x * side > 0.0).collect();
            let centre = box_centre(&on_side);
            assert!(
                (centre.y - ear.position.y).abs() < 0.003,
                "{template} {name}: cup centre {} vs ear {}",
                centre.y,
                ear.position.y
            );
            assert!(
                (centre.z - ear.position.z).abs() < 0.003,
                "{template} {name}: cup centre {} vs ear {}",
                centre.z,
                ear.position.z
            );
            assert!(centre.x * side > ear.position.x * side, "{template} {name}: the cup is outside the ear's root");
        }
    }
}

#[test]
fn without_a_side_ear_the_cups_still_sit_at_the_sides() {
    let mut def = character("standard", "headphones", json!({}), None);
    def.parts.get_mut(&pid("head")).unwrap().params["ears"] = json!({ "kind": "none" });
    let s = scene(&def).unwrap();
    assert!(s.built.anchors.keys().all(|(_, name)| !name.as_str().starts_with("ear_")), "no ear anchors");
    assert_closed(&s.built, "headphones without ears");
    let cups = mesh_named(&s.built, "-cup");
    let (left, right) = (
        cups.geometry.positions.iter().filter(|p| p.x > s.head.centre().x).count(),
        cups.geometry.positions.iter().filter(|p| p.x < s.head.centre().x).count(),
    );
    assert!(left > 0 && left == right, "a cup each side: {left} and {right} vertices");
}

#[test]
fn the_pair_is_an_exact_mirror_image() {
    let s = built_scene("standard", "headphones", json!({ "cat_ears": true }), None);
    assert_mirror_symmetric(&vertices(&s.built), s.head.centre().x, 5e-4, "headphones with cat ears");
}

#[test]
fn the_headphones_ride_the_head_and_the_mic_stays_off_the_face() {
    for template in TEMPLATES {
        let s = built_scene(template, "headphones", json!({ "mic": true, "cat_ears": true }), None);
        for turn in [Quat::from_rotation_y(40f32.to_radians()), Quat::from_rotation_x(-25f32.to_radians())] {
            assert_rides_the_bone(&s.built, HumanoidBone::Head, turn, &format!("headphones on {template}"));
        }
        // The capsule at the end of the boom stays outside the head.
        let accent = mesh_named(&s.built, "-accent");
        let lowest = accent
            .geometry
            .positions
            .iter()
            .map(|p| accessory_common::above_head(s.head.as_ref(), *p))
            .fold(f32::MAX, f32::min);
        assert!(lowest > 0.0, "{template}: the mic is {lowest} m inside the head");
    }
}

#[test]
fn a_palette_change_recolours_the_headphones_without_new_geometry() {
    let params = json!({ "band": "@b", "cup": "@c", "accent": "@a", "mic": true });
    assert_recolours_in_place(character("standard", "headphones", params, None), &["b", "c", "a"]);
}

#[test]
fn the_bytes_are_deterministic_and_inside_the_budget() {
    for (name, params) in variants() {
        let (a, b) = (
            built_scene("tall", "headphones", params.clone(), None),
            built_scene("tall", "headphones", params.clone(), None),
        );
        for (x, y) in meshes(&a.built).iter().zip(meshes(&b.built)) {
            assert_eq!(x.geometry, y.geometry, "{name}");
        }
        let triangles: usize = meshes(&a.built).iter().map(|m| m.geometry.indices.len() / 3).sum();
        println!("headphones {name}: {triangles} triangles");
        assert!(triangles <= TRIANGLE_BUDGET, "headphones {name}: {triangles} triangles");
    }
}

#[test]
fn the_headphones_build_in_5_ms_or_less() {
    let def = character("standard", "headphones", json!({ "mic": true, "cat_ears": true }), None);
    let ms = build_ms(&def, 20);
    println!("headphones: {ms:.3} ms");
    assert!(ms <= build_budget_ms(), "headphones took {ms:.2} ms");
}

#[test]
fn every_documented_preset_is_among_the_styles_the_sweeps_use() {
    accessory_common::assert_styles_found("headphones", &["cat_ears", "mic"]);
}
