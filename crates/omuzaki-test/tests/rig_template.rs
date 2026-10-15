//! The soft-sd template rig end to end (RG-01, RG-09): `omuzaki-char` builds the skeleton through the reference builder,
//! `omuzaki-rig` solves poses onto it and skins them into `Scene::joints`. The only place the two implementations meet.
//!
//! Four things are shown (decision 0014). The rig is a valid VRM humanoid with all 55 bones (30 of them fingers)
//! whatever the hand style, equal to what `template::skeleton::assemble` makes. It rests in the T-pose (arms level
//! along X) and is bound in the prototype's A-pose: posing it at `bind` gives the prototype's joints, with the prototype's
//! own arm angles, and the skinned mesh at `bind` is the built mesh. Then eight stress poses authored in the T (arms
//! up, crossed arms, squat, head yaw both ways, spine bend, fist, peace) solve and skin without a NaN and no bone
//! changes length by more than 1e-5. Finally a T-authored "raise both arms 90 degrees" gives arms straight up and the
//! peace sign opens the 20 degree V.
//!
//! The test mesh is weighted by the auto-weights (`omuzaki_char::kit::skin`), over the rig's own bind pose.
//! Set `OMUZAKI_RIG_TEMPLATE_DUMP` to a directory to write the posed joints there as JSON, for plotting.

// Test helpers may panic: a panic is a clear test failure. Counts and indices are tiny, so the casts are exact.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::path::Path;
use std::sync::Arc;

use omuzaki_anim::hands::{HandLibrary, HandRest, Side, apply};
use omuzaki_anim_api::{BoneId, HUMANOID_COUNT, HumanoidBone as B, HumanoidPose, Pose, Skeleton};
use omuzaki_char::kit::js_names;
use omuzaki_char::kit::skin::{CapsuleParams, SegmentSet};
use omuzaki_char::template::registry;
use omuzaki_char::template::skeleton::{self, TemplateFit};
use omuzaki_char_api::{
    BodyParams, BuildCtx, BuildError, BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef,
    CharacterRig, FitFact, HandStyle, HeadParams, JointFit, JointRef, MeshData, PartCategory, PartKind, PartMeta,
    PartOutput, Publishes, RegistryBuilder,
};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_render_api::{Camera, Scene};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_rig::skin::Skinner;
use serde_json::{Value, json};

// ------------------------------------------------------------------------------------------------ the two parts

/// Stands in for the `body` kind: the soft-sd layout for the real body numbers, published as the body's fit.
struct TemplateBody;

impl PartKind for TemplateBody {
    type Params = BodyParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "template-body",
            summary: "Publish the soft-sd layout.",
            category: PartCategory::Body,
            requires: &[],
        }
    }

    fn build(p: &BodyParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let template = registry::find("soft-sd").ok_or_else(|| ctx.fail("soft-sd is not registered"))?;
        let layout = template.try_layout(&p.proportions, &p.shape, &p.hand).map_err(|e| ctx.fail(e.to_string()))?;
        let fit = Arc::new(TemplateFit::new(layout));
        Ok(PartOutput { publishes: Publishes::default().with::<FitFact>(fit), ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(TemplateBody);

/// Where the head part puts the head, eyes and jaw relative to the end of the neck bone (stand-in numbers).
fn head_joints(base: Vec3) -> [(B, Vec3); 4] {
    let eye = Vec3::new(0.04, 0.15, 0.10);
    [
        (B::Head, base),
        (B::LeftEye, base + eye),
        (B::RightEye, base + Vec3::new(-eye.x, eye.y, eye.z)),
        (B::Jaw, base + Vec3::new(0.0, 0.06, 0.02)),
    ]
}

/// Stands in for the `head` kind: asks the rig where the head belongs and publishes it with the eyes and jaw.
struct TemplateHead;

impl PartKind for TemplateHead {
    type Params = HeadParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "template-head",
            summary: "Publish the head, eyes and jaw joints.",
            category: PartCategory::Head,
            requires: &["template-body"],
        }
    }

    fn build(_: &HeadParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let base = ctx.joint(&JointRef::HEAD).ok_or_else(|| ctx.fail("the body published no head joint"))?;
        let fit = JointFit::new(head_joints(base).map(|(bone, at)| (JointRef::Humanoid(bone), at)));
        Ok(PartOutput { publishes: Publishes::default().with::<FitFact>(Arc::new(fit)), ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(TemplateHead);

fn rig_of(body: &BodyParams) -> CharacterRig {
    let def: CharacterDef = serde_json::from_value(json!({
        "name": "rig-template",
        "template": "soft-sd",
        "parts": {
            "body": { "kind": "template-body", "params": serde_json::to_value(body).unwrap() },
            "head": { "kind": "template-head" },
        },
    }))
    .unwrap();
    let id = CharId::new("rig-template").unwrap();
    RegistryBuilder.build(&id, &def, &BuildOptions::default(), &BuildServices::default()).unwrap().rig
}

// ------------------------------------------------------------------------------------------------ the rig

#[test]
fn the_rig_is_a_valid_vrm_humanoid_with_all_55_bones_and_identity_rest_rotations() {
    let rig = rig_of(&BodyParams::default());
    let skeleton = &rig.skeleton;
    assert_eq!((skeleton.len(), rig.springs.chains.len()), (HUMANOID_COUNT, 0));
    let map = skeleton.require_humanoid().unwrap();
    assert_eq!(map.iter().count(), HUMANOID_COUNT, "every humanoid bone has a role");
    for bone in B::ALL {
        let id = skeleton.humanoid_bone(bone).unwrap_or_else(|| panic!("{bone} is missing"));
        let def = skeleton.get(id).unwrap();
        assert_eq!(def.parent, bone.parent().and_then(|p| skeleton.humanoid_bone(p)), "{bone}");
        assert_eq!(def.rest.rotation, Quat::IDENTITY, "{bone}");
    }
    let fingers = B::ALL
        .iter()
        .filter(|b| std::iter::successors(b.parent(), |p| p.parent()).any(|a| a == B::LeftHand || a == B::RightHand));
    assert_eq!(fingers.count(), 30);
    // Valid by the contract's own rules (15 required bones, the VRM hierarchy, unique names), not only by construction.
    assert_eq!(&Skeleton::new(Vec::from(skeleton.clone())).unwrap(), skeleton);
}

#[test]
fn hand_style_never_changes_the_rig() {
    let rig = |hands| rig_of(&BodyParams { hands, ..BodyParams::default() });
    let (mitten, simple, five) = (rig(HandStyle::Mitten), rig(HandStyle::Simple), rig(HandStyle::FiveFinger));
    assert!(mitten == simple && simple == five);
}

#[test]
fn the_builders_rig_is_the_template_assembly() {
    let body = BodyParams::default();
    let layout = registry::find("soft-sd").unwrap().layout(&body.proportions, &body.shape, &body.hand);
    let base = layout.head_base().unwrap().as_vec3();
    let all: Vec<_> = skeleton::joints(&layout).into_iter().chain(head_joints(base)).collect();
    let rig = rig_of(&body);
    assert_eq!(rig.bind, skeleton::bind_pose(&all), "the rig is bound in the template's pose");
    assert_eq!(skeleton::assemble(&all, &rig.bind).unwrap(), rig.skeleton);
    assert!(rig.validate_bind().is_ok());
}

/// The world position of every bone at rest (the T-pose), indexed by `BoneId`.
fn world_rest(skeleton: &Skeleton) -> Vec<Vec3> {
    let rest = RigSkeleton::new(skeleton.clone());
    (0..skeleton.len()).map(|i| rest.rest_world(BoneId(i as u16)).unwrap().translation).collect()
}

/// The world position of every bone in the bind pose (the sculpted A-pose): the translation of each inverse bind matrix,
/// inverted back.
fn world_bind(rig: &CharacterRig) -> Vec<Vec3> {
    rig.inverse_bind().iter().map(|m| m.inverse().transform_point3(Vec3::ZERO)).collect()
}

#[test]
fn the_rig_rests_in_the_t_pose_and_is_bound_in_the_prototypes_a_pose() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../omuzaki-char/tests/fixtures/template/soft_sd.json")).unwrap();
    let rig = rig_of(&BodyParams::default());
    let world = world_bind(&rig);
    let mut worst = 0.0_f32;
    for (name, at) in fixture["joints"].as_object().unwrap() {
        let at: Vec<f32> = at.as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
        let id = rig.bone(&js_names::joint(name).unwrap()).unwrap();
        worst = worst.max(world[id.index()].distance(Vec3::from_slice(&at)));
    }
    eprintln!("bind pose: 19 prototype joints, worst difference {worst:e} m");
    assert!(worst < 1e-6);

    // The prototype hangs the arms by 58.57 and 4.86 degrees (`armRest` in `vrm-load.js`): in the bind pose,
    // and at rest they are level.
    let (upper, lower) = arm_hang(&rig.skeleton, &world);
    eprintln!("bind arm hang: upper {:.3} deg, lower +{:.3} deg", upper.to_degrees(), (lower - upper).to_degrees());
    assert!((upper.to_degrees() - 58.57).abs() < 0.01 && ((lower - upper).to_degrees() - 4.86).abs() < 0.01);
    let (upper, lower) = arm_hang(&rig.skeleton, &world_rest(&rig.skeleton));
    eprintln!("rest arm hang: upper {upper:e} rad, lower {lower:e} rad");
    assert!(upper.abs() < 1e-6 && lower.abs() < 1e-6, "the arms rest horizontal (VRM tpose.md)");
}

/// How far below horizontal the left upper arm and the left forearm hang in `world`, in radians.
fn arm_hang(skeleton: &Skeleton, world: &[Vec3]) -> (f32, f32) {
    let at = |bone| world[skeleton.humanoid_bone(bone).unwrap().index()];
    let hang = |from, to| {
        let d: Vec3 = at(to) - at(from);
        (-d.y).atan2(d.x)
    };
    (hang(B::LeftUpperArm, B::LeftLowerArm), hang(B::LeftLowerArm, B::LeftHand))
}

// ------------------------------------------------------------------------------------------------ poses and skinning

/// The rig, the test mesh and everything a pose needs.
struct Figure {
    rig: CharacterRig,
    skeleton: Skeleton,
    humanoid: HumanoidRig,
    skinner: Skinner,
    bind: Vec<Vec3>,
    mesh: MeshData,
}

/// The segment set of the rig's bones in the bind pose: the same capsule axes a part gets from the rig's fit.
fn segment_set(skeleton: &Skeleton, bind: &[Vec3]) -> SegmentSet {
    let joints = B::ALL.map(|b| (JointRef::Humanoid(b), bind[skeleton.humanoid_bone(b).unwrap().index()]));
    let roles: Vec<JointRef> = B::ALL.map(JointRef::Humanoid).into();
    SegmentSet::from_fit(&JointFit::new(joints), &roles).unwrap()
}

/// A mesh of points around every bone (in the bind pose), weighted by auto-weights: `SegmentSet::weights_at` over all
/// 55 bones. Joint indices are `BoneId`s, as the skinner reads them.
fn test_mesh(skeleton: &Skeleton, bind: &[Vec3]) -> MeshData {
    let segments = segment_set(skeleton, bind);
    let candidates: Vec<JointRef> = B::ALL.map(JointRef::Humanoid).into();
    let mut mesh = MeshData::default();
    for (i, bone) in skeleton.bones().iter().enumerate() {
        let Some(parent) = bone.parent else { continue };
        for t in [0.2, 0.5, 0.8] {
            let spine = bind[parent.index()].lerp(bind[i], t);
            for offset in [Vec3::ZERO, Vec3::X, -Vec3::X, Vec3::Z, -Vec3::Z] {
                let p = spine + offset * 0.01;
                let blend = segments.weights_at(p.as_dvec3(), &candidates, CapsuleParams::PROTOTYPE);
                let (mut joints, mut weights) = ([0_u16; 4], [0.0_f32; 4]);
                for (k, (joint, w)) in blend.iter().enumerate() {
                    let JointRef::Humanoid(b) = joint else {
                        panic!("auto-weights returned a non-humanoid joint {joint:?}")
                    };
                    joints[k] = skeleton.humanoid_bone(*b).unwrap().0;
                    weights[k] = *w;
                }
                mesh.positions.push(p);
                mesh.joints.push(joints);
                mesh.weights.push(weights);
            }
        }
    }
    mesh
}

fn figure() -> Figure {
    let rig = rig_of(&BodyParams::default());
    let skeleton = rig.skeleton.clone();
    let bind = world_bind(&rig);
    // The inverse bind matrices come from the rig's bind pose, never from the T rest.
    let shared = RigSkeleton::with_inverse_bind(skeleton.clone(), rig.inverse_bind()).unwrap();
    let mesh = test_mesh(&skeleton, &bind);
    Figure { humanoid: HumanoidRig::new(shared.clone()), skinner: Skinner::new(shared), rig, skeleton, bind, mesh }
}

impl Figure {
    /// The skinning matrices of `pose`, pushed into a scene's joint palette the way a frame does.
    fn skin(&mut self, pose: &HumanoidPose) -> Vec<Mat4> {
        let mut local = Pose::IDENTITY;
        self.humanoid.apply_humanoid(pose, &mut local);
        let mut scene = Scene::new(Camera::default());
        let range = self.skinner.push(&local, &mut scene.joints);
        assert_eq!((range.start, range.len as usize), (0, self.skeleton.len()));
        scene.joints
    }

    fn bone(&self, bone: B) -> usize {
        self.skeleton.humanoid_bone(bone).unwrap().index()
    }

    /// The posed position of every bone: its bind joint (A-pose) carried by its own skinning matrix.
    fn posed(&self, matrices: &[Mat4]) -> Vec<Vec3> {
        matrices.iter().zip(&self.bind).map(|(m, p)| m.transform_point3(*p)).collect()
    }

    /// The test mesh skinned by `matrices`.
    fn skinned(&self, matrices: &[Mat4]) -> Vec<Vec3> {
        let m = &self.mesh;
        (0..m.positions.len())
            .map(|v| {
                (0..4)
                    .map(|k| m.weights[v][k] * matrices[usize::from(m.joints[v][k])].transform_point3(m.positions[v]))
                    .sum()
            })
            .collect()
    }
}

/// Sets `bone` and its mirror image, reflected the way `HumanoidPose::mirrored` does.
fn both(pose: &mut HumanoidPose, left: B, q: Quat) {
    pose.set(left, q);
    pose.set(left.mirrored(), Quat::from_xyzw(q.x, -q.y, -q.z, q.w));
}

/// How the thumbs of `skeleton` rest, measured once the way a rig does at bind time.
fn hand_rest(skeleton: &Skeleton) -> HandRest {
    HandRest::of(skeleton).unwrap()
}

/// `shape` from the hand-shape library on both hands.
fn hands(rest: &HandRest, shape: &str) -> HumanoidPose {
    let shape = HandLibrary::builtin().unwrap().get(shape).unwrap();
    let mut pose = HumanoidPose::REST;
    apply(&mut pose, Side::Left, rest, shape, 1.0);
    apply(&mut pose, Side::Right, rest, shape, 1.0);
    pose
}

/// Raises both arms by `angle` about Z in the T-pose: the pose a clip authored in the T would use.
fn raise_arms(angle: f32) -> HumanoidPose {
    let mut pose = HumanoidPose::REST;
    both(&mut pose, B::LeftUpperArm, Quat::from_rotation_z(angle));
    pose
}

fn stress_poses(rest: &HandRest) -> Vec<(&'static str, HumanoidPose)> {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_3};
    let mut arms_up = raise_arms(FRAC_PI_2);
    both(&mut arms_up, B::LeftLowerArm, Quat::from_rotation_z(0.2));

    let mut crossed = HumanoidPose::REST;
    both(&mut crossed, B::LeftUpperArm, Quat::from_rotation_y(-0.6) * Quat::from_rotation_x(-0.5));
    both(&mut crossed, B::LeftLowerArm, Quat::from_rotation_y(-2.0) * Quat::from_rotation_x(-0.3));
    both(&mut crossed, B::LeftHand, Quat::from_rotation_x(0.4));

    let mut squat = HumanoidPose::REST;
    squat.hips_offset = Vec3::new(0.0, -0.12, 0.0);
    both(&mut squat, B::LeftUpperLeg, Quat::from_rotation_x(-1.5));
    both(&mut squat, B::LeftLowerLeg, Quat::from_rotation_x(2.2));
    both(&mut squat, B::LeftFoot, Quat::from_rotation_x(-0.6));
    squat.set(B::Spine, Quat::from_rotation_x(0.3));

    let yaw = |angle: f32| {
        let mut pose = HumanoidPose::REST;
        pose.set(B::Neck, Quat::from_rotation_y(angle / 3.0));
        pose.set(B::Head, Quat::from_rotation_y(angle * 2.0 / 3.0));
        pose
    };

    let mut bend = HumanoidPose::REST;
    bend.set(B::Spine, Quat::from_rotation_x(0.5) * Quat::from_rotation_z(0.2));
    bend.set(B::Chest, Quat::from_rotation_x(0.3));
    bend.set(B::UpperChest, Quat::from_rotation_x(0.2) * Quat::from_rotation_z(-0.1));

    vec![
        ("arms up", arms_up),
        ("crossed arms", crossed),
        ("squat", squat),
        ("head yaw +60", yaw(FRAC_PI_3)),
        ("head yaw -60", yaw(-FRAC_PI_3)),
        ("spine bend", bend),
        ("fist", hands(rest, "fist")),
        ("peace", hands(rest, "peace")),
    ]
}

#[test]
fn the_bind_pose_is_rigid_and_the_stress_poses_skin_without_nan_and_keep_every_bone_length() {
    let mut f = figure();
    let (parent_of, rest_len): (Vec<_>, Vec<_>) = f
        .skeleton
        .bones()
        .iter()
        .enumerate()
        .map(|(i, b)| (b.parent.map(BoneId::index), b.parent.map_or(0.0, |p| f.bind[i].distance(f.bind[p.index()]))))
        .unzip();
    let mut dump = serde_json::Map::new();

    // Posed at `bind` every skinning matrix is the identity, so the skinned mesh is the built mesh.
    let bind_pose = f.rig.bind;
    let at_bind = f.skin(&bind_pose);
    let worst_matrix =
        at_bind.iter().map(|m| (*m - Mat4::IDENTITY).abs().to_cols_array().into_iter().fold(0.0, f32::max));
    eprintln!("posed at bind: worst skinning matrix difference from identity {:e}", worst_matrix.fold(0.0, f32::max));
    assert!(at_bind.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-5)), "the bind pose must be rigid");
    let drift = f.skinned(&at_bind).iter().zip(&f.mesh.positions).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
    eprintln!("skinned mesh at bind vs built mesh: {drift:e} m");
    assert!(drift < 1e-5, "the mesh moved {drift:e} m at bind");
    dump.insert("bind".into(), json!(f.posed(&at_bind)));
    let at_rest = f.skin(&HumanoidPose::REST);
    dump.insert("rest".into(), json!(f.posed(&at_rest)));

    let poses = stress_poses(&hand_rest(&f.skeleton));
    assert_eq!(poses.len(), 8, "seven stress poses, the head yaw in both directions");
    for (name, pose) in &poses {
        let matrices = f.skin(pose);
        assert!(
            matrices.iter().all(|m| m.to_cols_array().iter().all(|v| v.is_finite())),
            "{name}: a skinning matrix is not finite"
        );
        let moved = f.skinned(&matrices);
        assert!(moved.iter().all(|p| p.is_finite()), "{name}: a vertex is not finite");
        let solved = f.posed(&matrices);
        let (mut worst, mut moved_most) = (0.0_f32, 0.0_f32);
        for (i, parent) in parent_of.iter().enumerate() {
            if let Some(p) = parent {
                worst = worst.max((solved[i].distance(solved[*p]) - rest_len[i]).abs());
            }
        }
        for (a, b) in moved.iter().zip(&f.mesh.positions) {
            moved_most = moved_most.max(a.distance(*b));
        }
        eprintln!("{name:>13}: worst bone length change {worst:e} m, mesh moved up to {moved_most:.3} m");
        assert!(worst < 1e-5, "{name}: a bone changed length by {worst:e} m");
        assert!(moved_most > 0.01, "{name}: the pose moved nothing, so the checks above proved nothing");
        dump.insert((*name).into(), json!(solved));
    }

    if let Ok(dir) = std::env::var("OMUZAKI_RIG_TEMPLATE_DUMP") {
        let names: Vec<_> = f.skeleton.bones().iter().map(|b| b.name.clone()).collect();
        let doc = json!({ "names": names, "parents": parent_of, "poses": dump });
        std::fs::write(Path::new(&dir).join("posed.json"), serde_json::to_string(&doc).unwrap()).unwrap();
    }
}

/// The test mesh's weights are the auto-weights of its bones: normalised, at most four influences, and at each bone's
/// midpoint the heaviest influence is the nearest bone axis (the inverse-power falloff is monotonic in distance).
#[test]
fn the_test_mesh_carries_the_auto_weights_of_the_bones_it_sits_on() {
    let f = figure();
    for (vertex, (joints, weights)) in f.mesh.joints.iter().zip(&f.mesh.weights).enumerate() {
        let sum: f32 = weights.iter().sum();
        assert!((sum - 1.0).abs() <= 1e-6, "vertex {vertex}: weights {weights:?} sum to {sum}");
        assert!(weights.iter().filter(|w| **w > 0.0).count() <= 4, "vertex {vertex}: more than four influences");
        assert!(joints.iter().zip(weights).all(|(j, w)| *w == 0.0 || usize::from(*j) < f.skeleton.len()));
    }
    let set = segment_set(&f.skeleton, &f.bind);
    let mut checked = 0;
    for (i, bone) in f.skeleton.bones().iter().enumerate() {
        let Some(parent) = bone.parent else { continue };
        let parent = parent.index();
        let p = f.bind[parent].lerp(f.bind[i], 0.5);
        let vertex = f.mesh.positions.iter().position(|q| q.distance(p) < 1e-6).expect("the midpoint is a vertex");
        // Distance from the midpoint to every bone's axis, by bone id.
        let axes: Vec<(usize, f64)> = B::ALL
            .into_iter()
            .map(|b| {
                let id = f.bone(b);
                (id, set.get(&JointRef::Humanoid(b)).unwrap().distance(p.as_dvec3()).0)
            })
            .collect();
        let nearest = axes.iter().map(|(_, d)| *d).fold(f64::INFINITY, f64::min);
        let heaviest = (0..4)
            .max_by(|a, b| f.mesh.weights[vertex][*a].total_cmp(&f.mesh.weights[vertex][*b]))
            .map(|k| usize::from(f.mesh.joints[vertex][k]))
            .unwrap();
        let heaviest_axis = axes.iter().find(|(id, _)| *id == heaviest).map(|(_, d)| *d);
        assert!(
            heaviest_axis.is_some_and(|d| d <= nearest + 1e-6),
            "{}: the heaviest bone is not the nearest axis",
            bone.name
        );
        checked += 1;
    }
    assert_eq!(checked, f.skeleton.len() - 1, "every bone with a parent was checked");
}

#[test]
fn a_pose_authored_in_the_t_raises_the_arms_straight_up_and_head_yaw_turns_the_head_by_the_angle_asked() {
    let mut f = figure();
    let up = {
        let matrices = f.skin(&raise_arms(std::f32::consts::FRAC_PI_2));
        f.posed(&matrices)
    };
    for (shoulder, elbow, wrist) in
        [(B::LeftUpperArm, B::LeftLowerArm, B::LeftHand), (B::RightUpperArm, B::RightLowerArm, B::RightHand)]
    {
        let (top, mid, end) = (up[f.bone(shoulder)], up[f.bone(elbow)], up[f.bone(wrist)]);
        for (from, to) in [(top, mid), (mid, end)] {
            let d = (to - from).normalize();
            eprintln!("arm raised 90 degrees: direction {d}");
            assert!(d.y > 1.0 - 1e-5 && d.x.abs() < 1e-3 && d.z.abs() < 1e-3, "straight up: {top} {mid} {end}");
        }
    }

    let matrices =
        f.skin(&stress_poses(&hand_rest(&f.skeleton)).iter().find(|(name, _)| *name == "head yaw +60").unwrap().1);
    let facing = matrices[f.bone(B::Head)].transform_vector3(Vec3::Z);
    assert!((facing.angle_between(Vec3::Z).to_degrees() - 60.0).abs() < 0.01, "{facing}");
    assert!(facing.x > 0.0, "positive yaw turns the face towards +X");
}

#[test]
fn hand_shapes_act_on_the_t_rest_the_fist_curls_to_the_palm_and_the_peace_opens_a_20_degree_v() {
    let mut f = figure();
    let thumbs = hand_rest(&f.skeleton);
    let mut posed = |pose: &HumanoidPose| {
        let matrices = f.skin(pose);
        f.posed(&matrices)
    };
    let (rest, fist, peace) =
        (posed(&HumanoidPose::REST), posed(&hands(&thumbs, "fist")), posed(&hands(&thumbs, "peace")));
    let (hand, tip) = (f.bone(B::LeftHand), f.bone(B::LeftMiddleDistal));
    let (open, closed) = (rest[tip].distance(rest[hand]), fist[tip].distance(fist[hand]));
    eprintln!("middle fingertip to wrist: open {open:.4} m, fist {closed:.4} m");
    assert!(closed < 0.8 * open);
    // The palm faces -Y in the T-pose, so the curl moves the tip down.
    assert!(fist[tip].y < rest[tip].y - 0.005);

    // The library draws the peace sign as a 20 degree V, turning the fingers about the T-pose's -Y axis, which the
    // template now rests in.
    let finger = |from, to| peace[f.bone(to)] - peace[f.bone(from)];
    let index = finger(B::LeftIndexProximal, B::LeftIndexDistal);
    let v = index.angle_between(finger(B::LeftMiddleProximal, B::LeftMiddleDistal)).to_degrees();
    eprintln!("peace: index to middle finger angle {v:.2} deg (the library draws 20)");
    assert!((v - 20.0).abs() <= 3.0, "the V should be 20 +- 3 degrees: {v}");
}
