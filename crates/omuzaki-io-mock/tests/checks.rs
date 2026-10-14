//! Rule-by-rule tests for the checks in `omuzaki-io-api`: start from a valid scene, break exactly one thing, and expect
//! exactly that rule. They live here, not in the API crate, because they need a full avatar fixture.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::glam::{Quat, Vec3, Vec4};
use omuzaki_io_api::{
    Collider, ColliderGroup, ColliderShape, ConstraintKind, ExportOptions, ExpressionName, ExtraAttribute, IoScene,
    JointLimit, MeshIx, MorphTarget, Node, NodeConstraint, NodeIx, Purpose, Report, RollAxis, Severity, SpringChain,
    SpringJoint, VrmVersion, preflight, vrm_profile,
};
use omuzaki_io_mock::fixtures::{BODY_NODE, authored_provenance, options_without_face_channels, tiny_avatar};
use omuzaki_store_api::{Commercial, Consent};

fn node_of(scene: &IoScene, bone: &str) -> usize {
    scene.avatar.as_ref().unwrap().humanoid.node(bone).unwrap().index()
}

/// Asserts the report has exactly one error and that it is `rule`.
fn only_error(report: &Report, rule: &str) {
    let errors: Vec<_> = report.errors().map(|f| f.rule.as_str()).collect();
    assert_eq!(errors, [rule], "{report:#?}");
}

#[test]
fn the_fixture_is_clean_under_every_check() {
    let scene = tiny_avatar();
    assert_eq!(scene.check().findings, []);
    assert_eq!(vrm_profile(&scene, VrmVersion::V1).findings, []);
    assert_eq!(preflight(&scene, &options_without_face_channels()).findings, []);
}

type Break = fn(&mut IoScene);

#[test]
fn each_structural_rule_fires_on_its_violation() {
    let cases: [(&str, Break); 26] = [
        ("scene.index_out_of_range", |s| s.nodes[0].children.push(NodeIx(999))),
        ("scene.index_out_of_range", |s| s.roots.push(NodeIx(999))),
        ("scene.index_out_of_range", |s| s.nodes[16].mesh = Some(MeshIx(9))),
        ("scene.index_out_of_range", |s| s.meshes[0].primitives[0].material = Some(omuzaki_io_api::MaterialIx(9))),
        ("scene.node_multiple_parents", |s| s.nodes[0].children.push(NodeIx(2))),
        ("scene.root_has_parent", |s| s.roots.push(NodeIx(1))),
        ("scene.non_finite", |s| s.meshes[0].primitives[0].mesh.positions[0] = Vec3::NAN),
        ("scene.non_finite", |s| s.nodes[1].transform.translation = Vec3::INFINITY),
        ("scene.rotation_not_unit", |s| s.nodes[1].transform.rotation = Quat::from_xyzw(0.0, 0.0, 0.0, 2.0)),
        ("scene.attribute_length_mismatch", |s| {
            s.meshes[0].primitives[0].mesh.normals.pop();
        }),
        ("scene.attribute_length_mismatch", |s| {
            s.meshes[0].primitives[0].morph_targets[0].positions.pop();
        }),
        ("scene.skin_attributes_incomplete", |s| s.meshes[0].primitives[0].mesh.weights.clear()),
        ("scene.attribute_length_mismatch", |s| {
            s.meshes[0].primitives[0].mesh.uvs.pop();
        }),
        ("scene.indices_invalid", |s| s.meshes[0].primitives[0].mesh.indices.clear()),
        ("scene.mesh_invalid", |s| {
            // Only the shared mesh type's own validation knows extra attributes: five components is out of range.
            let name = omuzaki_core::id::Slug::new("strand").unwrap();
            s.meshes[0].primitives[0].mesh.extra.push(ExtraAttribute { name, components: 5, data: vec![0.0; 15] });
        }),
        ("scene.weights_not_normalised", |s| s.meshes[0].primitives[0].mesh.weights[0] = [0.5, 0.0, 0.0, 0.0]),
        ("scene.indices_invalid", |s| s.meshes[0].primitives[0].mesh.indices.push(0)),
        ("scene.indices_invalid", |s| s.meshes[0].primitives[0].mesh.indices[2] = 7),
        ("scene.morph_target_mismatch", |s| {
            let mut second = s.meshes[0].primitives[0].clone();
            second.morph_targets.push(MorphTarget::new(vec![Vec3::ZERO; 3]));
            s.meshes[0].primitives.push(second);
        }),
        ("scene.morph_target_mismatch", |s| s.meshes[0].morph_target_names.push("extra".into())),
        ("scene.skin_matrices", |s| {
            s.skins[0].inverse_bind.pop();
        }),
        ("scene.joint_out_of_range", |s| s.meshes[0].primitives[0].mesh.joints[0] = [99, 0, 0, 0]),
        ("avatar.humanoid_duplicate_node", |s| {
            let hips = s.avatar.as_ref().unwrap().humanoid.node("hips").unwrap();
            s.avatar.as_mut().unwrap().humanoid.assign("leftHand", hips).unwrap();
        }),
        ("avatar.bind_weight_range", |s| {
            let e = s.avatar.as_mut().unwrap().expressions.get_mut(&ExpressionName::parse("happy").unwrap()).unwrap();
            e.morph_binds[0].weight = 50.0; // a 0.x-style 0..100 weight that was never divided
        }),
        ("avatar.morph_bind_invalid", |s| {
            let e = s.avatar.as_mut().unwrap().expressions.get_mut(&ExpressionName::parse("happy").unwrap()).unwrap();
            e.morph_binds[0].index = 3;
        }),
        ("avatar.morph_bind_invalid", |s| {
            let e = s.avatar.as_mut().unwrap().expressions.get_mut(&ExpressionName::parse("happy").unwrap()).unwrap();
            e.morph_binds[0].node = NodeIx(2); // the spine has no mesh
        }),
    ];
    for (rule, mutate) in cases {
        let mut scene = tiny_avatar();
        mutate(&mut scene);
        let report = scene.check();
        assert!(report.has_rule(rule), "{rule} didn't fire: {report:#?}");
        assert!(report.errors().all(|f| f.rule.starts_with("scene.") || f.rule.starts_with("avatar.")), "{report:#?}");
    }
}

#[test]
fn a_node_cycle_is_found_even_when_nothing_reaches_it() {
    let mut s = tiny_avatar();
    let (a, b) = (s.add_node(Node::new("a")), s.add_node(Node::new("b")));
    s.attach(a, b);
    s.attach(b, a);
    only_error(&s.check(), "scene.node_cycle");
}

#[test]
fn spring_bones_and_constraints_are_checked() {
    let mut s = tiny_avatar();
    let (up, low) = (
        NodeIx(u32::try_from(node_of(&s, "leftUpperArm")).unwrap()),
        NodeIx(u32::try_from(node_of(&s, "leftLowerArm")).unwrap()),
    );
    let chain = |joints: &[NodeIx]| SpringChain {
        joints: joints.iter().map(|&n| SpringJoint::new(n)).collect(),
        ..SpringChain::default()
    };
    let sb = &mut s.avatar.as_mut().unwrap().spring_bones;
    sb.chains = vec![chain(&[up, low])];
    assert_eq!(s.check().findings, [], "a descending chain is fine");

    let sb = &mut s.avatar.as_mut().unwrap().spring_bones;
    sb.chains = vec![chain(&[low, up])];
    only_error(&s.check(), "avatar.spring_chain_order");

    let sb = &mut s.avatar.as_mut().unwrap().spring_bones;
    sb.chains = vec![chain(&[up, low]), chain(&[low])];
    only_error(&s.check(), "avatar.spring_joint_shared");

    let sb = &mut s.avatar.as_mut().unwrap().spring_bones;
    sb.chains = vec![SpringChain { collider_groups: vec![0], ..chain(&[up]) }];
    only_error(&s.check(), "scene.index_out_of_range");

    let sb = &mut s.avatar.as_mut().unwrap().spring_bones;
    sb.collider_groups = vec![ColliderGroup { name: "g".into(), colliders: vec![0] }];
    sb.chains = vec![];
    only_error(&s.check(), "scene.index_out_of_range");

    let mut s = tiny_avatar();
    let constraint =
        |source| NodeConstraint { node: up, kind: ConstraintKind::Roll { source, axis: RollAxis::X }, weight: 1.0 };
    s.avatar.as_mut().unwrap().constraints = vec![constraint(up)];
    only_error(&s.check(), "avatar.constraint_invalid");
    s.avatar.as_mut().unwrap().constraints = vec![constraint(low)];
    assert_eq!(s.check().findings, []);
}

/// A valid scene with all five collider shapes and a valid limit on a non-tail joint.
fn scene_with_extended_springs() -> IoScene {
    let mut s = tiny_avatar();
    let (up, low) = (
        NodeIx(u32::try_from(node_of(&s, "leftUpperArm")).unwrap()),
        NodeIx(u32::try_from(node_of(&s, "leftLowerArm")).unwrap()),
    );
    let (offset, tail) = (Vec3::new(0.0, 0.1, 0.0), Vec3::new(0.0, 0.3, 0.0));
    let shapes = [
        ColliderShape::Sphere { offset, radius: 0.1 },
        ColliderShape::Capsule { offset, radius: 0.1, tail },
        ColliderShape::SphereInside { offset, radius: 0.5 },
        ColliderShape::CapsuleInside { offset, radius: 0.5, tail },
        ColliderShape::Plane { offset, normal: Vec3::Y },
    ];
    let first = SpringJoint {
        limit: Some(JointLimit::Spherical { pitch: 0.5, yaw: 0.25, rotation: Quat::IDENTITY }),
        ..SpringJoint::new(up)
    };
    let sb = &mut s.avatar.as_mut().unwrap().spring_bones;
    sb.colliders = shapes.iter().map(|&shape| Collider { node: up, shape }).collect();
    sb.chains = vec![SpringChain { joints: vec![first, SpringJoint::new(low)], ..SpringChain::default() }];
    s
}

#[test]
fn extended_colliders_and_limits_are_clean_when_valid() {
    assert_eq!(scene_with_extended_springs().check().findings, []);
}

#[test]
fn extended_spring_rules_fire_on_one_line_mutations() {
    fn broken(edit: impl FnOnce(&mut IoScene)) -> Report {
        let mut s = scene_with_extended_springs();
        edit(&mut s);
        s.check()
    }
    fn shape(s: &mut IoScene, i: usize, shape: ColliderShape) {
        s.avatar.as_mut().unwrap().spring_bones.colliders[i].shape = shape;
    }
    fn limit(s: &mut IoScene, limit: JointLimit) {
        s.avatar.as_mut().unwrap().spring_bones.chains[0].joints[0].limit = Some(limit);
    }
    let at = Vec3::ZERO;

    only_error(&broken(|s| shape(s, 0, ColliderShape::Sphere { offset: at, radius: -0.1 })), "avatar.collider_invalid");
    only_error(
        &broken(|s| {
            shape(s, 1, ColliderShape::Capsule { offset: at, radius: 0.1, tail: Vec3::new(f32::NAN, 0.0, 0.0) });
        }),
        "avatar.collider_invalid",
    );
    only_error(
        &broken(|s| shape(s, 3, ColliderShape::CapsuleInside { offset: at, radius: f32::INFINITY, tail: at })),
        "avatar.collider_invalid",
    );
    only_error(
        &broken(|s| shape(s, 4, ColliderShape::Plane { offset: at, normal: Vec3::new(0.0, 2.0, 0.0) })),
        "avatar.collider_invalid",
    );
    only_error(
        &broken(|s| limit(s, JointLimit::Cone { angle: -0.1, rotation: Quat::IDENTITY })),
        "avatar.limit_invalid",
    );
    only_error(
        &broken(|s| limit(s, JointLimit::Hinge { angle: f32::NAN, rotation: Quat::IDENTITY })),
        "avatar.limit_invalid",
    );
    only_error(
        &broken(|s| limit(s, JointLimit::Spherical { pitch: 0.1, yaw: -0.1, rotation: Quat::IDENTITY })),
        "avatar.limit_invalid",
    );
    only_error(
        &broken(|s| limit(s, JointLimit::Cone { angle: 0.1, rotation: Quat::from_xyzw(0.0, 0.0, 0.0, 2.0) })),
        "avatar.limit_invalid",
    );

    let tail = broken(|s| {
        let chain = &mut s.avatar.as_mut().unwrap().spring_bones.chains[0];
        chain.joints[1].limit = Some(JointLimit::Cone { angle: 0.1, rotation: Quat::IDENTITY });
    });
    assert_eq!(tail.errors().count(), 0, "{tail:#?}");
    assert!(tail.has_rule("avatar.limit_on_tail"), "{tail:#?}");
}

#[test]
fn vrm_profile_rules() {
    let v1 = |s: &IoScene| vrm_profile(s, VrmVersion::V1);
    let check = |rule: &str, edit: Break| {
        let mut s = tiny_avatar();
        edit(&mut s);
        let report = v1(&s);
        assert!(report.has_rule(rule), "{rule} didn't fire: {report:#?}");
    };
    check("vrm.humanoid_required_bone", |s| {
        s.avatar.as_mut().unwrap().humanoid.bones.retain(|b, _| b.as_str() != "leftFoot");
    });
    check("vrm.humanoid_missing", |s| s.avatar = None);
    check("vrm.humanoid_scale", |s| {
        let hips = node_of(s, "hips");
        s.nodes[hips].transform.scale = Vec3::new(1.0, 0.0, 1.0);
    });
    check("vrm.humanoid_parent", |s| {
        // Move the left hand under the head: the humanoid tree wants it under the left lower arm.
        let (hand, head, arm) = (node_of(s, "leftHand"), node_of(s, "head"), node_of(s, "leftLowerArm"));
        s.nodes[arm].children.retain(|c| c.index() != hand);
        s.nodes[head].children.push(NodeIx(u32::try_from(hand).unwrap()));
    });
    check("vrm.humanoid_missing_parent", |s| {
        // An upperChest without a chest is not allowed.
        let spine = NodeIx(u32::try_from(node_of(s, "spine")).unwrap());
        let extra = s.add_node(Node::new("upperChest"));
        s.attach(spine, extra);
        s.avatar.as_mut().unwrap().humanoid.assign("upperChest", extra).unwrap();
    });
    check("vrm.meta_name_missing", |s| s.meta.name = "  ".into());
    check("vrm.meta_authors_missing", |s| s.meta.authors = vec![String::new()]);
    check("vrm.meta_license_url_missing", |s| s.meta.license_url.clear());
    check("vrm.meta_license_url_unknown", |s| s.meta.license_url = "https://example.com/terms".into());
    let warning = v1(&{
        let mut s = tiny_avatar();
        s.meta.license_url = "https://example.com/terms".into();
        s
    });
    assert_eq!(warning.findings[0].severity, Severity::Warning, "an unknown licence page is a warning, not an error");
}

#[test]
fn vrm0_requires_chest_and_neck_and_unique_material_names() {
    let s = tiny_avatar();
    let report = vrm_profile(&s, VrmVersion::V0);
    let missing: Vec<_> = report
        .errors()
        .filter(|f| f.rule == "vrm.humanoid_required_bone")
        .map(|f| f.location.clone().unwrap())
        .collect();
    assert_eq!(missing, ["avatar/humanoid/chest", "avatar/humanoid/neck"]);
    assert_eq!(vrm_profile(&s, VrmVersion::V1).findings, [] as [omuzaki_io_api::Finding; 0]);

    let mut s = tiny_avatar();
    let mut twin = s.materials[0].clone();
    twin.name = s.materials[0].name.clone();
    s.add_material(twin);
    let e = s.avatar.as_mut().unwrap().expressions.get_mut(&ExpressionName::parse("happy").unwrap()).unwrap();
    e.color_binds.push(omuzaki_io_api::ColorBind {
        material: omuzaki_io_api::MaterialIx(0),
        kind: omuzaki_io_api::ColorBindKind::Color,
        target: Vec4::ONE,
    });
    assert!(vrm_profile(&s, VrmVersion::V0).has_rule("vrm0.material_name_duplicate"));
    assert!(!vrm_profile(&s, VrmVersion::V1).has_rule("vrm0.material_name_duplicate"), "1.0 binds by index");
}

#[test]
fn rights_rules_follow_the_purpose_and_the_provenance() {
    let publish = options_without_face_channels().with_purpose(Purpose::Publish);
    let rules =
        |s: &IoScene, o: &ExportOptions| -> Vec<String> { preflight(s, o).errors().map(|f| f.rule.clone()).collect() };

    // Authored and commercial: publishing is fine.
    assert_eq!(rules(&tiny_avatar(), &publish), Vec::<String>::new());

    let with = |edit: &dyn Fn(&mut omuzaki_store_api::Provenance)| {
        let mut s = tiny_avatar();
        edit(s.provenance.as_mut().unwrap());
        s
    };
    assert_eq!(
        rules(&with(&|p| p.commercial = Commercial::No), &publish),
        ["rights.gate_non_commercial", "rights.terms_exceed_provenance"]
    );
    assert_eq!(
        rules(&with(&|p| p.commercial = Commercial::Unknown), &publish),
        ["rights.gate_unknown", "rights.terms_exceed_provenance"]
    );
    assert_eq!(
        rules(&with(&|p| p.private_test = true), &publish),
        ["rights.gate_private_test", "rights.terms_exceed_provenance"]
    );
    assert_eq!(rules(&tiny_avatar(), &publish.clone().with_real_person()), ["rights.gate_no_consent"]);
    let consent = Consent { person: "owner".into(), scope: "likeness".into(), document: None };
    assert_eq!(
        rules(&with(&|p| p.consent = Some(consent.clone())), &publish.clone().with_real_person()),
        Vec::<String>::new()
    );

    // A private export skips the gate but still can't grant more than we hold.
    let private = options_without_face_channels();
    assert_eq!(rules(&with(&|p| p.commercial = Commercial::No), &private), ["rights.terms_exceed_provenance"]);
    let mut restrictive = with(&|p| p.commercial = Commercial::No);
    restrictive.meta.terms.commercial_usage = omuzaki_io_api::CommercialUsage::PersonalNonProfit;
    assert_eq!(rules(&restrictive, &private), Vec::<String>::new());

    // Private test material may be exported privately, only with the most restrictive terms, and says so.
    let mut test_material = with(&|p| p.private_test = true);
    test_material.meta.terms = omuzaki_io_api::UseTerms::default();
    let report = preflight(&test_material, &private);
    assert_eq!(report.errors().count(), 0);
    assert!(report.has_rule("rights.private_test_export"));

    let mut missing = tiny_avatar();
    missing.provenance = None;
    only_error(&preflight(&missing, &private), "rights.provenance_missing");
    let _ = authored_provenance();
    let _ = BODY_NODE;
}
