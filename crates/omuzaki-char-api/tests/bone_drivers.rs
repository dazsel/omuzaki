//! Bone drivers through the reference builder: rebased onto final bone ids, validated per part, typed errors,
//! and the corrective shoulder helper expressed as a driver.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use omuzaki_anim_api::{Channel, ChannelBuf, HumanoidBone, Transform};
use omuzaki_char_api::{
    BoneDriver, BoneName, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder,
    CharacterDef, DriverError, ExtraBone, JointRef, PartCategory, PartDef, PartId, PartKind, PartKindId, PartMeta,
    PartOutput, RegistryBuilder, extra_id, humanoid_id,
};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_face_api::{Check, FaceProfile, Palette, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Params {
    mode: String,
}

impl Validate for Params {
    fn check(&self, _: &mut Check) {}
}

fn bone(name: &str, parent: JointRef, at: Vec3) -> ExtraBone {
    ExtraBone { name: BoneName::new(name).unwrap(), parent, rest: Transform::from_translation(at) }
}

fn spread() -> Transform {
    Transform { translation: Vec3::ZERO, rotation: Quat::from_rotation_z(1.0) }
}

struct DriverProbe;

impl PartKind for DriverProbe {
    type Params = Params;

    fn meta() -> PartMeta {
        PartMeta { id: "driver-probe", summary: "A test kind.", category: PartCategory::Accessory, requires: &[] }
    }

    fn build(params: &Params, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let mut out = PartOutput::default();
        let rig = &mut out.rig;
        let custom = Channel::custom(3).unwrap();
        // Two wing bones under the spine; `helper` hangs from the left shoulder joint's parent chain instead.
        let wing_0 = rig.add_bone(bone("wing_0", JointRef::Humanoid(HumanoidBone::Spine), Vec3::new(0.0, 0.3, 0.0)));
        let wing_1 = rig.add_bone(bone("wing_1", JointRef::Humanoid(HumanoidBone::Spine), Vec3::new(0.0, 0.3, 0.1)));
        let fold = |bone| BoneDriver::channel(bone, custom, Transform::IDENTITY, spread());
        match params.mode.as_str() {
            "wings" => rig.drivers.extend([fold(wing_0), fold(wing_1)]),
            "helper" => {
                let upper_arm = humanoid_id(HumanoidBone::LeftUpperArm);
                rig.drivers.push(BoneDriver::follow(wing_0, upper_arm, 0.5));
            }
            "not_custom" => {
                rig.drivers.push(BoneDriver::channel(wing_0, Channel::JAW_OPEN, Transform::IDENTITY, spread()));
            }
            "humanoid_driven" => rig.drivers.push(fold(humanoid_id(HumanoidBone::Head))),
            "twice" => rig.drivers.extend([fold(wing_0), fold(wing_0)]),
            "unknown_bone" => rig.drivers.push(fold(extra_id(9))),
            "unknown_source" => rig.drivers.push(BoneDriver::follow(wing_1, extra_id(9), 0.5)),
            "source_later" => rig.drivers.push(BoneDriver::follow(wing_0, wing_1, 0.5)),
            "bad_weight" => rig.drivers.push(BoneDriver::follow(wing_1, wing_0, 1.5)),
            _ => {}
        }
        Ok(out)
    }
}
omuzaki_char_api::register_part_kind!(DriverProbe);

fn build(mode: &str) -> Result<BuiltCharacter, BuildError> {
    let probe = PartDef {
        kind: PartKindId::new("driver-probe").unwrap(),
        parent: None,
        enabled: true,
        layer: 1,
        attach: None,
        params: serde_json::json!({ "mode": mode }),
    };
    let def = CharacterDef {
        name: "Test".to_owned(),
        template: omuzaki_char_api::BodyTemplateId::new("soft-sd").unwrap(),
        seed: 1,
        palette: Palette::default(),
        skin_tone: None,
        parts: [(PartId::new("body").unwrap(), common::body_part()), (PartId::new("wings").unwrap(), probe)]
            .into_iter()
            .collect(),
        face: FaceProfile::default(),
        weight_brushes: std::collections::BTreeMap::new(),
    };
    RegistryBuilder.build(&CharId::new("drv").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
}

fn drivers_error(mode: &str) -> DriverError {
    match build(mode).unwrap_err() {
        BuildError::Drivers { part, source } => {
            assert_eq!(part.as_str(), "wings");
            source
        }
        other => panic!("expected a driver error, got {other}"),
    }
}

#[test]
fn drivers_are_rebased_onto_final_bone_ids_and_carried_in_the_rig() {
    let built = build("wings").unwrap();
    let skeleton = &built.rig.skeleton;
    let wing = |name: &str| skeleton.find(name).unwrap();
    let bones: Vec<_> = built.rig.drivers.iter().map(|d| d.bone).collect();
    assert_eq!(bones, [wing("wing_0"), wing("wing_1")]);
    assert_eq!(build("wings").unwrap().rig.drivers, built.rig.drivers, "two builds agree");

    // The compiled plan folds and spreads the wings from the custom channel.
    let plan = built.rig.driver_plan().unwrap();
    let (mut folded, mut open) = (skeleton.rest_pose(), skeleton.rest_pose());
    let mut channels = ChannelBuf::NEUTRAL;
    plan.apply(&channels, &mut folded);
    channels.set(Channel::custom(3).unwrap(), 1.0);
    plan.apply(&channels, &mut open);
    let at = wing("wing_0").index();
    assert!(folded.local[at].rotation.angle_between(Quat::IDENTITY) < 1e-6);
    assert!((open.local[at].rotation.angle_between(Quat::IDENTITY) - 1.0).abs() < 1e-5);
}

#[test]
fn a_corrective_shoulder_helper_is_a_follow_driver_at_half_the_upper_arm() {
    let built = build("helper").unwrap();
    let skeleton = &built.rig.skeleton;
    let upper_arm = skeleton.humanoid_bone(HumanoidBone::LeftUpperArm).unwrap();
    let helper = skeleton.find("wing_0").unwrap(); // stands in for the helper bone
    assert_eq!(built.rig.drivers, [BoneDriver::follow(helper, upper_arm, 0.5)]);
    assert!(upper_arm < helper, "a follow source comes before the driven bone");

    let mut pose = skeleton.rest_pose();
    pose.local[upper_arm.index()].rotation = Quat::from_rotation_z(-1.2);
    built.rig.driver_plan().unwrap().apply(&ChannelBuf::NEUTRAL, &mut pose);
    let angle = pose.local[helper.index()].rotation.angle_between(skeleton.get(helper).unwrap().rest.rotation);
    assert!((angle - 0.6).abs() < 1e-5, "half of 1.2 rad, got {angle}");
}

#[test]
fn bad_drivers_are_typed_errors_naming_the_part_and_the_bone() {
    assert!(matches!(drivers_error("not_custom"), DriverError::NotCustomChannel { bone, .. } if bone == "wing_0"));
    assert!(matches!(drivers_error("humanoid_driven"), DriverError::HumanoidBone { bone } if bone == "head"));
    assert!(matches!(drivers_error("twice"), DriverError::DuplicateBone { bone } if bone == "wing_0"));
    assert!(matches!(drivers_error("source_later"), DriverError::SourceNotEarlier { bone, .. } if bone == "wing_0"));
    assert!(matches!(drivers_error("bad_weight"), DriverError::BadWeight { bone, .. } if bone == "wing_1"));
}

#[test]
fn a_provisional_id_with_nothing_behind_it_is_an_unknown_rig_bone() {
    for mode in ["unknown_bone", "unknown_source"] {
        let err = build(mode).unwrap_err();
        assert!(
            matches!(&err, BuildError::UnknownRigBone { part, what } if part.as_str() == "wings" && what.contains("bone driver")),
            "{mode}: {err}"
        );
    }
}

#[test]
fn drivers_survive_a_serde_round_trip_of_the_rig() {
    let built = build("helper").unwrap();
    let json = serde_json::to_string(&built.rig).unwrap();
    let back: omuzaki_char_api::CharacterRig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.drivers, built.rig.drivers);
    assert!(!serde_json::to_string(&build("none").unwrap().rig).unwrap().contains("drivers"), "empty is omitted");
}
