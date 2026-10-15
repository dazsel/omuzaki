//! A character's bone drivers in the gathered VRM avatar block (#801): `Follow` drivers as node constraints on their
//! nodes, a `Channel` driver left out with a warning. The real-format round trip needs the VRM writer, which an
//! implementation crate can't test against.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_anim_api::{BoneId, Channel, Transform};
use omuzaki_char_api::drivers::RollAxis;
use omuzaki_char_api::{
    BoneDriver, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Quat;
use omuzaki_io_api::{ConstraintKind, NodeConstraint, NodeIx};
use omuzaki_io_ops::convert::drivers::RULE_CHANNEL_DRIVER;
use omuzaki_io_ops::gather::avatar::add_avatar;
use omuzaki_io_ops::gather::{GatherOptions, GatherPurpose, gather};

/// The mock character with two drivers on its last two extra bones: the last follows the twist of the one before, and
/// that one folds with a channel.
fn driven() -> (BuiltCharacter, [BoneId; 2]) {
    let def = mock_character().unwrap();
    let mut built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let extra: Vec<BoneId> = (0..built.rig.skeleton.len())
        .filter(|&i| built.rig.skeleton.bones()[i].humanoid.is_none())
        .map(|i| BoneId(u16::try_from(i).unwrap()))
        .collect();
    let [.., folding, following] = extra[..] else { panic!("the mock character has extra bones: {extra:?}") };
    let rest = built.rig.skeleton.bones()[folding.index()].rest;
    let to = Transform { rotation: rest.rotation * Quat::from_rotation_z(0.8), ..rest };
    let mut follow = BoneDriver::follow(following, folding, 0.5);
    if let omuzaki_char_api::Drive::Follow { axis, .. } = &mut follow.drive {
        *axis = Some(RollAxis::Y);
    }
    built.rig.drivers = vec![BoneDriver::channel(folding, Channel::custom(1).unwrap(), rest, to), follow];
    (built, [folding, following])
}

#[test]
fn follow_drivers_reach_the_avatar_block_and_a_channel_driver_is_reported() {
    let (built, [folding, following]) = driven();
    let mut gathered = gather(&built, &GatherOptions { target_height: None, purpose: GatherPurpose::Vrm }).unwrap();
    add_avatar(&mut gathered, &built).unwrap();
    let avatar = gathered.scene.avatar.as_ref().unwrap();
    let node = |bone: BoneId| NodeIx(u32::from(bone.0));
    assert_eq!(
        avatar.constraints,
        [NodeConstraint {
            node: node(following),
            kind: ConstraintKind::Roll { source: node(folding), axis: omuzaki_io_api::RollAxis::Y },
            weight: 0.5,
        }]
    );
    let warned: Vec<_> = gathered.report.findings.iter().filter(|f| f.rule == RULE_CHANNEL_DRIVER).collect();
    assert_eq!(warned.len(), 1, "{:?}", gathered.report.findings);
    assert!(warned[0].message.contains(&format!("bone {}", folding.0)), "{}", warned[0].message);
}

#[test]
fn a_character_without_drivers_has_no_constraints_and_no_driver_finding() {
    let (mut built, _) = driven();
    built.rig.drivers.clear();
    let mut gathered = gather(&built, &GatherOptions { target_height: None, purpose: GatherPurpose::Vrm }).unwrap();
    add_avatar(&mut gathered, &built).unwrap();
    assert_eq!(gathered.scene.avatar.as_ref().unwrap().constraints, []);
    assert!(gathered.report.findings.iter().all(|f| f.rule != RULE_CHANNEL_DRIVER));
}
