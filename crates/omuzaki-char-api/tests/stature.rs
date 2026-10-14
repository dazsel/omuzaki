//! Real-size characters: the body and the head are authored against the template and scaled to the character's
//! stature before anything else is built. A character built at scale `s` must be the same character built at scale 1,
//! with every length multiplied by `s` and nothing else changed: meshes, joints, inverse bind matrices, the bind pose,
//! anchors, colliders, extra bones, spring hit radii, face windows, and what later parts see while they build.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_anim_api::{
    Collider, ColliderGroup, ColliderShape, HumanoidBone, HumanoidPose, Pose, SpringChain, SpringParams, Transform,
};
use omuzaki_char_api::{
    Anchor, AnchorId, BodyFit, BodyRegion, BoneName, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter,
    BuiltMesh, CharId, CharacterBuilder, CharacterDef, ExtraBone, FaceOverlay, HeadSurface, JointFit, JointRef,
    MaterialName, MaterialSpec, MeshData, PartCategory, PartDef, PartId, PartKind, PartKindId, PartMesh, PartMeta,
    PartOutput, PartRuntime, Publishes, RegistryBuilder, RigContribution, Section, Stature, extra_id, humanoid_id,
};
use omuzaki_core::glam::{Mat4, Quat, Vec2, Vec3};
use omuzaki_face_api::{Check, FaceLayerId, FaceProfile, FaceProjection, FaceRect, Palette, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The factor under test: the body claims this stature and the build applies it.
const S: f32 = 0.75;

// ---- the body, authored at the template's size (1.9 m tall), claiming a stature scale

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct BodyParams {
    scale: f32,
    a_pose: bool,
    simulate: bool,
}

impl Default for BodyParams {
    fn default() -> Self {
        Self { scale: 1.0, a_pose: false, simulate: false }
    }
}

impl Validate for BodyParams {
    fn check(&self, _: &mut Check) {}
}

fn joint_at(bone: HumanoidBone) -> Option<Vec3> {
    use HumanoidBone as B;
    Some(match bone {
        B::Hips => Vec3::new(0.0, 1.0, 0.0),
        B::Spine => Vec3::new(0.0, 1.3, 0.0),
        B::Head => Vec3::new(0.0, 1.8, 0.0),
        B::LeftUpperLeg => Vec3::new(0.12, 0.95, 0.0),
        B::LeftLowerLeg => Vec3::new(0.12, 0.5, 0.0),
        B::LeftFoot => Vec3::new(0.12, 0.08, 0.0),
        B::RightUpperLeg => Vec3::new(-0.12, 0.95, 0.0),
        B::RightLowerLeg => Vec3::new(-0.12, 0.5, 0.0),
        B::RightFoot => Vec3::new(-0.12, 0.08, 0.0),
        B::LeftUpperArm => Vec3::new(0.25, 1.6, 0.0),
        B::LeftLowerArm => Vec3::new(0.5, 1.6, 0.0),
        B::LeftHand => Vec3::new(0.75, 1.6, 0.0),
        B::RightUpperArm => Vec3::new(-0.25, 1.6, 0.0),
        B::RightLowerArm => Vec3::new(-0.5, 1.6, 0.0),
        B::RightHand => Vec3::new(-0.75, 1.6, 0.0),
        _ => return None,
    })
}

#[derive(Debug)]
struct Torso {
    bind: HumanoidPose,
}

impl BodyFit for Torso {
    fn section(&self, region: BodyRegion, _: f32) -> Option<Section> {
        (region == BodyRegion::Torso).then_some(Section {
            centre: Vec3::new(0.0, 1.3, 0.0),
            axis: Vec3::Y,
            radii: Vec2::new(0.16, 0.1),
        })
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        match joint {
            JointRef::Humanoid(b) => joint_at(*b),
            JointRef::Named(_) => None,
        }
    }

    fn bind(&self) -> HumanoidPose {
        self.bind
    }
}

fn a_pose() -> HumanoidPose {
    let mut bind = HumanoidPose::REST;
    bind.hips_offset = Vec3::new(0.0, -0.06, 0.02);
    bind.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(-0.5));
    bind.set(HumanoidBone::RightUpperArm, Quat::from_rotation_z(0.5));
    bind
}

/// A skinned mesh whose vertices straddle the left elbow, so posing the arm moves them.
fn skinned(name: &str, id: &str) -> (PartMesh, (MaterialName, MaterialSpec)) {
    let material = MaterialName::new(id).unwrap();
    let positions = vec![Vec3::new(0.45, 1.65, 0.05), Vec3::new(0.55, 1.55, -0.05), Vec3::new(0.0, 1.0, 0.1)];
    let mesh = PartMesh {
        name: name.to_owned(),
        geometry: MeshData {
            normals: vec![Vec3::Z; 3],
            joints: vec![[0, 1, 0, 0], [0, 1, 0, 0], [2, 0, 0, 0]],
            weights: vec![[0.5, 0.5, 0.0, 0.0], [0.25, 0.75, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]],
            positions,
            indices: vec![0, 1, 2],
            ..MeshData::default()
        },
        joint_bones: vec![
            JointRef::Humanoid(HumanoidBone::LeftUpperArm),
            JointRef::Humanoid(HumanoidBone::LeftLowerArm),
            JointRef::HIPS,
        ],
        material: material.clone(),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    };
    (mesh, (material, MaterialSpec::toon("#cccccc".parse().unwrap())))
}

struct StatureBody;

impl PartKind for StatureBody {
    type Params = BodyParams;

    fn meta() -> PartMeta {
        PartMeta { id: "stature-body", summary: "A body.", category: PartCategory::Body, requires: &[] }
    }

    fn build(p: &BodyParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let (mesh, material) = skinned("skin", "stature-body-skin");
        let mut rig = RigContribution::default();
        let tassel = rig.add_bone(ExtraBone {
            name: BoneName::new("tassel").unwrap(),
            parent: JointRef::HIPS,
            rest: Transform::from_translation(Vec3::new(0.1, 0.8, 0.1)),
        });
        let tip = rig.add_bone(ExtraBone {
            name: BoneName::new("tassel_tip").unwrap(),
            parent: JointRef::Named(BoneName::new("tassel").unwrap()),
            rest: Transform::from_translation(Vec3::new(0.1, 0.6, 0.1)),
        });
        let params = SpringParams { hit_radius: 0.04, stiffness: 0.8, gravity_power: 1.0, ..SpringParams::default() };
        rig.chains.push(SpringChain::new("body_tassel", &[tassel, tip], params));
        let knee = Collider {
            bone: humanoid_id(HumanoidBone::LeftLowerLeg),
            shape: ColliderShape::Capsule {
                offset: Vec3::new(0.0, 0.1, 0.0),
                radius: 0.07,
                tail: Vec3::new(0.0, -0.2, 0.0),
            },
        };
        rig.collider_groups.push(ColliderGroup { name: "body_knees".to_owned(), colliders: vec![knee] });
        let head = Collider {
            bone: humanoid_id(HumanoidBone::Head),
            shape: ColliderShape::Sphere { offset: Vec3::new(0.0, 0.05, 0.0), radius: 0.13 },
        };
        let anchor =
            Anchor { bone: JointRef::HEAD, position: Vec3::new(0.0, 1.95, 0.0), rotation: Quat::from_rotation_y(0.3) };
        Ok(PartOutput {
            meshes: vec![mesh],
            materials: vec![material],
            runtime: p.simulate.then(|| Box::new(Idle) as Box<dyn PartRuntime>),
            publishes: Publishes {
                fit: Some(Arc::new(Torso { bind: if p.a_pose { a_pose() } else { HumanoidPose::REST } })),
                colliders: vec![ColliderGroup { name: "body".to_owned(), colliders: vec![head] }],
                anchors: vec![(AnchorId::new("crown").unwrap(), anchor)],
                stature: Some(Stature { scale: p.scale, height_m: 1.9 * p.scale }),
                ..Publishes::default()
            },
            rig,
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(StatureBody);

/// A simulation that does nothing; it only has to exist.
struct Idle;

impl PartRuntime for Idle {
    fn step(&mut self, _: &omuzaki_char_api::StepCtx<'_>) {}
    fn reset(&mut self, _: &dyn omuzaki_char_api::PoseSource) {}
    fn dynamic_count(&self) -> usize {
        0
    }
    fn dynamic_mesh(&self, _: usize) -> Option<omuzaki_char_api::DynamicMesh<'_>> {
        None
    }
    fn cost(&self) -> omuzaki_char_api::RuntimeCost {
        omuzaki_char_api::RuntimeCost::default()
    }
}

// ---- the head, authored at the template's size: a ball over the neck, with a face window

#[derive(Debug)]
struct Ball;

impl HeadSurface for Ball {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 1.8, 0.02)
    }

    fn radius(&self, dir: Vec3) -> f32 {
        0.19 + 0.01 * dir.x.abs()
    }
}

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct NoParams {}

impl Validate for NoParams {
    fn check(&self, _: &mut Check) {}
}

struct StatureHead;

impl PartKind for StatureHead {
    type Params = NoParams;

    fn meta() -> PartMeta {
        PartMeta { id: "stature-head", summary: "A head.", category: PartCategory::Head, requires: &["stature-body"] }
    }

    fn build(_: &NoParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let projection =
            FaceProjection { rect: FaceRect { x0: -0.12, x1: 0.12, y0: -0.1, y1: 0.12 }, front_fade: [0.0, 0.05] };
        let (mesh, (name, mut spec)) = skinned("head", "stature-head-skin");
        spec.overlays.push(FaceOverlay::of(&projection, Ball.centre(), FaceLayerId::Base));
        let fit = JointFit::new([(JointRef::Humanoid(HumanoidBone::Head), Vec3::new(0.0, 1.78, 0.0))]);
        Ok(PartOutput {
            meshes: vec![mesh],
            materials: vec![(name, spec)],
            publishes: Publishes {
                head: Some(Arc::new(Ball)),
                face: Some(projection),
                fit: Some(Arc::new(fit)),
                ..Publishes::default()
            },
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(StatureHead);

// ---- hair, built after the head, so in metres; reads the head and body as a simulation would

struct StatureHair;

impl PartKind for StatureHair {
    type Params = NoParams;

    fn meta() -> PartMeta {
        PartMeta { id: "stature-hair", summary: "Hair.", category: PartCategory::Hair, requires: &["stature-head"] }
    }

    fn build(_: &NoParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let torso = ctx.fit()?.section(BodyRegion::Torso, 0.5).map(|s| s.radii.x).unwrap_or_default();
        let top = head.point(Vec3::Y, 0.0);
        let (mesh, (name, mut spec)) = skinned("lock", "stature-hair-material");
        let mut mesh = mesh;
        let c = head.centre();
        mesh.geometry.positions = vec![top, c + (top - c) * 0.9, c];
        spec.overlays.push(ctx.face_overlay(FaceLayerId::Brows)?);
        let mut rig = RigContribution::default();
        let a = rig.add_bone(ExtraBone {
            name: BoneName::new("lock_0").unwrap(),
            parent: JointRef::HEAD,
            rest: Transform::from_translation(top),
        });
        let b = rig.add_bone(ExtraBone {
            name: BoneName::new("lock_1").unwrap(),
            parent: JointRef::Named(BoneName::new("lock_0").unwrap()),
            rest: Transform::from_translation(top * 0.9),
        });
        // A real hair strand: its collision radius is an absolute 1 cm whatever the body's size.
        let params = SpringParams { hit_radius: 0.01, stiffness: 0.5, gravity_power: 1.0, ..SpringParams::default() };
        rig.chains.push(SpringChain::new("hair_lock", &[a, b], params));
        let _ = extra_id(0);
        Ok(PartOutput {
            meshes: vec![mesh],
            materials: vec![(name, spec)],
            rig,
            warnings: vec![format!("head_radius={} torso_radius={torso}", head.radius(Vec3::Y))],
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(StatureHair);

// ---- helpers

fn part(kind: &str, layer: i32, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer, attach: None, params }
}

fn character(parts: &[(&str, PartDef)]) -> CharacterDef {
    CharacterDef {
        name: "Test".to_owned(),
        template: omuzaki_char_api::BodyTemplateId::new("soft-sd").unwrap(),
        seed: 1,
        palette: Palette::default(),
        skin_tone: None,
        parts: parts.iter().map(|(n, p)| (PartId::new(*n).unwrap(), p.clone())).collect(),
        face: FaceProfile::default(),
        weight_brushes: BTreeMap::new(),
    }
}

fn full(scale: f32, a_pose: bool) -> Result<BuiltCharacter, BuildError> {
    let def = character(&[
        ("body", part("stature-body", 0, json!({"scale": scale, "a_pose": a_pose}))),
        ("head", part("stature-head", 1, json!({}))),
        ("hair", part("stature-hair", 2, json!({}))),
    ]);
    RegistryBuilder.build(&CharId::new("s").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a - b).length() < 1e-5
}

fn near_mat(a: Mat4, b: Mat4) -> bool {
    a.to_cols_array().iter().zip(b.to_cols_array()).all(|(x, y)| (x - y).abs() < 1e-5)
}

/// The mesh's vertices skinned by the skeleton posed with `pose`: the arm bent, the hips lifted by `lift` metres.
fn skin(c: &BuiltCharacter, mesh: &BuiltMesh, lift: f32) -> Vec<Vec3> {
    let skeleton = &c.rig.skeleton;
    let mut local: Pose = skeleton.rest_pose();
    let arm = skeleton.humanoid_bone(HumanoidBone::LeftUpperArm).unwrap();
    local.local[arm.index()].rotation = Quat::from_rotation_z(0.8);
    let hips = skeleton.humanoid_bone(HumanoidBone::Hips).unwrap();
    local.local[hips.index()].translation += Vec3::new(0.0, lift, 0.0);
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&local, &mut world);
    let inverse_bind = c.rig.inverse_bind();
    let g = &mesh.mesh.geometry;
    (0..g.positions.len())
        .map(|v| {
            let mut out = Vec3::ZERO;
            for (slot, &weight) in g.weights[v].iter().enumerate() {
                if weight > 0.0 {
                    let bone = c.rig.bone(&mesh.mesh.joint_bones[usize::from(g.joints[v][slot])]).unwrap();
                    let m = world.local[bone.index()].to_mat4() * inverse_bind[bone.index()];
                    out += weight * m.transform_point3(g.positions[v]);
                }
            }
            out
        })
        .collect()
}

// ---- the scaled character is the unscaled one scaled

#[test]
fn skinned_positions_at_bind_and_in_a_pose_are_the_unscaled_ones_scaled() {
    for a in [false, true] {
        let (unit, scaled) = (full(1.0, a).unwrap(), full(S, a).unwrap());
        assert_eq!(unit.meshes.len(), scaled.meshes.len());
        for (u, s) in unit.meshes.iter().zip(&scaled.meshes) {
            for (pu, ps) in u.mesh.geometry.positions.iter().zip(&s.mesh.geometry.positions) {
                assert!(near(*pu * S, *ps), "{}: bind position {pu} scaled is not {ps}", s.mesh.name);
            }
            assert_eq!(u.mesh.geometry.weights, s.mesh.geometry.weights, "weights are not lengths");
            for lift in [0.0, 0.2] {
                let (posed_u, posed_s) = (skin(&unit, u, lift), skin(&scaled, s, lift * S));
                for (pu, ps) in posed_u.iter().zip(&posed_s) {
                    assert!(near(*pu * S, *ps), "{} (a_pose {a}, lift {lift}): {pu} scaled is not {ps}", s.mesh.name);
                }
            }
        }
    }
}

#[test]
fn joints_inverse_bind_matrices_and_the_bind_pose_scale_and_rotations_do_not() {
    let (unit, scaled) = (full(1.0, true).unwrap(), full(S, true).unwrap());
    let (ku, ks) = (&unit.rig.skeleton, &scaled.rig.skeleton);
    assert_eq!(ku.len(), ks.len());
    let (ibu, ibs) = (unit.rig.inverse_bind(), scaled.rig.inverse_bind());
    for i in 0..ku.len() {
        let (bu, bs) = (
            ku.get(omuzaki_anim_api::BoneId(u16::try_from(i).unwrap())).unwrap(),
            ks.get(omuzaki_anim_api::BoneId(u16::try_from(i).unwrap())).unwrap(),
        );
        assert!(near(bu.rest.translation * S, bs.rest.translation), "{}: rest offset", bs.name);
        assert!((bu.rest.rotation.dot(bs.rest.rotation) - 1.0).abs() < 1e-6, "{}: rest rotation", bs.name);
        // A rotation block is the same; the translation column is a length.
        let mut expect = ibu[i];
        expect.w_axis = (ibu[i].w_axis.truncate() * S).extend(1.0);
        assert!(near_mat(expect, ibs[i]), "{}: inverse bind {} vs {}", bs.name, ibu[i], ibs[i]);
    }
    assert_eq!(unit.rig.bind.rotations, scaled.rig.bind.rotations);
    assert!(near(unit.rig.bind.hips_offset * S, scaled.rig.bind.hips_offset));
    assert!(unit.rig.bind.hips_offset.length() > 0.0, "the test needs a hips offset to scale");
    assert!(scaled.rig.validate_bind().is_ok());
}

#[test]
fn anchors_springs_colliders_and_extra_bones_scale_but_physical_constants_do_not() {
    let (unit, scaled) = (full(1.0, false).unwrap(), full(S, false).unwrap());
    for (key, a) in &unit.anchors {
        let b = &scaled.anchors[key];
        assert!(near(a.position * S, b.position), "anchor {key:?}");
        assert!(a.rotation.dot(b.rotation) > 0.999_999, "anchor rotation");
    }
    let chain = |c: &BuiltCharacter, name: &str| c.rig.springs.chains.iter().find(|x| x.name == name).unwrap().clone();
    // The body's tassel is scaled with the body; its stiffness and gravity are physical and stay.
    let (tu, ts) = (chain(&unit, "body_tassel"), chain(&scaled, "body_tassel"));
    assert!((tu.joints[0].params.hit_radius * S - ts.joints[0].params.hit_radius).abs() < 1e-7);
    assert_eq!(tu.joints[0].params.stiffness, ts.joints[0].params.stiffness);
    assert_eq!(tu.joints[0].params.gravity_power, ts.joints[0].params.gravity_power);
    assert_eq!(tu.joints[0].params.gravity_dir, ts.joints[0].params.gravity_dir);
    // The hair is built after the scale, in metres: its 1 cm radius is what the author wrote, at any stature.
    assert_eq!(chain(&unit, "hair_lock").joints[0].params.hit_radius, 0.01);
    assert_eq!(chain(&scaled, "hair_lock").joints[0].params.hit_radius, 0.01);
    // Colliders: rig groups and published ones both.
    assert_eq!(unit.rig.springs.groups.len(), scaled.rig.springs.groups.len());
    for (gu, gs) in unit.rig.springs.groups.iter().zip(&scaled.rig.springs.groups) {
        for (cu, cs) in gu.colliders.iter().zip(&gs.colliders) {
            match (cu.shape, cs.shape) {
                (
                    ColliderShape::Sphere { offset: ou, radius: ru },
                    ColliderShape::Sphere { offset: os, radius: rs },
                ) => {
                    assert!(near(ou * S, os) && (ru * S - rs).abs() < 1e-7, "{}", gs.name);
                }
                (
                    ColliderShape::Capsule { offset: ou, radius: ru, tail: tu },
                    ColliderShape::Capsule { offset: os, radius: rs, tail: ts },
                ) => assert!(near(ou * S, os) && near(tu * S, ts) && (ru * S - rs).abs() < 1e-7, "{}", gs.name),
                other => panic!("the collider changed shape: {other:?}"),
            }
        }
    }
    assert_eq!(scaled.stature, Some(Stature { scale: S, height_m: 1.9 * S }));
    assert_eq!(unit.stature.map(|s| s.scale), Some(1.0));
}

#[test]
fn parts_built_after_the_body_see_metres_and_the_face_window_follows() {
    let (unit, scaled) = (full(1.0, false).unwrap(), full(S, false).unwrap());
    let note = |c: &BuiltCharacter| {
        let w = c.report.parts.iter().find(|p| p.part.as_str() == "hair").unwrap().warnings[0].clone();
        w.split([' ', '=']).filter_map(|t| t.parse::<f32>().ok()).collect::<Vec<_>>()
    };
    let (nu, ns) = (note(&unit), note(&scaled));
    assert_eq!(nu.len(), 2);
    for (u, s) in nu.iter().zip(&ns) {
        assert!((u * S - s).abs() < 1e-6, "the hair saw {s} m where {u} m scaled is {}", u * S);
    }
    // Both the head's own overlay and the hair's (built from the published projection) land where the scaled head is.
    let windows = |c: &BuiltCharacter| {
        ["stature-head-skin", "stature-hair-material"]
            .map(|m| c.materials[&MaterialName::new(m).unwrap()].overlays[0].window)
    };
    for (wu, ws) in windows(&unit).iter().zip(windows(&scaled)) {
        assert!((wu.min * S - ws.min).length() < 1e-6 && (wu.max * S - ws.max).length() < 1e-6);
        assert!((wu.fade_from * S - ws.fade_from).abs() < 1e-6 && (wu.fade_to * S - ws.fade_to).abs() < 1e-6);
    }
    // The painter still works in the template's head-local units, so the textures keep their pixel size.
    let face = |c: &BuiltCharacter| c.face.as_ref().map(|f| f.projection);
    assert_eq!(face(&unit), face(&scaled));
}

#[test]
fn a_scale_of_one_changes_nothing_and_a_body_without_stature_is_left_alone() {
    let def = character(&[("body", part("stature-body", 0, json!({}))), ("head", part("stature-head", 1, json!({})))]);
    let built = RegistryBuilder
        .build(&CharId::new("s").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    assert_eq!(built.meshes[0].mesh.geometry.positions[0], Vec3::new(0.45, 1.65, 0.05));
    assert_eq!(built.stature.map(|s| s.scale), Some(1.0));
}

// ---- refusals

#[test]
fn a_body_or_head_with_a_simulation_is_refused_because_it_cannot_be_rescaled() {
    let def = character(&[("body", part("stature-body", 0, json!({"scale": S, "simulate": true})))]);
    let err = RegistryBuilder
        .build(&CharId::new("s").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .map(|_| ())
        .unwrap_err();
    assert!(err.to_string().contains("cannot carry a simulation"), "{err}");
}

#[test]
fn an_unusable_stature_is_refused_naming_the_part() {
    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let def = character(&[("body", part("stature-body", 0, json!({"scale": scale})))]);
        let result = RegistryBuilder.build(
            &CharId::new("s").unwrap(),
            &def,
            &BuildOptions::default(),
            &BuildServices::default(),
        );
        // JSON cannot carry NaN or infinity, so those arrive as parameter errors; the others as part errors.
        assert!(result.is_err(), "scale {scale} must not build");
    }
}

#[test]
fn a_head_built_before_the_body_published_the_stature_is_refused() {
    let def = character(&[
        ("head", part("stature-head", 0, json!({}))),
        ("torso", part("stature-body", 1, json!({"scale": S}))),
    ]);
    // `stature-head` requires `stature-body`, so the builder orders them; drop the requirement by building by hand.
    let id = CharId::new("s").unwrap();
    let options = BuildOptions::default();
    let mut assembler = omuzaki_char_api::Assembler::new(&id, &def, &options);
    let services = BuildServices::default();
    let head = PartId::new("head").unwrap();
    let out = omuzaki_char_api::build_part(&def, &head, &options, &services, assembler.published()).unwrap();
    assembler.add(&head, out).unwrap();
    let body = PartId::new("torso").unwrap();
    let out = omuzaki_char_api::build_part(&def, &body, &options, &services, assembler.published()).unwrap();
    let err = assembler.add(&body, out).unwrap_err();
    assert!(err.to_string().contains("make the head part require the body"), "{err}");
}
