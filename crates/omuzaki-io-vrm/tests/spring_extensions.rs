//! `VRMC_springBone_extended_collider` and `VRMC_springBone_limit` (spec v3 §27, EX-11): reading, writing, the
//! fallbacks for readers without the extensions, clamping, and files that are wrong in each way the schemas allow.

// Tests index fixtures they just built, and the helpers take JSON by value so call sites stay literal.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

mod common;

use std::f32::consts::{FRAC_PI_2, PI};

use common::{avatar, export, extended_springs, glb, import, parts};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_io_api::{ColliderShape, JointLimit, Report, SpringBones};
use omuzaki_io_vrm::{Vrm1Exporter, Vrm1Importer};
use serde_json::{Value, json};

const COLLIDER_EXT: &str = "VRMC_springBone_extended_collider";
const LIMIT_EXT: &str = "VRMC_springBone_limit";

fn springs(scene: &omuzaki_io_api::IoScene) -> &SpringBones {
    &scene.avatar.as_ref().unwrap().spring_bones
}

/// A file from the plain fixture, edited by `edit`, then imported.
fn import_edited(edit: impl FnOnce(&mut Value)) -> omuzaki_io_api::ImportOutcome {
    let (scene, _) = avatar();
    let (mut doc, bin) = parts(&export(&Vrm1Exporter, &scene));
    edit(&mut doc);
    import(&Vrm1Importer, &glb(&doc, &bin))
}

fn set_collider_extension(doc: &mut Value, index: usize, body: Value) {
    doc["extensions"]["VRMC_springBone"]["colliders"][index]["extensions"] = json!({ COLLIDER_EXT: body });
}

fn set_limit_extension(doc: &mut Value, joint: usize, body: Value) {
    doc["extensions"]["VRMC_springBone"]["springs"][0]["joints"][joint]["extensions"] = json!({ LIMIT_EXT: body });
}

fn has(report: &Report, rule: &str) -> bool {
    report.findings.iter().any(|f| f.rule == rule)
}

/// The example in `VRMC_springBone_extended_collider-1.0/README.md`: an inside sphere whose base shape is the
/// radius-0 fallback.
#[test]
fn reads_the_spec_example() {
    let out = import_edited(|doc| {
        doc["extensions"]["VRMC_springBone"]["colliders"][0]["shape"] =
            json!({"sphere": {"radius": 0.0, "offset": [0.0, -10000.0, 0.0]}});
        set_collider_extension(
            doc,
            0,
            json!({"specVersion": "1.0", "shape": {"sphere": {"radius": 0.5, "offset": [0.0, 0.0, 0.0], "inside": true}}}),
        );
    });
    assert_eq!(springs(&out.scene).colliders[0].shape, ColliderShape::SphereInside { offset: Vec3::ZERO, radius: 0.5 });
    // The reader's own findings are at `/extensions/...`; the contract's export profile reports at `avatar/...`.
    let reader_warnings =
        out.report.findings.iter().filter(|f| f.location.as_deref().is_some_and(|l| l.starts_with('/')));
    assert_eq!(reader_warnings.filter(|f| f.rule == "vrm.collider_dropped").count(), 0, "{:#?}", out.report);
}

#[test]
fn round_trip() {
    let (scene, _) = extended_springs();
    let file = export(&Vrm1Exporter, &scene);
    assert_eq!(file, export(&Vrm1Exporter, &scene), "writing twice is byte-identical");

    let back = import(&Vrm1Importer, &file);
    assert_eq!(springs(&back.scene), springs(&scene), "every extended shape and limit survives");
    assert!(!has(&back.report, "gltf.extension_dropped"), "{:#?}", back.report);
    assert!(!back.scene.check().has_errors());
    // Reading what was written and writing it again changes nothing.
    assert_eq!(export(&Vrm1Exporter, &back.scene), file);
}

#[test]
fn the_extensions_are_listed_as_used_exactly_when_used() {
    let used = |scene: &omuzaki_io_api::IoScene| {
        let (doc, _) = parts(&export(&Vrm1Exporter, scene));
        let names: Vec<String> =
            doc["extensionsUsed"].as_array().unwrap().iter().map(|n| n.as_str().unwrap().to_owned()).collect();
        (names.iter().any(|n| n == COLLIDER_EXT), names.iter().any(|n| n == LIMIT_EXT))
    };
    assert_eq!(used(&avatar().0), (false, false));
    assert_eq!(used(&extended_springs().0), (true, true));
    let (mut only_limit, _) = avatar();
    only_limit.avatar.as_mut().unwrap().spring_bones.chains[0].joints[0].limit =
        Some(JointLimit::Cone { angle: 1.0, rotation: Quat::IDENTITY });
    assert_eq!(used(&only_limit), (false, true));
}

#[test]
fn fallback_values() {
    // The fixture's plane passes through (0, 0.5, 0) with normal +Y.
    let (scene, _) = extended_springs();
    let (doc, _) = parts(&export(&Vrm1Exporter, &scene));
    let colliders = &doc["extensions"]["VRMC_springBone"]["colliders"];
    // Inside sphere and capsule: the inert sphere far below the model.
    for i in [3, 4] {
        assert_eq!(colliders[i]["shape"], json!({"sphere": {"offset": [0.0, -10000.0, 0.0], "radius": 0.0}}));
    }
    // The plane: a sphere of radius 1000 whose surface passes through the plane.
    assert_eq!(colliders[5]["shape"], json!({"sphere": {"offset": [0.0, -999.5, 0.0], "radius": 1000.0}}));
    assert_eq!(colliders[3]["extensions"][COLLIDER_EXT]["shape"]["sphere"]["inside"], json!(true));
    assert!(colliders[0].get("extensions").is_none(), "an ordinary sphere has no extension");
    assert!(colliders[1].get("extensions").is_none(), "an ordinary capsule has no extension");
}

#[test]
fn clamps() {
    let out = import_edited(|doc| {
        set_limit_extension(doc, 0, json!({"specVersion": "1.0", "limit": {"cone": {"angle": 4.0}}}));
        set_limit_extension(doc, 1, json!({"specVersion": "1.0", "limit": {"spherical": {"pitch": 4.0, "yaw": 2.0}}}));
    });
    let joints = &springs(&out.scene).chains[0].joints;
    assert_eq!(joints[0].limit, Some(JointLimit::Cone { angle: PI, rotation: Quat::IDENTITY }));
    assert_eq!(joints[1].limit, Some(JointLimit::Spherical { pitch: PI, yaw: FRAC_PI_2, rotation: Quat::IDENTITY }));
    assert_eq!(out.report.findings.iter().filter(|f| f.rule == "vrm.limit_clamped").count(), 2);
}

#[test]
fn an_angle_inside_the_range_is_not_clamped() {
    let out = import_edited(|doc| {
        set_limit_extension(doc, 0, json!({"specVersion": "1.0", "limit": {"hinge": {"angle": 3.0}}}));
    });
    assert_eq!(
        springs(&out.scene).chains[0].joints[0].limit,
        Some(JointLimit::Hinge { angle: 3.0, rotation: Quat::IDENTITY })
    );
    assert!(!has(&out.report, "vrm.limit_clamped"));
}

#[test]
fn limit_on_tail_is_not_written_and_is_ignored_on_read() {
    // Written: a limit on the last joint of a chain is left out.
    let (mut scene, _) = avatar();
    let chain = &mut scene.avatar.as_mut().unwrap().spring_bones.chains[0];
    let limit = Some(JointLimit::Cone { angle: 0.5, rotation: Quat::IDENTITY });
    chain.joints[0].limit = limit;
    chain.joints[2].limit = limit;
    let (doc, _) = parts(&export(&Vrm1Exporter, &scene));
    let joints = &doc["extensions"]["VRMC_springBone"]["springs"][0]["joints"];
    assert!(joints[0]["extensions"][LIMIT_EXT].is_object());
    assert!(joints[2].get("extensions").is_none(), "no limit on the tail");

    // Read: a file that has one anyway is read without it, and the report says so.
    let out = import_edited(|doc| {
        let body = json!({"specVersion": "1.0", "limit": {"cone": {"angle": 0.5}}});
        set_limit_extension(doc, 0, body.clone());
        set_limit_extension(doc, 2, body);
    });
    let joints = &springs(&out.scene).chains[0].joints;
    assert!(joints[0].limit.is_some());
    assert_eq!(joints[2].limit, None);
    assert!(has(&out.report, "vrm.limit_on_tail_ignored"), "{:#?}", out.report);
}

#[test]
fn a_plane_normal_is_normalised_and_reported_when_off() {
    let plane = |normal: Value| {
        import_edited(|doc| {
            set_collider_extension(doc, 0, json!({"specVersion": "1.0", "shape": {"plane": {"normal": normal}}}));
        })
    };
    let out = plane(json!([0.0, 2.0, 0.0]));
    assert_eq!(springs(&out.scene).colliders[0].shape, ColliderShape::Plane { offset: Vec3::ZERO, normal: Vec3::Y });
    assert!(has(&out.report, "vrm.collider_normal_normalised"), "{:#?}", out.report);

    let out = plane(json!([0.0, 1.0005, 0.0]));
    assert!(!has(&out.report, "vrm.collider_normal_normalised"), "within the tolerance: {:#?}", out.report);

    let out = plane(json!([0.0, 0.0, 0.0]));
    assert!(matches!(springs(&out.scene).colliders[0].shape, ColliderShape::Sphere { .. }), "the base shape stays");
    assert!(has(&out.report, "vrm.collider_dropped"), "{:#?}", out.report);
}

#[test]
fn the_plane_normal_defaults_to_plus_z() {
    let out = import_edited(|doc| {
        set_collider_extension(doc, 0, json!({"specVersion": "1.0", "shape": {"plane": {}}}));
    });
    assert_eq!(springs(&out.scene).colliders[0].shape, ColliderShape::Plane { offset: Vec3::ZERO, normal: Vec3::Z });
}

#[test]
fn a_unit_quaternion_off_by_a_little_is_normalised_and_one_far_off_drops_the_limit() {
    let out = import_edited(|doc| {
        set_limit_extension(
            doc,
            0,
            json!({"specVersion": "1.0", "limit": {"cone": {"angle": 1.0, "rotation": [0.0, 0.0, 0.0, 1.0005]}}}),
        );
        set_limit_extension(
            doc,
            1,
            json!({"specVersion": "1.0", "limit": {"cone": {"angle": 1.0, "rotation": [0.0, 0.0, 0.0, 2.0]}}}),
        );
    });
    let joints = &springs(&out.scene).chains[0].joints;
    assert_eq!(joints[0].limit, Some(JointLimit::Cone { angle: 1.0, rotation: Quat::IDENTITY }));
    assert_eq!(joints[1].limit, None);
    assert!(has(&out.report, "vrm.limit_invalid"), "{:#?}", out.report);
}

/// Each of these must import (with a finding, never a panic) and leave the base shape or the plain joint.
#[test]
fn malformed_collider_extensions_fall_back_to_the_base_shape() {
    let bodies = [
        json!({"specVersion": "1.0", "shape": {"sphere": {"radius": 1.0}, "capsule": {"radius": 1.0}}}),
        json!({"specVersion": "1.0", "shape": {"sphere": {"radius": 1.0}, "plane": {}}}),
        json!({"specVersion": "1.0", "shape": {}}),
        json!({"specVersion": "1.0", "shape": {"sphere": {"radius": null}}}),
        json!({"specVersion": "1.0", "shape": {"sphere": {"radius": 1e39}}}),
        json!({"specVersion": "1.0", "shape": {"sphere": {"radius": -1.0}}}),
        json!({"specVersion": "1.0", "shape": {"sphere": {"offset": [0.0, 1.0]}}}),
        json!({"specVersion": "1.0", "shape": {"plane": {"normal": [1e39, 0.0, 0.0]}}}),
        json!({"specVersion": "1.0", "shape": 7}),
        json!("not an object"),
    ];
    for body in bodies {
        let out = import_edited(|doc| set_collider_extension(doc, 0, body.clone()));
        let base = ColliderShape::Sphere { offset: Vec3::new(0.0, 0.05, 0.0), radius: 0.09 };
        assert_eq!(springs(&out.scene).colliders[0].shape, base, "{body}");
        assert!(has(&out.report, "vrm.collider_dropped"), "{body}: {:#?}", out.report);
    }
}

#[test]
fn malformed_limit_extensions_leave_the_joint_without_a_limit() {
    let bodies = [
        json!({"specVersion": "1.0"}),
        json!({"specVersion": "1.0", "limit": {}}),
        json!({"specVersion": "1.0", "limit": {"cone": {"angle": 1.0}, "hinge": {"angle": 1.0}}}),
        json!({"specVersion": "1.0", "limit": {"cone": {"angle": 1.0, "rotation": [0.0, 0.0, 1.0]}}}),
        json!({"specVersion": "1.0", "limit": {"cone": {"angle": null}}}),
        json!({"specVersion": "1.0", "limit": {"cone": {"angle": 1e39}}}),
        json!({"specVersion": "1.0", "limit": {"cone": {"angle": -0.5}}}),
        json!({"specVersion": "1.0", "limit": {"cone": {}}}),
        json!({"specVersion": "1.0", "limit": {"spherical": {"pitch": 1.0}}}),
        json!({"specVersion": "1.0", "limit": {"hinge": {"angle": 1.0, "rotation": [0.0, 0.0, 0.0, 0.0]}}}),
        json!({"specVersion": "1.0", "limit": {"hinge": {"angle": 1.0, "rotation": [null, 0.0, 0.0, 1.0]}}}),
        json!([]),
    ];
    for body in bodies {
        let out = import_edited(|doc| set_limit_extension(doc, 0, body.clone()));
        assert_eq!(springs(&out.scene).chains[0].joints[0].limit, None, "{body}");
        assert!(has(&out.report, "vrm.limit_invalid"), "{body}: {:#?}", out.report);
    }
}
