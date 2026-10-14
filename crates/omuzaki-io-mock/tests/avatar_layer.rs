//! The VRM avatar layer through the mock: look-at, first-person and spring bones are written and read back exactly,
//! the file uses the 1.0 key names, the reader refuses what it cannot represent and names the field at fault, and random
//! parameters survive a round trip.

// Integration-test helpers may panic: a panic is a clear test failure. The casts build small fixture indices.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_possible_truncation)]

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    Avatar, Collider, ColliderGroup, ColliderShape, ConstraintKind, ExportCtx, ExportOutcome, FirstPerson,
    FirstPersonFlag, ImportCtx, ImportInput, ImportOptions, ImportOutcome, IoError, IoScene, LookAt, LookAtKind,
    MeshAnnotation, Node, NodeConstraint, NodeIx, RangeMap, SpringBones, SpringChain, SpringJoint, run_export,
    run_import,
};
use omuzaki_io_mock::fixtures::{BODY_NODE, glb_from_json, glb_parts, options_without_face_channels, tiny_avatar};
use omuzaki_io_mock::{MockVrm1Exporter, MockVrm1Importer};
use proptest::prelude::*;
use serde_json::{Value, json};

fn export_outcome(scene: &IoScene) -> ExportOutcome {
    run_export(&MockVrm1Exporter, scene, &ExportCtx::new(&options_without_face_channels())).expect("the scene exports")
}

fn export(scene: &IoScene) -> Vec<u8> {
    let mut out = export_outcome(scene);
    out.files.remove(0).bytes
}

fn import_result(bytes: &[u8]) -> Result<ImportOutcome, IoError> {
    let options = ImportOptions::default();
    run_import(&MockVrm1Importer, &ImportInput::new(bytes).named("avatar.vrm"), &ImportCtx::new(&options))
}

fn import(bytes: &[u8]) -> IoScene {
    import_result(bytes).expect("the file imports").scene
}

fn avatar_of(scene: &IoScene) -> &Avatar {
    scene.avatar.as_ref().expect("the scene has an avatar")
}

/// The index of the fixture node called `name`.
fn ix(scene: &IoScene, name: &str) -> NodeIx {
    let at = scene.nodes.iter().position(|n| n.name == name).expect("the fixture has this node");
    NodeIx(at as u32)
}

/// The fixture with the whole avatar layer filled in: two spring chains over the arms, one with a centre, three
/// colliders in two groups, a look-at block, and four first-person flags on four meshes. Every value is exact in binary,
/// so the JSON asserts below need no tolerance.
fn rich_scene() -> IoScene {
    let mut s = tiny_avatar();
    let mesh = s.nodes[BODY_NODE.index()].mesh.expect("the fixture's body has a mesh");
    for name in ["hair", "hat", "scarf"] {
        let mut node = Node::new(name);
        node.mesh = Some(mesh);
        let node = s.add_node(node);
        s.attach(NodeIx(0), node);
    }
    let (hair, hat, scarf) = (ix(&s, "hair"), ix(&s, "hat"), ix(&s, "scarf"));
    let (spine, head) = (ix(&s, "spine"), ix(&s, "head"));
    let (left_upper, left_lower, left_hand) = (ix(&s, "leftUpperArm"), ix(&s, "leftLowerArm"), ix(&s, "leftHand"));
    let (right_upper, right_lower) = (ix(&s, "rightUpperArm"), ix(&s, "rightLowerArm"));
    let joint = |node: NodeIx, stiffness: f32| SpringJoint {
        node,
        hit_radius: 0.015_625,
        stiffness,
        gravity_power: 0.5,
        gravity_dir: Vec3::new(0.0, -1.0, 0.25),
        drag_force: 0.25,
        limit: None,
    };
    let av = s.avatar.as_mut().expect("the fixture has an avatar");
    av.spring_bones = SpringBones {
        colliders: vec![
            Collider {
                node: head,
                shape: ColliderShape::Sphere { offset: Vec3::new(0.0, 0.125, 0.0625), radius: 0.125 },
            },
            Collider {
                node: spine,
                shape: ColliderShape::Capsule { offset: Vec3::ZERO, radius: 0.0625, tail: Vec3::new(0.0, 0.25, 0.0) },
            },
            Collider {
                node: left_hand,
                shape: ColliderShape::Sphere { offset: Vec3::new(0.0, -0.031_25, 0.0), radius: 0.046_875 },
            },
        ],
        collider_groups: vec![
            ColliderGroup { name: "head".into(), colliders: vec![0] },
            ColliderGroup { name: "torso".into(), colliders: vec![1, 2] },
        ],
        chains: vec![
            SpringChain {
                name: "left arm".into(),
                joints: vec![joint(left_upper, 0.75), joint(left_lower, 0.5), joint(left_hand, 0.25)],
                collider_groups: vec![0, 1],
                center: Some(spine),
            },
            SpringChain {
                name: "right arm".into(),
                joints: vec![joint(right_upper, 1.0), joint(right_lower, 1.0)],
                collider_groups: vec![],
                center: None,
            },
        ],
    };
    av.look_at = Some(LookAt {
        kind: LookAtKind::Bone,
        offset_from_head_bone: Vec3::new(0.0, 0.0625, 0.125),
        horizontal_inner: RangeMap { input_max_value: 29.0, output_scale: 10.0 },
        horizontal_outer: RangeMap { input_max_value: 45.0, output_scale: 10.0 },
        vertical_down: RangeMap { input_max_value: 10.0, output_scale: 10.0 },
        vertical_up: RangeMap { input_max_value: 15.0, output_scale: 10.0 },
    });
    av.first_person = Some(FirstPerson {
        annotations: vec![
            MeshAnnotation { node: BODY_NODE, flag: FirstPersonFlag::Auto },
            MeshAnnotation { node: hair, flag: FirstPersonFlag::Both },
            MeshAnnotation { node: hat, flag: FirstPersonFlag::ThirdPersonOnly },
            MeshAnnotation { node: scarf, flag: FirstPersonFlag::FirstPersonOnly },
        ],
    });
    s
}

/// The rich scene exported, edited as JSON by `edit`, and read back.
fn edited_import(edit: impl FnOnce(&mut Value)) -> Result<ImportOutcome, IoError> {
    let (mut doc, bin) = glb_parts(&export(&rich_scene())).expect("the file is a GLB");
    edit(&mut doc);
    import_result(&glb_from_json(&doc, &bin))
}

/// The `at` of a malformed-file refusal, panicking on anything else.
fn malformed_at(result: Result<ImportOutcome, IoError>) -> String {
    match result {
        Err(IoError::Malformed { at, .. }) => at.unwrap_or_default(),
        Err(other) => panic!("expected a malformed-file error, got {other:?}"),
        Ok(_) => panic!("expected a refusal, got a scene"),
    }
}

#[test]
fn spring_bones_look_at_and_first_person_round_trip_through_the_mock() {
    let scene = rich_scene();
    let outcome = export_outcome(&scene);
    assert!(!outcome.report.has_rule("vrm.mock_dropped"), "the mock writes the whole avatar layer now");
    let back = import(&outcome.files[0].bytes);
    let (got, want) = (avatar_of(&back), avatar_of(&scene));
    assert_eq!(got.spring_bones, want.spring_bones, "every collider, group, chain and joint parameter survives");
    assert_eq!(got.look_at, want.look_at);
    assert_eq!(got.first_person, want.first_person);
}

#[test]
fn look_at_round_trips_in_both_kinds_and_absent_blocks_stay_absent() {
    for kind in [LookAtKind::Bone, LookAtKind::Expression] {
        let mut scene = rich_scene();
        scene.avatar.as_mut().unwrap().look_at.as_mut().unwrap().kind = kind;
        let back = import(&export(&scene));
        assert_eq!(avatar_of(&back).look_at, avatar_of(&scene).look_at, "{kind:?}");
    }
    let back = import(&export(&tiny_avatar()));
    assert!(avatar_of(&back).look_at.is_none());
    assert!(avatar_of(&back).first_person.is_none());
}

#[test]
fn first_person_round_trips_every_flag_and_an_empty_block() {
    let mut scene = rich_scene();
    let back = import(&export(&scene));
    assert_eq!(avatar_of(&back).first_person, avatar_of(&scene).first_person);
    // An empty block is still data: the file has first-person settings, with no mesh annotated.
    scene.avatar.as_mut().unwrap().first_person = Some(FirstPerson::default());
    let back = import(&export(&scene));
    assert_eq!(avatar_of(&back).first_person, Some(FirstPerson::default()));
}

#[test]
fn the_file_uses_the_spec_key_names_and_the_defaults_are_written_out() {
    let scene = rich_scene();
    let (doc, _) = glb_parts(&export(&scene)).expect("the file is a GLB");
    assert_eq!(doc["extensionsUsed"], json!(["VRMC_springBone", "VRMC_vrm"]));

    let sb = &doc["extensions"]["VRMC_springBone"];
    assert_eq!(sb["specVersion"], "1.0");
    assert_eq!(sb["colliders"][1]["shape"]["capsule"]["tail"], json!([0.0, 0.25, 0.0]));
    assert_eq!(sb["colliders"][0]["shape"]["sphere"]["radius"], 0.125);
    assert_eq!(sb["colliderGroups"][1], json!({ "name": "torso", "colliders": [1, 2] }));
    let chain = &sb["springs"][0];
    assert_eq!(chain["center"], json!(ix(&scene, "spine").index()));
    assert_eq!(chain["colliderGroups"], json!([0, 1]));
    let joint = chain["joints"][0].as_object().expect("a joint object");
    let mut keys: Vec<&str> = joint.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["dragForce", "gravityDir", "gravityPower", "hitRadius", "node", "stiffness"]);
    assert!(sb["springs"][1].get("center").is_none(), "no centre, no key");
    assert!(sb["springs"][1].get("colliderGroups").is_none(), "no groups, no key");

    let vrm = &doc["extensions"]["VRMC_vrm"];
    assert_eq!(vrm["lookAt"]["type"], "bone");
    assert_eq!(vrm["lookAt"]["offsetFromHeadBone"], json!([0.0, 0.0625, 0.125]));
    assert_eq!(vrm["lookAt"]["rangeMapHorizontalInner"], json!({ "inputMaxValue": 29.0, "outputScale": 10.0 }));
    assert_eq!(
        vrm["firstPerson"]["meshAnnotations"][2],
        json!({ "node": ix(&scene, "hat").index(), "type": "thirdPersonOnly" })
    );
}

#[test]
fn an_avatar_without_springs_writes_no_spring_extension_and_constraints_are_still_reported() {
    let (doc, _) = glb_parts(&export(&tiny_avatar())).expect("the file is a GLB");
    assert_eq!(doc["extensionsUsed"], json!(["VRMC_vrm"]));
    assert!(doc["extensions"].get("VRMC_springBone").is_none());

    // A constraint has no mock form, so it is still dropped, and the export says so.
    let mut constrained = tiny_avatar();
    let (arm, source) = (ix(&constrained, "leftLowerArm"), ix(&constrained, "leftUpperArm"));
    let kind = ConstraintKind::Rotation { source };
    constrained.avatar.as_mut().unwrap().constraints.push(NodeConstraint { node: arm, kind, weight: 1.0 });
    assert!(export_outcome(&constrained).report.has_rule("vrm.mock_dropped"));
}

#[test]
fn extended_colliders_and_limits_are_written_as_the_inert_fallback_and_reported() {
    let mut scene = rich_scene();
    let node = ix(&scene, "head");
    let sb = &mut scene.avatar.as_mut().unwrap().spring_bones;
    let before = sb.colliders.len();
    sb.colliders.push(Collider { node, shape: ColliderShape::Plane { offset: Vec3::ZERO, normal: Vec3::Y } });
    sb.chains[0].joints[0].limit =
        Some(omuzaki_io_api::JointLimit::Cone { angle: 0.5, rotation: omuzaki_core::glam::Quat::IDENTITY });
    let out = export_outcome(&scene);
    let dropped = out.report.findings.iter().filter(|f| f.rule == "gltf.extension_dropped").count();
    assert_eq!(dropped, 2, "one for the collider, one for the limit: {:#?}", out.report);
    let (doc, _) = glb_parts(&out.files[0].bytes).expect("a GLB");
    let shape = &doc["extensions"]["VRMC_springBone"]["colliders"][before]["shape"];
    assert_eq!(*shape, json!({ "sphere": { "offset": [0.0, -10000.0, 0.0], "radius": 0.0 } }));
}

#[test]
fn a_joint_that_leaves_its_parameters_out_reads_back_as_the_spec_defaults() {
    let scene = rich_scene();
    let right_upper = ix(&scene, "rightUpperArm");
    let back = edited_import(|doc| {
        doc["extensions"]["VRMC_springBone"]["springs"][1]["joints"][0] = json!({ "node": right_upper.index() });
    })
    .expect("a file that omits parameters is valid");
    assert_eq!(avatar_of(&back.scene).spring_bones.chains[1].joints[0], SpringJoint::new(right_upper));
}

#[test]
fn the_reader_refuses_what_it_cannot_represent_and_names_the_field_at_fault() {
    // A collider with no sphere or capsule is one that only the extended collider describes.
    let at = malformed_at(edited_import(|doc| {
        doc["extensions"]["VRMC_springBone"]["colliders"][0].as_object_mut().unwrap().remove("shape");
    }));
    assert_eq!(at, "/extensions/VRMC_springBone/colliders/0");

    let at = malformed_at(edited_import(|doc| doc["extensions"]["VRMC_vrm"]["lookAt"]["type"] = json!("sideways")));
    assert_eq!(at, "/extensions/VRMC_vrm/lookAt/type");

    let at = malformed_at(edited_import(|doc| {
        doc["extensions"]["VRMC_vrm"]["firstPerson"]["meshAnnotations"][0]["type"] = json!("everyone");
    }));
    assert_eq!(at, "/extensions/VRMC_vrm/firstPerson/meshAnnotations/0/type");

    let refused = edited_import(|doc| doc["extensions"]["VRMC_springBone"]["specVersion"] = json!("2.0"));
    assert!(matches!(refused, Err(IoError::Unsupported { .. })));

    // An index that points nowhere is caught by the scene's structural checks, which run on every import.
    let refused =
        edited_import(|doc| doc["extensions"]["VRMC_springBone"]["colliderGroups"][0]["colliders"] = json!([99]));
    assert!(matches!(refused, Err(IoError::Rejected(_))));
}

#[test]
fn a_dropped_spring_extension_is_reported_and_the_plain_joint_is_kept() {
    let kept = edited_import(|doc| {
        doc["extensions"]["VRMC_springBone"]["springs"][0]["joints"][0]["extensions"] =
            json!({ "VRMC_springBone_limit": { "specVersion": "1.0" } });
    })
    .expect("a dropped extension is a finding, not an error");
    assert!(kept.report.has_rule("gltf.extension_dropped"));
    assert_eq!(avatar_of(&kept.scene).spring_bones, avatar_of(&rich_scene()).spring_bones);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Any finite parameters survive export and import exactly: the file stores what the scene holds.
    #[test]
    fn any_finite_spring_and_look_at_parameters_round_trip_exactly(
        radius in 0.0f32..1.0,
        tail_y in -2.0f32..2.0,
        offset in -1.0f32..1.0,
        hit_radius in 0.0f32..0.5,
        stiffness in 0.0f32..10.0,
        gravity in 0.0f32..5.0,
        gravity_y in -1.0f32..1.0,
        drag in 0.0f32..1.0,
        input_max in 0.0f32..180.0,
        output in 0.0f32..90.0,
    ) {
        let mut scene = tiny_avatar();
        let head = ix(&scene, "head");
        let av = scene.avatar.as_mut().expect("the fixture has an avatar");
        av.spring_bones = SpringBones {
            colliders: vec![Collider {
                node: head,
                shape: ColliderShape::Capsule { offset: Vec3::splat(offset), radius, tail: Vec3::new(0.0, tail_y, 0.0) },
            }],
            collider_groups: vec![ColliderGroup { name: "group".into(), colliders: vec![0] }],
            chains: vec![SpringChain {
                name: "chain".into(),
                joints: vec![SpringJoint {
                    node: head,
                    hit_radius,
                    stiffness,
                    gravity_power: gravity,
                    gravity_dir: Vec3::new(offset, gravity_y, 0.0),
                    drag_force: drag,
                    limit: None,
                }],
                collider_groups: vec![0],
                center: None,
            }],
        };
        av.look_at = Some(LookAt {
            kind: LookAtKind::Expression,
            offset_from_head_bone: Vec3::new(offset, output, -offset),
            horizontal_inner: RangeMap { input_max_value: input_max, output_scale: output },
            horizontal_outer: RangeMap { input_max_value: output, output_scale: input_max },
            vertical_down: RangeMap { input_max_value: input_max, output_scale: 1.0 },
            vertical_up: RangeMap { input_max_value: 90.0, output_scale: output },
        });
        let back = import(&export(&scene));
        prop_assert_eq!(&avatar_of(&back).spring_bones, &avatar_of(&scene).spring_bones);
        prop_assert_eq!(avatar_of(&back).look_at, avatar_of(&scene).look_at);
    }
}

/// The mock's spring JSON for plain spheres, capsules and joints is what `main` wrote before the extended shapes.
#[test]
fn existing_spring_json_is_byte_identical_to_the_pre_extension_baseline() {
    let out = export_outcome(&rich_scene());
    let (doc, _) = glb_parts(&out.files[0].bytes).expect("a GLB");
    let baseline = include_str!("baseline/spring_mock.json").trim_end();
    assert_eq!(doc["extensions"]["VRMC_springBone"].to_string(), baseline);
}
