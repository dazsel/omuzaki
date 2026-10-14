//! Rig data from parts through the reference builder: the skeleton assembled from published joints, extra bones, spring
//! chains and colliders, every named error, and weight brushes (plan decision G2).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_anim_api::{
    Collider, ColliderGroup, ColliderShape, HumanoidBone, Pose, Skeleton, SkeletonError, SpringChain, SpringError,
    SpringParams, Transform,
};
use omuzaki_char_api::{
    BodyFit, BodyRegion, BoneName, BrushFalloff, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter,
    CharId, CharacterBuilder, CharacterDef, ExtraBone, JointFit, JointRef, MaterialName, MaterialSpec, MeshData,
    PartCategory, PartDef, PartId, PartKind, PartKindId, PartMesh, PartMeta, PartOutput, Publishes, RegistryBuilder,
    RigContribution, Section, WeightBrush, extra_id, humanoid_id, humanoid_of,
};
use omuzaki_core::glam::{Mat4, Vec2, Vec3};
use omuzaki_face_api::{Check, FaceProfile, Palette, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ---- a body that publishes the 15 required joints, with a torso section and a head collider

#[derive(Debug)]
struct Torso;

fn joint_at(bone: HumanoidBone) -> Option<Vec3> {
    use HumanoidBone as B;
    Some(match bone {
        B::Hips => Vec3::new(0.0, 1.0, 0.0),
        B::Spine => Vec3::new(0.0, 1.2, 0.0),
        B::Head => Vec3::new(0.0, 1.6, 0.0),
        B::LeftUpperLeg => Vec3::new(0.1, 0.95, 0.0),
        B::LeftLowerLeg => Vec3::new(0.1, 0.5, 0.0),
        B::LeftFoot => Vec3::new(0.1, 0.05, 0.0),
        B::RightUpperLeg => Vec3::new(-0.1, 0.95, 0.0),
        B::RightLowerLeg => Vec3::new(-0.1, 0.5, 0.0),
        B::RightFoot => Vec3::new(-0.1, 0.05, 0.0),
        B::LeftUpperArm => Vec3::new(0.2, 1.45, 0.0),
        B::LeftLowerArm => Vec3::new(0.45, 1.45, 0.0),
        B::LeftHand => Vec3::new(0.7, 1.45, 0.0),
        B::RightUpperArm => Vec3::new(-0.2, 1.45, 0.0),
        B::RightLowerArm => Vec3::new(-0.45, 1.45, 0.0),
        B::RightHand => Vec3::new(-0.7, 1.45, 0.0),
        _ => return None,
    })
}

impl BodyFit for Torso {
    fn section(&self, region: BodyRegion, _: f32) -> Option<Section> {
        (region == BodyRegion::Torso).then_some(Section {
            centre: Vec3::new(0.0, 1.2, 0.0),
            axis: Vec3::Y,
            radii: Vec2::splat(0.1),
        })
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        match joint {
            JointRef::Humanoid(b) => joint_at(*b),
            JointRef::Named(_) => None,
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct NoParams {}

impl Validate for NoParams {
    fn check(&self, _: &mut Check) {}
}

fn meta(id: &'static str, category: PartCategory, requires: &'static [&'static str]) -> PartMeta {
    PartMeta { id, summary: "A test kind.", category, requires }
}

struct RigBody;

impl PartKind for RigBody {
    type Params = NoParams;

    fn meta() -> PartMeta {
        meta("rig-body", PartCategory::Body, &[])
    }

    fn build(_: &NoParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = Collider {
            bone: humanoid_id(HumanoidBone::Head),
            shape: ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.12 },
        };
        Ok(PartOutput {
            publishes: Publishes {
                fit: Some(Arc::new(Torso)),
                colliders: vec![ColliderGroup { name: "body".to_owned(), colliders: vec![head] }],
                ..Publishes::default()
            },
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(RigBody);

// ---- a head that drops the head joint and adds the eyes and jaw, without wrapping anything

fn head_drop() -> PartOutput {
    let joints = [
        (HumanoidBone::Head, Vec3::new(0.0, 1.7, 0.0)),
        (HumanoidBone::LeftEye, Vec3::new(0.06, 1.72, 0.1)),
        (HumanoidBone::RightEye, Vec3::new(-0.06, 1.72, 0.1)),
        (HumanoidBone::Jaw, Vec3::new(0.0, 1.62, 0.05)),
    ];
    let fit = JointFit::new(joints.map(|(b, p)| (JointRef::Humanoid(b), p)));
    PartOutput { publishes: Publishes { fit: Some(Arc::new(fit)), ..Publishes::default() }, ..PartOutput::default() }
}

struct RigHead;

impl PartKind for RigHead {
    type Params = NoParams;

    fn meta() -> PartMeta {
        meta("rig-head", PartCategory::Head, &["rig-body"])
    }

    fn build(_: &NoParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        Ok(head_drop())
    }
}
omuzaki_char_api::register_part_kind!(RigHead);

/// The same head without `requires`: with the body named `torso` it builds first (same layer, ids break the tie).
struct RigLooseHead;

impl PartKind for RigLooseHead {
    type Params = NoParams;

    fn meta() -> PartMeta {
        meta("rig-loose-head", PartCategory::Head, &[])
    }

    fn build(_: &NoParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        Ok(head_drop())
    }
}
omuzaki_char_api::register_part_kind!(RigLooseHead);

/// A garment envelope that repeats the joints of the fit under it, as every real garment does.
#[derive(Debug)]
struct Coat {
    inner: Arc<dyn BodyFit>,
}

impl BodyFit for Coat {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        self.inner.section(region, t)
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        self.inner.joint(joint)
    }
}

struct RigCoat;

impl PartKind for RigCoat {
    type Params = NoParams;

    fn meta() -> PartMeta {
        meta("rig-coat", PartCategory::Garment, &["rig-body"])
    }

    fn build(_: &NoParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let envelope = Arc::new(Coat { inner: ctx.fit()? });
        Ok(PartOutput { publishes: Publishes { fit: Some(envelope), ..Publishes::default() }, ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(RigCoat);

/// Records where the head is as the rig sees it (`ctx.joint`) and as the outermost envelope reports it (`ctx.fit`).
struct RigReader;

impl PartKind for RigReader {
    type Params = NoParams;

    fn meta() -> PartMeta {
        meta("rig-reader", PartCategory::Accessory, &["rig-body"])
    }

    fn build(_: &NoParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let rig = ctx.joint(&JointRef::HEAD).map(|p| p.y);
        let envelope = ctx.fit()?.joint(&JointRef::HEAD).map(|p| p.y);
        Ok(PartOutput { warnings: vec![format!("rig={rig:?} envelope={envelope:?}")], ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(RigReader);

// ---- a garment that records what it can see

struct RigGarment;

impl PartKind for RigGarment {
    type Params = NoParams;

    fn meta() -> PartMeta {
        meta("rig-garment", PartCategory::Garment, &["rig-body"])
    }

    fn build(_: &NoParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let torso = ctx.fit()?.section(BodyRegion::Torso, 0.5).is_some();
        Ok(PartOutput { warnings: vec![format!("torso={torso}")], ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(RigGarment);

// ---- a part that contributes rig data, in good and bad ways

#[derive(Copy, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Good,
    ParentMissing,
    ParentListedLater,
    DuplicateBone,
    ClashesWithHumanoid,
    MeshBone,
    ChainBoneBeyondTheList,
    ChainOnAbsentHumanoid,
    ChainOutOfOrder,
    ChainGroupMissing,
    PublishedOnExtraBone,
    BadRest,
    TooManyBones,
    NonFiniteJoint,
    GroupNameClash,
    ChainNamedTwice,
}

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct ProbeParams {
    mode: Mode,
}

impl Validate for ProbeParams {
    fn check(&self, _: &mut Check) {}
}

fn bone(name: &str, parent: JointRef, at: Vec3) -> ExtraBone {
    ExtraBone { name: BoneName::new(name).unwrap(), parent, rest: Transform::from_translation(at) }
}

fn named(name: &str) -> JointRef {
    JointRef::Named(BoneName::new(name).unwrap())
}

fn chain(name: &str, joints: &[u16]) -> SpringChain {
    let bones: Vec<_> = joints.iter().map(|j| omuzaki_anim_api::BoneId(*j)).collect();
    SpringChain::new(name, &bones, SpringParams::default())
}

struct RigProbe;

impl PartKind for RigProbe {
    type Params = ProbeParams;

    fn meta() -> PartMeta {
        meta("rig-probe", PartCategory::Hair, &["rig-body"])
    }

    fn build(params: &ProbeParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let two = |c: &mut RigContribution| {
            c.add_bone(bone("probe_0", JointRef::HEAD, Vec3::new(0.0, 1.8, -0.2)));
            c.add_bone(bone("probe_1", named("probe_0"), Vec3::new(0.0, 1.7, -0.2)));
        };
        let mut out = PartOutput::default();
        let rig = &mut out.rig;
        let (first, second) = (extra_id(0).0, extra_id(1).0);
        match params.mode {
            Mode::Good => {
                two(rig);
                let mut c = chain("probe", &[first, second]);
                c.center = Some(humanoid_id(HumanoidBone::Head));
                c.collider_groups = ctx.colliders().iter().map(|g| g.name.clone()).collect();
                rig.chains.push(c);
                let seen: Vec<_> = ctx
                    .colliders()
                    .iter()
                    .flat_map(|g| g.colliders.iter().map(|c| humanoid_of(c.bone).map(HumanoidBone::name)))
                    .collect();
                out.warnings = vec![format!("rig_export={}", ctx.rig_export), format!("colliders={seen:?}")];
            }
            Mode::ParentMissing => rig.bones.push(bone("probe_0", named("nope"), Vec3::Y)),
            Mode::ParentListedLater => {
                rig.bones.push(bone("probe_0", named("probe_1"), Vec3::Y));
                rig.bones.push(bone("probe_1", JointRef::HEAD, Vec3::Y));
            }
            Mode::DuplicateBone => {
                two(rig);
                rig.bones.push(bone("probe_0", JointRef::HEAD, Vec3::Y));
            }
            Mode::ClashesWithHumanoid => rig.bones.push(bone("head", JointRef::HIPS, Vec3::Y)),
            Mode::MeshBone => {
                let geometry = MeshData {
                    positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
                    normals: vec![Vec3::Z; 3],
                    joints: vec![[0; 4]; 3],
                    weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
                    indices: vec![0, 1, 2],
                    ..MeshData::default()
                };
                out.meshes.push(PartMesh {
                    name: "strand".to_owned(),
                    geometry,
                    joint_bones: vec![named("ghost")],
                    material: MaterialName::new("rig-probe").unwrap(),
                    casts_shadow: false,
                    render_order: 0,
                    dynamic: false,
                });
                out.materials
                    .push((MaterialName::new("rig-probe").unwrap(), MaterialSpec::toon("#ffffff".parse().unwrap())));
            }
            Mode::ChainBoneBeyondTheList => {
                two(rig);
                rig.chains.push(chain("probe", &[first, extra_id(5).0]));
            }
            Mode::ChainOnAbsentHumanoid => {
                two(rig);
                rig.chains.push(chain("probe", &[first, humanoid_id(HumanoidBone::LeftEye).0]));
            }
            Mode::ChainOutOfOrder => {
                two(rig);
                rig.chains.push(chain("probe", &[second, first]));
            }
            Mode::ChainGroupMissing => {
                two(rig);
                let mut c = chain("probe", &[first, second]);
                c.collider_groups = vec!["nowhere".to_owned()];
                rig.chains.push(c);
            }
            Mode::BadRest => rig.bones.push(bone("probe_0", JointRef::HEAD, Vec3::NAN)),
            Mode::TooManyBones => {
                let mut parent = JointRef::HEAD;
                for i in 0..600 {
                    let name = format!("probe_{i}");
                    rig.bones.push(bone(&name, parent, Vec3::new(0.0, 1.8, 0.0)));
                    parent = named(&name);
                }
            }
            Mode::NonFiniteJoint => {
                let fit = JointFit::new([(JointRef::HEAD, Vec3::new(0.0, f32::INFINITY, 0.0))]);
                out.publishes.fit = Some(Arc::new(fit));
            }
            Mode::GroupNameClash => {
                let group = ColliderGroup { name: "body".to_owned(), colliders: Vec::new() };
                rig.collider_groups.push(group);
            }
            Mode::ChainNamedTwice => {
                two(rig);
                rig.chains.push(chain("probe", &[first, second]));
                rig.chains.push(chain("probe", &[first, second]));
            }
            Mode::PublishedOnExtraBone => {
                two(rig);
                let collider =
                    Collider { bone: extra_id(0), shape: ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.1 } };
                out.publishes.colliders.push(ColliderGroup { name: "mine".to_owned(), colliders: vec![collider] });
            }
        }
        Ok(out)
    }
}
omuzaki_char_api::register_part_kind!(RigProbe);

// ---- a skinned part for the brush tests

struct RigSkinned;

impl PartKind for RigSkinned {
    type Params = NoParams;

    fn meta() -> PartMeta {
        meta("rig-skinned", PartCategory::Accessory, &["rig-body"])
    }

    fn build(_: &NoParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let geometry = MeshData {
            positions: vec![Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.05, 1.0, 0.0), Vec3::new(5.0, 1.0, 0.0)],
            normals: vec![Vec3::Z; 3],
            joints: vec![[0; 4]; 3],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        };
        let material = MaterialName::new("rig-skinned").unwrap();
        let mesh = PartMesh {
            name: "skinned".to_owned(),
            geometry,
            joint_bones: vec![JointRef::HIPS],
            material: material.clone(),
            casts_shadow: false,
            render_order: 0,
            dynamic: false,
        };
        Ok(PartOutput {
            meshes: vec![mesh],
            materials: vec![(material, MaterialSpec::toon("#ffffff".parse().unwrap()))],
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(RigSkinned);

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

fn with_probe(mode: &str) -> CharacterDef {
    character(&[("body", part("rig-body", 0, json!({}))), ("probe", part("rig-probe", 0, json!({"mode": mode})))])
}

fn build_with(def: &CharacterDef, options: &BuildOptions) -> Result<BuiltCharacter, BuildError> {
    RegistryBuilder.build(&CharId::new("rig").unwrap(), def, options, &BuildServices::default())
}

fn build(def: &CharacterDef) -> Result<BuiltCharacter, BuildError> {
    build_with(def, &BuildOptions::default())
}

/// The parent-relative rest translation of the bone called `name`.
fn local(skeleton: &Skeleton, name: &str) -> Vec3 {
    skeleton.get(skeleton.find(name).unwrap()).unwrap().rest.translation
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a - b).length() < 1e-6
}

fn warnings(c: &BuiltCharacter, part: &str) -> Vec<String> {
    c.report.parts.iter().find(|p| p.part.as_str() == part).unwrap().warnings.clone()
}

// ---- the skeleton

#[test]
fn the_body_alone_gives_a_valid_humanoid_skeleton_with_identity_rest_rotations() {
    let c = build(&character(&[("body", part("rig-body", 0, json!({})))])).unwrap();
    let s = &c.rig.skeleton;
    assert_eq!(s.len(), 15, "exactly the bones VRM requires");
    assert!(s.bones().iter().all(|b| b.rest.rotation == omuzaki_core::glam::Quat::IDENTITY));
    // World minus parent world: the head hangs from the spine (no chest or neck), the legs from the hips.
    assert!(near(local(s, "hips"), Vec3::new(0.0, 1.0, 0.0)));
    assert!(near(local(s, "spine"), Vec3::new(0.0, 0.2, 0.0)));
    assert!(near(local(s, "head"), Vec3::new(0.0, 0.4, 0.0)));
    assert!(near(local(s, "leftUpperLeg"), Vec3::new(0.1, -0.05, 0.0)));
    assert!(near(local(s, "leftHand"), Vec3::new(0.25, 0.0, 0.0)));
    assert_eq!(c.rig.springs.groups.len(), 1, "the body's published collider is a rig collider");
    assert_eq!(c.rig.springs.chains, []);
}

#[test]
fn a_part_that_moves_joints_wins_over_the_body_and_garments_still_wrap_the_body() {
    let c = build(&character(&[
        ("body", part("rig-body", 0, json!({}))),
        ("head", part("rig-head", 0, json!({}))),
        ("shirt", part("rig-garment", 20, json!({}))),
    ]))
    .unwrap();
    let s = &c.rig.skeleton;
    assert_eq!(s.len(), 18, "the head part added the eyes and the jaw");
    assert!(near(local(s, "head"), Vec3::new(0.0, 0.5, 0.0)), "the head dropped from 1.6 to 1.7");
    assert_eq!(s.get(s.find("jaw").unwrap()).unwrap().parent, s.humanoid_bone(HumanoidBone::Head));
    assert!(near(local(s, "leftEye"), Vec3::new(0.06, 0.02, 0.1)));
    assert_eq!(warnings(&c, "shirt"), ["torso=true"], "a joint-only fit is not what garments wrap");
}

#[test]
fn extra_bones_are_placed_in_bind_space_and_parents_come_first() {
    let c = build(&with_probe("good")).unwrap();
    let s = &c.rig.skeleton;
    assert_eq!(s.len(), 17);
    let (p0, p1) = (s.find("probe_0").unwrap(), s.find("probe_1").unwrap());
    assert_eq!((p0.index(), p1.index()), (15, 16), "appended after the humanoid bones, in listed order");
    assert_eq!(s.get(p0).unwrap().parent, s.humanoid_bone(HumanoidBone::Head));
    assert_eq!(s.get(p1).unwrap().parent, Some(p0));
    assert!(near(local(s, "probe_0"), Vec3::new(0.0, 0.2, -0.2)), "1.8 minus the head at 1.6");
    assert!(near(local(s, "probe_1"), Vec3::new(0.0, -0.1, 0.0)));
}

#[test]
fn spring_chains_and_colliders_are_rebased_onto_the_final_bone_ids() {
    let c = build(&with_probe("good")).unwrap();
    let (s, springs) = (&c.rig.skeleton, &c.rig.springs);
    let chain = &springs.chains[0];
    let names: Vec<_> = chain.joints.iter().map(|j| s.get(j.bone).unwrap().name.as_str()).collect();
    assert_eq!(names, ["probe_0", "probe_1"]);
    assert_eq!(chain.center, s.humanoid_bone(HumanoidBone::Head));
    assert_eq!(chain.collider_groups, ["body"], "the chain used the body's group, found through ctx.colliders()");
    assert_eq!(springs.groups[0].colliders[0].bone, s.humanoid_bone(HumanoidBone::Head).unwrap());
    springs.validate(s).unwrap();
    assert_eq!(warnings(&c, "probe")[1], r#"colliders=[Some("head")]"#, "later parts read published colliders");
}

#[test]
fn at_rest_every_skinning_matrix_is_the_identity_and_two_builds_agree() {
    let def = character(&[
        ("body", part("rig-body", 0, json!({}))),
        ("head", part("rig-head", 0, json!({}))),
        ("probe", part("rig-probe", 0, json!({}))),
    ]);
    let (a, b) = (build(&def).unwrap(), build(&def).unwrap());
    assert_eq!(a.rig, b.rig, "the skeleton and springs are identical across builds (decision 0009)");
    let s = &a.rig.skeleton;
    let (inverse_bind, mut world) = (s.inverse_bind(), Pose::IDENTITY);
    let mut skin = vec![Mat4::ZERO; s.len()];
    s.skin_matrices(&s.rest_pose(), &inverse_bind, &mut world, &mut skin);
    assert!(skin.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-6)), "no pose, no skinning");
}

#[test]
fn the_export_flag_reaches_the_parts() {
    let def = with_probe("good");
    assert_eq!(warnings(&build(&def).unwrap(), "probe")[0], "rig_export=false");
    let on = BuildOptions { rig_export: true, ..BuildOptions::default() };
    assert_eq!(warnings(&build_with(&def, &on).unwrap(), "probe")[0], "rig_export=true");
}

// ---- the named errors

#[test]
fn a_character_without_a_body_has_no_skeleton() {
    let e = build(&character(&[])).unwrap_err();
    assert_eq!(e, BuildError::Skeleton(SkeletonError::MissingRequired(HumanoidBone::Hips)));
    assert!(e.to_string().contains("BodyFit::joint"), "{e}");
}

#[test]
fn an_extra_bone_needs_a_parent_that_exists_and_is_listed_earlier() {
    let part_id = PartId::new("probe").unwrap();
    let name = |s: &str| BoneName::new(s).unwrap();
    assert_eq!(
        build(&with_probe("parent_missing")).unwrap_err(),
        BuildError::MissingParentBone { part: part_id.clone(), bone: name("probe_0"), parent: named("nope") }
    );
    assert_eq!(
        build(&with_probe("parent_listed_later")).unwrap_err(),
        BuildError::MissingParentBone { part: part_id, bone: name("probe_0"), parent: named("probe_1") },
        "a parent must come first, even when it is listed later"
    );
}

#[test]
fn bone_names_are_unique_across_the_skeleton() {
    let part_id = PartId::new("probe").unwrap();
    let expect = |mode: &str, bone: &str| {
        assert_eq!(
            build(&with_probe(mode)).unwrap_err(),
            BuildError::DuplicateBone { part: part_id.clone(), bone: BoneName::new(bone).unwrap() }
        );
    };
    expect("duplicate_bone", "probe_0");
    expect("clashes_with_humanoid", "head");
}

#[test]
fn a_mesh_skinned_to_a_missing_bone_is_an_error() {
    let e = build(&with_probe("mesh_bone")).unwrap_err();
    assert_eq!(
        e,
        BuildError::UnknownMeshBone {
            part: PartId::new("probe").unwrap(),
            mesh: "strand".to_owned(),
            bone: named("ghost")
        }
    );
    assert!(e.to_string().contains("'ghost'"), "{e}");
}

#[test]
fn a_chain_naming_an_absent_bone_is_an_error() {
    for (mode, what) in [
        ("chain_bone_beyond_the_list", "joint 1 of chain 'probe'"),
        ("chain_on_absent_humanoid", "joint 1 of chain 'probe'"),
    ] {
        assert_eq!(
            build(&with_probe(mode)).unwrap_err(),
            BuildError::UnknownRigBone { part: PartId::new("probe").unwrap(), what: what.to_owned() },
            "{mode}"
        );
    }
}

#[test]
fn the_spring_set_is_validated_against_the_skeleton() {
    let e = build(&with_probe("chain_out_of_order")).unwrap_err();
    assert!(
        matches!(&e, BuildError::Springs { part, source: SpringError::NotAChain { .. } } if part.as_str() == "probe"),
        "{e}"
    );
    let e = build(&with_probe("chain_group_missing")).unwrap_err();
    assert!(
        matches!(&e, BuildError::Springs { part, source: SpringError::NoSuchGroup { group, .. } } if part.as_str() == "probe" && group == "nowhere"),
        "{e}"
    );
    assert!(e.to_string().contains("part 'probe'"), "the message names the part: {e}");
}

#[test]
fn published_colliders_must_sit_on_bones_other_parts_can_locate() {
    let e = build(&with_probe("published_on_extra_bone")).unwrap_err();
    assert!(
        matches!(e, BuildError::Part { ref part, ref message } if part.as_str() == "probe" && message.contains("'mine'")),
        "{e}"
    );
}

#[test]
fn errors_about_a_bone_name_the_part_that_made_it() {
    let probe = PartId::new("probe").unwrap();
    for (mode, needle) in [
        ("bad_rest", "extra bone 'probe_0' has a rest transform"),
        ("too_many_bones", "would be bone 513 of at most 512"),
        ("non_finite_joint", "published joint 'head' is not a finite position"),
    ] {
        let e = build(&with_probe(mode)).unwrap_err();
        assert!(
            matches!(&e, BuildError::Part { part, message } if *part == probe && message.contains(needle)),
            "{mode}: {e}"
        );
        assert!(e.to_string().contains("part 'probe'"), "{mode}: {e}");
    }
}

#[test]
fn a_repeated_chain_or_group_name_says_which_part_used_it_first() {
    let e = build(&with_probe("group_name_clash")).unwrap_err();
    assert_eq!(
        e,
        BuildError::DuplicateSpringName {
            part: PartId::new("probe").unwrap(),
            earlier: PartId::new("body").unwrap(),
            kind: "collider group",
            name: "body".to_owned(),
        }
    );
    let e = build(&with_probe("chain_named_twice")).unwrap_err();
    assert!(matches!(&e, BuildError::DuplicateSpringName { kind: "spring chain", name, .. } if name == "probe"), "{e}");
    assert!(e.to_string().contains("`<kind>_<part>`"), "{e}");
}

// ---- the head drop must survive every build order

/// `head` local translation from the spine when the head part dropped it (1.7 minus 1.2), against 0.4 undropped.
const DROPPED_HEAD_Y: f32 = 0.5;

#[test]
fn the_head_drop_survives_a_garment_envelope_that_repeats_the_joints() {
    let c = build(&character(&[
        ("body", part("rig-body", 0, json!({}))),
        ("head", part("rig-head", 0, json!({}))),
        ("coat", part("rig-coat", 20, json!({}))),
    ]))
    .unwrap();
    let head = local(&c.rig.skeleton, "head");
    assert!((head.y - DROPPED_HEAD_Y).abs() < 1e-6, "{head:?}");
    assert!(c.rig.skeleton.find("jaw").is_some(), "and the joints only the head part knows are still there");
}

#[test]
fn the_head_drop_survives_a_head_that_builds_before_the_body() {
    let def = character(&[
        ("torso", part("rig-body", 0, json!({}))),
        ("head", part("rig-loose-head", 0, json!({}))),
        ("coat", part("rig-coat", 20, json!({}))),
    ]);
    let c = build(&def).unwrap();
    let order: Vec<_> = c.report.parts.iter().map(|p| p.part.to_string()).collect();
    assert_eq!(order, ["head", "torso", "coat"], "the head really did build first");
    let head = local(&c.rig.skeleton, "head");
    assert!((head.y - DROPPED_HEAD_Y).abs() < 1e-6, "{head:?}");
}

#[test]
fn ctx_joint_is_the_rigs_joint_and_the_envelope_is_the_undropped_body() {
    let c = build(&character(&[
        ("body", part("rig-body", 0, json!({}))),
        ("head", part("rig-head", 0, json!({}))),
        ("coat", part("rig-coat", 20, json!({}))),
        ("reader", part("rig-reader", 30, json!({}))),
    ]))
    .unwrap();
    assert_eq!(warnings(&c, "reader"), ["rig=Some(1.7) envelope=Some(1.6)"]);
}

// ---- weight brushes

fn brush(centre: Vec3, radius: f64, strength: f64) -> WeightBrush {
    WeightBrush {
        centre: centre.to_array().map(f64::from),
        radius,
        joint: JointRef::HEAD,
        strength,
        falloff: BrushFalloff::Constant,
    }
}

fn skinned(brushes: Vec<WeightBrush>) -> CharacterDef {
    let mut def = character(&[("body", part("rig-body", 0, json!({}))), ("arm", part("rig-skinned", 0, json!({})))]);
    def.weight_brushes.insert(PartId::new("arm").unwrap(), brushes);
    def
}

#[test]
fn a_brush_moves_half_of_a_vertexs_weight_to_the_named_joint() {
    let c = build(&skinned(vec![brush(Vec3::new(0.0, 1.0, 0.0), 0.1, 0.5)])).unwrap();
    let mesh = &c.meshes[0].mesh;
    let head = mesh.joint_bones.iter().position(|j| *j == JointRef::HEAD).expect("the brush's joint was added");
    for v in 0..2 {
        let (joints, weights) = (mesh.geometry.joints[v], mesh.geometry.weights[v]);
        let on = |joint: usize| -> f32 {
            joints.iter().zip(weights).filter(|(j, _)| usize::from(**j) == joint).map(|(_, w)| w).sum()
        };
        assert!((on(head) - 0.5).abs() < 1e-6 && (on(0) - 0.5).abs() < 1e-6, "vertex {v}: {weights:?}");
        assert!((weights.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(weights.iter().filter(|w| **w > 0.0).count() <= 4);
    }
    assert_eq!(mesh.geometry.weights[2], [1.0, 0.0, 0.0, 0.0], "the far vertex is out of reach");
    assert!(c.rig.bone(&JointRef::HEAD).is_some(), "and the rig has the joint the brush uses");
}

#[test]
fn brushes_that_name_a_missing_part_or_carry_bad_numbers_fail_before_building() {
    let mut def = skinned(vec![]);
    def.weight_brushes.insert(PartId::new("ghost").unwrap(), vec![brush(Vec3::ZERO, 0.1, 0.5)]);
    let e = build(&def).unwrap_err();
    assert!(
        matches!(&e, BuildError::Brush { part, errors } if part.as_str() == "ghost" && errors.0[0].path == "weight_brushes/ghost"),
        "{e}"
    );

    let e = build(&skinned(vec![brush(Vec3::ZERO, 0.1, 0.5), brush(Vec3::ZERO, -1.0, 2.0)])).unwrap_err();
    let BuildError::Brush { part, errors } = e else { panic!("{e}") };
    assert_eq!(part.as_str(), "arm");
    let paths: Vec<_> = errors.0.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(paths, ["1/radius", "1/strength"]);
}

#[test]
fn a_brush_for_a_joint_the_rig_lacks_is_the_same_unknown_bone_error_as_any_mesh() {
    let mut b = brush(Vec3::new(0.0, 1.0, 0.0), 0.1, 0.5);
    b.joint = JointRef::Humanoid(HumanoidBone::LeftEye);
    let e = build(&skinned(vec![b])).unwrap_err();
    assert!(matches!(e, BuildError::UnknownMeshBone { ref mesh, .. } if mesh == "skinned"), "{e}");
}
