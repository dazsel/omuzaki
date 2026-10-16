//! `to_avatar`'s expressions, drivers, gaze routes and physics (#1217): every bind reaches the runtime, D3's driver
//! table and the person's overrides, one avatar per gaze route, and the file's springs, colliders, limits and
//! constraints in the runtime sets. Every scene is generated here.

mod common;

use common::{add_skinned_mesh, def, humanoid, humanoid_without, through_vrm, toon_material};
use omuzaki_anim_api::{Channel, ChannelMask};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::def::DriverChoice;
use omuzaki_avatar_api::gaze::AvatarGaze;
use omuzaki_core::glam::{Quat, Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    AimAxis, Collider, ColliderGroup, ColliderShape, ColorBind, ColorBindKind, ConstraintKind, Expression,
    ExpressionName, JointLimit, LookAt, LookAtKind, MaterialIx, Node, NodeConstraint, RangeMap, Report, RollAxis,
    SpringChain, SpringJoint, UvBind,
};
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_render_api::MtoonSupport;

fn name(n: &str) -> ExpressionName {
    ExpressionName::parse(n).unwrap()
}

/// A humanoid (eyes when `eyes`) with a face mesh of `targets` targets on a toon material, and `expressions`, each
/// `(name, target)` moving its target of the face at full weight.
fn face(eyes: bool, targets: usize, expressions: &[(&str, u32)]) -> common::Fixture {
    let mut f = if eyes { humanoid(0.0) } else { humanoid_without(0.0, &["leftEye", "rightEye"]) };
    f.scene.add_material(toon_material("Face", Vec4::ONE));
    let face = add_skinned_mesh(&mut f, "Face", &["head", "neck"], targets, Some(0));
    let avatar = f.scene.avatar.as_mut().unwrap();
    for (n, target) in expressions {
        avatar.expressions.insert(name(n), Expression::morph(face, *target));
    }
    f
}

fn import(f: &common::Fixture) -> (ImportedAvatar, Report) {
    to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("imports")
}

/// The gaze channels that reach the model.
fn gaze(a: &ImportedAvatar) -> ChannelMask {
    ChannelMask::from_bits_truncate(a.driven().bits() & ChannelMask::region(omuzaki_anim_api::Region::Gaze).bits())
}

#[test]
fn every_bind_of_a_generated_vrm_reaches_its_expression() {
    let f = face(true, 3, &[("aa", 0), ("happy", 1)]);
    let scene = through_vrm(&f.scene);
    let (a, _) = import(&common::Fixture { scene, nodes: f.nodes.clone() });
    let happy = &a.expressions[a.expression(&name("happy")).unwrap().index()];
    assert_eq!(happy.morph.iter().map(|b| (b.target, b.weight)).collect::<Vec<_>>(), [(1, 1.0)]);

    // Colour and UV binds (the mock writer keeps morph binds only, so these are on the scene itself).
    let mut f = face(true, 1, &[]);
    let mut blush = Expression::new();
    blush.color_binds =
        vec![ColorBind { material: MaterialIx(0), kind: ColorBindKind::Color, target: Vec4::new(1.0, 0.5, 0.5, 1.0) }];
    blush.uv_binds = vec![UvBind { material: MaterialIx(0), scale: Vec2::new(0.5, 1.0), offset: Vec2::new(0.5, 0.0) }];
    blush.is_binary = true;
    blush.override_mouth = omuzaki_io_api::Override::Block;
    f.scene.avatar.as_mut().unwrap().expressions.insert(name("Blush"), blush);
    let (a, _) = import(&f);
    let e = &a.expressions[a.expression(&name("Blush")).unwrap().index()];
    assert_eq!(e.colour.len(), 1);
    assert_eq!((e.colour[0].base, e.colour[0].target), (Vec4::ONE, Vec4::new(1.0, 0.5, 0.5, 1.0)));
    assert_eq!(
        (e.uv[0].scale, e.uv[0].offset, e.uv[0].base_scale),
        (Vec2::new(0.5, 1.0), Vec2::new(0.5, 0.0), Vec2::ONE)
    );
    assert!(e.binary);
    assert_eq!(e.overrides.mouth, omuzaki_io_api::Override::Block);
}

#[test]
fn a_bind_naming_nothing_is_reported_and_left_out() {
    let mut f = face(true, 1, &[]);
    let face = f.node("Face");
    let mut e = Expression::morph(face, 9);
    e.color_binds = vec![ColorBind { material: MaterialIx(0), kind: ColorBindKind::Color, target: Vec4::ONE }];
    f.scene.avatar.as_mut().unwrap().expressions.insert(name("Broken"), e);
    let (a, report) = import(&f);
    assert!(report.has_rule("avatar.bind_dropped"), "{report:?}");
    let broken = &a.expressions[a.expression(&name("Broken")).unwrap().index()];
    assert!(broken.morph.is_empty() && broken.colour.len() == 1);
}

#[test]
fn emotion_presets_take_the_first_custom_slots_and_the_seventeenth_is_undriven() {
    let emotions = ["happy", "angry", "sad", "relaxed", "surprised", "neutral"];
    let customs: Vec<String> = (0..14).map(|i| format!("Extra{i:02}")).collect();
    let mut all: Vec<(&str, u32)> = emotions.iter().map(|n| (*n, 0)).collect();
    all.extend(customs.iter().map(|n| (n.as_str(), 0)));
    let (a, report) = import(&face(true, 1, &all));
    for (slot, emotion) in emotions.iter().enumerate() {
        let channel = Channel::custom(slot).unwrap();
        assert_eq!(a.drivers.get(channel), a.expression(&name(emotion)), "{emotion} on {channel}");
    }
    assert_eq!(report.findings.iter().filter(|f| f.rule == "avatar.expression_custom").count(), 16);
    let undriven: Vec<_> = report.findings.iter().filter(|f| f.rule == "avatar.expression_undriven").collect();
    assert_eq!(undriven.len(), 4, "20 expressions for 16 slots: {report:?}");
}

#[test]
fn arkit_named_expressions_drive_their_channels_and_overrides_win() {
    let (a, _) = import(&face(true, 3, &[("jawOpen", 0), ("Puff", 1), ("happy", 2)]));
    assert_eq!(a.drivers.get(Channel::JAW_OPEN), a.expression(&name("jawOpen")));
    assert_eq!(a.drivers.get(Channel::CUSTOM_00), a.expression(&name("happy")));

    let f = face(true, 3, &[("jawOpen", 0), ("Puff", 1), ("happy", 2)]);
    let mut d = def();
    d.channels.insert(Channel::CHEEK_PUFF, DriverChoice::Expression("Puff".into()));
    d.channels.insert(Channel::JAW_OPEN, DriverChoice::Nothing);
    d.channels.insert(Channel::MOUTH_ROLL_LOWER, DriverChoice::Expression("NoSuch".into()));
    let (a, report) = to_avatar(&f.scene, &d, MtoonSupport::NONE).unwrap();
    assert_eq!(a.drivers.get(Channel::CHEEK_PUFF), a.expression(&name("Puff")), "the person's choice");
    assert_eq!(a.drivers.get(Channel::JAW_OPEN), None, "mapped to nothing");
    assert!(report.has_rule("avatar.channel_override_invalid"), "an unknown expression is reported");
}

/// Every look direction of the bone route's channels.
const GAZE: [Channel; 8] = [
    Channel::EYE_LOOK_UP_LEFT,
    Channel::EYE_LOOK_UP_RIGHT,
    Channel::EYE_LOOK_DOWN_LEFT,
    Channel::EYE_LOOK_DOWN_RIGHT,
    Channel::EYE_LOOK_IN_LEFT,
    Channel::EYE_LOOK_IN_RIGHT,
    Channel::EYE_LOOK_OUT_LEFT,
    Channel::EYE_LOOK_OUT_RIGHT,
];

#[test]
fn one_avatar_per_gaze_route() {
    // Bone: eye bones and no look-at (absent means bone type).
    let (a, report) = import(&face(true, 1, &[]));
    assert_eq!(a.gaze, AvatarGaze::Bone);
    assert_eq!(gaze(&a), ChannelMask::from_channels(&GAZE));
    assert!(!report.has_rule("arkit.gaze_partial") && !report.has_rule("arkit.gaze_inert"), "{report:?}");

    // Native ARKit: no eye bones, the model's own gaze shape for one channel.
    let (a, report) = import(&face(false, 1, &[("eyeLookUpLeft", 0)]));
    assert_eq!(a.gaze, AvatarGaze::NativeArkit);
    assert_eq!(gaze(&a), ChannelMask::of(Channel::EYE_LOOK_UP_LEFT));
    assert_eq!(a.drivers.get(Channel::EYE_LOOK_UP_LEFT), a.expression(&name("eyeLookUpLeft")));
    let partial = report.findings.iter().find(|f| f.rule == "arkit.gaze_partial").expect("partial");
    assert!(partial.message.contains("down") && partial.message.contains("left"), "{}", partial.message);

    // Presets: no eye bones, only `lookUp`.
    let (a, report) = import(&face(false, 1, &[("lookUp", 0)]));
    assert_eq!(a.gaze, AvatarGaze::Presets);
    assert_eq!(gaze(&a), ChannelMask::from_channels(&[Channel::EYE_LOOK_UP_LEFT, Channel::EYE_LOOK_UP_RIGHT]));
    assert_eq!(a.drivers.get(Channel::EYE_LOOK_UP_LEFT), None, "the look-at stage drives the preset");
    assert!(report.has_rule("arkit.gaze_partial"));

    // Inert: nothing shows gaze.
    let (a, report) = import(&face(false, 1, &[]));
    assert_eq!(a.gaze, AvatarGaze::Inert);
    assert!(gaze(&a).is_empty());
    assert!(report.has_rule("arkit.gaze_inert"));

    // An expression-type look-at with eye bones goes past the bone route to the presets.
    let mut f = face(true, 1, &[("lookUp", 0)]);
    let map = RangeMap { input_max_value: 90.0, output_scale: 1.0 };
    let look = LookAt {
        kind: LookAtKind::Expression,
        offset_from_head_bone: Vec3::ZERO,
        horizontal_inner: map,
        horizontal_outer: map,
        vertical_down: map,
        vertical_up: map,
    };
    f.scene.avatar.as_mut().unwrap().look_at = Some(look);
    let (a, _) = import(&f);
    assert_eq!(a.gaze, AvatarGaze::Presets);
    assert_eq!(a.look_at.map(|l| l.kind), Some(LookAtKind::Expression));
}

/// A chain of three nodes under `parent`, called `<prefix>1..3`, each 5 cm below the last.
fn chain(f: &mut common::Fixture, parent: &str, prefix: &str) -> Vec<omuzaki_io_api::NodeIx> {
    let mut up = f.node(parent);
    (1..=3)
        .map(|i| {
            let n = f.scene.add_node(Node::new(format!("{prefix}{i}")).at(Vec3::new(0.0, -0.05, -0.05)));
            f.scene.attach(up, n);
            f.nodes.insert(format!("{prefix}{i}"), n);
            up = n;
            n
        })
        .collect()
}

#[test]
fn every_spring_collider_limit_and_constraint_reaches_the_runtime() {
    let mut f = face(true, 1, &[]);
    let hair = chain(&mut f, "head", "Hair");
    let tail = chain(&mut f, "hips", "Tail");
    let (head, hand, lower, upper) =
        (f.node("head"), f.node("leftHand"), f.node("leftLowerArm"), f.node("leftUpperArm"));
    let twist = f.scene.add_node(Node::new("Twist.L").at(Vec3::X * 0.1));
    f.scene.attach(lower, twist);
    let sleeve = f.scene.add_node(Node::new("Sleeve.L").at(Vec3::X * 0.05));
    f.scene.attach(upper, sleeve);
    let sub = f.scene.add_node(Node::new("Sub.L").at(Vec3::Y * 0.02));
    f.scene.attach(upper, sub);
    let springs = &mut f.scene.avatar.as_mut().unwrap().spring_bones;
    springs.colliders = vec![
        Collider { node: head, shape: ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.1 } },
        Collider {
            node: head,
            shape: ColliderShape::Capsule { offset: Vec3::ZERO, radius: 0.05, tail: Vec3::Y * 0.1 },
        },
        Collider { node: head, shape: ColliderShape::SphereInside { offset: Vec3::ZERO, radius: 0.5 } },
        Collider { node: head, shape: ColliderShape::CapsuleInside { offset: Vec3::ZERO, radius: 0.5, tail: Vec3::Y } },
        Collider { node: head, shape: ColliderShape::Plane { offset: Vec3::ZERO, normal: Vec3::Y } },
    ];
    // Collider 0 is in both groups; both groups and both chains are unnamed.
    springs.collider_groups = vec![
        ColliderGroup { name: String::new(), colliders: vec![0, 1, 2] },
        ColliderGroup { name: String::new(), colliders: vec![0, 3, 4] },
    ];
    let joint = |node, limit| SpringJoint { limit, ..SpringJoint::new(node) };
    let rotated = Quat::from_rotation_x(0.2);
    springs.chains = vec![
        SpringChain {
            name: String::new(),
            joints: vec![
                joint(hair[0], Some(JointLimit::Cone { angle: 0.5, rotation: rotated })),
                joint(hair[1], Some(JointLimit::Hinge { angle: 0.4, rotation: rotated })),
                joint(hair[2], None),
            ],
            collider_groups: vec![0, 1],
            center: Some(head),
        },
        SpringChain {
            name: String::new(),
            joints: vec![
                joint(tail[0], Some(JointLimit::Spherical { pitch: 0.3, yaw: 0.2, rotation: rotated })),
                joint(tail[1], None),
            ],
            collider_groups: vec![1],
            center: None,
        },
    ];
    f.scene.avatar.as_mut().unwrap().constraints = vec![
        NodeConstraint { node: twist, kind: ConstraintKind::Roll { source: hand, axis: RollAxis::X }, weight: 0.5 },
        NodeConstraint {
            node: sleeve,
            kind: ConstraintKind::Aim { source: hand, axis: AimAxis::PositiveX },
            weight: 1.0,
        },
        NodeConstraint { node: sub, kind: ConstraintKind::Rotation { source: upper }, weight: 0.8 },
    ];
    let (a, report) = import(&f);
    assert!(
        !report.has_rule("avatar.spring_chain_unused") && !report.has_rule("avatar.constraint_unused"),
        "{report:?}"
    );
    assert_eq!(a.springs.chains.len(), 2);
    assert_eq!(a.springs.groups.len(), 2);
    let colliders: usize = a.springs.groups.iter().map(|g| g.colliders.len()).sum();
    assert_eq!(colliders, 6, "every group membership, the shared collider in both");
    let limits = a.springs.chains.iter().flat_map(|c| &c.joints).filter(|j| j.limit.is_some()).count();
    assert_eq!(limits, 3);
    assert_eq!(a.constraints.constraints.len(), 3);
    let names: Vec<&str> = a.springs.chains.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["chain_0", "chain_1"], "unnamed chains get unique runtime names");
    let r = &a.retained.springs;
    assert_eq!((r.colliders.len(), r.groups.len(), r.chains.len()), (5, 2, 2), "the file's records, by index");
    assert!(r.groups.iter().all(|g| g.name.is_empty()), "the file's empty names are kept");
    assert_eq!(r.chains[0].runtime.as_deref(), Some("chain_0"));
}

#[test]
fn a_chain_the_solver_cant_run_is_kept_for_export_and_reported() {
    let mut f = face(true, 1, &[]);
    let hair = chain(&mut f, "head", "Hair");
    let springs = &mut f.scene.avatar.as_mut().unwrap().spring_bones;
    springs.chains = vec![SpringChain {
        name: "Solo".into(),
        joints: vec![SpringJoint::new(hair[0])],
        collider_groups: Vec::new(),
        center: None,
    }];
    let (a, report) = import(&f);
    assert!(report.has_rule("avatar.spring_chain_unused"), "{report:?}");
    assert!(a.springs.chains.is_empty());
    assert_eq!((a.retained.springs.chains.len(), a.retained.springs.chains[0].runtime.clone()), (1, None));
}

#[test]
fn two_runs_give_equal_expressions_and_reports() {
    let f = face(false, 2, &[("happy", 0), ("lookUp", 1)]);
    assert_eq!(import(&f), import(&f));
}
