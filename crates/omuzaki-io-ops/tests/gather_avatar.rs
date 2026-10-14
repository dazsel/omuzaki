//! The VRM avatar block and `io.check_export`, against the mock character and the mock VRM format.
//!
//! The mock exporter writes the humanoid, the meta, morph-bind expressions, look-at, first-person and spring bones,
//! so the round trip below checks the file the mock itself reads back.
//!
//! `check_export_answers_for_a_good_and_a_broken_character` asserts, and also prints, `io.check_export` for a good
//! and a broken character (run it with `--nocapture`); the PR quotes it.

// Integration-test helpers may panic: a panic is a clear test failure. The casts build values from small indices.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::collections::BTreeSet;

use omuzaki_anim_api::{
    BoneDef, BoneId, Collider, ColliderGroup, ColliderShape, HumanoidBone, HumanoidPose, Skeleton, SpringChain,
    SpringJoint, SpringParams, SpringSet, Transform,
};
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, CharacterRig, PartId,
    RegistryBuilder,
};
use omuzaki_char_mock::{mock_character, mock_variants};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    self as io, ARKIT_52, BakeRequest, ExportCtx, ExportOptions, ExpressionBaker, ExpressionName, FirstPersonFlag,
    ImageIx, ImportCtx, ImportInput, ImportOptions, IoScene, NoProgress, Purpose, Severity, SpringBones, UseTerms,
    VRM_LICENSE_URLS, VrmMeta, VrmVersion, preflight, run_export, run_import, vrm_profile,
};
use omuzaki_io_mock::fixtures::{authored_provenance, options_without_face_channels};
use omuzaki_io_mock::{MockBaker, MockVrm1Exporter, MockVrm1Importer};
use omuzaki_io_ops::convert::bones::to_file;
use omuzaki_io_ops::gather::avatar::{LicenceRecord, RecordError, add_avatar, add_rights, record_path};
use omuzaki_io_ops::gather::skeleton::bone_node;
use omuzaki_io_ops::gather::springs;
use omuzaki_io_ops::gather::{GatherOptions, GatherPurpose, Gathered, gather};
use omuzaki_io_ops::ops::check_export::{CheckExport, ExportCheck};
use omuzaki_io_ops::ops::licence::source::PartSource;
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, OpKind, Scope, find};
use omuzaki_store_api::{Commercial, Consent, Provenance, licence_gate};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------------------------

fn mock_def() -> CharacterDef {
    mock_character().unwrap()
}

fn build(def: &CharacterDef) -> BuiltCharacter {
    RegistryBuilder
        .build(&CharId::new("mira").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

/// The mock character: 17 humanoid bones (no eyes), three hair bones on one spring chain, a head-and-neck collider
/// group, and a head, hair, body and two garment parts.
fn mock_built() -> BuiltCharacter {
    build(&mock_def())
}

/// Where the rich character's eyes sit in the head bone's frame. The head is turned, so this is not a world offset.
const LEFT_EYE: Vec3 = Vec3::new(0.03, 0.06, 0.08);
const RIGHT_EYE: Vec3 = Vec3::new(-0.03, 0.06, 0.08);

fn rest(bone: HumanoidBone) -> Transform {
    match bone {
        HumanoidBone::Head => Transform { translation: Vec3::new(0.0, 0.1, 0.0), rotation: Quat::from_rotation_y(0.7) },
        HumanoidBone::LeftEye => Transform::from_translation(LEFT_EYE),
        HumanoidBone::RightEye => Transform::from_translation(RIGHT_EYE),
        other => Transform::from_translation(Vec3::new(0.01 * (other.index() % 5) as f32, 0.05, 0.002)),
    }
}

fn params(stiffness: f32, gravity_power: f32, gravity_dir: Vec3, drag_force: f32, hit_radius: f32) -> SpringParams {
    SpringParams { hit_radius, stiffness, gravity_power, gravity_dir, drag_force }
}

/// The mock's meshes on a skeleton with all 55 humanoid bones (eyes included) and two spring chains over extra bones
/// whose parameters all differ: hair under the head (colliding with both groups, simulated about the head) and a tail
/// under the hips (tilted gravity, no groups, no centre).
fn rich_built() -> BuiltCharacter {
    rich_with(rest)
}

/// [`rich_built`] with the humanoid bones resting as `rest_of` says.
fn rich_with(rest_of: impl Fn(HumanoidBone) -> Transform) -> BuiltCharacter {
    let humanoid = Skeleton::humanoid(&HumanoidBone::ALL, rest_of).unwrap();
    let bone = |b| humanoid.humanoid_bone(b).unwrap();
    let (hips, head, neck, spine) =
        (bone(HumanoidBone::Hips), bone(HumanoidBone::Head), bone(HumanoidBone::Neck), bone(HumanoidBone::Spine));
    let mut defs = humanoid.bones().to_vec();
    let mut add = |name: &str, parent: BoneId, y: f32| {
        let rest = Transform::from_translation(Vec3::new(0.0, y, 0.02));
        defs.push(BoneDef { name: name.to_owned(), parent: Some(parent), rest, humanoid: None });
        BoneId((defs.len() - 1) as u16)
    };
    let hair = [add("hair_a_0", head, 0.1)];
    let hair = [hair[0], add("hair_a_1", hair[0], -0.05)];
    let hair = [hair[0], hair[1], add("hair_a_2", hair[1], -0.06)];
    let tail = [add("tail_0", hips, -0.1)];
    let tail = [tail[0], add("tail_1", tail[0], -0.12)];
    let skeleton = Skeleton::new(defs).unwrap();

    let sphere =
        |bone, y, radius| Collider { bone, shape: ColliderShape::Sphere { offset: Vec3::new(0.0, y, 0.01), radius } };
    let capsule = Collider {
        bone: neck,
        shape: ColliderShape::Capsule {
            offset: Vec3::new(0.0, -0.04, 0.0),
            radius: 0.05,
            tail: Vec3::new(0.0, 0.02, 0.01),
        },
    };
    let joint = |bone, params| SpringJoint { bone, params };
    let springs = SpringSet {
        groups: vec![
            ColliderGroup { name: "head".into(), colliders: vec![sphere(head, 0.2, 0.19), capsule] },
            ColliderGroup { name: "body".into(), colliders: vec![sphere(spine, 0.0, 0.12)] },
        ],
        chains: vec![
            SpringChain {
                name: "hair_a".into(),
                joints: vec![
                    joint(hair[0], params(1.0, 0.1, Vec3::NEG_Y, 0.5, 0.01)),
                    joint(hair[1], params(0.6, 0.2, Vec3::NEG_Y, 0.4, 0.015)),
                    joint(hair[2], params(0.2, 0.3, Vec3::NEG_Y, 0.3, 0.02)),
                ],
                collider_groups: vec!["head".into(), "body".into()],
                center: Some(head),
            },
            SpringChain {
                name: "tail".into(),
                joints: vec![
                    joint(tail[0], params(0.8, 0.5, Vec3::new(0.0, -1.0, 0.5).normalize(), 0.25, 0.03)),
                    joint(tail[1], params(0.1, 0.9, Vec3::new(0.0, -1.0, 0.5).normalize(), 0.1, 0.05)),
                ],
                collider_groups: vec![],
                center: None,
            },
        ],
    };
    springs.validate(&skeleton).unwrap();
    let mut built = mock_built();
    built.rig = CharacterRig { skeleton, springs, drivers: Vec::new(), bind: HumanoidPose::REST };
    built
}

/// A creature: a spine and a tail, no humanoid roles.
fn creature_built() -> BuiltCharacter {
    let bone = |name: &str, parent: Option<u16>| BoneDef {
        name: name.to_owned(),
        parent: parent.map(BoneId),
        rest: Transform::from_translation(Vec3::new(0.0, 0.2, 0.0)),
        humanoid: None,
    };
    let mut built = mock_built();
    built.rig = CharacterRig {
        skeleton: Skeleton::generic(vec![bone("body", None), bone("tail", Some(0))]).unwrap(),
        springs: SpringSet::default(),
        drivers: Vec::new(),
        bind: HumanoidPose::REST,
    };
    built.meshes.clear();
    built
}

fn record(commercial: Commercial) -> LicenceRecord {
    let provenance = Provenance { commercial, ..authored_provenance() };
    LicenceRecord::new(provenance, VrmMeta::new("Mira", vec!["Test Author".into()], VRM_LICENSE_URLS[0]))
}

fn clear_record() -> LicenceRecord {
    record(Commercial::Ok)
}

fn vrm_gathered(built: &BuiltCharacter, record: &LicenceRecord, target_height: Option<f32>) -> Gathered {
    let mut gathered = gather(built, &GatherOptions { target_height, purpose: GatherPurpose::Vrm }).unwrap();
    add_rights(&mut gathered.scene, built, Some(record));
    add_avatar(&mut gathered, built).unwrap();
    gathered
}

fn export_vrm(scene: &IoScene) -> Result<io::ExportOutcome, io::IoError> {
    run_export(&MockVrm1Exporter, scene, &ExportCtx::new(&options_without_face_channels()))
}

fn import_vrm(bytes: &[u8]) -> IoScene {
    let options = ImportOptions::default();
    run_import(&MockVrm1Importer, &ImportInput::new(bytes), &ImportCtx::new(&options)).unwrap().scene
}

fn errors(report: &io::Report) -> Vec<&str> {
    report.errors().map(|f| f.rule.as_str()).collect()
}

fn avatar_of(gathered: &Gathered) -> &io::Avatar {
    gathered.scene.avatar.as_ref().unwrap()
}

// ---------------------------------------------------------------------------------------------------------------
// The avatar block
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn every_humanoid_bone_of_the_skeleton_is_in_the_avatar_map() {
    for (built, expected) in [(mock_built(), 17), (rich_built(), 55)] {
        let gathered = vrm_gathered(&built, &clear_record(), None);
        let skeleton = &built.rig.skeleton;
        let humanoid = &avatar_of(&gathered).humanoid;
        let present: Vec<_> = HumanoidBone::ALL.into_iter().filter(|b| skeleton.humanoid_bone(*b).is_some()).collect();
        assert_eq!((present.len(), humanoid.bones.len()), (expected, expected));
        for role in present {
            let node = humanoid.node(to_file(role).unwrap().as_str()).unwrap();
            assert_eq!(node, bone_node(skeleton.humanoid_bone(role).unwrap()), "{role}: bone i is node i");
            assert_eq!(gathered.scene.nodes[node.index()].name, role.name());
        }
    }
}

#[test]
fn the_finished_scene_passes_every_check_and_the_mock_exporter_keeps_the_humanoid_and_the_meta() {
    for built in [mock_built(), rich_built()] {
        let gathered = vrm_gathered(&built, &clear_record(), None);
        let scene = &gathered.scene;
        assert_eq!(errors(&scene.check()), Vec::<&str>::new());
        assert_eq!(errors(&vrm_profile(scene, VrmVersion::V1)), Vec::<&str>::new());
        assert_eq!(errors(&preflight(scene, &options_without_face_channels())), Vec::<&str>::new());

        let outcome = export_vrm(scene).unwrap();
        // The mock writes spring bones, look-at and first-person data, so nothing is reported as dropped.
        assert!(!outcome.report.has_rule("vrm.mock_dropped"));

        let back = import_vrm(&outcome.files[0].bytes);
        assert_eq!(back.avatar.as_ref().unwrap().humanoid, avatar_of(&gathered).humanoid);
        assert_eq!(
            (&back.meta.name, &back.meta.authors, &back.meta.license_url),
            (&scene.meta.name, &scene.meta.authors, &scene.meta.license_url)
        );
        assert_eq!(back.meta.terms, scene.meta.terms);
    }
}

#[test]
fn head_and_hair_meshes_are_hidden_in_first_person_and_the_rest_are_not() {
    let built = mock_built();
    let gathered = vrm_gathered(&built, &clear_record(), None);
    let scene = &gathered.scene;
    let flags: Vec<(&str, FirstPersonFlag)> = avatar_of(&gathered)
        .first_person
        .as_ref()
        .unwrap()
        .annotations
        .iter()
        .map(|a| (scene.nodes[a.node.index()].name.as_str(), a.flag))
        .collect();
    let hidden = FirstPersonFlag::ThirdPersonOnly;
    let shown = FirstPersonFlag::Both;
    assert_eq!(
        flags,
        [
            ("body.torso", shown),
            ("head.head", hidden),
            ("hair.hair-dynamic", hidden),
            ("shirt.shirt", shown),
            ("vest.vest", shown)
        ]
    );
    assert!(
        avatar_of(&gathered)
            .first_person
            .as_ref()
            .unwrap()
            .annotations
            .iter()
            .all(|a| scene.nodes[a.node.index()].mesh.is_some())
    );

    // A mesh whose part the character doesn't have is left to the viewer's default rather than guessed.
    let mut orphan = mock_built();
    orphan.meshes[0].part = PartId::new("ghost").unwrap();
    let gathered = vrm_gathered(&orphan, &clear_record(), None);
    assert_eq!(avatar_of(&gathered).first_person.as_ref().unwrap().annotations.len(), 4);
}

#[test]
fn the_look_at_offset_is_the_point_between_the_eyes_in_the_head_frame_and_scales_with_the_scene() {
    let built = rich_built();
    for target in [None, Some(1.7)] {
        let gathered = vrm_gathered(&built, &clear_record(), target);
        assert_eq!(target.is_some(), (gathered.scale - 1.0).abs() > 1e-3);
        let look = avatar_of(&gathered).look_at.unwrap();
        let expected = (LEFT_EYE + RIGHT_EYE) * 0.5 * gathered.scale;
        assert!(
            (look.offset_from_head_bone - expected).length() < 1e-5,
            "{:?} vs {expected:?}",
            look.offset_from_head_bone
        );
        assert_eq!(look.kind, io::LookAtKind::Bone);
    }
    // Without eye bones there is no look-at block, and the report says why.
    let gathered = vrm_gathered(&mock_built(), &clear_record(), None);
    assert!(avatar_of(&gathered).look_at.is_none());
    let finding = gathered.report.findings.iter().find(|f| f.rule == "gather.look_at_missing").unwrap();
    assert!(finding.hint.is_some() && finding.severity == Severity::Info);
}

#[test]
fn expression_placeholders_are_the_presets_and_the_baker_fills_around_them() {
    let built = mock_built();
    let mut gathered = vrm_gathered(&built, &clear_record(), None);
    let presets = &avatar_of(&gathered).expressions;
    assert_eq!(presets.len(), 18);
    assert!(presets.iter().all(|(name, e)| matches!(name, ExpressionName::Preset(_)) && e.is_inert()));

    let head = gathered.scene.nodes.iter().position(|n| n.name == "head.head").map(|i| io::NodeIx(i as u32)).unwrap();
    let channels = ARKIT_52.iter().map(|n| ExpressionName::parse(n).unwrap()).collect();
    MockBaker.bake(&mut gathered.scene, &BakeRequest::new(channels, head), &NoProgress).unwrap();
    assert_eq!(avatar_of(&gathered).expressions.len(), 18 + 52);
    assert!(avatar_of(&gathered).humanoid.bones.len() == 17, "baking keeps the avatar block");

    // With the channels baked, the default options (perfect sync on) export, and the file keeps all of them.
    let outcome = run_export(&MockVrm1Exporter, &gathered.scene, &ExportCtx::new(&ExportOptions::default())).unwrap();
    assert_eq!(import_vrm(&outcome.files[0].bytes).avatar.unwrap().expressions.len(), 70);
}

// ---------------------------------------------------------------------------------------------------------------
// Spring bones
// ---------------------------------------------------------------------------------------------------------------

/// The runtime set a file's spring bones mean: the inverse of `springs::to_io`, written independently of it.
fn set_from_io(sb: &SpringBones) -> SpringSet {
    let bone = |node: io::NodeIx| BoneId(node.0 as u16);
    let shape = |s: io::ColliderShape| match s {
        io::ColliderShape::Sphere { offset, radius } => ColliderShape::Sphere { offset, radius },
        io::ColliderShape::Capsule { offset, radius, tail } => ColliderShape::Capsule { offset, radius, tail },
    };
    SpringSet {
        groups: sb
            .collider_groups
            .iter()
            .map(|g| ColliderGroup {
                name: g.name.clone(),
                colliders: g
                    .colliders
                    .iter()
                    .map(|&i| {
                        let c = sb.colliders[i as usize];
                        Collider { bone: bone(c.node), shape: shape(c.shape) }
                    })
                    .collect(),
            })
            .collect(),
        chains: sb
            .chains
            .iter()
            .map(|c| SpringChain {
                name: c.name.clone(),
                joints: c
                    .joints
                    .iter()
                    .map(|j| SpringJoint {
                        bone: bone(j.node),
                        params: params(j.stiffness, j.gravity_power, j.gravity_dir, j.drag_force, j.hit_radius),
                    })
                    .collect(),
                collider_groups: c
                    .collider_groups
                    .iter()
                    .map(|&i| sb.collider_groups[i as usize].name.clone())
                    .collect(),
                center: c.center.map(bone),
            })
            .collect(),
    }
}

#[test]
fn spring_chains_keep_their_parameters_through_the_file_structures() {
    for built in [mock_built(), rich_built()] {
        let set = &built.rig.springs;
        let file = springs::to_io(set, 1.0);
        assert_eq!(set_from_io(&file), *set, "set -> file structures -> set");
        assert_eq!((file.chains.len(), file.collider_groups.len()), (set.chains.len(), set.groups.len()));
        assert_eq!(file.colliders.len(), set.groups.iter().map(|g| g.colliders.len()).sum::<usize>());
    }
    // The rich set has everything the file can say: two shapes, two groups, a centre, per-joint parameters.
    let file = springs::to_io(&rich_built().rig.springs, 1.0);
    assert_eq!(
        (file.colliders.len(), file.chains[0].collider_groups.clone(), file.chains[1].center),
        (3, vec![0, 1], None)
    );
    assert_eq!(file.chains[1].joints[1].gravity_dir, Vec3::new(0.0, -1.0, 0.5).normalize());
}

#[test]
fn only_lengths_follow_the_scale() {
    let set = rich_built().rig.springs;
    let scale = 0.37_f32;
    let (base, scaled) = (springs::to_io(&set, 1.0), springs::to_io(&set, scale));
    for (a, b) in base.chains.iter().flat_map(|c| &c.joints).zip(scaled.chains.iter().flat_map(|c| &c.joints)) {
        assert_eq!(b.hit_radius, a.hit_radius * scale);
        assert_eq!(
            (b.stiffness, b.gravity_power, b.gravity_dir, b.drag_force),
            (a.stiffness, a.gravity_power, a.gravity_dir, a.drag_force)
        );
    }
    for (a, b) in base.colliders.iter().zip(&scaled.colliders) {
        let want = match a.shape {
            io::ColliderShape::Sphere { offset, radius } => {
                io::ColliderShape::Sphere { offset: offset * scale, radius: radius * scale }
            }
            io::ColliderShape::Capsule { offset, radius, tail } => {
                io::ColliderShape::Capsule { offset: offset * scale, radius: radius * scale, tail: tail * scale }
            }
        };
        assert_eq!((a.node, b.shape), (b.node, want));
    }
    // And the gather applies its own factor: the scene's springs are the set's at that scale.
    let mut built = rich_built();
    built.rig.springs = set.clone();
    let gathered = vrm_gathered(&built, &clear_record(), Some(1.2));
    assert_eq!(avatar_of(&gathered).spring_bones, springs::to_io(&set, gathered.scale));
}

#[test]
fn spring_bones_look_at_and_first_person_round_trip_through_the_mock_exporter_and_importer() {
    let built = rich_built();
    let gathered = vrm_gathered(&built, &clear_record(), Some(1.5));
    let want = avatar_of(&gathered);

    let outcome = export_vrm(&gathered.scene).unwrap();
    let back = import_vrm(&outcome.files[0].bytes);
    let carried = back.avatar.as_ref().unwrap();
    assert_eq!(carried.spring_bones, want.spring_bones, "every parameter, index and name survives the file");
    assert_eq!(carried.look_at, want.look_at);
    assert_eq!(carried.first_person, want.first_person);
    // Back at the runtime's side, the set is the built one at the gather's scale.
    let set = &built.rig.springs;
    let rebuilt = set_from_io(&carried.spring_bones);
    assert_eq!(rebuilt.groups.len(), set.groups.len());
    for (a, b) in set.chains.iter().zip(&rebuilt.chains) {
        assert_eq!((&a.name, &a.collider_groups, a.center), (&b.name, &b.collider_groups, b.center));
        for (x, y) in a.joints.iter().zip(&b.joints) {
            assert_eq!(
                (x.bone, x.params.stiffness, x.params.gravity_dir, x.params.drag_force),
                (y.bone, y.params.stiffness, y.params.gravity_dir, y.params.drag_force)
            );
            assert!((x.params.hit_radius * gathered.scale - y.params.hit_radius).abs() < 1e-7);
        }
    }
}

#[test]
fn the_avatar_block_is_deterministic() {
    let built = rich_built();
    let (a, b) = (vrm_gathered(&built, &clear_record(), Some(1.5)), vrm_gathered(&built, &clear_record(), Some(1.5)));
    assert_eq!(a, b);
    assert_eq!(export_vrm(&a.scene).unwrap().files, export_vrm(&b.scene).unwrap().files);
}

// ---------------------------------------------------------------------------------------------------------------
// Rights
// ---------------------------------------------------------------------------------------------------------------

fn rights_of(built: &BuiltCharacter, record: Option<&LicenceRecord>) -> IoScene {
    let mut scene = IoScene::new();
    add_rights(&mut scene, built, record);
    scene
}

#[test]
fn the_meta_is_the_records_and_nothing_is_invented() {
    let built = mock_built();
    let mut bare = clear_record();
    bare.provenance.author = None;
    bare.meta = VrmMeta::default();
    let scene = rights_of(&built, Some(&bare));
    // Only the character's own name fills in; no author, no licence URL, the most restrictive terms.
    assert_eq!(scene.meta, VrmMeta::new("Mock", vec![], ""));
    assert_eq!(scene.meta.terms, UseTerms::default());
    assert_eq!(scene.provenance, Some(bare.provenance.clone()));

    // What the VRM profile says about that: both gaps are errors, so nothing exports until a person fills them.
    let mut gathered = gather(&built, &GatherOptions { target_height: None, purpose: GatherPurpose::Vrm }).unwrap();
    add_rights(&mut gathered.scene, &built, Some(&bare));
    add_avatar(&mut gathered, &built).unwrap();
    let profile = vrm_profile(&gathered.scene, VrmVersion::V1);
    assert_eq!(errors(&profile), ["vrm.meta_authors_missing", "vrm.meta_license_url_missing"]);
    assert!(export_vrm(&gathered.scene).is_err());
}

#[test]
fn terms_are_the_records_never_widened_by_the_commercial_status() {
    let built = mock_built();
    let mut cleared = clear_record();
    assert_eq!(
        rights_of(&built, Some(&cleared)).meta.terms,
        UseTerms::default(),
        "cleared, but the record asked for little"
    );
    cleared.meta.terms.commercial_usage = io::CommercialUsage::PersonalProfit;
    cleared.meta.terms.allow_redistribution = true;
    assert_eq!(rights_of(&built, Some(&cleared)).meta.terms, cleared.meta.terms);
}

#[test]
fn the_provenance_author_names_the_author_only_when_the_record_lists_none() {
    let built = mock_built();
    let mut r = clear_record();
    r.meta.authors = vec![];
    assert_eq!(rights_of(&built, Some(&r)).meta.authors, ["Test Author"]);
    r.meta.authors = vec!["Eve".into()];
    assert_eq!(rights_of(&built, Some(&r)).meta.authors, ["Eve"]);
    r.meta.authors = vec!["  ".into()];
    r.provenance.author = Some(" ".into());
    assert_eq!(rights_of(&built, Some(&r)).meta.authors, ["  "], "blank is not an author, and nothing replaces it");
}

#[test]
fn attribution_travels_and_credit_is_required() {
    let built = mock_built();
    let mut r = clear_record();
    r.meta.terms.credit_notation = io::CreditNotation::Unnecessary;
    assert_eq!(rights_of(&built, Some(&r)).meta.copyright_information, None, "no attribution, nothing to carry");
    assert_eq!(rights_of(&built, Some(&r)).meta.terms.credit_notation, io::CreditNotation::Unnecessary);

    r.provenance.attribution = Some("Model by Dana, CC BY 4.0".into());
    let scene = rights_of(&built, Some(&r));
    assert_eq!(scene.meta.terms.credit_notation, io::CreditNotation::Required);
    assert_eq!(scene.meta.copyright_information.as_deref(), Some("Model by Dana, CC BY 4.0"));
    r.meta.copyright_information = Some("(c) Dana".into());
    assert_eq!(
        rights_of(&built, Some(&r)).meta.copyright_information.as_deref(),
        Some("(c) Dana"),
        "the record's own wins"
    );
}

#[test]
fn a_stale_thumbnail_index_is_cleared_and_a_missing_record_leaves_the_provenance_unset() {
    let built = mock_built();
    let mut r = clear_record();
    r.meta.thumbnail_image = Some(ImageIx(9));
    assert_eq!(rights_of(&built, Some(&r)).meta.thumbnail_image, None);
    let none = rights_of(&built, None);
    assert_eq!((none.provenance, none.meta.name.as_str(), none.meta.license_url.as_str()), (None, "Mock", ""));
}

#[test]
fn the_licence_record_lives_in_the_characters_own_node_and_carries_its_schema() {
    assert_eq!(record_path(&CharId::new("mira").unwrap()).to_string(), "characters/mira/licence");
    let node = serde_json::to_value(clear_record()).unwrap();
    assert_eq!(node["schema"], json!(1));
    assert_eq!(LicenceRecord::from_node(&node).unwrap(), clear_record());
}

#[test]
fn a_record_without_the_known_schema_is_refused_naming_the_field() {
    let good = serde_json::to_value(clear_record()).unwrap();
    let cases = [
        ("missing", None),
        ("a later version", Some(json!(2))),
        ("zero", Some(json!(0))),
        ("a string", Some(json!("1"))),
    ];
    for (label, schema) in cases {
        let mut node = good.clone();
        match schema {
            Some(version) => node["schema"] = version,
            None => {
                node.as_object_mut().unwrap().remove("schema");
            }
        }
        let error = LicenceRecord::from_node(&node).unwrap_err();
        assert!(matches!(error, RecordError::Schema { .. }) && error.field() == "schema", "{label}: {error}");
        assert!(error.to_string().contains("reads only schema 1"), "{label}: {error}");

        // The op says the same, with the field and where the record is.
        let mut root = doc(None).root().clone();
        root["characters"]["mira"]["licence"] = node;
        let refused =
            check_doc(&MemDoc::from_value(root), json!({ "character": "mira", "format": "vrm1" })).unwrap_err();
        assert_eq!((refused.code, refused.field.as_deref()), (ErrorCode::Invalid, Some("schema")), "{label}");
        assert!(refused.message.contains("characters/mira/licence"), "{label}: {}", refused.message);
        assert!(refused.hint.is_some());
    }
    // A record of the right schema that doesn't fit is a different fault, at the record as a whole.
    for bad in [json!({ "schema": 1, "provenance": "yes" }), json!({ "schema": 1, "extra": 1 })] {
        let error = LicenceRecord::from_node(&bad).unwrap_err();
        assert!(matches!(error, RecordError::Invalid(_)) && error.field() == "licence", "{error}");
    }
}

// ---------------------------------------------------------------------------------------------------------------
// io.check_export
// ---------------------------------------------------------------------------------------------------------------

/// The mock character as a project holds it: every part has the source row `clear_record` is derived from (authored,
/// cleared for commercial use, the most restrictive terms), so a clear record is no more permissive than its parts.
fn doc(record: Option<&LicenceRecord>) -> MemDoc {
    let mut root = json!({ "characters": { "mira": serde_json::to_value(mock_def()).unwrap() } });
    let mut row = serde_json::to_value(PartSource::new(authored_provenance())).unwrap();
    if let Some(r) = record {
        // A record is never more permissive than its parts, licence included.
        row["provenance"]["licence"] = json!(r.provenance.licence);
    }
    let parts: Vec<String> = root["characters"]["mira"]["parts"].as_object().unwrap().keys().cloned().collect();
    for part in parts {
        root["characters"]["mira"]["sources"][part] = row.clone();
    }
    if let Some(r) = record {
        root["characters"]["mira"]["licence"] = serde_json::to_value(r).unwrap();
    }
    MemDoc::from_value(root)
}

fn check_doc(doc: &MemDoc, args: Value) -> Result<ExportCheck, OpError> {
    let op = (find("io.check_export").expect("registered").decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let before = doc.root().clone();
    let plan = op.plan(&OpCtx::new(doc, &actor, &clock))?;
    assert!(plan.patch.changes.is_empty(), "io.check_export is a query and must not plan a change");
    assert_eq!(*doc.root(), before, "and it writes nothing");
    Ok(serde_json::from_value(plan.output.expect("a query answers with an output")).unwrap())
}

fn check(record: Option<&LicenceRecord>, format: &str, purpose: &str) -> ExportCheck {
    check_doc(&doc(record), json!({ "character": "mira", "format": format, "purpose": purpose })).unwrap()
}

fn finding<'a>(c: &'a ExportCheck, rule: &str) -> &'a io::Finding {
    c.report.findings.iter().find(|f| f.rule == rule).unwrap_or_else(|| panic!("no {rule} in {:#?}", c.report))
}

#[test]
fn the_op_is_a_read_scoped_query_with_flat_closed_arguments() {
    let meta = (find("io.check_export").unwrap().meta)();
    assert_eq!((meta.name, meta.kind, meta.scope), ("io.check_export", OpKind::Query, Scope::Read));
    let schema = serde_json::to_value((find("io.check_export").unwrap().schema)()).unwrap();
    assert_eq!(schema["required"], json!(["character", "format"]));
    let properties: Vec<_> = schema["properties"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(properties, ["character", "format", "purpose"]);
    assert_eq!(schema["additionalProperties"], json!(false));
    assert!(!schema.to_string().contains("$ref"), "MCP tool schemas are inlined");
    // The field is the argument that is wrong: a missing or unknown key names itself.
    for (bad, field) in [
        (json!({ "character": "mira" }), "format"),
        (json!({ "character": "mira", "format": "vrm1", "path": "x.vrm" }), "path"),
        (json!({ "character": "mira", "format": "vrm1", "purpose": "everyone" }), "purpose"),
    ] {
        let error = (find("io.check_export").unwrap().decode)(bad.clone()).err().expect("refused");
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some(field)), "{bad}");
        assert!(error.hint.is_some());
    }
}

#[test]
fn a_clear_record_passes_for_private_and_published_files_in_both_formats() {
    let clear = clear_record();
    for (format, purpose) in [("vrm1", "private"), ("vrm1", "publish"), ("glb", "private"), ("glb", "publish")] {
        let c = check(Some(&clear), format, purpose);
        assert!(c.ok && c.errors == 0, "{format} {purpose}: {:#?}", c.report);
    }
    let c = check(Some(&clear), "vrm1", "publish");
    assert_eq!((c.character.as_str(), c.format.as_str(), c.purpose), ("mira", "vrm1", Purpose::Publish));
    // What the check says it did not look at, and what the character lacks.
    assert_eq!(finding(&c, "perfect_sync.deferred").severity, Severity::Info);
    assert_eq!(finding(&c, "gather.look_at_missing").severity, Severity::Info);
    assert!(!check(Some(&clear), "glb", "private").report.has_rule("perfect_sync.deferred"));
}

#[test]
fn private_is_the_default_purpose() {
    let c = check_doc(&doc(Some(&clear_record())), json!({ "character": "mira", "format": "vrm1" })).unwrap();
    assert_eq!(c.purpose, Purpose::Private);
    let mut cc = clear_record();
    cc.provenance.commercial = Commercial::No;
    let c = check_doc(&doc(Some(&cc)), json!({ "character": "mira", "format": "vrm1" })).unwrap();
    assert!(c.ok, "the gate is not applied to a file for your own machine: {:#?}", c.report);
}

#[test]
fn a_non_commercial_record_is_stopped_for_publishing_with_the_gates_own_sentence() {
    let mut r = record(Commercial::No);
    r.provenance.licence = "CC-BY-NC-4.0".into();
    let sentence = licence_gate(&r.provenance, false).unwrap_err().to_string();
    for format in ["vrm1", "glb"] {
        let c = check(Some(&r), format, "publish");
        assert!(!c.ok);
        let f = finding(&c, "rights.gate_non_commercial");
        assert_eq!((f.severity, f.message.as_str()), (Severity::Error, sentence.as_str()));
        assert!(sentence.contains("CC-BY-NC-4.0"));
        assert!(f.hint.as_deref().unwrap().contains("characters/mira/licence"), "{f:?}");
    }
    assert!(check(Some(&r), "vrm1", "private").ok);
}

#[test]
fn unknown_commercial_status_and_private_test_material_are_stopped_for_publishing() {
    let r = record(Commercial::Unknown);
    let c = check(Some(&r), "vrm1", "publish");
    let f = finding(&c, "rights.gate_unknown");
    assert_eq!(f.message, licence_gate(&r.provenance, false).unwrap_err().to_string());
    assert!(f.hint.is_some());

    let mut test_material = clear_record();
    test_material.provenance.private_test = true;
    let c = check(Some(&test_material), "vrm1", "publish");
    assert_eq!(
        finding(&c, "rights.gate_private_test").message,
        licence_gate(&test_material.provenance, false).unwrap_err().to_string()
    );
    assert!(finding(&c, "rights.gate_private_test").hint.is_some());
    let private = check(Some(&test_material), "vrm1", "private");
    assert!(private.ok, "{:#?}", private.report);
    assert_eq!(finding(&private, "rights.private_test_export").severity, Severity::Info);
}

#[test]
fn a_file_may_not_grant_more_than_the_record_clears() {
    let mut r = record(Commercial::No);
    r.meta.terms.commercial_usage = io::CommercialUsage::PersonalProfit;
    let c = check(Some(&r), "vrm1", "private");
    assert!(!c.ok);
    assert!(finding(&c, "rights.terms_exceed_provenance").hint.is_some());
}

#[test]
fn missing_meta_is_reported_with_fix_hints_and_never_filled_in() {
    let mut r = clear_record();
    r.provenance.author = None;
    r.meta = VrmMeta::default();
    let c = check(Some(&r), "vrm1", "private");
    assert!(!c.ok);
    let rules: Vec<&str> = c.report.errors().map(|f| f.rule.as_str()).collect();
    assert_eq!(rules, ["vrm.meta_authors_missing", "vrm.meta_license_url_missing"]);
    assert!(c.report.errors().all(|f| f.hint.is_some()), "every error says how to fix it: {:#?}", c.report);
    assert!(finding(&c, "vrm.meta_authors_missing").hint.as_deref().unwrap().contains("characters/mira/licence"));
    assert!(
        finding(&c, "vrm.meta_license_url_missing").hint.as_deref().unwrap().contains("https://vrm.dev/licenses/1.0/")
    );
    // A plain GLB doesn't write VRM meta, so the same record is fine there.
    assert!(check(Some(&r), "glb", "private").ok);
}

#[test]
fn a_character_with_no_record_is_reported_not_guessed() {
    let c = check(None, "vrm1", "private");
    assert!(!c.ok);
    let f = finding(&c, "rights.provenance_missing");
    assert!(
        f.hint.as_deref().unwrap().contains("characters/mira/licence"),
        "the op layer's hint replaces the contract's: {f:?}"
    );
    assert!(c.report.errors().all(|f| f.hint.is_some()), "{:#?}", c.report);
}

#[test]
fn a_creature_is_not_a_vrm_but_is_a_glb() {
    let creature = creature_built();
    let vrm = CheckExport {
        character: CharId::new("mira").unwrap(),
        format: "vrm1".parse().unwrap(),
        purpose: Purpose::Private,
    };
    let c = vrm.check(&creature, Some(&clear_record())).unwrap();
    assert_eq!(c.report.errors().map(|f| f.rule.as_str()).collect::<Vec<_>>(), ["vrm.not_humanoid"]);
    assert!(finding(&c, "vrm.not_humanoid").hint.as_deref().unwrap().contains("glb"));
    let glb = CheckExport { format: "glb".parse().unwrap(), ..vrm };
    assert!(glb.check(&creature, Some(&clear_record())).unwrap().ok);
}

#[test]
fn requests_that_cannot_be_checked_are_errors_with_hints() {
    let d = doc(Some(&clear_record()));
    let err = |args| check_doc(&d, args).expect_err("an error");

    let unknown_format = err(json!({ "character": "mira", "format": "fbx" }));
    assert_eq!((unknown_format.code, unknown_format.field.as_deref()), (ErrorCode::Invalid, Some("format")));
    assert!(unknown_format.hint.as_deref().unwrap().contains("glb, vrm1"), "{unknown_format:?}");

    let unknown_character = err(json!({ "character": "nobody", "format": "vrm1" }));
    assert_eq!(unknown_character.code, ErrorCode::NotFound);
    assert!(unknown_character.hint.is_some());

    let mut root = d.root().clone();
    root["characters"]["mira"]["licence"] = json!({ "schema": 1, "provenance": "yes" });
    let broken = MemDoc::from_value(root);
    let unreadable = check_doc(&broken, json!({ "character": "mira", "format": "vrm1" })).unwrap_err();
    assert_eq!((unreadable.code, unreadable.field.as_deref()), (ErrorCode::Invalid, Some("licence")));
    assert!(unreadable.message.contains("characters/mira/licence"), "{}", unreadable.message);
}

#[test]
fn a_character_with_variants_and_other_sibling_nodes_still_reads() {
    let mut root = doc(Some(&clear_record())).root().clone();
    root["characters"]["mira"]["variants"] = serde_json::to_value(mock_variants().unwrap()).unwrap();
    root["characters"]["mira"]["rig"] = json!({ "note": "rig.toml is its own file" });
    let c = check_doc(&MemDoc::from_value(root), json!({ "character": "mira", "format": "vrm1" })).unwrap();
    assert!(c.ok, "{:#?}", c.report);
}

#[test]
fn errors_come_first_then_warnings_then_notes() {
    let c = check(None, "vrm1", "publish");
    let severities: Vec<_> = c.report.findings.iter().map(|f| f.severity).collect();
    assert!(severities.windows(2).all(|w| w[0] >= w[1]), "{severities:?}");
    assert_eq!(severities.iter().filter(|s| **s == Severity::Error).count(), c.errors);
    assert_eq!(severities.iter().filter(|s| **s == Severity::Warning).count(), c.warnings);
    assert!(c.errors >= 3 && c.warnings >= 1);
}

#[test]
fn the_answer_is_deterministic() {
    let d = doc(Some(&clear_record()));
    let args = json!({ "character": "mira", "format": "vrm1", "purpose": "publish" });
    assert_eq!(check_doc(&d, args.clone()).unwrap(), check_doc(&d, args).unwrap());
}

#[test]
fn node_names_and_findings_do_not_repeat() {
    let built = rich_built();
    let gathered = vrm_gathered(&built, &clear_record(), None);
    let names: BTreeSet<_> = gathered.scene.nodes.iter().map(|n| n.name.clone()).collect();
    assert_eq!(names.len(), gathered.scene.nodes.len());
    let c = CheckExport {
        character: CharId::new("mira").unwrap(),
        format: "vrm1".parse().unwrap(),
        purpose: Purpose::Private,
    }
    .check(&built, Some(&clear_record()))
    .unwrap();
    let rules: Vec<_> = c.report.findings.iter().map(|f| (&f.rule, &f.location)).collect();
    assert_eq!(rules.len(), rules.iter().collect::<BTreeSet<_>>().len(), "{rules:?}");
}

/// `io.check_export` for a good and a broken character, asserted and printed: the evidence the PR quotes.
#[test]
fn check_export_answers_for_a_good_and_a_broken_character() {
    let mut broken = record(Commercial::No);
    broken.provenance.licence = "CC-BY-NC-4.0".into();
    broken.provenance.author = None;
    broken.meta = VrmMeta::new("Mira", vec![], "");
    let answers = [
        ("good", Some(clear_record()), vec![]),
        (
            "broken",
            Some(broken),
            vec!["rights.gate_non_commercial", "vrm.meta_authors_missing", "vrm.meta_license_url_missing"],
        ),
        (
            "no record",
            None,
            vec!["rights.provenance_missing", "vrm.meta_authors_missing", "vrm.meta_license_url_missing"],
        ),
    ];
    for (label, record, expected_errors) in answers {
        let c = check(record.as_ref(), "vrm1", "publish");
        println!("--- {label} ---\n{}", serde_json::to_string_pretty(&c).unwrap());
        let rules: Vec<&str> = c.report.errors().map(|f| f.rule.as_str()).collect();
        assert_eq!(rules, expected_errors, "{label}");
        assert_eq!((c.ok, c.errors), (expected_errors.is_empty(), expected_errors.len()), "{label}");
        assert!(
            c.report.errors().all(|f| f.hint.as_deref().is_some_and(|h| !h.is_empty())),
            "{label}: every error has a hint"
        );
    }
    let good = check(Some(&clear_record()), "vrm1", "publish");
    let notes: Vec<&str> =
        good.report.findings.iter().filter(|f| f.severity == Severity::Info).map(|f| f.rule.as_str()).collect();
    for note in ["gather.look_at_missing", "perfect_sync.deferred", "rights.consent_not_checked"] {
        assert!(notes.contains(&note), "{note} in {notes:?}");
    }
}

#[test]
fn the_consent_rule_is_said_to_be_unchecked_unless_the_record_holds_a_consent() {
    let c = check(Some(&clear_record()), "vrm1", "publish");
    let note = finding(&c, "rights.consent_not_checked");
    assert_eq!(note.severity, Severity::Info);
    assert!(note.hint.as_deref().unwrap().contains("provenance.consent at characters/mira/licence"), "{note:?}");

    let mut consented = clear_record();
    consented.provenance.consent =
        Some(Consent { person: "Dana".into(), scope: "likeness for this avatar".into(), document: None });
    assert!(!check(Some(&consented), "vrm1", "publish").report.has_rule("rights.consent_not_checked"));
    assert!(
        !check(None, "vrm1", "publish").report.has_rule("rights.consent_not_checked"),
        "nothing to say without a record"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// No permissive export
// ---------------------------------------------------------------------------------------------------------------

/// Records that are not cleared to be published, as a project would hold them: non-commercial, non-profit (the VRM
/// `personalNonProfit` reading), unknown, and private test material (with and without a commercial clearance).
fn uncleared_records() -> Vec<(&'static str, LicenceRecord)> {
    let mut non_commercial = record(Commercial::No);
    non_commercial.provenance.licence = "CC-BY-NC-4.0".into();
    let mut non_profit = record(Commercial::No);
    non_profit.provenance.licence = io::VRM_LICENSE_REF.into();
    let unknown = record(Commercial::Unknown);
    let mut private_test = record(Commercial::No);
    private_test.provenance.private_test = true;
    let mut private_test_cleared = clear_record();
    private_test_cleared.provenance.private_test = true;
    vec![
        ("non-commercial", non_commercial),
        ("non-profit", non_profit),
        ("unknown", unknown),
        ("private test", private_test),
        ("private test, cleared", private_test_cleared),
    ]
}

#[test]
fn a_record_that_is_not_cleared_never_yields_permissive_terms() {
    for (label, r) in uncleared_records() {
        for built in [mock_built(), rich_built()] {
            let scene = vrm_gathered(&built, &r, Some(1.5)).scene;
            assert_eq!(scene.meta.terms, UseTerms::default(), "{label}: the most restrictive terms, nothing widened");
            assert_eq!(scene.meta.terms.commercial_usage, io::CommercialUsage::PersonalNonProfit, "{label}");
            assert_eq!(scene.provenance.as_ref(), Some(&r.provenance), "{label}: the record's status is kept as it is");
        }
    }
}

#[test]
fn a_record_that_is_not_cleared_is_refused_for_publishing_by_the_check_and_by_the_export() {
    for (label, r) in uncleared_records() {
        let gate = licence_gate(&r.provenance, false).unwrap_err();
        let rule = match gate {
            omuzaki_store_api::GateError::NonCommercial { .. } => "rights.gate_non_commercial",
            omuzaki_store_api::GateError::Unknown { .. } => "rights.gate_unknown",
            omuzaki_store_api::GateError::PrivateTest => "rights.gate_private_test",
            omuzaki_store_api::GateError::NoConsent => unreachable!("the check never says a real person is involved"),
        };
        for format in ["vrm1", "glb"] {
            let c = check(Some(&r), format, "publish");
            assert!(!c.ok, "{label} {format}");
            let f = finding(&c, rule);
            assert_eq!(
                (f.severity, f.message.as_str()),
                (Severity::Error, gate.to_string().as_str()),
                "{label} {format}"
            );
            assert!(f.hint.is_some(), "{label} {format}");
        }
        // The export itself, run on the finished scene, refuses for the same reason.
        let scene = vrm_gathered(&mock_built(), &r, None).scene;
        let options = options_without_face_channels().with_purpose(Purpose::Publish);
        match run_export(&MockVrm1Exporter, &scene, &ExportCtx::new(&options)) {
            Err(io::IoError::Rejected(report)) => assert!(report.has_rule(rule), "{label}: {report:?}"),
            other => panic!("{label}: expected a rejection, got {other:?}"),
        }
    }
}

/// Opens one permission of a record's terms.
type Open = fn(&mut UseTerms);

#[test]
fn open_terms_on_a_record_that_is_not_cleared_are_refused_even_for_a_private_file() {
    use io::CommercialUsage::{Corporation, PersonalProfit};
    for (label, mut r) in uncleared_records() {
        for usage in [PersonalProfit, Corporation] {
            r.meta.terms.commercial_usage = usage;
            let c = check(Some(&r), "vrm1", "private");
            assert!(!c.ok, "{label} {usage:?}");
            assert!(finding(&c, "rights.terms_exceed_provenance").hint.is_some(), "{label} {usage:?}");
        }
    }
    // Private test material may carry nothing but the most restrictive terms: any open flag is refused.
    let opens: [(&str, Open); 5] = [
        ("redistribution", |t| t.allow_redistribution = true),
        ("modification", |t| t.modification = io::Modification::AllowModificationRedistribution),
        ("everyone", |t| t.avatar_permission = io::AvatarPermission::Everyone),
        ("credit omitted", |t| t.credit_notation = io::CreditNotation::Unnecessary),
        ("political", |t| t.allow_political_or_religious_usage = true),
    ];
    let mut private_test = clear_record();
    private_test.provenance.private_test = true;
    for (label, open) in opens {
        let mut r = private_test.clone();
        open(&mut r.meta.terms);
        let private = check(Some(&r), "vrm1", "private");
        assert!(
            !private.ok && private.report.has_rule("rights.terms_exceed_provenance"),
            "{label}: {:#?}",
            private.report
        );
        let publish = check(Some(&r), "vrm1", "publish");
        assert!(publish.report.has_rule("rights.gate_private_test"), "{label}");
    }
}

#[test]
fn the_look_at_offset_does_not_depend_on_the_rest_of_other_bones() {
    // ADR 0014: rest is the T-pose and the pose a mesh was sculpted in lives in the inverse bind matrices. Turning
    // the shoulders, arms, spine and neck at rest must leave the offset in the head's frame where it was.
    let turned = |bone: HumanoidBone| {
        let mut t = rest(bone);
        if matches!(
            bone,
            HumanoidBone::LeftShoulder
                | HumanoidBone::RightShoulder
                | HumanoidBone::LeftUpperArm
                | HumanoidBone::RightUpperArm
                | HumanoidBone::Spine
                | HumanoidBone::Neck
        ) {
            t.rotation = Quat::from_rotation_z(0.9) * t.rotation;
        }
        t
    };
    let (upright, bent) = (rich_with(rest), rich_with(turned));
    for target in [None, Some(1.7)] {
        let (a, b) = (vrm_gathered(&upright, &clear_record(), target), vrm_gathered(&bent, &clear_record(), target));
        let neck = bone_node(upright.rig.skeleton.humanoid_bone(HumanoidBone::Neck).unwrap());
        assert_ne!(a.scene.nodes[neck.index()].transform.rotation, b.scene.nodes[neck.index()].transform.rotation);
        let (a, b) = (avatar_of(&a).look_at.unwrap(), avatar_of(&b).look_at.unwrap());
        assert!((a.offset_from_head_bone - b.offset_from_head_bone).length() < 1e-6, "{a:?} vs {b:?}");
    }
}
