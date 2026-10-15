//! Attachment and the rigid jewellery kinds: choker, bell, hairpin, halo and charm on the real body and head of
//! every template.
//!
//! Every character here is a real `body` and `head` plus one accessory. The tests check what the acceptance asks: the
//! attach resolution (rotation order, bone and anchor frames, errors that name the missing piece), closed shells on the
//! four templates, parts that move with their joint when it is posed, dangling parts that stay within 3 cm of rest at a
//! 25 degree swing, and the build time.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::chunks_exact_to_as_chunks)] // test arithmetic
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

use std::collections::HashMap;
use std::time::Instant;

use omuzaki_anim_api::{Collider, ColliderGroup, ColliderShape, HumanoidBone, Pose};
use omuzaki_char::accessory::attach::{self, AttachTarget, attach_points};
use omuzaki_char::accessory::rigid::{MAX_REACH, MAX_SWING_DEG};
use omuzaki_char_api::{
    Anchor, AnchorsFact, Assembler, Attach, AttachOffset, BuildCtx, BuildError, BuildOptions, BuildServices,
    BuiltCharacter, CharId, CharacterBuilder, CharacterDef, CollidersFact, FitFact, JointFit, JointRef, PartCategory,
    PartDef, PartId, PartKind, PartKindId, PartMesh, PartMeta, PartOutput, Published, Publishes, RegistryBuilder,
    build_part, find_kind, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// No parameters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Nothing {}

impl omuzaki_face_api::Validate for Nothing {
    fn check(&self, _: &mut omuzaki_face_api::Check) {}
}

/// Stands in for the head and body colliders, which no part publishes yet: a sphere on the head bone as big as the head,
/// and an `ear` anchor 4.5 cm outside the head at ear height, where a bell hangs.
struct HeadCollider;

impl PartKind for HeadCollider {
    type Params = Nothing;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-head-collider",
            summary: "a stand-in collider",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn build(_: &Nothing, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let joint = ctx.joint(&JointRef::HEAD).ok_or_else(|| ctx.fail("no head joint"))?;
        let sphere = ColliderShape::Sphere { offset: head.centre() - joint, radius: head.radius(Vec3::X) };
        let group = ColliderGroup {
            name: "head".to_owned(),
            colliders: vec![Collider { bone: omuzaki_char_api::humanoid_id(HumanoidBone::Head), shape: sphere }],
        };
        let ear = Anchor {
            bone: JointRef::HEAD,
            position: head.point(Vec3::new(1.0, 0.15, 0.0).normalize(), 0.045),
            rotation: Quat::IDENTITY,
        };
        Ok(PartOutput::new().with_publishes(
            Publishes::default().with::<CollidersFact>(group).with::<AnchorsFact>(("ear".parse().unwrap(), ear)),
        ))
    }
}

omuzaki_char_api::register_part_kind!(HeadCollider);

/// A bell hung from the `ear` anchor of the stand-in collider part, which is built before it.
fn ear_bell(template: &str, params: Value) -> CharacterDef {
    let attach =
        Attach::Anchor { part: pid("collider"), anchor: "ear".parse().unwrap(), offset: AttachOffset::default() };
    let mut def = character(template, "bell", params, Some(attach));
    def.parts.insert(pid("collider"), part("test-head-collider", 10, None, json!({})));
    def
}

const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

fn part(kind: &str, layer: i32, attach: Option<Attach>, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer, attach, params }
}

fn bone(joint: JointRef, translation: [f64; 3]) -> Attach {
    Attach::Bone { bone: joint, offset: AttachOffset { translation, ..AttachOffset::default() } }
}

fn on_head_top(translation: [f64; 3]) -> Attach {
    Attach::Anchor {
        part: pid("head"),
        anchor: "top".parse().unwrap(),
        offset: AttachOffset { translation, ..AttachOffset::default() },
    }
}

/// The head parameters that seat the head on `template`'s neck: the default cranium is sculpted for soft-sd, so on
/// another template its centre moves up with the neck by the same distance. Remove this when the bug that floats the head
/// on the other templates is fixed.
fn head_params(template: &str) -> Value {
    let body = omuzaki_char_api::BodyParams::default();
    let layout =
        omuzaki_char::template::registry::find(template).unwrap().layout(&body.proportions, &body.shape, &body.hand);
    let soft_sd =
        omuzaki_char::template::registry::find("soft-sd").unwrap().layout(&body.proportions, &body.shape, &body.hand);
    let rise = layout.head_base().unwrap().y - soft_sd.head_base().unwrap().y;
    let default = omuzaki_char_api::HeadParams::default().cranium.centre;
    json!({ "cranium": { "centre": [default[0], default[1] + rise, default[2]] } })
}

/// The real body and head of `template` with one accessory `acc`.
fn character(template: &str, kind: &str, params: Value, attach: Option<Attach>) -> CharacterDef {
    let mut def = mock_character().unwrap();
    def.template = template.parse().unwrap();
    def.parts.clear();
    def.parts.insert(pid("body"), part("body", 0, None, json!({})));
    def.parts.insert(pid("head"), part("head", 0, None, head_params(template)));
    def.parts.insert(pid("acc"), part(kind, 50, attach, params));
    def
}

fn build(def: &CharacterDef) -> Result<BuiltCharacter, BuildError> {
    RegistryBuilder.build(&CharId::new("t").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
}

fn built(template: &str, kind: &str, params: Value, attach: Option<Attach>) -> BuiltCharacter {
    build(&character(template, kind, params, attach)).unwrap_or_else(|e| panic!("{kind} on {template}: {e}"))
}

fn meshes(b: &BuiltCharacter) -> Vec<&PartMesh> {
    b.meshes.iter().filter(|m| m.part == pid("acc")).map(|m| &m.mesh).collect()
}

// ------------------------------------------------------------------------------------------------ closed shells

/// Welds positions that coincide (a wrapped seam repeats its vertices), drops the triangles that collapse to a line (a
/// pole's fan), and checks that every directed edge has exactly one opposite: a closed, consistently wound surface.
/// Returns the signed volume, which is positive when the normals point outward.
fn closed_volume(mesh: &PartMesh) -> f32 {
    let g = &mesh.geometry;
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    let weld: Vec<u32> = g
        .positions
        .iter()
        .map(|p| {
            let key = [p.x, p.y, p.z].map(|v| (f64::from(v) * 1e6).round() as i64);
            let next = u32::try_from(ids.len()).unwrap();
            *ids.entry(key).or_insert(next)
        })
        .collect();
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    let mut volume = 0.0_f32;
    for t in g.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| weld[i as usize]);
        if a == b || b == c || a == c {
            continue;
        }
        for edge in [(a, b), (b, c), (c, a)] {
            *edges.entry(edge).or_default() += 1;
        }
        let [pa, pb, pc] = [t[0], t[1], t[2]].map(|i| g.positions[i as usize]);
        volume += pa.dot(pb.cross(pc)) / 6.0;
    }
    for (&(a, b), &n) in &edges {
        assert_eq!(n, 1, "edge {a}->{b} is used {n} times in {}", mesh.name);
        assert_eq!(edges.get(&(b, a)), Some(&1), "edge {a}->{b} has no opposite in {}: the shell is open", mesh.name);
    }
    volume
}

fn assert_closed(b: &BuiltCharacter, what: &str) {
    let all = meshes(b);
    assert!(!all.is_empty(), "{what}: built no mesh");
    for mesh in all {
        let v = closed_volume(mesh);
        assert!(v > 0.0, "{what}: {} has volume {v}: its normals point inward", mesh.name);
    }
}

/// Every kind with a representative set of options, which between them build every shape.
fn kinds() -> Vec<(&'static str, Value)> {
    vec![
        ("choker", json!({})),
        ("choker", json!({ "pendant": { "shape": "drop", "size": 0.01 } })),
        ("choker", json!({ "pendant": { "shape": "gem" }, "along": 0.2 })),
        ("bell", json!({})),
        ("bell", json!({ "dangle": false, "cord": 0.0 })),
        ("charm", json!({})),
        ("charm", json!({ "shape": "drop" })),
        ("charm", json!({ "shape": "gem", "dangle": false })),
        ("hairpin", json!({})),
        ("hairpin", json!({ "style": "clip" })),
        ("hairpin", json!({ "style": "flower", "size": 0.04 })),
        ("halo", json!({})),
        ("halo", json!({ "bob": false, "radius": 0.12 })),
    ]
}

#[test]
fn every_kind_builds_closed_shells_on_the_four_templates() {
    for template in TEMPLATES {
        for (kind, params) in kinds() {
            let b = built(template, kind, params.clone(), None);
            assert_closed(&b, &format!("{kind} {params} on {template}"));
        }
    }
}

#[test]
fn the_kinds_are_accessories_registered_with_their_parameters() {
    for kind in ["choker", "bell", "hairpin", "halo", "charm"] {
        let registration = find_kind(kind).unwrap_or_else(|| panic!("{kind} is not registered"));
        let meta = (registration.meta)();
        assert_eq!(meta.category, omuzaki_char_api::PartCategory::Accessory);
        let defaults = (registration.defaults)();
        assert_eq!((registration.check)(&defaults).unwrap(), defaults, "{kind}: the defaults are valid and normalised");
        assert!((registration.check)(&json!({ "no_such_field": 1 })).is_err(), "{kind}: typos are refused");
    }
}

// ------------------------------------------------------------------------------------------------ attach resolution

/// A `Published` with a head joint and one anchor `top` on part `head`.
fn published() -> Published {
    let mut p = Published::default();
    let fit = JointFit::new([(JointRef::HEAD, Vec3::new(0.0, 1.5, 0.0)), (JointRef::NECK, Vec3::new(0.0, 1.4, 0.0))]);
    p.absorb(&pid("body"), Publishes::default().with::<FitFact>(std::sync::Arc::new(fit))).unwrap();
    let top = Anchor { bone: JointRef::HEAD, position: Vec3::new(0.0, 1.7, 0.0), rotation: Quat::from_rotation_y(0.5) };
    p.absorb(&pid("head"), Publishes::default().with::<AnchorsFact>(("top".parse().unwrap(), top))).unwrap();
    p
}

#[test]
fn a_bone_attachment_sits_at_the_joint_and_the_offset_is_in_its_frame() {
    let offset = AttachOffset { translation: [0.01, 0.02, 0.03], rotation_deg: [90.0, 0.0, 0.0], scale: 2.0 };
    let at = attach::resolve(&pid("acc"), &Attach::Bone { bone: JointRef::HEAD, offset }, &published()).unwrap();
    assert_eq!(at.joint, JointRef::HEAD);
    let origin = at.origin();
    assert!((origin - omuzaki_core::glam::DVec3::new(0.01, 1.52, 0.03)).length() < 1e-6, "{origin:?}");
    // Local +Z is turned to -Y by x 90 and doubled by the scale.
    let z = at.direction(omuzaki_core::glam::DVec3::Z);
    assert!((z - omuzaki_core::glam::DVec3::new(0.0, -2.0, 0.0)).length() < 1e-6, "{z:?}");
}

#[test]
fn an_anchor_attachment_follows_the_anchors_bone_and_frame() {
    let offset = AttachOffset { translation: [0.0, 0.1, 0.0], ..AttachOffset::default() };
    let attach = Attach::Anchor { part: pid("head"), anchor: "top".parse().unwrap(), offset };
    let at = attach::resolve(&pid("acc"), &attach, &published()).unwrap();
    assert_eq!(at.joint, JointRef::HEAD);
    // The anchor is turned 0.5 rad about y, so the offset up is still up and the origin is the anchor lifted by 0.1.
    assert!((at.origin() - omuzaki_core::glam::DVec3::new(0.0, 1.8, 0.0)).length() < 1e-6);
    let x = at.direction(omuzaki_core::glam::DVec3::X);
    assert!((x - omuzaki_core::glam::DVec3::new(0.5f64.cos(), 0.0, -(0.5f64.sin()))).length() < 1e-6, "{x:?}");
}

#[test]
fn a_missing_anchor_or_bone_is_an_error_that_names_it() {
    let p = published();
    let anchor = |part: &str, name: &str| Attach::Anchor {
        part: pid(part),
        anchor: name.parse().unwrap(),
        offset: AttachOffset::default(),
    };
    let message = |a: &Attach| attach::resolve(&pid("acc"), a, &p).unwrap_err().to_string();
    let wrong_name = message(&anchor("head", "crown"));
    assert!(
        wrong_name.contains("'crown'") && wrong_name.contains("'head'") && wrong_name.contains("top"),
        "{wrong_name}"
    );
    let wrong_part = message(&anchor("hair", "tip"));
    assert!(
        wrong_part.contains("'tip'") && wrong_part.contains("'hair'") && wrong_part.contains("no anchors"),
        "{wrong_part}"
    );
    let wrong_bone = message(&Attach::Bone { bone: JointRef::CHEST, offset: AttachOffset::default() });
    assert!(wrong_bone.contains("bone") && wrong_bone.contains("acc"), "{wrong_bone}");
    let bad_scale = message(&Attach::Bone {
        bone: JointRef::HEAD,
        offset: AttachOffset { scale: -1.0, ..AttachOffset::default() },
    });
    assert!(bad_scale.contains("scale"), "{bad_scale}");
}

#[test]
fn building_with_a_missing_anchor_fails_naming_the_part_and_the_anchor() {
    let attach =
        Attach::Anchor { part: pid("head"), anchor: "crown".parse().unwrap(), offset: AttachOffset::default() };
    let err = build(&character("standard", "halo", json!({}), Some(attach))).unwrap_err().to_string();
    assert!(err.contains("acc") && err.contains("'crown'") && err.contains("top"), "{err}");
}

#[test]
fn the_attach_points_list_the_bones_and_the_heads_top() {
    let b = built("standard", "halo", json!({}), None);
    let points = attach_points(&b);
    let head = points.iter().find(|p| p.name == "head").expect("the head bone is listed");
    assert!(matches!(&head.target, AttachTarget::Bone { joint } if *joint == JointRef::HEAD));
    let top = points.iter().find(|p| p.name == "head/top").expect("the head's top anchor is listed");
    assert!(top.position[1] > head.position[1], "the top of the head is above the head joint: {top:?} {head:?}");
    assert!(points.iter().any(|p| p.name == "leftHand"));
}

// ------------------------------------------------------------------------------------------------ posing

/// The positions of `mesh` skinned with the local pose `local`.
fn skinned(b: &BuiltCharacter, mesh: &PartMesh, local: &Pose) -> Vec<Vec3> {
    let skeleton = &b.rig.skeleton;
    let inverse_bind = b.rig.inverse_bind();
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(local, &mut world);
    let matrices: Vec<Mat4> = mesh
        .joint_bones
        .iter()
        .map(|joint| {
            let id = b.rig.bone(joint).unwrap_or_else(|| panic!("{joint:?} is not in the rig")).index();
            world.local[id].to_mat4() * inverse_bind[id]
        })
        .collect();
    let g = &mesh.geometry;
    (0..g.positions.len())
        .map(|v| {
            (0..4)
                .map(|k| matrices[usize::from(g.joints[v][k])].transform_point3(g.positions[v]) * g.weights[v][k])
                .sum()
        })
        .collect()
}

/// Where `bone` is in the skeleton's rest pose.
fn rest_joint(b: &BuiltCharacter, bone: HumanoidBone) -> Vec3 {
    let skeleton = &b.rig.skeleton;
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&skeleton.rest_pose(), &mut world);
    world.local[skeleton.humanoid_bone(bone).unwrap().index()].translation
}

/// Posing `bone` by `turn` moves every vertex of every accessory mesh as a rigid turn of the bone's joint.
fn assert_rides_the_bone(b: &BuiltCharacter, bone: HumanoidBone, turn: Quat, what: &str) {
    let skeleton = &b.rig.skeleton;
    let id = skeleton.humanoid_bone(bone).unwrap().index();
    let rest = skeleton.rest_pose();
    let mut posed = rest.clone();
    posed.local[id].rotation = turn * rest.local[id].rotation;
    let pivot = rest_joint(b, bone);
    for mesh in meshes(b) {
        let (before, after) = (skinned(b, mesh, &rest), skinned(b, mesh, &posed));
        let mut worst = 0.0_f32;
        for (p, q) in before.iter().zip(&after) {
            worst = worst.max((pivot + turn * (*p - pivot)).distance(*q));
        }
        assert!(worst < 1e-4, "{what}: {} is {worst} m off a rigid turn of the {bone:?}", mesh.name);
    }
}

#[test]
fn accessories_move_with_their_joint_on_every_template() {
    let yaw = Quat::from_rotation_y(MAX_SWING_DEG.to_radians() as f32);
    for template in TEMPLATES {
        // A hat-like pin and a halo on the head's `top` anchor, riding the head.
        let pin = built(template, "hairpin", json!({ "style": "flower" }), Some(on_head_top([0.0, 0.0, 0.0])));
        assert_rides_the_bone(&pin, HumanoidBone::Head, yaw, &format!("flower on top, {template}"));
        let halo = built(template, "halo", json!({ "bob": false }), None);
        assert_rides_the_bone(&halo, HumanoidBone::Head, yaw, &format!("halo, {template}"));
        // A choker follows the neck.
        let choker = built(template, "choker", json!({ "pendant": {} }), None);
        assert_rides_the_bone(&choker, HumanoidBone::Neck, yaw, &format!("choker, {template}"));
        // An item in the hand rides the hand bone, offset from it.
        let item = bone(JointRef::Humanoid(HumanoidBone::LeftHand), [0.0, -0.02, 0.01]);
        let charm = built(template, "charm", json!({ "dangle": false, "shape": "gem" }), Some(item));
        assert_rides_the_bone(
            &charm,
            HumanoidBone::LeftHand,
            Quat::from_rotation_z(0.7),
            &format!("charm in hand, {template}"),
        );
    }
}

#[test]
fn an_attachment_to_an_anchor_follows_the_bone_the_anchor_follows() {
    let b = built("standard", "hairpin", json!({}), Some(on_head_top([0.01, 0.0, 0.0])));
    assert_rides_the_bone(&b, HumanoidBone::Head, Quat::from_rotation_x(0.4), "pin on top");
    // Moving the offset moves the pin by exactly that, in the anchor's frame.
    let moved = built("standard", "hairpin", json!({}), Some(on_head_top([0.01, 0.05, 0.0])));
    let centre = |b: &BuiltCharacter| -> Vec3 {
        let all = meshes(b);
        let sum: Vec3 = all.iter().flat_map(|m| m.geometry.positions.iter().copied()).sum();
        sum / all.iter().map(|m| m.geometry.positions.len()).sum::<usize>() as f32
    };
    assert!((centre(&moved) - centre(&b) - Vec3::new(0.0, 0.05, 0.0)).length() < 1e-5);
}

// ------------------------------------------------------------------------------------------------ dangling parts

/// Swinging the dangling bone by `MAX_SWING_DEG` about `axis` moves no vertex more than 3 cm from where it rests.
fn assert_stays_within_3_cm(b: &BuiltCharacter, kind: &str, what: &str) {
    let swing =
        b.rig.skeleton.find(&format!("{kind}_acc_0")).unwrap_or_else(|| panic!("{what}: no swing bone")).index();
    let rest = b.rig.skeleton.rest_pose();
    let angle = MAX_SWING_DEG.to_radians() as f32;
    for axis in [Vec3::X, Vec3::Z, (Vec3::X + Vec3::Z).normalize()] {
        let mut turned = rest.clone();
        turned.local[swing].rotation = Quat::from_axis_angle(axis, angle) * rest.local[swing].rotation;
        for mesh in meshes(b) {
            let (before, after) = (skinned(b, mesh, &rest), skinned(b, mesh, &turned));
            let worst = before.iter().zip(&after).map(|(p, q)| p.distance(*q)).fold(0.0, f32::max);
            assert!(
                worst <= 0.03,
                "{what}: {} moves {worst} m at a {MAX_SWING_DEG} degree swing about {axis:?}",
                mesh.name
            );
        }
    }
}

#[test]
fn dangling_parts_add_a_valid_spring_chain_and_stay_within_3_cm_of_rest() {
    let longest_bell = json!({ "cord": 0.06, "radius": 0.003 });
    let cases = [
        ("bell", json!({})),
        ("bell", longest_bell),
        ("charm", json!({})),
        ("charm", json!({ "shape": "drop", "cord": 0.03 })),
    ];
    for template in TEMPLATES {
        for (kind, params) in &cases {
            let b = built(template, kind, params.clone(), None);
            let what = format!("{kind} {params} on {template}");
            let chain = b
                .rig
                .springs
                .chains
                .iter()
                .find(|c| c.name == format!("{kind}_acc"))
                .unwrap_or_else(|| panic!("{what}: no chain"));
            assert_eq!(chain.joints.len(), 2, "{what}: a two-bone chain");
            b.rig.springs.validate(&b.rig.skeleton).unwrap_or_else(|e| panic!("{what}: invalid springs: {e}"));
            assert_stays_within_3_cm(&b, kind, &what);
        }
    }
}

#[test]
fn a_piece_that_reaches_too_far_to_dangle_is_refused_with_the_reason() {
    let registration = find_kind("bell").unwrap();
    let err = (registration.check)(&json!({ "cord": 0.06, "radius": 0.012 })).unwrap_err().to_string();
    assert!(err.contains("cord") && err.contains("shorten") && err.contains("dangle"), "{err}");
    // The same piece is fine when it does not dangle.
    assert!((registration.check)(&json!({ "cord": 0.06, "radius": 0.012, "dangle": false })).is_ok());
    assert!(MAX_REACH * 2.0 * (MAX_SWING_DEG / 2.0).to_radians().sin() <= 0.03);
}

#[test]
fn a_halo_bob_is_a_valid_two_bone_chain_and_a_still_halo_adds_none() {
    let bobbing = built("standard", "halo", json!({}), None);
    assert_eq!(bobbing.rig.springs.chains.iter().filter(|c| c.name == "halo_acc").count(), 1);
    bobbing.rig.springs.validate(&bobbing.rig.skeleton).unwrap();
    let still = built("standard", "halo", json!({ "bob": false }), None);
    assert!(still.rig.springs.chains.iter().all(|c| c.name != "halo_acc"));
    assert!(still.rig.skeleton.find("halo_acc_0").is_none());
}

/// The head collider's sphere in model space after `pose`: centre and radius.
fn collider_sphere(b: &BuiltCharacter, pose: &Pose) -> (Vec3, f32) {
    let group = b.rig.springs.groups.iter().find(|g| g.name == "head").expect("the head group");
    let Collider { bone, shape: ColliderShape::Sphere { offset, radius } } = group.colliders[0] else {
        panic!("a sphere")
    };
    let mut world = Pose::IDENTITY;
    b.rig.skeleton.world_pose(pose, &mut world);
    (world.local[bone.index()].to_mat4().transform_point3(offset), radius)
}

#[test]
fn a_bell_declares_the_published_colliders_and_never_swings_into_the_head() {
    for template in TEMPLATES {
        let b = build(&ear_bell(template, json!({ "radius": 0.014, "cord": 0.03 }))).unwrap();
        let chain = b.rig.springs.chains.iter().find(|c| c.name == "bell_acc").expect("a chain");
        assert_eq!(chain.collider_groups, ["head"], "{template}: the chain opts into the head collider");
        b.rig.springs.validate(&b.rig.skeleton).unwrap();
        let skeleton = &b.rig.skeleton;
        let rest = skeleton.rest_pose();
        let (head, swing) =
            (skeleton.humanoid_bone(HumanoidBone::Head).unwrap().index(), skeleton.find("bell_acc_0").unwrap().index());
        // The head turns 25 degrees and the bell swings 25 degrees towards the head (about -Z; it hangs on +X).
        let mut posed = rest.clone();
        posed.local[head].rotation = Quat::from_rotation_y(MAX_SWING_DEG.to_radians() as f32);
        posed.local[swing].rotation = Quat::from_rotation_z(-(MAX_SWING_DEG.to_radians() as f32));
        let (centre, radius) = collider_sphere(&b, &posed);
        for mesh in meshes(&b) {
            for p in skinned(&b, mesh, &posed) {
                assert!(p.distance(centre) > radius, "{template}: {} reaches into the head collider", mesh.name);
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ review findings

/// The bind-space position and world rotation of `joint`, from the rig's inverse bind matrices.
fn world_bind(b: &BuiltCharacter, joint: &JointRef) -> Mat4 {
    let id = b.rig.bone(joint).expect("the bone exists").index();
    b.rig.inverse_bind()[id].inverse()
}

/// The mean of the vertices of all meshes of part `name`.
fn centroid_of(b: &BuiltCharacter, name: &str) -> Vec3 {
    let points: Vec<Vec3> = b
        .meshes
        .iter()
        .filter(|m| m.part == pid(name))
        .flat_map(|m| m.mesh.geometry.positions.iter().copied())
        .collect();
    points.iter().sum::<Vec3>() / points.len() as f32
}

#[test]
fn an_arm_bone_attachment_inherits_the_a_pose_hang_of_its_ancestors() {
    // `standard` is sculpted in an A-pose: the arm hangs, and the hand's world rotation is the product down the arm.
    let down = Vec3::NEG_Y;
    for bone in [HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand] {
        let joint = JointRef::Humanoid(bone);
        let charm = json!({ "dangle": false, "shape": "gem", "cord": 0.04 });
        let b = built(
            "standard",
            "charm",
            charm,
            Some(Attach::Bone { bone: joint.clone(), offset: AttachOffset::default() }),
        );
        let world = world_bind(&b, &joint);
        let origin = world.w_axis.truncate();
        // The charm hangs along its local -Y, so it points along the bone frame's -Y in bind space.
        let measured = (centroid_of(&b, "acc") - origin).normalize();
        let expected = world.transform_vector3(down).normalize();
        assert!(
            measured.dot(expected) > 0.9999,
            "{bone:?}: the charm points {measured:?}, the bone's frame says {expected:?}"
        );
        if bone != HumanoidBone::LeftUpperArm {
            // The bone's own (parent-relative) bind rotation alone would give another direction.
            let local = b.rig.bind.get(bone) * down;
            assert!(measured.dot(local) < 0.999, "{bone:?}: the test does not tell world from parent-relative");
        }
    }
}

#[test]
fn the_reach_cap_counts_the_attach_scale() {
    let at_scale = |scale: f64| {
        let offset = AttachOffset { scale, ..AttachOffset::default() };
        character("standard", "bell", json!({}), Some(Attach::Bone { bone: JointRef::HEAD, offset }))
    };
    let err = build(&at_scale(3.0)).unwrap_err().to_string();
    assert!(err.contains("scale 3") && err.contains("reaches") && err.contains("dangle"), "{err}");
    // A bigger bell that still fits swings within 3 cm, scale included.
    let b = build(&at_scale(1.5)).unwrap();
    assert_stays_within_3_cm(&b, "bell", "a bell at scale 1.5");
    // Without dangling there is nothing to cap.
    let offset = AttachOffset { scale: 3.0, ..AttachOffset::default() };
    let still =
        character("standard", "bell", json!({ "dangle": false }), Some(Attach::Bone { bone: JointRef::HEAD, offset }));
    build(&still).unwrap();
}

#[test]
fn a_part_can_attach_to_a_bone_another_part_contributed() {
    let mut def = character("standard", "halo", json!({}), None);
    let bone: omuzaki_char_api::BoneName = "halo_acc_0".parse().unwrap();
    let to_halo = Attach::Bone { bone: JointRef::Named(bone.clone()), offset: AttachOffset::default() };
    def.parts.insert(pid("hung"), part("charm", 60, Some(to_halo), json!({ "dangle": false, "cord": 0.02 })));
    let b = build(&def).unwrap();
    assert!(attach_points(&b).iter().any(|p| p.name == "halo_acc_0"), "the query lists the contributed bone");
    let at = world_bind(&b, &JointRef::Named(bone)).w_axis.truncate();
    let hung = centroid_of(&b, "hung");
    assert!(
        ((hung - at) * Vec3::new(1.0, 0.0, 1.0)).length() < 1e-3,
        "the charm hangs under the bone: {hung:?} vs {at:?}"
    );
    assert!(hung.y < at.y, "and below it");
    // The charm follows the bone: its mesh is skinned to the bone the halo contributed.
    let charm = b.meshes.iter().find(|m| m.part == pid("hung")).unwrap();
    assert!(charm.mesh.joint_bones.contains(&JointRef::Named("halo_acc_0".parse().unwrap())));
}

#[test]
fn the_collision_radius_holds_the_scaled_body() {
    let radius = |scale: f64| {
        let offset = AttachOffset { scale, ..AttachOffset::default() };
        let bell = json!({ "radius": 0.01 });
        let b =
            build(&character("standard", "bell", bell, Some(Attach::Bone { bone: JointRef::HEAD, offset }))).unwrap();
        let chain = b.rig.springs.chains.iter().find(|c| c.name == "bell_acc").unwrap().clone();
        (chain.joints[0].params.hit_radius, b)
    };
    let (one, _) = radius(1.0);
    let (scaled, b) = radius(1.5);
    assert!((one - 0.01).abs() < 1e-6, "the ball is the bell's radius: {one}");
    assert!((scaled - 0.015).abs() < 1e-6, "and grows with the attach scale: {scaled}");
    // The ball sits at the middle of the body (the tail bone) and holds every vertex of the bell's body: the bell sphere is
    // the primary mesh's lowest part, so its lowest vertex is within the radius of the tail.
    let tail = world_bind(&b, &JointRef::Named("bell_acc_1".parse().unwrap())).w_axis.truncate();
    let lowest = meshes(&b)
        .iter()
        .flat_map(|m| m.geometry.positions.iter().copied())
        .min_by(|a, c| a.y.total_cmp(&c.y))
        .unwrap();
    assert!(lowest.distance(tail) <= scaled + 1e-4, "the bell's bottom is {} m from its centre", lowest.distance(tail));
}

// ------------------------------------------------------------------------------------------------ colour and speed

#[test]
fn a_palette_change_recolours_without_rebuilding_geometry() {
    let mut def = character("standard", "hairpin", json!({ "primary": "@pin" }), None);
    def.palette.0.insert("pin".parse().unwrap(), "#336699".parse().unwrap());
    let first = build(&def).unwrap();
    def.palette.0.insert("pin".parse().unwrap(), "#993366".parse().unwrap());
    let second = build(&def).unwrap();
    let material = |b: &BuiltCharacter| b.materials[&"acc-primary".parse().unwrap()].base;
    assert_ne!(material(&first), material(&second));
    assert_eq!(meshes(&first)[0].geometry.positions, meshes(&second)[0].geometry.positions);
}

#[test]
fn each_kind_builds_in_5_ms_or_less() {
    // The budget is for an optimised build; a debug build is checked against a generous ceiling so it still catches a
    // pathological slowdown.
    let limit = if cfg!(debug_assertions) { 0.1 } else { 0.005 };
    let options = BuildOptions::default();
    let services = BuildServices::default();
    for (kind, params) in kinds() {
        let def = character("standard", kind, params.clone(), None);
        let id = CharId::new("t").unwrap();
        let mut assembler = Assembler::new(&id, &def, &options);
        let order = plan(&def).unwrap();
        for p in &order {
            if *p == pid("acc") {
                break;
            }
            let output = build_part(&def, p, &options, &services, assembler.published()).unwrap();
            assembler.add(p, output).unwrap();
        }
        let runs = 20;
        let start = Instant::now();
        for _ in 0..runs {
            build_part(&def, &pid("acc"), &options, &services, assembler.published()).unwrap();
        }
        let each = start.elapsed().as_secs_f64() / f64::from(runs);
        println!("{kind} {params}: {:.3} ms", each * 1e3);
        assert!(each <= limit, "{kind} {params} took {:.2} ms", each * 1e3);
    }
}

// ------------------------------------------------------------------------------------------------ contact sheets

/// One sample for the sheets: an accessory, and how the character is posed in the second state.
struct Sample {
    name: &'static str,
    kind: &'static str,
    params: Value,
    attach: Option<Attach>,
    /// Bones turned by an angle about an axis, in degrees.
    turns: Vec<(HumanoidBone, Vec3, f32)>,
    /// The dangling or bobbing bone turned about an axis, in degrees.
    swing: Option<(Vec3, f32)>,
}

fn samples() -> Vec<Sample> {
    let head_turn = vec![(HumanoidBone::Head, Vec3::Y, 25.0), (HumanoidBone::Head, Vec3::X, 12.0)];
    let hand = bone(JointRef::Humanoid(HumanoidBone::LeftHand), [0.0, -0.02, 0.01]);
    vec![
        Sample {
            name: "pin",
            kind: "hairpin",
            params: json!({ "style": "flower", "size": 0.04 }),
            attach: Some(on_head_top([0.05, -0.012, 0.03])),
            turns: head_turn.clone(),
            swing: None,
        },
        Sample {
            name: "halo",
            kind: "halo",
            params: json!({}),
            attach: None,
            turns: vec![(HumanoidBone::Head, Vec3::Y, 25.0)],
            swing: Some((Vec3::Z, 10.0)),
        },
        Sample {
            name: "choker",
            kind: "choker",
            params: json!({ "pendant": { "shape": "drop", "size": 0.01 } }),
            attach: None,
            turns: vec![(HumanoidBone::Neck, Vec3::Y, 25.0)],
            swing: None,
        },
        Sample {
            name: "bell",
            kind: "bell",
            params: json!({ "radius": 0.014, "cord": 0.03 }),
            attach: None,
            turns: vec![(HumanoidBone::Head, Vec3::Y, 25.0)],
            swing: Some((-Vec3::Z, 25.0)),
        },
        Sample {
            name: "charm",
            kind: "charm",
            params: json!({ "shape": "gem", "size": 0.02, "dangle": false }),
            attach: Some(hand),
            turns: vec![(HumanoidBone::LeftUpperArm, Vec3::Z, 70.0)],
            swing: None,
        },
    ]
}

/// Writes every sample at rest and posed as an OBJ file, for `tools/accessory-sheet.py`.
/// `OMUZAKI_SHEET_DIR=<dir> cargo test -p omuzaki-char --test accessory_rigid -- --ignored dump`.
#[test]
#[ignore = "writes the sheets' meshes to $OMUZAKI_SHEET_DIR"]
#[allow(clippy::disallowed_methods, reason = "a developer tool writing to a directory the person named, not an op")]
fn dump() {
    use std::fmt::Write as _;
    let dir =
        std::path::PathBuf::from(std::env::var("OMUZAKI_SHEET_DIR").expect("set OMUZAKI_SHEET_DIR to a directory"));
    std::fs::create_dir_all(&dir).unwrap();
    for template in ["standard", "soft-sd"] {
        for sample in samples() {
            let b = if sample.name == "bell" {
                build(&ear_bell(template, sample.params.clone())).unwrap()
            } else {
                built(template, sample.kind, sample.params.clone(), sample.attach.clone())
            };
            let skeleton = &b.rig.skeleton;
            // The pose the meshes were built in: the skeleton's rest turned by `bind` (the rest rotations are identity).
            let mut rest = skeleton.rest_pose();
            for (bone, id) in skeleton.humanoid_map().expect("a humanoid").iter() {
                rest.local[id.index()].rotation = b.rig.bind.get(bone);
            }
            let hips = skeleton.humanoid_map().unwrap().hips().index();
            rest.local[hips].translation += b.rig.bind.hips_offset;
            let mut posed = rest.clone();
            for (bone, axis, degrees) in &sample.turns {
                let id = skeleton.humanoid_bone(*bone).unwrap().index();
                posed.local[id].rotation =
                    Quat::from_axis_angle(*axis, degrees.to_radians()) * posed.local[id].rotation;
            }
            if let Some((axis, degrees)) = sample.swing {
                let id = skeleton.find(&format!("{}_acc_0", sample.kind)).expect("a swing bone").index();
                posed.local[id].rotation = Quat::from_axis_angle(axis, degrees.to_radians()) * posed.local[id].rotation;
            }
            for (state, pose) in [("rest", &rest), ("posed", &posed)] {
                let mut obj = String::new();
                let mut base = 1_usize;
                if sample.name == "bell" {
                    let (c, r) = collider_sphere(&b, pose);
                    writeln!(obj, "# collider {} {} {} {r}", c.x, c.y, c.z).unwrap();
                }
                for accessory in [false, true] {
                    for m in b.meshes.iter().filter(|m| (m.part == pid("acc")) == accessory) {
                        let name = if accessory {
                            format!("acc-{}", m.mesh.name)
                        } else {
                            format!("{}-{}", m.part, m.mesh.name)
                        };
                        let colour = b.materials[&m.mesh.material].base;
                        writeln!(obj, "# colour {name} {} {} {}", colour.r, colour.g, colour.b).unwrap();
                        writeln!(obj, "g {name}").unwrap();
                        let points = skinned(&b, &m.mesh, pose);
                        for p in &points {
                            writeln!(obj, "v {} {} {}", p.x, p.y, p.z).unwrap();
                        }
                        for t in m.mesh.geometry.indices.chunks(3) {
                            let [x, y, z] = [t[0], t[1], t[2]].map(|i| i as usize + base);
                            writeln!(obj, "f {x} {y} {z}").unwrap();
                        }
                        base += points.len();
                    }
                }
                std::fs::write(dir.join(format!("{template}-{}-{state}.obj", sample.name)), obj).unwrap();
            }
        }
    }
}
