//! The `hat` accessory (CB-43, #332): five styles on the four templates, on the head within 1 mm, clear of the ears and
//! of the face below the brow line.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

mod accessory_common;

use accessory_common::{
    TEMPLATES, above_head, assert_closed, assert_recolours_in_place, assert_rides_the_bone, assert_weights_valid,
    build_budget_ms, build_ms, built_scene, character, meshes, styles, vertices,
};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::find_kind;
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_face_api::FaceLayout;
use serde_json::json;

/// Triangles one accessory kind may use at live quality.
const TRIANGLE_BUDGET: usize = 2000;

#[test]
fn every_style_builds_closed_shells_on_the_four_templates() {
    for template in TEMPLATES {
        for (name, params) in styles("hat") {
            let s = built_scene(template, "hat", params.clone(), None);
            assert_closed(&s.built, &format!("hat {name} on {template}"));
            assert_weights_valid(&s.built, &format!("hat {name} on {template}"));
        }
    }
}

#[test]
fn the_kind_is_an_accessory_that_needs_the_head_and_refuses_what_would_cover_the_face() {
    let registration = find_kind("hat").expect("hat is registered");
    let meta = (registration.meta)();
    assert_eq!(meta.category, omuzaki_char_api::PartCategory::Accessory);
    assert_eq!(meta.requires, &["head"]);
    let defaults = (registration.defaults)();
    assert_eq!((registration.check)(&defaults).unwrap(), defaults);
    assert!((registration.check)(&json!({ "no_such_field": 1 })).is_err());
    for bad in [
        json!({ "rim_deg": 85.0 }),
        json!({ "style": "witch", "brim_tilt_deg": -10.0 }),
        json!({ "tilt_deg": 40.0 }),
        json!({ "thickness": 0.0 }),
        json!({ "style": "cap", "tilt_deg": 20.0, "rim_deg": 80.0 }),
    ] {
        assert!((registration.check)(&bad).is_err(), "{bad} should be refused");
    }
    // A brim may hang over the eyes only when asked.
    assert!((registration.check)(&json!({ "style": "witch", "brim_tilt_deg": -30.0, "brim_down": true })).is_ok());
}

#[test]
fn the_hat_rests_on_the_head_within_1_mm() {
    for template in TEMPLATES {
        for (name, params) in styles("hat") {
            let s = built_scene(template, "hat", params.clone(), None);
            let heights: Vec<f32> = vertices(&s.built).iter().map(|p| above_head(s.head.as_ref(), *p)).collect();
            let lowest = heights.iter().copied().fold(f32::MAX, f32::min);
            assert!(
                (-0.0005..=0.001).contains(&lowest),
                "hat {name} on {template}: its nearest vertex is {lowest} m off the head"
            );
        }
    }
}

#[test]
fn lift_raises_the_hat_to_sit_on_the_hair() {
    let s = built_scene("standard", "hat", json!({ "style": "beanie", "lift": 0.056 }), None);
    let lowest = vertices(&s.built).iter().map(|p| above_head(s.head.as_ref(), *p)).fold(f32::MAX, f32::min);
    assert!((lowest - 0.056).abs() < 0.001, "{lowest}");
}

#[test]
fn the_hat_clears_the_ears() {
    for template in TEMPLATES {
        for (name, params) in styles("hat") {
            let s = built_scene(template, "hat", params.clone(), None);
            let points = vertices(&s.built);
            for ((_, anchor), ear) in s.built.anchors.iter().filter(|((_, a), _)| a.as_str().starts_with("ear_")) {
                let nearest = points.iter().map(|p| p.distance(ear.position)).fold(f32::MAX, f32::min);
                assert!(nearest > 0.02, "hat {name} on {template}: {nearest} m from {anchor}");
            }
        }
    }
}

#[test]
fn the_front_stays_above_the_brow_line_unless_the_brim_hangs_down() {
    let layout = FaceLayout::default();
    let brow = (layout.eye_y + layout.brow_gap) as f32;
    for template in TEMPLATES {
        for (name, params) in styles("hat").into_iter().filter(|(_, p)| p.get("brim_down").is_none()) {
            let s = built_scene(template, "hat", params.clone(), None);
            let c = s.head.centre();
            let lowest_front = vertices(&s.built)
                .iter()
                .filter(|p| p.z > c.z + 0.3 * s.head.radius(Vec3::Z) && (p.x - c.x).abs() < 0.15)
                .map(|p| p.y - c.y)
                .fold(f32::MAX, f32::min);
            assert!(
                lowest_front >= brow,
                "hat {name} on {template}: its front reaches {lowest_front} m, below the brow at {brow}"
            );
        }
    }
}

#[test]
fn a_hanging_brim_may_pass_the_brow_and_the_default_may_not() {
    let low = built_scene(
        "standard",
        "hat",
        json!({ "style": "witch", "brim_down": true, "brim_tilt_deg": -50.0, "tilt_deg": 10.0 }),
        None,
    );
    let c = low.head.centre();
    let lowest = vertices(&low.built).iter().filter(|p| p.z > c.z).map(|p| p.y - c.y).fold(f32::MAX, f32::min);
    let flat = built_scene("standard", "hat", json!({ "style": "witch" }), None);
    let flat_lowest = vertices(&flat.built).iter().filter(|p| p.z > c.z).map(|p| p.y - c.y).fold(f32::MAX, f32::min);
    assert!(lowest < flat_lowest - 0.03, "the hanging brim is only {} m lower", flat_lowest - lowest);
}

#[test]
fn the_hat_rides_the_head() {
    for template in TEMPLATES {
        let s = built_scene(template, "hat", json!({ "style": "witch" }), None);
        for turn in [Quat::from_rotation_y(45f32.to_radians()), Quat::from_rotation_z(20f32.to_radians())] {
            assert_rides_the_bone(&s.built, HumanoidBone::Head, turn, &format!("hat on {template}"));
        }
    }
}

#[test]
fn a_palette_change_recolours_the_hat_without_new_geometry() {
    let params = json!({ "style": "beanie", "hat": "@h", "band": "@b" });
    assert_recolours_in_place(character("standard", "hat", params, None), &["h", "b"]);
}

#[test]
fn the_bytes_are_deterministic_and_inside_the_budget() {
    for (name, params) in styles("hat") {
        let (a, b) =
            (built_scene("tall", "hat", params.clone(), None), built_scene("tall", "hat", params.clone(), None));
        for (x, y) in meshes(&a.built).iter().zip(meshes(&b.built)) {
            assert_eq!(x.geometry, y.geometry, "{name}");
        }
        let triangles: usize = meshes(&a.built).iter().map(|m| m.geometry.indices.len() / 3).sum();
        println!("hat {name}: {triangles} triangles");
        assert!(triangles <= TRIANGLE_BUDGET, "hat {name}: {triangles} triangles");
    }
}

#[test]
fn the_hats_build_in_5_ms_or_less() {
    for (name, params) in styles("hat") {
        let ms = build_ms(&character("standard", "hat", params, None), 20);
        println!("hat {name}: {ms:.3} ms");
        assert!(ms <= build_budget_ms(), "hat {name} took {ms:.2} ms");
    }
}

#[test]
fn every_documented_preset_is_among_the_styles_the_sweeps_use() {
    accessory_common::assert_styles_found("hat", &["beret", "cap", "witch", "beanie", "crown"]);
}
