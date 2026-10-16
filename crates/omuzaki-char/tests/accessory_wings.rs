//! The `wings` accessory (CB-43, #196): four kinds on the four templates, a three-bone frame per wing that folds and
//! spreads through a custom channel, a spring-driven flutter, and wings that stay off the back in every pose.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

mod accessory_common;

use accessory_common::{
    TEMPLATES, assert_closed, assert_mirror_symmetric, assert_recolours_in_place, assert_rides_the_bone,
    assert_skins_to_the_built_mesh_at_bind, assert_weights_valid, back_of_the_torso, bind_pose, build_budget_ms,
    build_ms, built_scene, character, closed_volume, meshes, rest_joint, skinned, styles, vertices,
};
use omuzaki_anim_api::{Channel, ChannelBuf, HumanoidBone, Pose};
use omuzaki_char_api::{BodyRegion, BuiltCharacter, find_kind};
use omuzaki_core::glam::{Quat, Vec3};
use serde_json::json;

/// Triangles one accessory kind may use at live quality.
const TRIANGLE_BUDGET: usize = 2000;

/// The bind pose with the fold channel `custom00` at `value`, the drivers run.
fn folded(b: &BuiltCharacter, channel: Channel, value: f32) -> Pose {
    let mut pose = bind_pose(b);
    let mut channels = ChannelBuf::NEUTRAL;
    channels.set(channel, value);
    b.rig.driver_plan().expect("the drivers compile").apply(&channels, &mut pose);
    pose
}

fn wing_vertices(b: &BuiltCharacter, pose: &Pose) -> Vec<Vec3> {
    meshes(b).iter().flat_map(|m| skinned(b, m, pose)).collect()
}

#[test]
fn every_kind_builds_closed_shells_on_the_four_templates() {
    let mut all = styles("wings");
    all.push(("few-feathers".to_owned(), json!({ "kind": "feathered", "feathers": 3, "feather_size": 0.5 })));
    all.push(("many-feathers".to_owned(), json!({ "kind": "feathered", "feathers": 16, "feather_size": 1.5 })));
    all.push(("many-blades".to_owned(), json!({ "kind": "mechanical", "feathers": 16 })));
    for template in TEMPLATES {
        for (name, params) in &all {
            let s = built_scene(template, "wings", params.clone(), None);
            assert_closed(&s.built, &format!("wings {name} on {template}"));
            assert_weights_valid(&s.built, &format!("wings {name} on {template}"));
            assert_skins_to_the_built_mesh_at_bind(&s.built, &format!("wings {name} on {template}"));
        }
    }
}

#[test]
fn the_kind_is_an_accessory_with_checked_parameters() {
    let registration = find_kind("wings").expect("wings is registered");
    let meta = (registration.meta)();
    assert_eq!(meta.category, omuzaki_char_api::PartCategory::Accessory);
    assert_eq!(meta.requires, &["body"]);
    let defaults = (registration.defaults)();
    assert_eq!((registration.check)(&defaults).unwrap(), defaults);
    assert!((registration.check)(&json!({ "no_such_field": 1 })).is_err());
    for bad in [
        json!({ "fold_channel": 16 }),
        json!({ "feathers": 2 }),
        json!({ "feather_size": 3.0 }),
        json!({ "span": 0.0 }),
        json!({ "drag": -0.1 }),
    ] {
        assert!((registration.check)(&bad).is_err(), "{bad} should be refused");
    }
}

#[test]
fn the_rig_validates_and_has_a_frame_driver_and_a_flutter_chain_per_wing() {
    for template in TEMPLATES {
        for (name, params) in styles("wings") {
            let b = built_scene(template, "wings", params.clone(), None).built;
            let what = format!("wings {name} on {template}");
            b.rig.springs.validate(&b.rig.skeleton).unwrap_or_else(|e| panic!("{what}: invalid springs: {e}"));
            b.rig.driver_plan().unwrap_or_else(|e| panic!("{what}: bad drivers: {e}"));
            assert_eq!(b.rig.drivers.len(), 6, "{what}: shoulder, elbow and wrist per wing");
            for side in ["l", "r"] {
                let chain = b
                    .rig
                    .springs
                    .chains
                    .iter()
                    .find(|c| c.name == format!("wings_acc_{side}_flap"))
                    .unwrap_or_else(|| panic!("{what}: no {side} flutter chain"));
                assert_eq!(chain.joints.len(), 2, "{what}");
                assert_eq!(chain.center, None, "{what}: simulated in world space");
                // The flutter bones are never driven: the fold owns the frame bones, the springs the flutter.
                for joint in &chain.joints {
                    assert!(
                        b.rig.drivers.iter().all(|d| d.bone != joint.bone),
                        "{what}: a flutter bone is also driven"
                    );
                }
                for role in ["shoulder", "elbow", "wrist"] {
                    assert!(
                        b.rig.skeleton.find(&format!("wings_acc_{side}_{role}")).is_some(),
                        "{what}: no {side} {role}"
                    );
                }
            }
        }
    }
}

#[test]
fn spread_is_the_built_shape_and_fold_turns_the_frame() {
    let channel = Channel::custom(0).unwrap();
    for template in TEMPLATES {
        let b = built_scene(template, "wings", json!({ "kind": "bat" }), None).built;
        let bind = wing_vertices(&b, &bind_pose(&b));
        let spread = wing_vertices(&b, &folded(&b, channel, 1.0));
        let worst = bind.iter().zip(&spread).map(|(p, q)| p.distance(*q)).fold(0.0, f32::max);
        assert!(worst < 1e-4, "{template}: spread is {worst} m from the built wing");
        let fold = wing_vertices(&b, &folded(&b, channel, 0.0));
        let (spread_width, fold_width) = (width(&spread), width(&fold));
        assert!(fold_width < 0.5 * spread_width, "{template}: folded {fold_width} m wide, spread {spread_width}");
        // Halfway is between: the channel is continuous.
        let half = width(&wing_vertices(&b, &folded(&b, channel, 0.5)));
        assert!(fold_width < half && half < spread_width, "{template}: {fold_width} < {half} < {spread_width}");
    }
}

#[test]
fn a_folded_wing_lies_on_the_upper_back_from_its_root_towards_the_waist() {
    // Folded, a wing pivots at its root on the shoulder blade and lies along the back: its top is within a few
    // centimetres of the root's height, it ends above the hips, and below the waist nothing stands out past the side
    // of the torso (a tip poking out there reads as a skirt from the front).
    let channel = Channel::custom(0).unwrap();
    let standard_neck = rest_joint(&built_scene("standard", "wings", json!({}), None).built, HumanoidBone::Neck).y;
    for template in TEMPLATES {
        for (name, params) in styles("wings") {
            let s = built_scene(template, "wings", params.clone(), None);
            let b = &s.built;
            let what = format!("wings {name} on {template}");
            // Metres at a standard body: the wings are sized by the body.
            let k = rest_joint(b, HumanoidBone::Neck).y / standard_neck;
            let root = {
                let skeleton = &b.rig.skeleton;
                let mut world = Pose::IDENTITY;
                skeleton.world_pose(&skeleton.rest_pose(), &mut world);
                world.local[skeleton.find("wings_acc_l_shoulder").unwrap().index()].translation
            };
            let points = wing_vertices(b, &folded(b, channel, 0.0));
            let top = points.iter().map(|p| p.y).fold(f32::MIN, f32::max);
            let bottom = points.iter().map(|p| p.y).fold(f32::MAX, f32::min);
            assert!(
                (top - root.y).abs() <= 0.03 * k,
                "{what}: the folded wing's top is {} m from its root's height",
                top - root.y
            );
            let hips = rest_joint(b, HumanoidBone::Hips).y;
            assert!(bottom >= hips, "{what}: the folded wing reaches {} m below the hips", hips - bottom);
            let waist = rest_joint(b, HumanoidBone::Spine).y;
            let (low, high) = (
                s.fit.section(BodyRegion::Torso, 0.0).unwrap().centre.y,
                s.fit.section(BodyRegion::Torso, 1.0).unwrap().centre.y,
            );
            for p in points.iter().filter(|p| p.y < waist) {
                let side =
                    s.fit.section(BodyRegion::Torso, ((p.y - low) / (high - low)).clamp(0.0, 1.0)).unwrap().radii.x;
                assert!(
                    p.x.abs() <= side + 0.01 * k,
                    "{what}: below the waist a vertex stands {} m past the side of the torso",
                    p.x.abs() - side
                );
            }
        }
    }
}

fn width(points: &[Vec3]) -> f32 {
    let (lo, hi) = points.iter().fold((f32::MAX, f32::MIN), |(l, h), p| (l.min(p.x), h.max(p.x)));
    hi - lo
}

#[test]
fn the_fold_channel_is_a_parameter() {
    let b = built_scene("standard", "wings", json!({ "fold_channel": 7 }), None).built;
    let (other, mine) = (Channel::custom(0).unwrap(), Channel::custom(7).unwrap());
    let rest = wing_vertices(&b, &folded(&b, mine, 1.0));
    let folded_by_other = wing_vertices(&b, &folded(&b, other, 1.0));
    // Channel 0 drives nothing: the wing stays folded; channel 7 spreads it.
    assert!(width(&folded_by_other) < 0.5 * width(&rest));
}

#[test]
fn the_wings_stay_2_mm_off_the_back_folded_and_spread() {
    let channel = Channel::custom(0).unwrap();
    for template in TEMPLATES {
        for (name, params) in styles("wings") {
            let s = built_scene(template, "wings", params.clone(), None);
            let back = back_of_the_torso(s.fit.as_ref());
            for value in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let nearest = wing_vertices(&s.built, &folded(&s.built, channel, value))
                    .iter()
                    .map(|p| p.z)
                    .fold(f32::MIN, f32::max);
                assert!(
                    nearest <= back - 0.002,
                    "wings {name} on {template} at fold {value}: {} m from the back",
                    back - nearest
                );
            }
        }
    }
}

#[test]
fn folding_keeps_every_vertex_in_its_plane_so_the_shells_stay_watertight() {
    let channel = Channel::custom(0).unwrap();
    let s = built_scene("standard", "wings", json!({ "kind": "bat" }), None);
    let planes = |pose: &Pose| {
        let zs: Vec<f32> = wing_vertices(&s.built, pose).iter().map(|p| p.z).collect();
        (zs.iter().copied().fold(f32::MAX, f32::min), zs.iter().copied().fold(f32::MIN, f32::max))
    };
    let (spread, fold) = (planes(&folded(&s.built, channel, 1.0)), planes(&folded(&s.built, channel, 0.0)));
    assert!((spread.0 - fold.0).abs() < 1e-4 && (spread.1 - fold.1).abs() < 1e-4, "{spread:?} vs {fold:?}");
    // The index buffer never changes with a pose, so a closed, positively wound shell stays one.
    for mesh in meshes(&s.built) {
        assert!(closed_volume(mesh) > 0.0, "{}", mesh.name);
    }
}

#[test]
fn the_pair_is_an_exact_mirror_image_at_rest_and_folded() {
    let channel = Channel::custom(0).unwrap();
    for (name, params) in styles("wings") {
        let s = built_scene("standard", "wings", params, None);
        for value in [1.0, 0.0] {
            let points = wing_vertices(&s.built, &folded(&s.built, channel, value));
            assert_mirror_symmetric(&points, 0.0, 5e-4, &format!("wings {name} at fold {value}"));
        }
    }
}

#[test]
fn the_wings_ride_the_chest() {
    for template in TEMPLATES {
        let s = built_scene(template, "wings", json!({ "kind": "fairy" }), None);
        for turn in [Quat::from_rotation_y(50f32.to_radians()), Quat::from_rotation_z(15f32.to_radians())] {
            assert_rides_the_bone(&s.built, HumanoidBone::Chest, turn, &format!("wings on {template}"));
        }
    }
}

#[test]
fn a_palette_change_recolours_the_wings_without_new_geometry() {
    let params = json!({ "kind": "mechanical", "wing": "@w", "edge": "@e" });
    assert_recolours_in_place(character("standard", "wings", params, None), &["w", "e"]);
}

#[test]
fn the_bytes_are_deterministic_and_inside_the_budget() {
    for (name, params) in styles("wings") {
        let (a, b) =
            (built_scene("tall", "wings", params.clone(), None), built_scene("tall", "wings", params.clone(), None));
        for (x, y) in meshes(&a.built).iter().zip(meshes(&b.built)) {
            assert_eq!(x.geometry, y.geometry, "{name}");
        }
        assert_eq!(a.built.rig.springs, b.built.rig.springs, "{name}: the spring set");
        assert_eq!(a.built.rig.drivers, b.built.rig.drivers, "{name}: the drivers");
        let triangles: usize = meshes(&a.built).iter().map(|m| m.geometry.indices.len() / 3).sum();
        println!("wings {name}: {triangles} triangles");
        assert!(triangles <= TRIANGLE_BUDGET, "wings {name}: {triangles} triangles");
    }
}

#[test]
fn the_wings_scale_with_the_body_and_the_span_is_a_parameter() {
    let spread = |template, params| {
        let s = built_scene(template, "wings", params, None);
        width(&vertices(&s.built))
    };
    assert!(spread("soft-sd", json!({})) < 0.6 * spread("tall", json!({})));
    assert!(spread("standard", json!({ "span": 0.3 })) < 0.7 * spread("standard", json!({ "span": 0.6 })));
}

#[test]
fn the_wings_build_in_5_ms_or_less() {
    for (name, params) in styles("wings") {
        let ms = build_ms(&character("standard", "wings", params, None), 20);
        println!("wings {name}: {ms:.3} ms");
        assert!(ms <= build_budget_ms(), "wings {name} took {ms:.2} ms");
    }
}

fn offset(translation: [f64; 3], rotation_deg: [f64; 3], scale: f64) -> omuzaki_char_api::Attach {
    omuzaki_char_api::Attach::Bone {
        bone: omuzaki_char_api::JointRef::CHEST,
        offset: omuzaki_char_api::AttachOffset { translation, rotation_deg, scale },
    }
}

#[test]
fn a_turned_attachment_folds_the_wings_about_their_own_plane_and_a_scale_counts_once() {
    let channel = Channel::custom(0).unwrap();
    let base = built_scene("standard", "wings", json!({ "kind": "bat" }), None);
    let turned =
        built_scene("standard", "wings", json!({ "kind": "bat" }), Some(offset([0.0; 3], [0.0, 30.0, 0.0], 1.0)));
    assert_skins_to_the_built_mesh_at_bind(&turned.built, "wings on a turned attachment");
    // Spread is the built shape, and folding never takes a vertex out of the wings' (turned) plane.
    let normal = omuzaki_core::glam::Quat::from_rotation_y(30f32.to_radians()) * Vec3::Z;
    let plane = |points: &[Vec3]| {
        let ds: Vec<f32> = points.iter().map(|p| normal.dot(*p)).collect();
        (ds.iter().copied().fold(f32::MAX, f32::min), ds.iter().copied().fold(f32::MIN, f32::max))
    };
    let spread = wing_vertices(&turned.built, &folded(&turned.built, channel, 1.0));
    let built_shape = vertices(&turned.built);
    let worst = built_shape.iter().zip(&spread).map(|(p, q)| p.distance(*q)).fold(0.0, f32::max);
    assert!(worst < 1e-4, "spread is {worst} m from the built wing");
    for value in [0.0, 0.5] {
        let posed = plane(&wing_vertices(&turned.built, &folded(&turned.built, channel, value)));
        let at_rest = plane(&spread);
        assert!(
            (posed.0 - at_rest.0).abs() < 1e-3 && (posed.1 - at_rest.1).abs() < 1e-3,
            "fold {value}: {posed:?} against {at_rest:?}"
        );
    }
    // Scale 2 doubles the wings.
    let doubled = built_scene("standard", "wings", json!({ "kind": "bat" }), Some(offset([0.0; 3], [0.0; 3], 2.0)));
    assert_skins_to_the_built_mesh_at_bind(&doubled.built, "wings on a scaled attachment");
    let ratio = width(&vertices(&doubled.built)) / width(&vertices(&base.built));
    assert!((ratio - 2.0).abs() < 0.05, "scale 2 made the wings {ratio} times as wide");
}

#[test]
fn every_documented_preset_is_among_the_styles_the_sweeps_use() {
    accessory_common::assert_styles_found("wings", &["feathered", "bat", "fairy", "mechanical"]);
}
