//! The `headdress` accessory (#394): a frilled headband over the hair of every hairstyle on the four templates, closed,
//! symmetric, riding the head.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

mod accessory_common;

use accessory_common::{
    ACC, TEMPLATES, assert_closed, assert_mirror_symmetric, assert_recolours_in_place, assert_rides_the_bone,
    assert_weights_valid, built_scene, character, character_with, distance_to_mesh, hairstyles, mesh_named, scene,
    vertices, vertices_of,
};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::find_kind;
use omuzaki_core::glam::{Quat, Vec3};
use serde_json::json;

#[test]
fn the_kind_is_an_accessory_that_needs_the_head() {
    let registration = find_kind("headdress").expect("headdress is registered");
    let meta = (registration.meta)();
    assert_eq!(meta.category, omuzaki_char_api::PartCategory::Accessory);
    assert_eq!(meta.requires, &["head"]);
    let defaults = (registration.defaults)();
    assert_eq!((registration.check)(&defaults).unwrap(), defaults);
    for bad in [json!({ "frill": 0.5 }), json!({ "gathers": 1 }), json!({ "no_such_field": 1 })] {
        assert!((registration.check)(&bad).is_err(), "{bad} should be refused");
    }
}

#[test]
fn it_builds_closed_and_clears_the_hair_of_every_hairstyle_by_2_mm() {
    for template in TEMPLATES {
        for (hair_name, hair) in hairstyles() {
            let def = character_with(template, Some(&hair), &[(ACC, "headdress", json!({}), None)]);
            let s = scene(&def).unwrap();
            let what = format!("headdress on {template} over {hair_name}");
            assert_closed(&s.built, &what);
            assert_weights_valid(&s.built, &what);
            let band = mesh_named(&s.built, "-band");
            let top = s.head.centre().y;
            let worst = vertices_of(&s.built, "hair")
                .iter()
                .filter(|p| p.y > top - 0.02)
                .map(|p| distance_to_mesh(*p, band))
                .fold(f32::MAX, f32::min);
            assert!(worst >= 0.002, "{what}: the band is {worst} m from the hair");
            // The frill stands over the head, never in it.
            let lowest = vertices_of(&s.built, "headdress")
                .iter()
                .map(|p| accessory_common::above_head(s.head.as_ref(), *p))
                .fold(f32::MAX, f32::min);
            assert!(lowest > 0.0, "{what}: the headdress is {lowest} m inside the head");
        }
    }
}

#[test]
fn it_is_an_exact_mirror_image_and_rides_the_head() {
    let s = built_scene("standard", "headdress", json!({}), None);
    assert_mirror_symmetric(&vertices(&s.built), s.head.centre().x, 5e-4, "headdress");
    for template in TEMPLATES {
        let s = built_scene(template, "headdress", json!({}), None);
        for turn in [Quat::from_rotation_y(40f32.to_radians()), Quat::from_rotation_x(-25f32.to_radians())] {
            assert_rides_the_bone(&s.built, HumanoidBone::Head, turn, &format!("headdress on {template}"));
        }
    }
}

#[test]
fn a_palette_change_recolours_it_without_new_geometry() {
    assert_recolours_in_place(
        character("standard", "headdress", json!({ "band": "@b", "lace": "@l" }), None),
        &["b", "l"],
    );
    let _ = Vec3::ZERO;
}
