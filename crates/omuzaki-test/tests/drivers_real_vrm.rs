//! A driven character's bone drivers through the real VRM 1.0 format (#801): `omuzaki-io-ops` gathers the avatar
//! block with the drivers as `VRMC_node_constraint`s, `omuzaki-io-vrm` writes the file and reads it back, and every
//! constraint survives, node, kind, source, axis and weight. The gather tests in `omuzaki-io-ops` can only use the mock
//! format, which drops constraints.

// Test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_char_api::drivers::{Drive, RollAxis};
use omuzaki_char_api::{
    BoneDriver, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_io_api::{
    ConstraintKind, ExportCtx, ImportCtx, ImportInput, ImportOptions, IoScene, VRM_LICENSE_URLS, VrmMeta, run_export,
    run_import,
};
use omuzaki_io_mock::fixtures::{authored_provenance, options_without_face_channels};
use omuzaki_io_ops::gather::avatar::{LicenceRecord, add_avatar, add_rights};
use omuzaki_io_ops::gather::{GatherOptions, GatherPurpose, gather};
use omuzaki_io_vrm::{Vrm1Exporter, Vrm1Importer};

/// The mock character with its last three extra bones driven: the middle one follows three quarters of the first's
/// rotation, the last half of the middle one's twist about Z.
fn driven() -> BuiltCharacter {
    let def = mock_character().unwrap();
    let mut built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let skeleton = &built.rig.skeleton;
    let extra: Vec<_> = skeleton
        .bones()
        .iter()
        .filter(|b| b.humanoid.is_none())
        .map(|b| skeleton.find(&b.name).expect("a bone finds itself"))
        .collect();
    let [.., a, b, c] = extra[..] else { panic!("the mock character has three extra bones") };
    let mut roll = BoneDriver::follow(c, b, 0.5);
    if let Drive::Follow { axis, .. } = &mut roll.drive {
        *axis = Some(RollAxis::Z);
    }
    built.rig.drivers = vec![BoneDriver::follow(b, a, 0.75), roll];
    built
}

/// The gathered VRM scene of `built`, with the records the exporter insists on.
fn vrm_scene(built: &BuiltCharacter) -> IoScene {
    let mut gathered = gather(built, &GatherOptions { target_height: None, purpose: GatherPurpose::Vrm }).unwrap();
    let record = LicenceRecord::new(
        authored_provenance(),
        VrmMeta::new("Mock", vec!["Test Author".into()], VRM_LICENSE_URLS[0]),
    );
    add_rights(&mut gathered.scene, built, Some(&record));
    add_avatar(&mut gathered, built).unwrap();
    gathered.scene
}

fn round_trip(scene: &IoScene) -> IoScene {
    let options = options_without_face_channels();
    let outcome = run_export(&Vrm1Exporter, scene, &ExportCtx::new(&options)).unwrap();
    let bytes = outcome.files.into_iter().next().unwrap().bytes;
    let options = ImportOptions::default();
    run_import(&Vrm1Importer, &ImportInput::new(&bytes), &ImportCtx::new(&options)).unwrap().scene
}

#[test]
fn bone_drivers_survive_a_vrm_round_trip_as_node_constraints() {
    let scene = vrm_scene(&driven());
    let written = &scene.avatar.as_ref().unwrap().constraints;
    assert_eq!(written.len(), 2);
    assert!(matches!(written[0].kind, ConstraintKind::Rotation { .. }), "{written:?}");
    assert!(matches!(written[1].kind, ConstraintKind::Roll { axis: omuzaki_io_api::RollAxis::Z, .. }), "{written:?}");

    let back = round_trip(&scene);
    let names = |s: &IoScene| s.nodes.iter().map(|n| n.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&scene), names(&back), "the same nodes in the same order, so node indices compare");
    let mut read = back.avatar.as_ref().unwrap().constraints.clone();
    read.sort_by_key(|c| c.node.0);
    assert_eq!(&read, written, "the constraints read back as written");
}
