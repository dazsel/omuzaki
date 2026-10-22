//! The EX-11 end-to-end physics check on an imported avatar (#1334; `docs/design/byom.md` D8 steps 1 to 3): a VRM 1.0
//! with spring chains, an inside sphere, a plane, a cone and a hinge limit, and roll, aim and rotation constraints is
//! imported as an avatar, performs a scripted head shake for 120 frames on the stage, and is exported and read back.
//! The file is the kit's own mock character written as a VRM, with the physics added to it here; no third-party file.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "avatar/kit.rs"]
mod kit;

use omuzaki_anim_api::{BoneId, HumanoidBone};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::body::BodyView;
use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_io_api::{
    AimAxis, Collider, ColliderGroup, ColliderShape, ConstraintKind, ExportCtx, ExportOptions, HumanBoneName,
    ImportCtx, ImportInput, ImportOptions, IoScene, JointLimit, Node, NodeConstraint, NodeIx, PerfectSync, RollAxis,
    SpringBones, SpringChain, SpringJoint, run_export, run_import,
};
use omuzaki_io_ops::gather::avatar_import::gather_imported;
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_io_ops::report_sections::{Section, section};
use omuzaki_io_vrm::{Vrm1Exporter, Vrm1Importer};
use omuzaki_render_api::{AlphaEncoding, Camera, MtoonSupport, Projection, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::avatar::AvatarStage;
use omuzaki_stage::frame::{FrameInput, StageClock};
use omuzaki_store_api::ContentHash;

/// The inside sphere's radius and the joints' radius: the sphere pushes a joint's centre back to their difference.
const INSIDE_RADIUS: f32 = 0.055;
const HIT_RADIUS: f32 = 0.01;
/// The inside sphere's centre, in the head node's frame: 1 cm under the chain's root.
const INSIDE_CENTRE: Vec3 = Vec3::new(0.0, 0.24, 0.0);
/// The chain's root, in the head node's frame, and the length of each of its two bones.
const BOB_ROOT: Vec3 = Vec3::new(0.0, 0.25, 0.0);
const BOB_BONE: f32 = 0.02;
/// The frames of the head shake.
const FRAMES: usize = 120;

/// The file as the importer reads it, and its head node.
struct Fixture {
    scene: IoScene,
    head: NodeIx,
}

/// Adds a node called `name` under `parent`, `at` in its frame.
fn child(scene: &mut IoScene, parent: NodeIx, name: &str, at: Vec3) -> NodeIx {
    let node = scene.add_node(Node::new(name).at(at));
    scene.nodes[parent.index()].children.push(node);
    node
}

fn joint(node: NodeIx, gravity: Vec3, limit: Option<JointLimit>) -> SpringJoint {
    let mut joint = SpringJoint::new(node);
    joint.hit_radius = HIT_RADIUS;
    joint.stiffness = 0.1;
    joint.drag_force = 0.2;
    joint.gravity_power = gravity.length();
    joint.gravity_dir = gravity.normalize_or(Vec3::NEG_Y);
    joint.limit = limit;
    joint
}

/// The kit's mock character as a VRM 1.0 with the physics of this test added, as the importer reads it back.
fn fixture() -> Fixture {
    let mut scene = kit::as_vrm(&kit::kit());
    let bones = scene.avatar.as_ref().unwrap().humanoid.bones.clone();
    let head = bones[&HumanBoneName::new("head").unwrap()];
    let chest = bones[&HumanBoneName::new("chest").unwrap()];
    assert_eq!(scene.nodes[head.index()].transform.rotation, Quat::IDENTITY, "the head node has the world's axes");

    // A short chain standing on the head inside a sphere. At rest its tip is already in the sphere's margin (the
    // joints' radius), so the sphere pushes on it from the first frame, and the shake swings it round the wall.
    let bob0 = child(&mut scene, head, "bob_0", BOB_ROOT);
    let bob1 = child(&mut scene, bob0, "bob_1", Vec3::Y * BOB_BONE);
    let bob2 = child(&mut scene, bob1, "bob_2", Vec3::Y * BOB_BONE);
    // A flap beside the head, hinged so it swings only in the head's YZ plane, pushed out of it by its gravity.
    let flap0 = child(&mut scene, head, "flap_0", Vec3::new(0.15, 0.12, 0.0));
    let flap1 = child(&mut scene, flap0, "flap_1", Vec3::new(0.0, -0.05, 0.0));
    let flap2 = child(&mut scene, flap1, "flap_2", Vec3::new(0.0, -0.05, 0.0));
    // The constraints: a roll and a rotation copying the head, and an aim at a point the head carries round.
    let roll = child(&mut scene, head, "roll", Vec3::new(0.1, 0.05, 0.0));
    let copy = child(&mut scene, chest, "copy", Vec3::new(0.0, 0.05, 0.1));
    let aim = child(&mut scene, chest, "aim", Vec3::new(0.0, 0.15, 0.2));

    let springs = &mut scene.avatar.as_mut().unwrap().spring_bones;
    let base = u32::try_from(springs.colliders.len()).unwrap();
    springs.colliders.push(Collider {
        node: head,
        shape: ColliderShape::SphereInside { offset: INSIDE_CENTRE, radius: INSIDE_RADIUS },
    });
    springs
        .colliders
        .push(Collider { node: head, shape: ColliderShape::Plane { offset: Vec3::ZERO, normal: Vec3::Y } });
    let groups = u32::try_from(springs.collider_groups.len()).unwrap();
    springs.collider_groups.push(ColliderGroup { name: "inside".into(), colliders: vec![base] });
    springs.collider_groups.push(ColliderGroup { name: "floor".into(), colliders: vec![base + 1] });
    let cone = JointLimit::Cone { angle: 1.2, rotation: Quat::IDENTITY };
    let hinge = JointLimit::Hinge { angle: 1.0, rotation: Quat::IDENTITY };
    let sideways = Vec3::X * 2.0;
    // The chain is drawn up and to one side, into the sphere's wall.
    let upwards = Vec3::new(1.0, 2.0, 0.0);
    springs.chains.push(SpringChain {
        name: "bob".into(),
        joints: vec![joint(bob0, upwards, Some(cone)), joint(bob1, upwards, None), joint(bob2, upwards, None)],
        collider_groups: vec![groups],
        center: None,
    });
    springs.chains.push(SpringChain {
        name: "flap".into(),
        joints: vec![
            joint(flap0, sideways, Some(hinge)),
            joint(flap1, sideways, Some(hinge)),
            joint(flap2, sideways, None),
        ],
        collider_groups: vec![groups + 1],
        center: None,
    });
    let constraints = &mut scene.avatar.as_mut().unwrap().constraints;
    constraints.push(NodeConstraint {
        node: roll,
        kind: ConstraintKind::Roll { source: head, axis: RollAxis::Y },
        weight: 1.0,
    });
    constraints.push(NodeConstraint { node: copy, kind: ConstraintKind::Rotation { source: head }, weight: 0.5 });
    constraints.push(NodeConstraint {
        node: aim,
        kind: ConstraintKind::Aim { source: roll, axis: AimAxis::PositiveZ },
        weight: 1.0,
    });
    // Through the real writer and reader, as a person's file would come.
    Fixture { scene: write_and_read(&scene), head }
}

/// `scene` written by the VRM 1.0 exporter and read back by the importer.
fn write_and_read(scene: &IoScene) -> IoScene {
    let mut scene = scene.clone();
    scene.provenance = Some(kit::provenance());
    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
    let outcome = run_export(&Vrm1Exporter, &scene, &ExportCtx::new(&options)).expect("writes");
    let bytes = outcome.files.into_iter().next().expect("the .vrm file").bytes;
    run_import(&Vrm1Importer, &ImportInput::new(&bytes), &ImportCtx::new(&ImportOptions::default()))
        .expect("reads")
        .scene
}

fn def() -> AvatarDef {
    AvatarDef::new(AvatarSource::new(ContentHash([3; 32]), SourceFormat::Vrm1, "physics.vrm"))
}

fn bone(avatar: &ImportedAvatar, name: &str) -> BoneId {
    avatar.skeleton.find(name).unwrap_or_else(|| panic!("no bone {name}"))
}

/// The limits on joints that use them (the last joint of a chain is only a tail; VRM ignores a limit there).
fn file_limits(springs: &SpringBones) -> usize {
    springs.chains.iter().map(|c| c.joints.iter().rev().skip(1).filter(|j| j.limit.is_some()).count()).sum()
}

#[test]
fn an_imported_avatars_physics_is_kept_simulated_and_written_back() {
    let Fixture { scene, head } = fixture();
    let file = &scene.avatar.as_ref().unwrap();

    // Step 1: import. Everything the file has is in the runtime sets, and nothing about physics is under Not used.
    let (avatar, report) = to_avatar(&scene, &def(), MtoonSupport::NONE).expect("imports as an avatar");
    let physics = |rule: &str| ["spring", "collider", "limit", "constraint"].iter().any(|w| rule.contains(w));
    let dropped: Vec<&str> = report
        .findings
        .iter()
        .filter(|f| section(&f.rule) == Section::NotUsed && physics(&f.rule))
        .map(|f| f.rule.as_str())
        .collect();
    assert!(dropped.is_empty(), "physics under Not used: {dropped:?}");
    let colliders: usize = avatar.springs.groups.iter().map(|g| g.colliders.len()).sum();
    let in_groups: usize = file.spring_bones.collider_groups.iter().map(|g| g.colliders.len()).sum();
    assert_eq!(colliders, in_groups, "every collider of every group");
    assert_eq!(avatar.springs.chains.len(), file.spring_bones.chains.len());
    let limits: usize =
        avatar.springs.chains.iter().map(|c| c.joints.iter().filter(|j| j.limit.is_some()).count()).sum();
    assert_eq!(limits, file_limits(&file.spring_bones));
    assert_eq!(avatar.constraints.constraints.len(), file.constraints.len());

    // Step 2: 120 frames of a head shake on the stage.
    let ids = |names: &[&str]| names.iter().map(|n| bone(&avatar, n)).collect::<Vec<_>>();
    let (bob, flap) = (ids(&["bob_1", "bob_2"]), ids(&["flap_0", "flap_1"]));
    let (head_bone, roll, aim) = (bone(&avatar, &bone_name(&scene, head)), bone(&avatar, "roll"), bone(&avatar, "aim"));
    let at = |stage: &AvatarStage, b: BoneId| stage.bone_world(b).unwrap().transform_point3(Vec3::ZERO);
    let (mut farthest, mut pressed, mut widest, mut off_aim) = (0.0_f32, 0, 0.0_f32, 0.0_f32);
    let mut tips = Vec::with_capacity(FRAMES);
    perform(&avatar, |stage| {
        let head_world = stage.bone_world(head_bone).unwrap();
        // The inside sphere. VRM's solver pushes a joint back to the wall less its radius and then restores the bone's
        // length, so a joint the sphere pushed ends a little past that line, by as much as the bone is longer than the
        // root's distance to it. The chain fits the sphere (root room plus bone lengths equals the radius), so the
        // spec keeps every joint inside the wall itself, and the joints spend frames in the margin it pushes on.
        let centre = head_world.transform_point3(INSIDE_CENTRE);
        for &b in &bob {
            let from_centre = at(stage, b).distance(centre);
            farthest = farthest.max(from_centre);
            pressed += usize::from(from_centre > INSIDE_RADIUS - HIT_RADIUS + 1e-4);
        }
        tips.push(at(stage, bob[1]));
        // The hinge: the flap's tail stays in the head's YZ plane, the limit's frame under its parent (the head).
        let (root, tail) = (at(stage, flap[0]), at(stage, flap[1]));
        let local = head_world.inverse().transform_vector3(tail - root).normalize();
        widest = widest.max(local.x.abs());
        // The aim: its +Z points at the roll node wherever the head carries it. The angle is read from its sine: an
        // arccosine near 1 can't resolve less than 3e-4 rad in f32.
        let towards = (at(stage, roll) - at(stage, aim)).normalize();
        let axis = stage.bone_world(aim).unwrap().transform_vector3(Vec3::Z).normalize();
        off_aim = off_aim.max(axis.cross(towards).length().asin());
        assert!(axis.dot(towards) > 0.0, "the aim points away from its source");
    });
    // The same shake with the sphere taken off the chain: the chain moves differently, so the sphere is what held it.
    let mut free = avatar.clone();
    free.springs.chains.iter_mut().filter(|c| c.name == "bob").for_each(|c| c.collider_groups.clear());
    let mut held = 0.0_f32;
    let mut frame = 0;
    perform(&free, |stage| {
        held = held.max(at(stage, bob[1]).distance(tips[frame]));
        frame += 1;
    });
    println!(
        "inside sphere: farthest joint {farthest:.6} m from the centre (wall {INSIDE_RADIUS}), {pressed} joint-frames \
         in the margin, tip up to {held:.4} m from where it goes without the sphere; hinge: worst {widest:.2e} off \
         plane; aim: worst {off_aim:.2e} rad"
    );
    assert!(farthest <= INSIDE_RADIUS + 1e-5, "a joint left the inside sphere: {farthest} m from its centre");
    assert!(pressed >= FRAMES / 4, "the sphere pushed on the chain in only {pressed} joint-frames");
    assert!(held > 1e-3, "the chain moves the same without the sphere ({held} m)");
    assert!(widest <= 1e-4, "the hinged flap left its plane by {widest}");
    assert!(off_aim <= 1e-4, "the aim missed its source by {off_aim} rad");

    // Step 3: export and read back. The physics equals the file's, floats to 1e-6, and the scene passes its checks.
    let mut out = gather_imported(&BodyView::imported(&avatar), &def(), &|_| None).expect("gathers");
    out.provenance = Some(kit::provenance());
    let again = write_and_read(&out);
    let problems = again.check();
    assert!(!problems.has_errors(), "{problems:?}");
    let back = again.avatar.as_ref().unwrap();
    same_springs(&back.spring_bones, &file.spring_bones);
    assert_eq!(back.constraints.len(), file.constraints.len());
    for (a, b) in back.constraints.iter().zip(&file.constraints) {
        assert_eq!((a.node, a.kind), (b.node, b.kind));
        assert!((a.weight - b.weight).abs() <= 1e-6);
    }
}

/// `avatar` on a stage on the mock renderer for [`FRAMES`] frames of a head shake, calling `each` after every frame.
fn perform(avatar: &ImportedAvatar, mut each: impl FnMut(&AvatarStage)) {
    let mut renderer = MockRenderer::new();
    let target = renderer
        .create_target(&TargetDesc { size: omuzaki_core::glam::UVec2::new(64, 64), alpha: AlphaEncoding::Straight })
        .unwrap();
    let camera = Camera::look_at(
        Vec3::new(0.0, 0.6, 2.4),
        Vec3::new(0.0, 0.6, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    );
    let mut stage = AvatarStage::new(
        &mut renderer,
        avatar.clone(),
        Scene::new(camera),
        target,
        Quality::live(),
        StageClock::fixed(60.0),
    )
    .expect("stages");
    for frame in 0..FRAMES {
        let mut pose = stage.base_pose();
        #[allow(clippy::cast_precision_loss)] // a frame number
        let shake = Quat::from_rotation_y(0.6 * (frame as f32 * 0.35).sin());
        pose.set(HumanoidBone::Head, shake * pose.get(HumanoidBone::Head));
        stage.tick(&mut renderer, &FrameInput { pose, ..FrameInput::default() }).expect("a frame");
        each(&stage);
    }
    stage.release(&mut renderer).unwrap();
}

/// The bone name the importer gave node `node`.
fn bone_name(scene: &IoScene, node: NodeIx) -> String {
    scene.nodes[node.index()].name.to_lowercase()
}

/// A collider's shape as its kind and its numbers.
fn shape_numbers(shape: ColliderShape) -> (u8, Vec<f32>) {
    let v = |v: Vec3| v.to_array();
    match shape {
        ColliderShape::Sphere { offset, radius } => (0, [v(offset).as_slice(), &[radius]].concat()),
        ColliderShape::SphereInside { offset, radius } => (1, [v(offset).as_slice(), &[radius]].concat()),
        ColliderShape::Capsule { offset, radius, tail } => (2, [v(offset).as_slice(), &[radius], &v(tail)].concat()),
        ColliderShape::CapsuleInside { offset, radius, tail } => {
            (3, [v(offset).as_slice(), &[radius], &v(tail)].concat())
        }
        ColliderShape::Plane { offset, normal } => (4, [v(offset), v(normal)].concat()),
        other => panic!("a shape this test doesn't know: {other:?}"),
    }
}

/// A joint limit as its kind and its numbers, its rotation's sign made positive (`q` and `-q` are one rotation).
fn limit_numbers(limit: Option<JointLimit>) -> (u8, Vec<f32>) {
    let turn = |q: Quat| if q.w < 0.0 { (-q).to_array() } else { q.to_array() };
    match limit {
        None => (0, Vec::new()),
        Some(JointLimit::Cone { angle, rotation }) => (1, [&[angle], turn(rotation).as_slice()].concat()),
        Some(JointLimit::Hinge { angle, rotation }) => (2, [&[angle], turn(rotation).as_slice()].concat()),
        Some(JointLimit::Spherical { pitch, yaw, rotation }) => {
            (3, [&[pitch, yaw], turn(rotation).as_slice()].concat())
        }
        Some(other) => panic!("a limit this test doesn't know: {other:?}"),
    }
}

/// `got` equals `want` as numbers: the same kind, every float within 1e-6.
fn same_numbers(got: &(u8, Vec<f32>), want: &(u8, Vec<f32>), what: &str) {
    let close = got.1.len() == want.1.len() && got.1.iter().zip(&want.1).all(|(x, y)| (x - y).abs() <= 1e-6);
    assert!(got.0 == want.0 && close, "{what}: {got:?} vs {want:?}");
}

/// `got` equals `want`: the same nodes, groups and chains, every float within 1e-6.
fn same_springs(got: &SpringBones, want: &SpringBones) {
    assert_eq!(got.colliders.len(), want.colliders.len());
    for (mine, theirs) in got.colliders.iter().zip(&want.colliders) {
        assert_eq!(mine.node, theirs.node);
        same_numbers(&shape_numbers(mine.shape), &shape_numbers(theirs.shape), "collider");
    }
    assert_eq!(got.collider_groups, want.collider_groups);
    assert_eq!(got.chains.len(), want.chains.len());
    for (mine, theirs) in got.chains.iter().zip(&want.chains) {
        assert_eq!(
            (&mine.name, &mine.collider_groups, mine.center),
            (&theirs.name, &theirs.collider_groups, theirs.center)
        );
        assert_eq!(mine.joints.len(), theirs.joints.len(), "{}", mine.name);
        for (joint, other) in mine.joints.iter().zip(&theirs.joints) {
            assert_eq!(joint.node, other.node);
            let numbers = |j: &SpringJoint| {
                (
                    0,
                    [&[j.hit_radius, j.stiffness, j.drag_force, j.gravity_power], j.gravity_dir.to_array().as_slice()]
                        .concat(),
                )
            };
            same_numbers(&numbers(joint), &numbers(other), "joint");
            same_numbers(&limit_numbers(joint.limit), &limit_numbers(other.limit), "limit");
        }
    }
}
