//! The `tail` accessory (CB-43, #196): four presets on the four templates, rooted at the tailbone, skinned along a
//! spring-bone chain that validates against the built skeleton. The lag of the chain under a hips turn is measured with
//! the real solver in `omuzaki-test-char` (`char_accessory_tail_springs`).

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

mod accessory_common;

use accessory_common::{
    TEMPLATES, assert_closed, assert_recolours_in_place, assert_rides_the_bone, assert_skins_to_the_built_mesh_at_bind,
    assert_weights_valid, back_of_the_torso, build_budget_ms, build_ms, built_scene, character, meshes, skinned,
    styles, vertices,
};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{JointRef, find_kind};
use omuzaki_core::glam::{Quat, Vec3};
use serde_json::json;

/// Triangles one accessory kind may use at live quality.
const TRIANGLE_BUDGET: usize = 2000;

#[test]
fn every_preset_builds_closed_shells_on_the_four_templates() {
    for template in TEMPLATES {
        for (name, params) in styles("tail") {
            let s = built_scene(template, "tail", params.clone(), None);
            assert_closed(&s.built, &format!("tail {name} on {template}"));
            assert_weights_valid(&s.built, &format!("tail {name} on {template}"));
            assert_skins_to_the_built_mesh_at_bind(&s.built, &format!("tail {name} on {template}"));
        }
    }
}

#[test]
fn the_kind_is_an_accessory_with_checked_parameters() {
    let registration = find_kind("tail").expect("tail is registered");
    let meta = (registration.meta)();
    assert_eq!(meta.category, omuzaki_char_api::PartCategory::Accessory);
    assert_eq!(meta.requires, &["body"]);
    let defaults = (registration.defaults)();
    assert_eq!((registration.check)(&defaults).unwrap(), defaults);
    assert!((registration.check)(&json!({ "no_such_field": 1 })).is_err());
    for bad in [
        json!({ "bones": 2 }),
        json!({ "bones": 9 }),
        json!({ "length": 0.0 }),
        json!({ "drag": 1.5 }),
        json!({ "tuft": -0.1 }),
    ] {
        assert!((registration.check)(&bad).is_err(), "{bad} should be refused");
    }
}

#[test]
fn the_chain_validates_against_the_built_skeleton_and_hangs_from_the_hips() {
    for template in TEMPLATES {
        for (name, params) in styles("tail") {
            let b = built_scene(template, "tail", params.clone(), None).built;
            let what = format!("tail {name} on {template}");
            b.rig.springs.validate(&b.rig.skeleton).unwrap_or_else(|e| panic!("{what}: invalid springs: {e}"));
            let chain = b
                .rig
                .springs
                .chains
                .iter()
                .find(|c| c.name == "tail_acc")
                .unwrap_or_else(|| panic!("{what}: no chain"));
            let bones = b.rig.skeleton.bones();
            // The first bone hangs from the hips and each one from the one before.
            let first = &bones[chain.joints[0].bone.index()];
            assert_eq!(first.parent, b.rig.bone(&JointRef::HIPS), "{what}: the root hangs from the hips");
            for pair in chain.joints.windows(2) {
                assert_eq!(bones[pair[1].bone.index()].parent, Some(pair[0].bone), "{what}: a chain of children");
            }
            assert_eq!(chain.center, None, "{what}: simulated in world space, so a hips turn makes it lag");
            let stiffness: Vec<f32> = chain.joints.iter().map(|j| j.params.stiffness).collect();
            assert!(stiffness.windows(2).all(|w| w[0] >= w[1]), "{what}: stiffness falls to the tip: {stiffness:?}");
        }
    }
}

#[test]
fn the_bone_count_is_a_parameter() {
    for bones in 3..=8_u8 {
        let b = built_scene("standard", "tail", json!({ "bones": bones }), None).built;
        let chain = b.rig.springs.chains.iter().find(|c| c.name == "tail_acc").unwrap();
        assert_eq!(chain.joints.len(), usize::from(bones));
        b.rig.springs.validate(&b.rig.skeleton).unwrap();
        assert_closed(&b, &format!("tail with {bones} bones"));
    }
}

#[test]
fn the_tail_leaves_the_back_and_is_rooted_inside_it() {
    for template in TEMPLATES {
        for (name, params) in styles("tail") {
            let s = built_scene(template, "tail", params.clone(), None);
            let back = back_of_the_torso(s.fit.as_ref());
            let zs: Vec<f32> = vertices(&s.built).iter().map(|p| p.z).collect();
            let (rearmost, frontmost) =
                (zs.iter().copied().fold(f32::MAX, f32::min), zs.iter().copied().fold(f32::MIN, f32::max));
            // A bunny's tail is a puff a few centimetres deep; the others stream out behind.
            let reach = if name == "bunny" { 0.01 } else { 0.03 };
            assert!(
                rearmost < back - reach,
                "tail {name} on {template}: reaches only to {rearmost}, the back is at {back}"
            );
            assert!(
                frontmost > back,
                "tail {name} on {template}: its root does not reach into the back ({frontmost} vs {back})"
            );
        }
    }
}

#[test]
fn a_turn_of_the_first_bone_swings_the_tip_and_leaves_the_root() {
    let b = built_scene("standard", "tail", json!({}), None).built;
    let skeleton = &b.rig.skeleton;
    let root = skeleton.find("tail_acc_0").unwrap().index();
    let rest = accessory_common::bind_pose(&b);
    let mut turned = rest.clone();
    turned.local[root].rotation = Quat::from_rotation_y(40f32.to_radians()) * rest.local[root].rotation;
    let mesh = meshes(&b)[0];
    let (before, after) = (skinned(&b, mesh, &rest), skinned(&b, mesh, &turned));
    let moved: Vec<f32> = before.iter().zip(&after).map(|(p, q)| p.distance(*q)).collect();
    let (least, most) = (moved.iter().copied().fold(f32::MAX, f32::min), moved.iter().copied().fold(0.0, f32::max));
    assert!(least < 0.005, "the root end moves {least} m");
    assert!(most > 0.1, "the tip moves only {most} m");
}

#[test]
fn the_tail_rides_the_hips() {
    for template in TEMPLATES {
        let s = built_scene(template, "tail", json!({}), None);
        for turn in [Quat::from_rotation_y(60f32.to_radians()), Quat::from_rotation_x(-30f32.to_radians())] {
            assert_rides_the_bone(&s.built, HumanoidBone::Hips, turn, &format!("tail on {template}"));
        }
    }
}

#[test]
fn a_palette_change_recolours_the_tail_without_new_geometry() {
    let params = json!({ "kind": "dragon", "tail": "@t", "tuft_colour": "@u", "accent": "@a" });
    assert_recolours_in_place(character("standard", "tail", params, None), &["t", "u", "a"]);
}

#[test]
fn the_bytes_are_deterministic_and_inside_the_budget() {
    for (name, params) in styles("tail") {
        let (a, b) =
            (built_scene("tall", "tail", params.clone(), None), built_scene("tall", "tail", params.clone(), None));
        for (x, y) in meshes(&a.built).iter().zip(meshes(&b.built)) {
            assert_eq!(x.geometry, y.geometry, "{name}");
        }
        assert_eq!(a.built.rig.springs, b.built.rig.springs, "{name}: the spring set");
        let triangles: usize = meshes(&a.built).iter().map(|m| m.geometry.indices.len() / 3).sum();
        println!("tail {name}: {triangles} triangles");
        assert!(triangles <= TRIANGLE_BUDGET, "tail {name}: {triangles} triangles");
    }
}

#[test]
fn the_length_follows_the_body() {
    let reach = |template| {
        let s = built_scene(template, "tail", json!({}), None);
        let zs: Vec<f32> = vertices(&s.built).iter().map(|p| p.z).collect();
        zs.iter().copied().fold(f32::MIN, f32::max) - zs.iter().copied().fold(f32::MAX, f32::min)
    };
    let (chibi, tall) = (reach("soft-sd"), reach("tall"));
    assert!(chibi < 0.6 * tall, "a chibi's tail ({chibi}) is shorter than a tall body's ({tall})");
}

#[test]
fn a_bunnys_puff_is_a_fifth_as_wide_as_the_hips_on_every_template() {
    for template in TEMPLATES {
        let s = built_scene(template, "tail", json!({ "kind": "bunny" }), None);
        let hips = accessory_common::rest_joint(&s.built, HumanoidBone::Hips);
        // The torso's width at the hips' height: the section nearest it.
        let width = (0..=40)
            .filter_map(|k| s.fit.section(omuzaki_char_api::BodyRegion::Torso, k as f32 / 40.0))
            .min_by(|a, b| (a.centre.y - hips.y).abs().total_cmp(&(b.centre.y - hips.y).abs()))
            .map(|section| 2.0 * section.radii.x)
            .unwrap();
        let points = vertices(&s.built);
        let (lo, hi) =
            points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), p| (l.min(*p), h.max(*p)));
        let size = hi - lo;
        let share = size.x / width;
        assert!(
            (0.17..=0.23).contains(&share),
            "bunny on {template}: {} m across, {share} of the hips' {width} m",
            size.x
        );
        // A puff, not a stub: about as long and as tall as it is wide.
        for (axis, along) in [("tall", size.y), ("long", size.z)] {
            assert!(
                (0.8..=1.25).contains(&(along / size.x)),
                "bunny on {template}: {along} m {axis}, {} m across",
                size.x
            );
        }
    }
}

#[test]
fn the_tails_build_in_5_ms_or_less() {
    for (name, params) in styles("tail") {
        let ms = build_ms(&character("standard", "tail", params, None), 20);
        println!("tail {name}: {ms:.3} ms");
        assert!(ms <= build_budget_ms(), "tail {name} took {ms:.2} ms");
    }
}

fn offset(translation: [f64; 3], rotation_deg: [f64; 3], scale: f64) -> omuzaki_char_api::Attach {
    omuzaki_char_api::Attach::Bone {
        bone: JointRef::HIPS,
        offset: omuzaki_char_api::AttachOffset { translation, rotation_deg, scale },
    }
}

#[test]
fn an_attachment_moves_turns_and_scales_the_whole_tail_exactly_once() {
    let base = built_scene("standard", "tail", json!({}), None);
    let hips = base.built.rig.skeleton.humanoid_bone(HumanoidBone::Hips).unwrap();
    let pivot = accessory_common::rest_joint(&base.built, HumanoidBone::Hips);
    let _ = hips;
    let (before, after_move) = (vertices(&base.built), {
        let moved = built_scene("standard", "tail", json!({}), Some(offset([0.1, 0.02, -0.03], [0.0; 3], 1.0)));
        assert_skins_to_the_built_mesh_at_bind(&moved.built, "a translated tail");
        vertices(&moved.built)
    });
    for (a, b) in before.iter().zip(&after_move) {
        assert!((*b - *a - omuzaki_core::glam::Vec3::new(0.1, 0.02, -0.03)).length() < 1e-4, "{a:?} to {b:?}");
    }
    // Scale 2 doubles the tail about the hips, not four times: the farthest vertex from the pivot is twice as far.
    let reach = |points: &[omuzaki_core::glam::Vec3]| points.iter().map(|p| p.distance(pivot)).fold(0.0, f32::max);
    let scaled = built_scene("standard", "tail", json!({}), Some(offset([0.0; 3], [0.0; 3], 2.0)));
    assert_skins_to_the_built_mesh_at_bind(&scaled.built, "a scaled tail");
    let ratio = reach(&vertices(&scaled.built)) / reach(&before);
    assert!((ratio - 2.0).abs() < 0.02, "scale 2 made the tail {ratio} times as far");
    // A turn about the hips keeps every vertex at its distance from them.
    let turned = built_scene("standard", "tail", json!({}), Some(offset([0.0; 3], [0.0, 0.0, 40.0], 1.0)));
    assert_skins_to_the_built_mesh_at_bind(&turned.built, "a turned tail");
    for (a, b) in before.iter().zip(vertices(&turned.built)) {
        assert!((a.distance(pivot) - b.distance(pivot)).abs() < 1e-4);
    }
    // The chain follows the attachment: it hangs from the joint it names.
    let chest = Some(omuzaki_char_api::Attach::Bone {
        bone: JointRef::CHEST,
        offset: omuzaki_char_api::AttachOffset::default(),
    });
    let on_chest = built_scene("standard", "tail", json!({}), chest).built;
    let chain = on_chest.rig.springs.chains.iter().find(|c| c.name == "tail_acc").unwrap();
    assert_eq!(on_chest.rig.skeleton.bones()[chain.joints[0].bone.index()].parent, on_chest.rig.bone(&JointRef::CHEST));
    assert_skins_to_the_built_mesh_at_bind(&on_chest, "a tail on the chest");
}

#[test]
fn every_documented_preset_is_among_the_styles_the_sweeps_use() {
    accessory_common::assert_styles_found("tail", &["fox", "cat", "dragon", "bunny"]);
}
